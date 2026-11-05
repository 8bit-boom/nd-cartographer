//! Document model: a project (`.ndmap` in the plan; M0 keeps one floor per
//! document) holds a manifest + floor docs (grid, lights, labels).

use crate::error::{CoreError, CoreResult};
use crate::grid::RoomGrid;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum DocKind {
    Battle,
    Region,
    Schematic,
}

impl DocKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            DocKind::Battle => "battle",
            DocKind::Region => "region",
            DocKind::Schematic => "schematic",
        }
    }
}

/// Where this document was last published (plan §9: slug capture, so a
/// re-publish updates the same map in place). Pinned to one server + world.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct NdWorldLink {
    pub server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
    pub slug: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub kind: DocKind,
    /// Pixels per cell for the battle lattice (region docs may ignore it).
    pub cell_size: f64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nd_world: Option<NdWorldLink>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Light {
    pub id: String,
    pub x: f64,
    pub y: f64,
    /// Range in squares (grid cells) — nd-world's unit.
    pub range: f64,
    pub color: String,
    /// 0.1..=1.0 (nd-world's accepted band).
    pub intensity: f64,
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Label {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub text: String,
    pub size: f64,
    pub color: String,
    pub bold: bool,
    /// A GM-only note: never leaves the studio in a player-facing output.
    #[serde(default)]
    pub gm_only: bool,
}

/// A raster picture behind everything (imported art, an opened `.dd2vtt`'s map
/// image, later an AI generation): plan §5.3 "image underlays remain referenced
/// assets with blend mode + opacity". The bytes live in the asset store, named
/// by their SHA-256; the document holds only the reference.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Underlay {
    /// 64 lowercase hex digits: the asset's SHA-256.
    pub asset: String,
    /// Top-left corner and size, in cells.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default = "full")]
    pub opacity: f32,
}

fn full() -> f32 {
    1.0
}

