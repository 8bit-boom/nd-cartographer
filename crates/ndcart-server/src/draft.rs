//! Draft a MapSpec from a plain-language prompt, through nd-world's AI queue
//! (plan §8). The model is only ever asked for the room graph; the layout is
//! our own deterministic code, and a spec that breaks a rule is handed back to
//! the model with the exact complaint (twice at most) before giving up.
//!
//!   ndcart draft "abandoned metro station, 3 levels, flooded bottom"
//!                --server URL [--world SLUG] --email EMAIL [--model NAME] [--out FILE]
//!
//! The studio uses the same code for its "Draft with AI" button, configured by
//! `ND_CARTO_SERVER`, `ND_CARTO_EMAIL`, `ND_CARTO_PASSWORD[_FILE]` (never typed
//! into a prompt or an argument).

use crate::aipack::{self, Kind};
use crate::repair::{self, Failure, Job, Outcome, Progress, Turn};
use ndcart_bridge::Bridge;
use ndcart_core::mapspec::MapSpec;

pub use crate::speccheck::extract_json;

const NO_JSON: &str = "The reply contained no JSON object. Reply with only the JSON object.";

/// Judge one map reply: every complaint (JSON, unknown fields, rules, layout, lint errors).
pub fn parse_reply_all(reply: &str) -> Result<MapSpec, Vec<String>> {
    let json = extract_json(reply).ok_or_else(|| vec![NO_JSON.to_string()])?;
    let c = crate::speccheck::check_map(json);
    if !c.errors.is_empty() {
        return Err(c.errors);
    }
    c.spec
        .ok_or_else(|| vec!["The spec could not be read.".into()])
}

/// Parse and validate one reply; the first complaint (tests and the CLI check).
#[cfg(test)]
pub fn parse_reply(reply: &str) -> Result<MapSpec, String> {
    parse_reply_all(reply).map_err(|mut e| e.swap_remove(0))
}

/// A drafted map and what the linter noticed about it (shown beside the preview; none of it
/// stops the GM from accepting).
pub struct Drafted {
    pub spec: MapSpec,
    pub warnings: Vec<ndcart_core::speclint::SpecProblem>,
    /// How many asks it took (1 = right the first time).
    pub attempts: usize,
    pub seconds: f64,
}

/// The linter's non-error findings for a spec that passed [`parse_reply`].
pub fn warnings_of(spec: &MapSpec) -> Vec<ndcart_core::speclint::SpecProblem> {
    ndcart_core::speclint::lint_spec(spec, 1).unwrap_or_default()
}

/// Judge one world reply (every complaint goes back to the model).
pub fn parse_world_reply_all(reply: &str) -> Result<crate::worldspec::WorldSpec, Vec<String>> {
    let json = extract_json(reply).ok_or_else(|| vec![NO_JSON.to_string()])?;
    let c = crate::speccheck::check_world(json);
    if !c.errors.is_empty() {
        return Err(c.errors);
    }
    c.spec
        .ok_or_else(|| vec!["The spec could not be read.".into()])
}

/// Parse and validate one world reply (the first complaint).
#[cfg(test)]
pub fn parse_world_reply(reply: &str) -> Result<crate::worldspec::WorldSpec, String> {
    parse_world_reply_all(reply).map_err(|mut e| e.swap_remove(0))
}

/// Where the model lives: nd-world's AI queue, or a direct OpenAI-compatible endpoint.
pub struct Config {
    pub server: Option<String>,
    pub world: Option<String>,
    pub email: Option<String>,
    pub model: Option<String>,
    /// Standalone mode: `(base url, model)`; the key comes from
    /// `ND_CARTO_LLM_KEY[_FILE]` or the saved AI settings (env wins).
    pub direct: Option<(String, String)>,
    /// Bearer key for the direct endpoint (from the same two sources).
    pub key: Option<String>,
    /// Let reasoning models think on JSON tasks (default off).
    pub thinking: bool,
    /// Reuse accepted results (set when the settings turn the cache on).
    pub cache: crate::aicache::Use,
    /// Always build maps in steps (outline, then floors).
    pub stepwise: bool,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

impl Config {
    /// The studio's configuration: environment variables keep the final word
    /// per field (a direct model, `ND_CARTO_LLM_URL` + `ND_CARTO_LLM_MODEL`, wins over nd-world); the saved AI settings (Unsloth, Ollama, ...) fill the rest.
    pub fn from_env_and(stored: &crate::ai_settings::StoredAiSettings) -> Result<Config, String> {
        let direct = match (
            env("ND_CARTO_LLM_URL").or_else(|| stored.direct_url.clone()),
            env("ND_CARTO_LLM_MODEL").or_else(|| stored.direct_model.clone()),
        ) {
            (Some(u), Some(m)) => Some((u, m)),
            (Some(_), None) => {
                return Err("set the model name as well as the endpoint address".into())
            }
            _ => None,
        };
        let key = env("ND_CARTO_LLM_KEY").or_else(|| stored.key.clone());
        let cfg = Config {
            server: env("ND_CARTO_SERVER"),
            world: env("ND_CARTO_WORLD"),
            email: env("ND_CARTO_EMAIL"),
            model: env("ND_CARTO_MODEL"),
            direct,
            key,
            thinking: stored.thinking.unwrap_or(false),
            cache: Default::default(),
            stepwise: stored.stepwise.unwrap_or(false),
        };
        if cfg.direct.is_none() && (cfg.server.is_none() || cfg.email.is_none()) {
            return Err("to draft with AI set ND_CARTO_SERVER and ND_CARTO_EMAIL (nd-world), or ND_CARTO_LLM_URL and ND_CARTO_LLM_MODEL (a model you run yourself)".into());
        }
        Ok(cfg)
    }

