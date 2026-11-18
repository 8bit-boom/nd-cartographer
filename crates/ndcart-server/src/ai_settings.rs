//! The studio's AI model settings (plan §21's "standalone direct mode",
//! e.g. a local llama.cpp server, Ollama or Unsloth Studio): persisted at
//! `<data dir>/ai-settings.json`, with environment variables keeping the
//! final word per field (so CI, the CLI and Docker keep working unchanged).
//!
//! The stored `key` never leaves the server: the GET redacts it, and it is
//! only ever sent to the model endpoint itself as `Authorization: Bearer`.

// The managed-model helpers are used by the bridge-only AI code.
#![cfg_attr(not(feature = "bridge"), allow(dead_code))]

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct StoredAiSettings {
    /// OpenAI-compatible base URL, e.g. `http://127.0.0.1:8391/v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_url: Option<String>,
    /// The model name the endpoint serves (`unsloth/Qwen3-...`, `llama3.1:8b`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_model: Option<String>,
    /// Sent as `Authorization: Bearer`; many local servers need none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Let reasoning models think on JSON tasks. Unset means off: a spec wants the answer,
    /// not a monologue (sent to llama.cpp servers as `enable_thinking: false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
    /// Keep accepted AI results and reuse them for the same request (default off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<bool>,
    /// Build maps in steps (an outline, then each floor) even for one floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stepwise: Option<bool>,
    /// A model the studio runs itself with llama.cpp (`ndcart ai setup`). Used when no other
    /// endpoint is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<Managed>,
}

/// The managed local server (plan item 4.5). Defaults are applied by the accessors, so a file
/// that names only `model_file` works.
#[derive(Serialize, Deserialize, Clone, Default, PartialEq, Debug)]
pub struct Managed {
    /// The GGUF file, relative to `<data>/models` (or an absolute path).
    pub model_file: String,
    /// Context size (default 16384).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<u32>,
    /// Loopback port (default 8391).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `-ngl`: `auto` (default), `all`, or a number (`0` for CPU only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_layers: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<u32>,
    /// Start (and keep running) the server whenever `ndcart serve` runs.
    #[serde(default)]
    pub autostart: bool,
    /// An explicit `llama-server` path (the environment variable still wins).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_path: Option<String>,
    /// The file holding the server's API key (set when the server is first started).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<String>,
    /// The catalog id this came from, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

pub const MANAGED_ALIAS: &str = "ndcart-local";
pub const DEFAULT_PORT: u16 = 8391;
pub const DEFAULT_CTX: u32 = 16384;

impl Managed {
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_PORT)
    }
    pub fn ctx(&self) -> u32 {
        self.ctx.unwrap_or(DEFAULT_CTX)
    }
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port())
    }
}

/// The managed server's key, from its 0600 file (never from the command line).
pub fn read_key_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
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
            crate::store::write_atomic_private(&d.join(FILE_NAME), body.as_bytes())
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
        // Nothing else set: the model the studio runs itself.
        (None, None) => stored
            .managed
            .as_ref()
            .map(|m| (m.base_url(), MANAGED_ALIAS.to_string())),
        _ => None,
    }
}

/// The key for the endpoint at `url`: the environment (`ND_CARTO_LLM_KEY`, or the file named by
/// `ND_CARTO_LLM_KEY_FILE`), else the managed server's key file when `url` is the managed
/// server (so its key is never sent anywhere else), else the saved key.
pub fn resolve_key_for(
    url: &str,
    env_key: Option<String>,
    env_key_file: Option<String>,
    stored: &StoredAiSettings,
) -> Option<String> {
    if let Some(k) = env_key {
        return Some(k);
    }
    if let Some(k) = env_key_file.as_deref().and_then(read_key_file) {
        return Some(k);
    }
    if let Some(m) = &stored.managed {
        if url.trim_end_matches('/') == m.base_url() {
            return m.key_file.as_deref().and_then(read_key_file);
        }
    }
    stored.key.clone()
}

