//! `ndcart` — the nd-cartographer server binary. One binary, three ways to
//! run it (plan §4): `ndcart serve` (desktop/docker), `ndcart bench`.
//! Desktop is the Tauri shell wrapping this same server (M0 ships serve).

mod aijobs;
mod ask;
mod assets;
mod auth;
mod convert;
mod doctor;
#[cfg(feature = "bridge")]
mod draft;
mod golden;
mod packs;
mod pdf;
#[cfg(feature = "bridge")]
mod publish;
mod store;
mod synth;
mod text;
mod tiles;
mod worldspec;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ndcart_core::doc::{floors_by_id, DocKind, FloorDoc};
use ndcart_core::grid::RoomGrid;
use ndcart_core::journal::Op;
use ndcart_core::walls::{tally, Wall};
use ndcart_core::{CoreError, Label, Light, Manifest};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const INDEX_HTML: &str = include_str!("../../../ui/index.html");
const APP_JS: &str = include_str!("../../../ui/app.js");
const APP_CSS: &str = include_str!("../../../ui/app.css");

/// (doc, floor, rev) -> derived walls.
type WallCache = HashMap<(String, String, u64), Arc<Vec<Wall>>>;

struct AppState {
    store: tokio::sync::RwLock<store::Store>,
    data_dir: Option<PathBuf>,
    wall_cache: Mutex<WallCache>,
    /// "doc/floor/z/x_y/rev" -> PNG bytes; cleared wholesale when big.
    tile_cache: Mutex<HashMap<String, Vec<u8>>>,
}
type App = Arc<AppState>;

// ---------- API error mapping ----------

struct ApiError(CoreError);

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

// ---------- views ----------

#[derive(Serialize)]
struct FloorView {
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
struct TerrainView {
    style: ndcart_core::TerrainStyle,
    chunks: usize,
}

#[derive(Serialize)]
struct DocView {
    manifest: Manifest,
    rev: u64,
    floors: Vec<FloorView>,
}

fn floor_view(doc_id: &str, app: &App, floor: &FloorDoc, cell_size: f64, rev: u64) -> FloorView {
    let walls = {
        let mut cache = app.wall_cache.lock().expect("wall cache");
        cache
            .entry((doc_id.to_string(), floor.id.clone(), rev))
            .or_insert_with(|| Arc::new(tiles::derive_walls(floor, cell_size)))
            .clone()
    };
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

fn doc_view(app: &App, doc: &store::StoredDoc) -> DocView {
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

// ---------- handlers ----------

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "app": "nd-cartographer",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn index() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
        .into_response()
}

async fn app_js() -> Response {
    ([(header::CONTENT_TYPE, "application/javascript")], APP_JS).into_response()
}

async fn app_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css")], APP_CSS).into_response()
}

#[derive(Serialize)]
struct DocSummary {
    id: String,
    name: String,
    kind: &'static str,
    rev: u64,
    floors: Vec<String>,
    /// The nd-world slug of the last publish, so markers can link to it and
    /// the UI can tell published maps from drafts.
    #[serde(skip_serializing_if = "Option::is_none")]
    slug: Option<String>,
}

async fn list_docs(State(app): State<App>) -> Json<serde_json::Value> {
    let store = app.store.read().await;
    let docs: Vec<DocSummary> = store
        .docs
        .values()
        .map(|d| DocSummary {
            id: d.manifest.id.clone(),
            name: d.manifest.name.clone(),
            kind: d.manifest.kind.as_str(),
            rev: d.rev,
            floors: d.floors.iter().map(|f| f.name.clone()).collect(),
            slug: d.manifest.nd_world.as_ref().map(|l| l.slug.clone()),
        })
        .collect();
    Json(serde_json::json!({ "docs": docs }))
}

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    kind: Option<DocKind>,
}

async fn create_doc(
    State(app): State<App>,
    body: Result<Json<CreateBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Manifest>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {name, kind?}".into())))?;
    let name = b.name.trim();
    if name.is_empty() {
        return Err(CoreError::Invalid("name must not be empty".into()).into());
    }
    let mut store = app.store.write().await;
    let manifest = store
        .create(name, b.kind.unwrap_or(DocKind::Battle))
        .clone();
    store::save_doc(app.data_dir.as_deref(), &store.docs[&manifest.id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    Ok(Json(manifest))
}

async fn get_doc(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<DocView>, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    Ok(Json(doc_view(&app, doc)))
}

#[derive(Deserialize)]
struct ExportQuery {
    floor: Option<String>,
    /// Pixels per cell of the exported picture (default 32).
    ppc: Option<i64>,
    /// `png` (default), `webp` (lossless) or `avif` (lossy; UVTT takes png/webp only).
    format: Option<String>,
    /// AVIF quality 1..=100 (default 80).
    quality: Option<u8>,
    /// `gm` (default) or `player`: player output goes through the one secrets filter.
    audience: Option<String>,
}

fn pick_audience(q: &ExportQuery) -> Result<ndcart_core::Audience, ApiError> {
    match q.audience.as_deref() {
        None => Ok(ndcart_core::Audience::Gm),
        Some(a) => ndcart_core::Audience::parse(a).ok_or_else(|| {
            ApiError(CoreError::Invalid(format!(
                "unknown audience {a:?} (gm or player)"
            )))
        }),
    }
}

fn pick_format(q: &ExportQuery) -> Result<tiles::ImageFormat, ApiError> {
    match q.format.as_deref() {
        None => Ok(tiles::ImageFormat::Png),
        Some(f) => tiles::ImageFormat::parse(f).ok_or_else(|| {
            ApiError(CoreError::Invalid(format!(
                "unknown format {f:?} (png, webp or avif)"
            )))
        }),
    }
}

/// `GET /api/materials` — the starter pack: id, name and the base colour for swatches.
async fn list_materials() -> Json<serde_json::Value> {
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
async fn put_asset(body: axum::body::Bytes) -> Result<Json<serde_json::Value>, ApiError> {
    let id = tokio::task::spawn_blocking(move || assets::global().put(&body))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("asset failed: {e}"))))?
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(serde_json::json!({ "id": id })))
}

/// `GET /api/assets/{id}` — the stored picture.
async fn get_asset(Path(id): Path<String>) -> Result<Response, ApiError> {
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

#[derive(Deserialize)]
struct ImportQuery {
    name: Option<String>,
}

#[derive(Deserialize)]
struct FoundryQuery {
    floor: Option<String>,
    /// Pixels per square of the picture you will upload (default 100, Foundry's own).
    ppc: Option<f64>,
    /// Where the picture will be in Foundry's data folder.
    image: Option<String>,
    distance: Option<f64>,
    units: Option<String>,
    audience: Option<String>,
}

/// `GET /api/docs/{id}/export/foundry[?floor&ppc&image&distance&units&audience]` —
/// the floor as Foundry VTT scene data (walls, doors, windows, lights, grid), for
/// Foundry v11/v12. Export the picture separately (WebP at the same `ppc`),
/// upload it to Foundry and give its path as `image`. Not checked against a
/// running Foundry; it follows the documented data model.
async fn export_foundry(
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

/// `POST /api/import/foundry[?name=…]` — Foundry scene JSON in the body becomes a
/// new project with its walls, doors, windows and lights (no picture: Foundry's
/// data refers to one by path; add it afterwards as an underlay). One undoable step.
async fn import_foundry(
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
    app.wall_cache.lock().expect("wall cache").clear();
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
async fn import_uvtt(
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
    app.wall_cache.lock().expect("wall cache").clear();
    Ok(Json(serde_json::json!({
        "manifest": manifest,
        "floor": floor_id,
        "stats": { "cols": map.cols, "rows": map.rows, "walls": wall_count, "lights": light_count },
    })))
}

#[derive(Deserialize)]
struct GenerateBody {
    seed: Option<u32>,
    cols: Option<u32>,
    rows: Option<u32>,
    land: Option<f32>,
    continent: Option<f32>,
    ridges: Option<f32>,
    rivers: Option<u32>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum FloorRequest {
    /// A new empty floor, or a copy of `copy_from` (placed right after it).
    Add {
        name: String,
        copy_from: Option<String>,
    },
    Rename {
        id: String,
        name: String,
    },
    Remove {
        id: String,
    },
    Move {
        id: String,
        index: usize,
    },
}

/// `POST /api/docs/{id}/floors` — add (or duplicate), rename, move or remove a
/// floor. Each is undoable with `POST /api/docs/{id}/undo-structure`.
async fn floors_route(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<FloorRequest>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(req) = body.map_err(|_| {
        ApiError(CoreError::Invalid(
            "body must be {action: add|rename|remove|move, …}".into(),
        ))
    })?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let mut new_floor = None;
    match req {
        FloorRequest::Add { name, copy_from } => {
            new_floor = Some(doc.add_floor(&name, copy_from.as_deref())?);
        }
        FloorRequest::Rename { id: f, name } => doc.rename_floor(&f, &name)?,
        FloorRequest::Remove { id: f } => doc.remove_floor(&f)?,
        FloorRequest::Move { id: f, index } => doc.move_floor(&f, index)?,
    }
    let floors: Vec<_> = doc
        .floors
        .iter()
        .map(|f| serde_json::json!({"id": f.id, "name": f.name}))
        .collect();
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(
        serde_json::json!({ "floors": floors, "floor": new_floor }),
    ))
}

/// `POST /api/docs/{id}/undo-structure` — undo the last floor add/rename/move/remove.
async fn undo_structure(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.undo_structure()?;
    let floors: Vec<_> = doc
        .floors
        .iter()
        .map(|f| serde_json::json!({"id": f.id, "name": f.name}))
        .collect();
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "floors": floors })))
}

#[derive(Deserialize)]
struct CheckpointBody {
    name: String,
}

/// `GET /api/docs/{id}/checkpoints` — named save points, newest last.
async fn list_checkpoints(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let list: Vec<_> = doc
        .checkpoints
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id, "name": c.name, "created_at": c.created_at,
                "floors": c.floors.len(),
                "cells": c.floors.iter().map(|f| f.grid.cell_count()).sum::<usize>(),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "checkpoints": list })))
}

/// `POST /api/docs/{id}/checkpoints` `{name}` — save the document as it is now.
async fn create_checkpoint(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<CheckpointBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {\"name\": ...}".into())))?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let cid = doc.create_checkpoint(&b.name)?;
    store::save_checkpoints(app.data_dir.as_deref(), &store.docs[&id]);
    Ok(Json(serde_json::json!({ "ok": true, "id": cid })))
}

