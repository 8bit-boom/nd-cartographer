//! The local-model catalog (`assets/ai/models.toml`): which GGUF files the studio knows how
//! to fetch and which machines they suit. It holds no checksums and no live sizes (those come
//! from the Hub when a file is pulled), and `verified` stays false until `ndcart ai test` has
//! passed on this machine (recorded in `<data>/ai-verified.json`, see [`Record`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const CATALOG_TOML: &str = include_str!("../../assets/ai/models.toml");

/// Memory classes, smallest first. A model of tier T fits machines of tier T and above.
pub const TIERS: &[&str] = &["cpu8", "vram12", "apple16", "vram24", "apple32"];

/// A rough usable-memory budget for a tier, in bytes (the tier's name says the class).
pub fn tier_budget(tier: &str) -> u64 {
    const G: u64 = 1_000_000_000;
    match tier {
        "cpu8" => 6 * G,
        "vram12" => 12 * G,
        "apple16" => 11 * G,
        "vram24" => 24 * G,
        "apple32" => 22 * G,
        _ => 0,
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub label: String,
    pub tier: String,
    pub hf_repo: String,
    pub file: String,
    pub quant: String,
    pub approx_bytes: u64,
    pub ctx_default: u32,
    pub ctx_native: u32,
    #[serde(default = "default_kv")]
    pub kv_kib_per_token: u64,
    pub license: String,
    #[serde(default)]
    pub notes: String,
    /// Passed `ndcart ai test` somewhere. Always false in the bundled catalog.
    #[serde(default)]
    pub verified: bool,
    /// The date the repo and file names were last seen live on the Hub; empty means never.
    #[serde(default)]
    pub listing_checked: String,
    /// Optional pinned SHA-256 (wins over the Hub's when present).
    #[serde(default)]
    pub sha256: Option<String>,
    /// Allow a repo that is not `unsloth/*-GGUF`.
    #[serde(default)]
    pub allow_non_unsloth: bool,
}

fn default_kv() -> u64 {
    64
}

#[derive(Deserialize)]
struct Doc {
    #[allow(dead_code)]
    version: u32,
    #[serde(default)]
    model: Vec<Entry>,
}

/// `unsloth/<name>-GGUF`, the shape of a repo the studio recommends.
pub fn is_unsloth_repo(repo: &str) -> bool {
    match repo.strip_prefix("unsloth/") {
        Some(name) => {
            name.len() > "-GGUF".len()
                && name.ends_with("-GGUF")
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        }
        None => false,
    }
}

/// Parse and check a catalog text.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let doc: Doc = toml::from_str(text).map_err(|e| format!("models.toml: {e}"))?;
    let mut seen = std::collections::BTreeSet::new();
    for m in &doc.model {
        if !seen.insert(m.id.clone()) {
            return Err(format!("models.toml: duplicate id '{}'", m.id));
        }
        if m.id.is_empty()
            || !m
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-'))
        {
            return Err(format!("models.toml: bad id '{}'", m.id));
        }
        if !TIERS.contains(&m.tier.as_str()) {
            return Err(format!(
                "models.toml: '{}' has unknown tier '{}'",
                m.id, m.tier
            ));
        }
        if m.ctx_default > m.ctx_native {
            return Err(format!("models.toml: '{}' ctx_default > ctx_native", m.id));
        }
        if !m.allow_non_unsloth && !is_unsloth_repo(&m.hf_repo) {
            return Err(format!(
                "models.toml: '{}' repo '{}' is not unsloth/<name>-GGUF (set allow_non_unsloth)",
                m.id, m.hf_repo
            ));
        }
        if !m.file.ends_with(".gguf") || m.file.contains('/') || m.file.contains("..") {
            return Err(format!(
                "models.toml: '{}' file must be a plain .gguf name",
                m.id
            ));
        }
        if let Some(h) = &m.sha256 {
            if h.len() != 64 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "models.toml: '{}' sha256 must be 64 hex digits",
                    m.id
                ));
            }
        }
    }
    Ok(doc.model)
}

/// The bundled catalog (checked by a test, so a failure here is a build-time mistake).
pub fn bundled() -> Vec<Entry> {
    parse(CATALOG_TOML).expect("bundled models.toml is valid")
}

pub fn find<'a>(cat: &'a [Entry], id: &str) -> Option<&'a Entry> {
    cat.iter().find(|m| m.id == id)
}