impl Underlay {
    pub fn validate(&self) -> CoreResult<()> {
        if !crate::hash::is_asset_id(&self.asset) {
            return Err(CoreError::Invalid(
                "underlay asset must be a sha256 id".into(),
            ));
        }
        let lim = crate::error::MAX_CELL as f32;
        for (v, what) in [
            (self.x, "x"),
            (self.y, "y"),
            (self.w, "w"),
            (self.h, "h"),
            (self.opacity, "opacity"),
        ] {
            if !v.is_finite() {
                return Err(CoreError::Invalid(format!(
                    "underlay {what} must be finite"
                )));
            }
        }
        if self.x.abs() > lim || self.y.abs() > lim {
            return Err(CoreError::OutOfBounds("underlay position".into()));
        }
        if !(0.01..=20_000.0).contains(&self.w) || !(0.01..=20_000.0).contains(&self.h) {
            return Err(CoreError::Invalid(
                "underlay size must be 0.01..=20000 cells".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err(CoreError::Invalid("underlay opacity must be 0..=1".into()));
        }
        Ok(())
    }
}

/// A wall that is not derived from rooms — traced or imported (a `.dd2vtt`'s
/// line of sight). Coordinates are cells, so it does not depend on the lattice
/// size. Derived walls stay the single source for painted rooms (D7); these
/// exist because an opened Dungeondraft map has walls but no rooms.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ImportedWall {
    pub pts: Vec<[f32; 2]>,
    pub kind: crate::walls::WallKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<crate::walls::WallState>,
}

pub const MAX_IMPORTED_WALLS: usize = 100_000;
pub const MAX_IMPORTED_POINTS: usize = 400_000;

impl ImportedWall {
    pub fn validate(&self) -> CoreResult<()> {
        if self.pts.len() < 2 {
            return Err(CoreError::Invalid(
                "an imported wall needs 2+ points".into(),
            ));
        }
        let lim = crate::error::MAX_CELL as f32;
        for p in &self.pts {
            if !(p[0].is_finite() && p[1].is_finite()) || p[0].abs() > lim || p[1].abs() > lim {
                return Err(CoreError::OutOfBounds("imported wall point".into()));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct FloorDoc {
    pub id: String,
    pub name: String,
    pub grid: RoomGrid,
    #[serde(default)]
    pub lights: Vec<Light>,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// Paint strokes in z-order (later draws over earlier). Cosmetic: they
    /// shape the baked picture, never walls or rooms.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strokes: Vec<crate::stroke::Stroke>,
    /// Placed props in z-order, drawn over the strokes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stamps: Vec<crate::stamp::Stamp>,
    /// A picture behind everything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlay: Option<Underlay>,
    /// Walls that are not derived from rooms (see [`ImportedWall`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imported_walls: Vec<ImportedWall>,
}

pub fn now_iso() -> String {
    // A wall-clock stamp without a chrono dependency: epoch seconds marked
    // clearly as an M0 encoding. A real RFC3339 writer arrives with the
    // manifest work (the plan's M5 docs pass).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch+{secs}")
}

fn finite_ok(v: f64, what: &str) -> CoreResult<()> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(CoreError::Invalid(format!("{what} must be finite")))
    }
}

impl FloorDoc {
    pub fn new(id: String, name: String) -> Self {
        FloorDoc {
            id,
            name,
            grid: RoomGrid::default(),
            lights: Vec::new(),
            labels: Vec::new(),
            strokes: Vec::new(),
            stamps: Vec::new(),
            underlay: None,
            imported_walls: Vec::new(),
        }
    }

    /// Add a stroke (validated), at `index` in the z-order or on top.
    pub fn add_stroke(
        &mut self,
        stroke: crate::stroke::Stroke,
        index: Option<usize>,
    ) -> CoreResult<()> {
        stroke.validate()?;
        if self.strokes.iter().any(|s| s.id == stroke.id) {
            return Err(CoreError::Invalid(format!(
                "duplicate stroke {}",
                stroke.id
            )));
        }
        if self.strokes.len() >= crate::stroke::MAX_STROKES {
            return Err(CoreError::CapExceeded(format!(
                "at most {} strokes per floor",
                crate::stroke::MAX_STROKES
            )));
        }
        let at = index.unwrap_or(self.strokes.len()).min(self.strokes.len());
        self.strokes.insert(at, stroke);
        Ok(())
    }

    pub fn set_underlay(&mut self, underlay: Option<Underlay>) -> CoreResult<()> {
        if let Some(u) = &underlay {
            u.validate()?;
        }
        self.underlay = underlay;
        Ok(())
    }

    pub fn set_imported_walls(&mut self, walls: Vec<ImportedWall>) -> CoreResult<()> {
        if walls.len() > MAX_IMPORTED_WALLS {
            return Err(CoreError::CapExceeded(format!(
                "at most {MAX_IMPORTED_WALLS} imported walls"
            )));
        }
        if walls.iter().map(|w| w.pts.len()).sum::<usize>() > MAX_IMPORTED_POINTS {
            return Err(CoreError::CapExceeded(format!(
                "at most {MAX_IMPORTED_POINTS} imported wall points"
            )));
        }
        for w in &walls {
            w.validate()?;
        }
        self.imported_walls = walls;
        Ok(())
    }

    /// The smallest `cols x rows` (cells, from the origin) that holds
    /// everything on the floor: painted cells, the underlay, strokes (with their
    /// reach) and stamps. Never smaller than 1x1; negative coordinates are
    /// outside the exported image.
    pub fn extent(&self) -> (u32, u32) {
        let (mut cols, mut rows) = (1.0f32, 1.0f32);
        for (c, _) in self.grid.cells() {
            cols = cols.max(c.x as f32 + 1.0);
            rows = rows.max(c.y as f32 + 1.0);
        }
        if let Some(u) = &self.underlay {
            cols = cols.max(u.x + u.w);
            rows = rows.max(u.y + u.h);
        }
        for s in &self.strokes {
            let b = s.point_bounds();
            cols = cols.max(b[2] + s.size);
            rows = rows.max(b[3] + s.size);
        }
        for s in &self.stamps {
            // The prop catalogue lives outside core, so use a generous reach
            // (the largest stamp is 3 cells at scale 1).
            let reach = 2.0 * s.scale + 1.0;
            cols = cols.max(s.x + reach);
            rows = rows.max(s.y + reach);
        }
        let cap = 20_000.0;
        (
            cols.ceil().clamp(1.0, cap) as u32,
            rows.ceil().clamp(1.0, cap) as u32,
        )
    }

    pub fn add_stamp(
        &mut self,
        stamp: crate::stamp::Stamp,
        index: Option<usize>,
    ) -> CoreResult<()> {
        stamp.validate()?;
        if self.stamps.iter().any(|s| s.id == stamp.id) {
            return Err(CoreError::Invalid(format!("duplicate stamp {}", stamp.id)));
        }
        if self.stamps.len() >= crate::stamp::MAX_STAMPS {
            return Err(CoreError::CapExceeded(format!(
                "at most {} stamps per floor",
                crate::stamp::MAX_STAMPS
            )));
        }
        let at = index.unwrap_or(self.stamps.len()).min(self.stamps.len());
        self.stamps.insert(at, stamp);
        Ok(())
    }

    /// Replace a placed stamp in place (move / rotate / re-tint), keeping its z-order.
    pub fn update_stamp(&mut self, stamp: crate::stamp::Stamp) -> CoreResult<()> {
        stamp.validate()?;
        let slot = self
            .stamps
            .iter_mut()
            .find(|s| s.id == stamp.id)
            .ok_or_else(|| CoreError::NotFound(format!("stamp {}", stamp.id)))?;
        *slot = stamp;
        Ok(())
    }

    pub fn remove_stamp(&mut self, id: &str) -> CoreResult<()> {
        let i = self
            .stamps
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| CoreError::NotFound(format!("stamp {id}")))?;
        self.stamps.remove(i);
        Ok(())
    }

    pub fn remove_stroke(&mut self, id: &str) -> CoreResult<()> {
        let i = self
            .strokes
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| CoreError::NotFound(format!("stroke {id}")))?;
        self.strokes.remove(i);
        Ok(())
    }

