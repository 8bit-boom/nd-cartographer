//! `ndcart` — the nd-cartographer server binary. One binary, three ways to
//! run it (plan §4): `ndcart serve` (desktop/docker), `ndcart bench`.
//! Desktop is the Tauri shell wrapping this same server (M0 ships serve).
//!
//! Layout: `state` (the app handle, caches, errors, JSON views), `routes/*`
//! (the HTTP handlers, grouped by what they serve) and this file, which lists
//! every route in [`build_router`] and holds the CLI entry points. The module
//! map is in docs/ARCHITECTURE.md.

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
mod specshape;
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

const TABLE_HTML: &str = include_str!("../../../ui/table.html");

/// Every route of the studio, in one place. To add one: write the handler in
/// the `routes/` module it belongs to (a new module for a new area: add it to
/// `routes/mod.rs` and import it with the others above), then register it
/// here. Body limits: axum applies a `.layer(DefaultBodyLimit…)` to the routes
/// registered *above* it, so a route taking a big body brings its own limit
/// (as `/api/packs/import` and `/api/import/bundle` do) and goes below the
/// shared `/api/convert` limit.
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

#[cfg(test)]
mod tests;
