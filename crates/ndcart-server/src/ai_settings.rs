//! The studio's AI model settings (plan §21's "standalone direct mode",
//! e.g. a local Unsloth Studio or Ollama): persisted at
//! `<data dir>/ai-settings.json`, with environment variables keeping the
//! final word per field (so CI, the CLI and Docker keep working unchanged).
//!
//! The stored `key` never leaves the server: the GET redacts it, and it is
//! only ever sent to the model endpoint itself as `Authorization: Bearer`.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct StoredAiSettings {
    /// OpenAI-compatible base URL, e.g. `http://127.0.0.1:3915/v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_url: Option<String>,
    /// The model name the endpoint serves (`unsloth/Qwen3-...`, `llama3.1:8b`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_model: Option<String>,
    /// Sent as `Authorization: Bearer`; many local servers need none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

pub const FILE_NAME: &str = "ai-settings.json";

pub fn load(dir: Option<&std::path::Path>) -> StoredAiSettings {
    dir.and_then(|d| {
        std::fs::read_to_string(d.join(FILE_NAME))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    })
    .unwrap_or_default()
}

pub fn save(dir: Option<&std::path::Path>, s: &StoredAiSettings) -> Result<(), String> {
    let body = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    match dir {
        Some(d) => {
            std::fs::create_dir_all(d)
                .map_err(|e| format!("cannot create {}: {e}", d.display()))?;
            crate::store::write_atomic(&d.join(FILE_NAME), body.as_bytes())
                .map_err(|e| format!("cannot save the AI settings: {e}"))
        }
        None => Ok(()), // no data dir: in-memory only (tests)
    }
}

/// The direct model as env-else-file: environment variables keep the final
/// word per field, so existing deployments (CLI, Docker, CI) are untouched.
pub fn resolve_direct(
    env_url: Option<String>,
    env_model: Option<String>,
    stored: &StoredAiSettings,
) -> Option<(String, String)> {
    let url = env_url.or_else(|| stored.direct_url.clone());
    let model = env_model.or_else(|| stored.direct_model.clone());
    match (url, model) {
        (Some(u), Some(m)) if !u.trim().is_empty() && !m.trim().is_empty() => {
            Some((u.trim().to_string(), m.trim().to_string()))
        }
        _ => None,
    }
}

/// The key as env-else-file.
pub fn resolve_key(env_key: Option<String>, stored: &StoredAiSettings) -> Option<String> {
    env_key.or_else(|| stored.key.clone())
}

/// A redacted view for the UI: which fields are set, never their secrets.
#[derive(Serialize)]
pub struct AiSettingsView {
    pub direct_url: Option<String>,
    pub direct_model: Option<String>,
    pub key_set: bool,
    pub source: &'static str,
}

pub fn view(stored: &StoredAiSettings) -> AiSettingsView {
    let env_url = std::env::var("ND_CARTO_LLM_URL")
        .ok()
        .filter(|v| !v.is_empty());
    let env_model = std::env::var("ND_CARTO_LLM_MODEL")
        .ok()
        .filter(|v| !v.is_empty());
    let source = if (env_url.is_some() && env_model.is_some())
        || std::env::var("ND_CARTO_LLM_KEY").is_ok()
    {
        "environment"
    } else if stored.direct_url.is_some() || stored.direct_model.is_some() {
        "saved"
    } else {
        "none"
    };
    AiSettingsView {
        direct_url: env_url.or_else(|| stored.direct_url.clone()),
        direct_model: env_model.or_else(|| stored.direct_model.clone()),
        key_set: std::env::var("ND_CARTO_LLM_KEY").is_ok() || stored.key.is_some(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_env_per_field_and_fills_from_stored() {
        let stored = StoredAiSettings {
            direct_url: Some("http://file:1/v1".into()),
            direct_model: Some("file-model".into()),
            key: Some("file-key".into()),
        };
        // Both from the file.
        assert_eq!(
            resolve_direct(None, None, &stored),
            Some(("http://file:1/v1".to_string(), "file-model".to_string()))
        );
        // Env wins per field, the other comes from the file.
        assert_eq!(
            resolve_direct(Some("http://env:2/v1".into()), None, &stored),
            Some(("http://env:2/v1".to_string(), "file-model".to_string()))
        );
        assert_eq!(resolve_key(None, &stored), Some("file-key".to_string()));
        assert_eq!(
            resolve_key(Some("env-key".into()), &stored),
            Some("env-key".to_string())
        );
        // Half-configured (url without model) is not a direct model.
        let half = StoredAiSettings {
            direct_url: Some("http://x/v1".into()),
            ..Default::default()
        };
        assert_eq!(resolve_direct(None, None, &half), None);
    }

    #[test]
    fn save_then_load_round_trips_through_a_dir() {
        let dir = std::env::temp_dir().join(format!("ndcart-ai-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let stored = StoredAiSettings {
            direct_url: Some("http://127.0.0.1:3915/v1".into()),
            direct_model: Some("unsloth/Qwen3-4B-GGUF".into()),
            key: None,
        };
        save(Some(&dir), &stored).unwrap();
        assert_eq!(load(Some(&dir)), stored);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
