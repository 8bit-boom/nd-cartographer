//! The picture library: materials, props, packs and uploaded assets.

use crate::*;

/// `GET /api/materials` — the starter pack: id, name and the base colour for swatches.
pub(crate) async fn list_materials() -> Json<serde_json::Value> {
    let mut mats: Vec<serde_json::Value> = ndcart_paint::starter()
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "name": m.name,
                "color": format!("#{:02x}{:02x}{:02x}", m.base[0], m.base[1], m.base[2]),
            })
        })
        .collect();
    let a = assets::global();
    for c in a.customs("material") {
        // The swatch colour is the picture's average.
        let colour = a
            .get_pixmap(&c.asset)
            .map(|p| {
                let n = (p.pixels().len() as u64).max(1);
                let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
                for px in p.pixels() {
                    let d = px.demultiply();
                    r += u64::from(d.red());
                    g += u64::from(d.green());
                    b += u64::from(d.blue());
                }
                format!("#{:02x}{:02x}{:02x}", r / n, g / n, b / n)
            })
            .unwrap_or_else(|| "#888888".into());
        mats.push(serde_json::json!({
            "id": c.id, "name": c.name, "color": colour, "custom": true, "origin": c.origin,
        }));
    }
    Json(serde_json::json!({ "pack": "starter", "materials": mats }))
}

/// `POST /api/assets` — raw image bytes in; `{id}` (their SHA-256) out. Anything
/// that does not decode as an image within the size limits is refused.
pub(crate) async fn put_asset(
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = tokio::task::spawn_blocking(move || assets::global().put(&body))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("asset failed: {e}"))))?
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(serde_json::json!({ "id": id })))
}

/// `GET /api/assets/{id}` — the stored picture.
pub(crate) async fn get_asset(Path(id): Path<String>) -> Result<Response, ApiError> {
    let bytes = assets::global()
        .get_bytes(&id)
        .ok_or_else(|| CoreError::NotFound(format!("asset {id}")))?;
    let mime = match bytes.as_slice() {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xff, 0xd8, ..] => "image/jpeg",
        [b'G', b'I', b'F', ..] => "image/gif",
        b if b.len() > 12 && &b[8..12] == b"WEBP" => "image/webp",
        _ => "application/octet-stream",
    };
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes.to_vec(),
    )
        .into_response())
}

/// `GET /api/stamps` — the starter stamp pack.
pub(crate) async fn list_stamps() -> Json<serde_json::Value> {
    let mut stamps: Vec<serde_json::Value> = ndcart_paint::stamps()
        .iter()
        .map(|s| serde_json::json!({ "id": s.id, "name": s.name, "size": s.size }))
        .collect();
    let a = assets::global();
    for c in a.customs("stamp") {
        let aspect = a
            .get_pixmap(&c.asset)
            .map(|p| p.height() as f32 / p.width() as f32)
            .unwrap_or(1.0);
        stamps.push(serde_json::json!({
            "id": c.id, "name": c.name, "size": [c.size, c.size * aspect],
            "custom": true, "origin": c.origin, "tags": c.tags, "meta": c.meta,
        }));
    }
    Json(serde_json::json!({ "pack": "starter", "stamps": stamps }))
}

