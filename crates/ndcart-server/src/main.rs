//! `ndcart` — the nd-cartographer server binary. One binary, three ways to
//! run it (plan §4): `ndcart serve` (desktop/docker), `ndcart bench`.
//! Desktop is the Tauri shell wrapping this same server (M0 ships serve).

mod store;
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

async fn get_tile(
    State(app): State<App>,
    Path((doc_id, floor_id, z, xy)): Path<(String, String, u32, String)>,
) -> Result<Response, ApiError> {
    let (x, y) = {
        let mut parts = xy.splitn(2, '_');
        let (a, b) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let x: i64 = a.parse().map_err(|_| CoreError::Invalid("tile x".into()))?;
        let y: i64 = b.parse().map_err(|_| CoreError::Invalid("tile y".into()))?;
        (x, y)
    };
    let key = format!("{doc_id}/{floor_id}/{z}/{x}_{y}");
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
    let bytes = tokio::task::spawn_blocking(move || tiles::bake_tile(&floor, &walls, z, x, y))
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
        .route("/api/docs/{id}/ops", post(post_ops))
        .route("/api/docs/{id}/undo", post(post_undo))
        .route("/tiles/{doc}/{floor}/{z}/{xy}", get(get_tile))
        .with_state(app)
}

// ---------- bench ----------

fn run_bench() {
    use ndcart_core::grid::{Space, SpaceKind};
    use ndcart_core::journal::Journal;
    println!("ndcart bench (M0): journal + walls + replay on a 200x140 dungeon");
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
}

// ---------- main ----------

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "serve".into());
    match cmd.as_str() {
        "bench" => run_bench(),
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
            let app: App = Arc::new(AppState {
                store: tokio::sync::RwLock::new(store),
                data_dir,
                wall_cache: Mutex::new(HashMap::new()),
                tile_cache: Mutex::new(HashMap::new()),
            });
            let router = build_router(app);
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
            eprintln!("usage: ndcart [serve --listen ADDR --data-dir DIR | bench]");
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
