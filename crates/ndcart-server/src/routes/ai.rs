//! AI features: picture jobs, drafting and refining, asking the maps,
//! the model settings and nd-world entities.

use crate::*;

#[cfg(not(feature = "bridge"))]
pub(crate) async fn theme_pack(
    _app: State<App>,
    _body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
}

#[derive(Deserialize)]
pub(crate) struct AiImageBody {
    /// `underlay`, `material` or `prop`.
    pub(crate) kind: String,
    pub(crate) prompt: String,
    #[serde(default = "d_img")]
    pub(crate) width: u32,
    #[serde(default = "d_img")]
    pub(crate) height: u32,
    /// How many variants (1-4; a set shares a seed family).
    #[serde(default = "one_usize")]
    pub(crate) count: usize,
    #[serde(default = "minus_one")]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) seed: i64,
    /// Start from a picture an earlier job made (`{job, index}`), so a set stays coherent.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) like: Option<AnchorRef>,
    /// 0.1-0.95: how strongly the anchor holds (default 0.6).
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) strength: Option<f64>,
    /// Upscale the result by this factor (needs `ND_CARTO_UPSCALE_MODEL` in the studio's environment).
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) upscale: Option<f64>,
}

#[derive(Deserialize)]
pub(crate) struct AnchorRef {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) job: String,
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) index: usize,
}

pub(crate) fn d_img() -> u32 {
    512
}

pub(crate) fn one_usize() -> usize {
    1
}

/// `POST /api/ai/image` `{kind, prompt, width?, height?, count?, seed?}` — queue
/// pictures through nd-world's imagegen in the background. Returns the job; poll
/// `GET /api/ai/jobs/{id}`. Nothing is added anywhere until the GM accepts a picture.
pub(crate) async fn ai_image(
    State(app): State<App>,
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
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
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
pub(crate) async fn ai_job(Path(id): Path<String>) -> Result<Json<aijobs::AiJob>, ApiError> {
    aijobs::get(&id)
        .map(Json)
        .ok_or_else(|| ApiError(CoreError::NotFound(format!("job {id}"))))
}

/// `POST /api/ai/jobs/{id}/cancel`.
pub(crate) async fn ai_job_cancel(
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if aijobs::cancel(&id) {
        Ok(Json(serde_json::json!({ "ok": true })))
    } else {
        Err(ApiError(CoreError::NotFound(format!(
            "no running job {id}"
        ))))
    }
}

#[derive(Deserialize)]
pub(crate) struct DraftBody {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) prompt: String,
    /// The project whose house style to follow.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) doc: Option<String>,
    /// Reuse the names of the world's existing locations (nd-world only).
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) lore: bool,
    /// Return `{job}` at once and poll `GET /api/ai/jobs/{id}` for the phase and the spec.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) background: bool,
    /// With no AI set up, answer 404 instead of a keyword-built layout.
    #[serde(default)]
    pub(crate) strict_ai: bool,
    /// "Ask again": skip the cache for this request (the new answer is still stored).
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) nocache: bool,
}

/// The no-model answer: a layout built from the prompt's keywords, valid and linted.
fn template_draft(prompt: &str) -> Result<Json<serde_json::Value>, ApiError> {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > 2_000 {
        return Err(ApiError(CoreError::Invalid(
            "describe the map in 1-2000 characters".into(),
        )));
    }
    // A stable seed from the words, so the same prompt gives the same layout.
    let seed = prompt.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let spec = ndcart_core::template::template_from_prompt(prompt, seed);
    let warnings = ndcart_core::speclint::lint_spec(&spec, 1).unwrap_or_default();
    Ok(Json(serde_json::json!({
        "spec": spec,
        "warnings": warnings,
        "source": "template",
        "note": "No AI is set up; this layout came from keywords.",
    })))
}