    /// One line for the UI banner, or None when no AI is configured.
    pub fn describe(&self) -> String {
        match (&self.direct, &self.server) {
            (Some((u, m)), _) => format!("standalone: {u} ({m})"),
            (None, Some(s)) => format!("nd-world: {s}"),
            _ => "not configured".into(),
        }
    }
}

/// A connected model.
pub enum Backend {
    World(Bridge, Option<String>),
    Direct(ndcart_bridge::DirectChat),
}

impl Backend {
    pub async fn connect(cfg: &Config) -> Result<Backend, String> {
        if let Some((url, model)) = &cfg.direct {
            return Ok(Backend::Direct(
                ndcart_bridge::DirectChat::new(url, model, cfg.key.clone())
                    .map_err(|e| e.to_string())?
                    .with_thinking(cfg.thinking),
            ));
        }
        let (server, email) = (
            cfg.server.as_deref().ok_or("--server is required")?,
            cfg.email.as_deref().ok_or("--email is required")?,
        );
        let pw = crate::publish::password()?;
        let totp = std::env::var("ND_CARTO_TOTP").ok();
        let bridge = Bridge::new(server, cfg.world.as_deref()).map_err(|e| e.to_string())?;
        bridge
            .login(email, &pw, totp.as_deref())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Backend::World(bridge, cfg.model.clone()))
    }

    pub async fn chat(&self, system: &str, user: &str) -> Result<String, String> {
        match self {
            Backend::World(b, model) => b
                .chat(system, user, model.as_deref())
                .await
                .map_err(|e| e.to_string()),
            Backend::Direct(d) => d.chat(system, user).await.map_err(|e| e.to_string()),
        }
    }

    /// One request over the turns so far (the pack's system prompt first). A direct model
    /// gets the whole conversation; nd-world takes one user text, so the earlier turns are
    /// folded into it.
    pub async fn ask(
        &self,
        system: &str,
        turns: &[Turn],
        shape: Option<&Shape>,
    ) -> Result<String, String> {
        match self {
            Backend::World(b, model) => {
                // nd-world's queue takes no schema; the judge and the repair loop still apply.
                let user = match turns {
                    [only] => only.content.clone(),
                    [first, rest @ ..] => {
                        let mut u = first.content.clone();
                        for t in rest {
                            let label = if t.role == "assistant" {
                                "Your previous reply"
                            } else {
                                "Feedback"
                            };
                            u.push_str(&format!("\n\n{label}:\n{}", t.content));
                        }
                        u
                    }
                    [] => String::new(),
                };
                b.chat(system, &user, model.as_deref())
                    .await
                    .map_err(|e| e.to_string())
            }
            Backend::Direct(d) => {
                let mut msgs = vec![ndcart_bridge::Msg::system(system)];
                msgs.extend(
                    turns
                        .iter()
                        .map(|t| ndcart_bridge::Msg::new(t.role, &t.content)),
                );
                d.chat_structured(
                    &msgs,
                    shape.map(|s| (s.name, &s.schema)),
                    &ndcart_bridge::ChatOptions::default(),
                )
                .await
                .map(|r| r.content)
                .map_err(|e| e.to_string())
            }
        }
    }

    /// The world's location names for lore-grounded naming (nd-world only).
    pub async fn locations(&self) -> Result<Vec<String>, String> {
        match self {
            Backend::World(b, _) => b.location_names().await.map_err(|e| e.to_string()),
            Backend::Direct(_) => {
                Err("lore grounding needs nd-world (this is a standalone model)".into())
            }
        }
    }
}

/// What a draft may lean on besides the prompt.
#[derive(Default, Clone)]
pub struct Context {
    /// The project's look: "gritty cyberpunk, neon and rust".
    pub tone: Option<String>,
    /// Location names to reuse where they fit.
    pub locations: Vec<String>,
    /// Fetch the world's location names from nd-world before asking.
    pub lore: bool,
}

impl Context {
    /// Fill in the lore if it was asked for (a failure is reported, not ignored).
    async fn with_lore(&self, backend: &Backend) -> Result<Context, String> {
        let mut c = self.clone();
        if c.lore {
            c.locations = backend.locations().await?;
        }
        Ok(c)
    }
}

/// The system prompt with the project's style and the world's places added.
pub fn with_context(base: &str, ctx: &Context) -> String {
    let mut out = base.to_string();
    if let Some(t) = ctx.tone.as_deref().filter(|t| !t.trim().is_empty()) {
        out.push_str(&format!(
            "\n\nHouse style for this project (let it shape names and feel): {}",
            t.trim()
        ));
    }
    if !ctx.locations.is_empty() {
        let names: Vec<&str> = ctx.locations.iter().take(60).map(String::as_str).collect();
        out.push_str(&format!(
            "\n\nPlaces that already exist in this world. Reuse these names where they genuinely fit, and invent new names in the same voice for the rest: {}",
            names.join("; ")
        ));
    }
    out
}