/// A redacted view for the UI: which fields are set, never their secrets.
#[derive(Serialize)]
pub struct AiSettingsView {
    pub direct_url: Option<String>,
    pub direct_model: Option<String>,
    pub key_set: bool,
    pub thinking: bool,
    pub cache: bool,
    pub stepwise: bool,
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
    } else if stored.managed.is_some() {
        "managed"
    } else {
        "none"
    };
    AiSettingsView {
        direct_url: env_url.or_else(|| stored.direct_url.clone()),
        direct_model: env_model.or_else(|| stored.direct_model.clone()),
        key_set: std::env::var("ND_CARTO_LLM_KEY").is_ok() || stored.key.is_some(),
        thinking: stored.thinking.unwrap_or(false),
        cache: stored.cache.unwrap_or(false),
        stepwise: stored.stepwise.unwrap_or(false),
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
            ..Default::default()
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
        assert_eq!(
            resolve_key_for("http://file:1/v1", None, None, &stored),
            Some("file-key".to_string())
        );
        assert_eq!(
            resolve_key_for("http://file:1/v1", Some("env-key".into()), None, &stored),
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
            direct_url: Some("http://127.0.0.1:8391/v1".into()),
            direct_model: Some("unsloth/Qwen3-4B-GGUF".into()),
            key: None,
            thinking: Some(true),
            cache: Some(true),
            stepwise: Some(true),
            managed: Some(Managed {
                model_file: "unsloth/X-GGUF/X.gguf".into(),
                autostart: true,
                ..Default::default()
            }),
        };
        save(Some(&dir), &stored).unwrap();
        assert_eq!(load(Some(&dir)), stored);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_settings_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ndcart-ai-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let stored = StoredAiSettings {
            key: Some("sk-secret".into()),
            ..Default::default()
        };
        save(Some(&dir), &stored).unwrap();
        // A second save (replacing an existing file) stays private too.
        save(Some(&dir), &stored).unwrap();
        let mode = std::fs::metadata(dir.join(FILE_NAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_managed_model_is_the_fallback_and_its_key_goes_only_to_its_own_address() {
        let dir = std::env::temp_dir().join(format!("ndcart-ai-managed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let kf = dir.join("k");
        std::fs::write(&kf, "managed-key\n").unwrap();
        let stored = StoredAiSettings {
            key: Some("saved-key".into()),
            managed: Some(Managed {
                model_file: "m.gguf".into(),
                port: Some(9000),
                key_file: Some(kf.to_string_lossy().into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_direct(None, None, &stored),
            Some((
                "http://127.0.0.1:9000/v1".to_string(),
                "ndcart-local".to_string()
            ))
        );
        assert_eq!(
            resolve_key_for("http://127.0.0.1:9000/v1", None, None, &stored).as_deref(),
            Some("managed-key")
        );
        // Another address gets the saved key, never the managed one.
        assert_eq!(
            resolve_key_for("http://elsewhere:1/v1", None, None, &stored).as_deref(),
            Some("saved-key")
        );
        // The environment wins: a key file there beats everything.
        let ef = dir.join("e");
        std::fs::write(&ef, "env-file-key").unwrap();
        assert_eq!(
            resolve_key_for(
                "http://127.0.0.1:9000/v1",
                None,
                ef.to_str().map(String::from),
                &stored
            )
            .as_deref(),
            Some("env-file-key")
        );
        // An explicit saved endpoint beats the managed one.
        let both = StoredAiSettings {
            direct_url: Some("http://x/v1".into()),
            direct_model: Some("m".into()),
            ..stored.clone()
        };
        assert_eq!(resolve_direct(None, None, &both).unwrap().0, "http://x/v1");
        // A half-set environment does not silently fall back to the managed model.
        assert_eq!(
            resolve_direct(Some("http://y/v1".into()), None, &stored),
            None
        );
        assert_eq!(view(&stored).source, "managed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