/// Queue a draft as a background job and answer with its id.
#[cfg(feature = "bridge")]
fn start_draft_job(
    cfg: draft::Config,
    prompt: &str,
    ctx: draft::Context,
    world: bool,
) -> Result<Json<serde_json::Value>, ApiError> {
    if prompt.trim().is_empty() || prompt.chars().count() > 2_000 {
        return Err(ApiError(CoreError::Invalid(
            "describe the map in 1-2000 characters".into(),
        )));
    }
    let id = aijobs::register(if world { "draft-world" } else { "draft" }, prompt, 1)
        .map_err(|m| ApiError(CoreError::CapExceeded(m)))?;
    let task = tokio::spawn(aijobs::run_draft(
        id.clone(),
        cfg,
        prompt.to_string(),
        ctx,
        world,
    ));
    aijobs::set_abort(&id, task.abort_handle());
    Ok(Json(serde_json::json!({ "job": id })))
}

#[cfg(not(feature = "bridge"))]
pub(crate) const NO_BRIDGE: &str = "this build has no AI client (built without the bridge feature)";

#[cfg(feature = "bridge")]
pub(crate) async fn draft_context(app: &App, doc: &Option<String>, lore: bool) -> draft::Context {
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
pub(crate) async fn draft_spec(
    State(app): State<App>,
    body: Result<Json<DraftBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {\"prompt\": ...}".into())))?;
    #[cfg(feature = "bridge")]
    {
        let mut cfg = match app.resolve_ai() {
            Ok(c) => c,
            Err(_) if !b.strict_ai => return template_draft(&b.prompt),
            Err(m) => return Err(ApiError(CoreError::NotFound(m))),
        };
        cfg.cache.skip_read = b.nocache;
        let ctx = draft_context(&app, &b.doc, b.lore).await;
        if b.background {
            return start_draft_job(cfg, &b.prompt, ctx, false);
        }
        let d = draft::draft(&cfg, &b.prompt, &ctx)
            .await
            .map_err(|m| ApiError(CoreError::Invalid(m)))?;
        Ok(Json(
            serde_json::json!({ "spec": d.spec, "warnings": d.warnings, "attempts": d.attempts, "seconds": d.seconds, "source": "ai" }),
        ))
    }
    #[cfg(not(feature = "bridge"))]
    {
        let _ = app;
        if b.strict_ai {
            return Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())));
        }
        template_draft(&b.prompt)
    }
}

/// `POST /api/draft-world` `{prompt, doc?, lore?}` — like `/api/draft`, for an overland map.
pub(crate) async fn draft_world_spec(
    State(app): State<App>,
    body: Result<Json<DraftBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) =
        body.map_err(|_| ApiError(CoreError::Invalid("body must be {\"prompt\": ...}".into())))?;
    #[cfg(feature = "bridge")]
    {
        let mut cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
        cfg.cache.skip_read = b.nocache;
        let ctx = draft_context(&app, &b.doc, b.lore).await;
        if b.background {
            return start_draft_job(cfg, &b.prompt, ctx, true);
        }
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
pub(crate) struct RefineBody {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) floor: Option<String>,
    /// Room ids to keep exactly as they are.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) pins: Vec<String>,
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) instruction: String,
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) lore: bool,
    /// Refine this floor spec (a previewed proposal) instead of a floor of the project:
    /// the "keep talking to it" case. Pins then refer to this spec's room ids.
    #[serde(default)]
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) spec: Option<ndcart_core::mapspec::FloorSpec>,
}

/// `POST /api/docs/{id}/refine` `{floor?, pins:[room ids], instruction}` — describe
/// the floor as a spec, ask the AI to revise it keeping the pinned rooms, and
/// return the revised spec for preview (it becomes a proposal, a NEW floor; the
/// floor being refined is never touched).
pub(crate) async fn refine_floor(
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
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
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

#[derive(Deserialize)]
pub(crate) struct AskBody {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) question: String,
}

