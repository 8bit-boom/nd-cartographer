//! Shared server state: the app handle, caches and their invalidation, the
//! API error mapping and the JSON views of a document.

use crate::*;

/// (doc, floor) -> (rev, derived walls): one entry per floor, replaced when
/// the rev moves on, so the cache cannot grow with the number of edits.
pub(crate) type WallCache = HashMap<(String, String), (u64, Arc<Vec<Wall>>)>;

/// Byte budget of the tile cache.
pub(crate) const TILE_CACHE_BYTES: usize = 128 << 20;

/// Drop every cached tile of one document.
pub(crate) fn invalidate_tiles(app: &App, doc_id: &str) {
    let prefix = format!("{doc_id}/");
    app.tile_cache
        .lock()
        .expect("tile cache")
        .retain_keys(|k| !k.starts_with(&prefix));
}

/// After an edit that moved the doc from `old_rev` to `new_rev` and changed
/// only `dirty`: drop the tiles that touch it and carry the rest over to the
/// new rev, so an edit does not make the whole viewport bake again. Tiles of
/// other revs are stale leftovers and go. Called with the store write lock
/// held, so it is ordered against `get_tile`.
pub(crate) fn invalidate_dirty(
    app: &App,
    doc_id: &str,
    floor_id: &str,
    old_rev: u64,
    new_rev: u64,
    dirty: ndcart_core::Dirty,
) {
    if dirty == ndcart_core::Dirty::All || new_rev != old_rev + 1 {
        invalidate_tiles(app, doc_id);
        return;
    }
    let prefix = format!("{doc_id}/");
    let old = old_rev.to_string();
    app.tile_cache.lock().expect("tile cache").rekey(|k| {
        let Some(rest) = k.strip_prefix(&prefix) else {
            return Some(k.to_string()); // another document's tile
        };
        // floor/z/x_y/rev/onion/era
        let mut p = rest.split('/');
        let (floor, z, xy, rev, onion, era) = (
            p.next()?,
            p.next()?,
            p.next()?,
            p.next()?,
            p.next()?,
            p.next()?,
        );
        if rev != old {
            return None;
        }
        // A tile that shows the edited floor as its ghost may change too.
        let affected = if floor == floor_id {
            let z: u32 = z.parse().ok()?;
            let (x, y) = xy.split_once('_')?;
            let (x, y): (i64, i64) = (x.parse().ok()?, y.parse().ok()?);
            // Tile (x, y) covers 256 px at 16 << z px per cell.
            let ppc = tiles::PPC0 << z.min(tiles::MAX_Z);
            let t = i64::from(tiles::TILE);
            let (cx0, cx1) = ((x * t).div_euclid(ppc), ((x + 1) * t - 1).div_euclid(ppc));
            let (cy0, cy1) = ((y * t).div_euclid(ppc), ((y + 1) * t - 1).div_euclid(ppc));
            dirty.touches(cx0, cy0, cx1, cy1)
        } else {
            onion == floor_id && dirty != ndcart_core::Dirty::Nothing
        };
        (!affected).then(|| format!("{prefix}{floor}/{z}/{xy}/{new_rev}/{onion}/{era}"))
    });
}

/// The `dirty` field of an edit's response: a cell rectangle
/// `[x0, y0, x1, y1]`, `[]` when no tile pixel changed, or null when the
/// whole picture may have.
pub(crate) fn dirty_json(d: ndcart_core::Dirty) -> serde_json::Value {
    match d {
        ndcart_core::Dirty::All => serde_json::Value::Null,
        ndcart_core::Dirty::Nothing => serde_json::json!([]),
        ndcart_core::Dirty::Rect { x0, y0, x1, y1 } => serde_json::json!([x0, y0, x1, y1]),
    }
}

/// What an edit of `floor_id` by `ops` can change in the picture.
pub(crate) fn dirty_of(doc: &store::StoredDoc, floor_id: &str, ops: &[Op]) -> ndcart_core::Dirty {
    match doc.floors.iter().find(|f| f.id == floor_id) {
        Some(f) => ndcart_core::dirty_for_ops(f, ops, &tiles::stamp_reach_cells),
        None => ndcart_core::Dirty::All,
    }
}

/// Drop one document's cached tiles and derived walls (after anything that
/// changes it outside the plain edit path).
pub(crate) fn invalidate_doc(app: &App, doc_id: &str) {
    invalidate_tiles(app, doc_id);
    app.wall_cache
        .lock()
        .expect("wall cache")
        .retain(|k, _| k.0 != doc_id);
}

pub(crate) struct AppState {
    pub(crate) store: tokio::sync::RwLock<store::Store>,
    pub(crate) data_dir: Option<PathBuf>,
    /// The studio's AI model settings ( Unsloth / Ollama / any
    /// OpenAI-compatible endpoint); loaded from the data dir, saved on POST.
    pub(crate) ai: Mutex<ai_settings::StoredAiSettings>,
    pub(crate) wall_cache: Mutex<WallCache>,
    /// "doc/floor/z/x_y/rev/onion/era" -> PNG bytes; least recently used
    /// tiles are evicted over [`TILE_CACHE_BYTES`].
    pub(crate) tile_cache: Mutex<cache::Lru<bytes::Bytes>>,
    /// Limits how many big bakes (exports, pdf, previews, convert) run at
    /// once, so a few parallel requests cannot exhaust memory.
    pub(crate) heavy: tokio::sync::Semaphore,
    /// Scoped tokens for the table screen (project id -> token); see `auth::gate`.
    pub(crate) table_tokens: auth::TableTokens,
}

