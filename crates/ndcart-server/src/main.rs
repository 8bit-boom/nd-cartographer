//! `ndcart` — the nd-cartographer server binary. One binary, three ways to
//! run it (plan §4): `ndcart serve` (desktop/docker), `ndcart bench`.
//! Desktop is the Tauri shell wrapping this same server (M0 ships serve).

mod ai_settings;
mod aijobs;
mod ask;
mod assets;
mod auth;
mod bench;
mod bundle;
mod cache;
mod convert;
mod doctor;
#[cfg(feature = "bridge")]
mod draft;
mod golden;
mod mcp;
mod packs;
mod pdf;
#[cfg(feature = "bridge")]
mod publish;
mod routes;
mod state;
mod store;
mod synth;
mod text;
mod tiles;
mod worldspec;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::sse::Sse;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ndcart_core::doc::{floors_by_id, DocKind, FloorDoc};
use ndcart_core::grid::RoomGrid;
use ndcart_core::journal::Op;
use ndcart_core::walls::{tally, Wall};
use ndcart_core::Audience;
use ndcart_core::{CoreError, Label, Light, Manifest};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use routes::ai::*;
use routes::assets::*;
use routes::docs::*;
use routes::edit::*;
use routes::export::*;
use routes::import::*;
use routes::proposals::*;
use routes::system::*;
use routes::table::*;
use routes::tiles::*;
use state::*;

const INDEX_HTML: &str = include_str!("../../../ui/index.html");
const APP_JS: &str = include_str!("../../../ui/app.js");
const APP_CSS: &str = include_str!("../../../ui/app.css");

// ---------- API error mapping ----------

// ---------- views ----------

// ---------- handlers ----------

