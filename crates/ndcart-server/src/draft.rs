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

use ndcart_bridge::Bridge;
use ndcart_core::mapspec::MapSpec;
use std::future::Future;

pub const SYSTEM_PROMPT: &str = r#"You design tabletop battle maps as a room graph. Reply with ONE JSON object and nothing else (no prose, no code fences).

Schema:
{
  "name": string (1-80 chars),
  "floors": [                       // 1-12 floors
    {
      "name": string (unique, 1-40 chars),
      "rooms": [                    // 1-60 rooms per floor
        {"id": string (a-z 0-9 _ -), "name": string, "w": 2-30, "h": 2-30, "kind": "room"|"corridor"|"area", "lit": true|false}
      ],
      "links": [                    // how rooms connect; EVERY room must be reachable from the first room
        {"a": room id, "b": room id, "kind": "door"|"opening"|"secret"}
      ],
      "stairs": [                   // optional, to ANOTHER floor
        {"room": room id, "to_floor": floor name, "to_room": room id on that floor, "label": string}
      ]
    }
  ]
}

Rules: sizes are in squares (a normal room is 4-10). Give rooms evocative names. Use "secret" sparingly. Do not invent fields. Do not describe positions; the layout is computed for you."#;

/// The first JSON object in `text`, tolerating code fences and chatter around it.
pub fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let (mut in_str, mut esc) = (false, false);
    for (i, ch) in text[start..].char_indices() {
        if in_str {
            match (esc, ch) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse and validate one reply. The error text is what is sent back to the model.
pub fn parse_reply(reply: &str) -> Result<MapSpec, String> {
    let json = extract_json(reply)
        .ok_or("The reply contained no JSON object. Reply with only the JSON object.")?;
    let spec: MapSpec = serde_json::from_str(json)
        .map_err(|e| format!("The JSON did not match the schema: {e}."))?;
    spec.validate()
        .map_err(|e| format!("The spec broke a rule: {e}."))?;
    // Laying it out is the last check: it may not fit.
    ndcart_core::mapspec::compile(&spec, 1)
        .map_err(|e| format!("The spec could not be laid out: {e}."))?;
    Ok(spec)
}

/// Ask until a valid spec comes back (the first try plus `repairs` corrections).
pub async fn draft_with<F, Fut>(prompt: &str, repairs: usize, ask: F) -> Result<MapSpec, String>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    draft_loop(prompt, repairs, ask, parse_reply).await
}

/// The ask/judge/repair loop, for any kind of spec.
pub async fn draft_loop<T, F, Fut>(
    prompt: &str,
    repairs: usize,
    mut ask: F,
    judge: impl Fn(&str) -> Result<T, String>,
) -> Result<T, String>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let mut user = format!("Design this map: {}", prompt.trim());
    let mut last = String::new();
    for _ in 0..=repairs {
        let reply = ask(user.clone()).await?;
        match judge(&reply) {
            Ok(spec) => return Ok(spec),
            Err(problem) => {
                user = format!(
                    "Design this map: {}\n\nYour previous reply was refused. {problem}\nReply again with the corrected JSON object only.",
                    prompt.trim()
                );
                last = problem;
            }
        }
    }
    Err(format!(
        "the model did not produce a usable spec after {} tries; last problem: {last}",
        repairs + 1
    ))
}

pub const WORLD_SYSTEM_PROMPT: &str = r##"You design overland fantasy/sci-fi maps. Reply with ONE JSON object and nothing else (no prose, no code fences).

Schema (every coordinate is 0..1 across the map: x left to right, y top to bottom):
{
  "name": string (1-80 chars),
  "cols": 16-512 (default 160), "rows": 16-512 (default 110),
  "land": 0.05-0.95        // fraction of the map that is dry land
  "continent": 0-1         // 0 = scattered islands, 1 = one central continent
  "ridges": 0-1            // mountain chains
  "rivers": 0-40,
  "biomes": [ {"biome": "forest"|"meadow"|"rocky"|"swamp"|"desert", "x": 0-1, "y": 0-1, "rx": 0.02-0.5, "ry": 0.02-0.5, "density": 0.5-4 (higher = sparser)} ],
  "places": [ {"name": string, "note": string, "x": 0-1, "y": 0-1} ],     // cities, ruins, landmarks
  "regions": [ {"name": string, "points": [[x,y], ...3-100 points], "color": "#rrggbb"} ] // kingdoms, wilds
}

