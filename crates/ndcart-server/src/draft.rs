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
pub async fn draft_with<F, Fut>(prompt: &str, repairs: usize, mut ask: F) -> Result<MapSpec, String>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let mut user = format!("Design this map: {}", prompt.trim());
    let mut last = String::new();
    for _ in 0..=repairs {
        let reply = ask(user.clone()).await?;
        match parse_reply(&reply) {
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

pub struct Config {
    pub server: String,
    pub world: Option<String>,
    pub email: String,
    pub model: Option<String>,
}

impl Config {
    /// The studio's own configuration, from the environment.
    pub fn from_env() -> Result<Config, String> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Ok(Config {
            server: get("ND_CARTO_SERVER")
                .ok_or("set ND_CARTO_SERVER (the nd-world address) to draft with AI")?,
            world: get("ND_CARTO_WORLD"),
            email: get("ND_CARTO_EMAIL").ok_or("set ND_CARTO_EMAIL to draft with AI")?,
            model: get("ND_CARTO_MODEL"),
        })
    }
}

/// Log in to nd-world and draft a spec from `prompt`.
pub async fn draft(cfg: &Config, prompt: &str) -> Result<MapSpec, String> {
    if prompt.trim().is_empty() || prompt.chars().count() > 2_000 {
        return Err("describe the map in 1-2000 characters".into());
    }
    let pw = crate::publish::password()?;
    let totp = std::env::var("ND_CARTO_TOTP").ok();
    let bridge = Bridge::new(&cfg.server, cfg.world.as_deref()).map_err(|e| e.to_string())?;
    bridge
        .login(&cfg.email, &pw, totp.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let model = cfg.model.clone();
    draft_with(prompt, 2, |user| {
        let (bridge, model) = (&bridge, model.clone());
        async move {
            bridge
                .chat(SYSTEM_PROMPT, &user, model.as_deref())
                .await
                .map_err(|e| e.to_string())
        }
    })
    .await
}

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let (mut prompt, mut server, mut world, mut email, mut model, mut out) =
        (None, None, None, None, None, None);
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
        match a.as_str() {
            "--server" => server = Some(val("--server")?),
            "--world" => world = Some(val("--world")?),
            "--email" => email = Some(val("--email")?),
            "--model" => model = Some(val("--model")?),
            "--out" => out = Some(val("--out")?),
            o if !o.starts_with("--") && prompt.is_none() => prompt = Some(o.to_string()),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let cfg = Config {
        server: server.ok_or("--server is required")?,
        world,
        email: email.ok_or("--email is required")?,
        model,
    };
    let spec = draft(&cfg, &prompt.ok_or("describe the map, in quotes")?).await?;
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
