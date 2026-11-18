//! Projects: listing, creating and reading, floors, checkpoints, history,
//! lint, search and the change feed.

use crate::*;

#[derive(Serialize)]
pub(crate) struct DocSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: &'static str,
    pub(crate) rev: u64,
    pub(crate) floors: Vec<String>,
    /// The nd-world slug of the last publish, so markers can link to it and
    /// the UI can tell published maps from drafts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) slug: Option<String>,
}

pub(crate) async fn list_docs(State(app): State<App>) -> Json<serde_json::Value> {
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
pub(crate) struct CreateBody {
    pub(crate) name: String,
    pub(crate) kind: Option<DocKind>,
}

pub(crate) async fn create_doc(
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
    invalidate_doc(&app, &manifest.id);
    Ok(Json(manifest))
}

#[derive(Deserialize)]
pub(crate) struct DocQuery {
    /// With `lite`: the one floor to send in full.
    pub(crate) floor: Option<String>,
    /// `1`: the other floors come as `{id, name, digest}` only.
    pub(crate) lite: Option<String>,
}

/// A floor in a lite document view.
#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum LiteFloor {
    Full(Box<FloorView>),
    Stub {
        id: String,
        name: String,
        digest: String,
    },
}

#[derive(Serialize)]
pub(crate) struct LiteDocView {
    pub(crate) manifest: Manifest,
    pub(crate) rev: u64,
    pub(crate) floors: Vec<LiteFloor>,
}

pub(crate) async fn get_doc(
    State(app): State<App>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<DocQuery>,
) -> Result<Response, ApiError> {
    let store = app.store.read().await;
    let doc = store
        .docs
        .get(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    if q.lite.as_deref().is_none_or(|l| l == "0" || l.is_empty()) {
        return Ok(Json(doc_view(&app, doc)).into_response());
    }
    // Lite: one floor in full, the rest as stubs; after an edit the studio
    // only needs the floor it is looking at.
    let full_id = q
        .floor
        .as_deref()
        .ok_or_else(|| CoreError::Invalid("lite=1 needs floor=<id>".into()))?;
    floors_by_id(&doc.floors, full_id)?;
    let floors = doc
        .floors
        .iter()
        .map(|f| {
            if f.id == full_id {
                LiteFloor::Full(Box::new(floor_view(
                    &doc.manifest.id,
                    &app,
                    f,
                    doc.manifest.cell_size,
                    doc.rev,
                )))
            } else {
                LiteFloor::Stub {
                    id: f.id.clone(),
                    name: f.name.clone(),
                    digest: f.digest(),
                }
            }
        })
        .collect();
    Ok(Json(LiteDocView {
        manifest: doc.manifest.clone(),
        rev: doc.rev,
        floors,
    })
    .into_response())
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum FloorRequest {
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
pub(crate) async fn floors_route(
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
    invalidate_doc(&app, &id);
    Ok(Json(
        serde_json::json!({ "floors": floors, "floor": new_floor }),
    ))
}

/// `POST /api/docs/{id}/undo-structure` — undo the last floor add/rename/move/remove.
pub(crate) async fn undo_structure(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "floors": floors })))
}

#[derive(Deserialize)]
pub(crate) struct CheckpointBody {
    pub(crate) name: String,
}

