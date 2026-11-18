//! `ndcart bench --large [--json]`: the "large floor" scenario from
//! ARCHITECTURE.md (400x280 cells in 5 materials, 14,800 props), timed on the
//! operations a busy session leans on. Budgets are loose enough to pass today
//! and tight enough to catch an order-of-magnitude regression.

use super::*;
use ndcart_core::grid::{Space, SpaceKind};
use ndcart_core::journal::Journal;
use std::time::Instant;

/// Budgets, in milliseconds.
const TILE_BUDGET_MS: f64 = 50.0;
const EDIT_BUDGET_MS: f64 = 50.0;
const GET_DOC_BUDGET_MS: f64 = 200.0;

/// The probe floor: 400x280 = 112,000 painted cells in 5 materials and 14,800 props.
pub(crate) fn large_floor() -> FloorDoc {
    let mut floor = FloorDoc::new("bench".into(), "Bench".into());
    let mut j = Journal::default();
    for i in 0..5 {
        let space = Space {
            id: format!("sp{i}"),
            name: format!("S{i}"),
            color: "#808080".into(),
            kind: SpaceKind::Room,
            material: Some(ndcart_paint::starter()[i].id.to_string()),
            tint: None,
        };
        store::apply_ops_direct(&mut floor, &mut j, &[Op::UpsertSpace { space }]).unwrap();
    }
    for i in 0..5 {
        let cells: Vec<_> = (0..400)
            .flat_map(|x| (0..280).map(move |y| (x, y)))
            .filter(|(x, y)| ((x / 7) + (y / 7)) % 5 == i)
            .map(|(x, y)| ndcart_core::CellKey::new(x, y))
            .collect();
        floor.grid.paint(&cells, Some(&format!("sp{i}"))).unwrap();
    }
    let assets = [
        "crate", "barrel", "table", "chair", "pillar", "torch", "bed", "rug", "chest", "tree",
        "bush", "rock",
    ];
    for i in 0..14_800usize {
        let st = ndcart_core::Stamp {
            id: format!("st{i}"),
            asset: assets[i % 12].into(),
            x: (i % 400) as f32 + 0.5,
            y: ((i / 400) * 7 % 280) as f32 + 0.5,
            scale: 1.0,
            rotation: 0.0,
            flip: false,
            tint: None,
            shadow: true,
            gen: None,
        };
        floor.add_stamp(st, None).unwrap();
    }
    floor
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn avg_ms(n: u32, mut f: impl FnMut()) -> f64 {
    let t = Instant::now();
    for _ in 0..n {
        f();
    }
    ms_since(t) / f64::from(n)
}

pub(crate) async fn run_large(json: bool) {
    let floor = large_floor();
    let cells = floor.grid.cell_count();
    let stamps = floor.stamps.len();

    let clone_ms = avg_ms(10, || {
        std::hint::black_box(floor.clone());
    });
    let walls = tiles::derive_walls(&floor, 32.0);
    // Cold bakes straight through the baker (no cache involved).
    let tile_centre_ms = avg_ms(5, || {
        std::hint::black_box(tiles::bake_tile_era(
            &floor, &walls, 32.0, 0, 5, 5, None, None, 0.0,
        ));
    });
    let tile_far_ms = avg_ms(5, || {
        std::hint::black_box(tiles::bake_tile_era(
            &floor, &walls, 32.0, 0, 1000, 1000, None, None, 0.0,
        ));
    });

    // Edits through the store, as the router does them.
    let mut doc = store::StoredDoc::new("bench", ndcart_core::doc::DocKind::Battle);
    doc.floors[0] = floor.clone();
    let fid = doc.floors[0].id.clone();
    let one = Op::PaintCells {
        cells: vec![ndcart_core::CellKey::new(500, 500)],
        space: Some("sp0".into()),
    };
    let edit_ms = avg_ms(20, || {
        doc.apply_ops(&fid, std::slice::from_ref(&one)).unwrap();
    });
    let batch = Op::Batch {
        ops: vec![one.clone()],
    };
    let edit_batch_ms = avg_ms(20, || {
        doc.apply_ops(&fid, std::slice::from_ref(&batch)).unwrap();
    });

    // GET /api/docs/{id}: floor_view plus JSON.
    let app: App = Arc::new(AppState {
        store: tokio::sync::RwLock::new(store::Store::default()),
        data_dir: None,
        ai: Mutex::new(ai_settings::load(None)),
        wall_cache: Mutex::new(HashMap::new()),
        tile_cache: Mutex::new(crate::cache::Lru::new(TILE_CACHE_BYTES)),
        heavy: tokio::sync::Semaphore::new(heavy_permits()),
        table_tokens: Default::default(),
    });
    let t = Instant::now();
    let fv = floor_view("bench", &app, &doc.floors[0], 32.0, 1);
    let js = serde_json::to_string(&fv).unwrap();
    let get_doc_ms = ms_since(t);
    let get_doc_bytes = js.len();
    drop((fv, js));

    // The inverse of one 20,000-cell paint.
    let paint_cells: Vec<_> = (0..200)
        .flat_map(|x| (0..100).map(move |y| ndcart_core::CellKey::new(x, y)))
        .collect();
    let big = Op::PaintCells {
        cells: paint_cells,
        space: Some("sp1".into()),
    };
    let inv = big.inverse_against(&doc.floors[0]).unwrap();
    let inverse_bytes = serde_json::to_string(&inv).unwrap().len();

    // Cold and warm tile through the real handler (cache, lock, clone, bake).
    let doc_id = doc.manifest.id.clone();
    app.store.write().await.docs.insert(doc_id.clone(), doc);
    let fetch = |x: i64, y: i64| {
        let app = app.clone();
        let (d, f) = (doc_id.clone(), fid.clone());
        async move {
            get_tile(
                axum::extract::State(app),
                axum::extract::Path((d, f, 0, format!("{x}_{y}"))),
                axum::extract::Query(TileQuery { onion: None }),
            )
            .await
            .ok()
            .expect("tile")
        }
    };
    let t = Instant::now();
    fetch(5, 5).await;
    let handler_cold_ms = ms_since(t);
    let t = Instant::now();
    for _ in 0..100 {
        fetch(5, 5).await;
    }
    let handler_warm_ms = ms_since(t) / 100.0;

    // (`get_tile_cold` also derives the walls once, so it is reported, not budgeted.)
    let worst_tile = tile_centre_ms;
    let worst_edit = edit_ms.max(edit_batch_ms);
    let pass = worst_tile < TILE_BUDGET_MS
        && tile_far_ms < TILE_BUDGET_MS
        && worst_edit < EDIT_BUDGET_MS
        && get_doc_ms < GET_DOC_BUDGET_MS;

    if json {
        let r = |v: f64| (v * 1000.0).round() / 1000.0;
        let out = serde_json::json!({
            "cells": cells,
            "stamps": stamps,
            "floor_clone_ms": r(clone_ms),
            "edit_1_cell_ms": r(edit_ms),
            "edit_1_cell_batch_ms": r(edit_batch_ms),
            "tile_z0_centre_ms": r(tile_centre_ms),
            "tile_z0_far_ms": r(tile_far_ms),
            "get_tile_cold_ms": r(handler_cold_ms),
            "get_tile_warm_ms": r(handler_warm_ms),
            "get_doc_ms": r(get_doc_ms),
            "get_doc_bytes": get_doc_bytes,
            "paint_20k_inverse_bytes": inverse_bytes,
            "budgets_ms": {"tile": TILE_BUDGET_MS, "edit": EDIT_BUDGET_MS, "get_doc": GET_DOC_BUDGET_MS},
            "pass": pass,
        });
        println!("{out}");
    } else {
        println!("ndcart bench --large: {cells} cells, {stamps} props");
        println!("  floor clone:            {clone_ms:>8.2} ms");
        println!("  edit, 1-cell paint:     {edit_ms:>8.2} ms   (budget {EDIT_BUDGET_MS})");
        println!("  edit, 1-cell batch:     {edit_batch_ms:>8.2} ms");
        println!("  tile z0 centre (bake):  {tile_centre_ms:>8.2} ms   (budget {TILE_BUDGET_MS})");
        println!("  tile z0 far (bake):     {tile_far_ms:>8.2} ms");
        println!("  GET tile cold:          {handler_cold_ms:>8.2} ms");
        println!("  GET tile warm:          {handler_warm_ms:>8.3} ms");
        println!("  GET doc:                {get_doc_ms:>8.2} ms, {get_doc_bytes} bytes   (budget {GET_DOC_BUDGET_MS})");
        println!("  20k-cell paint inverse: {inverse_bytes} bytes of JSON");
    }
    if !pass {
        eprintln!("large-floor budget blown (see the numbers above)");
        std::process::exit(1);
    }
}
