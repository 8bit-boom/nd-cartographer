//! Exports: images, PDF, UVTT, Foundry, SVG, Roll20, conversion and the
//! project backup bundle.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct ExportQuery {
    pub(crate) floor: Option<String>,
    /// Pixels per cell of the exported picture (default 32).
    pub(crate) ppc: Option<i64>,
    /// `png` (default), `webp` (lossless) or `avif` (lossy; UVTT takes png/webp only).
    pub(crate) format: Option<String>,
    /// AVIF quality 1..=100 (default 80).
    pub(crate) quality: Option<u8>,
    /// `gm` (default) or `player`: player output goes through the one secrets filter.
    pub(crate) audience: Option<String>,
}

pub(crate) fn pick_audience(q: &ExportQuery) -> Result<ndcart_core::Audience, ApiError> {
    match q.audience.as_deref() {
        None => Ok(ndcart_core::Audience::Gm),
        Some(a) => ndcart_core::Audience::parse(a).ok_or_else(|| {
            ApiError(CoreError::Invalid(format!(
                "unknown audience {a:?} (gm or player)"
            )))
        }),
    }
}

pub(crate) fn pick_format(q: &ExportQuery) -> Result<tiles::ImageFormat, ApiError> {
    match q.format.as_deref() {
        None => Ok(tiles::ImageFormat::Png),
        Some(f) => tiles::ImageFormat::parse(f).ok_or_else(|| {
            ApiError(CoreError::Invalid(format!(
                "unknown format {f:?} (png, webp or avif)"
            )))
        }),
    }
}

#[derive(Deserialize)]
pub(crate) struct FoundryQuery {
    pub(crate) floor: Option<String>,
    /// Pixels per square of the picture you will upload (default 100, Foundry's own).
    pub(crate) ppc: Option<f64>,
    /// Where the picture will be in Foundry's data folder.
    pub(crate) image: Option<String>,
    pub(crate) distance: Option<f64>,
    pub(crate) units: Option<String>,
    pub(crate) audience: Option<String>,
}

/// `GET /api/docs/{id}/export/foundry[?floor&ppc&image&distance&units&audience]` —
/// the floor as Foundry VTT scene data (walls, doors, windows, lights, grid), for
/// Foundry v11/v12. Export the picture separately (WebP at the same `ppc`),
/// upload it to Foundry and give its path as `image`. Not checked against a
/// running Foundry; it follows the documented data model.
pub(crate) async fn export_foundry(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<FoundryQuery>,
) -> Result<Response, ApiError> {
    let ppc = q.ppc.unwrap_or(100.0);
    let distance = q.distance.unwrap_or(5.0);
    if !(8.0..=1000.0).contains(&ppc) || !(0.01..=1000.0).contains(&distance) {
        return Err(ApiError(CoreError::Invalid(
            "ppc must be 8-1000 and distance 0.01-1000".into(),
        )));
    }
    let audience = match q.audience.as_deref() {
        None => ndcart_core::Audience::Gm,
        Some(a) => ndcart_core::Audience::parse(a)
            .ok_or_else(|| ApiError(CoreError::Invalid("audience must be gm or player".into())))?,
    };
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let idx = match &q.floor {
        Some(f) => floors_by_id(&doc.floors, f)?,
        None => 0,
    };
    let floor = ndcart_core::for_audience(&doc.floors[idx], audience);
    let cell_size = doc.manifest.cell_size;
    let name = format!("{} - {}", doc.manifest.name, doc.floors[idx].name);
    drop(store);
    let k = ppc / cell_size.max(1.0);
    let scale = |p: &[f64; 2]| [p[0] * k, p[1] * k];
    let walls: Vec<ndcart_core::Wall> = tiles::derive_walls(&floor, cell_size)
        .into_iter()
        .map(|mut w| {
            w.pts = w.pts.iter().map(scale).collect();
            w
        })
        .collect();
    let lights: Vec<ndcart_core::Light> = floor
        .lights
        .iter()
        .cloned()
        .map(|mut l| {
            l.x *= k;
            l.y *= k;
            l
        })
        .collect();
    let (cols, rows) = tiles::grid_extent(&floor);
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let default_image = format!("maps/{}.webp", slug.trim_matches('-'));
    let scene = ndcart_formats::foundry::to_scene(&ndcart_formats::foundry::SceneOut {
        name: &name,
        grid_px: ppc,
        cols,
        rows,
        walls: &walls,
        lights: &lights,
        image_path: q.image.as_deref().unwrap_or(&default_image),
        distance,
        units: q.units.as_deref().unwrap_or("ft"),
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        scene.to_string(),
    )
        .into_response())
}

