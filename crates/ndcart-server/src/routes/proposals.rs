//! Map and world proposals: previews that change nothing until accepted.

use crate::*;

#[derive(Deserialize)]
pub(crate) struct ProposeBody {
    pub(crate) spec: ndcart_core::mapspec::MapSpec,
    #[serde(default)]
    pub(crate) seed: Option<u64>,
}

pub(crate) fn proposal_summary(p: &store::Proposal) -> serde_json::Value {
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
    // What the linter found about the spec, mapped back to the spec's room ids.
    let problems = ndcart_core::speclint::lint_compiled(&p.spec, &p.floors).unwrap_or_default();
    serde_json::json!({ "id": p.id, "name": p.spec.name, "seed": p.seed, "floors": floors, "problems": problems })
}

/// `POST /api/docs/{id}/propose` `{spec, seed?}` — lay a MapSpec (rooms and how
/// they connect) out and hold it as a PROPOSAL. Nothing in the document changes
/// until it is accepted; a bad spec is refused with a message that says what to fix.
pub(crate) async fn propose_map(
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
pub(crate) struct ProposalPreviewQuery {
    /// Which floor of the proposal (0-based; default 0).
    pub(crate) floor: Option<usize>,
    pub(crate) ppc: Option<i64>,
}

/// `GET /api/docs/{id}/proposals/{pid}/preview?floor=N` — the proposed floor as a PNG.
pub(crate) async fn proposal_preview(
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
    let _heavy = app.heavy.acquire().await;
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
pub(crate) async fn accept_proposal(
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
    invalidate_doc(&app, &id);
    Ok(Json(
        serde_json::json!({ "ok": true, "rev": rev, "floors": floors }),
    ))
}

/// `DELETE /api/docs/{id}/proposals/{pid}` — reject it.
pub(crate) async fn reject_proposal(
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
pub(crate) struct ProposeWorldBody {
    pub(crate) spec: worldspec::WorldSpec,
    #[serde(default)]
    pub(crate) seed: Option<u32>,
}

/// `POST /api/docs/{id}/propose-world` `{spec, seed?}` — an overland map as a
/// proposal (terrain, rivers, biome props, places, regions). Nothing changes until accepted.
pub(crate) async fn propose_world(
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
pub(crate) async fn world_proposal_preview(
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
    let _heavy = app.heavy.acquire().await;
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
pub(crate) async fn accept_world(
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
    invalidate_doc(&app, &id);
    Ok(Json(serde_json::json!({ "ok": true, "rev": rev })))
}

/// `DELETE /api/docs/{id}/world-proposals/{pid}`.
pub(crate) async fn reject_world(
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
