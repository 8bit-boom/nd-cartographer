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
        })
    }

    pub fn describe(&self) -> String {
        format!("{} ({})", self.base, self.model)
    }

    /// One non-streaming turn: `system` then `user`; returns the reply text.
    pub async fn chat(&self, system: &str, user: &str) -> Result<String, BridgeError> {
        let _turn = self
            .gate
            .acquire()
            .await
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        let body = json!({
            "model": self.model,
            "stream": false,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
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
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(BridgeError::AuthRequired);
        }
        if !status.is_success() {
            return Err(BridgeError::Rejected(format!(
                "{status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| BridgeError::Protocol(format!("chat answer: {e}")))?;
        let out = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::Protocol("the model's answer had no choices[0].message.content".into())
            })?;
        if out.trim().is_empty() {
            return Err(BridgeError::Rejected(
                "the model returned an empty reply".into(),
            ));
        }
        Ok(out.to_string())
    }
}
