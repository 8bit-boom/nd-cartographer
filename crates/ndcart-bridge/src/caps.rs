//! What an OpenAI-compatible endpoint can do, found out once and remembered for ten minutes
//! (plan 3.2). llama.cpp and Ollama enforce a JSON schema while decoding, so a model cannot
//! write an invented field; a hosted or unknown endpoint may reject the request field, in
//! which case the caller falls back to a plain request and the answer is remembered here.

use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EndpointKind {
    LlamaCpp,
    Ollama,
    UnslothStudio,
    OpenAiLike,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct Caps {
    pub kind: EndpointKind,
    /// `Some(true)`: constrained decoding works. `Some(false)`: the server refused it once.
    /// `None`: not known; try it once.
    pub json_schema: Option<bool>,
    /// The context window in tokens, when the server says (llama.cpp's `/props`).
    pub ctx: Option<u32>,
    pub model_ids: Vec<String>,
}

impl Caps {
    /// Send `response_format` with a schema? Unless the server has already refused it.
    pub fn wants_schema(&self) -> bool {
        self.json_schema != Some(false)
    }
}

/// `http://h:1/v1` -> `http://h:1`: where `/props`, `/health` and `/api/version` live.
pub fn origin_of(base: &str) -> String {
    let b = base.trim_end_matches('/');
    match b.find("://") {
        Some(i) => match b[i + 3..].find('/') {
            Some(j) => b[..i + 3 + j].to_string(),
            None => b.to_string(),
        },
        None => b.to_string(),
    }
}

fn cache() -> &'static Mutex<HashMap<String, (Instant, Caps)>> {
    static C: OnceLock<Mutex<HashMap<String, (Instant, Caps)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remember what a request taught us: the endpoint took (`true`) or refused (`false`) a schema.
pub fn remember_json_schema(base: &str, ok: bool) {
    if let Some((_, c)) = cache().lock().expect("caps").get_mut(base) {
        c.json_schema = Some(ok);
    }
}

/// Forget what is known about `base` (the next [`probe`] asks again).
pub fn forget(base: &str) {
    cache().lock().expect("caps").remove(base);
}

async fn get_json(http: &Client, url: &str, key: Option<&str>) -> Option<Value> {
    let mut req = http.get(url);
    if let Some(k) = key {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Value>().await.ok()
}

/// Ask the endpoint what it is. Never fails: an endpoint that cannot be reached is `Unknown`
/// (and the real request will say why). Cached per `base` for ten minutes.
pub async fn probe(base: &str, key: Option<&str>) -> Caps {
    if let Some((at, c)) = cache().lock().expect("caps").get(base) {
        if at.elapsed() < TTL {
            return c.clone();
        }
    }
    let http = Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .expect("http client");
    let origin = origin_of(base);
    let models = get_json(
        &http,
        &format!("{}/models", base.trim_end_matches('/')),
        key,
    )
    .await;
    let model_ids: Vec<String> = models
        .as_ref()
        .and_then(|m| m["data"].as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let owned_by_unsloth = models
        .as_ref()
        .and_then(|m| m["data"].as_array())
        .is_some_and(|a| {
            a.iter().any(|m| {
                m["owned_by"]
                    .as_str()
                    .is_some_and(|o| o.to_lowercase().contains("unsloth"))
            })
        });
    let props = get_json(&http, &format!("{origin}/props"), key).await;
    let llama = props.as_ref().filter(|p| {
        p.get("default_generation_settings").is_some()
            || p.get("build_info").is_some()
            || p.get("chat_template").is_some()
    });
    let caps = if let Some(p) = llama {
        Caps {
            kind: EndpointKind::LlamaCpp,
            json_schema: Some(true),
            ctx: p
                .pointer("/default_generation_settings/n_ctx")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
            model_ids,
        }
    } else if get_json(&http, &format!("{origin}/api/version"), key)
        .await
        .is_some_and(|v| v.get("version").is_some())
    {
        Caps {
            kind: EndpointKind::Ollama,
            json_schema: Some(true),
            ctx: None,
            model_ids,
        }
    } else {
        let kind = if owned_by_unsloth {
            EndpointKind::UnslothStudio
        } else if models.is_some() {
            EndpointKind::OpenAiLike
        } else {
            EndpointKind::Unknown
        };
        Caps {
            kind,
            json_schema: None,
            ctx: None,
            model_ids,
        }
    };
    cache()
        .lock()
        .expect("caps")
        .insert(base.to_string(), (Instant::now(), caps.clone()));
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_origin_is_everything_before_the_path() {
        assert_eq!(origin_of("http://h:1/v1"), "http://h:1");
        assert_eq!(origin_of("https://h/"), "https://h");
        assert_eq!(origin_of("http://h:8/openai/v1"), "http://h:8");
    }
}