    pub fn validate_light(light: &Light) -> CoreResult<()> {
        finite_ok(light.x, "light x")?;
        finite_ok(light.y, "light y")?;
        finite_ok(light.range, "light range")?;
        finite_ok(light.intensity, "light intensity")?;
        if !(0.1..=1.0).contains(&light.intensity) {
            return Err(CoreError::Invalid("intensity must be 0.1..=1.0".into()));
        }
        if light.range <= 0.0 || light.range > 60.0 {
            // nd-world's import-uvtt / lights route cap
            return Err(CoreError::Invalid("range must be 0 < range <= 60".into()));
        }
        if light.color.len() != 7 || !light.color.starts_with('#') {
            return Err(CoreError::Invalid("light color must be #rrggbb".into()));
        }
        Ok(())
    }

    pub fn add_light(&mut self, light: Light) -> CoreResult<()> {
        Self::validate_light(&light)?;
        if self.lights.iter().any(|l| l.id == light.id) {
            return Err(CoreError::Invalid(format!("duplicate light {}", light.id)));
        }
        if self.lights.len() >= crate::error::MAX_LIGHTS {
            return Err(CoreError::CapExceeded(format!(
                "at most {} lights (nd-world publish cap)",
                crate::error::MAX_LIGHTS
            )));
        }
        self.lights.push(light);
        Ok(())
    }

    pub fn remove_light(&mut self, id: &str) -> CoreResult<()> {
        let before = self.lights.len();
        self.lights.retain(|l| l.id != id);
        if self.lights.len() == before {
            return Err(CoreError::NotFound(format!("light {id}")));
        }
        Ok(())
    }

