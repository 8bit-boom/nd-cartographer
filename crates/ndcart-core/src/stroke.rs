//! Paint strokes (plan §5.3): painting is *data, not pixels*. A stroke is a
//! journal command — brush, material, pointer samples with pressure and tilt,
//! size, opacity, seed — and the pixels are a deterministic bake of that data,
//! so undo, replay and reproducible exports work exactly as for geometry.
//! Strokes live in document cell coordinates (floats) on top of the room grid
//! and are cosmetic: they never change walls, rooms or what nd-world publishes
//! except through the baked picture.

use crate::error::{CoreError, CoreResult, MAX_CELL};
use serde::{Deserialize, Serialize};

pub const MAX_STROKES: usize = 20_000;
pub const MAX_POINTS: usize = 4_000;
pub const MIN_SIZE: f32 = 0.05;
pub const MAX_SIZE: f32 = 64.0;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum StrokeKind {
    /// Lay material down.
    Paint,
    /// Reveal what is underneath the strokes (the room layer), softly.
    Erase,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum BrushKind {
    /// A soft or hard round brush; `roughness` hand-draws the edge.
    Round,
    /// Seeded scatter: pebbles, tufts, debris laid along the path.
    Scatter,
}

/// One pointer sample. Coordinates are document cells; `pressure` and `tilt`
/// are 0..=1 (a mouse reports pressure 1, tilt 0).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    #[serde(default = "one")]
    pub pressure: f32,
    #[serde(default)]
    pub tilt: f32,
}

fn one() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Stroke {
    pub id: String,
    pub kind: StrokeKind,
    pub brush: BrushKind,
    /// A pack material id; `None` paints the flat `color`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    /// `#rrggbb` — the colour when no material applies, and the fallback if
    /// the pack does not know the id.
    pub color: String,
    /// Brush diameter in cells at full pressure.
    pub size: f32,
    /// 0..=1 — how much each pixel's coverage is applied.
    pub opacity: f32,
    /// 0..=1 — 1 is a hard edge, 0 fades from the centre.
    #[serde(default = "one")]
    pub hardness: f32,
    /// 0..=1 — hand-drawn edge jitter.
    #[serde(default)]
    pub roughness: f32,
    /// 0..=1 — scatter density.
    #[serde(default = "half")]
    pub density: f32,
    /// Same seed + same points => the same pixels.
    pub seed: u32,
    pub points: Vec<StrokePoint>,
}

fn half() -> f32 {
    0.5
}

fn unit(v: f32, what: &str) -> CoreResult<()> {
    if v.is_finite() && (0.0..=1.0).contains(&v) {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!("{what} must be 0..=1")))
    }
}

impl Stroke {
    /// Every number is finite and bounded, ids and colours are well-formed.
    /// Nothing invalid is ever journaled (the D1 lesson).
    pub fn validate(&self) -> CoreResult<()> {
        if self.id.is_empty() || self.id.len() > 64 {
            return Err(CoreError::Invalid("stroke id must be 1-64 chars".into()));
        }
        if !self.size.is_finite() || !(MIN_SIZE..=MAX_SIZE).contains(&self.size) {
            return Err(CoreError::Invalid(format!(
                "stroke size must be {MIN_SIZE}..={MAX_SIZE} cells"
            )));
        }
        unit(self.opacity, "opacity")?;
        unit(self.hardness, "hardness")?;
        unit(self.roughness, "roughness")?;
        unit(self.density, "density")?;
        if self.points.is_empty() || self.points.len() > MAX_POINTS {
            return Err(CoreError::Invalid(format!(
                "a stroke has 1..={MAX_POINTS} points"
            )));
        }
        let lim = MAX_CELL as f32;
        for p in &self.points {
            if !(p.x.is_finite() && p.y.is_finite()) || p.x.abs() > lim || p.y.abs() > lim {
                return Err(CoreError::OutOfBounds(format!(
                    "stroke point ({}, {})",
                    p.x, p.y
                )));
            }
            unit(p.pressure, "pressure")?;
            unit(p.tilt, "tilt")?;
        }
        let c = self.color.as_bytes();
        if !(c.len() == 7 && c[0] == b'#' && c[1..].iter().all(u8::is_ascii_hexdigit)) {
            return Err(CoreError::Invalid("stroke color must be #rrggbb".into()));
        }
        if let Some(m) = &self.material {
            let ok = !m.is_empty()
                && m.len() <= 40
                && m.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !ok {
                return Err(CoreError::Invalid(
                    "material must be 1-40 chars of a-z, 0-9 and _".into(),
                ));
            }
        }
        Ok(())
    }

    /// Axis-aligned bounds of the pointer samples, in cells: `[x0, y0, x1, y1]`.
    pub fn point_bounds(&self) -> [f32; 4] {
        let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for p in &self.points {
            b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
        }
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> Stroke {
        Stroke {
            id: "st1".into(),
            kind: StrokeKind::Paint,
            brush: BrushKind::Round,
            material: Some("grass".into()),
            color: "#5a8a3c".into(),
            size: 2.0,
            opacity: 0.9,
            hardness: 0.7,
            roughness: 0.2,
            density: 0.5,
            seed: 7,
            points: vec![
                StrokePoint {
                    x: 1.0,
                    y: 1.0,
                    pressure: 0.4,
                    tilt: 0.0,
                },
                StrokePoint {
                    x: 5.0,
                    y: 2.0,
                    pressure: 1.0,
                    tilt: 0.3,
                },
            ],
        }
    }

    #[test]
    fn a_good_stroke_validates_and_roundtrips() {
        let s = sample();
        s.validate().unwrap();
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Stroke>(&j).unwrap(), s);
        // A bare client payload: defaults fill in (mouse = pressure 1, tilt 0).
        let bare = r##"{"id":"a","kind":"paint","brush":"round","color":"#112233","size":1,"opacity":1,
            "seed":1,"points":[{"x":0,"y":0},{"x":1,"y":1}]}"##;
        let b: Stroke = serde_json::from_str(bare).unwrap();
        b.validate().unwrap();
        assert_eq!(
            (b.points[0].pressure, b.points[0].tilt, b.hardness),
            (1.0, 0.0, 1.0)
        );
    }

    #[test]
    fn hostile_numbers_are_refused() {
        let mut bad: Vec<Stroke> = Vec::new();
        for f in [
            |s: &mut Stroke| s.size = f32::NAN,
            |s: &mut Stroke| s.size = 0.0,
            |s: &mut Stroke| s.size = 1e9,
            |s: &mut Stroke| s.opacity = 1.5,
            |s: &mut Stroke| s.hardness = -0.1,
            |s: &mut Stroke| s.points.clear(),
            |s: &mut Stroke| s.points[0].x = f32::INFINITY,
            |s: &mut Stroke| s.points[0].y = 1e7,
            |s: &mut Stroke| s.points[1].pressure = 2.0,
            |s: &mut Stroke| s.points[1].tilt = f32::NAN,
            |s: &mut Stroke| s.color = "red".into(),
            |s: &mut Stroke| s.material = Some("../x".into()),
            |s: &mut Stroke| s.id = String::new(),
            |s: &mut Stroke| s.points = vec![s.points[0]; MAX_POINTS + 1],
        ] {
            let mut s = sample();
            f(&mut s);
            bad.push(s);
        }
        for (i, s) in bad.iter().enumerate() {
            assert!(s.validate().is_err(), "case {i} should be refused");
        }
    }

    #[test]
    fn bounds_cover_the_points() {
        assert_eq!(sample().point_bounds(), [1.0, 1.0, 5.0, 2.0]);
    }
}
