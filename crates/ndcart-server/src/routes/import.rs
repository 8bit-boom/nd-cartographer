//! Imports: Universal VTT, Foundry scenes and project backup bundles.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct ImportQuery {
    pub(crate) name: Option<String>,
}

/// `POST /api/import/foundry[?name=…]` — Foundry scene JSON in the body becomes a
/// new project with its walls, doors, windows and lights (no picture: Foundry's
/// data refers to one by path; add it afterwards as an underlay). One undoable step.
pub(crate) async fn import_foundry(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<ImportQuery>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    use ndcart_core::doc::ImportedWall;
    let text = std::str::from_utf8(&body)
        .map_err(|_| ApiError(CoreError::Invalid("a Foundry scene is UTF-8 JSON".into())))?;
    let scene =
        ndcart_formats::foundry::from_scene(text).map_err(|m| ApiError(CoreError::Invalid(m)))?;
    let g = scene.grid_px;
    let walls: Vec<ImportedWall> = scene
        .walls
        .iter()
        .map(|w| ImportedWall {
            pts: w
                .pts
                .iter()
                .map(|p| [(p[0] / g) as f32, (p[1] / g) as f32])
                .collect(),
            kind: w.kind,
            state: w.state,
        })
        .collect();
    let name = q
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| scene.name.clone());
    let mut ops = vec![
        Op::SetCanvas {
            canvas: Some([scene.cols.clamp(1, 20_000), scene.rows.clamp(1, 20_000)]),
        },
        Op::SetImportedWalls { walls },
    ];
    ops.extend(
        scene
            .lights
            .iter()
            .cloned()
            .map(|light| Op::AddLight { light }),
    );
    let mut store = app.store.write().await;
    let id = store.create(&name, DocKind::Battle).id.clone();
    let doc = store.docs.get_mut(&id).expect("just created");
    doc.manifest.cell_size = g;
    let floor_id = doc.floors[0].id.clone();
    if let Err(e) = doc.apply_ops(&floor_id, &[Op::Batch { ops }]) {
        store.docs.remove(&id);
        return Err(e.into());
    }
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    let manifest = store.docs[&id].manifest.clone();
    drop(store);
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({
        "manifest": manifest, "floor": floor_id,
        "stats": { "cols": scene.cols, "rows": scene.rows, "walls": scene.walls.len(), "lights": scene.lights.len() },
        "image": scene.image_path,
    })))
}

/// `POST /api/import/uvtt[?name=…]` — a Universal VTT file (`.dd2vtt` / `.uvtt`)
/// in the body becomes a new project: the picture is the underlay (stored as an
/// asset), walls and doors are imported walls, lights are lights, and the grid
/// is the file's squares. One undoable step. Rooms cannot be reconstructed from
/// line segments, so the project has none.
pub(crate) async fn import_uvtt(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<ImportQuery>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    use ndcart_core::doc::{ImportedWall, Underlay};
    let text = std::str::from_utf8(&body)
        .map_err(|_| ApiError(CoreError::Invalid("a UVTT file is UTF-8 JSON".into())))?;
    let map = ndcart_formats::uvtt::from_uvtt(text)
        .map_err(|e| ApiError(CoreError::Invalid(e.to_string())))?;
    let picture = map
        .image_b64
        .as_deref()
        .and_then(ndcart_formats::base64::decode)
        .ok_or_else(|| {
            ApiError(CoreError::Invalid(
                "the file has no readable picture".into(),
            ))
        })?;
    let asset = tokio::task::spawn_blocking(move || assets::global().put(&picture))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("import failed: {e}"))))?
        .map_err(|m| ApiError(CoreError::Invalid(format!("the picture: {m}"))))?;

    let g = map.cell_size;
    let walls: Vec<ImportedWall> = map
        .walls
        .iter()
        .map(|w| ImportedWall {
            pts: w
                .pts
                .iter()
                .map(|p| [(p[0] / g) as f32, (p[1] / g) as f32])
                .collect(),
            kind: w.kind,
            state: w.state,
        })
        .collect();
    let name = q
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Imported map");
    let mut ops = vec![
        Op::SetUnderlay {
            underlay: Some(Underlay {
                asset,
                x: 0.0,
                y: 0.0,
                w: map.cols as f32,
                h: map.rows as f32,
                opacity: 1.0,
            }),
        },
        Op::SetImportedWalls { walls },
    ];
    ops.extend(
        map.lights
            .iter()
            .cloned()
            .map(|light| Op::AddLight { light }),
    );
    let (wall_count, light_count) = (map.walls.len(), map.lights.len());

    let mut store = app.store.write().await;
    let id = store.create(name, DocKind::Battle).id.clone();
    let doc = store.docs.get_mut(&id).expect("just created");
    doc.manifest.cell_size = g;
    let floor_id = doc.floors[0].id.clone();
    if let Err(e) = doc.apply_ops(&floor_id, &[Op::Batch { ops }]) {
        store.docs.remove(&id); // never leave a half-imported project behind
        return Err(e.into());
    }
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    let manifest = store.docs[&id].manifest.clone();
    drop(store);
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({
        "manifest": manifest,
        "floor": floor_id,
        "stats": { "cols": map.cols, "rows": map.rows, "walls": wall_count, "lights": light_count },
    })))
}

/// Largest bundle `POST /api/import/bundle` accepts.
pub(crate) const MAX_BUNDLE_BYTES: usize = 200 << 20;

/// `POST /api/import/bundle` — a `.ndmapz` backup in the body comes back as a
/// new project (always a new id). All of it is validated before anything is
/// stored; a refused bundle creates nothing.
pub(crate) async fn import_bundle(
    State(app): State<App>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _heavy = app.heavy.acquire().await;
    let imp = tokio::task::spawn_blocking(move || bundle::read_bundle(&body))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("import failed: {e}"))))?
        .map_err(CoreError::Invalid)?;
    let imp = tokio::task::spawn_blocking(move || {
        bundle::store_assets(&imp, assets::global()).map(|()| imp.doc)
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("import failed: {e}"))))?
    .map_err(CoreError::Invalid)?;
    let id = imp.manifest.id.clone();
    let mut store = app.store.write().await;
    store.docs.insert(id.clone(), imp);
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    let doc = &store.docs[&id];
    let out = serde_json::json!({
        "id": id,
        "manifest": doc.manifest,
        "floors": doc.floors.iter().map(|f| serde_json::json!({"id": f.id, "name": f.name})).collect::<Vec<_>>(),
    });
    drop(store);
    invalidate_doc(&app, &id);
    Ok(Json(out))
}
