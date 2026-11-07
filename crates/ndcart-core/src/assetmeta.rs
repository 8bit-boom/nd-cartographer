//! Pack-asset metadata v2 (plan §22.1): how a prop sits, how much room it
//! needs, and what its license allows - the structured half of the `.ndpack`
//! idea. Everything has serde defaults, so a v1 manifest loads unchanged.
//!
//! Geometry convention: a footprint is a polygon in CELLS, centred on the
//! prop (the same convention the renderer uses for a stamp's x/y). It is
//! used as its bounding box for collisions; polygon-exact overlap is a
//! later refinement recorded in the plan.

use crate::error::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};

/// What a prop stands on or hangs from.
#[derive(
    Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Debug,
)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    #[default]
    Floor,
    Wall,
    Water,
    Air,
}

/// A licence field whose answer the pack author gave, or did not give.
#[derive(
    Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Debug,
)]
#[serde(rename_all = "lowercase")]
pub enum Rights {
    Allowed,
    Forbidden,
    #[default]
    Unknown,
}

/// The structured licence block (the plan's §22.1): facts a pack author
/// states, surfaced on import and in the publish report. It assists review;
/// it is not legal advice and it never claims more than the author wrote.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct License {
    /// Short name, e.g. `CC-BY-4.0` or `pack-private`.
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub commercial_map: Rights,
    #[serde(default)]
    pub redistribution: Rights,
    #[serde(default)]
    pub attribution_required: bool,
    /// Where the real terms live in the pack (a file name), or a short quote.
    #[serde(default)]
    pub terms: String,
}

/// How a prop prefers to be placed (furnishing rules and future tools read
/// this; the GM can always place by hand however they like).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Placement {
    #[serde(default)]
    pub surface: Surface,
    /// Beds and cupboards face into the room: furnishing prefers a wall.
    #[serde(default)]
    pub align_to_wall: bool,
    #[serde(default = "yes")]
    pub allow_mirror: bool,
}

fn yes() -> bool {
    true
}

impl Default for Placement {
    fn default() -> Self {
        Placement {
            surface: Surface::Floor,
            align_to_wall: false,
            allow_mirror: true,
        }
    }
}

/// A footprint polygon in cells, centred on the prop.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Footprint(pub Vec<[f32; 2]>);

impl Footprint {
    pub fn validate(&self) -> CoreResult<()> {
        if self.0.len() < 3 {
            return Err(CoreError::Invalid(
                "a footprint needs at least 3 points".into(),
            ));
        }
        if self.0.len() > 32 {
            return Err(CoreError::Invalid(
                "a footprint has at most 32 points".into(),
            ));
        }
        let lim = 20.0f32;
        for p in &self.0 {
            if p.len() != 2
                || !p[0].is_finite()
                || !p[1].is_finite()
                || p[0].abs() > lim
                || p[1].abs() > lim
            {
                return Err(CoreError::Invalid(
                    "footprint points must be finite and within ±20 cells".into(),
                ));
            }
        }
        Ok(())
    }

    /// The axis-aligned box that contains it, as (w, h) in cells. A footprint
    /// is centred, so collisions take the box from the prop's centre.
    pub fn aabb(&self) -> [f32; 2] {
        let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
        let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
        for p in &self.0 {
            min_x = min_x.min(p[0]);
            min_y = min_y.min(p[1]);
            max_x = max_x.max(p[0]);
            max_y = max_y.max(p[1]);
        }
        [(max_x - min_x).max(1.0), (max_y - min_y).max(1.0)]
    }
}

/// Everything v2 says about one asset.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct AssetMeta {
    /// None = unknown: collisions fall back to the natural size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint: Option<Footprint>,
    /// Free space to keep clear around the footprint, in cells (0..=5).
    #[serde(default)]
    pub clearance: f32,
    #[serde(default)]
    pub placement: Placement,
    /// How this asset is styled (`ink`, `neon`, `hand-drawn`), for
    /// style-matching later; never used for search (that is `tags`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub style_tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<License>,
    /// The pack it came from, for the rights audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
}

pub const MAX_STYLE_TAGS: usize = 8;

impl AssetMeta {
    pub fn is_default(&self) -> bool {
        *self == AssetMeta::default()
    }

    pub fn validate(&self) -> CoreResult<()> {
        if let Some(f) = &self.footprint {
            f.validate()?;
        }
        if !self.clearance.is_finite() || !(0.0..=5.0).contains(&self.clearance) {
            return Err(CoreError::Invalid("clearance must be 0..=5 cells".into()));
        }
        if self.style_tags.len() > MAX_STYLE_TAGS {
            return Err(CoreError::CapExceeded(format!(
                "at most {MAX_STYLE_TAGS} style tags"
            )));
        }
        if self.style_tags.iter().any(|t| t.len() > 24) {
            return Err(CoreError::Invalid("style tags are at most 24 chars".into()));
        }
        if let Some(l) = &self.license {
            if l.identifier.len() > 40 || l.terms.len() > 4_000 {
                return Err(CoreError::Invalid(
                    "licence identifier is at most 40 chars, terms at most 4000".into(),
                ));
            }
        }
        if let Some(p) = &self.pack {
            if p.is_empty() || p.len() > 80 {
                return Err(CoreError::Invalid("pack name must be 1..=80 chars".into()));
            }
        }
        Ok(())
    }

    /// The collision box in cells at `scale`: the footprint's box when known,
    /// else the natural size (a prop of unknown shape collides as a box).
    pub fn collision_box(&self, natural: [f32; 2], scale: f32) -> [f32; 2] {
        let base = self.footprint.as_ref().map(|f| f.aabb()).unwrap_or(natural);
        [(base[0] * scale).max(1.0), (base[1] * scale).max(1.0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_v1_behaviour() {
        let m = AssetMeta::default();
        assert!(m.is_default());
        assert!(m.validate().is_ok());
        // Unknown shape: collision is the natural size, at least a cell.
        assert_eq!(m.collision_box([2.0, 1.0], 1.0), [2.0, 1.0]);
        assert_eq!(m.collision_box([0.3, 0.3], 1.0), [1.0, 1.0]);
    }

    #[test]
    fn footprints_validate_and_box() {
        let good = Footprint(vec![[-1.0, -0.4], [1.0, -0.4], [1.0, 0.4], [-1.0, 0.4]]);
        assert!(good.validate().is_ok());
        // The box never drops below a cell: a prop occupies at least one.
        assert_eq!(good.aabb(), [2.0, 1.0]);
        assert!(Footprint(vec![[0.0, 0.0]]).validate().is_err());
        assert!(Footprint(vec![[0.0, 0.0], [99.0, 0.0], [0.0, 99.0]])
            .validate()
            .is_err());
        assert!(Footprint(vec![[0.0, 0.0], [f32::NAN, 0.0], [0.0, 5.0]])
            .validate()
            .is_err());
    }

    #[test]
    fn v1_json_loads_without_the_new_fields() {
        #[derive(Serialize, Deserialize)]
        struct Holder {
            #[serde(default)]
            meta: AssetMeta,
        }
        let v1: Holder = serde_json::from_str("{}").unwrap();
        assert!(v1.meta.is_default());
    }
}