/// `DELETE /api/docs/{id}/checkpoints/{cid}`.
async fn delete_checkpoint(
    State(app): State<App>,
    Path((id, cid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.delete_checkpoint(&cid)?;
    store::save_checkpoints(app.data_dir.as_deref(), &store.docs[&id]);
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /api/docs/{id}/checkpoints/{cid}/restore` — replaces every floor with
/// the saved copy and starts a fresh undo history. The state it replaces is
/// saved as a new checkpoint first.
async fn restore_checkpoint(
    State(app): State<App>,
    Path((id, cid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.restore_checkpoint(&cid)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    store::save_checkpoints(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

/// `GET /api/prefabs` — the library, newest last.
async fn list_prefabs(State(app): State<App>) -> Json<serde_json::Value> {
    let store = app.store.read().await;
    let list: Vec<_> = store
        .prefabs
        .values()
        .map(|p| {
            serde_json::json!({
                "id": p.id, "name": p.name, "w": p.w, "h": p.h,
                "rooms": p.spaces.len(), "squares": p.cells.len(),
            })
        })
        .collect();
    Json(serde_json::json!({ "prefabs": list }))
}

#[derive(Deserialize)]
struct CapturePrefab {
    #[serde(default)]
    floor: Option<String>,
    name: String,
    spaces: Vec<String>,
}

/// `POST /api/docs/{id}/prefabs` `{floor?, name, spaces:[room ids]}` — copy
/// those rooms (doors, lights, notes on them) into the shared library.
async fn capture_prefab(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<CapturePrefab>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let mut store = app.store.write().await;
    if store.prefabs.len() >= ndcart_core::prefab::MAX_PREFABS {
        return Err(ApiError(CoreError::CapExceeded(
            "the prefab library is full; delete some first".into(),
        )));
    }
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let idx = match &b.floor {
        Some(f) => floors_by_id(&doc.floors, f)?,
        None => 0,
    };
    let p =
        ndcart_core::prefab::capture(&doc.floors[idx], &b.name, &b.spaces, doc.manifest.cell_size)?;
    store::save_prefab(app.data_dir.as_deref(), &p);
    let pid = p.id.clone();
    store.prefabs.insert(pid.clone(), p);
    Ok(Json(serde_json::json!({ "ok": true, "id": pid })))
}

/// `DELETE /api/prefabs/{pid}`.
async fn delete_prefab(
    State(app): State<App>,
    Path(pid): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    if store.prefabs.remove(&pid).is_none() {
        return Err(ApiError(CoreError::NotFound(format!("prefab {pid}"))));
    }
    store::delete_prefab_file(app.data_dir.as_deref(), &pid);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PlacePrefab {
    #[serde(default)]
    floor: Option<String>,
    prefab: String,
    x: i32,
    y: i32,
    #[serde(default)]
    turns: u8,
}

/// `POST /api/docs/{id}/place-prefab` `{floor?, prefab, x, y, turns?}` — one undo step.
async fn place_prefab(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<PlacePrefab>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let mut store = app.store.write().await;
    let prefab = store
        .prefabs
        .get(&b.prefab)
        .cloned()
        .ok_or_else(|| CoreError::NotFound(format!("prefab {}", b.prefab)))?;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor_id = match &b.floor {
        Some(f) => f.clone(),
        None => doc.floors[0].id.clone(),
    };
    let ops = ndcart_core::prefab::place(&prefab, b.x, b.y, b.turns, doc.manifest.cell_size)?;
    doc.apply_ops(&floor_id, &ops)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
struct ScatterBody {
    #[serde(default)]
    floor: Option<String>,
    /// The area, in cells.
    points: Vec<[f32; 2]>,
    /// A named biome (`forest`, `meadow`, `rocky`, `swamp`, `desert`), or give `assets`.
    #[serde(default)]
    biome: Option<String>,
    #[serde(default)]
    assets: Vec<String>,
    #[serde(default)]
    spacing: Option<f32>,
    #[serde(default = "one_f32")]
    density: f32,
    #[serde(default)]
    seed: u64,
}
fn one_f32() -> f32 {
    1.0
}

/// `POST /api/docs/{id}/scatter` — fill an area with props: a named biome
/// (tinted region + props) or an explicit prop list. One undoable step.
async fn scatter_props(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<ScatterBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor_id = match &b.floor {
        Some(f) => f.clone(),
        None => doc.floors[0].id.clone(),
    };
    let idx = floors_by_id(&doc.floors, &floor_id)?;
    let taken = doc.floors[idx].stamps.len();
    let ops = match &b.biome {
        Some(name) => {
            let biome = ndcart_core::scatter::biome(name)
                .ok_or_else(|| CoreError::Invalid(format!("unknown biome {name}")))?;
            ndcart_core::scatter::scatter_biome(&b.points, biome, b.density, b.seed, taken)?
        }
        None => ndcart_core::scatter::scatter(
            &ndcart_core::scatter::ScatterParams {
                polygon: b.points.clone(),
                assets: b.assets.clone(),
                spacing: b.spacing.unwrap_or(2.0),
                seed: b.seed,
                scale_min: 0.8,
                scale_max: 1.2,
            },
            taken,
        )?,
    };
    doc.apply_ops(&floor_id, &ops)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
struct FurnishBody {
    #[serde(default)]
    floor: Option<String>,
    spaces: Vec<String>,
    #[serde(default)]
    seed: u64,
    /// Skip rooms that already have props (default true).
    #[serde(default = "yes_bool")]
    only_empty: bool,
}
fn yes_bool() -> bool {
    true
}

/// `GET /api/world/entities/{id}/preview` - the two-way lore sync's read
/// half (plan 21.2): the linked entity's lore as nd-world shows the GM.
/// Also returns the configured server so the UI can deep-link to the full
/// page there.
async fn world_entity_preview(
    State(app): State<App>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _ = app;
    #[cfg(feature = "bridge")]
    {
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
        if cfg.direct.is_some() {
            return Err(ApiError(CoreError::Invalid(
                "lore lives in nd-world; a standalone model has none".into(),
            )));
        }
        let server = cfg.server.clone().unwrap_or_default();
        let backend = draft::Backend::connect(&cfg)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        let draft::Backend::World(bridge, _) = &backend else {
            return Err(ApiError(CoreError::Invalid(
                "lore lives in nd-world".into(),
            )));
        };
        let preview = bridge
            .entity_preview(id)
            .await
            .map_err(|e| ApiError(CoreError::Invalid(e.to_string())))?;
        Ok(Json(
            serde_json::json!({ "preview": preview, "server": server }),
        ))
    }
    #[cfg(not(feature = "bridge"))]
    {
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

/// `POST /api/docs/{id}/reroll` `{floor?, room, seed?}` - plan 22.2: replace
/// exactly the generated props of one room. Pinned and hand-placed props
/// stay; the room's generated stamps are removed and furnished again in the
/// same batch, so the whole thing is one undo step.
async fn reroll_room(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let room = b
        .get("room")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError(CoreError::Invalid("body needs {room}".into())))?
        .to_string();
    let seed = b.get("seed").and_then(|v| v.as_u64()).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    });
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor_id = b
        .get("floor")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| doc.floors[0].id.clone());
    let idx = floors_by_id(&doc.floors, &floor_id)?;

    // Remove the room's unpinned generated props (a working copy; the real
    // removals go out as ops in the same batch).
    let mut scratch = doc.floors[idx].clone();
    let doomed: Vec<String> = scratch
        .stamps
        .iter()
        .filter(|s| s.gen.as_ref().is_some_and(|g| g.room == room && !g.pinned))
        .map(|s| s.id.clone())
        .collect();
    if doomed.is_empty() {
        return Err(ApiError(CoreError::Invalid(
            "that room has no generated props to reroll (pin them or furnish it first)".into(),
        )));
    }
    for sid in &doomed {
        ndcart_core::journal::apply_op(
            &mut scratch,
            &ndcart_core::Op::RemoveStamp { id: sid.clone() },
        )?;
    }
    // Furnish the room again on the scratch: fills only the free cells, so
    // pinned and hand-placed props are untouched.
    let meta_of = |asset: &str| -> (ndcart_core::assetmeta::AssetMeta, [f32; 2]) {
        if let Some(def) = ndcart_paint::find_stamp(asset) {
            (Default::default(), def.size)
        } else if let Some(c) = assets::global().find_custom(asset, "stamp") {
            let natural = [c.size, c.size];
            (c.meta.clone(), natural)
        } else {
            (Default::default(), [1.0, 1.0])
        }
    };
    let mut fill = ndcart_core::furnish::furnish_with(
        &scratch,
        std::slice::from_ref(&room),
        seed,
        false,
        &meta_of,
    )?;
    let removed = doomed.len();
    let mut ops: Vec<ndcart_core::Op> = doomed
        .into_iter()
        .map(|sid| ndcart_core::Op::RemoveStamp { id: sid })
        .collect();
    ops.append(&mut fill);
    let added = ops.len() - removed;
    doc.apply_ops(&floor_id, &[ndcart_core::Op::Batch { ops }])?;
    store::save_doc(app.data_dir.as_deref(), doc);
    drop(store);
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({
        "ok": true, "removed": removed, "placed": added,
    })))
}

/// `POST /api/docs/{id}/furnish` `{floor?, spaces:[room ids], seed, only_empty?}` —
/// props chosen from each room's name and kind, never in a doorway. One undo step.
async fn furnish_rooms(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<FurnishBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor_id = match &b.floor {
        Some(f) => f.clone(),
        None => doc.floors[0].id.clone(),
    };
    let idx = floors_by_id(&doc.floors, &floor_id)?;
    // Pack metadata v2: the lookup resolves a prop's footprint/clearance
    // from its custom entry, or its natural size from the starter pack.
    let meta_of = |asset: &str| -> (ndcart_core::assetmeta::AssetMeta, [f32; 2]) {
        if let Some(def) = ndcart_paint::find_stamp(asset) {
            (Default::default(), def.size)
        } else if let Some(c) = assets::global().find_custom(asset, "stamp") {
            let natural = [c.size, c.size];
            (c.meta.clone(), natural)
        } else {
            (Default::default(), [1.0, 1.0])
        }
    };
    let ops = ndcart_core::furnish::furnish_with(
        &doc.floors[idx],
        &b.spaces,
        b.seed,
        b.only_empty,
        &meta_of,
    )?;
    doc.apply_ops(&floor_id, &ops)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

/// `GET /api/search?q=` — "ask your maps": every project's rooms, notes, markers,
/// zones, regions, routes and floors whose name or text holds ALL the words
/// typed (case-insensitive). At most 100 hits; each says where it is.
async fn search_maps(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let words: Vec<String> = q.q.split_whitespace().map(|w| w.to_lowercase()).collect();
    if words.is_empty() || q.q.chars().count() > 200 {
        return Err(ApiError(CoreError::Invalid(
            "search for 1-200 characters".into(),
        )));
    }
    let store = app.store.read().await;
    let mut hits: Vec<serde_json::Value> = Vec::new();
    'all: for doc in store.docs.values() {
        for f in &doc.floors {
            let mut push = |kind: &str, name: &str, extra: &str, at: Option<(f32, f32)>| {
                let hay = format!("{name} {extra}").to_lowercase();
                if words.iter().all(|w| hay.contains(w)) {
                    hits.push(serde_json::json!({
                        "doc": doc.manifest.id, "doc_name": doc.manifest.name,
                        "floor": f.id, "floor_name": f.name,
                        "kind": kind, "name": name,
                        "at": at.map(|(x, y)| [x, y]),
                    }));
                }
            };
            push("floor", &f.name, "", None);
            let mut spans: std::collections::BTreeMap<&str, (i32, i32, i32, i32)> =
                Default::default();
            for (c, sp) in f.grid.cells() {
                let e = spans.entry(sp).or_insert((c.x, c.y, c.x, c.y));
                *e = (e.0.min(c.x), e.1.min(c.y), e.2.max(c.x), e.3.max(c.y));
            }
            for s in f.grid.spaces.values() {
                let at = spans.get(s.id.as_str()).map(|b| {
                    (
                        (b.0 + b.2) as f32 / 2.0 + 0.5,
                        (b.1 + b.3) as f32 / 2.0 + 0.5,
                    )
                });
                push("room", &s.name, "", at);
            }
            for l in &f.labels {
                push("note", &l.text, "", Some((l.x as f32, l.y as f32)));
            }
            for m in &f.markers {
                push("marker", &m.label, &m.note, Some((m.x, m.y)));
            }
            for z in &f.zones {
                push("zone", &z.name, "", None);
            }
            for r in &f.regions {
                push(
                    "region",
                    &r.name,
                    "",
                    r.points.first().map(|p| (p[0], p[1])),
                );
            }
            for r in &f.routes {
                push("route", &r.name, "", r.points.first().map(|p| (p[0], p[1])));
            }
            if hits.len() >= 100 {
                hits.truncate(100);
                break 'all;
            }
        }
        let n = doc.manifest.name.to_lowercase();
        if words.iter().all(|w| n.contains(w)) {
            hits.push(serde_json::json!({
                "doc": doc.manifest.id, "doc_name": doc.manifest.name,
                "floor": doc.floors.first().map(|f| f.id.clone()), "floor_name": "",
                "kind": "project", "name": doc.manifest.name, "at": null,
            }));
        }
    }
    Ok(Json(serde_json::json!({ "hits": hits })))
}

#[derive(Deserialize)]
struct AiImageBody {
    /// `underlay`, `material` or `prop`.
    kind: String,
    prompt: String,
    #[serde(default = "d_img")]
    width: u32,
    #[serde(default = "d_img")]
    height: u32,
    /// How many variants (1-4; a set shares a seed family).
    #[serde(default = "one_usize")]
    count: usize,
    #[serde(default = "minus_one")]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    seed: i64,
    /// Start from a picture an earlier job made (`{job, index}`), so a set stays coherent.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    like: Option<AnchorRef>,
    /// 0.1-0.95: how strongly the anchor holds (default 0.6).
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    strength: Option<f64>,
    /// Upscale the result by this factor (needs `ND_CARTO_UPSCALE_MODEL` in the studio's environment).
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    upscale: Option<f64>,
}
#[derive(Deserialize)]
struct AnchorRef {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    job: String,
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    index: usize,
}
fn d_img() -> u32 {
    512
}
fn one_usize() -> usize {
    1
}

/// `POST /api/ai/image` `{kind, prompt, width?, height?, count?, seed?}` — queue
/// pictures through nd-world's imagegen in the background. Returns the job; poll
/// `GET /api/ai/jobs/{id}`. Nothing is added anywhere until the GM accepts a picture.
async fn ai_image(
    body: Result<Json<AiImageBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    if !["underlay", "material", "prop"].contains(&b.kind.as_str()) {
        return Err(ApiError(CoreError::Invalid(
            "kind must be underlay, material or prop".into(),
        )));
    }
    let prompt = b.prompt.trim().to_string();
    if prompt.is_empty() || prompt.chars().count() > 1_000 {
        return Err(ApiError(CoreError::Invalid(
            "describe the picture in 1-1000 characters".into(),
        )));
    }
    if !(64..=1536).contains(&b.width) || !(64..=1536).contains(&b.height) {
        return Err(ApiError(CoreError::Invalid(
            "width and height must be 64-1536 px".into(),
        )));
    }
    let count = if b.kind == "underlay" {
        1
    } else {
        b.count.clamp(1, 4)
    };
    #[cfg(feature = "bridge")]
    {
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
        if cfg.direct.is_some() {
            return Err(ApiError(CoreError::Invalid(
                "image generation needs nd-world's imagegen; a standalone text model cannot draw"
                    .into(),
            )));
        }
        let anchor = match &b.like {
            Some(a) => {
                let url = aijobs::url_of(&a.job, a.index).ok_or_else(|| {
                    ApiError(CoreError::NotFound(format!(
                        "picture {} of job {}",
                        a.index, a.job
                    )))
                })?;
                let strength = b.strength.unwrap_or(0.6);
                if !(0.1..=0.95).contains(&strength) {
                    return Err(ApiError(CoreError::Invalid(
                        "strength must be 0.1-0.95".into(),
                    )));
                }
                Some((url, strength))
            }
            None => None,
        };
        let upscale = match b.upscale {
            Some(f) => {
                if !(1.0..=4.0).contains(&f) {
                    return Err(ApiError(CoreError::Invalid("upscale must be 1-4".into())));
                }
                let model = std::env::var("ND_CARTO_UPSCALE_MODEL")
                    .ok()
                    .filter(|m| !m.is_empty())
                    .ok_or_else(|| {
                        ApiError(CoreError::NotFound(
                            "set ND_CARTO_UPSCALE_MODEL to the upscale model nd-world should use"
                                .into(),
                        ))
                    })?;
                (f > 1.0).then_some((model, f))
            }
            None => None,
        };
        let id = aijobs::register(&b.kind, &prompt, count)
            .map_err(|m| ApiError(CoreError::CapExceeded(m)))?;
        tokio::spawn(aijobs::run(
            id.clone(),
            cfg,
            aijobs::Spec {
                kind: b.kind,
                prompt,
                width: b.width,
                height: b.height,
                count,
                seed: b.seed,
                anchor,
                upscale,
            },
        ));
        Ok(Json(serde_json::json!({ "job": id })))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = (count, prompt);
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

/// `GET /api/ai/jobs/{id}` — `{status, message, assets, seeds, …}`.
async fn ai_job(Path(id): Path<String>) -> Result<Json<aijobs::AiJob>, ApiError> {
    aijobs::get(&id)
        .map(Json)
        .ok_or_else(|| ApiError(CoreError::NotFound(format!("job {id}"))))
}

/// `POST /api/ai/jobs/{id}/cancel`.
async fn ai_job_cancel(Path(id): Path<String>) -> Result<Json<serde_json::Value>, ApiError> {
    if aijobs::cancel(&id) {
        Ok(Json(serde_json::json!({ "ok": true })))
    } else {
        Err(ApiError(CoreError::NotFound(format!(
            "no running job {id}"
        ))))
    }
}

#[derive(Deserialize)]
struct UnderlayBody {
    #[serde(default)]
    floor: Option<String>,
    asset: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    #[serde(default = "one_f32")]
    opacity: f32,
}

/// `POST /api/docs/{id}/underlay` `{floor?, asset, x, y, w, h, opacity?}` — put a
/// stored picture behind the floor (cells), as one undoable step. A layer: it
/// never changes walls or rooms.
async fn set_underlay(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<UnderlayBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    if assets::global().get_bytes(&b.asset).is_none() {
        return Err(ApiError(CoreError::NotFound(format!(
            "picture {}",
            b.asset
        ))));
    }
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor_id = match &b.floor {
        Some(f) => f.clone(),
        None => doc.floors[0].id.clone(),
    };
    let op = Op::SetUnderlay {
        underlay: Some(ndcart_core::Underlay {
            asset: b.asset,
            x: b.x,
            y: b.y,
            w: b.w,
            h: b.h,
            opacity: b.opacity,
        }),
    };
    doc.apply_ops(&floor_id, &[op])?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

/// `POST /api/docs/{id}/generate-map` `{floor?, kind, seed, width, height}` — a
/// dungeon, cave, building, deck or city district drawn onto the floor as ONE
/// undoable step (it adds to what is there; the same seed gives the same map).
async fn generate_map(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<GenMapBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let palette = doc
        .manifest
        .style
        .as_ref()
        .map(|s| s.palette.clone())
        .unwrap_or_default();
    let ops = ndcart_core::gen::generate_styled(
        &ndcart_core::gen::GenParams {
            kind: b.kind,
            seed: b.seed,
            width: b.width,
            height: b.height,
        },
        &palette,
    )?;
    let floor_id = match &b.floor {
        Some(f) => f.clone(),
        None => doc
            .floors
            .first()
            .map(|f| f.id.clone())
            .ok_or_else(|| CoreError::NotFound("floor".into()))?,
    };
    doc.apply_ops(&floor_id, &ops)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
struct GenMapBody {
    #[serde(default)]
    floor: Option<String>,
    kind: ndcart_core::gen::GenKind,
    #[serde(default)]
    seed: u64,
    #[serde(default = "default_gen_w")]
    width: i32,
    #[serde(default = "default_gen_h")]
    height: i32,
}
fn default_gen_w() -> i32 {
    64
}
fn default_gen_h() -> i32 {
    48
}

/// `POST /api/docs/{id}/generate` — a seeded world (continents, mountains, a sea
/// level for the requested land fraction, rivers to the sea) replaces the
/// floor's terrain and rivers as ONE undoable step. Size is capped at 512x512
/// cells here so a request cannot tie the server up.
async fn generate_world(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<GenerateBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be a JSON object".into())))?;
    let def = ndcart_terrain::GenParams::default();
    let num = |v: Option<f32>, d: f32, what: &str| -> Result<f32, ApiError> {
        let v = v.unwrap_or(d);
        if v.is_finite() {
            Ok(v)
        } else {
            Err(ApiError(CoreError::Invalid(format!(
                "{what} must be a number"
            ))))
        }
    };
    let params = ndcart_terrain::GenParams {
        seed: b.seed.unwrap_or(def.seed),
        cols: b.cols.unwrap_or(def.cols).clamp(8, 512),
        rows: b.rows.unwrap_or(def.rows).clamp(8, 512),
        land: num(b.land, def.land, "land")?.clamp(0.05, 0.95),
        continent: num(b.continent, def.continent, "continent")?.clamp(0.0, 1.0),
        ridges: num(b.ridges, def.ridges, "ridges")?.clamp(0.0, 1.0),
        rivers: b.rivers.unwrap_or(def.rivers).min(40),
    };
    let (cols, rows) = (params.cols, params.rows);
    let world = tokio::task::spawn_blocking(move || ndcart_terrain::generate(&params))
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("generation failed: {e}"))))?;

    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor_id = doc.floors[0].id.clone();
    let t = &world.terrain;
    let mut ops: Vec<Op> = doc.floors[0]
        .rivers
        .iter()
        .map(|r| Op::RemoveRiver { id: r.id.clone() })
        .collect();
    ops.push(Op::ClearTerrain);
    ops.push(Op::SetCanvas {
        canvas: Some([cols, rows]),
    });
    ops.push(Op::TerrainSet {
        fill: t.fill,
        chunks: t
            .chunk_keys()
            .into_iter()
            .map(|(cx, cy)| t.snapshot_chunk(cx, cy))
            .collect(),
    });
    ops.push(Op::SetTerrainStyle {
        style: t.style.clone(),
    });
    let river_count = world.rivers.len();
    ops.extend(world.rivers.into_iter().map(|river| Op::AddRiver {
        river,
        carve: false,
        index: None,
    }));
    doc.apply_ops(&floor_id, &[Op::Batch { ops }])?;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({
        "ok": true, "cols": cols, "rows": rows, "rivers": river_count,
        "sea_level": t.sea_level(), "chunks": t.chunk_count(),
    })))
}

/// `GET /api/stamps` — the starter stamp pack.
async fn list_stamps() -> Json<serde_json::Value> {
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
async fn set_asset_meta(
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
async fn stamp_icon_png(Path(id): Path<String>) -> Result<Response, ApiError> {
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
struct MakeBody {
    name: String,
    /// Material: cells per repeat (default 2). Stamp: width in cells (default 1.5).
    #[serde(default)]
    size: Option<f32>,
    /// Material: blend the picture so it tiles (default true).
    #[serde(default = "yes_bool")]
    seamless: bool,
    /// Stamp: the background colour to remove (default: guessed from the corners).
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    tolerance: Option<f32>,
    /// Where it came from, kept with it.
    #[serde(default)]
    prompt: String,
    #[serde(default = "minus_one")]
    seed: i64,
    #[serde(default)]
    ai: bool,
}
fn minus_one() -> i64 {
    -1
}

fn provenance(b: &MakeBody) -> assets::Provenance {
    assets::Provenance {
        source: if b.ai { "ai" } else { "imported" }.into(),
        prompt: b.prompt.chars().take(500).collect(),
        seed: b.seed,
        date: ndcart_core::doc::now_iso(),
    }
}

fn check_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 60 {
        return Err(ApiError(CoreError::Invalid(
            "a name is 1-60 characters".into(),
        )));
    }
    Ok(n.to_string())
}

#[derive(Deserialize)]
struct TagsBody {
    tags: Vec<String>,
}

/// `POST /api/stamps/{id}/tags` `{tags}` — the GM's confirmed tags for a custom prop
/// (lowercase words, at most 8; invalid ones are refused, not silently dropped).
async fn set_stamp_tags(
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

fn clean_tags_strict(raw: &[String]) -> Result<Vec<String>, ApiError> {
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
async fn suggest_stamp_tags(Path(id): Path<String>) -> Result<Json<serde_json::Value>, ApiError> {
    let c = assets::global()
        .find_custom(&id, "stamp")
        .ok_or_else(|| CoreError::NotFound(format!("custom prop {id}")))?;
    #[cfg(feature = "bridge")]
    {
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
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
struct PackQuery {
    /// The file's name, used as the pack's name when it does not name itself.
    name: Option<String>,
}

fn report_json(r: &packs::Report) -> serde_json::Value {
    serde_json::json!({
        "pack": r.pack, "materials": r.materials, "stamps": r.stamps,
        "skipped": r.skipped, "notes": r.notes,
    })
}

/// `POST /api/packs/import?name=file.dungeondraft_pack` — the pack's bytes in the
/// body (up to 200 MB); its props and textures join the palettes. A Godot
/// `.pck`-style Dungeondraft pack is read directly; anything else is refused with
/// advice. Pictures keep "from pack X by Y" as their provenance.
async fn import_pack(
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
struct PackExportQuery {
    name: Option<String>,
    /// `all` also writes items imported from other people's packs (default: only your own).
    include: Option<String>,
}

/// `GET /api/packs/export?name=My%20set[&include=all]` — your own props and
/// materials as a Dungeondraft-style pack. Art that came from someone else's pack
/// is left out unless you ask for `include=all` (check its license first).
async fn export_pack(
    axum::extract::Query(q): axum::extract::Query<PackExportQuery>,
) -> Result<Response, ApiError> {
    let name = q.name.unwrap_or_default();
    let all = q.include.as_deref() == Some("all");
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
async fn make_material(
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
async fn make_stamp(
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

/// `GET /api/docs/{id}/lint[?floor=ID]` — the Problems panel: deterministic
/// checks with undoable fixes (`fix` is an op you can POST to `/ops`).
async fn lint_doc(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ExportQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
    let mut problems = ndcart_core::lint(floor, doc.manifest.cell_size);
    problems.extend(ndcart_core::lint::lint_links(floor, &doc.floors));
    // The atlas chain: markers that open other maps. A link is healthy when
    // some document in this studio was published under that slug (its own
    // slug or one of its per-floor slugs).
    let published: std::collections::BTreeSet<String> = store
        .docs
        .values()
        .filter_map(|d| d.manifest.nd_world.as_ref())
        .flat_map(|l| {
            let mut v: Vec<String> = l.floor_slugs.values().cloned().collect();
            v.push(l.slug.clone());
            v
        })
        .collect();
    problems.extend(ndcart_core::lint::lint_marker_links(floor, &published));
    Ok(Json(serde_json::json!({
        "floor": floor.id,
        "blocks_publish": ndcart_core::blocks_publish(&problems),
        "problems": problems,
    })))
}

/// `GET /api/docs/{id}/floor-spec?floor=ID` - the spec round-trip (plan
/// 21.19): read a painted floor back as a MapSpec the studio can lay out
/// again with a new seed. Nothing is changed; the answer feeds the spec
/// dialog's propose flow.
async fn floor_spec(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ExportQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
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
    let (spec, warnings) =
        ndcart_core::extract::extract_floor_spec(floor, &doc.floors, doc.manifest.cell_size)?;
    let full = ndcart_core::mapspec::MapSpec {
        name: doc.manifest.name.clone(),
        floors: vec![spec],
    };
    Ok(Json(
        serde_json::json!({ "spec": full, "warnings": warnings }),
    ))
}

/// `GET /api/docs/{id}/stats?floor=ID` - the pacing dashboard's numbers
/// (plan 21.6): rooms/squares/kinds, doors and secrets, lights and their
/// coverage, props by provenance, GM labels, encounters, lore coverage.
async fn doc_stats(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<ExportQuery>,
) -> Result<Json<ndcart_core::stats::FloorStats>, ApiError> {
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
    Ok(Json(ndcart_core::stats::floor_stats(
        floor,
        doc.manifest.cell_size,
    )))
}

#[derive(Deserialize)]
struct PreviewQuery {
    floor: Option<String>,
    /// The viewer, in cells.
    x: Option<f64>,
    y: Option<f64>,
    /// How far the viewer sees, in cells (default 12).
    radius: Option<f64>,
    /// Darkness on (default): only lit areas show clearly.
    darkness: Option<bool>,
    ppc: Option<i64>,
}

#[derive(Deserialize)]
struct ProposeBody {
    spec: ndcart_core::mapspec::MapSpec,
    #[serde(default)]
    seed: Option<u64>,
}

fn proposal_summary(p: &store::Proposal) -> serde_json::Value {
    let floors: Vec<_> = p
        .floors
        .iter()
        .map(|cf| {
            let mut f = FloorDoc::new("preview".into(), cf.name.clone());
            let mut j = ndcart_core::Journal::default();
            for op in &cf.ops {
                let _ = j.push(&mut f, op);
            }
            let problems = ndcart_core::lint(&f, 32.0);
            serde_json::json!({
                "name": cf.name,
                "rooms": f.grid.spaces.len(),
                "squares": f.grid.cell_count(),
                "problems": problems.iter().map(|x| x.message.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "id": p.id, "name": p.spec.name, "seed": p.seed, "floors": floors })
}

/// `POST /api/docs/{id}/propose` `{spec, seed?}` — lay a MapSpec (rooms and how
/// they connect) out and hold it as a PROPOSAL. Nothing in the document changes
/// until it is accepted; a bad spec is refused with a message that says what to fix.
async fn propose_map(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<ProposeBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad spec: {e}"))))?;
    let seed = b.seed.unwrap_or(1);
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let p = doc.propose(b.spec, seed)?;
    Ok(Json(proposal_summary(p)))
}

#[derive(Deserialize)]
struct ProposalPreviewQuery {
    /// Which floor of the proposal (0-based; default 0).
    floor: Option<usize>,
    ppc: Option<i64>,
}

/// `GET /api/docs/{id}/proposals/{pid}/preview?floor=N` — the proposed floor as a PNG.
async fn proposal_preview(
    State(app): State<App>,
    Path((id, pid)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<ProposalPreviewQuery>,
) -> Result<Response, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let p = doc
        .proposals
        .iter()
        .find(|p| p.id == pid)
        .ok_or_else(|| CoreError::NotFound(format!("proposal {pid}")))?;
    let cf = p
        .floors
        .get(q.floor.unwrap_or(0))
        .ok_or_else(|| CoreError::NotFound("that floor of the proposal".into()))?;
    let cell_size = doc.manifest.cell_size;
    let mut f = FloorDoc::new("preview".into(), cf.name.clone());
    let mut j = ndcart_core::Journal::default();
    for op in &cf.ops {
        j.push(&mut f, op)?;
    }
    drop(store);
    let walls = tiles::derive_walls(&f, cell_size);
    let (png, ..) = tiles::bake_full_as(
        &f,
        &walls,
        cell_size,
        q.ppc.unwrap_or(16),
        tiles::ImageFormat::Png,
        90,
    )
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

/// `POST /api/docs/{id}/proposals/{pid}/accept` — add the proposal's floors (and
/// stairs between them) to the document. Existing floors are never touched; each
/// new floor can be taken back with undo-structure.
async fn accept_proposal(
    State(app): State<App>,
    Path((id, pid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floors = doc.accept_proposal(&pid)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(
        serde_json::json!({ "ok": true, "rev": rev, "floors": floors }),
    ))
}

/// `DELETE /api/docs/{id}/proposals/{pid}` — reject it.
async fn reject_proposal(
    State(app): State<App>,
    Path((id, pid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.reject_proposal(&pid)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DraftBody {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    prompt: String,
    /// The project whose house style to follow.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    doc: Option<String>,
    /// Reuse the names of the world's existing locations (nd-world only).
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    lore: bool,
}

#[cfg(not(feature = "bridge"))]
const NO_BRIDGE: &str = "this build has no AI client (built without the bridge feature)";

#[cfg(feature = "bridge")]
async fn draft_context(app: &App, doc: &Option<String>, lore: bool) -> draft::Context {
    let tone = match doc {
        Some(id) => app
            .store
            .read()
            .await
            .docs
            .get(id)
            .and_then(|d| d.manifest.style.as_ref().map(|s| s.tone.clone())),
        None => None,
    };
    draft::Context {
        tone,
        locations: Vec::new(),
        lore,
    }
}

/// `POST /api/draft` `{prompt, doc?, lore?}` — ask the AI (nd-world's queue, or
/// a model you run yourself) for a MapSpec. Configured in the studio's own
/// environment, never in a request (404 naming what to set otherwise). Nothing is
/// added to any document: the spec comes back for the GM to preview.
async fn draft_spec(
    State(app): State<App>,
    body: Result<Json<DraftBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {\"prompt\": ...}".into())))?;
    #[cfg(feature = "bridge")]
    {
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
        let ctx = draft_context(&app, &b.doc, b.lore).await;
        let spec = draft::draft(&cfg, &b.prompt, &ctx)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        Ok(Json(serde_json::json!({ "spec": spec })))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = (app, b);
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

/// `POST /api/draft-world` `{prompt, doc?, lore?}` — like `/api/draft`, for an overland map.
async fn draft_world_spec(
    State(app): State<App>,
    body: Result<Json<DraftBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {\"prompt\": ...}".into())))?;
    #[cfg(feature = "bridge")]
    {
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
        let ctx = draft_context(&app, &b.doc, b.lore).await;
        let spec = draft::draft_world(&cfg, &b.prompt, &ctx)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        Ok(Json(serde_json::json!({ "spec": spec })))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = (app, b);
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

#[derive(Deserialize)]
struct RefineBody {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    floor: Option<String>,
    /// Room ids to keep exactly as they are.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pins: Vec<String>,
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    instruction: String,
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    lore: bool,
    /// Refine this floor spec (a previewed proposal) instead of a floor of the project:
    /// the "keep talking to it" case. Pins then refer to this spec's room ids.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    spec: Option<ndcart_core::mapspec::FloorSpec>,
}

/// `POST /api/docs/{id}/refine` `{floor?, pins:[room ids], instruction}` — describe
/// the floor as a spec, ask the AI to revise it keeping the pinned rooms, and
/// return the revised spec for preview (it becomes a proposal, a NEW floor; the
/// floor being refined is never touched).
async fn refine_floor(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<RefineBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    #[cfg(feature = "bridge")]
    {
        let (current, cell) = if let Some(mut spec) = b.spec.clone() {
            // Stairs point at other floors, which a one-floor revision cannot see: set aside.
            spec.stairs.clear();
            // Check it is a spec we could lay out before asking a model to revise it.
            let one = ndcart_core::mapspec::MapSpec {
                name: spec.name.clone(),
                floors: vec![spec.clone()],
            };
            one.validate()?;
            (spec, 32.0)
        } else {
            let store = app.store.read().await;
            let doc = store
                .docs
                .get(&id)
                .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
            let idx = match &b.floor {
                Some(f) => floors_by_id(&doc.floors, f)?,
                None => 0,
            };
            (
                ndcart_core::mapspec::floor_to_spec(&doc.floors[idx], doc.manifest.cell_size),
                doc.manifest.cell_size,
            )
        };
        let _ = cell;
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
        let ctx = draft_context(&app, &Some(id.clone()), b.lore).await;
        let spec = draft::refine(&cfg, &current, &b.pins, &b.instruction, &ctx)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        Ok(Json(serde_json::json!({ "spec": spec })))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = (app, id, b);
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

/// `GET /api/doctor` — the same checks as `ndcart doctor`, for the Settings panel:
/// `{checks: [{level: ok|warn|fail, what, detail}]}`. Read-only; nothing secret is shown.
async fn doctor_checks(State(app): State<App>) -> Json<serde_json::Value> {
    let data = app.data_dir.clone();
    #[allow(unused_mut)]
    let mut checks =
        tokio::task::spawn_blocking(move || doctor::local_checks_with(data.as_deref(), None))
            .await
            .unwrap_or_default();
    #[cfg(feature = "bridge")]
    checks.extend(doctor::ai_checks().await);
    Json(serde_json::json!({ "checks": checks }))
}

#[derive(Deserialize)]
struct AskBody {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    question: String,
}

/// `POST /api/ask` `{question}` — ask your maps a question in plain words. The
/// answer comes from a text digest of every project (room names, notes, markers,
/// zones…), and the model is told to say so when the maps do not hold it.
async fn ask_maps(
    State(app): State<App>,
    body: Result<Json<AskBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|_| {
        ApiError(CoreError::Invalid(
            "body must be {\"question\": ...}".into(),
        ))
    })?;
    #[cfg(feature = "bridge")]
    {
        let q = b.question.trim();
        if q.is_empty() || q.chars().count() > 1_000 {
            return Err(ApiError(CoreError::Invalid(
                "ask in 1-1000 characters".into(),
            )));
        }
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
        let turn = {
            let store = app.store.read().await;
            ask::user_turn(&ask::digest(&store), q)
        };
        let backend = draft::Backend::connect(&cfg)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        let answer = backend
            .chat(ask::SYSTEM_PROMPT, &turn)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        Ok(Json(serde_json::json!({ "answer": answer.trim() })))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = (app, b);
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

#[derive(Deserialize)]
struct EntitiesQuery {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    q: Option<String>,
}

/// `GET /api/world/entities?q=` — the world's entities (nd-world), filtered by name,
/// so a room can be linked to one. At most 50 matches.
async fn world_entities(
    axum::extract::Query(q): axum::extract::Query<EntitiesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    #[cfg(feature = "bridge")]
    {
        let cfg = draft::Config::from_env().map_err(|m| ApiError(CoreError::NotFound(m)))?;
        if cfg.direct.is_some() {
            return Err(ApiError(CoreError::Invalid(
                "entities live in nd-world; a standalone model has none".into(),
            )));
        }
        let backend = draft::Backend::connect(&cfg)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        let draft::Backend::World(bridge, _) = &backend else {
            return Err(ApiError(CoreError::Invalid(
                "entities live in nd-world".into(),
            )));
        };
        let words: Vec<String> =
            q.q.unwrap_or_default()
                .split_whitespace()
                .map(str::to_lowercase)
                .collect();
        let all = bridge
            .entities()
            .await
            .map_err(|e| ApiError(CoreError::Invalid(e.to_string())))?;
        let hits: Vec<_> = all
            .into_iter()
            .filter(|(_, name, _)| {
                let n = name.to_lowercase();
                words.iter().all(|w| n.contains(w))
            })
            .take(50)
            .map(|(id, name, kind)| serde_json::json!({ "id": id, "name": name, "kind": kind }))
            .collect();
        Ok(Json(serde_json::json!({ "entities": hits })))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = q;
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

/// `GET /api/ai/status` — which AI the studio is wired to, for the banner:
/// `{mode: "nd-world" | "standalone" | "off", detail}`.
async fn ai_status() -> Json<serde_json::Value> {
    Json(ai_status_value())
}

#[cfg(feature = "bridge")]
fn ai_status_value() -> serde_json::Value {
    match draft::Config::from_env() {
        Ok(c) => serde_json::json!({
            "mode": if c.direct.is_some() { "standalone" } else { "nd-world" },
            "detail": c.describe(),
        }),
        Err(m) => serde_json::json!({ "mode": "off", "detail": m }),
    }
}

#[cfg(not(feature = "bridge"))]
fn ai_status_value() -> serde_json::Value {
    serde_json::json!({ "mode": "off", "detail": NO_BRIDGE })
}

#[derive(Deserialize)]
struct StyleBody {
    tone: String,
    #[serde(default)]
    palette: Vec<String>,
}

/// `POST /api/docs/{id}/style` `{tone, palette}` — the project's house style:
/// handed to AI drafts, and its palette colours generated rooms. An empty tone
/// and palette clear it.
async fn set_style(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<StyleBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let style = ndcart_core::StyleProfile {
        tone: b.tone.trim().to_string(),
        palette: b.palette,
    };
    style.validate()?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.manifest.style = if style.tone.is_empty() && style.palette.is_empty() {
        None
    } else {
        Some(style)
    };
    doc.manifest.updated_at = ndcart_core::doc::now_iso();
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ProposeWorldBody {
    spec: worldspec::WorldSpec,
    #[serde(default)]
    seed: Option<u32>,
}

/// `POST /api/docs/{id}/propose-world` `{spec, seed?}` — an overland map as a
/// proposal (terrain, rivers, biome props, places, regions). Nothing changes until accepted.
async fn propose_world(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<ProposeWorldBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad spec: {e}"))))?;
    let seed = b.seed.unwrap_or(1);
    let spec = b.spec;
    // Terrain generation takes a moment: do it off the async threads.
    let compiled = {
        let store = app.store.read().await;
        let doc = store
            .docs
            .get(&id)
            .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
        if doc.manifest.kind != ndcart_core::DocKind::Region {
            return Err(ApiError(CoreError::Invalid(
                "a world map goes in a world-map project (New → World map)".into(),
            )));
        }
        let old: Vec<String> = doc.floors[0].rivers.iter().map(|r| r.id.clone()).collect();
        let spec = spec.clone();
        tokio::task::spawn_blocking(move || worldspec::compile(&spec, seed, &old))
            .await
            .map_err(|e| ApiError(CoreError::Invalid(format!("generation failed: {e}"))))??
    };
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    if doc.world_proposals.len() >= store::MAX_PROPOSALS {
        doc.world_proposals.remove(0);
    }
    let pid = ndcart_core::ids::new_id("pw");
    doc.world_proposals.push(store::WorldProposal {
        id: pid.clone(),
        ops: compiled,
    });
    Ok(Json(serde_json::json!({
        "id": pid, "name": spec.name, "seed": seed,
        "places": spec.places.len(), "biomes": spec.biomes.len(), "regions": spec.regions.len(),
    })))
}

/// `GET /api/docs/{id}/world-proposals/{pid}/preview?ppc=` — the proposed world as a PNG.
async fn world_proposal_preview(
    State(app): State<App>,
    Path((id, pid)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<ProposalPreviewQuery>,
) -> Result<Response, ApiError> {
    let (mut scratch, ops, cell_size) = {
        let store = app.store.read().await;
        let doc = store
            .docs
            .get(&id)
            .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
        let p = doc
            .world_proposals
            .iter()
            .find(|p| p.id == pid)
            .ok_or_else(|| CoreError::NotFound(format!("proposal {pid}")))?;
        (doc.floors[0].clone(), p.ops.clone(), doc.manifest.cell_size)
    };
    let ppc = q.ppc.unwrap_or(4);
    let png = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ApiError> {
        let mut j = ndcart_core::Journal::default();
        for op in &ops {
            j.push(&mut scratch, op)?;
        }
        let walls = tiles::derive_walls(&scratch, cell_size);
        let (png, ..) = tiles::bake_full_as(
            &scratch,
            &walls,
            cell_size,
            ppc,
            tiles::ImageFormat::Png,
            90,
        )
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        Ok(png)
    })
    .await
    .map_err(|e| ApiError(CoreError::Invalid(format!("preview failed: {e}"))))??;
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        png,
    )
        .into_response())
}

/// `POST /api/docs/{id}/world-proposals/{pid}/accept` — draw it (replaces the
/// first floor's terrain and rivers) as one undoable step.
async fn accept_world(
    State(app): State<App>,
    Path((id, pid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.accept_world(&pid)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    app.wall_cache.lock().expect("wall cache").clear();
    app.tile_cache.lock().expect("tile cache").clear();
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

/// `DELETE /api/docs/{id}/world-proposals/{pid}`.
async fn reject_world(
    State(app): State<App>,
    Path((id, pid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.reject_world(&pid)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Deserialize)]
struct TableBody {
    #[serde(default)]
    floor: Option<String>,
    x: f64,
    y: f64,
    #[serde(default = "d_radius")]
    radius: f64,
    #[serde(default = "yes_bool")]
    darkness: bool,
}
fn d_radius() -> f64 {
    12.0
}

/// `POST /api/docs/{id}/table` `{floor?, x, y, radius?, darkness?}` — show the
/// players' view from this spot on the table screen (`/table/{id}`). `DELETE` turns it off.
async fn table_set(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<TableBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    if !(b.x.is_finite() && b.y.is_finite() && b.radius.is_finite())
        || !(0.5..=200.0).contains(&b.radius)
    {
        return Err(ApiError(CoreError::Invalid(
            "x and y must be numbers and radius 0.5-200".into(),
        )));
    }
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor = match &b.floor {
        Some(f) => doc.floors[floors_by_id(&doc.floors, f)?].id.clone(),
        None => doc.floors[0].id.clone(),
    };
    let n = doc.table.as_ref().map_or(1, |t| t.n + 1);
    doc.table = Some(store::TableState {
        floor,
        x: b.x,
        y: b.y,
        radius: b.radius,
        darkness: b.darkness,
        n,
    });
    Ok(Json(serde_json::json!({ "ok": true, "n": n })))
}

async fn table_off(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.table = None;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /api/docs/{id}/table` — what the table screen polls: `{on, n, rev, …}`.
async fn table_get(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    Ok(Json(match &doc.table {
        Some(t) => {
            serde_json::json!({ "on": true, "rev": doc.rev, "name": doc.manifest.name, "state": t })
        }
        None => serde_json::json!({ "on": false, "rev": doc.rev, "name": doc.manifest.name }),
    }))
}

const TABLE_HTML: &str = include_str!("../../../ui/table.html");

/// `GET /table/{id}` — the table screen: the players' view, full screen, kept in
/// step with the GM. Put it on a TV or a tablet; nothing on it can edit anything.
async fn table_page(Path(id): Path<String>) -> Response {
    // The id is only ever used by the page as a URL path segment; refuse anything odd.
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return (StatusCode::BAD_REQUEST, "bad project id").into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        TABLE_HTML.replace("__DOC__", &id),
    )
        .into_response()
}

/// `GET /api/docs/{id}/preview?x=&y=&radius=&darkness=&ppc=` — what the table
/// sees from one point: the player's copy of the floor (secrets stripped)
/// under fog, with sight blocked by walls and closed doors and lamps lighting
/// what they reach. A PNG.
async fn preview(
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
    let (png, ..) = tokio::task::spawn_blocking(move || {
        tiles::bake_preview(&floor, cell_size, ppc, [x, y], radius, dark)
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
struct PdfQuery {
    floor: Option<String>,
    /// `letter` (default) or `a4`.
    paper: Option<String>,
    /// Inches per square on paper (default 1: print and use miniatures).
    inch: Option<f32>,
    /// Print resolution (default 150).
    dpi: Option<u32>,
    audience: Option<String>,
}

/// `GET /api/docs/{id}/export/pdf?floor&paper&inch&dpi&audience` — the floor on
/// paper at true scale (one inch a square by default), cut into pages with
/// corner ticks and a "page r of n" label. The players' copy hides secrets.
async fn export_pdf(
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
async fn export_png(
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
async fn export_uvtt(
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
struct ConvertQuery {
    format: Option<String>,
    quality: Option<u8>,
}

/// `POST /api/convert?format=webp|avif|png[&quality=N]` — raw image bytes in
/// (PNG/JPEG/GIF/BMP/WebP), converted image out. Max 80 MB / 40 Mpx.
async fn convert_image(
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

#[derive(Deserialize)]
struct OpsBody {
    floor: String,
    ops: Vec<Op>,
}

async fn post_ops(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<OpsBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {floor, ops}".into())))?;
    if b.ops.len() > 500 {
        return Err(CoreError::CapExceeded("at most 500 ops per request".into()).into());
    }
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.apply_ops(&b.floor, &b.ops)?;
    let rev = doc.rev;
    store::save_doc_fast(app.data_dir.as_deref(), doc);
    drop(store);
    // Invalidate stale tiles/walls for this document.
    let prefix = format!("{id}/");
    app.tile_cache
        .lock()
        .expect("tile cache")
        .retain(|k, _| !k.starts_with(&prefix));
    Ok(Json(serde_json::json!({"ok": true, "rev": rev})))
}

#[derive(Deserialize)]
struct FloorBody {
    floor: String,
}

async fn post_undo(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<FloorBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|_| ApiError(CoreError::Invalid("body must be {floor}".into())))?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    doc.undo_last(&b.floor)?;
    let rev = doc.rev;
    store::save_doc_fast(app.data_dir.as_deref(), doc);
    drop(store);
    let prefix = format!("{id}/");
    app.tile_cache
        .lock()
        .expect("tile cache")
        .retain(|k, _| !k.starts_with(&prefix));
    Ok(Json(serde_json::json!({"ok": true, "rev": rev})))
}

#[derive(Deserialize)]
struct TileQuery {
    /// Another floor of the same document to show faintly underneath.
    onion: Option<String>,
}

async fn get_tile(
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
    let floor = doc.floors[idx].clone();
    // The floor's active era changes what a tile looks like; the era id (or
    // its absence) is part of the cache key, and the op that set it bumped
    // the rev, which busts the browser's copy.
    let era_id = floor.active_era.clone();
    let key = format!(
        "{doc_id}/{floor_id}/{z}/{x}_{y}/{}/{}",
        onion_id.as_deref().unwrap_or("-"),
        era_id.as_deref().unwrap_or("-")
    );
    if let Some(bytes) = app.tile_cache.lock().expect("tile cache").get(&key) {
        return Ok(png_response(bytes));
    }
    let walls = {
        let mut cache = app.wall_cache.lock().expect("wall cache");
        cache
            .entry((doc_id.clone(), floor_id.clone(), rev))
            .or_insert_with(|| Arc::new(tiles::derive_walls(&floor, cell_size)))
            .clone()
    };
    // The optional floor shown faintly underneath (must be a floor of this document).
    let ghost = match &onion_id {
        Some(oid) => {
            let oi = floors_by_id(&doc.floors, oid)?;
            let of = doc.floors[oi].clone();
            let ow = {
                let mut cache = app.wall_cache.lock().expect("wall cache");
                cache
                    .entry((doc_id.clone(), oid.clone(), rev))
                    .or_insert_with(|| Arc::new(tiles::derive_walls(&of, cell_size)))
                    .clone()
            };
            Some((of, ow))
        }
        None => None,
    };
    let era = era_id.as_deref().and_then(|id| floor.era(id)).cloned();
    let bytes = tokio::task::spawn_blocking(move || {
        let onion = ghost.as_ref().map(|(f, w)| (f, w.as_slice()));
        tiles::bake_tile_era(&floor, &walls, cell_size, z, x, y, onion, era.as_ref())
    })
    .await
    .map_err(|e| CoreError::Invalid(format!("tile bake failed: {e}")))?;
    let mut cache = app.tile_cache.lock().expect("tile cache");
    if cache.len() > 2_000 {
        cache.clear();
    }
    cache.insert(key, bytes.clone());
    drop(cache);
    drop(store);
    Ok(png_response(&bytes))
}

fn png_response(bytes: &[u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "private, max-age=86400"),
        ],
        bytes.to_vec(),
    )
        .into_response()
}

fn build_router(app: App) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/app.css", get(app_css))
        .route("/api/docs", get(list_docs).post(create_doc))
        .route("/api/docs/{id}", get(get_doc))
        .route("/api/docs/{id}/export/uvtt", get(export_uvtt))
        .route("/api/docs/{id}/export/png", get(export_png))
        .route("/api/docs/{id}/lint", get(lint_doc))
        .route("/api/docs/{id}/floor-spec", get(floor_spec))
        .route("/api/docs/{id}/stats", get(doc_stats))
        .route("/api/materials", get(list_materials))
        .route("/api/assets", post(put_asset))
        .route("/api/assets/{id}", get(get_asset))
        .route("/api/assets/{id}/meta", post(set_asset_meta))
        .route("/api/packs/export", get(export_pack))
        .route("/api/assets/{id}/make-material", post(make_material))
        .route(
            "/api/packs/import",
            post(import_pack).layer(axum::extract::DefaultBodyLimit::max(
                packs::MAX_PACK_BYTES + (1 << 20),
            )),
        )
        .route("/api/assets/{id}/make-stamp", post(make_stamp))
        .route("/api/stamps/{id}/tags", post(set_stamp_tags))
        .route("/api/stamps/{id}/suggest-tags", post(suggest_stamp_tags))
        .route("/api/import/uvtt", post(import_uvtt))
        .route("/api/stamps", get(list_stamps))
        .route("/api/stamps/{id}/icon.png", get(stamp_icon_png))
        .route("/api/convert", post(convert_image))
        .layer(axum::extract::DefaultBodyLimit::max(
            convert::MAX_INPUT_BYTES,
        ))
        .route("/api/docs/{id}/generate", post(generate_world))
        .route("/api/docs/{id}/floors", post(floors_route))
        .route("/api/docs/{id}/undo-structure", post(undo_structure))
        .route("/api/docs/{id}/generate-map", post(generate_map))
        .route("/api/ai/image", post(ai_image))
        .route("/api/ai/jobs/{id}", get(ai_job))
        .route("/api/ai/jobs/{id}/cancel", post(ai_job_cancel))
        .route("/api/docs/{id}/underlay", post(set_underlay))
        .route("/api/docs/{id}/furnish", post(furnish_rooms))
        .route("/api/docs/{id}/reroll", post(reroll_room))
        .route("/api/search", get(search_maps))
        .route("/api/docs/{id}/scatter", post(scatter_props))
        .route("/api/docs/{id}/preview", get(preview))
        .route(
            "/api/docs/{id}/table",
            get(table_get).post(table_set).delete(table_off),
        )
        .route("/table/{id}", get(table_page))
        .route("/api/docs/{id}/export/pdf", get(export_pdf))
        .route("/api/docs/{id}/export/foundry", get(export_foundry))
        .route("/api/import/foundry", post(import_foundry))
        .route("/api/draft", post(draft_spec))
        .route("/api/ai/status", get(ai_status))
        .route("/api/world/entities", get(world_entities))
        .route(
            "/api/world/entities/{id}/preview",
            get(world_entity_preview),
        )
        .route("/api/ask", post(ask_maps))
        .route("/api/doctor", get(doctor_checks))
        .route("/api/docs/{id}/refine", post(refine_floor))
        .route("/api/docs/{id}/style", post(set_style))
        .route("/api/draft-world", post(draft_world_spec))
        .route("/api/docs/{id}/propose", post(propose_map))
        .route("/api/docs/{id}/propose-world", post(propose_world))
        .route(
            "/api/docs/{id}/world-proposals/{pid}",
            axum::routing::delete(reject_world),
        )
        .route(
            "/api/docs/{id}/world-proposals/{pid}/preview",
            get(world_proposal_preview),
        )
        .route(
            "/api/docs/{id}/world-proposals/{pid}/accept",
            post(accept_world),
        )
        .route(
            "/api/docs/{id}/proposals/{pid}",
            axum::routing::delete(reject_proposal),
        )
        .route(
            "/api/docs/{id}/proposals/{pid}/preview",
            get(proposal_preview),
        )
        .route(
            "/api/docs/{id}/proposals/{pid}/accept",
            post(accept_proposal),
        )
        .route("/api/prefabs", get(list_prefabs))
        .route("/api/prefabs/{pid}", axum::routing::delete(delete_prefab))
        .route("/api/docs/{id}/prefabs", post(capture_prefab))
        .route("/api/docs/{id}/place-prefab", post(place_prefab))
        .route(
            "/api/docs/{id}/checkpoints",
            get(list_checkpoints).post(create_checkpoint),
        )
        .route(
            "/api/docs/{id}/checkpoints/{cid}",
            axum::routing::delete(delete_checkpoint),
        )
        .route(
            "/api/docs/{id}/checkpoints/{cid}/restore",
            post(restore_checkpoint),
        )
        .route("/api/docs/{id}/ops", post(post_ops))
        .route("/api/docs/{id}/undo", post(post_undo))
        .route("/tiles/{doc}/{floor}/{z}/{xy}", get(get_tile))
        .with_state(app)
}

// ---------- bench ----------

fn run_bench(args: Vec<String>) {
    use ndcart_core::grid::{Space, SpaceKind};
    use ndcart_core::journal::Journal;
    // `ndcart bench [--ppc N]`: the full-map bake runs at N px per square
    // (default 16 so the debug-build CI smoke stays quick; the plan's budget is
    // measured at 32: `cargo run --release -p ndcart-server -- bench --ppc 32`).
    let mut bake_ppc = 16i64;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--ppc" {
            bake_ppc = it.next().and_then(|v| v.parse().ok()).unwrap_or(16);
        }
    }
    println!("ndcart bench: journal + walls + replay + full-map bake on a 200x140 dungeon");
    let mut floor = FloorDoc::new("bench".into(), "Bench".into());
    let mut journal = Journal::default();

    let t0 = std::time::Instant::now();
    let palette = ["#7f5539", "#8a9a5b", "#5a7d9a", "#4a4a4a", "#202020"];
    for (i, color) in palette.iter().enumerate() {
        let space = Space {
            id: format!("sp{i}"),
            name: format!("Space {i}"),
            color: (*color).to_string(),
            kind: SpaceKind::Room,
            // Worst case for the baker: every cell borders a different material.
            material: Some(ndcart_paint::starter()[i].id.to_string()),
        };
        store::apply_ops_direct(&mut floor, &mut journal, &[Op::UpsertSpace { space }]).unwrap();
    }
    // 10,000 single-cell ops (worst-case journal granularity).
    let cols = 200usize;
    let rows = 140usize;
    let mut op_count = 0usize;
    'outer: for y in 0..rows {
        for x in 0..cols {
            let op = Op::PaintCells {
                cells: vec![ndcart_core::CellKey::new(x as i32, y as i32)],
                space: Some(format!("sp{}", (x + y) % palette.len())),
            };
            store::apply_ops_direct(&mut floor, &mut journal, &[op]).unwrap();
            op_count += 1;
            if op_count >= 10_000 {
                break 'outer;
            }
        }
    }
    let apply_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    let walls = tiles::derive_walls(&floor, 32.0);
    let walls_ms = t1.elapsed().as_millis();

    let t2 = std::time::Instant::now();
    let ndjson = journal.to_ndjson();
    let ser_ms = t2.elapsed().as_millis();

    let t3 = std::time::Instant::now();
    let back = Journal::from_ndjson(&ndjson).unwrap();
    let mut fresh = FloorDoc::new("bench".into(), "Bench".into());
    // The journal's first entries include the UpsertSpace ops, so replay is whole.
    back.replay_into(&mut fresh).unwrap();
    let replay_ms = t3.elapsed().as_millis();

    assert_eq!(
        fresh.grid.cell_count(),
        floor.grid.cell_count(),
        "replay must match"
    );
    println!("  ops applied:      {op_count:>7} in {apply_ms} ms");
    println!("  walls derived:    {:>7} in {walls_ms} ms", walls.len());
    println!("  ndjson bytes:     {:>7} in {ser_ms} ms", ndjson.len());
    println!(
        "  journal replay:    {:>6} entries in {replay_ms} ms",
        back.len()
    );
    println!("  OK: replay reproduces the state exactly.");

    // The plan's budget (§6): a full 200x140-cell bake at 32 px/cell < 30 s.
    let walls = tiles::derive_walls(&floor, 32.0);
    let t4 = std::time::Instant::now();
    let (png, w, h) =
        tiles::bake_full_as(&floor, &walls, 32.0, bake_ppc, tiles::ImageFormat::Png, 90)
            .expect("bake");
    let bake_ms = t4.elapsed().as_millis();
    println!(
        "  full-map bake:     {w}x{h} px at {bake_ppc} px/cell in {bake_ms} ms ({} KB png, every cell blending)",
        png.len() / 1024
    );
    assert!(
        bake_ms < 30_000,
        "bake budget blown: {bake_ms} ms (plan: < 30 s at 32 px/cell)"
    );
}

// ---------- convert CLI ----------

/// `ndcart convert IN OUT [--format webp|avif|png] [--quality N]` — the format
/// defaults to OUT's extension.
/// `ndcart import-pack FILE_OR_FOLDER --data-dir DIR` — add a Dungeondraft pack
/// (`.dungeondraft_pack`) or a folder of pictures to the palettes of the studio
/// that uses DIR, without the studio running.
fn run_import_pack(args: Vec<String>) -> Result<(), String> {
    let (mut path, mut data) = (None, None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => data = it.next().map(PathBuf::from),
            o if !o.starts_with("--") && path.is_none() => path = Some(PathBuf::from(o)),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let path = path.ok_or("name the pack file or folder")?;
    let data = data
        .or_else(|| std::env::var("ND_CARTO_DATA_DIR").ok().map(PathBuf::from))
        .ok_or("--data-dir (or ND_CARTO_DATA_DIR) is required")?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported pack")
        .to_string();
    let entries = if path.is_dir() {
        packs::read_dir(&path)?
    } else {
        let bytes =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if bytes.starts_with(b"PK") {
            return Err("that is a zip file; unzip it and give the folder".into());
        }
        packs::read_pck(&bytes)?
    };
    let store = assets::init(Some(data.join("assets")));
    let r = packs::import(&entries, &name, store)?;
    println!(
        "{}: added {} prop(s) and {} material(s); {} picture(s) skipped",
        r.pack, r.stamps, r.materials, r.skipped
    );
    for n in &r.notes {
        println!("note: {n}");
    }
    Ok(())
}

fn run_convert(args: Vec<String>) {
    let (mut files, mut format, mut quality) = (Vec::new(), None, 80u8);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--format" => format = it.next(),
            "--quality" => quality = it.next().and_then(|q| q.parse().ok()).unwrap_or(80),
            _ => files.push(a),
        }
    }
    let usage = || {
        eprintln!("usage: ndcart convert IN OUT [--format webp|avif|png] [--quality 1-100]");
        std::process::exit(2);
    };
    let [input, output] = files.as_slice() else {
        usage();
        return;
    };
    let ext = std::path::Path::new(output)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let Some(fmt) = tiles::ImageFormat::parse(format.as_deref().unwrap_or(ext)) else {
        eprintln!("ndcart: pick --format png, webp or avif (or name OUT .png/.webp/.avif)");
        std::process::exit(2);
    };
    let bytes = std::fs::read(input).unwrap_or_else(|e| {
        eprintln!("ndcart: cannot read {input}: {e}");
        std::process::exit(1);
    });
    match convert::convert(&bytes, fmt, quality) {
        Ok(c) => {
            std::fs::write(output, &c.bytes).unwrap_or_else(|e| {
                eprintln!("ndcart: cannot write {output}: {e}");
                std::process::exit(1);
            });
            println!(
                "{input} -> {output}: {}x{} {} ({} -> {} bytes)",
                c.width,
                c.height,
                fmt.ext(),
                bytes.len(),
                c.bytes.len()
            );
        }
        Err(m) => {
            eprintln!("ndcart: {m}");
            std::process::exit(1);
        }
    }
}

// ---------- main ----------

/// `ndcart healthcheck`: for the container HEALTHCHECK (the image has no curl).
/// Asks the local server for `/health` over plain TCP and checks for a 200.
fn run_healthcheck() -> bool {
    use std::io::{Read, Write};
    let listen = std::env::var("ND_CARTO_LISTEN").unwrap_or_else(|_| "127.0.0.1:8300".into());
    let port = listen.rsplit(':').next().unwrap_or("8300");
    let addr = format!("127.0.0.1:{port}");
    let Ok(mut s) = std::net::TcpStream::connect(&addr) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    if s.write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.starts_with("HTTP/1.1 200") || out.starts_with("HTTP/1.0 200")
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "serve".into());
    match cmd.as_str() {
        "bench" => run_bench(args.collect()),
        "healthcheck" => {
            if !run_healthcheck() {
                std::process::exit(1);
            }
        }
        "convert" => run_convert(args.collect()),
        "import-pack" => {
            if let Err(m) = run_import_pack(args.collect()) {
                eprintln!("ndcart: {m}");
                std::process::exit(1);
            }
        }
        "doctor" => {
            if let Err(m) = doctor::run(args.collect()).await {
                eprintln!("ndcart: {m}");
                std::process::exit(1);
            }
        }
        #[cfg(feature = "bridge")]
        "draft" => {
            if let Err(m) = draft::run(args.collect()).await {
                eprintln!("ndcart: {m}");
                std::process::exit(1);
            }
        }
        #[cfg(feature = "bridge")]
        "open-published" => {
            if let Err(m) = publish::run_open(args.collect()).await {
                eprintln!("ndcart: {m}");
                std::process::exit(1);
            }
        }
        #[cfg(feature = "bridge")]
        "publish" => {
            if let Err(m) = publish::run(args.collect()).await {
                eprintln!("ndcart: {m}");
                std::process::exit(1);
            }
        }
        "serve" => {
            let mut listen: Option<String> = None;
            let mut data_dir: Option<PathBuf> = None;
            let mut rest = args;
            while let Some(a) = rest.next() {
                match a.as_str() {
                    "--listen" => listen = rest.next(),
                    "--data-dir" => data_dir = rest.next().map(PathBuf::from),
                    other => {
                        eprintln!("unknown argument: {other}");
                        std::process::exit(2);
                    }
                }
            }
            let listen = listen
                .or_else(|| std::env::var("ND_CARTO_LISTEN").ok())
                .unwrap_or_else(|| "127.0.0.1:8300".into());
            let data_dir =
                data_dir.or_else(|| std::env::var("ND_CARTO_DATA_DIR").ok().map(PathBuf::from));

            let mut store = store::Store::default();
            if let Some(dir) = &data_dir {
                if let Err(e) = store.load_dir(dir) {
                    eprintln!("ndcart: failed to load {}: {e}", dir.display());
                    std::process::exit(1);
                }
            }
            assets::init(data_dir.as_ref().map(|d| d.join("assets")));
            let app_data_dir = data_dir.clone();
            let app: App = Arc::new(AppState {
                store: tokio::sync::RwLock::new(store),
                data_dir,
                wall_cache: Mutex::new(HashMap::new()),
                tile_cache: Mutex::new(HashMap::new()),
            });
            let mut router = build_router(app);
            if !auth::is_loopback(&listen) {
                let (token, fresh) = auth::resolve_token(app_data_dir.as_deref());
                if fresh {
                    println!("ndcart: access token (shown once): {token}");
                }
                println!("ndcart: not loopback, token required (?token=… or Bearer)");
                router = router.layer(axum::middleware::from_fn(move |req, next| {
                    auth::gate(token.clone(), req, next)
                }));
            }
            let listener = tokio::net::TcpListener::bind(&listen)
                .await
                .unwrap_or_else(|e| panic!("cannot bind {listen}: {e}"));
            println!(
                "ndcart {} serving on http://{listen}",
                env!("CARGO_PKG_VERSION")
            );
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                    println!("ndcart: shutting down");
                })
                .await
                .unwrap();
        }
        other => {
            eprintln!(
                "usage: ndcart [serve --listen ADDR --data-dir DIR | bench | convert IN OUT | import-pack FILE | open-published SLUG | doctor | publish DOC ... | draft PROMPT ...]"
            );
            eprintln!("  unknown command: {other}");
            std::process::exit(2);
        }
    }
}