fn too_long(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() || prompt.chars().count() > 2_000 {
        return Err("describe the map in 1-2000 characters".into());
    }
    Ok(())
}

/// A JSON schema a structured reply must follow, and the name the API gives it.
pub struct Shape {
    pub name: &'static str,
    pub schema: serde_json::Value,
}

fn from_pack(text: &str) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_str(text).expect("pack schema is JSON");
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
    }
    v
}

/// The schema sent with the request of `kind` (item 2.1's for maps and worlds, small ones for
/// tags and theme packs). Servers that enforce it make an invented field impossible; the judge
/// still checks everything a schema cannot say.
pub fn shape(kind: Kind) -> &'static Shape {
    use std::sync::LazyLock;
    static SHAPES: LazyLock<[Shape; 5]> = LazyLock::new(|| {
        let hex = serde_json::json!({"type": "string", "pattern": "^#[0-9a-fA-F]{6}$"});
        let named = |placed: bool| {
            let mut props = serde_json::json!({
                "name": {"type": "string"},
                "prompt": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 4},
            });
            let mut req = vec!["name", "prompt", "tags"];
            if placed {
                props["placement"] =
                    serde_json::json!({"type": "string", "enum": ["floor", "wall"]});
                req.push("placement");
            }
            serde_json::json!({"type": "object", "properties": props, "required": req, "additionalProperties": false})
        };
        [
            Shape {
                name: "map_spec",
                schema: from_pack(aipack::MAPSPEC_SCHEMA),
            },
            Shape {
                name: "world_spec",
                schema: from_pack(aipack::WORLDSPEC_SCHEMA),
            },
            Shape {
                name: "tags",
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {"tags": {"type": "array", "items": {"type": "string", "maxLength": 24}, "minItems": 2, "maxItems": 8}},
                    "required": ["tags"],
                    "additionalProperties": false,
                }),
            },
            Shape {
                name: "outline",
                schema: from_pack(aipack::OUTLINE_SCHEMA),
            },
            Shape {
                name: "theme_pack",
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "maxLength": 60},
                        "style": {"type": "string", "maxLength": 120},
                        "palette": {"type": "array", "items": hex, "minItems": 4, "maxItems": 8},
                        "materials": {"type": "array", "items": named(false), "minItems": 4, "maxItems": 6},
                        "props": {"type": "array", "items": named(true), "minItems": 6, "maxItems": 12},
                    },
                    "required": ["name", "style", "palette", "materials", "props"],
                    "additionalProperties": false,
                }),
            },
        ]
    });
    match kind {
        Kind::Map | Kind::Refine => &SHAPES[0],
        Kind::World => &SHAPES[1],
        Kind::Tags => &SHAPES[2],
        Kind::Outline => &SHAPES[3],
        Kind::ThemePack => &SHAPES[4],
    }
}

/// Run the shared repair loop against a connected model.
pub(crate) struct Task<'a> {
    pub(crate) system: &'a str,
    pub(crate) kind: Kind,
    pub(crate) first_user: String,
    pub(crate) rounds: usize,
    pub(crate) what: &'a str,
    pub(crate) cache: &'a crate::aicache::Use,
}

pub(crate) async fn solve<T: serde::Serialize + serde::de::DeserializeOwned>(
    backend: &Backend,
    task: Task<'_>,
    progress: &(dyn Fn(Progress) + Sync),
    judge: impl Fn(&str) -> Result<T, Vec<String>>,
) -> Result<Outcome<T>, Failure> {
    let Task {
        system,
        kind,
        first_user,
        rounds,
        what,
        cache,
    } = task;
    // Only a direct model is cached: its id is known, and its answers are what we pay for.
    let cache_key = match (&cache.dir, backend) {
        (Some(_), Backend::Direct(d)) => {
            let schema = match d.caps().await.json_schema {
                Some(true) => "schema:yes",
                Some(false) => "schema:no",
                None => "schema:unknown",
            };
            Some(crate::aicache::key(&[
                d.model(),
                aipack::version(),
                system,
                &first_user,
                shape(kind).name,
                &rounds.to_string(),
                schema,
            ]))
        }
        _ => None,
    };
    if let (Some(dir), Some(k), false) = (&cache.dir, &cache_key, cache.skip_read) {
        if let Some(hit) = crate::aicache::get(dir, k) {
            if let Ok(value) = serde_json::from_value::<T>(hit) {
                return Ok(Outcome {
                    value,
                    attempts: 0,
                    seconds: 0.0,
                });
            }
        }
    }
    let job = Job {
        first_user,
        rounds,
        what,
    };
    let out = repair::run(
        &job,
        progress,
        |turns| async move { backend.ask(system, &turns, Some(shape(kind))).await },
        judge,
    )
    .await?;
    if let (Some(dir), Some(k)) = (&cache.dir, &cache_key) {
        if let Ok(v) = serde_json::to_value(&out.value) {
            crate::aicache::put(dir, k, &v);
        }
    }
    Ok(out)
}

