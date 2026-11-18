//! An opt-in cache of accepted AI results (plan 3.5): asking the same thing twice should not
//! cost a second wait. Only a spec the judge accepted is stored, keyed by everything that
//! could change the answer: the model, the pack version, the prompts, the schema state.
//! Files live in `<data>/ai-cache/<hex>.json` (private), at most 200, oldest-used dropped first.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MAX_ENTRIES: usize = 200;

/// Where results are kept for one call.
#[derive(Clone, Default)]
pub struct Use {
    pub dir: Option<PathBuf>,
    /// "Ask again": do not read, but store the new answer.
    pub skip_read: bool,
}

/// The cache key: a hash over the parts, which are separated so no two lists collide.
pub fn key(parts: &[&str]) -> String {
    let mut buf = Vec::new();
    for p in parts {
        buf.extend_from_slice(p.as_bytes());
        buf.push(0x1f);
    }
    ndcart_core::hash::sha256_hex(&buf)
}

fn file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

pub fn get(dir: &Path, key: &str) -> Option<Value> {
    let p = file(dir, key);
    let v = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
    // Touch it so the least recently used goes first.
    if let Ok(f) = std::fs::OpenOptions::new().append(true).open(&p) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some(v)
}

pub fn put(dir: &Path, key: &str, value: &Value) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(value) else {
        return;
    };
    let _ = crate::store::write_atomic_private(&file(dir, key), &bytes);
    prune(dir, MAX_ENTRIES);
}

fn prune(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort();
    for (_, p) in files.iter().take(files.len() - keep) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_changes_with_any_part_and_parts_do_not_run_together() {
        let a = key(&["m", "1.1.0", "sys", "user"]);
        assert_eq!(a, key(&["m", "1.1.0", "sys", "user"]));
        assert_ne!(a, key(&["m", "1.2.0", "sys", "user"]), "pack version");
        assert_ne!(a, key(&["m2", "1.1.0", "sys", "user"]), "model");
        assert_ne!(key(&["ab", "c"]), key(&["a", "bc"]));
    }

    #[test]
    fn entries_round_trip_and_the_oldest_are_dropped() {
        let dir = std::env::temp_dir().join(format!("ndcart-aicache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        put(&dir, "a", &serde_json::json!({"x": 1}));
        assert_eq!(get(&dir, "a").unwrap()["x"], 1);
        assert!(get(&dir, "zz").is_none());
        for i in 0..5 {
            put(&dir, &format!("k{i}"), &serde_json::json!(i));
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        prune(&dir, 3);
        let left = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(left, 3);
        assert!(get(&dir, "k4").is_some() && get(&dir, "a").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