Rules: biomes are ellipses where props grow, and only on dry land. Put places on land (near the middle of the map is safest for islands). Use few, meaningful features. Do not invent fields."##;

/// Parse and validate one world reply (the error goes back to the model).
pub fn parse_world_reply(reply: &str) -> Result<crate::worldspec::WorldSpec, String> {
    let json = extract_json(reply)
        .ok_or("The reply contained no JSON object. Reply with only the JSON object.")?;
    let spec: crate::worldspec::WorldSpec = serde_json::from_str(json)
        .map_err(|e| format!("The JSON did not match the schema: {e}."))?;
    spec.validate()
        .map_err(|e| format!("The spec broke a rule: {e}."))?;
    crate::worldspec::compile(&spec, 1, &[])
        .map_err(|e| format!("The spec could not be drawn: {e}."))?;
    Ok(spec)
}

/// Where the model lives: nd-world's AI queue, or a direct OpenAI-compatible endpoint.
pub struct Config {
    pub server: Option<String>,
    pub world: Option<String>,
    pub email: Option<String>,
    pub model: Option<String>,
    /// Standalone mode: `(base url, model)`; the key comes from `ND_CARTO_LLM_KEY[_FILE]`.
    pub direct: Option<(String, String)>,
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

impl Config {
    /// The studio's own configuration, from the environment. A direct model
    /// (`ND_CARTO_LLM_URL` + `ND_CARTO_LLM_MODEL`) wins over nd-world.
    pub fn from_env() -> Result<Config, String> {
        let direct = match (env("ND_CARTO_LLM_URL"), env("ND_CARTO_LLM_MODEL")) {
            (Some(u), Some(m)) => Some((u, m)),
            (Some(_), None) => {
                return Err(
                    "set ND_CARTO_LLM_MODEL (the model name) as well as ND_CARTO_LLM_URL".into(),
                )
            }
            _ => None,
        };
        let cfg = Config {
            server: env("ND_CARTO_SERVER"),
            world: env("ND_CARTO_WORLD"),
            email: env("ND_CARTO_EMAIL"),
            model: env("ND_CARTO_MODEL"),
            direct,
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
            let key = env("ND_CARTO_LLM_KEY").or_else(|| {
                env("ND_CARTO_LLM_KEY_FILE")
                    .and_then(|f| std::fs::read_to_string(f).ok())
                    .map(|k| k.trim().to_string())
            });
            return Ok(Backend::Direct(
                ndcart_bridge::DirectChat::new(url, model, key).map_err(|e| e.to_string())?,
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

/// Draft a battle-map spec from `prompt`.
pub async fn draft(cfg: &Config, prompt: &str, ctx: &Context) -> Result<MapSpec, String> {
    too_long(prompt)?;
    let backend = Backend::connect(cfg).await?;
    let system = with_context(SYSTEM_PROMPT, &ctx.with_lore(&backend).await?);
    draft_with(prompt, 2, |user| {
        let (backend, system) = (&backend, system.clone());
        async move { backend.chat(&system, &user).await }
    })
    .await
}

/// Draft a WORLD spec from `prompt`.
pub async fn draft_world(
    cfg: &Config,
    prompt: &str,
    ctx: &Context,
) -> Result<crate::worldspec::WorldSpec, String> {
    too_long(prompt)?;
    let backend = Backend::connect(cfg).await?;
    let system = with_context(WORLD_SYSTEM_PROMPT, &ctx.with_lore(&backend).await?);
    draft_loop(
        prompt,
        2,
        |user| {
            let (backend, system) = (&backend, system.clone());
            async move { backend.chat(&system, &user).await }
        },
        parse_world_reply,
    )
    .await
}

pub const REFINE_SYSTEM_PROMPT: &str = r##"You revise an existing tabletop battle-map floor, given as a room graph. Reply with ONE JSON object and nothing else: the COMPLETE revised map spec in the same schema as the one you are shown (name, floors[], rooms[], links[], stairs[]), with exactly one floor.

Rules: PINNED rooms must come back unchanged: same id, name, w, h and kind. Every other room may be changed, replaced or removed, and new rooms may be added (give them new unique ids). Keep every room reachable from the first through links. Do not describe positions; the layout is computed."##;

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
) -> impl Fn(&str) -> Result<MapSpec, String> {
    let current = current.clone();
    move |reply: &str| {
        let spec = parse_reply(reply)?;
        if spec.floors.len() != 1 {
            return Err("The spec must have exactly one floor.".into());
        }
        for id in &pins {
            let before = current.rooms.iter().find(|r| &r.id == id);
            let after = spec.floors[0].rooms.iter().find(|r| &r.id == id);
            match (before, after) {
                (Some(b), Some(a)) if a.name == b.name && a.w == b.w && a.h == b.h && a.kind == b.kind => {}
                (Some(_), Some(_)) => return Err(format!("Pinned room '{id}' was changed; pinned rooms must keep their id, name, w, h and kind.")),
                (Some(_), None) => return Err(format!("Pinned room '{id}' was dropped; pinned rooms must stay.")),
                (None, _) => return Err(format!("'{id}' is not a room of the current map and cannot be pinned.")),
            }
        }
        Ok(spec)
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
    let system = with_context(REFINE_SYSTEM_PROMPT, &ctx.with_lore(&backend).await?);
    let first = refine_user_turn(current, pins, instruction)?;
    let judge = judge_refine(current, pins.to_vec());
    // The repair rounds quote the complaint after the same current map and request.
    let mut turn = first.clone();
    let mut last = String::new();
    for _ in 0..3 {
        let reply = backend.chat(&system, &turn).await?;
        match judge(&reply) {
            Ok(spec) => return Ok(spec),
            Err(problem) => {
                turn = format!("{first}\n\nYour previous reply was refused. {problem}\nReply again with the corrected JSON object only.");
                last = problem;
            }
        }
    }
    Err(format!(
        "the model did not produce a usable revision after 3 tries; last problem: {last}"
    ))
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

pub const TAG_SYSTEM_PROMPT: &str = r##"You label props for a tabletop map editor so they can be found by search. Reply with ONE JSON object and nothing else: {"tags": ["word", ...]} with 3 to 8 short lowercase tags (1-24 characters: letters, digits, spaces, hyphens). Cover what it is, its material, its setting or mood, and where it would be placed (e.g. "crate", "wooden", "storage", "dungeon", "cargo")."##;

fn judge_tags(reply: &str) -> Result<Vec<String>, String> {
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
    draft_loop(
        &subject,
        1,
        |user| {
            let backend = &backend;
            async move { backend.chat(TAG_SYSTEM_PROMPT, &user).await }
        },
        judge_tags,
    )
    .await
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
    let cfg = Config {
        server,
        world,
        email,
        model,
        direct,
    };
    let spec = draft(
        &cfg,
        &prompt.ok_or("describe the map, in quotes")?,
        &Context::default(),
    )
    .await?;
    let text = serde_json::to_string_pretty(&spec).map_err(|e| e.to_string())?;
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

    #[tokio::test]
    async fn a_refused_reply_is_sent_back_with_the_reason_and_the_next_try_can_succeed() {
        let mut seen: Vec<String> = Vec::new();
        let replies = [
            "not json at all".to_string(),
            GOOD.replace(r#""kind":"secret""#, r#""kind":"warp""#),
            GOOD.to_string(),
        ];
        let mut i = 0;
        let spec = draft_with("a crypt", 2, |user| {
            seen.push(user);
            let r = replies[i].clone();
            i += 1;
            async move { Ok(r) }
        })
        .await
        .unwrap();
        assert_eq!(spec.name, "Crypt");
        assert_eq!(seen.len(), 3);
        assert!(seen[0].contains("a crypt") && !seen[0].contains("refused"));
        assert!(seen[1].contains("refused") && seen[1].contains("no JSON"));
        assert!(seen[2].contains("schema"), "{}", seen[2]);
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
        let replies = [r#"{"name":"X","land":3}"#.to_string(), good.to_string()];
        let mut i = 0;
        let mut seen = Vec::new();
        let spec = draft_loop(
            "an archipelago",
            2,
            |u| {
                seen.push(u);
                let r = replies[i].clone();
                i += 1;
                async move { Ok(r) }
            },
            parse_world_reply,
        )
        .await
        .unwrap();
        assert_eq!(spec.name, "Isles");
        assert!(seen[1].contains("land must be"), "{}", seen[1]);
        assert!(WORLD_SYSTEM_PROMPT.contains("biomes") && WORLD_SYSTEM_PROMPT.contains("0..1"));
    }

    #[test]
    fn style_and_lore_are_added_to_the_system_prompt_only_when_present() {
        assert_eq!(
            with_context(SYSTEM_PROMPT, &Context::default()),
            SYSTEM_PROMPT
        );
        let ctx = Context {
            tone: Some("gritty cyberpunk".into()),
            locations: vec!["Port Neon".into(), "The Drowned Abbey".into()],
            lore: false,
        };
        let s = with_context(SYSTEM_PROMPT, &ctx);
        assert!(
            s.contains("gritty cyberpunk")
                && s.contains("Port Neon; The Drowned Abbey")
                && s.starts_with(SYSTEM_PROMPT)
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
            .contains("was changed"));
        assert!(judge(&GOOD.replace("Nave", "Chapel"))
            .unwrap_err()
            .contains("was changed"));
        let no_a = r#"{"name":"Crypt","floors":[{"name":"G","rooms":[{"id":"b","name":"Ossuary","w":5,"h":5}],"links":[]}]}"#;
        assert!(judge(no_a).unwrap_err().contains("was dropped"));
        assert!(judge_refine(&current, vec!["zzz".into()])(GOOD)
            .unwrap_err()
            .contains("not a room"));
        let two = r#"{"name":"C","floors":[{"name":"G","rooms":[{"id":"a","name":"Nave","w":8,"h":6}],"links":[]},{"name":"H","rooms":[{"id":"a","name":"Nave","w":8,"h":6}],"links":[]}]}"#;
        assert!(judge(two).unwrap_err().contains("exactly one floor"));
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
            judge_tags("```{\"tags\":[\"Crate\",\"wooden\",\"storage\"]}```").unwrap(),
            ["crate", "wooden", "storage"]
        );
        assert!(judge_tags("{\"tags\":[\"a!\"]}")
            .unwrap_err()
            .contains("at least 2"));
        assert!(judge_tags("{\"nope\":1}").unwrap_err().contains("tags"));
        let replies = [
            "tags: crate, wooden".to_string(),
            "{\"tags\":[\"crate\",\"cargo\"]}".to_string(),
        ];
        let mut i = 0;
        let got = draft_loop(
            "Prop name: Crate",
            1,
            |_| {
                let r = replies[i].clone();
                i += 1;
                async move { Ok(r) }
            },
            judge_tags,
        )
        .await
        .unwrap();
        assert_eq!(got, ["crate", "cargo"]);
        assert!(TAG_SYSTEM_PROMPT.contains("\"tags\""));
    }

    #[tokio::test]
    async fn it_gives_up_after_the_repairs_and_passes_a_backend_error_straight_through() {
        let e = draft_with("x", 1, |_| async { Ok("nope".to_string()) })
            .await
            .unwrap_err();
        assert!(e.contains("2 tries"), "{e}");
        let e = draft_with("x", 2, |_| async {
            Err::<String, _>("nd-world refused it".to_string())
        })
        .await
        .unwrap_err();
        assert_eq!(e, "nd-world refused it");
        assert!(SYSTEM_PROMPT.contains("\"links\"") && SYSTEM_PROMPT.contains("reachable"));
    }
}