/// `GET /api/docs/{id}/checkpoints` — named save points, newest last.
pub(crate) async fn list_checkpoints(
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
pub(crate) async fn create_checkpoint(
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
pub(crate) async fn delete_checkpoint(
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
pub(crate) async fn restore_checkpoint(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

#[derive(Deserialize)]
pub(crate) struct SearchQuery {
    pub(crate) q: String,
}

/// `GET /api/search?q=` — "ask your maps": every project's rooms, notes, markers,
/// zones, regions, routes and floors whose name or text holds ALL the words
/// typed (case-insensitive). At most 100 hits; each says where it is.
pub(crate) async fn search_maps(
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

/// `GET /api/docs/{id}/lint[?floor=ID]` — the Problems panel: deterministic
/// checks with undoable fixes (`fix` is an op you can POST to `/ops`).
pub(crate) async fn lint_doc(
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
pub(crate) async fn floor_spec(
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

/// `GET /api/docs/{id}/history?floor=ID` - the history browser's data
/// (honest-limits closure): the journal summarized one line per entry
/// (oldest first) with its index, so the GM sees everything that happened
/// beyond checkpoints. Summaries come from the op's serde tag plus a small
/// human hint (room name, cell count) where cheap.
pub(crate) async fn doc_history(
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
    let floor_id = doc.floors[idx].id.clone();
    let entries: Vec<serde_json::Value> = doc
        .journal
        .entries()
        .iter()
        .enumerate()
        .filter(|(_, e)| e.is_for(&floor_id))
        .map(|(i, e)| {
            serde_json::json!({
                "index": i,
                "summary": ndcart_core::journal::op_summary(&e.op),
                "undo": e.undoes.is_some(),
            })
        })
        .collect();
    Ok(Json(
        serde_json::json!({ "floor": floor_id, "entries": entries }),
    ))
}

/// `POST /api/docs/{id}/revert` `{floor, index}` - the history browser's
/// jump: the floor's state at `index` is reached by applying the stored
/// inverses of every later entry, newest first, as ONE undoable batch
/// (nothing is lost; undo returns to now).
pub(crate) async fn revert_to(
    State(app): State<App>,
    Path(id): Path<String>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let index = b
        .get("index")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| ApiError(CoreError::Invalid("body needs {floor, index}".into())))?
        as usize;
    let floor_id = b.get("floor").and_then(|v| v.as_str()).map(str::to_string);
    let mut store = app.store.write().await;
    let doc = store
        .docs
        .get_mut(&id)
        .ok_or_else(|| CoreError::NotFound(format!("document {id}")))?;
    let fid = match &floor_id {
        Some(f) => f.clone(),
        None => doc.floors[0].id.clone(),
    };
    floors_by_id(&doc.floors, &fid)?;
    let journal = &doc.journal;
    if index > journal.len() {
        return Err(ApiError(CoreError::Invalid(format!(
            "index {index} is beyond the journal ({} entries)",
            journal.len()
        ))));
    }
    // Walk the journal's stored inverses back from the newest entry down to
    // `index`. Every layer comes back, and it works after compaction, floor
    // duplication and checkpoint restore (nothing is replayed from empty).
    let ops: Vec<ndcart_core::Op> = journal.entries()[index..]
        .iter()
        .rev()
        .filter(|e| e.is_for(&fid))
        .map(|e| e.inverse.clone())
        .collect();
    if ops.is_empty() {
        return Ok(Json(
            serde_json::json!({ "ok": true, "unchanged": true, "rev": doc.rev, "restored_to": index }),
        ));
    }
    doc.apply_ops(&fid, &[ndcart_core::Op::Batch { ops }])?;
    let rev = doc.rev;
    store::save_doc(app.data_dir.as_deref(), doc);
    drop(store);
    invalidate_doc(&app, &id);
    Ok(Json(
        serde_json::json!({ "ok": true, "rev": rev, "restored_to": index }),
    ))
}

/// `GET /api/docs/{id}/stats?floor=ID` — the pacing dashboard's numbers
/// (plan §21.6): rooms/squares/kinds, doors and secrets, lights and their
/// coverage, props by provenance, GM labels, encounters, lore coverage.
pub(crate) async fn doc_stats(
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

/// `GET /api/docs/{id}/events` — a server-sent-events feed of this
/// document's revision (plan §21.16 + the M0 leftover "WebSocket change
/// feed"): every tab, the table screen and a script stay in sync without
/// polling. The loop reads the in-memory rev every 400 ms and pushes an
/// event only when it changes; a 15 s comment keeps proxies alive.
pub(crate) async fn doc_events(
    State(app): State<App>,
    Path(id): Path<String>,
) -> Sse<
    impl futures_core::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
> {
    let stream = futures_util::stream::unfold(
        (
            app,
            id,
            0u64,
            tokio::time::interval(std::time::Duration::from_millis(400)),
            false, // `gone` has been sent: end the stream
        ),
        |(app, id, mut last, mut tick, gone)| async move {
            if gone {
                return None;
            }
            tick.tick().await;
            let rev = app.store.read().await.docs.get(&id).map(|d| d.rev);
            match rev {
                Some(r) if r != last => {
                    last = r;
                    let ev = axum::response::sse::Event::default()
                        .event("rev")
                        .data(r.to_string());
                    Some((
                        Ok::<_, std::convert::Infallible>(ev),
                        (app, id, last, tick, false),
                    ))
                }
                Some(_) => {
                    let ping = axum::response::sse::Event::default().comment("ping");
                    Some((Ok(ping), (app, id, last, tick, false)))
                }
                None => Some((
                    Ok(axum::response::sse::Event::default().event("gone").data("")),
                    (app, id, last, tick, true),
                )),
            }
        },
    );
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

#[derive(Deserialize)]
pub(crate) struct StyleBody {
    pub(crate) tone: String,
    #[serde(default)]
    pub(crate) palette: Vec<String>,
}

/// `POST /api/docs/{id}/style` `{tone, palette}` — the project's house style:
/// handed to AI drafts, and its palette colours generated rooms. An empty tone
/// and palette clear it.
pub(crate) async fn set_style(
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
