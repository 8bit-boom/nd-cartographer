//! `ndcart` — the nd-cartographer server binary. One binary, three ways to
//! run it (plan §4): `ndcart serve` (desktop/docker), `ndcart bench`.
//! Desktop is the Tauri shell wrapping this same server (M0 ships serve).

mod assets;
mod auth;
mod convert;
#[cfg(feature = "bridge")]
mod publish;
mod store;
mod text;
mod tiles;

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
    Json(serde_json::json!({
        "pack": "starter",
        "materials": ndcart_paint::starter().iter().map(|m| serde_json::json!({
            "id": m.id,
            "name": m.name,
            "color": format!("#{:02x}{:02x}{:02x}", m.base[0], m.base[1], m.base[2]),
        })).collect::<Vec<_>>(),
    }))
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

/// `POST /api/docs/{id}/generate-map` `{floor?, kind, seed, width, height}` — a
/// dungeon, cave, building, deck or city district drawn onto the floor as ONE
/// undoable step (it adds to what is there; the same seed gives the same map).
async fn generate_map(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<GenMapBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let ops = ndcart_core::gen::generate(&ndcart_core::gen::GenParams {
        kind: b.kind,
        seed: b.seed,
        width: b.width,
        height: b.height,
    })?;
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
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
    Json(serde_json::json!({
        "pack": "starter",
        "stamps": ndcart_paint::stamps().iter().map(|s| serde_json::json!({
            "id": s.id, "name": s.name, "size": s.size,
        })).collect::<Vec<_>>(),
    }))
}

/// `GET /api/stamps/{id}/icon.png` — a 64 px icon for the palette.
async fn stamp_icon_png(Path(id): Path<String>) -> Result<Response, ApiError> {
    let def =
        ndcart_paint::find_stamp(&id).ok_or_else(|| CoreError::NotFound(format!("stamp {id}")))?;
    let png = tiles::stamp_icon(def, 64);
    Ok((
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        png,
    )
        .into_response())
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
    Ok(Json(serde_json::json!({
        "floor": floor.id,
        "blocks_publish": ndcart_core::blocks_publish(&problems),
        "problems": problems,
    })))
}

/// `GET /api/docs/{id}/export/png[?floor=ID&ppc=N]` — the whole floor as one PNG.
async fn export_png(
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
    let fmt = pick_format(&q)?;
    let (bytes, ..) = tiles::bake_full_as(
        floor,
        &walls,
        cell_size,
        q.ppc.unwrap_or(32),
        fmt,
        q.quality.unwrap_or(80),
    )
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
    store::save_doc(app.data_dir.as_deref(), doc);
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
    store::save_doc(app.data_dir.as_deref(), doc);
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
    let key = format!(
        "{doc_id}/{floor_id}/{z}/{x}_{y}/{}",
        onion_id.as_deref().unwrap_or("-")
    );
    if let Some(bytes) = app.tile_cache.lock().expect("tile cache").get(&key) {
        return Ok(png_response(bytes));
    }
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&doc_id)
        .ok_or_else(|| CoreError::NotFound(format!("document {doc_id}")))?;
    let idx = floors_by_id(&doc.floors, &floor_id)?;
    let cell_size = doc.manifest.cell_size;
    let rev = doc.rev;
    let floor = doc.floors[idx].clone();
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
    let bytes = tokio::task::spawn_blocking(move || {
        let onion = ghost.as_ref().map(|(f, w)| (f, w.as_slice()));
        tiles::bake_tile_onion(&floor, &walls, cell_size, z, x, y, onion)
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
        .route("/api/materials", get(list_materials))
        .route("/api/assets", post(put_asset))
        .route("/api/assets/{id}", get(get_asset))
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

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "serve".into());
    match cmd.as_str() {
        "bench" => run_bench(args.collect()),
        "convert" => run_convert(args.collect()),
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
                "usage: ndcart [serve --listen ADDR --data-dir DIR | bench | convert IN OUT | publish DOC ...]"
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