pub(crate) type App = Arc<AppState>;

/// Permits for `AppState::heavy`: half the cores, at least one.
pub(crate) fn heavy_permits() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get() / 2)
        .max(1)
}

pub(crate) struct ApiError(pub(crate) CoreError);

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            CoreError::NotFound(_) => StatusCode::NOT_FOUND,
            CoreError::Invalid(_) | CoreError::OutOfBounds(_) => StatusCode::BAD_REQUEST,
            CoreError::CapExceeded(_) => StatusCode::UNPROCESSABLE_ENTITY,
        };
        (
            status,
            Json(serde_json::json!({"error": self.0.to_string()})),
        )
            .into_response()
    }
}

#[derive(Serialize)]
pub(crate) struct FloorView {
    id: String,
    name: String,
    digest: String,
    grid: RoomGrid,
    walls: Vec<Wall>,
    wall_tally: std::collections::BTreeMap<&'static str, usize>,
    lights: Vec<Light>,
    labels: Vec<Label>,
    outline: ndcart_core::Outline,
    #[serde(skip_serializing_if = "Option::is_none")]
    underlay: Option<ndcart_core::Underlay>,
    /// Region maps: what the terrain panel needs (the heights stay server-side).
    #[serde(skip_serializing_if = "Option::is_none")]
    terrain: Option<TerrainView>,
    rivers: Vec<ndcart_core::River>,
    #[serde(skip_serializing_if = "Option::is_none")]
    canvas: Option<[u32; 2]>,
    markers: Vec<ndcart_core::Marker>,
    regions: Vec<ndcart_core::Region>,
    routes: Vec<ndcart_core::Route>,
    zones: Vec<ndcart_core::Zone>,
    stamps: Vec<ndcart_core::Stamp>,
    entity_links: Vec<ndcart_core::doc::EntityLink>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    encounters: Vec<ndcart_core::doc::Encounter>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    eras: Vec<ndcart_core::doc::Era>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_era: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    map_grid: Option<ndcart_core::MapGrid>,
}

#[derive(Serialize)]
pub(crate) struct TerrainView {
    style: ndcart_core::TerrainStyle,
    chunks: usize,
}

#[derive(Serialize)]
pub(crate) struct DocView {
    manifest: Manifest,
    rev: u64,
    floors: Vec<FloorView>,
}

/// The floor's derived walls at `rev`: cached when the rev matches, otherwise
/// derived and stored in place of the floor's previous entry.
pub(crate) fn walls_for(
    app: &App,
    doc_id: &str,
    floor: &FloorDoc,
    cell_size: f64,
    rev: u64,
) -> Arc<Vec<Wall>> {
    let mut cache = app.wall_cache.lock().expect("wall cache");
    let key = (doc_id.to_string(), floor.id.clone());
    if let Some((r, w)) = cache.get(&key) {
        if *r == rev {
            return w.clone();
        }
    }
    let w = Arc::new(tiles::derive_walls(floor, cell_size));
    cache.insert(key, (rev, w.clone()));
    w
}

pub(crate) fn floor_view(
    doc_id: &str,
    app: &App,
    floor: &FloorDoc,
    cell_size: f64,
    rev: u64,
) -> FloorView {
    let walls = walls_for(app, doc_id, floor, cell_size, rev);
    FloorView {
        id: floor.id.clone(),
        name: floor.name.clone(),
        digest: floor.digest(),
        grid: floor.grid.clone(),
        walls: (*walls).clone(),
        wall_tally: tally(&walls),
        lights: floor.lights.clone(),
        labels: floor.labels.clone(),
        outline: ndcart_core::outline(floor, cell_size),
        underlay: floor.underlay.clone(),
        terrain: floor.terrain.as_ref().map(|t| TerrainView {
            style: t.style.clone(),
            chunks: t.chunk_count(),
        }),
        rivers: floor.rivers.clone(),
        canvas: floor.canvas,
        markers: floor.markers.clone(),
        regions: floor.regions.clone(),
        routes: floor.routes.clone(),
        zones: floor.zones.clone(),
        stamps: floor.stamps.clone(),
        entity_links: floor.entity_links.clone(),
        encounters: floor.encounters.clone(),
        eras: floor.eras.clone(),
        active_era: floor.active_era.clone(),
        map_grid: floor.map_grid.clone(),
    }
}

pub(crate) fn doc_view(app: &App, doc: &store::StoredDoc) -> DocView {
    DocView {
        manifest: doc.manifest.clone(),
        rev: doc.rev,
        floors: doc
            .floors
            .iter()
            .map(|f| floor_view(&doc.manifest.id, app, f, doc.manifest.cell_size, doc.rev))
            .collect(),
    }
}
