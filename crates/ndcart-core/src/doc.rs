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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub kind: DocKind,
    /// Pixels per cell for the battle lattice (region docs may ignore it).
    pub cell_size: f64,
    pub created_at: String,
    pub updated_at: String,
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
        }
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
            "{}: {cells} cells, {spaces} spaces, {} lights, {} labels",
            self.name,
            self.lights.len(),
            self.labels.len()
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
