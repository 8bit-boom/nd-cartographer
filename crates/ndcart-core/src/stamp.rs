//! Stamps: placed props (crates, tables, trees…) on top of the paint (plan
//! §7.1 "stamp placer"). A stamp is a placement of a pack asset — position in
//! cells, scale, rotation, flip, optional tint — and the pixels are a
//! deterministic bake. Like strokes they are cosmetic: they shape the
//! published picture, never walls or rooms.

use crate::error::{CoreError, CoreResult, MAX_CELL};
use serde::{Deserialize, Serialize};

pub const MAX_STAMPS: usize = 20_000;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Stamp {
    pub id: String,
    /// A pack stamp id (`crate`, `barrel`, …). An unknown id is simply not drawn.
    pub asset: String,
    /// Centre, in cells.
    pub x: f32,
    pub y: f32,
    /// Multiplier on the asset's natural size (cells).
    #[serde(default = "one")]
    pub scale: f32,
    /// Radians, clockwise on screen.
    #[serde(default)]
    pub rotation: f32,
    #[serde(default)]
    pub flip: bool,
    /// `#rrggbb` multiplied into the asset's colours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<String>,
    /// A soft drop shadow under the stamp.
    #[serde(default = "yes")]
    pub shadow: bool,
    /// How this prop came to be (plan 22.2): the generator that placed it,
    /// which room, its slot there, and whether the GM pinned it (a reroll
    /// keeps pinned and hand-placed props). None = placed by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gen: Option<Generation>,
}

fn one() -> f32 {
    1.0
}
fn yes() -> bool {
    true
}

impl Stamp {
    pub fn validate(&self) -> CoreResult<()> {
        let id_ok = |s: &str, max: usize| {
            !s.is_empty()
                && s.len() <= max
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        };
        if !id_ok(&self.id, 64) {
            return Err(CoreError::Invalid(
                "stamp id must be 1-64 chars of a-z, 0-9, _ -".into(),
            ));
        }
        if !id_ok(&self.asset, 40) {
            return Err(CoreError::Invalid(
                "stamp asset must be 1-40 chars of a-z, 0-9, _ -".into(),
            ));
        }
        let lim = MAX_CELL as f32;
        if !(self.x.is_finite() && self.y.is_finite()) || self.x.abs() > lim || self.y.abs() > lim {
            return Err(CoreError::OutOfBounds(format!(
                "stamp at ({}, {})",
                self.x, self.y
            )));
        }
        if !self.scale.is_finite() || !(0.1..=8.0).contains(&self.scale) {
            return Err(CoreError::Invalid("stamp scale must be 0.1..=8".into()));
        }
        if !self.rotation.is_finite() || self.rotation.abs() > 1000.0 {
            return Err(CoreError::Invalid("stamp rotation must be finite".into()));
        }
        if let Some(t) = &self.tint {
            let b = t.as_bytes();
            if !(b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)) {
                return Err(CoreError::Invalid("stamp tint must be #rrggbb".into()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Stamp {
        Stamp {
            id: "p1".into(),
            asset: "crate".into(),
            x: 3.5,
            y: 2.5,
            scale: 1.0,
            rotation: 0.3,
            gen: None,
            flip: false,
            tint: None,
            shadow: true,
        }
    }

    #[test]
    fn good_stamps_validate_and_defaults_fill_in() {
        sample().validate().unwrap();
        let bare: Stamp =
            serde_json::from_str(r#"{"id":"a","asset":"barrel","x":1,"y":2}"#).unwrap();
        bare.validate().unwrap();
        assert_eq!(
            (bare.scale, bare.rotation, bare.flip, bare.shadow),
            (1.0, 0.0, false, true)
        );
    }

    #[test]
    fn hostile_stamps_are_refused() {
        let cases: Vec<fn(&mut Stamp)> = vec![
            |s| s.id = String::new(),
            |s| s.id = "a b".into(),
            |s| s.asset = "../etc".into(),
            |s| s.x = f32::NAN,
            |s| s.y = 1e9,
            |s| s.scale = 0.0,
            |s| s.scale = 100.0,
            |s| s.rotation = f32::INFINITY,
            |s| s.tint = Some("red".into()),
        ];
        for (i, f) in cases.iter().enumerate() {
            let mut s = sample();
            f(&mut s);
            assert!(s.validate().is_err(), "case {i}");
        }
    }
}

/// Per-object generation provenance (plan 22.2): what placed this prop, so a
/// local reroll can replace exactly the generated clutter and keep pinned and
/// hand-placed work.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Generation {
    /// `furnish` today; scatter and others later.
    pub generator: String,
    /// The room the generator placed it for.
    pub room: String,
    /// The stable slot within the generator's plan (`table#0`).
    pub slot: String,
    #[serde(default)]
    pub pinned: bool,
}
