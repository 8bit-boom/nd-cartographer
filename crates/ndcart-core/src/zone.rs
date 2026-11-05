//! Zones: named groups of rooms ("Collapsed East Wing" contains these rooms).
//! They carry no geometry of their own, so they never change walls, tiles or
//! exports; they are for selecting, restyling and organising many rooms at once.

use crate::error::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};

pub const MAX_ZONES: usize = 500;
pub const MAX_ZONE_SPACES: usize = 2_000;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Zone {
    pub id: String,
    pub name: String,
    /// `#rrggbb`, for the outliner swatch.
    #[serde(default = "default_color")]
    pub color: String,
    /// Space ids. A room can belong to several zones.
    #[serde(default)]
    pub spaces: Vec<String>,
}

fn default_color() -> String {
    "#8a8f98".into()
}

impl Zone {
    pub fn validate(&self) -> CoreResult<()> {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !id_ok {
            return Err(CoreError::Invalid(
                "zone id must be 1-64 chars of a-z, 0-9, _ -".into(),
            ));
        }
        let n = self.name.chars().count();
        if n == 0 || n > 80 {
            return Err(CoreError::Invalid(
                "zone name must be 1-80 characters".into(),
            ));
        }
        let c = self.color.as_bytes();
        if !(c.len() == 7 && c[0] == b'#' && c[1..].iter().all(u8::is_ascii_hexdigit)) {
            return Err(CoreError::Invalid("zone color must be #rrggbb".into()));
        }
        if self.spaces.len() > MAX_ZONE_SPACES {
            return Err(CoreError::CapExceeded(format!(
                "a zone holds at most {MAX_ZONE_SPACES} rooms"
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for s in &self.spaces {
            if s.is_empty() || s.len() > 64 || !seen.insert(s) {
                return Err(CoreError::Invalid(
                    "zone rooms must be distinct, non-empty ids".into(),
                ));
            }
        }
        Ok(())
    }
}