/// Draft a battle-map spec from `prompt`.
pub async fn draft(cfg: &Config, prompt: &str, ctx: &Context) -> Result<Drafted, String> {
    draft_with_progress(cfg, prompt, ctx, &|_| {}).await
}

/// [`draft`], reporting where the repair loop is.
pub async fn draft_with_progress(
    cfg: &Config,
    prompt: &str,
    ctx: &Context,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<Drafted, String> {
    too_long(prompt)?;
    let backend = Backend::connect(cfg).await?;
    let ctx = ctx.with_lore(&backend).await?;
    let system = with_context(aipack::system(Kind::Map), &ctx);
    // Several floors, or the setting: plan first, then draw each floor on its own.
    if cfg.stepwise || ndcart_core::template::floors_asked(prompt) > 1 {
        let (spec, attempts, seconds) =
            crate::stepwise::draft(cfg, &backend, &system, prompt, progress).await?;
        let warnings = warnings_of(&spec);
        return Ok(Drafted {
            spec,
            warnings,
            attempts,
            seconds,
        });
    }
    let one_shot = solve(
        &backend,
        Task {
            cache: &cfg.cache,
            system: &system,
            kind: Kind::Map,
            first_user: format!("Design this map: {}", prompt.trim()),
            rounds: 2,
            what: "spec",
        },
        progress,
        parse_reply_all,
    )
    .await;
    let out = match one_shot {
        Ok(o) => o,
        // Too big to lay out in one go is what a plan-then-floors draft is for.
        Err(f) if size_related(&f) => {
            let (spec, attempts, seconds) =
                crate::stepwise::draft(cfg, &backend, &system, prompt, progress).await?;
            let warnings = warnings_of(&spec);
            return Ok(Drafted {
                spec,
                warnings,
                attempts: attempts + f.attempts,
                seconds,
            });
        }
        Err(f) => return Err(f.message),
    };
    let warnings = warnings_of(&out.value);
    Ok(Drafted {
        spec: out.value,
        warnings,
        attempts: out.attempts,
        seconds: out.seconds,
    })
}

/// Did the repair loop give up on a size problem (too many rooms, would not fit)?
fn size_related(f: &Failure) -> bool {
    f.complaints.iter().any(|c| {
        let c = c.to_lowercase();
        c.contains("could not fit")
            || c.contains("too large")
            || c.contains("make the map smaller")
            || c.contains("rooms") && (c.contains("needs 1-") || c.contains("1-60"))
    })
}

/// Draft a WORLD spec from `prompt`.
pub async fn draft_world(
    cfg: &Config,
    prompt: &str,
    ctx: &Context,
) -> Result<crate::worldspec::WorldSpec, String> {
    Ok(draft_world_with(cfg, prompt, ctx, &|_| {}).await?.0)
}

/// [`draft_world`], reporting progress and how many asks it took.
pub async fn draft_world_with(
    cfg: &Config,
    prompt: &str,
    ctx: &Context,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<(crate::worldspec::WorldSpec, usize), String> {
    too_long(prompt)?;
    let backend = Backend::connect(cfg).await?;
    let system = with_context(aipack::system(Kind::World), &ctx.with_lore(&backend).await?);
    let out = solve(
        &backend,
        Task {
            cache: &cfg.cache,
            system: &system,
            kind: Kind::World,
            first_user: format!("Design this map: {}", prompt.trim()),
            rounds: 2,
            what: "spec",
        },
        progress,
        parse_world_reply_all,
    )
    .await?;
    Ok((out.value, out.attempts))
}

/// The user turn for a refinement: the current floor, the pins, the request.
pub fn refine_user_turn(
    current: &ndcart_core::mapspec::FloorSpec,
    pins: &[String],
    instruction: &str,
) -> Result<String, String> {
    let json = serde_json::to_string(&MapSpec {
        name: current.name.clone(),
        floors: vec![current.clone()],
    })
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "Current map:\n{json}\n\nPinned room ids (keep exactly): {}\n\nChange request: {}",
        if pins.is_empty() {
            "none".into()
        } else {
            pins.join(", ")
        },
        instruction.trim()
    ))
}

/// A refined reply is valid when it is a valid one-floor spec that still holds every pinned room unchanged.
pub fn judge_refine(
    current: &ndcart_core::mapspec::FloorSpec,
    pins: Vec<String>,
) -> impl Fn(&str) -> Result<MapSpec, Vec<String>> {
    let current = current.clone();
    move |reply: &str| {
        let spec = parse_reply_all(reply)?;
        if spec.floors.len() != 1 {
            return Err(vec!["The spec must have exactly one floor.".into()]);
        }
        let mut complaints = Vec::new();
        for id in &pins {
            let before = current.rooms.iter().find(|r| &r.id == id);
            let after = spec.floors[0].rooms.iter().find(|r| &r.id == id);
            match (before, after) {
                (Some(b), Some(a)) if a.name == b.name && a.w == b.w && a.h == b.h && a.kind == b.kind => {}
                (Some(_), Some(_)) => complaints.push(format!("Pinned room '{id}' was changed; pinned rooms must keep their id, name, w, h and kind.")),
                (Some(_), None) => complaints.push(format!("Pinned room '{id}' was dropped; pinned rooms must stay.")),
                (None, _) => complaints.push(format!("'{id}' is not a room of the current map and cannot be pinned.")),
            }
        }
        if complaints.is_empty() {
            Ok(spec)
        } else {
            Err(complaints)
        }
    }
}

