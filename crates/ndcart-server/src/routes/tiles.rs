//! Map tiles and the player preview.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct PreviewQuery {
    pub(crate) floor: Option<String>,
    /// The viewer, in cells.
    pub(crate) x: Option<f64>,
    pub(crate) y: Option<f64>,
    /// How far the viewer sees, in cells (default 12).
    pub(crate) radius: Option<f64>,
    /// Darkness on (default): only lit areas show clearly.
    pub(crate) darkness: Option<bool>,
    pub(crate) ppc: Option<i64>,
    /// Animation phase for drifting materials (water): 0..3 frames cycle.
    pub(crate) frame: Option<f32>,
}

/// `GET /api/docs/{id}/preview?x=&y=&radius=&darkness=&ppc=` — what the table
/// sees from one point: the player's copy of the floor (secrets stripped)
/// under fog, with sight blocked by walls and closed doors and lamps lighting
/// what they reach. A PNG.
pub(crate) async fn preview(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<PreviewQuery>,
) -> Result<Response, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let idx = match &q.floor {
        Some(f) => floors_by_id(&doc.floors, f)?,
        None => 0,
    };
    let floor = ndcart_core::for_audience(&doc.floors[idx], ndcart_core::Audience::Player);
    let cell_size = doc.manifest.cell_size;
    drop(store);
    let (x, y) = (q.x.unwrap_or(0.5), q.y.unwrap_or(0.5));
    let radius = q.radius.unwrap_or(12.0);
    if !(x.is_finite() && y.is_finite() && radius.is_finite()) || !(0.5..=200.0).contains(&radius) {
        return Err(ApiError(CoreError::Invalid(
            "x and y must be numbers and radius 0.5-200".into(),
        )));
    }
    let (ppc, dark) = (q.ppc.unwrap_or(16), q.darkness.unwrap_or(true));
    let phase = q.frame.unwrap_or(0.0).clamp(0.0, 6.0);
    let _heavy = app.heavy.acquire().await;
    let (png, ..) = tokio::task::spawn_blocking(move || {
        tiles::bake_preview_frame(&floor, cell_size, ppc, [x, y], radius, dark, phase)
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("preview failed: {e}"))))?
    .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        png,
    )
        .into_response())
}

#[derive(Deserialize)]
pub(crate) struct TileQuery {
    /// Another floor of the same document to show faintly underneath.
    pub(crate) onion: Option<String>,
}

pub(crate) async fn get_tile(
    State(app): State<App>,
    Path((doc_id, floor_id, z, xy)): Path<(String, String, u32, String)>,
    axum::extract::Query(q): axum::extract::Query<TileQuery>,
) -> Result<Response, ApiError> {
    let (x, y) = {
        let mut parts = xy.splitn(2, '_');
        let (a, b) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let x: i64 = a.parse().map_err(|_| CoreError::Invalid("tile x".into()))?;
        let y: i64 = b.parse().map_err(|_| CoreError::Invalid("tile y".into()))?;
        (x, y)
    };
    let onion_id = q.onion.filter(|o| *o != floor_id);
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&doc_id)
        .ok_or_else(|| CoreError::NotFound(format!("document {doc_id}")))?;
    let idx = floors_by_id(&doc.floors, &floor_id)?;
    let cell_size = doc.manifest.cell_size;
    let rev = doc.rev;
    // The floor's active era changes what a tile looks like; the era id (or
    // its absence) is part of the cache key, and the op that set it bumped
    // the rev, which busts the browser's copy. Look in the cache before
    // cloning the floor: a hit must not pay for the clone.
    let era_id = doc.floors[idx].active_era.as_deref();
    let key = format!(
        "{doc_id}/{floor_id}/{z}/{x}_{y}/{rev}/{}/{}",
        onion_id.as_deref().unwrap_or("-"),
        era_id.unwrap_or("-")
    );
    let hit = app
        .tile_cache
        .lock()
        .expect("tile cache")
        .get(&key)
        .cloned();
    if let Some(bytes) = hit {
        return Ok(png_response(bytes));
    }
    let floor = doc.floors[idx].clone();
    let walls = walls_for(&app, &doc_id, &floor, cell_size, rev);
    // The optional floor shown faintly underneath (must be a floor of this document).
    let ghost = match &onion_id {
        Some(oid) => {
            let oi = floors_by_id(&doc.floors, oid)?;
            let of = doc.floors[oi].clone();
            let ow = walls_for(&app, &doc_id, &of, cell_size, rev);
            Some((of, ow))
        }
        None => None,
    };
    let era = floor
        .active_era
        .as_deref()
        .and_then(|id| floor.era(id))
        .cloned();
    // Bake without the store lock: edits are not blocked behind a tile. The
    // key carries the rev, so a bake that finishes after an edit lands under
    // its old rev and is never served.
    drop(store);
    let bytes = tokio::task::spawn_blocking(move || {
        let onion = ghost.as_ref().map(|(f, w)| (f, w.as_slice()));
        tiles::bake_tile_era(&floor, &walls, cell_size, z, x, y, onion, era.as_ref(), 0.0)
    })
    .await
    .map_err(|e| CoreError::Invalid(format!("tile bake failed: {e}")))?;
    let bytes = bytes::Bytes::from(bytes);
    app.tile_cache
        .lock()
        .expect("tile cache")
        .insert(key, bytes.clone(), bytes.len());
    Ok(png_response(bytes))
}

pub(crate) fn png_response(bytes: bytes::Bytes) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes,
    )
        .into_response()
}