#[derive(Deserialize)]
pub(crate) struct BundleQuery {
    /// `1`: the pictures the project uses are packed in too (a full backup).
    pub(crate) assets: Option<String>,
}

/// `GET /api/docs/{id}/bundle[?assets=1]` — the project as a `.ndmapz` backup:
/// its files, the licences of the packs it uses, and with `assets=1` the
/// pictures too. `POST /api/import/bundle` reads it back.
pub(crate) async fn get_bundle(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<BundleQuery>,
) -> Result<Response, ApiError> {
    let embed = matches!(q.assets.as_deref(), Some("1" | "true"));
    // Gather the files under the lock (a consistent copy); zip them without it.
    let (parts, name) = {
        let store = app.store.read().await;
        let doc = store
            .docs
            .get(&id)
            .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
        (
            bundle::bundle_parts(doc, embed).map_err(CoreError::Invalid)?,
            doc.manifest.name.clone(),
        )
    };
    let _heavy = app.heavy.acquire().await;
    let bytes = tokio::task::spawn_blocking(move || bundle::zip_parts(&parts.files))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("bundle failed: {e}"))))?
        .map_err(CoreError::Invalid)?;
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{safe}.ndmapz\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// `GET /api/docs/{id}/export.svg?floor=ID&audience=gm|player` - the floor's
/// geometry as an editable SVG (plan 21.11): rooms as rects, walls/doors as
/// lines, labels as real text. Cells are the unit: 1 cell = 1 user unit, so
/// the file scales cleanly in Inkscape/Illustrator.
pub(crate) async fn export_svg(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ExportQuery>,
) -> Result<Response, ApiError> {
    use ndcart_core::WallKind;
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let idx = match &q.floor {
        Some(fid) => floors_by_id(&doc.floors, fid)?,
        None => 0,
    };
    let audience = Audience::parse(q.audience.as_deref().unwrap_or("gm"))
        .ok_or_else(|| CoreError::Invalid("audience must be gm or player".into()))?;
    let floor_name = doc.floors[idx].name.clone();
    let doc_id_for_name = doc.manifest.id.clone();
    let floor = ndcart_core::for_audience(&doc.floors[idx], audience);
    let cell_size = doc.manifest.cell_size;
    let cell = cell_size;
    let walls = tiles::derive_walls(&floor, cell);
    // Bounds: the painted bounding box with a one-cell margin.
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (c, _) in floor.grid.cells() {
        xs.push(c.x);
        ys.push(c.y);
    }
    let (Some(&min_x), Some(&min_y), Some(&max_x), Some(&max_y)) = (
        xs.iter().min(),
        ys.iter().min(),
        xs.iter().max(),
        ys.iter().max(),
    ) else {
        return Err(ApiError(CoreError::Invalid("the floor is empty".into())));
    };
    let (w, h) = ((max_x - min_x + 3) as i64, (max_y - min_y + 3) as i64);
    let off_x = (min_x - 1) as f64;
    let off_y = (min_y - 1) as f64;
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    // 22.4: px -> cells through the one transform (a cropped identity: the
    // crop is the bounding box's outer margin cell).
    let tr = ndcart_core::units::Transform::cropped(cell_size, [off_x, off_y]);
    let to_cells =
        |p: [f64; 2]| -> [f64; 2] { tr.px_to_cell(p).unwrap_or([f64::INFINITY, f64::INFINITY]) };
    let mut b = String::new();
    b.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">
"
    ));
    b.push_str(&format!(
        "<rect width=\"{w}\" height=\"{h}\" fill=\"#14161a\"/>
"
    ));
    // Rooms: their painted cells grouped as one rect per space (bounding box,
    // matching the publish elements' shapes).
    for (space_id, space) in &floor.grid.spaces {
        let cells: Vec<_> = floor
            .grid
            .cells()
            .filter(|(_, s)| *s == space_id)
            .map(|(c, _)| c)
            .collect();
        if cells.is_empty() {
            continue;
        }
        let (bx0, by0) = (
            cells.iter().map(|c| c.x).min().unwrap(),
            cells.iter().map(|c| c.y).min().unwrap(),
        );
        let (bx1, by1) = (
            cells.iter().map(|c| c.x).max().unwrap(),
            cells.iter().map(|c| c.y).max().unwrap(),
        );
        b.push_str(&format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\" fill-opacity=\"0.85\"/>
",
            bx0 as f64 - off_x,
            by0 as f64 - off_y,
            (bx1 - bx0 + 1) as f64,
            (by1 - by0 + 1) as f64,
            esc(&space.color)
        ));
        if !space.name.is_empty() {
            b.push_str(&format!(
                "<text x=\"{}\" y=\"{}\" font-family=\"sans-serif\" font-size=\"1\" fill=\"#ffffff\" text-anchor=\"middle\">{}</text>
",
                bx0 as f64 - off_x + (bx1 - bx0 + 1) as f64 / 2.0,
                by0 as f64 - off_y + (by1 - by0 + 1) as f64 / 2.0,
                esc(&space.name)
            ));
        }
    }
    // Walls and doors: kind colours on top.
    for w in &walls {
        if w.pts.len() != 2 {
            continue;
        }
        let stroke = match w.kind {
            WallKind::Wall => "#d8e2ec",
            WallKind::Door => "#e8b030",
            WallKind::Window => "#7fb4d8",
            WallKind::Secret => "#b06ad0",
        };
        let [ax, ay] = to_cells(w.pts[0]);
        let [bx, by] = to_cells(w.pts[1]);
        b.push_str(&format!(
            "<line x1=\"{ax}\" y1=\"{ay}\" x2=\"{bx}\" y2=\"{by}\" stroke=\"{stroke}\" stroke-width=\"0.3\"/>
"
        ));
    }
    // Labels as real text.
    for l in &floor.labels {
        b.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" font-family=\"sans-serif\" font-size=\"{}\" fill=\"{}\"{}>{}</text>
",
            to_cells([l.x, l.y])[0], to_cells([l.x, l.y])[1],
            (l.size / 12.0).clamp(0.6, 6.0),
            esc(&l.color),
            if l.bold { " font-weight=\"bold\"" } else { "" },
            esc(&l.text)
        ));
    }
    b.push_str(
        "</svg>
",
    );
    drop(store);
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{doc_id_for_name}-{floor_name}.svg\""),
            ),
        ],
        b,
    )
        .into_response())
}