    pub fn validate_label(label: &Label) -> CoreResult<()> {
        finite_ok(label.x, "label x")?;
        finite_ok(label.y, "label y")?;
        finite_ok(label.size, "label size")?;
        if !(2.0..=400.0).contains(&label.size) {
            return Err(CoreError::Invalid("size must be 2..=400".into()));
        }
        if label.text.len() > 200 {
            return Err(CoreError::Invalid(
                "label text is capped at 200 chars".into(),
            ));
        }
        if !label.color.starts_with('#') {
            return Err(CoreError::Invalid("label color must be #rrggbb".into()));
        }
        Ok(())
    }

    pub fn upsert_label(&mut self, label: Label) -> CoreResult<()> {
        Self::validate_label(&label)?;
        if let Some(existing) = self.labels.iter_mut().find(|l| l.id == label.id) {
            *existing = label;
        } else {
            if self.labels.len() >= 2_000 {
                return Err(CoreError::CapExceeded("at most 2000 labels".into()));
            }
            self.labels.push(label);
        }
        Ok(())
    }

    pub fn remove_label(&mut self, id: &str) -> CoreResult<()> {
        let before = self.labels.len();
        self.labels.retain(|l| l.id != id);
        if self.labels.len() == before {
            return Err(CoreError::NotFound(format!("label {id}")));
        }
        Ok(())
    }

    /// One line per floor for AI prompts and the outliner (the
    /// `pc_digest.py` idea, applied to floors).
    pub fn digest(&self) -> String {
        let spaces = self.grid.spaces.len();
        let cells = self.grid.cell_count();
        format!(
            "{}: {cells} cells, {spaces} spaces, {} lights, {} labels, {} strokes, {} stamps",
            self.name,
            self.lights.len(),
            self.labels.len(),
            self.strokes.len(),
            self.stamps.len()
        )
    }
}

/// Find a floor's index by id.
pub fn floors_by_id(floors: &[FloorDoc], id: &str) -> CoreResult<usize> {
    floors
        .iter()
        .position(|f| f.id == id)
        .ok_or_else(|| CoreError::NotFound(format!("floor {id}")))
}

#[cfg(test)]
mod layer_tests {
    use super::*;
    use crate::grid::{default_space, CellKey, SpaceKind};
    use crate::journal::{Journal, Op};
    use crate::walls::{floor_walls, WallKind};

    fn underlay() -> Underlay {
        Underlay {
            asset: crate::hash::sha256_hex(b"pic"),
            x: 0.0,
            y: 0.0,
            w: 30.0,
            h: 20.0,
            opacity: 0.8,
        }
    }

    #[test]
    fn underlay_is_validated_and_undoable() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        j.push(
            &mut f,
            &Op::SetUnderlay {
                underlay: Some(underlay()),
            },
        )
        .unwrap();
        assert_eq!(f.underlay.as_ref().unwrap().w, 30.0);
        // Replace, then undo restores the first; clearing and undoing restores too.
        let mut u2 = underlay();
        u2.opacity = 0.2;
        j.push(&mut f, &Op::SetUnderlay { underlay: Some(u2) })
            .unwrap();
        j.undo_last(&mut f).unwrap();
        assert_eq!(f.underlay.as_ref().unwrap().opacity, 0.8);
        j.push(&mut f, &Op::SetUnderlay { underlay: None }).unwrap();
        assert!(f.underlay.is_none());
        j.undo_last(&mut f).unwrap();
        assert!(f.underlay.is_some());
        // Hostile values are refused and never journaled.
        let n = j.len();
        for bad in [
            |u: &mut Underlay| u.asset = "../../etc/passwd".into(),
            |u: &mut Underlay| u.w = 0.0,
            |u: &mut Underlay| u.w = 1e9,
            |u: &mut Underlay| u.x = f32::NAN,
            |u: &mut Underlay| u.opacity = 2.0,
        ] {
            let mut u = underlay();
            bad(&mut u);
            assert!(j
                .push(&mut f, &Op::SetUnderlay { underlay: Some(u) })
                .is_err());
        }
        assert_eq!(j.len(), n);
    }

