//! Short, readable, collision-resistant-enough ids for documents and objects.
//!
//! Format: `<prefix>_<millis-base36><counter-base36>` — sortable by creation
//! time within a process, unique across restarts by the time component.
//! (A full ULID/uuid is not worth a dependency for M0.)

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU32 = AtomicU32::new(0);

const CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(CHARS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 chars are ascii")
}

/// A new id like `cell_mf2x9q3z04` (prefix + time + counter).
pub fn new_id(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}{}", base36(millis), base36(c as u64))
}

/// A lowercase slug from a name (the same rule nd-world uses): non
/// alphanumerics collapse to `-`, trimmed. Empty input yields "untitled".
pub fn slug_from_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "untitled".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_prefix_and_differ() {
        let a = new_id("w");
        let b = new_id("w");
        assert!(a.starts_with("w_"));
        assert_ne!(a, b);
    }

    #[test]
    fn slugs_match_nd_world_rule() {
        assert_eq!(slug_from_name("The Grand Hall"), "the-grand-hall");
        assert_eq!(slug_from_name("  --  "), "untitled");
        assert_eq!(slug_from_name("Крыша / Deck 3"), "deck-3");
    }
}