/// `POST /api/ask` `{question}` — ask your maps a question in plain words. The
/// answer comes from a text digest of every project (room names, notes, markers,
/// zones…), and the model is told to say so when the maps do not hold it.
pub(crate) async fn ask_maps(
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
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
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
pub(crate) struct EntitiesQuery {
    #[cfg_attr(not(feature = "bridge"), allow(dead_code))]
    pub(crate) q: Option<String>,
}

/// `GET /api/world/entities?q=` — the world's entities (nd-world), filtered by name,
/// so a room can be linked to one. At most 50 matches.
pub(crate) async fn world_entities(
    State(app): State<App>,
    axum::extract::Query(q): axum::extract::Query<EntitiesQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    #[cfg(feature = "bridge")]
    {
        let cfg = app
            .resolve_ai()
            .map_err(|m| ApiError(CoreError::NotFound(m)))?;
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

impl AppState {
    /// The AI config for this call: the saved settings merged under the
    /// environment (env wins per field - plan 22's settings work).
    #[cfg(feature = "bridge")]
    pub(crate) fn resolve_ai(&self) -> Result<draft::Config, String> {
        let stored = self.ai.lock().expect("ai settings").clone();
        let mut cfg = draft::Config::from_env_and(&stored)?;
        if stored.cache == Some(true) {
            cfg.cache.dir = self.data_dir.as_ref().map(|d| d.join("ai-cache"));
        }
        Ok(cfg)
    }
}

/// `GET /api/ai-settings` — the saved AI model config, redacted (the key is
/// only ever "set"/"not set").
pub(crate) async fn get_ai_settings(State(app): State<App>) -> Json<serde_json::Value> {
    let stored = app.ai.lock().expect("ai settings").clone();
    let view = ai_settings::view(&stored);
    Json(serde_json::json!({ "settings": view }))
}

/// `POST /api/ai-settings` — save the AI model config (Unsloth, Ollama, ...
/// any OpenAI-compatible endpoint). Fields left out keep their saved value.
/// Takes effect immediately: the next draft/refine/ask uses it.
#[derive(Deserialize, Default)]
pub(crate) struct AiSettingsBody {
    pub(crate) direct_url: Option<String>,
    pub(crate) direct_model: Option<String>,
    pub(crate) key: Option<String>,
    pub(crate) thinking: Option<bool>,
    pub(crate) cache: Option<bool>,
}

pub(crate) async fn post_ai_settings(
    State(app): State<App>,
    body: Result<Json<AiSettingsBody>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let mut stored = app.ai.lock().expect("ai settings").clone();
    let mut key_cleared = false;
    if let Some(u) = b.direct_url {
        let u = u.trim().to_string();
        if !u.is_empty() && !u.starts_with("http://") && !u.starts_with("https://") {
            return Err(ApiError(CoreError::Invalid(
                "the model address must start with http:// or https://".into(),
            )));
        }
        let new_url = (!u.is_empty()).then_some(u);
        // A new address must not inherit the old key: that would let a forged
        // request send the saved key to a host of the attacker's choosing.
        if new_url != stored.direct_url && b.key.is_none() && stored.key.is_some() {
            stored.key = None;
            key_cleared = true;
        }
        stored.direct_url = new_url;
    }
    if let Some(m) = b.direct_model {
        let m = m.trim().to_string();
        stored.direct_model = (!m.is_empty()).then_some(m);
    }
    if let Some(t) = b.thinking {
        stored.thinking = Some(t);
    }
    if let Some(c) = b.cache {
        stored.cache = Some(c);
    }
    if let Some(k) = b.key {
        let k = k.trim().to_string();
        stored.key = (!k.is_empty()).then_some(k);
    }
    ai_settings::save(app.data_dir.as_deref(), &stored)
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    *app.ai.lock().expect("ai settings") = stored.clone();
    let view = ai_settings::view(&stored);
    Ok(Json(
        serde_json::json!({ "ok": true, "settings": view, "status": ai_status_value(&app), "key_cleared": key_cleared }),
    ))
}

/// `GET /api/ai-models` — the endpoint's own model list (`GET {base}/models`,
/// the OpenAI-compatible shape Unsloth Studio, Ollama and friends serve), so
/// the settings dialog offers a dropdown instead of a typed guess.
#[cfg(not(feature = "bridge"))]
pub(crate) async fn ai_models(_app: State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
}

#[cfg(feature = "bridge")]
pub(crate) async fn ai_models(State(app): State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    let url = {
        let stored = app.ai.lock().expect("ai settings").clone();
        ai_settings::resolve_direct(
            std::env::var("ND_CARTO_LLM_URL")
                .ok()
                .filter(|v| !v.is_empty()),
            std::env::var("ND_CARTO_LLM_MODEL")
                .ok()
                .filter(|v| !v.is_empty()),
            &stored,
        )
        .map(|(u, _)| u)
        .ok_or_else(|| ApiError(CoreError::NotFound("no direct model configured".into())))?
    };
    let client = ndcart_bridge::plain_client();
    let base = url.trim_end_matches('/');
    let models_url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    let key = {
        let stored = app.ai.lock().expect("ai settings").clone();
        ai_settings::resolve_key(
            std::env::var("ND_CARTO_LLM_KEY")
                .ok()
                .filter(|v| !v.is_empty()),
            &stored,
        )
    };
    let mut req = client.get(&models_url);
    if let Some(k) = &key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.map_err(|e| {
        ApiError(CoreError::Invalid(format!(
            "cannot reach {models_url}: {e}"
        )))
    })?;
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ApiError(CoreError::Invalid(format!("bad model list: {e}"))))?;
    let list: Vec<String> = v
        .get("data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(Json(serde_json::json!({ "models": list })))
}

/// `POST /api/ai-test` — send one tiny request to the configured model and
/// report what came back, so the settings dialog can prove the connection.
#[cfg(not(feature = "bridge"))]
pub(crate) async fn ai_test(_app: State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    Err(ApiError(CoreError::NotFound(NO_BRIDGE.into())))
}

#[cfg(feature = "bridge")]
pub(crate) async fn ai_test(State(app): State<App>) -> Result<Json<serde_json::Value>, ApiError> {
    let cfg = app
        .resolve_ai()
        .map_err(|m| ApiError(CoreError::NotFound(m)))?;
    let backend = draft::Backend::connect(&cfg)
        .await
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    match backend
        .chat(
            "You are a connection test.",
            "Reply with the single word OK.",
        )
        .await
    {
        Ok(reply) => Ok(Json(serde_json::json!({
            "ok": true,
            "reply": reply.chars().take(120).collect::<String>(),
        }))),
        Err(e) => Ok(Json(serde_json::json!({ "ok": false, "error": e }))),
    }
}

/// `POST /api/docs/{id}/theme-pack` `{theme}` — plan 21.20: the model turns a
/// theme into a pack PLAN: 4-6 material prompts, 8-12 prop prompts with
/// names/tags/placement, and a palette. Returns the plan; the GM generates
/// the pictures with the existing art pipeline and drops them into the pack.
#[cfg(feature = "bridge")]
pub(crate) async fn theme_pack(
    State(app): State<App>,
    body: Result<Json<serde_json::Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Json(b) = body.map_err(|e| ApiError(CoreError::Invalid(format!("bad request: {e}"))))?;
    let theme = b
        .get("theme")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.chars().count() <= 200)
        .ok_or_else(|| ApiError(CoreError::Invalid("body needs {theme: 1-200 chars}".into())))?
        .to_string();
    let cfg = app
        .resolve_ai()
        .map_err(|m| ApiError(CoreError::NotFound(m)))?;
    let plan = draft::theme_pack(&cfg, &theme)
        .await
        .map_err(|m| ApiError(CoreError::Invalid(m)))?;
    Ok(Json(serde_json::json!({ "plan": plan })))
}

/// `GET /api/ai/status`/// `GET /api/ai/status`/// `GET /api/ai/status` — which AI the studio is wired to, for the banner:
/// `{mode: "nd-world" | "standalone" | "off", detail}`.
pub(crate) async fn ai_status(State(app): State<App>) -> Json<serde_json::Value> {
    Json(ai_status_value(&app))
}

#[cfg(feature = "bridge")]
pub(crate) fn ai_status_value(app: &App) -> serde_json::Value {
    match app.resolve_ai() {
        Ok(c) => serde_json::json!({
            "mode": if c.direct.is_some() { "standalone" } else { "nd-world" },
            "detail": c.describe(),
        }),
        Err(m) => serde_json::json!({ "mode": "off", "detail": m }),
    }
}

#[cfg(not(feature = "bridge"))]
pub(crate) fn ai_status_value(_app: &App) -> serde_json::Value {
    serde_json::json!({ "mode": "off", "detail": NO_BRIDGE })
}