/// `GET /api/docs/{id}/export/roll20?floor=ID` — a Roll20 wall-data sidecar
/// (plan 21.13): the floor's wall segments as polylines in page pixels at
/// Roll20's default 70 px per square, plus the expected picture scale.
/// **Unverified live** — Roll20 imports walls through API scripts, and the
/// exact script shape varies; this emits the widely-used polyline array.
pub(crate) async fn export_roll20(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ExportQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    const ROLL20_PX_PER_SQUARE: f64 = 70.0;
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let idx = match &q.floor {
        Some(fid) => floors_by_id(&doc.floors, fid)?,
        None => 0,
    };
    let floor = ndcart_core::for_audience(&doc.floors[idx], pick_audience(&q)?);
    let cell = doc.manifest.cell_size;
    let walls = tiles::derive_walls(&floor, cell);
    let (cols, rows) = tiles::grid_extent(&floor);
    let s = ROLL20_PX_PER_SQUARE;
    let walls_out: Vec<_> = walls
        .iter()
        .filter(|w| w.pts.len() == 2)
        .map(|w| {
            serde_json::json!({
                "polyline": [
                    [ (w.pts[0][0] / cell * s).round(), (w.pts[0][1] / cell * s).round() ],
                    [ (w.pts[1][0] / cell * s).round(), (w.pts[1][1] / cell * s).round() ],
                ],
                "kind": match w.kind {
                    ndcart_core::WallKind::Wall => "wall",
                    ndcart_core::WallKind::Door => "door",
                    ndcart_core::WallKind::Window => "window",
                    ndcart_core::WallKind::Secret => "secret",
                },
                "state": w.state.map(|x| match x {
                    ndcart_core::WallState::Open => "open",
                    ndcart_core::WallState::Closed => "closed",
                }),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "page": {
            "width_px": (f64::from(cols) * s).round(),
            "height_px": (f64::from(rows) * s).round(),
            "scale_number": 5.0,
            "scale_units": "ft",
            "grid_type": "square",
        },
        "walls": walls_out,
        "note": "unverified live: import via your Roll20 API script of choice",
    })))
}

