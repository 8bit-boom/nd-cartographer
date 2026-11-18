//! Editing a project: ops, undo and redo, prefabs, scatter, furnishing,
//! underlays and the generators.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct GenerateBody {
    /// The floor to draw the world on (default: the first).
    pub(crate) floor: Option<String>,
    pub(crate) seed: Option<u32>,
    pub(crate) cols: Option<u32>,
    pub(crate) rows: Option<u32>,
    pub(crate) land: Option<f32>,
    pub(crate) continent: Option<f32>,
    pub(crate) ridges: Option<f32>,
    pub(crate) rivers: Option<u32>,
}

/// `GET /api/prefabs` — the library, newest last.
pub(crate) async fn list_prefabs(State(app): State<App>) -> Json<serde_json::Value> {
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
pub(crate) struct CapturePrefab {
    #[serde(default)]
    pub(crate) floor: Option<String>,
    pub(crate) name: String,
    pub(crate) spaces: Vec<String>,
}

/// `POST /api/docs/{id}/prefabs` `{floor?, name, spaces:[room ids]}` — copy
/// those rooms (doors, lights, notes on them) into the shared library.
pub(crate) async fn capture_prefab(
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
pub(crate) async fn delete_prefab(
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
pub(crate) struct PlacePrefab {
    #[serde(default)]
    pub(crate) floor: Option<String>,
    pub(crate) prefab: String,
    pub(crate) x: i32,
    pub(crate) y: i32,
    #[serde(default)]
    pub(crate) turns: u8,
}

/// `POST /api/docs/{id}/place-prefab` `{floor?, prefab, x, y, turns?}` — one undo step.
pub(crate) async fn place_prefab(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
pub(crate) struct ScatterBody {
    #[serde(default)]
    pub(crate) floor: Option<String>,
    /// The area, in cells.
    pub(crate) points: Vec<[f32; 2]>,
    /// A named biome (`forest`, `meadow`, `rocky`, `swamp`, `desert`), or give `assets`.
    #[serde(default)]
    pub(crate) biome: Option<String>,
    #[serde(default)]
    pub(crate) assets: Vec<String>,
    #[serde(default)]
    pub(crate) spacing: Option<f32>,
    #[serde(default = "one_f32")]
    pub(crate) density: f32,
    #[serde(default)]
    pub(crate) seed: u64,
}

pub(crate) fn one_f32() -> f32 {
    1.0
}

/// `POST /api/docs/{id}/scatter` — fill an area with props: a named biome
/// (tinted region + props) or an explicit prop list. One undoable step.
pub(crate) async fn scatter_props(
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
    // Terrain-aware rejection (the M2 leftover): when the floor has terrain,
    // props are not placed under water or on steep slopes. A 3x3 height
    // window around each candidate gives the local slope.
    let keep: Option<(ndcart_core::terrain::HeightGrid, u16, f32)> =
        doc.floors[idx].terrain.as_ref().map(|terr| {
            let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
            let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
            for p in &b.points {
                min_x = min_x.min(p[0]);
                min_y = min_y.min(p[1]);
                max_x = max_x.max(p[0]);
                max_y = max_y.max(p[1]);
            }
            let sea = terr.sea_level();
            let grid = terr.extract(
                min_x.floor() as i32 - 2,
                min_y.floor() as i32 - 2,
                (max_x - min_x).ceil() as i32 + 5,
                (max_y - min_y).ceil() as i32 + 5,
            );
            (grid, sea, 0.35) // slope threshold: rise/run per cell
        });
    let keep_fn = |x: f32, y: f32| -> bool {
        match &keep {
            None => true,
            Some((grid, sea, slope_max)) => {
                let gx = x - grid.x0 as f32;
                let gy = y - grid.y0 as f32;
                if gx < 1.0 || gy < 1.0 || gx > grid.w as f32 - 2.0 || gy > grid.h as f32 - 2.0 {
                    return false;
                }
                let ix = gx as i32 + 1;
                let iy = gy as i32 + 1;
                let h = grid.at(ix, iy);
                if h < f32::from(*sea) + 40.0 {
                    return false; // under water or the shoreline band
                }
                let dx = (grid.at(ix + 1, iy) - grid.at(ix - 1, iy)).abs();
                let dy = (grid.at(ix, iy + 1) - grid.at(ix, iy - 1)).abs();
                dx.max(dy) / 65535.0 <= *slope_max
            }
        }
    };
    let ops = match &b.biome {
        Some(name) => {
            let biome = ndcart_core::scatter::biome(name)
                .ok_or_else(|| CoreError::Invalid(format!("unknown biome {name}")))?;
            ndcart_core::scatter::scatter_biome_where(
                &b.points, biome, b.density, b.seed, taken, true, &keep_fn,
            )?
        }
        None => ndcart_core::scatter::scatter_where(
            &ndcart_core::scatter::ScatterParams {
                polygon: b.points.clone(),
                assets: b.assets.clone(),
                spacing: b.spacing.unwrap_or(2.0),
                seed: b.seed,
                scale_min: 0.8,
                scale_max: 1.2,
            },
            taken,
            &keep_fn,
        )?,
    };
    doc.apply_ops(&floor_id, &ops)?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), &store.docs[&id]);
    drop(store);
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
pub(crate) struct FurnishBody {
    #[serde(default)]
    pub(crate) floor: Option<String>,
    pub(crate) spaces: Vec<String>,
    #[serde(default)]
    pub(crate) seed: u64,
    /// Skip rooms that already have props (default true).
    #[serde(default = "yes_bool")]
    pub(crate) only_empty: bool,
}

pub(crate) fn yes_bool() -> bool {
    true
}

/// `GET /api/world/tables` — the world's random tables (plan 21.4), so a
/// room's contents can be rolled from the world's own tables.
#[cfg(feature = "bridge")]
#[cfg(feature = "bridge")]
pub(crate) async fn world_tables(
    State(app): State<App>,
) -> Result<Json<serde_json::Value>, ApiError> {
    #[cfg(feature = "bridge")]
    {
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
        if cfg.direct.is_some() {
            return Err(ApiError(CoreError::Invalid(
                "tables live in nd-world; a standalone model has none".into(),
            )));
        }
        let backend = draft::Backend::connect(&cfg)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        let draft::Backend::World(bridge, _) = &backend else {
            return Err(ApiError(CoreError::Invalid(
                "tables live in nd-world".into(),
            )));
        };
        let tables = bridge
            .tables()
            .await
            .map_err(|e| ApiError(CoreError::Invalid(e.to_string())))?;
        Ok(Json(tables))
    }
    #[cfg(not(feature = "bridge"))]
    {
        Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
    }
}

/// `POST /api/docs/{id}/stock-room` `{floor, room, table_name}` — roll the
/// named world table (one weighted pick per click) into the room's encounter
/// creatures. One undo step. Needs nd-world; the studio-side encounter is
/// GM information and never publishes to players.
#[cfg(feature = "bridge")]
#[cfg(feature = "bridge")]
pub(crate) async fn stock_room(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let room = b
        .get("room")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError(CoreError::Invalid("body needs {room, table_name}".into())))?
        .to_string();
    let table_name = b
        .get("table_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError(CoreError::Invalid("body needs {room, table_name}".into())))?
        .to_string();
    let floor_id = b.get("floor").and_then(|v| v.as_str()).map(str::to_string);

    // The table comes from nd-world (its own roll semantics are a weighted
    // pick; the studio rolls locally so repeated picks are visible).
    #[cfg(feature = "bridge")]
    let entries: Vec<String> = {
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
        if cfg.direct.is_some() {
            return Err(ApiError(CoreError::Invalid(
                "tables live in nd-world; a standalone model has none".into(),
            )));
        }
        let backend = draft::Backend::connect(&cfg)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        let draft::Backend::World(bridge, _) = &backend else {
            return Err(ApiError(CoreError::Invalid(
                "tables live in nd-world".into(),
            )));
        };
        let tables = bridge
            .tables()
            .await
            .map_err(|e| ApiError(CoreError::Invalid(e.to_string())))?;
        let tbl = tables
            .get("tables")
            .and_then(|t| t.as_array())
            .and_then(|a| {
                a.iter()
                    .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(&table_name))
            })
            .ok_or_else(|| ApiError(CoreError::NotFound(format!("table {table_name}"))))?
            .clone();
        tbl.get("entries")
            .and_then(|e| e.as_array())
            .map(|entries| {
                let mut pool: Vec<String> = Vec::new();
                for e in entries {
                    let label = e
                        .get("label")
                        .and_then(|l| l.as_str())
                        .unwrap_or("")
                        .to_string();
                    let weight =
                        e.get("weight").and_then(|w| w.as_i64()).unwrap_or(1).max(1) as usize;
                    for _ in 0..weight {
                        pool.push(label.clone());
                    }
                }
                pool
            })
            .ok_or_else(|| {
                ApiError(CoreError::NotFound(format!(
                    "table {table_name} has no entries"
                )))
            })?
    };
    #[cfg(not(feature = "bridge"))]
    let entries: Vec<String> = Vec::new();
    #[cfg(not(feature = "bridge"))]
    let _ = table_name;

    #[cfg(feature = "bridge")]
    let pick = {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        entries
            .get(rng.gen_range(0..entries.len()))
            .cloned()
            .unwrap_or_default()
    };
    #[cfg(not(feature = "bridge"))]
    let pick = String::new();

    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let floor = match &floor_id {
        Some(f) => {
            ndcart_core::doc::floors_by_id(&doc.floors, f)?;
            f.clone()
        }
        None => doc.floors[0].id.clone(),
    };
    let idx = ndcart_core::doc::floors_by_id(&doc.floors, &floor)?;
    if !doc.floors[idx].grid.spaces.contains_key(&room) {
        return Err(ApiError(CoreError::NotFound(format!("room {room}"))));
    }
    let existing = doc.floors[idx]
        .encounters
        .iter()
        .find(|e| e.space == room)
        .cloned();
    let mut creatures: Vec<String> = existing
        .as_ref()
        .map(|e| e.creatures.clone())
        .unwrap_or_default();
    if !pick.is_empty() {
        creatures.push(pick.clone());
    }
    creatures.truncate(ndcart_core::doc::MAX_CREATURE_LINES);
    let (enc_id, note) = existing
        .as_ref()
        .map(|x| (x.id.clone(), x.note.clone()))
        .unwrap_or_else(|| (ndcart_core::ids::new_id("enc"), String::new()));
    let e = ndcart_core::doc::Encounter {
        id: enc_id,
        space: room.clone(),
        creatures,
        note,
    };
    doc.apply_ops(&floor, &[ndcart_core::Op::PutEncounter { encounter: e }])?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), doc);
    drop(store);
    Ok(Json(
        serde_json::json!({ "ok": true, "rolled": pick, "rev": rev }),
    ))
}

