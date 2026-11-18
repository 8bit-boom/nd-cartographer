//! Standalone AI: talk straight to an OpenAI-compatible endpoint (Ollama's
//! `/v1`, llama.cpp's server, LM Studio, a hosted API) when no nd-world is
//! around (plan §8.3 "standalone mode"). A one-permit semaphore stands in for
//! nd-world's queue, so a local GPU is never hit by two requests at once.

use crate::BridgeError;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct DirectChat {
    http: Client,
    /// Everything before `/chat/completions`, e.g. `http://127.0.0.1:11434/v1`.
    base: String,
    model: String,
    /// Sent as `Authorization: Bearer …`; local servers need none.
    key: Option<String>,
    gate: Arc<Semaphore>,
    /// Ask llama.cpp servers not to think (`enable_thinking: false`) on structured calls.
    thinking_off: bool,
}

/// A plain reqwest client with the same timeout profile the chat client
/// uses - for callers that only need a `GET` (the model list).
pub fn plain_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("http client")
}

impl DirectChat {
    pub fn new(base_url: &str, model: &str, key: Option<String>) -> Result<Self, BridgeError> {
        if model.trim().is_empty() {
            return Err(BridgeError::Precheck("name the model to use".into()));
        }
        let url = base_url.trim_end_matches('/');
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(BridgeError::Precheck(
                "the model address must start with http:// or https://".into(),
            ));
        }
        let http = Client::builder()
            .user_agent(concat!("ndcart/", env!("CARGO_PKG_VERSION")))
            // A local model can be slow to answer; a map spec is a long reply.
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        Ok(DirectChat {
            http,
            base: url.to_string(),
            model: model.to_string(),
            key,
            gate: Arc::new(Semaphore::new(1)),
            thinking_off: true,
        })
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.base, self.model)
    }

    /// One non-streaming turn: `system` then `user`; returns the reply text.
    pub async fn chat(&self, system: &str, user: &str) -> Result<String, BridgeError> {
        let msgs = [Msg::system(system), Msg::user(user)];
        Ok(self
            .chat_messages(&msgs, &ChatOptions::default())
            .await?
            .content)
    }

    /// Let reasoning models think on structured calls (off by default: a JSON task wants
    /// the answer, not a monologue).
    pub fn with_thinking(mut self, on: bool) -> Self {
        self.thinking_off = !on;
        self
    }

    /// The endpoint's address (without `/chat/completions`).
    pub fn base(&self) -> &str {
        &self.base
    }

    /// One non-streaming turn over any message list, with optional request options.
    pub async fn chat_messages(
        &self,
        msgs: &[Msg],
        opts: &ChatOptions,
    ) -> Result<ChatReply, BridgeError> {
        let (status, text) = self.post_raw(msgs, opts).await?;
        finish(status, &text)
    }

    /// A turn whose answer must be one JSON object of the given schema (`name`, `schema`).
    /// Where the endpoint is known to enforce schemas (llama.cpp, Ollama) or might (unknown,
    /// tried once), the request carries `response_format`; a server that refuses it with a
    /// 400 or 422 is asked again without, and remembered. llama.cpp is also told not to
    /// think, unless [`with_thinking`](Self::with_thinking) turned that on.
    pub async fn chat_structured(
        &self,
        msgs: &[Msg],
        schema: Option<(&str, &Value)>,
        base_opts: &ChatOptions,
    ) -> Result<ChatReply, BridgeError> {
        let caps = crate::caps::probe(&self.base, self.key.as_deref()).await;
        let mut opts = base_opts.clone();
        if caps.kind == crate::caps::EndpointKind::LlamaCpp && self.thinking_off {
            opts.extra
                .entry("chat_template_kwargs".to_string())
                .or_insert_with(|| json!({"enable_thinking": false}));
        }
        let sent_schema = match schema {
            Some((name, schema)) if caps.wants_schema() => {
                opts.response_format = Some(json!({
                    "type": "json_schema",
                    "json_schema": {"name": name, "strict": true, "schema": schema},
                }));
                true
            }
            _ => false,
        };
        let (mut status, mut text) = self.post_raw(msgs, &opts).await?;
        if sent_schema && refused_schema(status, &text) {
            crate::caps::remember_json_schema(&self.base, false);
            opts.response_format = None;
            (status, text) = self.post_raw(msgs, &opts).await?;
        } else if sent_schema && status.is_success() && caps.json_schema.is_none() {
            crate::caps::remember_json_schema(&self.base, true);
        }
        finish(status, &text)
    }

    async fn post_raw(
        &self,
        msgs: &[Msg],
        opts: &ChatOptions,
    ) -> Result<(StatusCode, String), BridgeError> {
        let _turn = self
            .gate
            .acquire()
            .await
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        let body = opts.body(&self.model, msgs);
        let mut req = self
            .http
            .post(format!("{}/chat/completions", self.base))
            .json(&body);
        if let Some(k) = &self.key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| BridgeError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        Ok((status, text))
    }
}