    #[test]
    fn imported_walls_join_the_derived_ones_in_pixels_with_stable_ids() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.grid
            .upsert_space(default_space("a", "A", SpaceKind::Room))
            .unwrap();
        f.grid.paint(&[CellKey::new(0, 0)], Some("a")).unwrap();
        let derived = floor_walls(&f, 64.0).unwrap().len();
        assert_eq!(derived, 4);
        let mut j = Journal::default();
        j.push(
            &mut f,
            &Op::SetImportedWalls {
                walls: vec![
                    ImportedWall {
                        pts: vec![[2.0, 0.0], [2.0, 3.0]],
                        kind: WallKind::Wall,
                        state: None,
                    },
                    ImportedWall {
                        pts: vec![[5.0, 1.0], [6.0, 1.0]],
                        kind: WallKind::Door,
                        state: Some(crate::walls::WallState::Open),
                    },
                ],
            },
        )
        .unwrap();
        let w = floor_walls(&f, 64.0).unwrap();
        assert_eq!(w.len(), derived + 2);
        let door = w.iter().find(|x| x.id == "iw1").unwrap();
        assert_eq!(
            (door.kind, door.pts[0], door.pts[1]),
            (WallKind::Door, [320.0, 64.0], [384.0, 64.0])
        );
        assert_eq!(
            floor_walls(&f, 64.0).unwrap(),
            w,
            "deterministic: same ids and points"
        );
        j.undo_last(&mut f).unwrap();
        assert!(f.imported_walls.is_empty());
        // Refused: one point, NaN, absurd size.
        for bad in [
            ImportedWall {
                pts: vec![[0.0, 0.0]],
                kind: WallKind::Wall,
                state: None,
            },
            ImportedWall {
                pts: vec![[0.0, 0.0], [f32::NAN, 1.0]],
                kind: WallKind::Wall,
                state: None,
            },
            ImportedWall {
                pts: vec![[0.0, 0.0], [1e9, 1.0]],
                kind: WallKind::Wall,
                state: None,
            },
        ] {
            assert!(j
                .push(&mut f, &Op::SetImportedWalls { walls: vec![bad] })
                .is_err());
        }
    }

    #[test]
    fn extent_holds_every_layer() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        assert_eq!(f.extent(), (1, 1));
        f.grid
            .upsert_space(default_space("a", "A", SpaceKind::Room))
            .unwrap();
        f.grid.paint(&[CellKey::new(4, 2)], Some("a")).unwrap();
        assert_eq!(f.extent(), (5, 3));
        f.set_underlay(Some(underlay())).unwrap();
        assert_eq!(f.extent(), (30, 20));
        use crate::stroke::{BrushKind, Stroke, StrokeKind, StrokePoint};
        f.add_stroke(
            Stroke {
                id: "s".into(),
                kind: StrokeKind::Paint,
                brush: BrushKind::Round,
                material: None,
                color: "#112233".into(),
                size: 2.0,
                opacity: 1.0,
                hardness: 1.0,
                roughness: 0.0,
                density: 0.5,
                seed: 1,
                points: vec![StrokePoint {
                    x: 40.0,
                    y: 5.0,
                    pressure: 1.0,
                    tilt: 0.0,
                }],
            },
            None,
        )
        .unwrap();
        assert_eq!(
            f.extent(),
            (42, 20),
            "a stroke beyond the rooms is inside the export"
        );
        f.add_stamp(
            crate::stamp::Stamp {
                id: "p".into(),
                asset: "tree".into(),
                x: 10.0,
                y: 50.0,
                scale: 1.0,
                rotation: 0.0,
                flip: false,
                tint: None,
                shadow: true,
            },
            None,
        )
        .unwrap();
        assert_eq!(f.extent().1, 53);
    }
}