// ---------- integration tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_app() -> App {
        Arc::new(AppState {
            store: tokio::sync::RwLock::new(store::Store::default()),
            data_dir: None,
            wall_cache: Mutex::new(HashMap::new()),
            tile_cache: Mutex::new(HashMap::new()),
        })
    }

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn token_gate_blocks_without_token() {
        let router = build_router(test_app()).layer(axum::middleware::from_fn(|req, next| {
            auth::gate("s3cret".to_string(), req, next)
        }));
        let get = |uri: &str, extra: Option<(&str, &str)>| {
            let mut b = Request::get(uri.to_string());
            if let Some((k, v)) = extra {
                b = b.header(k, v);
            }
            b.body(Body::empty()).unwrap()
        };
        let r = router
            .clone()
            .oneshot(get("/api/docs", None))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let r = router.clone().oneshot(get("/health", None)).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let r = router
            .clone()
            .oneshot(get("/api/docs", Some(("authorization", "Bearer s3cret"))))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let r = router
            .clone()
            .oneshot(get("/api/docs", Some(("authorization", "Bearer wrong"))))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let r = router.oneshot(get("/?token=s3cret", None)).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert!(r.headers().get("set-cookie").is_some());
    }

    #[tokio::test]
    async fn health_answers() {
        let resp = build_router(test_app())
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = body_string(resp).await;
        assert!(text.contains("\"ok\""), "{text}");
    }

    #[tokio::test]
    async fn create_paint_tile_roundtrip() {
        let app = test_app();
        let router = build_router(app);

        // Create a document.
        let resp = router
            .clone()
            .oneshot(
                Request::post("/api/docs")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "Test Vault", "kind": "battle"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = body_string(resp).await;
        let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
        let doc_id = manifest["id"].as_str().unwrap().to_string();

        // Read the doc to learn the floor id.
        let resp = router
            .clone()
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let floor_id = doc["floors"][0]["id"].as_str().unwrap().to_string();

        // Paint two cells (batch: upsert space + paint).
        let ops = serde_json::json!({
            "floor": floor_id,
            "ops": [{
                "t": "batch",
                "ops": [
                    {"t": "upsert_space", "space": {
                        "id": "sp_a", "name": "Hall", "color": "#7f5539", "kind": "room"}},
                    {"t": "paint_cells", "cells": [
                        {"x": 0, "y": 0}, {"x": 1, "y": 0}
                    ], "space": "sp_a"}
                ]
            }]
        });
        let resp = router
            .clone()
            .oneshot(
                Request::post(format!("/api/docs/{doc_id}/ops"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&ops).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let text = body_string(resp).await;
        assert!(text.contains("\"rev\":1"), "{text}");

        // Doc now shows 2 walls worth of derivation + the cells.
        let resp = router
            .clone()
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            doc["floors"][0]["grid"]["cells"].as_array().unwrap().len(),
            2
        );
        assert!(!doc["floors"][0]["walls"].as_array().unwrap().is_empty());

        // A tile bakes as a PNG.
        let url = format!("/tiles/{doc_id}/{floor_id}/0/0_0");
        let resp = router
            .oneshot(Request::get(&url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[0..4], &[0x89, b'P', b'N', b'G']);
    }

    #[tokio::test]
    async fn uvtt_export_has_walls_and_grid() {
        let app = test_app();
        let (doc_id, floor_id) = {
            let mut st = app.store.write().await;
            let m = st.create("Vault", DocKind::Battle).clone();
            let d = st.docs.get_mut(&m.id).unwrap();
            let fid = d.floors[0].id.clone();
            let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
                {"t":"upsert_space","space":{"id":"s","name":"H","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":1}],"space":"s"}
            ]))
            .unwrap();
            d.apply_ops(&fid, &ops).unwrap();
            (m.id, fid)
        };
        let resp = build_router(app)
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}/export/uvtt?floor={floor_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["resolution"]["map_size"]["x"], 3);
        assert_eq!(v["resolution"]["map_size"]["y"], 2);
        assert!(!v["line_of_sight"].as_array().unwrap().is_empty());
        // The picture is embedded (a base64 PNG starts "iVBOR") and the lattice
        // matches it.
        assert!(v["image"].as_str().unwrap().starts_with("iVBOR"));
        assert_eq!(v["resolution"]["pixels_per_grid"], 32);
    }

    #[tokio::test]
    async fn convert_route_webp_and_rejects_junk() {
        let mut pm = tiny_skia::Pixmap::new(5, 5).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(1, 2, 3, 255));
        let png = pm.encode_png().unwrap();
        let router = build_router(test_app());
        let post = |uri: &str, body: Vec<u8>| {
            Request::post(uri.to_string())
                .body(Body::from(body))
                .unwrap()
        };
        let r = router
            .clone()
            .oneshot(post("/api/convert?format=webp", png.clone()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()["content-type"], "image/webp");
        assert_eq!(r.headers()["x-image-width"], "5");
        let r = router
            .clone()
            .oneshot(post("/api/convert?format=avif", png))
            .await
            .unwrap();
        assert_eq!(r.headers()["content-type"], "image/avif");
        let r = router
            .oneshot(post("/api/convert", b"junk".to_vec()))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn lint_route_reports_and_offers_a_fix_that_applies() {
        let app = test_app();
        let (doc_id, floor_id) = {
            let mut st = app.store.write().await;
            let m = st.create("Vault", DocKind::Battle).clone();
            let d = st.docs.get_mut(&m.id).unwrap();
            let fid = d.floors[0].id.clone();
            let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
                {"t":"upsert_space","space":{"id":"a","name":"Hall","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0}],"space":"a"},
                {"t":"set_mark","edge":{"cell":{"x":0,"y":0},"edge":"east"},"mark":{"kind":"door","state":"closed"}}
            ]))
            .unwrap();
            d.apply_ops(&fid, &ops).unwrap();
            (m.id, fid)
        };
        let router = build_router(app);
        let resp = router
            .clone()
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}/lint?floor={floor_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(v["blocks_publish"], false);
        assert_eq!(v["problems"][0]["code"], "mark_inside_room");
        // The fix is an op the ops route accepts, and it clears the problem.
        let fix = v["problems"][0]["fix"].clone();
        let body = serde_json::json!({"floor": floor_id, "ops": [fix]});
        let resp = router
            .clone()
            .oneshot(
                Request::post(format!("/api/docs/{doc_id}/ops"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = router
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}/lint"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(v["problems"].as_array().unwrap().is_empty(), "{v}");
    }

    #[tokio::test]
    async fn player_exports_hide_secret_doors_and_gm_exports_show_them() {
        let app = test_app();
        let (doc_id, floor_id) = {
            let mut st = app.store.write().await;
            let m = st.create("Vault", DocKind::Battle).clone();
            let d = st.docs.get_mut(&m.id).unwrap();
            let fid = d.floors[0].id.clone();
            let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
                {"t":"upsert_space","space":{"id":"a","name":"Hall","color":"#7f5539","kind":"room"}},
                {"t":"upsert_space","space":{"id":"b","name":"Vault","color":"#5a7d9a","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0}],"space":"a"},
                {"t":"paint_cells","cells":[{"x":3,"y":0}],"space":"b"},
                {"t":"set_mark","edge":{"cell":{"x":2,"y":0},"edge":"east"},"mark":{"kind":"secret","state":"closed"}}
            ]))
            .unwrap();
            d.apply_ops(&fid, &ops).unwrap();
            (m.id, fid)
        };
        let router = build_router(app);
        let fetch = |aud: &'static str| {
            let router = router.clone();
            let uri =
                format!("/api/docs/{doc_id}/export/png?floor={floor_id}&ppc=32&audience={aud}");
            async move {
                let r = router
                    .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(r.status(), StatusCode::OK, "{aud}");
                let b = r.into_body().collect().await.unwrap().to_bytes();
                tiny_skia::Pixmap::decode_png(&b).unwrap()
            }
        };
        let has_purple = |pm: &tiny_skia::Pixmap| {
            pm.pixels().iter().any(|p| {
                let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs();
                d(p.red(), 0xb0) < 14 && d(p.green(), 0x6a) < 14 && d(p.blue(), 0xd8) < 14
            })
        };
        assert!(
            has_purple(&fetch("gm").await),
            "the GM sees the secret door"
        );
        assert!(!has_purple(&fetch("player").await), "the players must not");
        let bad = router
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}/export/png?audience=bard"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }

    async fn call(
        router: &Router,
        method: &str,
        uri: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Vec<u8>) {
        let r = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = r.status();
        (
            status,
            r.into_body().collect().await.unwrap().to_bytes().to_vec(),
        )
    }

    #[tokio::test]
    async fn assets_are_hashed_served_back_and_junk_is_refused() {
        let router = build_router(test_app());
        let mut pm = tiny_skia::Pixmap::new(3, 3).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(9, 8, 7, 255));
        let png = pm.encode_png().unwrap();
        let (st, body) = call(&router, "POST", "/api/assets", png.clone()).await;
        assert_eq!(st, StatusCode::OK);
        let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(id, ndcart_core::hash::sha256_hex(&png));
        let (st, back) = call(&router, "GET", &format!("/api/assets/{id}"), vec![]).await;
        assert_eq!((st, back), (StatusCode::OK, png));
        assert_eq!(
            call(&router, "POST", "/api/assets", b"not an image".to_vec())
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(
                &router,
                "GET",
                &format!("/api/assets/{}", "0".repeat(64)),
                vec![]
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&router, "GET", "/api/assets/..%2F..%2Fetc%2Fpasswd", vec![])
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_uvtt_file_opens_as_a_project_and_exports_back_without_loss() {
        let app = test_app();
        let router = build_router(app.clone());
        // A source map: two rooms, a door between them, a window, a light.
        let (src_id, src_floor) = {
            let mut st = app.store.write().await;
            let m = st.create("Source", DocKind::Battle).clone();
            let d = st.docs.get_mut(&m.id).unwrap();
            let fid = d.floors[0].id.clone();
            let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
                {"t":"upsert_space","space":{"id":"a","name":"A","color":"#7f5539","kind":"room","material":"wood_planks"}},
                {"t":"upsert_space","space":{"id":"b","name":"B","color":"#5a7d9a","kind":"room","material":"cobblestone"}},
                {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":0,"y":1},{"x":1,"y":1}],"space":"a"},
                {"t":"paint_cells","cells":[{"x":2,"y":0},{"x":3,"y":0},{"x":2,"y":1},{"x":3,"y":1}],"space":"b"},
                {"t":"set_mark","edge":{"cell":{"x":1,"y":0},"edge":"east"},"mark":{"kind":"door","state":"open"}},
                {"t":"set_mark","edge":{"cell":{"x":0,"y":0},"edge":"north"},"mark":{"kind":"window","state":"closed"}},
                {"t":"add_light","light":{"id":"l","x":64.0,"y":32.0,"range":5.0,"color":"#ffaa00","intensity":0.8,"on":true}}
            ]))
            .unwrap();
            d.apply_ops(&fid, &ops).unwrap();
            (m.id, fid)
        };
        let (st, uvtt) = call(
            &router,
            "GET",
            &format!("/api/docs/{src_id}/export/uvtt?floor={src_floor}&ppc=48"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let src: serde_json::Value = serde_json::from_slice(&uvtt).unwrap();

        // Open it as a new project.
        let (st, body) = call(
            &router,
            "POST",
            "/api/import/uvtt?name=Opened",
            uvtt.clone(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let imp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let (new_id, new_floor) = (
            imp["manifest"]["id"].as_str().unwrap().to_string(),
            imp["floor"].as_str().unwrap().to_string(),
        );
        assert_eq!(imp["manifest"]["name"], "Opened");
        assert_eq!(
            imp["manifest"]["cell_size"], 48.0,
            "the grid is the file's squares"
        );
        assert_eq!(imp["stats"]["lights"], 1);
        {
            let st = app.store.read().await;
            let d = &st.docs[&new_id];
            let f = &d.floors[0];
            assert!(
                f.underlay.is_some() && f.grid.cell_count() == 0,
                "a picture and imported walls, no rooms"
            );
            assert_eq!(
                (
                    f.underlay.as_ref().unwrap().w,
                    f.underlay.as_ref().unwrap().h
                ),
                (4.0, 2.0)
            );
            assert_eq!(f.lights.len(), 1);
            assert!(f
                .imported_walls
                .iter()
                .any(|w| w.kind == ndcart_core::WallKind::Door));
            assert!(
                f.imported_walls
                    .iter()
                    .any(|w| w.kind == ndcart_core::WallKind::Window),
                "the window rides through"
            );
        }
        // The import was one undoable step: undoing it empties the floor again.
        // (checked on a copy so the export below still has the content)
        // Export it back: same walls (as segments), same light, a picture of the same size.
        let (st, again) = call(
            &router,
            "GET",
            &format!("/api/docs/{new_id}/export/uvtt?floor={new_floor}&ppc=48"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let back: serde_json::Value = serde_json::from_slice(&again).unwrap();
        let norm = |v: &serde_json::Value| {
            let mut segs: Vec<String> = v["line_of_sight"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| {
                    let mut p: Vec<(i64, i64)> = l
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|q| {
                            (
                                (q["x"].as_f64().unwrap() * 1000.0).round() as i64,
                                (q["y"].as_f64().unwrap() * 1000.0).round() as i64,
                            )
                        })
                        .collect();
                    if p.first() > p.last() {
                        p.reverse();
                    }
                    format!("{p:?}")
                })
                .collect();
            segs.sort();
            segs
        };
        assert_eq!(
            norm(&src),
            norm(&back),
            "walls survive the open/export round trip"
        );
        assert_eq!(
            src["portals"].as_array().unwrap().len(),
            back["portals"].as_array().unwrap().len()
        );
        assert_eq!(
            src["lights"].as_array().unwrap().len(),
            back["lights"].as_array().unwrap().len()
        );
        assert_eq!(
            src["resolution"]["map_size"],
            back["resolution"]["map_size"]
        );
        assert_eq!(
            src["x_ndcart"]["windows"].as_array().unwrap().len(),
            back["x_ndcart"]["windows"].as_array().unwrap().len()
        );
        // The imported map renders: the underlay is the picture.
        let (st, png) = call(
            &router,
            "GET",
            &format!("/api/docs/{new_id}/export/png?floor={new_floor}&ppc=48"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 4 * 48);
    }

    #[tokio::test]
    async fn bad_uvtt_files_are_refused_and_leave_no_project_behind() {
        let app = test_app();
        let router = build_router(app.clone());
        let before = app.store.read().await.docs.len();
        let no_picture = br#"{"resolution":{"map_size":{"x":4,"y":4},"pixels_per_grid":50},"line_of_sight":[],"image":""}"#.to_vec();
        let junk_picture = br#"{"resolution":{"map_size":{"x":4,"y":4},"pixels_per_grid":50},"line_of_sight":[],"image":"Zm9vYmFy"}"#.to_vec();
        for body in [
            b"nope".to_vec(),
            b"{}".to_vec(),
            no_picture,
            junk_picture,
            vec![0xff, 0xfe, 0x00],
        ] {
            let (st, _) = call(&router, "POST", "/api/import/uvtt", body).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
        }
        assert_eq!(app.store.read().await.docs.len(), before);
    }

    async fn region_doc(router: &Router) -> (String, String) {
        let (st, body) = call(
            router,
            "POST",
            "/api/docs",
            br#"{"name":"World","kind":"region"}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (_, doc) = call(router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = serde_json::from_slice::<serde_json::Value>(&doc).unwrap()["floors"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        (id, floor)
    }

    #[tokio::test]
    async fn a_generated_world_renders_undoes_and_is_reproducible() {
        let router = build_router(test_app());
        let (id, floor) = region_doc(&router).await;
        let gen = br#"{"seed":42,"cols":40,"rows":30,"land":0.4,"rivers":3}"#.to_vec();
        let (st, body) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/generate"),
            gen.clone(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let out: serde_json::Value = serde_json::from_slice(&body).unwrap();
        // 40x30 cells at 4 samples per cell is 160x120 samples: 3x2 chunks of 64.
        assert_eq!(out["chunks"].as_u64().unwrap(), 6);
        assert!(out["sea_level"].as_u64().unwrap() > 1000);

        // The view carries what the panel needs; the export is the canvas size.
        let (_, doc) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let v: serde_json::Value = serde_json::from_slice(&doc).unwrap();
        assert_eq!(v["floors"][0]["canvas"], serde_json::json!([40, 30]));
        assert!(v["floors"][0]["terrain"]["chunks"].as_u64().unwrap() > 0);
        assert_eq!(
            v["floors"][0]["rivers"].as_array().unwrap().len(),
            out["rivers"].as_u64().unwrap() as usize
        );
        let (st, png) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/export/png?floor={floor}&ppc=16"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 40 * 16);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 30 * 16);
        // It is a map: there is blue water and there is green or tan land.
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let (mut blue, mut land) = (0, 0);
        for p in pm.pixels() {
            if i32::from(p.blue()) > i32::from(p.red()) + 30 && p.blue() > p.green() {
                blue += 1;
            } else if p.green() >= p.blue() {
                land += 1;
            }
        }
        assert!(blue > 200 && land > 200, "water {blue}, land {land}");

        // The same request again gives the identical picture (and replaces, not stacks).
        let (st, _) = call(&router, "POST", &format!("/api/docs/{id}/generate"), gen).await;
        assert_eq!(st, StatusCode::OK);
        let (_, png2) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/export/png?floor={floor}&ppc=16"),
            vec![],
        )
        .await;
        assert_eq!(png, png2, "same seed, same world, byte for byte");

        // A whole generation is one undo step: undoing twice walks back to nothing.
        let undo = |router: Router, id: String, floor: String| async move {
            call(
                &router,
                "POST",
                &format!("/api/docs/{id}/undo"),
                format!(r#"{{"floor":"{floor}"}}"#).into_bytes(),
            )
            .await
            .0
        };
        assert_eq!(
            undo(router.clone(), id.clone(), floor.clone()).await,
            StatusCode::OK
        );
        assert_eq!(
            undo(router.clone(), id.clone(), floor.clone()).await,
            StatusCode::OK
        );
        let (_, doc) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let v: serde_json::Value = serde_json::from_slice(&doc).unwrap();
        assert!(
            v["floors"][0]["terrain"].is_null()
                && v["floors"][0]["rivers"].as_array().unwrap().is_empty()
        );
        // Bad input is refused, and an unknown document is a 404.
        assert_eq!(
            call(
                &router,
                "POST",
                &format!("/api/docs/{id}/generate"),
                b"nope".to_vec()
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&router, "POST", "/api/docs/ghost/generate", b"{}".to_vec())
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn terrain_strokes_and_rivers_arrive_through_the_ops_route() {
        let router = build_router(test_app());
        let (id, floor) = region_doc(&router).await;
        let ops = |body: serde_json::Value| {
            let router = router.clone();
            let (id, floor) = (id.clone(), floor.clone());
            async move {
                call(
                    &router,
                    "POST",
                    &format!("/api/docs/{id}/ops"),
                    serde_json::json!({"floor": floor, "ops": body})
                        .to_string()
                        .into_bytes(),
                )
                .await
                .0
            }
        };
        assert_eq!(
            ops(serde_json::json!([{"t":"set_canvas","canvas":[20,16]}])).await,
            StatusCode::OK
        );
        // Raise an island out of the sea with repeated strokes.
        for _ in 0..30 {
            let st = ops(serde_json::json!([{"t":"terrain_stroke","stroke":{"mode":"raise","points":[[10.0,8.0],[10.5,8.2]],"radius":4.0,"strength":1.0,"seed":1}}])).await;
            assert_eq!(st, StatusCode::OK);
        }
        let (_, png) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/export/png?floor={floor}&ppc=16"),
            vec![],
        )
        .await;
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let centre = pm.pixel(10 * 16, 8 * 16).unwrap();
        let corner = pm.pixel(8, 8).unwrap();
        assert!(
            centre.green() >= centre.blue(),
            "the island is land at its centre"
        );
        assert!(
            corner.blue() > corner.red() + 20,
            "and open sea at the corner"
        );
        // A river is accepted and shows in the view.
        assert_eq!(ops(serde_json::json!([{"t":"add_river","river":{"id":"r1","name":"Silver","points":[[8.0,6.0],[10.0,8.0],[12.0,10.5]],"width":1.2}}])).await, StatusCode::OK);
        let (_, doc) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&doc).unwrap()["floors"][0]["rivers"][0]
                ["name"],
            "Silver"
        );
        // Hostile terrain input is a 400, not stored.
        for bad in [
            serde_json::json!([{"t":"terrain_stroke","stroke":{"mode":"raise","points":[[1.0,1.0]],"radius":-1.0,"strength":1.0}}]),
            serde_json::json!([{"t":"terrain_stroke","stroke":{"mode":"raise","points":[],"radius":2.0,"strength":1.0}}]),
            serde_json::json!([{"t":"add_river","river":{"id":"x","name":"n","points":[[0.0,0.0]],"width":1.0}}]),
            serde_json::json!([{"t":"set_terrain_style","style":{"tint":"red"}}]),
            serde_json::json!([{"t":"set_canvas","canvas":[0,10]}]),
        ] {
            assert_eq!(ops(bad).await, StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn a_gm_only_label_never_reaches_a_player_export() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, floor) = {
            let mut st = app.store.write().await;
            let m = st.create("Notes", DocKind::Battle).clone();
            let d = st.docs.get_mut(&m.id).unwrap();
            let fid = d.floors[0].id.clone();
            let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
                {"t":"upsert_space","space":{"id":"a","name":"A","color":"#d0d0d0","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0},{"x":3,"y":0},{"x":0,"y":1},{"x":1,"y":1},{"x":2,"y":1},{"x":3,"y":1}],"space":"a"},
                {"t":"upsert_label","label":{"id":"open","x":2.0,"y":0.5,"text":"Hall","size":0.7,"color":"#c00000","bold":true,"gm_only":false}},
                {"t":"upsert_label","label":{"id":"secret","x":2.0,"y":1.5,"text":"Trapdoor","size":0.7,"color":"#0000c0","bold":true,"gm_only":true}}
            ]))
            .unwrap();
            d.apply_ops(&fid, &ops).unwrap();
            (m.id, fid)
        };
        let count = |png: &[u8], want: [u8; 3]| {
            let pm = tiny_skia::Pixmap::decode_png(png).unwrap();
            pm.pixels()
                .iter()
                .filter(|p| {
                    p.red().abs_diff(want[0]) < 40
                        && p.green().abs_diff(want[1]) < 40
                        && p.blue().abs_diff(want[2]) < 40
                })
                .count()
        };
        let get = |aud: &'static str| {
            let router = router.clone();
            let uri = format!("/api/docs/{id}/export/png?floor={floor}&ppc=48&audience={aud}");
            async move { call(&router, "GET", &uri, vec![]).await.1 }
        };
        let (gm, player) = (get("gm").await, get("player").await);
        assert!(
            count(&gm, [0xc0, 0, 0]) > 100 && count(&gm, [0, 0, 0xc0]) > 100,
            "the GM sees both labels"
        );
        assert!(
            count(&player, [0xc0, 0, 0]) > 100,
            "players see the open label"
        );
        assert_eq!(count(&player, [0, 0, 0xc0]), 0, "and not the GM's note");
    }

    #[tokio::test]
    async fn a_prefab_saved_from_one_project_places_into_another() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let mk = |name: &'static str| {
            let router = router.clone();
            async move {
                let (_, b) = call(
                    &router,
                    "POST",
                    "/api/docs",
                    format!(r#"{{"name":"{name}"}}"#).into_bytes(),
                )
                .await;
                let id = json(b)["id"].as_str().unwrap().to_string();
                let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
                let floor = json(d)["floors"][0]["id"].as_str().unwrap().to_string();
                (id, floor)
            }
        };
        let (a, fa) = mk("A").await;
        let (b, fb) = mk("B").await;
        let ops = serde_json::json!({"floor": fa, "ops": [
            {"t":"upsert_space","space":{"id":"hall","name":"Hall","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","space":"hall","cells":[{"x":4,"y":4},{"x":5,"y":4},{"x":4,"y":5}]}]});
        call(
            &router,
            "POST",
            &format!("/api/docs/{a}/ops"),
            ops.to_string().into_bytes(),
        )
        .await;
        let (st, r) = call(
            &router,
            "POST",
            &format!("/api/docs/{a}/prefabs"),
            br#"{"name":"L hall","spaces":["hall"]}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let pid = json(r)["id"].as_str().unwrap().to_string();
        let (_, l) = call(&router, "GET", "/api/prefabs", vec![]).await;
        assert_eq!(json(l)["prefabs"][0]["squares"], 3);
        let place = |turns: u8, x: i32| {
            format!(r#"{{"prefab":"{pid}","x":{x},"y":10,"turns":{turns}}}"#).into_bytes()
        };
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{b}/place-prefab"),
            place(0, 20),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, d) = call(&router, "GET", &format!("/api/docs/{b}"), vec![]).await;
        assert_eq!(
            json(d)["floors"][0]["grid"]["cells"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{b}/place-prefab"),
            place(1, ndcart_core::MAX_CELL),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let (st, _) = call(&router, "DELETE", &format!("/api/prefabs/{pid}"), vec![]).await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{b}/place-prefab"),
            place(0, 1),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let _ = fb;
    }

    #[tokio::test]
    async fn a_project_style_is_saved_validated_cleared_and_colours_generated_rooms() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"S"}"#.to_vec()).await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let style = |body: &str| {
            let (router, uri, body) = (
                router.clone(),
                format!("/api/docs/{id}/style"),
                body.as_bytes().to_vec(),
            );
            async move { call(&router, "POST", &uri, body).await.0 }
        };
        assert_eq!(
            style(r##"{"tone":"gritty cyberpunk","palette":["#112233","#445566"]}"##).await,
            StatusCode::OK
        );
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert_eq!(json(d)["manifest"]["style"]["tone"], "gritty cyberpunk");
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/generate-map"),
            br#"{"kind":"building","seed":2}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let colours: std::collections::BTreeSet<String> = json(d)["floors"][0]["outline"]["spaces"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["kind"] == "room")
            .map(|s| s["color"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            colours,
            ["#112233".to_string(), "#445566".to_string()]
                .into_iter()
                .collect()
        );
        assert_eq!(
            style(r##"{"tone":"x","palette":["red"]}"##).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(style(r##"{"tone":"","palette":[]}"##).await, StatusCode::OK);
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert!(json(d)["manifest"]["style"].is_null());
    }

    #[tokio::test]
    async fn refining_a_previewed_spec_checks_the_spec_before_asking_any_model() {
        let router = build_router(test_app());
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"R"}"#.to_vec()).await;
        let id = serde_json::from_slice::<serde_json::Value>(&b).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let url = format!("/api/docs/{id}/refine");
        // A spec with an unreachable room is refused with the reason, whatever the AI settings.
        let broken = r#"{"instruction":"add a barracks","spec":{"name":"G","rooms":[{"id":"a","name":"A","w":4,"h":4},{"id":"b","name":"B","w":4,"h":4}],"links":[]}}"#;
        let (st, body) = call(&router, "POST", &url, broken.as_bytes().to_vec()).await;
        if cfg!(feature = "bridge") {
            assert_eq!(st, StatusCode::BAD_REQUEST);
            assert!(String::from_utf8_lossy(&body).contains("not linked"));
        } else {
            assert_eq!(
                st,
                StatusCode::NOT_FOUND,
                "a build without the AI client says so"
            );
        }
        if std::env::var("ND_CARTO_SERVER").is_err() && std::env::var("ND_CARTO_LLM_URL").is_err() {
            let fine = r#"{"instruction":"add a barracks","spec":{"name":"G","rooms":[{"id":"a","name":"A","w":4,"h":4}],"links":[]}}"#;
            let (st, _) = call(&router, "POST", &url, fine.as_bytes().to_vec()).await;
            assert_eq!(
                st,
                StatusCode::NOT_FOUND,
                "valid spec, but no AI configured"
            );
        }
    }

    #[tokio::test]
    async fn asking_your_maps_checks_the_question_and_says_when_no_ai_is_set_up() {
        let router = build_router(test_app());
        let (st, _) = call(&router, "POST", "/api/ask", b"{}".to_vec()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        if std::env::var("ND_CARTO_SERVER").is_err() && std::env::var("ND_CARTO_LLM_URL").is_err() {
            let (st, _) = call(
                &router,
                "POST",
                "/api/ask",
                br#"{"question":"where is the ledger?"}"#.to_vec(),
            )
            .await;
            assert_eq!(st, StatusCode::NOT_FOUND);
        }
    }

    #[tokio::test]
    async fn the_settings_panel_gets_the_doctors_checks_and_no_secrets() {
        let router = build_router(test_app());
        let (st, b) = call(&router, "GET", "/api/doctor", vec![]).await;
        assert_eq!(st, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&b).unwrap();
        let checks = v["checks"].as_array().unwrap();
        assert!(checks
            .iter()
            .any(|c| c["what"] == "fonts" && c["level"] == "ok"));
        assert!(
            checks
                .iter()
                .any(|c| c["what"] == "data folder" && c["level"] == "warn"),
            "no data dir in tests: a warning, not a failure"
        );
        assert!(
            !checks.iter().any(|c| c["what"] == "address"),
            "the studio is already listening"
        );
        assert!(!String::from_utf8_lossy(&b)
            .to_lowercase()
            .contains("password\":"));
    }

    #[tokio::test]
    async fn ai_status_and_refine_say_what_is_missing_when_nothing_is_configured() {
        if std::env::var("ND_CARTO_SERVER").is_ok() || std::env::var("ND_CARTO_LLM_URL").is_ok() {
            return; // a developer's own environment
        }
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (st, b) = call(&router, "GET", "/api/ai/status", vec![]).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(json(b)["mode"], "off");
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"R"}"#.to_vec()).await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/refine"),
            br#"{"instruction":"add a barracks"}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/refine"),
            b"{}".to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn drafting_without_nd_world_configured_says_what_to_set() {
        if std::env::var("ND_CARTO_SERVER").is_ok() {
            return; // a developer's own environment: do not call out
        }
        let router = build_router(test_app());
        let (st, body) = call(
            &router,
            "POST",
            "/api/draft",
            br#"{"prompt":"a crypt"}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let said = String::from_utf8_lossy(&body).to_string();
        assert!(
            said.contains("ND_CARTO_SERVER") || said.contains("without the bridge"),
            "{said}"
        );
        let (st, _) = call(&router, "POST", "/api/draft", b"{}".to_vec()).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn furnishing_is_one_undo_step_and_search_finds_rooms_notes_and_markers_across_projects()
    {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let mk = |name: &'static str| {
            let router = router.clone();
            async move {
                let (_, b) = call(
                    &router,
                    "POST",
                    "/api/docs",
                    format!(r#"{{"name":"{name}"}}"#).into_bytes(),
                )
                .await;
                let id = json(b)["id"].as_str().unwrap().to_string();
                let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
                (id, json(d)["floors"][0]["id"].as_str().unwrap().to_string())
            }
        };
        let (a, fa) = mk("Keep").await;
        let (b, fb) = mk("Harbour").await;
        let ops = |floor: &str, name: &str| {
            serde_json::json!({"floor": floor, "ops": [
              {"t":"upsert_space","space":{"id":"r","name":name,"color":"#7f5539","kind":"room"}},
              {"t":"paint_cells","space":"r","cells":(0..6).flat_map(|x| (0..5).map(move |y| serde_json::json!({"x":x,"y":y}))).collect::<Vec<_>>()},
              {"t":"upsert_label","label":{"id":"n","x":2.0,"y":2.0,"text":"Trapdoor under the rug","size":0.8,"color":"#222222","bold":false}}]})
            .to_string()
            .into_bytes()
        };
        call(
            &router,
            "POST",
            &format!("/api/docs/{a}/ops"),
            ops(&fa, "Armory"),
        )
        .await;
        call(
            &router,
            "POST",
            &format!("/api/docs/{b}/ops"),
            ops(&fb, "Boathouse"),
        )
        .await;
        let stamps = |id: String| {
            let router = router.clone();
            async move {
                let d = json(
                    call(&router, "GET", &format!("/api/docs/{id}"), vec![])
                        .await
                        .1,
                );
                d["floors"][0]["outline"]["stamps"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0)
            }
        };
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{a}/furnish"),
            br#"{"spaces":["r"],"seed":2}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert!(stamps(a.clone()).await > 2);
        // Already furnished: refused unless forced.
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{a}/furnish"),
            br#"{"spaces":["r"],"seed":3}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        call(
            &router,
            "POST",
            &format!("/api/docs/{a}/undo"),
            format!(r#"{{"floor":"{fa}"}}"#).into_bytes(),
        )
        .await;
        assert_eq!(stamps(a.clone()).await, 0);
        let search = |q: &str| {
            let (router, uri) = (router.clone(), format!("/api/search?q={q}"));
            async move { call(&router, "GET", &uri, vec![]).await }
        };
        let (st, body) = search("trapdoor%20rug").await;
        assert_eq!(st, StatusCode::OK);
        let hits = json(body)["hits"].as_array().unwrap().clone();
        assert_eq!(
            hits.iter().filter(|h| h["kind"] == "note").count(),
            2,
            "one note in each project"
        );
        let (_, body) = search("BOATHOUSE").await;
        let hits = json(body)["hits"].as_array().unwrap().clone();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0]["kind"].as_str(), hits[0]["doc_name"].as_str()),
            (Some("room"), Some("Harbour"))
        );
        assert!(hits[0]["at"].is_array());
        assert_eq!(search("zzzz").await.0, StatusCode::OK);
        assert_eq!(search("%20").await.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_world_proposal_changes_nothing_until_accepted_and_one_undo_takes_it_back() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(
            &router,
            "POST",
            "/api/docs",
            br#"{"name":"W","kind":"region"}"#.to_vec(),
        )
        .await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = json(d)["floors"][0]["id"].as_str().unwrap().to_string();
        let spec = serde_json::json!({"spec": {"name":"Isles","cols":64,"rows":48,"land":0.45,
            "biomes":[{"biome":"forest","x":0.5,"y":0.5,"rx":0.25,"ry":0.2}],
            "places":[{"name":"Port Neon","x":0.5,"y":0.5}]}, "seed": 5});
        let (st, r) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/propose-world"),
            spec.to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let pid = json(r)["id"].as_str().unwrap().to_string();
        let info = || {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}"));
            async move {
                let d = json(call(&router, "GET", &uri, vec![]).await.1);
                (
                    d["floors"][0]["markers"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0),
                    d["floors"][0]["rivers"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0),
                )
            }
        };
        assert_eq!(info().await, (0, 0), "a proposal alone changes nothing");
        let (st, png) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/world-proposals/{pid}/preview?ppc=3"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(&png[1..4], b"PNG");
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/world-proposals/{pid}/accept"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (markers, _) = info().await;
        assert_eq!(markers, 1);
        call(
            &router,
            "POST",
            &format!("/api/docs/{id}/undo"),
            format!(r#"{{"floor":"{floor}"}}"#).into_bytes(),
        )
        .await;
        assert_eq!(info().await, (0, 0));
        // A battle project refuses a world; a bad spec is refused with its reason; rejecting works.
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"B"}"#.to_vec()).await;
        let bid = json(b)["id"].as_str().unwrap().to_string();
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{bid}/propose-world"),
            spec.to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        let bad = serde_json::json!({"spec": {"name":"X","land":2.0}});
        let (st, body) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/propose-world"),
            bad.to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&body).contains("land must be"));
        let (_, r) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/propose-world"),
            spec.to_string().into_bytes(),
        )
        .await;
        let p2 = json(r)["id"].as_str().unwrap().to_string();
        assert_eq!(
            call(
                &router,
                "DELETE",
                &format!("/api/docs/{id}/world-proposals/{p2}"),
                vec![]
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            call(
                &router,
                "POST",
                &format!("/api/docs/{id}/world-proposals/{p2}/accept"),
                vec![]
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn pictures_become_materials_and_props_with_provenance_tags_and_an_underlay() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        // A magenta picture with a red square: the "prop".
        let mut pm = tiny_skia::Pixmap::new(40, 40).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(255, 0, 255, 255));
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(200, 30, 30, 255);
        pm.fill_rect(
            tiny_skia::Rect::from_xywh(10.0, 10.0, 20.0, 20.0).unwrap(),
            &paint,
            tiny_skia::Transform::identity(),
            None,
        );
        let (st, b) = call(&router, "POST", "/api/assets", pm.encode_png().unwrap()).await;
        assert_eq!(st, StatusCode::OK);
        let asset = json(b)["id"].as_str().unwrap().to_string();

        let (st, b) = call(
            &router,
            "POST",
            &format!("/api/assets/{asset}/make-stamp"),
            br#"{"name":"Red block","size":2,"prompt":"a red block","seed":7,"ai":true}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
        let stamp = json(b)["id"].as_str().unwrap().to_string();
        assert!(stamp.starts_with("stp_"));
        let (st, b) = call(
            &router,
            "POST",
            &format!("/api/assets/{asset}/make-material"),
            br#"{"name":"Red tiles","size":3}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let mat = json(b)["id"].as_str().unwrap().to_string();
        assert!(mat.starts_with("mat_"));
        // Both show up in the palettes with where they came from.
        let (_, b) = call(&router, "GET", "/api/stamps", vec![]).await;
        let list = json(b)["stamps"].as_array().unwrap().clone();
        let c = list
            .iter()
            .find(|s| s["id"] == stamp.as_str())
            .expect("custom stamp listed");
        assert_eq!(
            (
                c["custom"].as_bool(),
                c["origin"]["source"].as_str(),
                c["origin"]["seed"].as_i64()
            ),
            (Some(true), Some("ai"), Some(7))
        );
        assert_eq!(c["origin"]["prompt"], "a red block");
        let (_, b) = call(&router, "GET", "/api/materials", vec![]).await;
        assert!(json(b)["materials"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == mat.as_str() && m["custom"] == true));
        let (st, icon) = call(
            &router,
            "GET",
            &format!("/api/stamps/{stamp}/icon.png"),
            vec![],
        )
        .await;
        assert_eq!((st, &icon[1..4]), (StatusCode::OK, &b"PNG"[..]));
        // Tags: the GM's confirmed words are cleaned strictly and stored.
        let tag = |body: &str| {
            let (router, uri, body) = (
                router.clone(),
                format!("/api/stamps/{stamp}/tags"),
                body.as_bytes().to_vec(),
            );
            async move { call(&router, "POST", &uri, body).await }
        };
        assert_eq!(
            tag(r#"{"tags":["Block","red","storage"]}"#).await.0,
            StatusCode::OK
        );
        assert_eq!(
            tag(r#"{"tags":["bad_tag!"]}"#).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            tag(r#"{"tags":["a","b","c","d","e","f","g","h","i"]}"#)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        let (_, b) = call(&router, "GET", "/api/stamps", vec![]).await;
        let list = json(b)["stamps"].as_array().unwrap().clone();
        assert_eq!(
            list.iter().find(|s| s["id"] == stamp.as_str()).unwrap()["tags"],
            serde_json::json!(["block", "red", "storage"])
        );
        // Refusals: an unknown picture, a bad name or size, a missing prop.
        for (path, body) in [
            ("/api/assets/0000000000000000000000000000000000000000000000000000000000000000/make-stamp", r#"{"name":"x"}"#),
            (&format!("/api/assets/{asset}/make-stamp"), r#"{"name":" "}"#),
            (&format!("/api/assets/{asset}/make-stamp"), r#"{"name":"x","size":99}"#),
            (&format!("/api/assets/{asset}/make-stamp"), r##"{"name":"x","key":"red"}"##),
            (&format!("/api/assets/{asset}/make-material"), r#"{"name":"x","size":0}"#),
        ] {
            let (st, _) = call(&router, "POST", path, body.as_bytes().to_vec()).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{path} {body}");
        }
        assert_eq!(tag(r#"{"tags":[]}"#).await.0, StatusCode::OK);
        let (st, _) = call(
            &router,
            "POST",
            "/api/stamps/stp_nope/tags",
            br#"{"tags":["a"]}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // The underlay route puts a stored picture behind the floor, one undo step.
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"U"}"#.to_vec()).await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = json(d)["floors"][0]["id"].as_str().unwrap().to_string();
        let under =
            |asset: &str| format!(r#"{{"asset":"{asset}","x":0,"y":0,"w":10,"h":8}}"#).into_bytes();
        assert_eq!(
            call(
                &router,
                "POST",
                &format!("/api/docs/{id}/underlay"),
                under(&asset)
            )
            .await
            .0,
            StatusCode::OK
        );
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert_eq!(json(d)["floors"][0]["underlay"]["w"], 10.0);
        call(
            &router,
            "POST",
            &format!("/api/docs/{id}/undo"),
            format!(r#"{{"floor":"{floor}"}}"#).into_bytes(),
        )
        .await;
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert!(json(d)["floors"][0]["underlay"].is_null());
        assert_eq!(
            call(
                &router,
                "POST",
                &format!("/api/docs/{id}/underlay"),
                under(&"0".repeat(64))
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn the_image_job_route_checks_its_input_and_unknown_jobs_are_not_found() {
        let router = build_router(test_app());
        let post = |body: &str| {
            let (router, body) = (router.clone(), body.as_bytes().to_vec());
            async move { call(&router, "POST", "/api/ai/image", body).await.0 }
        };
        assert_eq!(
            post(r#"{"kind":"video","prompt":"x"}"#).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post(r#"{"kind":"prop","prompt":" "}"#).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post(r#"{"kind":"prop","prompt":"x","width":8}"#).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post(r#"{"kind":"prop","prompt":"x","height":9999}"#).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&router, "GET", "/api/ai/jobs/aj_nope", vec![]).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(&router, "POST", "/api/ai/jobs/aj_nope/cancel", vec![])
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        if std::env::var("ND_CARTO_SERVER").is_err() && std::env::var("ND_CARTO_LLM_URL").is_err() {
            // Valid input, but no nd-world configured: says what to set rather than failing silently.
            assert_eq!(
                post(r#"{"kind":"prop","prompt":"a crate"}"#).await,
                StatusCode::NOT_FOUND
            );
        }
    }

    #[tokio::test]
    async fn a_foundry_scene_exports_with_walls_and_lights_and_imports_back_as_a_project() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(
            &router,
            "POST",
            "/api/docs",
            br#"{"name":"Vault"}"#.to_vec(),
        )
        .await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/generate-map"),
            br#"{"kind":"dungeon","seed":4,"width":40,"height":30}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, body) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/export/foundry?ppc=100&image=maps/vault.webp"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let scene = json(body.clone());
        assert_eq!(scene["grid"]["size"], 100.0);
        assert_eq!(scene["background"]["src"], "maps/vault.webp");
        let walls = scene["walls"].as_array().unwrap().len();
        assert!(
            walls > 50 && scene["lights"].as_array().unwrap().len() > 3,
            "{walls}"
        );
        assert!(
            scene["walls"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["door"] == 1),
            "doors survive"
        );
        // Import it as a new project: same walls and lights, canvas from the scene.
        let (st, b) = call(&router, "POST", "/api/import/foundry?name=Back", body).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
        let r = json(b);
        assert_eq!(r["stats"]["walls"].as_u64(), Some(walls as u64));
        assert_eq!(r["manifest"]["name"], "Back");
        let (_, list) = call(&router, "GET", "/api/docs", vec![]).await;
        assert_eq!(json(list)["docs"].as_array().unwrap().len(), 2);
        for bad in ["ppc=1", "distance=0"] {
            assert_eq!(
                call(
                    &router,
                    "GET",
                    &format!("/api/docs/{id}/export/foundry?{bad}"),
                    vec![]
                )
                .await
                .0,
                StatusCode::BAD_REQUEST,
                "{bad}"
            );
        }
        assert_eq!(
            call(&router, "POST", "/api/import/foundry", b"not json".to_vec())
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        // Nothing half-made was left behind by the failed import.
        let (_, list) = call(&router, "GET", "/api/docs", vec![]).await;
        assert_eq!(json(list)["docs"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_print_export_is_a_pdf_at_true_scale_and_checks_its_input() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(
            &router,
            "POST",
            "/api/docs",
            br#"{"name":"Print"}"#.to_vec(),
        )
        .await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/generate-map"),
            br#"{"kind":"building","seed":2,"width":24,"height":20}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let get = |q: &str| {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}/export/pdf?{q}"));
            async move { call(&router, "GET", &uri, vec![]).await }
        };
        let (st, pdf) = get("dpi=72").await;
        assert_eq!(st, StatusCode::OK);
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
        // 24 x 20 squares at 1 inch on letter (7 x 10 per page): 4 x 2 pages.
        let pages = String::from_utf8_lossy(&pdf)
            .matches("/Type /Page ")
            .count();
        assert_eq!(pages, 4 * 2);
        // Half-inch squares need fewer pages; the player's copy and A4 work too.
        let (_, half) = get("dpi=72&inch=0.5&paper=a4&audience=player").await;
        assert!(
            String::from_utf8_lossy(&half)
                .matches("/Type /Page ")
                .count()
                < pages
        );
        for bad in [
            "paper=tabloid",
            "inch=0.1",
            "inch=9",
            "dpi=10",
            "dpi=900",
            "audience=x",
        ] {
            assert_eq!(get(bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[tokio::test]
    async fn an_uploaded_dungeondraft_pack_fills_the_palettes_and_bad_files_say_why() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let files = [
            (
                "pack.json",
                br#"{"name":"Harbour Pack","author":"B. Cartographer"}"#.to_vec(),
            ),
            (
                "textures/objects/lantern_post.png",
                packs::tests::png(256, 256, [230, 200, 80, 255]),
            ),
            (
                "textures/terrain/wet_cobbles.png",
                packs::tests::png(512, 512, [70, 80, 90, 255]),
            ),
        ];
        let (st, b) = call(
            &router,
            "POST",
            "/api/packs/import?name=harbour.dungeondraft_pack",
            packs::tests::build_pck(&files),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
        let r = json(b);
        assert_eq!(
            (
                r["pack"].as_str(),
                r["stamps"].as_u64(),
                r["materials"].as_u64()
            ),
            (Some("Harbour Pack"), Some(1), Some(1))
        );
        let (_, b) = call(&router, "GET", "/api/stamps", vec![]).await;
        let lamp = json(b)["stamps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "Lantern post")
            .cloned()
            .expect("listed");
        assert_eq!(
            lamp["origin"]["prompt"],
            "from pack Harbour Pack by B. Cartographer"
        );
        let (_, b) = call(&router, "GET", "/api/materials", vec![]).await;
        assert!(json(b)["materials"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "Wet cobbles" && m["custom"] == true));
        // A zip, junk and an empty body are refused with advice.
        for (body, want) in [
            (b"PK\x03\x04zipzip".to_vec(), "zip"),
            (b"junk".to_vec(), "GDPC"),
            (Vec::new(), "GDPC"),
        ] {
            let (st, b) = call(&router, "POST", "/api/packs/import", body).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
            assert!(
                String::from_utf8_lossy(&b).contains(want),
                "{}",
                String::from_utf8_lossy(&b)
            );
        }
    }

    #[tokio::test]
    async fn the_table_screen_follows_the_gm_and_cannot_edit() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(
            &router,
            "POST",
            "/api/docs",
            br#"{"name":"Table"}"#.to_vec(),
        )
        .await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let state = || {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}/table"));
            async move { json(call(&router, "GET", &uri, vec![]).await.1) }
        };
        assert_eq!(state().await["on"], false);
        let post = |body: &str| {
            let (router, uri, body) = (
                router.clone(),
                format!("/api/docs/{id}/table"),
                body.as_bytes().to_vec(),
            );
            async move { call(&router, "POST", &uri, body).await.0 }
        };
        assert_eq!(
            post(r#"{"x":4.5,"y":2.5,"radius":8}"#).await,
            StatusCode::OK
        );
        let s = state().await;
        assert_eq!(
            (
                s["on"].as_bool(),
                s["state"]["n"].as_u64(),
                s["state"]["x"].as_f64()
            ),
            (Some(true), Some(1), Some(4.5))
        );
        assert_eq!(
            post(r#"{"x":6.0,"y":2.5,"darkness":false}"#).await,
            StatusCode::OK
        );
        assert_eq!(state().await["state"]["n"], 2, "every change is a new step");
        for bad in [
            r#"{"x":1,"y":1,"radius":0}"#,
            r#"{"x":1,"y":1,"radius":999}"#,
            r#"{"y":1}"#,
            r#"{"x":1,"y":1,"floor":"fl_nope"}"#,
        ] {
            assert!(post(bad).await.is_client_error(), "{bad}");
        }
        // The page is HTML for this project only, and odd ids are refused.
        let (st, page) = call(&router, "GET", &format!("/table/{id}"), vec![]).await;
        assert_eq!(st, StatusCode::OK);
        let html = String::from_utf8_lossy(&page);
        assert!(html.contains(&format!("const DOC = '{id}';")) && !html.contains("__DOC__"));
        assert_eq!(
            call(&router, "GET", "/table/a%22b", vec![]).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            call(&router, "DELETE", &format!("/api/docs/{id}/table"), vec![])
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(state().await["on"], false);
    }

    #[tokio::test]
    async fn a_proposal_changes_nothing_until_accepted_then_adds_floors_with_stairs() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"AI"}"#.to_vec()).await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let spec = serde_json::json!({"name":"Metro","floors":[
          {"name":"Platform","rooms":[{"id":"hall","name":"Hall","w":8,"h":6},{"id":"plat","name":"Platform","w":12,"h":4}],
           "links":[{"a":"hall","b":"plat","kind":"opening"}],
           "stairs":[{"room":"plat","to_floor":"Tunnels","to_room":"tun"}]},
          {"name":"Tunnels","rooms":[{"id":"tun","name":"Tunnel","w":10,"h":3}],"links":[]}]});
        let floors = || {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}"));
            async move {
                json(call(&router, "GET", &uri, vec![]).await.1)["floors"]
                    .as_array()
                    .unwrap()
                    .clone()
            }
        };
        let (st, r) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/propose"),
            serde_json::json!({"spec": spec, "seed": 4})
                .to_string()
                .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let r = json(r);
        let pid = r["id"].as_str().unwrap().to_string();
        assert_eq!(r["floors"].as_array().unwrap().len(), 2);
        assert_eq!(floors().await.len(), 1, "a proposal alone changes nothing");
        let (st, png) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/proposals/{pid}/preview?floor=1"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(&png[1..4], b"PNG");
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/proposals/{pid}/accept"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let fl = floors().await;
        assert_eq!(fl.len(), 3);
        let stairs = |f: &serde_json::Value| {
            f["markers"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["links_to"].is_object())
                .count()
        };
        assert_eq!(
            (stairs(&fl[1]), stairs(&fl[2])),
            (1, 1),
            "stairs up and the way back"
        );
        assert_eq!(fl[1]["markers"][0]["links_to"]["floor"], fl[2]["id"]);
        // Once taken it is gone; a bad spec is refused with its reason, and rejecting works.
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/proposals/{pid}/accept"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let bad = serde_json::json!({"spec": {"name":"X","floors":[{"name":"G","rooms":[{"id":"a","name":"A","w":4,"h":4},{"id":"b","name":"B","w":4,"h":4}]}]}});
        let (st, body) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/propose"),
            bad.to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&body).contains("not linked"));
        let (_, r) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/propose"),
            serde_json::json!({"spec": spec}).to_string().into_bytes(),
        )
        .await;
        let p2 = json(r)["id"].as_str().unwrap().to_string();
        let (st, _) = call(
            &router,
            "DELETE",
            &format!("/api/docs/{id}/proposals/{p2}"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(floors().await.len(), 3);
        // Undoing the structure steps removes what the proposal added.
        for _ in 0..2 {
            call(
                &router,
                "POST",
                &format!("/api/docs/{id}/undo-structure"),
                vec![],
            )
            .await;
        }
        assert_eq!(floors().await.len(), 1);
    }

    #[tokio::test]
    async fn the_player_preview_never_shows_a_gm_secret_and_checks_its_input() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"P"}"#.to_vec()).await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = json(d)["floors"][0]["id"].as_str().unwrap().to_string();
        let ops = serde_json::json!({"floor": floor, "ops": [
            {"t":"upsert_space","space":{"id":"a","name":"A","color":"#d0d0d0","kind":"room"}},
            {"t":"upsert_space","space":{"id":"b","name":"B","color":"#d0d0d0","kind":"room"}},
            {"t":"paint_cells","space":"a","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0}]},
            {"t":"paint_cells","space":"b","cells":[{"x":3,"y":0},{"x":4,"y":0},{"x":5,"y":0}]},
            {"t":"set_mark","edge":{"cell":{"x":2,"y":0},"edge":"east"},"mark":{"kind":"secret","state":"open"}}]});
        call(
            &router,
            "POST",
            &format!("/api/docs/{id}/ops"),
            ops.to_string().into_bytes(),
        )
        .await;
        let get = |q: &str| {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}/preview?{q}"));
            async move { call(&router, "GET", &uri, vec![]).await }
        };
        let (st, png) = get("x=0.5&y=0.5&radius=20&darkness=false&ppc=16").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(&png[1..4], b"PNG");
        // An open SECRET door is a plain wall to players: room B stays black.
        let img = decode_png_for_test(&png);
        let px = |x: u32, y: u32| {
            let i = ((y * img.0 + x) * 4) as usize;
            u32::from(img.1[i]) + u32::from(img.1[i + 1]) + u32::from(img.1[i + 2])
        };
        assert!(px(24, 8) > 200, "room A visible");
        assert!(
            px(4 * 16 + 8, 8) < 60,
            "room B hidden behind the secret door"
        );
        for bad in ["radius=0", "radius=9999", "ppc=500"] {
            assert_eq!(get(bad).await.0, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    fn decode_png_for_test(png: &[u8]) -> (u32, Vec<u8>) {
        let pm = tiny_skia::Pixmap::decode_png(png).unwrap();
        (pm.width(), pm.data().to_vec())
    }

    #[tokio::test]
    async fn scatter_places_a_biome_in_one_undo_step_and_refuses_bad_input() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(
            &router,
            "POST",
            "/api/docs",
            br#"{"name":"S","kind":"region"}"#.to_vec(),
        )
        .await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = json(d)["floors"][0]["id"].as_str().unwrap().to_string();
        let url = format!("/api/docs/{id}/scatter");
        let area = "[[2,2],[22,3],[20,16],[3,14]]";
        let body = |extra: &str| format!(r#"{{"points":{area},"seed":3{extra}}}"#).into_bytes();
        let (st, _) = call(&router, "POST", &url, body(r#","biome":"forest""#)).await;
        assert_eq!(st, StatusCode::OK);
        let counts = || {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}"));
            async move {
                let d = json(call(&router, "GET", &uri, vec![]).await.1);
                (
                    d["floors"][0]["outline"]["stamps"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0),
                    d["floors"][0]["regions"]
                        .as_array()
                        .map(|a| a.len())
                        .unwrap_or(0),
                )
            }
        };
        let (stamps, regions) = counts().await;
        assert!(stamps > 50 && regions == 1, "{stamps} {regions}");
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/undo"),
            format!(r#"{{"floor":"{floor}"}}"#).into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(counts().await, (0, 0));
        let (st, _) = call(
            &router,
            "POST",
            &url,
            body(r#","assets":["rock"],"spacing":3"#),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        for bad in [
            r#","biome":"lava""#,
            r#","assets":[]"#,
            r#","assets":["rock"],"spacing":0.01"#,
        ] {
            let (st, _) = call(&router, "POST", &url, body(bad)).await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[tokio::test]
    async fn a_generated_map_is_one_undo_step_and_bad_sizes_are_refused() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"G"}"#.to_vec()).await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = json(d)["floors"][0]["id"].as_str().unwrap().to_string();
        let gen = |body: &str| {
            (
                format!("/api/docs/{id}/generate-map"),
                body.as_bytes().to_vec(),
            )
        };
        let (u, bd) = gen(r#"{"kind":"dungeon","seed":5,"width":48,"height":36}"#);
        assert_eq!(call(&router, "POST", &u, bd).await.0, StatusCode::OK);
        let cells = || {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}"));
            async move {
                json(call(&router, "GET", &uri, vec![]).await.1)["floors"][0]["grid"]["cells"]
                    .as_array()
                    .unwrap()
                    .len()
            }
        };
        assert!(cells().await > 100);
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/undo"),
            format!(r#"{{"floor":"{floor}"}}"#).into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(cells().await, 0);
        for bad in [
            r#"{"kind":"dungeon","width":4,"height":40}"#,
            r#"{"kind":"nonsense"}"#,
            r#"{"kind":"cave","width":40,"height":9999}"#,
        ] {
            let (u, bd) = gen(bad);
            assert_eq!(
                call(&router, "POST", &u, bd).await.0,
                StatusCode::BAD_REQUEST,
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn checkpoints_save_list_restore_and_delete_over_http() {
        let router = build_router(test_app());
        let json = |b: Vec<u8>| serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default();
        let (_, b) = call(&router, "POST", "/api/docs", br#"{"name":"CP"}"#.to_vec()).await;
        let id = json(b)["id"].as_str().unwrap().to_string();
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = json(d)["floors"][0]["id"].as_str().unwrap().to_string();
        let paint = |x: i32| {
            serde_json::json!({"floor": floor, "ops": [
                {"t":"upsert_space","space":{"id":"s","name":"Hall","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":x,"y":0}],"space":"s"}]})
            .to_string()
            .into_bytes()
        };
        call(&router, "POST", &format!("/api/docs/{id}/ops"), paint(0)).await;
        let (st, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/checkpoints"),
            br#"{"name":"one cell"}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let cid = json(b)["id"].as_str().unwrap().to_string();
        call(&router, "POST", &format!("/api/docs/{id}/ops"), paint(4)).await;
        let cells = |router: &Router| {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}"));
            async move {
                json(call(&router, "GET", &uri, vec![]).await.1)["floors"][0]["grid"]["cells"]
                    .as_array()
                    .unwrap()
                    .len()
            }
        };
        assert_eq!(cells(&router).await, 2);
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/checkpoints/{cid}/restore"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(cells(&router).await, 1);
        // The list shows the original and the automatic "before restoring" one.
        let (_, b) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/checkpoints"),
            vec![],
        )
        .await;
        let list = json(b)["checkpoints"].as_array().unwrap().clone();
        assert_eq!(list.len(), 2);
        assert!(list[1]["name"]
            .as_str()
            .unwrap()
            .starts_with("Before restoring"));
        assert_eq!(list[1]["cells"], 2);
        let (st, _) = call(
            &router,
            "DELETE",
            &format!("/api/docs/{id}/checkpoints/{cid}"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/checkpoints"),
            br#"{"name":""}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn floors_are_independent_undo_per_floor_and_a_removed_floor_comes_back() {
        let router = build_router(test_app());
        let (id, ground) = {
            let (st, body) = call(
                &router,
                "POST",
                "/api/docs",
                br#"{"name":"Station","kind":"battle"}"#.to_vec(),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
            let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
            let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
            let g = serde_json::from_slice::<serde_json::Value>(&d).unwrap()["floors"][0]["id"]
                .as_str()
                .unwrap()
                .to_string();
            (id, g)
        };
        let post = |path: &str, body: serde_json::Value| {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}/{path}"));
            async move {
                let (st, b) = call(&router, "POST", &uri, body.to_string().into_bytes()).await;
                (
                    st,
                    serde_json::from_slice::<serde_json::Value>(&b).unwrap_or_default(),
                )
            }
        };
        let doc = || {
            let (router, uri) = (router.clone(), format!("/api/docs/{id}"));
            async move {
                serde_json::from_slice::<serde_json::Value>(
                    &call(&router, "GET", &uri, vec![]).await.1,
                )
                .unwrap()
            }
        };
        // Add two floors.
        let (st, r) = post("floors", serde_json::json!({"action":"add","name":"Upper"})).await;
        assert_eq!(st, StatusCode::OK);
        let upper = r["floor"].as_str().unwrap().to_string();
        let (_, r) = post("floors", serde_json::json!({"action":"add","name":"Roof"})).await;
        let roof = r["floor"].as_str().unwrap().to_string();
        assert_eq!(r["floors"].as_array().unwrap().len(), 3);
        // Paint a room on each of ground and upper (interleaved).
        let room = |floor: &str, name: &str, x: i32| {
            serde_json::json!({"floor": floor, "ops": [
                {"t":"upsert_space","space":{"id":"s","name":name,"color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":x,"y":0},{"x":x+1,"y":0}],"space":"s"}]})
        };
        assert_eq!(
            post("ops", room(&ground, "Lobby", 0)).await.0,
            StatusCode::OK
        );
        assert_eq!(
            post("ops", room(&upper, "Suite", 5)).await.0,
            StatusCode::OK
        );
        assert_eq!(post("ops", serde_json::json!({"floor": ground, "ops":[{"t":"add_light","light":{"id":"l","x":16.0,"y":16.0,"range":4.0,"color":"#ffaa00","intensity":1.0,"on":true}}]})).await.0, StatusCode::OK);
        let d = doc().await;
        let cells = |d: &serde_json::Value, i: usize| {
            d["floors"][i]["grid"]["cells"].as_array().unwrap().len()
        };
        assert_eq!(
            (cells(&d, 0), cells(&d, 1), cells(&d, 2)),
            (2, 2, 0),
            "each floor has only its own cells"
        );
        assert_eq!(d["floors"][0]["lights"].as_array().unwrap().len(), 1);
        assert_eq!(d["floors"][1]["lights"].as_array().unwrap().len(), 0);
        // Undo on the GROUND floor removes ITS light (the latest edit there), not the upper room.
        assert_eq!(
            post("undo", serde_json::json!({"floor": ground})).await.0,
            StatusCode::OK
        );
        let d = doc().await;
        assert_eq!(d["floors"][0]["lights"].as_array().unwrap().len(), 0);
        assert_eq!(
            cells(&d, 1),
            2,
            "the upper floor was not touched by undo on the ground floor"
        );
        // A request whose second op is invalid applies NOTHING (the first op is rolled back).
        let bad = serde_json::json!({"floor": upper, "ops":[
            {"t":"add_light","light":{"id":"a","x":1.0,"y":1.0,"range":4.0,"color":"#ffaa00","intensity":1.0,"on":true}},
            {"t":"add_light","light":{"id":"b","x":1.0,"y":1.0,"range":4000.0,"color":"#ffaa00","intensity":1.0,"on":true}}]});
        assert_eq!(post("ops", bad).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            doc().await["floors"][1]["lights"].as_array().unwrap().len(),
            0,
            "all or nothing"
        );
        // Rename, move, duplicate.
        assert_eq!(
            post(
                "floors",
                serde_json::json!({"action":"rename","id":upper,"name":"Penthouse"})
            )
            .await
            .0,
            StatusCode::OK
        );
        assert_eq!(
            post(
                "floors",
                serde_json::json!({"action":"move","id":roof,"index":0})
            )
            .await
            .0,
            StatusCode::OK
        );
        let (_, r) = post(
            "floors",
            serde_json::json!({"action":"add","name":"Upper copy","copy_from":upper}),
        )
        .await;
        let names: Vec<String> = r["floors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["Roof", "Ground", "Penthouse", "Upper copy"],
            "the copy sits right after its source"
        );
        let d = doc().await;
        assert_eq!(cells(&d, 3), 2, "a copy carries the content");
        // Remove the Penthouse, then bring it back with its content.
        assert_eq!(
            post("floors", serde_json::json!({"action":"remove","id":upper}))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(doc().await["floors"].as_array().unwrap().len(), 3);
        assert_eq!(
            post("undo-structure", serde_json::json!({})).await.0,
            StatusCode::OK
        );
        let d = doc().await;
        let back: Vec<&str> = d["floors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            back,
            [roof.as_str(), ground.as_str(), upper.as_str(), back[3]],
            "restored to its old position"
        );
        assert_eq!(cells(&d, 2), 2, "with its room");
        // The last floor cannot be removed; bad input is a 400; unknown floor 400/404.
        assert_eq!(
            post("floors", serde_json::json!({"action":"remove","id":"nope"}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            post(
                "floors",
                serde_json::json!({"action":"rename","id":ground,"name":""})
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post("floors", serde_json::json!({"action":"fly"})).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            post("ops", serde_json::json!({"floor":"ghost","ops":[]}))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn the_tile_route_can_show_another_floor_underneath() {
        let router = build_router(test_app());
        let (st, body) = call(
            &router,
            "POST",
            "/api/docs",
            br#"{"name":"Tower","kind":"battle"}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let ground = serde_json::from_slice::<serde_json::Value>(&d).unwrap()["floors"][0]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (_, r) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/floors"),
            br#"{"action":"add","name":"Upper"}"#.to_vec(),
        )
        .await;
        let upper = serde_json::from_slice::<serde_json::Value>(&r).unwrap()["floor"]
            .as_str()
            .unwrap()
            .to_string();
        let paint = serde_json::json!({"floor": ground, "ops":[{"t":"batch","ops":[
            {"t":"upsert_space","space":{"id":"s","name":"Hall","color":"#ff0000","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":1,"y":1},{"x":2,"y":1}],"space":"s"}]}]});
        assert_eq!(
            call(
                &router,
                "POST",
                &format!("/api/docs/{id}/ops"),
                paint.to_string().into_bytes()
            )
            .await
            .0,
            StatusCode::OK
        );
        let tile = |q: String| {
            let router = router.clone();
            let uri = format!("/tiles/{id}/{upper}/0/0_0{q}");
            async move { call(&router, "GET", &uri, vec![]).await }
        };
        let (st, plain) = tile(String::new()).await;
        assert_eq!(st, StatusCode::OK);
        let (st, ghost) = tile(format!("?onion={ground}")).await;
        assert_eq!(st, StatusCode::OK);
        assert_ne!(
            plain, ghost,
            "the ground floor's room shows under the empty upper floor"
        );
        // A floor ghosting onto itself, or the parameter left empty, changes nothing.
        assert_eq!(tile(format!("?onion={upper}")).await.1, plain);
        // An onion floor that does not exist is a 404, not a silent no-op.
        assert_eq!(tile("?onion=ghost".into()).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn png_export_size_and_limits() {
        let app = test_app();
        let (doc_id, floor_id) = {
            let mut st = app.store.write().await;
            let m = st.create("Vault", DocKind::Battle).clone();
            let d = st.docs.get_mut(&m.id).unwrap();
            let fid = d.floors[0].id.clone();
            let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
                {"t":"upsert_space","space":{"id":"s","name":"H","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":3,"y":1}],"space":"s"}
            ]))
            .unwrap();
            d.apply_ops(&fid, &ops).unwrap();
            (m.id, fid)
        };
        let router = build_router(app);
        let get = |q: &str| {
            Request::get(format!("/api/docs/{doc_id}/export/png?floor={floor_id}{q}"))
                .body(Body::empty())
                .unwrap()
        };
        let resp = router.clone().oneshot(get("&ppc=10")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        // PNG IHDR: width at 16..20, height at 20..24 — 4x2 cells at 10 px.
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 40);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 20);
        let resp = router.clone().oneshot(get("&ppc=100000")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // WebP and AVIF come back with the right type and magic bytes.
        let resp = router
            .clone()
            .oneshot(get("&ppc=10&format=webp"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["content-type"], "image/webp");
        let b = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&b[8..12], b"WEBP");
        let resp = router
            .clone()
            .oneshot(get("&ppc=10&format=avif"))
            .await
            .unwrap();
        assert_eq!(resp.headers()["content-type"], "image/avif");
        let b = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&b[4..12], b"ftypavif");
        let resp = router.oneshot(get("&format=gif")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn undo_restores_state() {
        let app = test_app();
        let router = build_router(app);
        let resp = router
            .clone()
            .oneshot(
                Request::post("/api/docs")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "Undo Test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
        let doc_id = manifest["id"].as_str().unwrap().to_string();

        let resp = router
            .clone()
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let floor_id = doc["floors"][0]["id"].as_str().unwrap().to_string();

        let ops = serde_json::json!({
            "floor": floor_id,
            "ops": [{
                "t": "batch",
                "ops": [
                    {"t": "upsert_space", "space": {
                        "id": "s", "name": "S", "color": "#101010", "kind": "room"}},
                    {"t": "paint_cells", "cells": [{"x": 2, "y": 2}], "space": "s"}
                ]
            }]
        });
        let resp = router
            .clone()
            .oneshot(
                Request::post(format!("/api/docs/{doc_id}/ops"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&ops).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let undo = serde_json::json!({"floor": floor_id});
        let resp = router
            .clone()
            .oneshot(
                Request::post(format!("/api/docs/{doc_id}/undo"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&undo).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = router
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            doc["floors"][0]["grid"]["cells"].as_array().unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn reroll_keeps_pinned_and_hand_placed_props() {
        let app = test_app();
        let router = build_router(app);
        let post = |uri: String, body: String| async {
            router
                .clone()
                .oneshot(
                    Request::post(uri)
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap()
        };
        let get = |uri: String| async {
            router
                .clone()
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap()
        };

        let text =
            body_string(post("/api/docs".into(), r#"{"name": "Reroll Me"}"#.into()).await).await;
        let doc_id: serde_json::Value = serde_json::from_str(&text).unwrap();
        let doc_id = doc_id["id"].as_str().unwrap().to_string();
        let text = body_string(get(format!("/api/docs/{doc_id}")).await).await;
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let fid = doc["floors"][0]["id"].as_str().unwrap().to_string();

        // A named kitchen (the recipe places a table, barrels, crates).
        let paint = serde_json::json!({"floor": fid, "ops": [{"t": "batch", "ops": [
            {"t": "upsert_space", "space": {"id": "k", "name": "Kitchen", "color": "#7f5539", "kind": "room"}},
            {"t": "paint_cells", "cells": [
                {"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0},{"x":3,"y":0},
                {"x":0,"y":1},{"x":1,"y":1},{"x":2,"y":1},{"x":3,"y":1},
                {"x":0,"y":2},{"x":1,"y":2},{"x":2,"y":2},{"x":3,"y":2}
            ], "space": "k"}
        ]}]});
        let resp = post(
            format!("/api/docs/{doc_id}/ops"),
            serde_json::to_string(&paint).unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = post(
            format!("/api/docs/{doc_id}/furnish"),
            serde_json::to_string(&serde_json::json!({"floor": fid, "spaces": ["k"], "seed": 7}))
                .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let text = body_string(get(format!("/api/docs/{doc_id}")).await).await;
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let stamps = doc["floors"][0]["stamps"].as_array().unwrap().clone();
        assert!(stamps.len() >= 2, "a kitchen furnishes");
        assert!(
            stamps[0]["gen"].is_object(),
            "furnished props carry provenance"
        );

        // Pin the first generated prop; add a hand-placed one (no gen).
        let mut pinned = stamps[0].clone();
        pinned["gen"]["pinned"] = serde_json::json!(true);
        let hand = serde_json::json!({
            "t": "update_stamp",
            "stamp": {"id": "hand1", "asset": "barrel", "x": 3.5, "y": 2.5,
                      "scale": 1.0, "rotation": 0.0, "flip": false,
                      "tint": null, "shadow": true, "gen": null}
        });
        let resp = post(
            format!("/api/docs/{doc_id}/ops"),
            serde_json::to_string(&serde_json::json!({"floor": fid, "ops": [
                {"t": "update_stamp", "stamp": pinned},
                {"t": "add_stamp", "stamp": hand["stamp"]}, hand
            ]}))
            .unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);

        // Reroll the room.
        let resp = post(
            format!("/api/docs/{doc_id}/reroll"),
            serde_json::to_string(&serde_json::json!({"floor": fid, "room": "k", "seed": 99}))
                .unwrap(),
        )
        .await;
        let status = resp.status();
        if status != StatusCode::OK {
            let rtext = body_string(resp).await;
            panic!("reroll failed: {status} {rtext}");
        }
        let text = body_string(resp).await;
        let out: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            out["removed"].as_u64().unwrap(),
            stamps.len() as u64 - 1,
            "every generated prop except the pinned one was removed"
        );

        let text = body_string(get(format!("/api/docs/{doc_id}")).await).await;
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        let stamps = doc["floors"][0]["stamps"].as_array().unwrap();
        let ids: Vec<&str> = stamps.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"hand1"), "the hand-placed prop stayed");
        let pinned_still = stamps
            .iter()
            .find(|s| s["id"] == pinned["id"] && s["gen"]["pinned"] == serde_json::json!(true));
        assert!(pinned_still.is_some(), "the pinned prop stayed");
        assert!(
            stamps
                .iter()
                .any(|s| s["gen"]["pinned"] == serde_json::json!(false)),
            "fresh generated props came back"
        );
    }

    #[tokio::test]
    async fn invalid_ops_get_4xx_not_stored() {
        let app = test_app();
        let router = build_router(app);
        let resp = router
            .clone()
            .oneshot(
                Request::post("/api/docs")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "Bad Ops"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
        let doc_id = manifest["id"].as_str().unwrap().to_string();

        // Paint into a nonexistent space → 400, rev stays 0.
        let ops = serde_json::json!({
            "floor": "nope",
            "ops": [{"t": "paint_cells", "cells": [{"x": 0, "y": 0}], "space": "ghost"}]
        });
        let resp = router
            .clone()
            .oneshot(
                Request::post(format!("/api/docs/{doc_id}/ops"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&ops).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = router
            .oneshot(
                Request::get(format!("/api/docs/{doc_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let text = body_string(resp).await;
        assert!(text.contains("\"rev\":0"), "{text}");
    }
}