#[derive(Deserialize)]
pub(crate) struct PdfQuery {
    pub(crate) floor: Option<String>,
    /// `letter` (default) or `a4`.
    pub(crate) paper: Option<String>,
    /// Inches per square on paper (default 1: print and use miniatures).
    pub(crate) inch: Option<f32>,
    /// Print resolution (default 150).
    pub(crate) dpi: Option<u32>,
    pub(crate) audience: Option<String>,
}

/// `GET /api/docs/{id}/export/pdf?floor&paper&inch&dpi&audience` — the floor on
/// paper at true scale (one inch a square by default), cut into pages with
/// corner ticks and a "page r of n" label. The players' copy hides secrets.
pub(crate) async fn export_pdf(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PdfQuery>,
) -> Result<Response, ApiError> {
    let paper = pdf::paper(q.paper.as_deref().unwrap_or("letter"))
        .ok_or_else(|| ApiError(CoreError::Invalid("paper must be letter or a4".into())))?;
    let inch = q.inch.unwrap_or(1.0);
    let dpi = q.dpi.unwrap_or(150);
    if !(72..=300).contains(&dpi) {
        return Err(ApiError(CoreError::Invalid("dpi must be 72-300".into())));
    }
    let audience = match q.audience.as_deref() {
        None => ndcart_core::Audience::Gm,
        Some(a) => ndcart_core::Audience::parse(a)
            .ok_or_else(|| ApiError(CoreError::Invalid("audience must be gm or player".into())))?,
    };
    let (floor, name, cell_size) = {
        let store = app.store.read().await;
        let doc = store
            .docs
            .get(&id)
            .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
        let idx = match &q.floor {
            Some(f) => floors_by_id(&doc.floors, f)?,
            None => 0,
        };
        (
            ndcart_core::for_audience(&doc.floors[idx], audience),
            format!("{} - {}", doc.manifest.name, doc.floors[idx].name),
            doc.manifest.cell_size,
        )
    };
    let margin = 36.0;
    let _heavy = app.heavy.acquire().await;
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let (cols, rows) = tiles::grid_extent(&floor);
        let plan = pdf::plan(cols, rows, inch, paper, margin)?;
        let ppc = ((f64::from(dpi) * f64::from(inch)).round() as i64).clamp(8, 300);
        let walls = tiles::derive_walls(&floor, cell_size);
        let mut pages = Vec::with_capacity(plan.len());
        for r in &plan {
            let (px_w, px_h) = (r.cols * ppc as u32, r.rows * ppc as u32);
            let rgb = tiles::print_region(
                &floor,
                &walls,
                cell_size,
                ppc,
                (i64::from(r.cx) * ppc, i64::from(r.cy) * ppc, px_w, px_h),
            );
            pages.push(pdf::Page {
                rgb,
                px: (px_w, px_h),
                size_pt: (r.cols as f32 * inch * 72.0, r.rows as f32 * inch * 72.0),
                label: format!(
                    "{name} - page {} of {} (row {}, column {}) - {inch} in per square",
                    r.page, r.of, r.row, r.col
                ),
            });
        }
        pdf::build(paper, margin, &name, &pages)
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("pdf failed: {e}"))))?
    .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