/// Revise `current` per `instruction`, keeping the pinned rooms.
pub async fn refine(
    cfg: &Config,
    current: &ndcart_core::mapspec::FloorSpec,
    pins: &[String],
    instruction: &str,
    ctx: &Context,
) -> Result<MapSpec, String> {
    too_long(instruction)?;
    let backend = Backend::connect(cfg).await?;
    let system = with_context(
        aipack::system(Kind::Refine),
        &ctx.with_lore(&backend).await?,
    );
    let first = refine_user_turn(current, pins, instruction)?;
    let judge = judge_refine(current, pins.to_vec());
    Ok(solve(
        &backend,
        Task {
            cache: &cfg.cache,
            system: &system,
            kind: Kind::Refine,
            first_user: first,
            rounds: 3,
            what: "revision",
        },
        &|_| {},
        judge,
    )
    .await?
    .value)
}

/// Clean a list of tags: lowercase, 1-24 characters of letters, digits, spaces and
/// hyphens, no duplicates, at most 8. Anything else is dropped.
pub fn clean_tags(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim().to_lowercase();
        let ok = !t.is_empty()
            && t.chars().count() <= 24
            && t.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-');
        if ok && !out.contains(&t) {
            out.push(t);
        }
        if out.len() == 8 {
            break;
        }
    }
    out
}

fn judge_tags(reply: &str) -> Result<Vec<String>, Vec<String>> {
    judge_tags_one(reply).map_err(|e| vec![e])
}

fn judge_tags_one(reply: &str) -> Result<Vec<String>, String> {
    let json = extract_json(reply)
        .ok_or("The reply contained no JSON object. Reply with only the JSON object.")?;
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("That was not valid JSON: {e}."))?;
    let raw: Vec<String> = v
        .get("tags")
        .and_then(|t| t.as_array())
        .ok_or("The object needs a \"tags\" array of strings.")?
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect();
    let tags = clean_tags(&raw);
    if tags.len() < 2 {
        return Err(
            "Give at least 2 usable tags (1-24 characters: letters, digits, spaces, hyphens)."
                .into(),
        );
    }
    Ok(tags)
}

/// Suggest tags for a prop from its name and the prompt that made it. The GM
/// confirms them; nothing is saved here.
pub async fn suggest_tags(cfg: &Config, name: &str, prompt: &str) -> Result<Vec<String>, String> {
    let backend = Backend::connect(cfg).await?;
    let subject = format!(
        "Prop name: {name}\nHow it was described: {}",
        if prompt.is_empty() {
            "(not recorded)"
        } else {
            prompt
        }
    );
    Ok(solve(
        &backend,
        Task {
            cache: &cfg.cache,
            system: aipack::system(Kind::Tags),
            kind: Kind::Tags,
            first_user: subject,
            rounds: 1,
            what: "tag list",
        },
        &|_| {},
        judge_tags,
    )
    .await?
    .value)
}

fn hex_colour(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|h| h.is_ascii_hexdigit())
}

/// A theme-pack plan is good when it has a name, a style, 4-8 hex colours, 4-6 materials and
/// 6-12 props, each with a prompt, 1-4 tags, and (props) a placement.
pub fn judge_theme_pack(reply: &str) -> Result<serde_json::Value, Vec<String>> {
    let json = extract_json(reply).ok_or_else(|| vec![NO_JSON.to_string()])?;
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| vec![format!("That was not valid JSON: {e}.")])?;
    let mut bad: Vec<String> = Vec::new();
    let text = |k: &str, max: usize| {
        v.get(k)
            .and_then(|x| x.as_str())
            .is_some_and(|t| !t.trim().is_empty() && t.chars().count() <= max)
    };
    if !text("name", 60) {
        bad.push("\"name\" must be a string of 1-60 characters.".into());
    }
    if !text("style", 120) {
        bad.push("\"style\" must be a string of 1-120 characters.".into());
    }
    match v.get("palette").and_then(|p| p.as_array()) {
        Some(p) if (4..=8).contains(&p.len()) => {
            for c in p {
                if !c.as_str().is_some_and(hex_colour) {
                    bad.push(format!("palette entry {c} must be a \"#rrggbb\" colour."));
                }
            }
        }
        _ => bad.push("\"palette\" must be an array of 4-8 \"#rrggbb\" colours.".into()),
    }
    for (key, lo, hi, placed) in [("materials", 4, 6, false), ("props", 6, 12, true)] {
        match v.get(key).and_then(|m| m.as_array()) {
            Some(items) if (lo..=hi).contains(&items.len()) => {
                for (i, it) in items.iter().enumerate() {
                    let s = |k: &str| {
                        it.get(k)
                            .and_then(|x| x.as_str())
                            .is_some_and(|t| !t.trim().is_empty())
                    };
                    let tags = it
                        .get("tags")
                        .and_then(|t| t.as_array())
                        .map_or(0, |t| t.len());
                    if !s("name") || !s("prompt") || !(1..=4).contains(&tags) {
                        bad.push(format!("{key}[{i}] needs a name, a prompt and 1-4 tags."));
                    }
                    if placed
                        && !matches!(
                            it.get("placement").and_then(|p| p.as_str()),
                            Some("floor" | "wall")
                        )
                    {
                        bad.push(format!(
                            "{key}[{i}].placement must be \"floor\" or \"wall\"."
                        ));
                    }
                }
            }
            _ => bad.push(format!("\"{key}\" must be an array of {lo}-{hi} items.")),
        }
    }
    if bad.is_empty() {
        Ok(v)
    } else {
        Err(bad)
    }
}