/// `POST /api/assets/{id}/meta` — set an asset's v2 metadata (footprint,
/// clearance, placement, style tags, licence). The asset's picture is
/// untouched.
pub(crate) async fn set_asset_meta(
    Path(id): Path<String>,
    body: Result<Json<ndcart_core::assetmeta::AssetMeta>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(meta) = body.map_err(|_| {
        ApiError(CoreError::Invalid(
            "body must be the metadata object".into(),
        ))
    })?;
    let kind = if meta.is_default() && assets::global().find_custom(&id, "material").is_some() {
        "material"
    } else {
        "stamp"
    };
    assets::global()
        .set_meta(&id, kind, meta)
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /api/stamps/{id}/icon.png` — a 64 px icon for the palette.
pub(crate) async fn stamp_icon_png(Path(id): Path<String>) -> Result<Response, ApiError> {
    let png = if let Some(def) = ndcart_paint::find_stamp(&id) {
        tiles::stamp_icon(def, 64)
    } else if let Some(c) = assets::global().find_custom(&id, "stamp") {
        let pm = assets::global()
            .get_pixmap(&c.asset)
            .ok_or_else(|| CoreError::NotFound(format!("stamp {id}")))?;
        tiles::picture_icon(&pm, 64)
    } else {
        return Err(ApiError(CoreError::NotFound(format!("stamp {id}"))));
    };
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        png,
    )
        .into_response())
}

#[derive(Deserialize)]
pub(crate) struct MakeBody {
    pub(crate) name: String,
    /// Material: cells per repeat (default 2). Stamp: width in cells (default 1.5).
    #[serde(default)]
    pub(crate) size: Option<f32>,
    /// Material: blend the picture so it tiles (default true).
    #[serde(default = "yes_bool")]
    pub(crate) seamless: bool,
    /// Stamp: the background colour to remove (default: guessed from the corners).
    #[serde(default)]
    pub(crate) key: Option<String>,
    #[serde(default)]
    pub(crate) tolerance: Option<f32>,
    /// Where it came from, kept with it.
    #[serde(default)]
    pub(crate) prompt: String,
    #[serde(default = "minus_one")]
    pub(crate) seed: i64,
    #[serde(default)]
    pub(crate) ai: bool,
}

pub(crate) fn minus_one() -> i64 {
    -1
}

pub(crate) fn provenance(b: &MakeBody) -> assets::Provenance {
    assets::Provenance {
        source: if b.ai { "ai" } else { "imported" }.into(),
        prompt: b.prompt.chars().take(500).collect(),
        seed: b.seed,
        date: ndcart_core::doc::now_iso(),
    }
}

pub(crate) fn check_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 60 {
        return Err(ApiError(CoreError::Invalid(
            "a name is 1-60 characters".into(),
        )));
    }
    Ok(n.to_string())
}

#[derive(Deserialize)]
pub(crate) struct TagsBody {
    pub(crate) tags: Vec<String>,
}

/// `POST /api/stamps/{id}/tags` `{tags}` — the GM's confirmed tags for a custom prop
/// (lowercase words, at most 8; invalid ones are refused, not silently dropped).
pub(crate) async fn set_stamp_tags(
    Path(id): Path<String>,
    body: Result<Json<TagsBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let cleaned = clean_tags_strict(&b.tags)?;
    assets::global()
        .find_custom(&id, "stamp")
        .ok_or_else(|| CoreError::NotFound(format!("custom prop {id}")))?;
    assets::global()
        .set_tags(&id, cleaned.clone())
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(serde_json::json!({ "ok": true, "tags": cleaned })))
}

pub(crate) fn clean_tags_strict(raw: &[String]) -> Result<Vec<String>, ApiError> {
    if raw.len() > 8 {
        return Err(ApiError(CoreError::Invalid("at most 8 tags".into())));
    }
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim().to_lowercase();
        let ok = !t.is_empty()
            && t.chars().count() <= 24
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-');
        if !ok {
            return Err(ApiError(CoreError::Invalid(format!(
                "tag {t:?} must be 1-24 characters: letters, digits, spaces, hyphens"
            ))));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// `POST /api/stamps/{id}/suggest-tags` — ask the AI for tags from the prop's
/// name and the prompt that made it. Returns suggestions only; nothing is saved.
pub(crate) async fn suggest_stamp_tags(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = assets::global()
        .find_custom(&id, "stamp")
        .ok_or_else(|| CoreError::NotFound(format!("custom prop {id}")))?;
    #[cfg(feature = "bridge")]
    {
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
        let tags = draft::suggest_tags(&cfg, &c.name, &c.origin.prompt)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        Ok(Json(serde_json::json!({ "tags": tags })))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = c;
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

#[derive(Deserialize)]
pub(crate) struct PackQuery {
    /// The file's name, used as the pack's name when it does not name itself.
    pub(crate) name: Option<String>,
}

pub(crate) fn report_json(r: &packs::Report) -> serde_json::Value {
    serde_json::json!({
        "pack": r.pack, "materials": r.materials, "stamps": r.stamps,
        "skipped": r.skipped, "notes": r.notes,
    })
}

/// `POST /api/packs/import?name=file.dungeondraft_pack` — the pack's bytes in the
/// body (up to 200 MB); its props and textures join the palettes. A Godot
/// `.pck`-style Dungeondraft pack is read directly; anything else is refused with
/// advice. Pictures keep "from pack X by Y" as their provenance.
pub(crate) async fn import_pack(
    axum::extract::Query(q): axum::extract::Query<PackQuery>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    if body.len() > packs::MAX_PACK_BYTES {
        return Err(ApiError(CoreError::Invalid(
            "that pack is over 200 MB".into(),
        )));
    }
    let fallback = q
        .name
        .as_deref()
        .map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n))
        .map(|n| n.rsplit_once('.').map(|(s, _)| s).unwrap_or(n))
        .filter(|n| !n.is_empty())
        .unwrap_or("Imported pack")
        .chars()
        .take(60)
        .collect::<String>();
    let report = tokio::task::spawn_blocking(move || -> Result<packs::Report, String> {
        if body.starts_with(b"PK") {
            return Err("that is a zip file; unzip it and import the folder with `ndcart import-pack FOLDER`".into());
        }
        let entries = packs::read_pck(&body)?;
        packs::import(&entries, &fallback, assets::global())
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("import failed: {e}"))))?
    .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(report_json(&report)))
}

#[derive(Deserialize)]
pub(crate) struct PackExportQuery {
    pub(crate) name: Option<String>,
    /// `all` also writes items imported from other people's packs (default: only your own).
    pub(crate) include: Option<String>,
}

/// `GET /api/packs/export?name=My%20set[&include=all]` — your own props and
/// materials as a Dungeondraft-style pack. Art that came from someone else's pack
/// is left out unless you ask for `include=all` (check its license first).
pub(crate) async fn export_pack(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<PackExportQuery>,
) -> Result<Response, ApiError> {
    let name = q.name.unwrap_or_default();
    let all = q.include.as_deref() == Some("all");
    let _heavy = app.heavy.acquire().await;
    let (bytes, ..) =
        tokio::task::spawn_blocking(move || packs::export_own(assets::global(), &name, all))
            .await
            .map_err(|e| ApiError(CoreError::Invalid(format!("export failed: {e}"))))?
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"ndcart.dungeondraft_pack\"",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        bytes,
    )
        .into_response())
}

/// `POST /api/assets/{id}/make-material` `{name, size?, seamless?, prompt?, seed?, ai?}` —
/// turn a picture into a paintable material: made seamless (unless told not
/// to), stored, and added to the palette. Provenance is kept.
pub(crate) async fn make_material(
    Path(id): Path<String>,
    body: Result<Json<MakeBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let name = check_name(&b.name)?;
    let size = b.size.unwrap_or(2.0);
    if !size.is_finite() || !(0.25..=40.0).contains(&size) {
        return Err(ApiError(CoreError::Invalid(
            "size must be 0.25-40 cells".into(),
        )));
    }
    let origin = provenance(&b);
    let seamless = b.seamless;
    let out = tokio::task::spawn_blocking(move || -> Result<(String, String), String> {
        let a = assets::global();
        let bytes = a.get_bytes(&id).ok_or("no such picture")?;
        let (rgba, w, h) = convert::decode_rgba(&bytes)?;
        let png = if seamless {
            synth::encode_png(&synth::make_tileable(&rgba, w, h, 0.3)?, w, h)?
        } else {
            synth::encode_png(&rgba, w, h)?
        };
        let asset = a.put(&png)?;
        let mid = format!("mat_{}", &asset[..12]);
        a.register(assets::Custom {
            meta: Default::default(),
            id: mid.clone(),
            name,
            kind: "material".into(),
            asset: asset.clone(),
            size,
            origin,
            tags: Vec::new(),
        })?;
        Ok((mid, asset))
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("failed: {e}"))))?
    .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(serde_json::json!({ "id": out.0, "asset": out.1 })))
}

/// `POST /api/assets/{id}/make-stamp` `{name, size?, key?, tolerance?, prompt?, seed?, ai?}` —
/// cut a prop out of its flat background (colour guessed from the corners
/// unless given), trim it, store it and add it to the stamp palette.
pub(crate) async fn make_stamp(
    Path(id): Path<String>,
    body: Result<Json<MakeBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let name = check_name(&b.name)?;
    let size = b.size.unwrap_or(1.5);
    if !size.is_finite() || !(0.3..=12.0).contains(&size) {
        return Err(ApiError(CoreError::Invalid(
            "width must be 0.3-12 cells".into(),
        )));
    }
    let key = match &b.key {
        Some(k) => {
            let t = k.as_bytes();
            if !(t.len() == 7 && t[0] == b'#' && t[1..].iter().all(u8::is_ascii_hexdigit)) {
                return Err(ApiError(CoreError::Invalid("key must be #rrggbb".into())));
            }
            Some(tiles::parse_hex(k))
        }
        None => None,
    };
    let tolerance = b.tolerance.unwrap_or(60.0);
    let origin = provenance(&b);
    let out = tokio::task::spawn_blocking(move || -> Result<(String, String), String> {
        let a = assets::global();
        let bytes = a.get_bytes(&id).ok_or("no such picture")?;
        let (rgba, w, h) = convert::decode_rgba(&bytes)?;
        let key = key.unwrap_or_else(|| synth::guess_key(&rgba, w, h));
        let (cut, cw, ch) = synth::cut_out(&rgba, w, h, key, tolerance)?;
        let asset = a.put(&synth::encode_png(&cut, cw, ch)?)?;
        let sid = format!("stp_{}", &asset[..12]);
        a.register(assets::Custom {
            meta: Default::default(),
            id: sid.clone(),
            name,
            kind: "stamp".into(),
            asset: asset.clone(),
            size,
            origin,
            tags: Vec::new(),
        })?;
        Ok((sid, asset))
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("failed: {e}"))))?
    .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(serde_json::json!({ "id": out.0, "asset": out.1 })))
}