const TABLE_HTML: &str = include_str!("../../../ui/table.html");

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
        .route("/api/docs/{id}/history", get(doc_history))
        .route("/api/docs/{id}/revert", post(revert_to))
        .route("/api/docs/{id}/export.svg", get(export_svg))
        .route("/api/docs/{id}/export/roll20", get(export_roll20))
        .route("/api/docs/{id}/events", get(doc_events))
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
        .route("/api/docs/{id}/bundle", get(get_bundle))
        .route(
            "/api/import/bundle",
            post(import_bundle).layer(axum::extract::DefaultBodyLimit::max(
                MAX_BUNDLE_BYTES + (1 << 20),
            )),
        )
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
        .route(
            "/api/ai-settings",
            get(get_ai_settings).post(post_ai_settings),
        )
        .route("/api/ai-models", get(ai_models))
        .route("/api/ai-test", post(ai_test))
        .route("/api/docs/{id}/theme-pack", post(theme_pack))
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
        .route("/api/world/tables", get(world_tables))
        .route("/api/docs/{id}/stock-room", post(stock_room))
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
        .route("/api/docs/{id}/redo", post(post_redo))
        .route("/tiles/{doc}/{floor}/{z}/{xy}", get(get_tile))
        .with_state(app)
        .layer(axum::middleware::from_fn(auth::same_origin_guard))
        .layer(axum::middleware::from_fn(auth::security_headers))
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
            tint: None,
        };
        store::apply_ops_direct(&mut floor, &mut journal, &[Op::UpsertSpace { space }]).unwrap();
    }
    // One single-cell op per square (worst-case journal granularity).
    let cols = 200usize;
    let rows = 140usize;
    println!(
        "ndcart bench: journal + walls + replay + full-map bake on a {cols}x{rows} dungeon ({} cells)",
        cols * rows
    );
    let mut op_count = 0usize;
    for y in 0..rows {
        for x in 0..cols {
            let op = Op::PaintCells {
                cells: vec![ndcart_core::CellKey::new(x as i32, y as i32)],
                space: Some(format!("sp{}", (x + y) % palette.len())),
            };
            store::apply_ops_direct(&mut floor, &mut journal, &[op]).unwrap();
            op_count += 1;
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
        "bench" => {
            let rest: Vec<String> = args.collect();
            if rest.iter().any(|a| a == "--large") {
                bench::run_large(rest.iter().any(|a| a == "--json")).await;
            } else {
                run_bench(rest);
            }
        }
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
        "bundle" => {
            if let Err(e) = bundle::run_bundle(args.collect()).await {
                eprintln!("ndcart: {e}");
                std::process::exit(1);
            }
        }
        "mcp" => {
            let mut data_dir = None;
            let mut rest = args.collect::<Vec<_>>().into_iter();
            while let Some(a) = rest.next() {
                if a == "--data-dir" {
                    data_dir = rest.next().map(PathBuf::from);
                }
            }
            if let Err(e) = mcp::run(data_dir) {
                eprintln!("ndcart: {e}");
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
            let ai = ai_settings::load(data_dir.as_deref());
            let app: App = Arc::new(AppState {
                store: tokio::sync::RwLock::new(store),
                data_dir,
                ai: Mutex::new(ai),
                wall_cache: Mutex::new(HashMap::new()),
                tile_cache: Mutex::new(cache::Lru::new(TILE_CACHE_BYTES)),
                heavy: tokio::sync::Semaphore::new(heavy_permits()),
                table_tokens: Default::default(),
            });
            let table_tokens = app.table_tokens.clone();
            let mut router = build_router(app);
            if auth::is_loopback(&listen) {
                let allowed = Arc::new(auth::allowed_hosts());
                router = router.layer(axum::middleware::from_fn(move |req, next| {
                    auth::host_guard(allowed.clone(), req, next)
                }));
            } else {
                let (token, fresh) = auth::resolve_token(app_data_dir.as_deref());
                if fresh {
                    println!("ndcart: access token (shown once): {token}");
                }
                println!("ndcart: not loopback, token required (?token=… or Bearer)");
                router = router.layer(axum::middleware::from_fn(move |req, next| {
                    auth::gate(token.clone(), table_tokens.clone(), req, next)
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
            ai: Mutex::new(ai_settings::load(None)),
            wall_cache: Mutex::new(HashMap::new()),
            tile_cache: Mutex::new(cache::Lru::new(TILE_CACHE_BYTES)),
            heavy: tokio::sync::Semaphore::new(heavy_permits()),
            table_tokens: Default::default(),
        })
    }

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn token_gate_blocks_without_token() {
        let router = build_router(test_app()).layer(axum::middleware::from_fn(|req, next| {
            auth::gate("s3cret".to_string(), Default::default(), req, next)
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
    async fn a_world_is_generated_on_the_chosen_floor() {
        let router = build_router(test_app());
        let (id, first) = region_doc(&router).await;
        let (_, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/floors"),
            serde_json::json!({"action":"add","name":"Second"})
                .to_string()
                .into_bytes(),
        )
        .await;
        let second = jv(&b)["floor"].as_str().unwrap().to_string();
        let gen =
            serde_json::json!({"floor": second, "seed": 7, "cols": 24, "rows": 20, "rivers": 2});
        let (st, body) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/generate"),
            gen.to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(jv(&body)["floor"], serde_json::json!(second));
        let (_, doc) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let v = jv(&doc);
        let floor = |fid: &str| {
            v["floors"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["id"] == fid)
                .unwrap()
                .clone()
        };
        assert!(
            floor(&first)["terrain"].is_null(),
            "the first floor is untouched"
        );
        assert!(floor(&first)["canvas"].is_null());
        assert!(floor(&second)["terrain"]["chunks"].as_u64().unwrap() > 0);
        assert_eq!(floor(&second)["canvas"], serde_json::json!([24, 20]));
        // An unknown floor is refused; no floor means the first.
        let bad = serde_json::json!({"floor": "nope", "cols": 16, "rows": 16});
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/generate"),
            bad.to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, body) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/generate"),
            br#"{"cols":16,"rows":16}"#.to_vec(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(jv(&body)["floor"], serde_json::json!(first));
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
    async fn ai_settings_save_read_redact_and_take_effect() {
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

        // Initially: no direct model.
        let text = body_string(get("/api/ai-settings".into()).await).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["settings"]["direct_url"], serde_json::Value::Null);
        assert_eq!(v["settings"]["key_set"], serde_json::json!(false));

        // Save an Unsloth-style config; the key is never echoed back.
        let save = serde_json::json!({
            "direct_url": "http://127.0.0.1:3915/v1",
            "direct_model": "unsloth/Qwen3-4B-GGUF",
            "key": "sk-local-1"
        });
        let resp = post(
            "/api/ai-settings".into(),
            serde_json::to_string(&save).unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let text = body_string(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["settings"]["direct_url"],
            serde_json::json!("http://127.0.0.1:3915/v1")
        );
        assert!(text.contains("\"key_set\":true"));
        assert!(
            !text.contains("sk-local-1"),
            "the key never leaves the server"
        );

        // The status banner flips to standalone immediately (a bridge build;
        // without the bridge there is no AI client and the banner stays off).
        let text = body_string(get("/api/ai/status".into()).await).await;
        if cfg!(feature = "bridge") {
            assert!(text.contains("standalone"), "{text}");
        } else {
            assert!(text.contains("no AI client"), "{text}");
        }

        // A non-http URL is refused with the reason.
        let resp = post(
            "/api/ai-settings".into(),
            serde_json::to_string(&serde_json::json!({"direct_url": "ftp://nope"})).unwrap(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Posting the same address again (no key) keeps the key; a different
        // address without a key drops it and says so.
        let same = serde_json::json!({"direct_url": "http://127.0.0.1:3915/v1"});
        let v: serde_json::Value = serde_json::from_str(
            &body_string(post("/api/ai-settings".into(), same.to_string()).await).await,
        )
        .unwrap();
        assert_eq!(v["settings"]["key_set"], serde_json::json!(true));
        assert_eq!(v["key_cleared"], serde_json::json!(false));
        let other = serde_json::json!({"direct_url": "https://attacker.example/v1"});
        let v: serde_json::Value = serde_json::from_str(
            &body_string(post("/api/ai-settings".into(), other.to_string()).await).await,
        )
        .unwrap();
        assert_eq!(v["settings"]["key_set"], serde_json::json!(false));
        assert_eq!(v["key_cleared"], serde_json::json!(true));
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

    // ---- batch 1 helpers and tests ----

    fn jv(b: &[u8]) -> serde_json::Value {
        serde_json::from_slice(b).unwrap_or_default()
    }

    async fn new_doc(router: &Router, name: &str) -> (String, String) {
        let (_, b) = call(
            router,
            "POST",
            "/api/docs",
            format!(r#"{{"name":"{name}"}}"#).into_bytes(),
        )
        .await;
        let id = jv(&b)["id"].as_str().unwrap().to_string();
        let (_, d) = call(router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let floor = jv(&d)["floors"][0]["id"].as_str().unwrap().to_string();
        (id, floor)
    }

    async fn post_ops_json(router: &Router, id: &str, v: serde_json::Value) -> StatusCode {
        call(
            router,
            "POST",
            &format!("/api/docs/{id}/ops"),
            v.to_string().into_bytes(),
        )
        .await
        .0
    }

    async fn floor_digest(router: &Router, id: &str) -> String {
        let (_, d) = call(router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        jv(&d)["floors"][0]["digest"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn undoing_a_revert_brings_back_the_pre_revert_floor() {
        let router = build_router(test_app());
        let (id, f) = new_doc(&router, "R").await;
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":[{"x":1,"y":1},{"x":2,"y":1}]}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let before = floor_digest(&router, &id).await;
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/revert"),
            serde_json::json!({"floor": f, "index": 0})
                .to_string()
                .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_ne!(floor_digest(&router, &id).await, before);
        let (st, body) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/undo"),
            serde_json::json!({"floor": f}).to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(floor_digest(&router, &id).await, before);
    }

    /// Every tile of a 6x6 block at z0 and the middle 6x6 at z1, in a fixed order.
    async fn tile_block(router: &Router, id: &str, f: &str) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (z, from, to) in [(0, 0, 6), (1, 2, 8)] {
            for y in from..to {
                for x in from..to {
                    let (st, b) = call(
                        router,
                        "GET",
                        &format!("/tiles/{id}/{f}/{z}/{x}_{y}"),
                        vec![],
                    )
                    .await;
                    assert_eq!(st, StatusCode::OK);
                    out.push(b);
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn an_edit_reports_its_dirty_rect_and_keeps_far_tiles_cached() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "D").await;
        let (st, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/ops"),
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":[{"x":0,"y":0},{"x":1,"y":0}]}]})
            .to_string()
            .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // Painting makes the space's own cells dirty (and nothing else).
        assert_eq!(jv(&b)["dirty"], serde_json::json!([-2, -2, 3, 2]));
        let far = format!("/tiles/{id}/{f}/0/0_0");
        let near = format!("/tiles/{id}/{f}/0/6_6");
        let (_, far_before) = call(&router, "GET", &far, vec![]).await;
        let (_, near_before) = call(&router, "GET", &near, vec![]).await;
        let (st, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/ops"),
            serde_json::json!({"floor": f, "ops": [
                {"t":"paint_cells","space":"sp_a","cells":[{"x":100,"y":100}]}]})
            .to_string()
            .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(jv(&b)["dirty"], serde_json::json!([98, 98, 102, 102]));
        let rev = jv(&b)["rev"].as_u64().unwrap();
        // The far tile was carried over to the new rev: a cache hit, same bytes.
        let key = format!("{id}/{f}/0/0_0/{rev}/-/-");
        assert!(
            app.tile_cache.lock().unwrap().get(&key).is_some(),
            "the far tile moved to the new rev instead of being dropped"
        );
        let near_key = format!("{id}/{f}/0/6_6/{rev}/-/-");
        assert!(app.tile_cache.lock().unwrap().get(&near_key).is_none());
        let (_, far_after) = call(&router, "GET", &far, vec![]).await;
        let (_, near_after) = call(&router, "GET", &near, vec![]).await;
        assert_eq!(far_before, far_after);
        assert_ne!(
            near_before, near_after,
            "the tile under the edit is redrawn"
        );
        // Undo reports the same area.
        let (st, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/undo"),
            serde_json::json!({"floor": f}).to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(jv(&b)["dirty"], serde_json::json!([98, 98, 102, 102]));
        // A light changes no tile pixel at all.
        let (_, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/ops"),
            serde_json::json!({"floor": f, "ops": [{"t":"add_light","light":{"id":"l","x":5.0,"y":5.0,"range":4.0,"color":"#ffaa00","intensity":0.8,"on":true}}]})
            .to_string()
            .into_bytes(),
        )
        .await;
        assert_eq!(jv(&b)["dirty"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn lite_doc_sends_one_floor_in_full() {
        let router = build_router(test_app());
        let (id, f) = new_doc(&router, "L").await;
        let mut ids = vec![f.clone()];
        for name in ["Up", "Roof"] {
            let (_, b) = call(
                &router,
                "POST",
                &format!("/api/docs/{id}/floors"),
                serde_json::json!({"action":"add","name":name})
                    .to_string()
                    .into_bytes(),
            )
            .await;
            ids.push(jv(&b)["floor"].as_str().unwrap().to_string());
        }
        let cells: Vec<_> = (0..40)
            .flat_map(|x| (0..40).map(move |y| serde_json::json!({"x":x,"y":y})))
            .collect();
        for fl in &ids {
            let st = post_ops_json(
                &router,
                &id,
                serde_json::json!({"floor": fl, "ops": [
                    {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                    {"t":"paint_cells","space":"sp_a","cells":cells}]}),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
        }
        let (_, full) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let (st, lite) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}?floor={}&lite=1", ids[1]),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (full, lite_v) = (jv(&full), jv(&lite));
        assert_eq!(lite_v["rev"], full["rev"]);
        assert_eq!(lite_v["floors"].as_array().unwrap().len(), 3);
        for (i, fl) in lite_v["floors"].as_array().unwrap().iter().enumerate() {
            assert_eq!(fl["id"], full["floors"][i]["id"]);
            assert_eq!(fl["name"], full["floors"][i]["name"]);
            assert_eq!(fl["digest"], full["floors"][i]["digest"]);
            if i == 1 {
                assert_eq!(fl, &full["floors"][1], "the chosen floor is complete");
            } else {
                assert!(fl.get("grid").is_none(), "floor {i} is a stub");
                assert!(fl.get("walls").is_none());
            }
        }
        assert!(lite.len() * 2 < full.to_string().len(), "much smaller");
        // An unknown floor is refused; lite needs one.
        let (st, _) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}?floor=nope&lite=1"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _) = call(&router, "GET", &format!("/api/docs/{id}?lite=1"), vec![]).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_edits_with_periodic_snapshots_reload_to_the_live_state() {
        let dir = std::env::temp_dir().join(format!("ndcart-snap-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = Arc::new(AppState {
            store: tokio::sync::RwLock::new(store::Store::default()),
            data_dir: Some(dir.clone()),
            ai: Mutex::new(ai_settings::load(None)),
            wall_cache: Mutex::new(HashMap::new()),
            tile_cache: Mutex::new(cache::Lru::new(TILE_CACHE_BYTES)),
            heavy: tokio::sync::Semaphore::new(heavy_permits()),
            table_tokens: Default::default(),
        });
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "Race").await;
        // 600 edits from 8 concurrent writers: several snapshots fall due.
        let mut tasks = Vec::new();
        for w in 0..8 {
            let (router, id, f) = (router.clone(), id.clone(), f.clone());
            tasks.push(tokio::spawn(async move {
                for i in 0..75 {
                    let st = post_ops_json(
                        &router,
                        &id,
                        serde_json::json!({"floor": f, "ops": [
                            {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                            {"t":"paint_cells","space":"sp_a","cells":[{"x": w * 100 + i, "y": 0}]}]}),
                    )
                    .await;
                    assert_eq!(st, StatusCode::OK);
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        // Let the last background snapshot finish, then "crash and restart".
        for _ in 0..200 {
            let busy = {
                let store = app.store.read().await;
                store.docs[&id]
                    .persisted
                    .snapshot_in_flight
                    .load(std::sync::atomic::Ordering::SeqCst)
            };
            if !busy {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let mut back = store::Store::default();
        back.load_dir(&dir).unwrap();
        let live = app.store.read().await;
        assert_eq!(back.docs[&id].floors, live.docs[&id].floors);
        assert_eq!(back.docs[&id].floors[0].grid.cell_count(), 600);
        assert_eq!(back.docs[&id].journal.len(), live.docs[&id].journal.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn redo_takes_back_an_undo_and_a_new_edit_forgets_it() {
        let router = build_router(test_app());
        let (id, f) = new_doc(&router, "Redo").await;
        let space = serde_json::json!({"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}});
        let paint =
            |x: i32| serde_json::json!({"t":"paint_cells","space":"sp_a","cells":[{"x":x,"y":0}]});
        let ops = |ops: Vec<serde_json::Value>| serde_json::json!({"floor": f, "ops": ops});
        assert_eq!(
            post_ops_json(&router, &id, ops(vec![space, paint(0)])).await,
            StatusCode::OK
        );
        let a = floor_digest(&router, &id).await;
        assert_eq!(
            post_ops_json(&router, &id, ops(vec![paint(1)])).await,
            StatusCode::OK
        );
        let ab = floor_digest(&router, &id).await;
        assert_ne!(a, ab);
        let post = |what: &'static str| {
            let (router, id, f) = (router.clone(), id.clone(), f.clone());
            async move {
                call(
                    &router,
                    "POST",
                    &format!("/api/docs/{id}/{what}"),
                    serde_json::json!({"floor": f}).to_string().into_bytes(),
                )
                .await
            }
        };
        // Nothing to redo yet.
        let (st, body) = post("redo").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(String::from_utf8_lossy(&body).contains("nothing to redo"));
        // Undo B, redo B.
        assert_eq!(post("undo").await.0, StatusCode::OK);
        assert_eq!(floor_digest(&router, &id).await, a);
        let (st, body) = post("redo").await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(jv(&body)["dirty"], serde_json::json!([-1, -2, 3, 2]));
        assert_eq!(floor_digest(&router, &id).await, ab);
        // The redo is itself undoable. undo, undo, redo, redo gets back to A+B.
        assert_eq!(post("undo").await.0, StatusCode::OK);
        assert_eq!(floor_digest(&router, &id).await, a);
        assert_eq!(post("undo").await.0, StatusCode::OK);
        assert_eq!(post("redo").await.0, StatusCode::OK);
        assert_eq!(floor_digest(&router, &id).await, a);
        assert_eq!(post("redo").await.0, StatusCode::OK);
        assert_eq!(floor_digest(&router, &id).await, ab);
        assert_eq!(post("redo").await.0, StatusCode::NOT_FOUND);
        // undo, then a new edit: the redo is gone.
        assert_eq!(post("undo").await.0, StatusCode::OK);
        assert_eq!(
            post_ops_json(&router, &id, ops(vec![paint(7)])).await,
            StatusCode::OK
        );
        let (st, body) = post("redo").await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        assert!(String::from_utf8_lossy(&body).contains("nothing to redo"));
    }

    fn tiny_png() -> Vec<u8> {
        let mut pm = tiny_skia::Pixmap::new(8, 8).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(200, 40, 40, 255));
        pm.encode_png().unwrap()
    }

    /// Rebuild a zip entry by entry: `f` may rename, change or drop each one.
    fn rezip(bytes: &[u8], f: impl Fn(&str, Vec<u8>) -> Option<(String, Vec<u8>)>) -> Vec<u8> {
        use std::io::{Read, Write};
        let mut r = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        for i in 0..r.len() {
            let mut e = r.by_index(i).unwrap();
            let name = e.name().to_string();
            let mut data = Vec::new();
            e.read_to_end(&mut data).unwrap();
            if let Some((n, d)) = f(&name, data) {
                w.start_file(n, opts).unwrap();
                w.write_all(&d).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    async fn doc_count(app: &App) -> usize {
        app.store.read().await.docs.len()
    }

    #[tokio::test]
    async fn a_backup_restores_as_a_new_project_with_everything_in_it() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "Backup me").await;
        // A custom prop with its own picture, placed on the floor.
        let a = assets::global();
        let picture = a.put(&tiny_png()).unwrap();
        a.register(assets::Custom {
            meta: Default::default(),
            id: "stp_backup00001".into(),
            name: "Red square".into(),
            kind: "stamp".into(),
            asset: picture.clone(),
            size: 1.0,
            origin: assets::Provenance {
                source: "ai".into(),
                prompt: String::new(),
                seed: 0,
                date: "2026-11-18".into(),
            },
            tags: Vec::new(),
        })
        .unwrap();
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":[{"x":0,"y":0},{"x":1,"y":0}]},
                {"t":"add_stamp","stamp":{"id":"p1","asset":"stp_backup00001","x":1.5,"y":1.5,"scale":1.0,
                    "rotation":0.0,"flip":false,"tint":null,"shadow":true,"gen":null}},
                {"t":"add_stamp","stamp":{"id":"p2","asset":"barrel","x":3.5,"y":1.5,"scale":1.0,
                    "rotation":0.0,"flip":false,"tint":null,"shadow":true,"gen":null}}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, zip_bytes) = call(
            &router,
            "GET",
            &format!("/api/docs/{id}/bundle?assets=1"),
            vec![],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // The bundle lists only the allowed names and carries the picture + its library entry.
        let names: Vec<String> = {
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(&zip_bytes[..])).unwrap();
            (0..z.len())
                .map(|i| z.by_index(i).unwrap().name().to_string())
                .collect()
        };
        assert!(
            names.contains(&format!("{id}/assets/{picture}")),
            "{names:?}"
        );
        assert!(names.contains(&format!("{id}/custom.json")));
        assert!(names.contains(&format!("{id}/PACKS.txt")));
        // Without assets=1 the picture stays out.
        let (_, light) = call(&router, "GET", &format!("/api/docs/{id}/bundle"), vec![]).await;
        assert!(!String::from_utf8_lossy(&light).contains(&picture));
        // Import it back.
        let (st, b) = call(&router, "POST", "/api/import/bundle", zip_bytes.clone()).await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
        let new_id = jv(&b)["id"].as_str().unwrap().to_string();
        assert_ne!(new_id, id, "a restored project always gets a new id");
        let (_, old) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let (st, new) = call(&router, "GET", &format!("/api/docs/{new_id}"), vec![]).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            jv(&old)["floors"],
            jv(&new)["floors"],
            "the floors are equal"
        );
        assert_eq!(jv(&new)["floors"][0]["stamps"].as_array().unwrap().len(), 2);
        assert_eq!(jv(&new)["manifest"]["name"], jv(&old)["manifest"]["name"]);
        assert_ne!(jv(&new)["manifest"]["id"], jv(&old)["manifest"]["id"]);
        // Undo history came along too.
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{new_id}/undo"),
            serde_json::json!({"floor": f}).to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // The same bundle again is another new project.
        let (st, b2) = call(&router, "POST", "/api/import/bundle", zip_bytes).await;
        assert_eq!(st, StatusCode::OK);
        assert_ne!(jv(&b2)["id"], jv(&b)["id"]);
    }

    #[tokio::test]
    async fn a_bad_bundle_is_refused_and_creates_nothing() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "Bad bundles").await;
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":[{"x":0,"y":0}]}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, good) = call(&router, "GET", &format!("/api/docs/{id}/bundle"), vec![]).await;
        let before = doc_count(&app).await;
        let refuse = |bytes: Vec<u8>| {
            let router = router.clone();
            async move {
                let (st, body) = call(&router, "POST", "/api/import/bundle", bytes).await;
                (st, String::from_utf8_lossy(&body).to_string())
            }
        };
        // Not a zip at all.
        assert_eq!(
            refuse(b"definitely not a zip".to_vec()).await.0,
            StatusCode::BAD_REQUEST
        );
        // A path that climbs out.
        let evil = rezip(&good, |n, d| {
            Some((
                if n.ends_with("manifest.json") {
                    "x/../../evil".to_string()
                } else {
                    n.to_string()
                },
                d,
            ))
        });
        let (st, msg) = refuse(evil).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{msg}");
        // An absolute path, and a name that is not on the list.
        let abs = rezip(&good, |n, d| {
            Some((
                if n.ends_with("PACKS.txt") {
                    "/etc/passwd".to_string()
                } else {
                    n.to_string()
                },
                d,
            ))
        });
        assert_eq!(refuse(abs).await.0, StatusCode::BAD_REQUEST);
        let extra = rezip(&good, |n, d| {
            Some((
                if n.ends_with("PACKS.txt") {
                    format!("{}/notes.txt", n.split('/').next().unwrap())
                } else {
                    n.to_string()
                },
                d,
            ))
        });
        assert_eq!(refuse(extra).await.0, StatusCode::BAD_REQUEST);
        // A corrupt journal.
        let corrupt = rezip(&good, |n, d| {
            Some((
                n.to_string(),
                if n.ends_with("journal.ndjson") {
                    b"{not json}\n".to_vec()
                } else {
                    d
                },
            ))
        });
        let (st, msg) = refuse(corrupt).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{msg}");
        // A corrupt snapshot, and a picture that is not what its name says.
        let broken_snapshot = rezip(&good, |n, d| {
            Some((
                n.to_string(),
                if n.ends_with("snapshot.json") {
                    b"{".to_vec()
                } else {
                    d
                },
            ))
        });
        assert_eq!(refuse(broken_snapshot).await.0, StatusCode::BAD_REQUEST);
        let fake_picture = rezip(&good, |n, d| Some((n.to_string(), d)));
        let fake_picture = {
            let top = id.clone();
            let name = format!("{top}/assets/{}", "ab".repeat(32));
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            let mut r = zip::ZipArchive::new(std::io::Cursor::new(&fake_picture[..])).unwrap();
            for i in 0..r.len() {
                let e = r.by_index(i).unwrap();
                w.raw_copy_file(e).unwrap();
            }
            w.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut w, &tiny_png()).unwrap();
            w.finish().unwrap().into_inner()
        };
        assert_eq!(refuse(fake_picture).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(doc_count(&app).await, before, "nothing was created");
        // And the good one still restores.
        assert_eq!(refuse(good).await.0, StatusCode::OK);
        assert_eq!(doc_count(&app).await, before + 1);
    }

    #[tokio::test]
    async fn dirty_is_null_when_text_is_laid_out_globally() {
        let router = build_router(test_app());
        let (id, f) = new_doc(&router, "D").await;
        let uri = format!("/api/docs/{id}/ops");
        let post = |ops: serde_json::Value| {
            call(
                &router,
                "POST",
                &uri,
                serde_json::json!({"floor": f, "ops": ops})
                    .to_string()
                    .into_bytes(),
            )
        };
        let (_, b) = post(serde_json::json!([
            {"t":"upsert_label","label":{"id":"n","x":2.0,"y":2.0,"text":"Hall","size":0.8,"color":"#222222","bold":false}}
        ]))
        .await;
        assert!(jv(&b)["dirty"].is_null(), "an upsert_label is global");
        let (_, b) = post(serde_json::json!([
            {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","space":"sp_a","cells":[{"x":3,"y":3}]}
        ]))
        .await;
        assert!(jv(&b)["dirty"].is_null(), "label collisions are global");
    }

    /// The strongest check on the dirty rectangles: after each kind of edit,
    /// every tile served from the carried-over cache equals a fresh bake.
    #[tokio::test]
    async fn carried_over_tiles_equal_fresh_bakes_after_every_edit_kind() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "D").await;
        let stamp = |id: &str, x: f64, y: f64, scale: f64| {
            serde_json::json!({"id":id,"asset":"barrel","x":x,"y":y,"scale":scale,
                "rotation":0.3,"flip":false,"tint":null,"shadow":true,"gen":null})
        };
        let stroke = serde_json::json!({"id":"st1","kind":"paint","brush":"round","color":"#336699",
            "size":2.0,"opacity":0.8,"hardness":0.5,"roughness":0.2,"density":0.5,"seed":7,
            "points":[{"x":20.0,"y":20.0,"pressure":1.0,"tilt":0.0},{"x":30.0,"y":24.0,"pressure":1.0,"tilt":0.0}]});
        let cells = |x0: i32, y0: i32, x1: i32, y1: i32| -> Vec<serde_json::Value> {
            (x0..=x1)
                .flat_map(|x| (y0..=y1).map(move |y| serde_json::json!({"x":x,"y":y})))
                .collect()
        };
        let steps: Vec<(&str, serde_json::Value)> = vec![
            (
                "rooms",
                serde_json::json!([
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"upsert_space","space":{"id":"sp_b","name":"B","color":"#5a7d9a","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":cells(2,2,12,10)},
                {"t":"paint_cells","space":"sp_b","cells":cells(13,2,20,10)}]),
            ),
            (
                "paint beyond",
                serde_json::json!([{"t":"paint_cells","space":"sp_b","cells":cells(40,30,44,33)}]),
            ),
            (
                "clear cells",
                serde_json::json!([{"t":"paint_cells","space":null,"cells":cells(5,5,6,6)}]),
            ),
            (
                "mark",
                serde_json::json!([{"t":"set_mark","edge":{"cell":{"x":12,"y":5},"edge":"east"},"mark":{"kind":"door","state":"closed"}}]),
            ),
            (
                "recolour",
                serde_json::json!([{"t":"upsert_space","space":{"id":"sp_b","name":"B","color":"#aa3355","kind":"room"}}]),
            ),
            (
                "stamp",
                serde_json::json!([{"t":"add_stamp","stamp":stamp("p1", 25.0, 15.0, 1.0)}]),
            ),
            (
                "big stamp",
                serde_json::json!([{"t":"add_stamp","stamp":stamp("p2", 60.0, 50.0, 4.0)}]),
            ),
            (
                "move stamp",
                serde_json::json!([{"t":"update_stamp","stamp":stamp("p1", 45.0, 38.0, 1.0)}]),
            ),
            (
                "stroke",
                serde_json::json!([{"t":"add_stroke","stroke":stroke}]),
            ),
            (
                "remove stroke",
                serde_json::json!([{"t":"remove_stroke","id":"st1"}]),
            ),
            (
                "remove stamp",
                serde_json::json!([{"t":"remove_stamp","id":"p2"}]),
            ),
        ];
        // Warm every tile of the block, then edit and compare after each step.
        let mut kinds = Vec::new();
        for (name, ops) in steps {
            let _ = tile_block(&router, &id, &f).await;
            let (st, b) = call(
                &router,
                "POST",
                &format!("/api/docs/{id}/ops"),
                serde_json::json!({"floor": f, "ops": ops})
                    .to_string()
                    .into_bytes(),
            )
            .await;
            assert_eq!(
                st,
                StatusCode::OK,
                "{name}: {}",
                String::from_utf8_lossy(&b)
            );
            kinds.push((name, jv(&b)["dirty"].clone()));
            let carried = tile_block(&router, &id, &f).await;
            app.tile_cache.lock().unwrap().retain_keys(|_| false);
            let fresh = tile_block(&router, &id, &f).await;
            assert!(
                carried == fresh,
                "{name}: a carried-over tile differs from a fresh bake"
            );
        }
        // The edits above really were localised (not every one fell back to All).
        assert!(
            kinds.iter().filter(|(_, d)| d.is_array()).count() >= 9,
            "{kinds:?}"
        );
        // Undo each of them the same way.
        for _ in 0..11 {
            let _ = tile_block(&router, &id, &f).await;
            let (st, _) = call(
                &router,
                "POST",
                &format!("/api/docs/{id}/undo"),
                serde_json::json!({"floor": f}).to_string().into_bytes(),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
            let carried = tile_block(&router, &id, &f).await;
            app.tile_cache.lock().unwrap().retain_keys(|_| false);
            let fresh = tile_block(&router, &id, &f).await;
            assert!(
                carried == fresh,
                "undo: a carried-over tile differs from a fresh bake"
            );
        }
    }

    #[tokio::test]
    async fn warm_tile_hits_the_cache_without_cloning() {
        let router = build_router(test_app());
        let (id, f) = new_doc(&router, "T").await;
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":[{"x":0,"y":0},{"x":1,"y":0}]}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let uri = format!("/tiles/{id}/{f}/0/0_0");
        let (_, cold) = call(&router, "GET", &uri, vec![]).await;
        let t = std::time::Instant::now();
        for _ in 0..100 {
            let (st, warm) = call(&router, "GET", &uri, vec![]).await;
            assert_eq!(st, StatusCode::OK);
            assert_eq!(warm, cold, "a cached tile is byte-identical to the bake");
        }
        let ms = t.elapsed().as_secs_f64() * 1e3;
        println!("100 warm tile GETs: {ms:.1} ms");
        #[cfg(not(debug_assertions))]
        assert!(ms < 50.0, "100 cached GETs took {ms:.1} ms");
    }

    #[tokio::test]
    async fn tile_baked_during_an_edit_is_never_served_afterwards() {
        let router = build_router(test_app());
        let (id, f) = new_doc(&router, "T").await;
        let (_, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/floors"),
            serde_json::json!({"action":"add","name":"Up"})
                .to_string()
                .into_bytes(),
        )
        .await;
        let up = jv(&b)["floor"].as_str().unwrap().to_string();
        // A busy onion floor makes the bake slow enough to overlap the edit.
        let cells: Vec<_> = (0..60)
            .flat_map(|x| (0..60).map(move |y| serde_json::json!({"x":x,"y":y})))
            .collect();
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": up, "ops": [
                {"t":"upsert_space","space":{"id":"sp_b","name":"B","color":"#5a7d9a","kind":"room"}},
                {"t":"paint_cells","space":"sp_b","cells":cells}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let uri = format!("/tiles/{id}/{f}/0/0_0?onion={up}");
        let (_, before) = call(&router, "GET", &uri, vec![]).await;
        // A different, uncached tile, so this GET really bakes while the edit lands.
        let slow = format!("/tiles/{id}/{f}/0/1_0?onion={up}");
        let edit = serde_json::json!({"floor": f, "ops": [
            {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","space":"sp_a","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":0,"y":1}]}]});
        let (_, st) = tokio::join!(
            call(&router, "GET", &slow, vec![]),
            post_ops_json(&router, &id, edit)
        );
        assert_eq!(st, StatusCode::OK);
        let (_, after) = call(&router, "GET", &uri, vec![]).await;
        assert_ne!(before, after, "the edit must show in the next tile");
    }

    #[tokio::test]
    async fn tile_cache_stays_inside_its_byte_budget() {
        let app = test_app();
        let (id, f);
        let router = build_router(app.clone());
        (id, f) = new_doc(&router, "T").await;
        let (_, one) = call(&router, "GET", &format!("/tiles/{id}/{f}/0/0_0"), vec![]).await;
        let budget = one.len() * 3 + one.len() / 2;
        *app.tile_cache.lock().unwrap() = cache::Lru::new(budget);
        for x in 0..10 {
            let (st, _) = call(&router, "GET", &format!("/tiles/{id}/{f}/0/{x}_0"), vec![]).await;
            assert_eq!(st, StatusCode::OK);
        }
        let c = app.tile_cache.lock().unwrap();
        assert!(c.used() <= budget, "{} > {budget}", c.used());
        assert!(c.len() >= 1 && c.len() <= 10);
    }

    #[tokio::test]
    async fn revert_busts_the_tile_cache() {
        let router = build_router(test_app());
        let (id, f) = new_doc(&router, "T").await;
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":0,"y":1},{"x":1,"y":1}]}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let uri = format!("/tiles/{id}/{f}/0/0_0");
        let (st, painted) = call(&router, "GET", &uri, vec![]).await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/revert"),
            serde_json::json!({"floor": f, "index": 0})
                .to_string()
                .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, reverted) = call(&router, "GET", &uri, vec![]).await;
        assert_eq!(st, StatusCode::OK);
        assert_ne!(painted, reverted, "revert must not serve the old tile");
    }

    #[tokio::test]
    async fn revert_keeps_duplicated_content_and_restores_every_layer() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "V").await;
        let cells: Vec<serde_json::Value> = (0..100)
            .map(|i| serde_json::json!({"x": i % 10, "y": i / 10}))
            .collect();
        let stamp = serde_json::json!({"id":"st1","asset":"barrel","x":1.5,"y":1.5,
            "scale":1.0,"rotation":0.0,"flip":false,"tint":null,"shadow":true,"gen":null});
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","space":"sp_a","cells":cells},
                {"t":"add_stamp","stamp":stamp}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        // Duplicate the floor: its content is not in the journal.
        let (st, b) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/floors"),
            serde_json::json!({"action":"add","name":"Copy","copy_from":f})
                .to_string()
                .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let dv = jv(&d);
        let copy = dv["floors"][1]["id"].as_str().unwrap().to_string();
        assert_eq!(
            dv["floors"][1]["grid"]["cells"].as_array().unwrap().len(),
            100
        );
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/ops"),
            serde_json::json!({"floor": copy, "ops": [
                {"t":"paint_cells","space":"sp_a","cells":[{"x":50,"y":50}]}]})
            .to_string()
            .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let n = app.store.read().await.docs[&id].journal.len() - 1;
        let revert = |floor: String, index: usize| {
            let router = router.clone();
            let id = id.clone();
            async move {
                call(
                    &router,
                    "POST",
                    &format!("/api/docs/{id}/revert"),
                    serde_json::json!({"floor": floor, "index": index})
                        .to_string()
                        .into_bytes(),
                )
                .await
            }
        };
        let (st, _) = revert(copy.clone(), n).await;
        assert_eq!(st, StatusCode::OK);
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let dv = jv(&d);
        assert_eq!(
            dv["floors"][1]["grid"]["cells"].as_array().unwrap().len(),
            100
        );
        assert_eq!(dv["floors"][1]["stamps"].as_array().unwrap().len(), 1);

        // A stamp added on the first floor, reverted away, then undone.
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [{"t":"add_stamp","stamp":
                {"id":"st2","asset":"crate","x":3.5,"y":3.5,"scale":1.0,"rotation":0.0,
                 "flip":false,"tint":null,"shadow":true,"gen":null}}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let stamps = |dv: &serde_json::Value| dv["floors"][0]["stamps"].as_array().unwrap().len();
        let at = app.store.read().await.docs[&id].journal.len() - 1;
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert_eq!(stamps(&jv(&d)), 2);
        let (st, _) = revert(f.clone(), at).await;
        assert_eq!(st, StatusCode::OK);
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert_eq!(
            stamps(&jv(&d)),
            1,
            "the stamp is gone after reverting before it"
        );
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/undo"),
            serde_json::json!({"floor": f}).to_string().into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        assert_eq!(stamps(&jv(&d)), 2, "undo brings the stamp back");
    }

    #[tokio::test]
    async fn revert_after_journal_compaction_does_not_empty_the_floor() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "C").await;
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        {
            let mut store = app.store.write().await;
            let doc = store.docs.get_mut(&id).unwrap();
            for i in 0..4100 {
                let op: ndcart_core::Op = serde_json::from_value(serde_json::json!(
                    {"t":"paint_cells","space":"sp_a","cells":[{"x":i % 200,"y":i / 200}]}
                ))
                .unwrap();
                doc.apply_ops(&f, &[op]).unwrap();
            }
            assert!(doc.journal.len() < 4100, "the journal compacted");
        }
        let (st, _) = call(
            &router,
            "POST",
            &format!("/api/docs/{id}/revert"),
            serde_json::json!({"floor": f, "index": 0})
                .to_string()
                .into_bytes(),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (_, d) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
        let cells = jv(&d)["floors"][0]["grid"]["cells"]
            .as_array()
            .unwrap()
            .len();
        assert!(
            cells > 0,
            "reverting inside the retained window keeps older content"
        );
    }

    #[tokio::test]
    async fn the_wall_cache_does_not_grow_with_edits() {
        let app = test_app();
        let router = build_router(app.clone());
        let (id, f) = new_doc(&router, "W").await;
        let st = post_ops_json(
            &router,
            &id,
            serde_json::json!({"floor": f, "ops": [
                {"t":"upsert_space","space":{"id":"sp_a","name":"A","color":"#7f5539","kind":"room"}}]}),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        for i in 0..50 {
            let st = post_ops_json(
                &router,
                &id,
                serde_json::json!({"floor": f, "ops": [
                    {"t":"paint_cells","space":"sp_a","cells":[{"x":i,"y":0}]}]}),
            )
            .await;
            assert_eq!(st, StatusCode::OK);
            let (st, _) = call(&router, "GET", &format!("/api/docs/{id}"), vec![]).await;
            assert_eq!(st, StatusCode::OK);
        }
        let floors = app.store.read().await.docs[&id].floors.len();
        let entries = app.wall_cache.lock().unwrap().len();
        assert!(
            entries >= 1 && entries <= floors,
            "{entries} entries, {floors} floors"
        );
    }

    #[tokio::test]
    async fn the_event_stream_ends_after_gone_for_a_missing_doc() {
        let router = build_router(test_app());
        let r = router
            .oneshot(
                Request::get("/api/docs/nope/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = tokio::time::timeout(std::time::Duration::from_secs(5), body_string(r))
            .await
            .expect("the stream must end, not run forever");
        assert_eq!(body.matches("event: gone").count(), 1, "{body}");
    }

    #[tokio::test]
    async fn cross_site_posts_are_refused_and_same_origin_ones_pass() {
        let router = build_router(test_app());
        let post = |hdrs: &[(&str, &str)]| {
            let mut b = Request::builder().method("POST").uri("/api/assets");
            for (k, v) in hdrs {
                b = b.header(*k, *v);
            }
            router
                .clone()
                .oneshot(b.body(Body::from(b"junk".to_vec())).unwrap())
        };
        let host = ("host", "127.0.0.1:8300");
        let r = post(&[host, ("origin", "https://evil.example")])
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = post(&[host, ("sec-fetch-site", "cross-site")])
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = post(&[host, ("sec-fetch-site", "same-site")])
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        // Same origin reaches the handler (junk body -> 400).
        let r = post(&[
            host,
            ("origin", "http://127.0.0.1:8300"),
            ("sec-fetch-site", "same-origin"),
        ])
        .await
        .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        // No Origin at all (curl, CLI, bridge) reaches the handler.
        let r = post(&[]).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        // GET is never filtered.
        let r = router
            .clone()
            .oneshot(
                Request::get("/health")
                    .header("origin", "https://evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn host_guard_refuses_unknown_hosts_in_loopback_mode() {
        let allowed = Arc::new(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
            "[::1]".to_string(),
        ]);
        let router = build_router(test_app()).layer(axum::middleware::from_fn(move |req, next| {
            auth::host_guard(allowed.clone(), req, next)
        }));
        let get = |host: Option<&str>| {
            let mut b = Request::get("/api/docs");
            if let Some(h) = host {
                b = b.header("host", h);
            }
            router.clone().oneshot(b.body(Body::empty()).unwrap())
        };
        let r = get(Some("evil.example:8300")).await.unwrap();
        assert_eq!(r.status(), StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(body_string(r).await, "unknown host");
        for ok in [
            "localhost:8300",
            "127.0.0.1:8300",
            "[::1]:8300",
            "LOCALHOST",
        ] {
            assert_eq!(
                get(Some(ok)).await.unwrap().status(),
                StatusCode::OK,
                "{ok}"
            );
        }
        assert_eq!(get(None).await.unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn hardening_headers_are_on_every_response() {
        let app = test_app();
        let id = ndcart_core::hash::sha256_hex(b"nope");
        let router = build_router(app);
        for uri in ["/health".to_string(), format!("/api/assets/{id}")] {
            let r = router
                .clone()
                .oneshot(Request::get(&uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let h = r.headers();
            assert_eq!(h["x-content-type-options"], "nosniff", "{uri}");
            assert_eq!(h["referrer-policy"], "no-referrer", "{uri}");
            assert_eq!(
                h["content-security-policy"], "frame-ancestors 'none'",
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn heavy_bakes_wait_for_a_permit() {
        let app = test_app();
        // One permit, held: an export must not start until it is released.
        let app1 = Arc::new(AppState {
            store: tokio::sync::RwLock::new(store::Store::default()),
            data_dir: None,
            ai: Mutex::new(ai_settings::load(None)),
            wall_cache: Mutex::new(HashMap::new()),
            tile_cache: Mutex::new(cache::Lru::new(TILE_CACHE_BYTES)),
            heavy: tokio::sync::Semaphore::new(1),
            table_tokens: Default::default(),
        });
        assert!(app.heavy.available_permits() >= 1);
        let router = build_router(app1.clone());
        let held = app1.heavy.acquire().await.unwrap();
        let mut png = tiny_skia::Pixmap::new(2, 2).unwrap();
        png.fill(tiny_skia::Color::from_rgba8(1, 2, 3, 255));
        let png = png.encode_png().unwrap();
        let req = Request::post("/api/convert?format=png")
            .body(Body::from(png))
            .unwrap();
        let mut fut = Box::pin(router.clone().oneshot(req));
        let early = tokio::time::timeout(std::time::Duration::from_millis(200), &mut fut).await;
        assert!(
            early.is_err(),
            "a convert must wait while the only permit is held"
        );
        drop(held);
        let r = tokio::time::timeout(std::time::Duration::from_secs(10), fut)
            .await
            .expect("runs once the permit is free")
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(app1.heavy.available_permits(), 1, "the permit is returned");
    }

    #[tokio::test]
    async fn table_token_is_scoped_to_the_table_screen_and_revocable() {
        let app = test_app();
        let tt = app.table_tokens.clone();
        let router = build_router(app).layer(axum::middleware::from_fn(move |req, next| {
            auth::gate("G".to_string(), tt.clone(), req, next)
        }));
        let send = |method: &str, uri: &str, hdr: Option<(&str, String)>, body: &str| {
            let mut b = Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json");
            if let Some((k, v)) = hdr {
                b = b.header(k, v);
            }
            router
                .clone()
                .oneshot(b.body(Body::from(body.to_string())).unwrap())
        };
        let gm = || Some(("authorization", "Bearer G".to_string()));
        let r = send("POST", "/api/docs", gm(), r#"{"name": "T"}"#)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        let r = send(
            "POST",
            &format!("/api/docs/{id}/table"),
            gm(),
            r#"{"x":1,"y":1}"#,
        )
        .await
        .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        let t = v["table_token"].as_str().unwrap().to_string();
        // A second move keeps the same token.
        let r = send(
            "POST",
            &format!("/api/docs/{id}/table"),
            gm(),
            r#"{"x":2,"y":1}"#,
        )
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&body_string(r).await).unwrap();
        assert_eq!(v["table_token"], t.as_str());

        let status = |m: &'static str, uri: String, hdr: Option<(&'static str, String)>| {
            let send = &send;
            async move { send(m, &uri, hdr, "{}").await.unwrap().status() }
        };
        // The GM document (secrets, labels) is out of reach for the table token.
        assert_eq!(
            status("GET", format!("/api/docs/{id}?token={t}"), None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(
                "GET",
                "/api/docs".into(),
                Some(("authorization", format!("Bearer {t}")))
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(
                "GET",
                format!("/api/docs/{id}/preview?token={t}&x=1&y=1"),
                None
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            status("GET", format!("/api/docs/{id}/events?token={t}"), None).await,
            StatusCode::OK
        );
        assert_eq!(
            status("GET", format!("/api/docs/{id}/table?token={t}"), None).await,
            StatusCode::OK
        );
        assert_eq!(
            status("POST", format!("/api/docs/{id}/table?token={t}"), None).await,
            StatusCode::BAD_REQUEST
        ); // reaches the handler ("{}" lacks x, y)
        assert_eq!(
            status("DELETE", format!("/api/docs/{id}/table?token={t}"), None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status("GET", format!("/api/docs/{id}/export/png?token={t}"), None).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status("GET", format!("/api/docs/other/preview?token={t}"), None).await,
            StatusCode::UNAUTHORIZED
        );
        // The page sets a cookie named for the project, which then carries the screen.
        let r = send("GET", &format!("/table/{id}?token={t}"), None, "")
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let cookie = r.headers()["set-cookie"].to_str().unwrap().to_string();
        assert!(
            cookie.starts_with(&format!("ndcart_table_{id}={t};")),
            "{cookie}"
        );
        let c = Some(("cookie", format!("ndcart_table_{id}={t}")));
        assert_eq!(
            status("GET", format!("/api/docs/{id}/table"), c).await,
            StatusCode::OK
        );

        // Turning the table off revokes the token.
        assert_eq!(
            status("DELETE", format!("/api/docs/{id}/table"), gm()).await,
            StatusCode::OK
        );
        assert_eq!(
            status("GET", format!("/api/docs/{id}/preview?token={t}"), None).await,
            StatusCode::UNAUTHORIZED
        );
        // And the GM token still works everywhere.
        assert_eq!(
            status("GET", format!("/api/docs/{id}"), gm()).await,
            StatusCode::OK
        );
    }
}