#[cfg(not(feature = "bridge"))]
pub(crate) async fn world_tables(_app: State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
}

#[cfg(not(feature = "bridge"))]
pub(crate) async fn stock_room(
    _app: State<App>,
    _id: Path<String>,
    _body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
}

/// `GET /api/world/entities/{id}/preview` - the two-way lore sync's read
/// half (plan 21.2): the linked entity's lore as nd-world shows the GM.
/// Also returns the configured server so the UI can deep-link to the full
/// page there.
pub(crate) async fn world_entity_preview(
    #[cfg_attr(not(feature = "bridge"), allow(unused_variables))] State(app): State<App>,
    #[cfg_attr(not(feature = "bridge"), allow(unused_variables))] Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _ = app;
    #[cfg(feature = "bridge")]
    {
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
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
pub(crate) async fn reroll_room(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({
        "ok": true, "removed": removed, "placed": added,
    })))
}

/// `POST /api/docs/{id}/furnish` `{floor?, spaces:[room ids], seed, only_empty?}` —
/// props chosen from each room's name and kind, never in a doorway. One undo step.
pub(crate) async fn furnish_rooms(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
pub(crate) struct UnderlayBody {
    #[serde(default)]
    pub(crate) floor: Option<String>,
    pub(crate) asset: String,
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
    #[serde(default = "one_f32")]
    pub(crate) opacity: f32,
}

/// `POST /api/docs/{id}/underlay` `{floor?, asset, x, y, w, h, opacity?}` — put a
/// stored picture behind the floor (cells), as one undoable step. A layer: it
/// never changes walls or rooms.
pub(crate) async fn set_underlay(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

/// `POST /api/docs/{id}/generate-map` `{floor?, kind, seed, width, height}` — a
/// dungeon, cave, building, deck or city district drawn onto the floor as ONE
/// undoable step (it adds to what is there; the same seed gives the same map).
pub(crate) async fn generate_map(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
pub(crate) struct GenMapBody {
    #[serde(default)]
    pub(crate) floor: Option<String>,
    pub(crate) kind: ndcart_core::gen::GenKind,
    #[serde(default)]
    pub(crate) seed: u64,
    #[serde(default = "default_gen_w")]
    pub(crate) width: i32,
    #[serde(default = "default_gen_h")]
    pub(crate) height: i32,
}

pub(crate) fn default_gen_w() -> i32 {
    64
}

pub(crate) fn default_gen_h() -> i32 {
    48
}

/// `POST /api/docs/{id}/generate` — a seeded world (continents, mountains, a sea
/// level for the requested land fraction, rivers to the sea) replaces the
/// floor's terrain and rivers as ONE undoable step. Size is capped at 512x512
/// cells here so a request cannot tie the server up.
pub(crate) async fn generate_world(
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
    let idx = match &b.floor {
        Some(fid) => floors_by_id(&doc.floors, fid)?,
        None => 0,
    };
    let floor_id = doc.floors[idx].id.clone();
    let t = &world.terrain;
    let mut ops: Vec<Op> = doc.floors[idx]
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({
        "ok": true, "floor": floor_id, "cols": cols, "rows": rows, "rivers": river_count,
        "sea_level": t.sea_level(), "chunks": t.chunk_count(),
    })))
}

/// Write a periodic snapshot after the store lock is released, on a blocking
/// thread (the journal append already happened; this is the slow part).
pub(crate) fn write_snapshot_later(job: Option<store::SnapshotJob>) {
    if let Some(job) = job {
        tokio::task::spawn_blocking(move || job.write());
    }
}

#[derive(Deserialize)]
pub(crate) struct OpsBody {
    pub(crate) floor: String,
    pub(crate) ops: Vec<Op>,
}

pub(crate) async fn post_ops(
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
    let dirty = dirty_of(doc, &b.floor, &b.ops);
    let old_rev = doc.rev;
    doc.apply_ops(&b.floor, &b.ops)?;
    let rev = doc.rev;
    let snapshot = store::save_doc_edit(app.data_dir.as_deref(), doc);
    // Tiles that the edit cannot have touched carry over to the new rev.
    invalidate_dirty(&app, &id, &b.floor, old_rev, rev, dirty);
    drop(store);
    write_snapshot_later(snapshot);
    Ok(Json(
        serde_json::json!({"ok": true, "rev": rev, "dirty": dirty_json(dirty)}),
    ))
}

#[derive(Deserialize)]
pub(crate) struct FloorBody {
    pub(crate) floor: String,
}

pub(crate) async fn post_undo(
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
    let dirty = match doc.peek_undo(&b.floor) {
        Some(inverse) => dirty_of(doc, &b.floor, std::slice::from_ref(&inverse)),
        None => ndcart_core::Dirty::All,
    };
    let old_rev = doc.rev;
    doc.undo_last(&b.floor)?;
    let rev = doc.rev;
    let snapshot = store::save_doc_edit(app.data_dir.as_deref(), doc);
    invalidate_dirty(&app, &id, &b.floor, old_rev, rev, dirty);
    drop(store);
    write_snapshot_later(snapshot);
    Ok(Json(
        serde_json::json!({"ok": true, "rev": rev, "dirty": dirty_json(dirty)}),
    ))
}

pub(crate) async fn post_redo(
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
    let dirty = match doc.peek_redo(&b.floor) {
        Some(op) => dirty_of(doc, &b.floor, std::slice::from_ref(&op)),
        None => ndcart_core::Dirty::All,
    };
    let old_rev = doc.rev;
    doc.redo_last(&b.floor)?;
    let rev = doc.rev;
    let snapshot = store::save_doc_edit(app.data_dir.as_deref(), doc);
    invalidate_dirty(&app, &id, &b.floor, old_rev, rev, dirty);
    drop(store);
    write_snapshot_later(snapshot);
    Ok(Json(
        serde_json::json!({"ok": true, "rev": rev, "dirty": dirty_json(dirty)}),
    ))
}