/// One recorded self-test result for a model id, on this machine.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Record {
    pub verdict: String,
    pub at: String,
    #[serde(default)]
    pub tokens_per_second: Option<f64>,
}

const RECORD_FILE: &str = "ai-verified.json";

pub fn load_records(data: &Path) -> BTreeMap<String, Record> {
    std::fs::read_to_string(data.join(RECORD_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Remember a self-test result. Only a recorded run can mark a model as tested.
pub fn record(data: &Path, id: &str, rec: Record) -> Result<(), String> {
    let mut all = load_records(data);
    all.insert(id.to_string(), rec);
    std::fs::create_dir_all(data).map_err(|e| e.to_string())?;
    let body = serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?;
    crate::store::write_atomic_private(&data.join(RECORD_FILE), body.as_bytes())
        .map_err(|e| e.to_string())
}

/// What to say about a model's test status (never "verified" without a record).
pub fn status_label(rec: Option<&Record>) -> String {
    match rec {
        Some(r) => format!("tested here: {} ({})", r.verdict, r.at),
        None => "not tested yet".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_catalog_parses_and_is_honest() {
        let cat = bundled();
        assert!(cat.len() >= 6);
        for tier in TIERS {
            assert!(
                cat.iter().any(|m| m.tier == *tier) || *tier == "apple16" || *tier == "apple32"
            );
        }
        for m in &cat {
            assert!(m.ctx_default <= m.ctx_native, "{}", m.id);
            assert!(is_unsloth_repo(&m.hf_repo), "{}", m.id);
            // No model is marked tested, or listed as live-checked, or carries a checksum,
            // until someone ran it (the catalog was written without Hub access).
            assert!(!m.verified, "{} must start unverified", m.id);
            assert!(m.sha256.is_none(), "{} has an unfetched checksum", m.id);
        }
    }

    #[test]
    fn bad_catalogs_are_refused() {
        let head = "version = 1\n[[model]]\n";
        let body = |extra: &str| {
            format!(
                "{head}id=\"a\"\nlabel=\"A\"\ntier=\"cpu8\"\nhf_repo=\"unsloth/A-GGUF\"\nfile=\"a.gguf\"\nquant=\"Q4\"\napprox_bytes=1\nctx_default=1\nctx_native=2\nlicense=\"mit\"\n{extra}"
            )
        };
        assert!(parse(&body("")).is_ok());
        let dup = format!(
            "{}\n{}",
            body(""),
            body("").replacen("version = 1\n", "", 1)
        );
        assert!(parse(&dup).unwrap_err().contains("duplicate"));
        assert!(parse(&body("").replace("cpu8", "huge"))
            .unwrap_err()
            .contains("tier"));
        assert!(parse(&body("").replace("ctx_default=1", "ctx_default=9"))
            .unwrap_err()
            .contains("ctx_default"));
        assert!(parse(&body("").replace("unsloth/A-GGUF", "someone/A-GGUF"))
            .unwrap_err()
            .contains("not unsloth"));
        assert!(parse(
            &body("allow_non_unsloth = true").replace("unsloth/A-GGUF", "someone/A-GGUF")
        )
        .is_ok());
        assert!(parse(&body("sha256 = \"abc\""))
            .unwrap_err()
            .contains("sha256"));
        assert!(parse(&body("").replace("a.gguf", "../a.gguf"))
            .unwrap_err()
            .contains("file"));
    }

    #[test]
    fn repo_names_follow_the_unsloth_shape() {
        assert!(is_unsloth_repo("unsloth/Qwen3.5-4B-GGUF"));
        assert!(!is_unsloth_repo("unsloth/Qwen3.5-4B"));
        assert!(!is_unsloth_repo("other/Qwen3.5-4B-GGUF"));
        assert!(!is_unsloth_repo("unsloth/-GGUF"));
        assert!(!is_unsloth_repo("unsloth/a b-GGUF"));
    }

    #[test]
    fn records_round_trip_and_label() {
        let dir = std::env::temp_dir().join(format!("ndcart-cat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(status_label(load_records(&dir).get("x")), "not tested yet");
        let rec = Record {
            verdict: "good".into(),
            at: "2026-11-18".into(),
            tokens_per_second: Some(20.0),
        };
        record(&dir, "x", rec.clone()).unwrap();
        assert_eq!(load_records(&dir).get("x"), Some(&rec));
        assert!(status_label(Some(&rec)).contains("good"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