/// Turn a theme into an art-pack plan (the GM makes the pictures).
pub async fn theme_pack(cfg: &Config, theme: &str) -> Result<serde_json::Value, String> {
    let backend = Backend::connect(cfg).await?;
    Ok(solve(
        &backend,
        Task {
            cache: &cfg.cache,
            system: aipack::system(Kind::ThemePack),
            kind: Kind::ThemePack,
            first_user: format!("Theme: {theme}"),
            rounds: 2,
            what: "plan",
        },
        &|_| {},
        judge_theme_pack,
    )
    .await?
    .value)
}

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let (mut prompt, mut server, mut world, mut email, mut model, mut out) =
        (None, None, None, None, None, None);
    let (mut llm_url, mut llm_model) = (None, None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
        match a.as_str() {
            "--server" => server = Some(val("--server")?),
            "--world" => world = Some(val("--world")?),
            "--email" => email = Some(val("--email")?),
            "--model" => model = Some(val("--model")?),
            "--llm-url" => llm_url = Some(val("--llm-url")?),
            "--llm-model" => llm_model = Some(val("--llm-model")?),
            "--out" => out = Some(val("--out")?),
            o if !o.starts_with("--") && prompt.is_none() => prompt = Some(o.to_string()),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let direct = match (llm_url, llm_model) {
        (Some(u), Some(m)) => Some((u, m)),
        (None, None) => None,
        _ => return Err("--llm-url and --llm-model go together".into()),
    };
    if direct.is_none() && (server.is_none() || email.is_none()) {
        return Err("use --server and --email (nd-world), or --llm-url and --llm-model (a model you run yourself)".into());
    }
    let key = std::env::var("ND_CARTO_LLM_KEY").ok().or_else(|| {
        std::env::var("ND_CARTO_LLM_KEY_FILE")
            .ok()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|k| k.trim().to_string())
    });
    let cfg = Config {
        server,
        world,
        email,
        model,
        direct,
        key,
        thinking: false,
        cache: Default::default(),
        stepwise: false,
    };
    let spec = draft(
        &cfg,
        &prompt.ok_or("describe the map, in quotes")?,
        &Context::default(),
    )
    .await?;
    for w in &spec.warnings {
        eprintln!("note ({}): {}", w.floor, w.message);
    }
    let text = serde_json::to_string_pretty(&spec.spec).map_err(|e| e.to_string())?;
    match out {
        Some(f) => {
            std::fs::write(&f, text).map_err(|e| format!("cannot write {f}: {e}"))?;
            println!("wrote {f}; paste it into the studio's \u{2728} From a spec dialog");
        }
        None => println!("{text}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"name":"Crypt","floors":[{"name":"G","rooms":[{"id":"a","name":"Nave","w":8,"h":6},{"id":"b","name":"Ossuary","w":5,"h":5}],"links":[{"a":"a","b":"b","kind":"secret"}]}]}"#;

    /// Drive the repair loop with canned replies (the last repeats); returns the result and
    /// the turns each ask carried.
    async fn drive<T>(
        first: &str,
        rounds: usize,
        replies: &[&str],
        judge: impl Fn(&str) -> Result<T, Vec<String>>,
    ) -> (Result<Outcome<T>, Failure>, Vec<Vec<Turn>>) {
        let seen = std::sync::Mutex::new(Vec::new());
        let mut i = 0;
        let job = Job {
            first_user: first.into(),
            rounds,
            what: "spec",
        };
        let out = repair::run(
            &job,
            &|_| {},
            |turns| {
                seen.lock().unwrap().push(turns);
                let r = replies[i.min(replies.len() - 1)].to_string();
                i += 1;
                async move { Ok(r) }
            },
            judge,
        )
        .await;
        (out, seen.into_inner().unwrap())
    }

    fn last_user(turns: &[Turn]) -> &str {
        &turns.last().unwrap().content
    }

    #[test]
    fn json_is_found_through_fences_and_chatter_and_braces_in_strings() {
        assert_eq!(
            extract_json("Sure!\n```json\n{\"a\":1}\n```\nEnjoy"),
            Some("{\"a\":1}")
        );
        assert_eq!(
            extract_json(r#"x {"n":"a } b","m":{"k":1}} tail"#),
            Some(r#"{"n":"a } b","m":{"k":1}}"#)
        );
        assert_eq!(extract_json("no braces"), None);
        assert_eq!(extract_json("{\"open\":"), None);
    }

    #[test]
    fn replies_are_judged_on_schema_rules_and_layout() {
        assert!(parse_reply(&format!("```{GOOD}```")).is_ok());
        assert!(parse_reply("I cannot").unwrap_err().contains("no JSON"));
        assert!(parse_reply(r#"{"name":1}"#).unwrap_err().contains("schema"));
        let unlinked = GOOD.replace(r#","links":[{"a":"a","b":"b","kind":"secret"}]"#, "");
        assert!(parse_reply(&unlinked).unwrap_err().contains("not linked"));
        let huge = GOOD.replace(r#""w":8"#, r#""w":80"#);
        assert!(parse_reply(&huge).unwrap_err().contains("2-30"));
    }

    #[test]
    fn linter_warnings_ride_along_with_a_valid_reply() {
        // Warnings are findings, not refusals: GOOD has a secret-only room and still parses.
        let spec = parse_reply(GOOD).unwrap();
        let w = warnings_of(&spec);
        assert!(w.iter().any(|p| p.code == "secret_only_room"), "{w:?}");
        assert!(w.iter().all(|p| p.severity != ndcart_core::Severity::Error));
    }

    #[tokio::test]
    async fn an_invented_field_is_a_repair_round_not_silently_dropped() {
        let with_props = GOOD.replace(
            r#""id":"b","name":"Ossuary""#,
            r#""id":"b","props":["bones"],"name":"Ossuary""#,
        );
        let e = parse_reply(&with_props).unwrap_err();
        assert!(e.contains("floors[0].rooms[1].props"), "{e}");
        assert!(
            e.contains("Allowed in a room: id, name, kind, w, h, lit"),
            "{e}"
        );
        let (out, seen) = drive("a crypt", 2, &[&with_props, GOOD], parse_reply_all).await;
        assert_eq!(out.unwrap().value.name, "Crypt");
        assert_eq!(seen.len(), 2);
        assert!(
            last_user(&seen[1]).contains("Unknown field(s)"),
            "{:?}",
            seen[1]
        );
        let w =
            parse_world_reply(r#"{"name":"X","places":[{"name":"P","x":0.5,"y":0.5,"icon":"s"}]}"#)
                .unwrap_err();
        assert!(w.contains("places[0].icon"), "{w}");
    }

    #[tokio::test]
    async fn a_refused_reply_is_sent_back_with_the_reason_and_the_next_try_can_succeed() {
        let bad_kind = GOOD.replace(r#""kind":"secret""#, r#""kind":"warp""#);
        let (out, seen) = drive(
            "Design this map: a crypt",
            2,
            &["not json at all", &bad_kind, GOOD],
            parse_reply_all,
        )
        .await;
        let out = out.unwrap();
        assert_eq!((out.value.name.as_str(), out.attempts), ("Crypt", 3));
        assert!(seen[0][0].content.contains("a crypt") && seen[0].len() == 1);
        assert!(last_user(&seen[1]).contains("refused") && last_user(&seen[1]).contains("no JSON"));
        assert!(last_user(&seen[2]).contains("schema"), "{:?}", seen[2]);
        // The conversation grows: first request, each refused reply, each complaint.
        assert_eq!(seen[2].len(), 5);
        assert_eq!(seen[2][3].content, bad_kind);
    }

    #[tokio::test]
    async fn a_world_reply_is_judged_like_a_map_reply_and_repaired_the_same_way() {
        let good = r#"{"name":"Isles","cols":64,"rows":48,"land":0.4,"places":[{"name":"Port","x":0.5,"y":0.5}]}"#;
        assert!(parse_world_reply(&format!("```{good}```")).is_ok());
        assert!(parse_world_reply(r#"{"name":"X","land":3}"#)
            .unwrap_err()
            .contains("land must be"));
        assert!(parse_world_reply(
            r#"{"name":"X","biomes":[{"biome":"lava","x":0.5,"y":0.5,"rx":0.1,"ry":0.1}]}"#
        )
        .unwrap_err()
        .contains("use one of"));
        let (out, seen) = drive(
            "an archipelago",
            2,
            &[r#"{"name":"X","land":3}"#, good],
            parse_world_reply_all,
        )
        .await;
        assert_eq!(out.unwrap().value.name, "Isles");
        assert!(
            last_user(&seen[1]).contains("land must be"),
            "{:?}",
            seen[1]
        );
        let world_prompt = aipack::system(Kind::World);
        assert!(world_prompt.contains("biomes") && world_prompt.contains("0..1"));
    }

    #[test]
    fn style_and_lore_are_added_to_the_system_prompt_only_when_present() {
        let system_prompt = aipack::system(Kind::Map);
        assert_eq!(
            with_context(system_prompt, &Context::default()),
            system_prompt
        );
        let ctx = Context {
            tone: Some("gritty cyberpunk".into()),
            locations: vec!["Port Neon".into(), "The Drowned Abbey".into()],
            lore: false,
        };
        let s = with_context(system_prompt, &ctx);
        assert!(
            s.contains("gritty cyberpunk")
                && s.contains("Port Neon; The Drowned Abbey")
                && s.starts_with(system_prompt)
        );
    }

    #[test]
    fn a_refinement_must_keep_its_pinned_rooms_exactly() {
        let current = parse_reply(GOOD).unwrap().floors.remove(0);
        let turn = refine_user_turn(&current, &["a".into()], "add a barracks").unwrap();
        assert!(
            turn.contains("Pinned room ids (keep exactly): a")
                && turn.contains("add a barracks")
                && turn.contains("Nave")
        );
        let judge = judge_refine(&current, vec!["a".into()]);
        let with_barracks = GOOD
            .replace(
                r#"{"id":"b","#,
                r#"{"id":"c","name":"Barracks","w":6,"h":4},{"id":"b","#,
            )
            .replace(
                r#""links":[{"a":"a","b":"b","kind":"secret"}]"#,
                r#""links":[{"a":"a","b":"b","kind":"secret"},{"a":"a","b":"c"}]"#,
            );
        assert!(
            judge(&with_barracks).is_ok(),
            "{:?}",
            judge(&with_barracks).err()
        );
        // Changing a pinned room, dropping it, or pinning a stranger is refused with the reason.
        assert!(judge(&GOOD.replace(r#""w":8"#, r#""w":9"#))
            .unwrap_err()
            .join(" ")
            .contains("was changed"));
        assert!(judge(&GOOD.replace("Nave", "Chapel"))
            .unwrap_err()
            .join(" ")
            .contains("was changed"));
        let no_a = r#"{"name":"Crypt","floors":[{"name":"G","rooms":[{"id":"b","name":"Ossuary","w":5,"h":5}],"links":[]}]}"#;
        assert!(judge(no_a).unwrap_err().join(" ").contains("was dropped"));
        assert!(judge_refine(&current, vec!["zzz".into()])(GOOD)
            .unwrap_err()
            .join(" ")
            .contains("not a room"));
        let two = r#"{"name":"C","floors":[{"name":"G","rooms":[{"id":"a","name":"Nave","w":8,"h":6}],"links":[]},{"name":"H","rooms":[{"id":"a","name":"Nave","w":8,"h":6}],"links":[]}]}"#;
        assert!(judge(two)
            .unwrap_err()
            .join(" ")
            .contains("exactly one floor"));
    }

    #[tokio::test]
    async fn suggested_tags_are_cleaned_and_a_bad_reply_is_repaired() {
        assert_eq!(
            clean_tags(&[
                "Crate".into(),
                " crate ".into(),
                "WOODEN".into(),
                "bad_tag!".into(),
                "".into(),
                "x".repeat(30)
            ]),
            ["crate", "wooden"]
        );
        assert_eq!(
            clean_tags(&(0..20).map(|i| format!("t{i}")).collect::<Vec<_>>()).len(),
            8
        );
        assert_eq!(
            judge_tags_one("```{\"tags\":[\"Crate\",\"wooden\",\"storage\"]}```").unwrap(),
            ["crate", "wooden", "storage"]
        );
        assert!(judge_tags_one("{\"tags\":[\"a!\"]}")
            .unwrap_err()
            .contains("at least 2"));
        assert!(judge_tags_one("{\"nope\":1}").unwrap_err().contains("tags"));
        let (out, seen) = drive(
            "Prop name: Crate",
            1,
            &["tags: crate, wooden", "{\"tags\":[\"crate\",\"cargo\"]}"],
            judge_tags,
        )
        .await;
        assert_eq!(out.unwrap().value, ["crate", "cargo"]);
        // The first turn is the subject itself (it used to say "Design this map:").
        assert_eq!(seen[0][0].content, "Prop name: Crate");
        assert!(aipack::system(Kind::Tags).contains("\"tags\""));
    }

    #[tokio::test]
    async fn a_theme_pack_plan_is_judged_and_a_bad_palette_is_repaired_once() {
        let good = |palette: &str| {
            format!(
                r#"{{"name":"Ice","style":"cold","palette":{palette},"materials":[{m},{m},{m},{m}],"props":[{p},{p},{p},{p},{p},{p}]}}"#,
                m = r#"{"name":"Snow","prompt":"snow, top-down","tags":["snow"]}"#,
                p = r#"{"name":"Sled","prompt":"a sled","tags":["sled"],"placement":"floor"}"#,
            )
        };
        let ok = good(r##"["#112233","#445566","#778899","#aabbcc"]"##);
        let bad = good(r##"["blue","#445566","#778899"]"##);
        let e = judge_theme_pack(&bad).unwrap_err();
        assert!(e.iter().any(|c| c.contains("palette")), "{e:?}");
        let (out, seen) = drive("Theme: ice", 2, &[&bad, &ok], judge_theme_pack).await;
        assert_eq!(out.unwrap().attempts, 2);
        assert!(last_user(&seen[1]).contains("palette"));
        assert!(judge_theme_pack(&ok.replace("floor", "ceiling")).is_err());
    }

    #[tokio::test]
    async fn it_gives_up_after_the_repairs_and_passes_a_backend_error_straight_through() {
        let (e, _) = drive("x", 1, &["nope"], parse_reply_all).await;
        let e = e.unwrap_err();
        assert!(e.message.contains("2 tries"), "{e}");
        let job = Job {
            first_user: "x".into(),
            rounds: 2,
            what: "spec",
        };
        let e = repair::run(
            &job,
            &|_| {},
            |_| async { Err::<String, _>("nd-world refused it".to_string()) },
            parse_reply_all,
        )
        .await
        .unwrap_err();
        assert_eq!(e.message, "nd-world refused it");
        let system_prompt = aipack::system(Kind::Map);
        assert!(system_prompt.contains("\"links\"") && system_prompt.contains("reachable"));
    }
}