/// Did the server say no to `response_format` (rather than to the request as a whole)?
fn refused_schema(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::BAD_REQUEST && status != StatusCode::UNPROCESSABLE_ENTITY {
        return false;
    }
    let b = body.to_lowercase();
    b.contains("response_format") || b.contains("json_schema")
}

/// Turn one HTTP answer into a reply or the plain-words error.
fn finish(status: StatusCode, text: &str) -> Result<ChatReply, BridgeError> {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(BridgeError::AuthRequired);
    }
    if !status.is_success() {
        return Err(BridgeError::Rejected(format!(
            "{status}: {}",
            text.chars().take(300).collect::<String>()
        )));
    }
    let v: Value = serde_json::from_str(text)
        .map_err(|e| BridgeError::Protocol(format!("chat answer: {e}")))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BridgeError::Protocol("the model's answer had no choices[0].message.content".into())
        })?;
    let reasoning = v
        .pointer("/choices/0/message/reasoning_content")
        .and_then(Value::as_str)
        .filter(|r| !r.trim().is_empty())
        .map(str::to_string);
    let finish_reason = v
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    if content.trim().is_empty() {
        let cut = if finish_reason.as_deref() == Some("length") {
            " (reply cut off: max_tokens)"
        } else {
            ""
        };
        return Err(BridgeError::Rejected(if reasoning.is_some() {
            format!(
                "the model used its whole reply for thinking; turn thinking off or raise max_tokens{cut}"
            )
        } else {
            format!("the model returned an empty reply{cut}")
        }));
    }
    Ok(ChatReply {
        content: content.to_string(),
        reasoning,
        finish_reason,
        usage: v.get("usage").cloned(),
    })
}

/// One chat message.
#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub role: String,
    pub content: String,
}

impl Msg {
    pub fn new(role: &str, content: impl Into<String>) -> Msg {
        Msg {
            role: role.into(),
            content: content.into(),
        }
    }
    pub fn system(content: impl Into<String>) -> Msg {
        Msg::new("system", content)
    }
    pub fn user(content: impl Into<String>) -> Msg {
        Msg::new("user", content)
    }
    pub fn assistant(content: impl Into<String>) -> Msg {
        Msg::new("assistant", content)
    }
}

/// Optional request fields; each is sent only when set.
#[derive(Clone, Debug, Default)]
pub struct ChatOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub seed: Option<i64>,
    /// The OpenAI `response_format` object (JSON mode or a JSON schema).
    pub response_format: Option<Value>,
    /// Server-specific extras (llama.cpp's `chat_template_kwargs`, ...), merged at the top
    /// level of the request body. Fill only for endpoints known to understand them.
    pub extra: serde_json::Map<String, Value>,
}

impl ChatOptions {
    /// The request body: `model`, `stream:false`, `messages`, then whatever is set.
    pub fn body(&self, model: &str, msgs: &[Msg]) -> Value {
        let mut body = json!({
            "model": model,
            "stream": false,
            "messages": msgs
                .iter()
                .map(|m| json!({"role": m.role, "content": m.content}))
                .collect::<Vec<_>>(),
        });
        let o = body.as_object_mut().expect("object");
        if let Some(t) = self.temperature {
            o.insert("temperature".into(), json!(t));
        }
        if let Some(n) = self.max_tokens {
            o.insert("max_tokens".into(), json!(n));
        }
        if let Some(n) = self.seed {
            o.insert("seed".into(), json!(n));
        }
        if let Some(f) = &self.response_format {
            o.insert("response_format".into(), f.clone());
        }
        for (k, v) in &self.extra {
            o.entry(k.clone()).or_insert_with(|| v.clone());
        }
        body
    }
}

/// A successful answer.
#[derive(Clone, Debug)]
pub struct ChatReply {
    pub content: String,
    /// `reasoning_content`, when the server separates thinking from the answer.
    pub reasoning: Option<String>,
    /// `stop`, `length`, ...
    pub finish_reason: Option<String>,
    pub usage: Option<Value>,
}