/// `GET /api/docs/{id}/export/png[?floor=ID&ppc=N]` — the whole floor as one PNG.
pub(crate) async fn export_png(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let fmt = pick_format(&q)?;
    let audience = pick_audience(&q)?;
    let (filtered, cell_size) = {
        let store = app.store.read().await;
        let doc = store
            .docs
            .get(&id)
            .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
        let idx = match &q.floor {
            Some(fid) => floors_by_id(&doc.floors, fid)?,
            None => 0,
        };
        let floor = doc
            .floors
            .get(idx)
            .ok_or_else(|| CoreError::NotFound("floor".into()))?;
        (
            ndcart_core::for_audience(floor, audience),
            doc.manifest.cell_size,
        )
    };
    // A big export takes a while: bake off the async threads, without holding the project lock.
    let (ppc, quality) = (q.ppc.unwrap_or(32), q.quality.unwrap_or(80));
    // Exports honour the active era: what the GM sees is what is exported.
    let era = filtered
        .active_era
        .as_deref()
        .and_then(|id| filtered.era(id))
        .cloned();
    let _heavy = app.heavy.acquire().await;
    let (bytes, ..) = tokio::task::spawn_blocking(move || {
        let walls = tiles::derive_walls(&filtered, cell_size);
        tiles::bake_full_limit_era(
            &filtered,
            &walls,
            cell_size,
            ppc,
            fmt,
            quality,
            tiles::MAX_EXPORT_PX,
            era.as_ref(),
            0.0,
        )
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("export failed: {e}"))))?
    .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok((
        [
            (header::CONTENT_TYPE, fmt.mime()),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

/// `GET /api/docs/{id}/export/uvtt[?floor=ID]` — Universal VTT (walls, doors,
/// lights; no image yet: the baked underlay joins when the full-map render lands).
pub(crate) async fn export_uvtt(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ExportQuery>,
) -> Result<Response, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let idx = match &q.floor {
        Some(fid) => floors_by_id(&doc.floors, fid)?,
        None => 0,
    };
    let floor = doc
        .floors
        .get(idx)
        .ok_or_else(|| CoreError::NotFound("floor".into()))?;
    let filtered = ndcart_core::for_audience(floor, pick_audience(&q)?);
    let floor = &filtered;
    let cell_size = doc.manifest.cell_size;
    let walls = tiles::derive_walls(floor, cell_size);
    let (cols, rows) = tiles::grid_extent(floor);
    let ppc = q.ppc.unwrap_or(32);
    let fmt = pick_format(&q)?;
    if fmt == tiles::ImageFormat::Avif {
        return Err(ApiError(CoreError::Invalid(
            "UVTT pictures are png or webp (avif is not read by VTTs or nd-world)".into(),
        )));
    }
    let (png, ..) = tiles::bake_full_as(floor, &walls, cell_size, ppc, fmt, 90)
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    // The picture is `ppc` px per cell, so the UVTT lattice and walls use it too.
    let map = ndcart_formats::uvtt::UvttMap {
        cols,
        rows,
        cell_size: ppc as f64,
        walls: tiles::derive_walls(floor, ppc as f64),
        lights: floor.lights.clone(),
        image_b64: Some(ndcart_formats::base64::encode(&png)),
    };
    let name = format!(
        "{}.dd2vtt",
        doc.manifest.name.replace(['"', '/', '\\'], "_")
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        ndcart_formats::uvtt::to_uvtt(&map).to_string(),
    )
        .into_response())
}

#[derive(Deserialize)]
pub(crate) struct ConvertQuery {
    pub(crate) format: Option<String>,
    pub(crate) quality: Option<u8>,
}

/// `POST /api/convert?format=webp|avif|png[&quality=N]` — raw image bytes in
/// (PNG/JPEG/GIF/BMP/WebP), converted image out. Max 80 MB / 40 Mpx.
pub(crate) async fn convert_image(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<ConvertQuery>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    let fmt = match q.format.as_deref() {
        Some(f) => tiles::ImageFormat::parse(f).ok_or_else(|| {
            ApiError(CoreError::Invalid(format!(
                "unknown format {f:?} (png, webp or avif)"
            )))
        })?,
        None => tiles::ImageFormat::Webp,
    };
    let quality = q.quality.unwrap_or(80);
    let _heavy = app.heavy.acquire().await;
    let out = tokio::task::spawn_blocking(move || convert::convert(&body, fmt, quality))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("convert failed: {e}"))))?
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok((
        [
            (header::CONTENT_TYPE, fmt.mime().to_string()),
            ("x-image-width".parse().unwrap(), out.width.to_string()),
            ("x-image-height".parse().unwrap(), out.height.to_string()),
        ],
        out.bytes,
    )
        .into_response())
}
