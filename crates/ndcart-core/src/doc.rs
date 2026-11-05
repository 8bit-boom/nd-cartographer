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
    /// Slug of each floor published on its own (nd-world has no multi-floor
    /// concept, so every floor is its own map). `slug` stays the first/only one.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub floor_slugs: std::collections::BTreeMap<String, String>,
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

/// Text on the map (plan §7.2 typography). Position and size are in document
/// **cells** (`size` is the font's height), so a label scales with the map. A
/// label is either straight (centred on `x, y`, turned by `rotation`) or, with a
/// `path`, set along that polyline — "a river's name follows the river".
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Label {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub text: String,
    /// Font height in cells, 0.1..=40.
    pub size: f64,
    pub color: String,
    pub bold: bool,
    /// A GM-only note: never leaves the studio in a player-facing output.
    #[serde(default)]
    pub gm_only: bool,
    /// Set the text along this polyline (cells); empty means a straight label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<[f32; 2]>,
    /// A contrasting outline behind the letters so text reads over any ground.
    #[serde(default = "halo_on")]
    pub halo: bool,
    /// Extra letter spacing in em, 0..=1 (small caps and regions look better spaced).
    #[serde(default)]
    pub tracking: f32,
    /// Radians, clockwise on screen (straight labels only).
    #[serde(default)]
    pub rotation: f32,
}

fn halo_on() -> bool {
    true
}

impl Label {
    /// A label with sensible defaults (straight, haloed, regular weight).
    pub fn new(id: &str, x: f64, y: f64, text: &str) -> Label {
        Label {
            id: id.into(),
            x,
            y,
            text: text.into(),
            size: 0.8,
            color: "#222222".into(),
            bold: false,
            gm_only: false,
            path: Vec::new(),
            halo: true,
            tracking: 0.0,
            rotation: 0.0,
        }
    }
}

/// A lattice over the map. `size` is the width of one square, or the
/// flat-to-flat width of one pointy-top hex, in cells.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct MapGrid {
    pub kind: MapGridKind,
    pub size: f32,
    #[serde(default = "grid_colour")]
    pub color: String,
    #[serde(default = "grid_opacity")]
    pub opacity: f32,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum MapGridKind {
    Square,
    Hex,
}

fn grid_colour() -> String {
    "#ffffff".into()
}
fn grid_opacity() -> f32 {
    0.35
}

impl MapGrid {
    pub fn validate(&self) -> CoreResult<()> {
        if !self.size.is_finite() || !(0.5..=200.0).contains(&self.size) {
            return Err(CoreError::Invalid("grid size must be 0.5-200 cells".into()));
        }
        if !self.opacity.is_finite() || !(0.05..=1.0).contains(&self.opacity) {
            return Err(CoreError::Invalid("grid opacity must be 0.05-1".into()));
        }
        let c = self.color.as_bytes();
        if !(c.len() == 7 && c[0] == b'#' && c[1..].iter().all(u8::is_ascii_hexdigit)) {
            return Err(CoreError::Invalid("grid color must be #rrggbb".into()));
        }
        Ok(())
    }
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<crate::zone::Zone>,
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
    /// Region maps: the heightmap (plan §7.2). `None` until first edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terrain: Option<crate::terrain::Terrain>,
    /// Rivers: carved into the terrain when added, drawn as water on top.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rivers: Vec<crate::terrain::River>,
    /// An explicit map size in cells `[cols, rows]` (region maps): the export is
    /// at least this big even where nothing is painted (open sea, empty land).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<[u32; 2]>,
    /// A square or hex lattice drawn over the map (travel distances, hex crawls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_grid: Option<MapGrid>,
    /// Points of interest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<crate::overlay::Marker>,
    /// Political regions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<crate::overlay::Region>,
    /// Roads, trade routes, journeys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<crate::overlay::Route>,
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
            zones: Vec::new(),
            map_grid: None,
            strokes: Vec::new(),
            stamps: Vec::new(),
            underlay: None,
            imported_walls: Vec::new(),
            terrain: None,
            rivers: Vec::new(),
            canvas: None,
            markers: Vec::new(),
            regions: Vec::new(),
            routes: Vec::new(),
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

    /// The terrain, created (open ocean) on first use.
    pub fn terrain_mut(&mut self) -> &mut crate::terrain::Terrain {
        self.terrain
            .get_or_insert_with(crate::terrain::Terrain::default)
    }

    pub fn terrain_stroke(&mut self, s: &crate::terrain::TerrainStroke) -> CoreResult<()> {
        s.validate()?;
        self.terrain_mut()
            .stroke(s.mode, &s.points, s.radius, s.strength, s.seed);
        Ok(())
    }

    /// Raw chunk replacement (undo, generators). `fill` is the open-water level;
    /// it can only be chosen when the terrain is created.
    pub fn terrain_set(
        &mut self,
        fill: u16,
        chunks: &[crate::terrain::ChunkData],
    ) -> CoreResult<()> {
        match &self.terrain {
            Some(t) if t.fill != fill => {
                return Err(CoreError::Invalid(
                    "the open-water level cannot change on existing terrain".into(),
                ))
            }
            _ => {}
        }
        // Validate on a scratch copy so a bad chunk leaves the terrain untouched.
        let mut t = self.terrain.clone().unwrap_or_else(|| {
            let mut t = crate::terrain::Terrain::default();
            t.fill = fill;
            t
        });
        t.set_chunks(chunks)?;
        self.terrain = Some(t);
        Ok(())
    }

    pub fn clear_terrain(&mut self) {
        self.terrain = None;
    }

    pub fn set_terrain_style(&mut self, style: crate::terrain::TerrainStyle) -> CoreResult<()> {
        style.validate()?;
        self.terrain_mut().style = style;
        Ok(())
    }

    /// Add a river (validated). With `carve`, the channel is cut into the terrain;
    /// undo uses `carve: false` to put a removed river back without cutting twice.
    pub fn add_river(
        &mut self,
        river: crate::terrain::River,
        carve: bool,
        index: Option<usize>,
    ) -> CoreResult<()> {
        river.validate()?;
        if self.rivers.iter().any(|r| r.id == river.id) {
            return Err(CoreError::Invalid(format!("duplicate river {}", river.id)));
        }
        if self.rivers.len() >= crate::terrain::MAX_RIVERS {
            return Err(CoreError::CapExceeded(format!(
                "at most {} rivers",
                crate::terrain::MAX_RIVERS
            )));
        }
        if carve {
            self.terrain_mut().carve_river(&river);
        }
        let at = index.unwrap_or(self.rivers.len()).min(self.rivers.len());
        self.rivers.insert(at, river);
        Ok(())
    }

    pub fn remove_river(&mut self, id: &str) -> CoreResult<()> {
        let i = self
            .rivers
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| CoreError::NotFound(format!("river {id}")))?;
        self.rivers.remove(i);
        Ok(())
    }

    /// Add or replace an overlay item (same kind and id replaces in place, keeping
    /// its z-order; otherwise it is inserted at `index` or appended).
    pub fn put_overlay(
        &mut self,
        item: crate::overlay::OverlayItem,
        index: Option<usize>,
    ) -> CoreResult<()> {
        use crate::overlay::{OverlayItem as I, MAX_MARKERS, MAX_REGIONS, MAX_ROUTES};
        item.validate()?;
        macro_rules! put {
            ($list:expr, $v:expr, $cap:expr, $what:expr) => {{
                if let Some(slot) = $list.iter_mut().find(|x| x.id == $v.id) {
                    *slot = $v;
                } else {
                    if $list.len() >= $cap {
                        return Err(CoreError::CapExceeded(format!(
                            "at most {} {}",
                            $cap, $what
                        )));
                    }
                    let at = index.unwrap_or($list.len()).min($list.len());
                    $list.insert(at, $v);
                }
            }};
        }
        match item {
            I::Marker(m) => put!(self.markers, m, MAX_MARKERS, "markers"),
            I::Region(r) => put!(self.regions, r, MAX_REGIONS, "regions"),
            I::Route(r) => put!(self.routes, r, MAX_ROUTES, "routes"),
        }
        Ok(())
    }

    pub fn remove_overlay(
        &mut self,
        kind: crate::overlay::OverlayKind,
        id: &str,
    ) -> CoreResult<()> {
        use crate::overlay::OverlayKind as K;
        macro_rules! rm {
            ($list:expr) => {{
                let i = $list
                    .iter()
                    .position(|x| x.id == id)
                    .ok_or_else(|| CoreError::NotFound(format!("{kind:?} {id}")))?;
                $list.remove(i);
            }};
        }
        match kind {
            K::Marker => rm!(self.markers),
            K::Region => rm!(self.regions),
            K::Route => rm!(self.routes),
        }
        Ok(())
    }

    /// The overlay item with this kind and id, and its z-index.
    pub fn find_overlay(
        &self,
        kind: crate::overlay::OverlayKind,
        id: &str,
    ) -> Option<(usize, crate::overlay::OverlayItem)> {
        use crate::overlay::{OverlayItem as I, OverlayKind as K};
        match kind {
            K::Marker => self
                .markers
                .iter()
                .position(|x| x.id == id)
                .map(|i| (i, I::Marker(self.markers[i].clone()))),
            K::Region => self
                .regions
                .iter()
                .position(|x| x.id == id)
                .map(|i| (i, I::Region(self.regions[i].clone()))),
            K::Route => self
                .routes
                .iter()
                .position(|x| x.id == id)
                .map(|i| (i, I::Route(self.routes[i].clone()))),
        }
    }

    pub fn set_canvas(&mut self, canvas: Option<[u32; 2]>) -> CoreResult<()> {
        if let Some([c, r]) = canvas {
            if !(1..=20_000).contains(&c) || !(1..=20_000).contains(&r) {
                return Err(CoreError::Invalid(
                    "canvas must be 1..=20000 cells a side".into(),
                ));
            }
        }
        self.canvas = canvas;
        Ok(())
    }

    pub fn set_map_grid(&mut self, grid: Option<MapGrid>) -> CoreResult<()> {
        if let Some(g) = &grid {
            g.validate()?;
        }
        self.map_grid = grid;
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

    /// The `cols x rows` (cells, from the origin) of the exported map: the explicit
    /// `canvas` if one is set, otherwise the smallest size that holds
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
        if let Some(b) = self.terrain.as_ref().and_then(|t| t.bounds_cells()) {
            cols = cols.max(b[2]);
            rows = rows.max(b[3]);
        }
        for r in &self.rivers {
            for p in &r.points {
                cols = cols.max(p[0] + r.width);
                rows = rows.max(p[1] + r.width);
            }
        }
        // Overlay items and labels: markers (with room for their name), region and
        // route points, and text (a label's width is about half its size per character).
        for m in &self.markers {
            cols = cols.max(m.x + 1.0 + m.label.chars().count() as f32 * 0.3);
            rows = rows.max(m.y + 1.0);
        }
        for r in &self.regions {
            for p in &r.points {
                cols = cols.max(p[0]);
                rows = rows.max(p[1]);
            }
        }
        for r in &self.routes {
            for p in &r.points {
                cols = cols.max(p[0] + r.width);
                rows = rows.max(p[1] + r.width);
            }
        }
        for l in &self.labels {
            let half_w = (l.text.chars().count() as f64 * l.size * 0.3) as f32;
            if l.path.is_empty() {
                cols = cols.max(l.x as f32 + half_w);
                rows = rows.max(l.y as f32 + l.size as f32);
            }
            for p in &l.path {
                cols = cols.max(p[0] + l.size as f32);
                rows = rows.max(p[1] + l.size as f32);
            }
        }
        // An explicit map size (region maps) is authoritative: it IS the map, and
        // content past it (terrain chunks are 16 cells wide and overshoot) is outside.
        if let Some([c, r]) = self.canvas {
            return (c.clamp(1, 20_000), r.clamp(1, 20_000));
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
        if label.id.is_empty() || label.id.len() > 64 {
            return Err(CoreError::Invalid("label id must be 1-64 chars".into()));
        }
        finite_ok(label.x, "label x")?;
        finite_ok(label.y, "label y")?;
        finite_ok(label.size, "label size")?;
        let lim = f64::from(crate::error::MAX_CELL);
        if label.x.abs() > lim || label.y.abs() > lim {
            return Err(CoreError::OutOfBounds("label position".into()));
        }
        if !(0.1..=40.0).contains(&label.size) {
            return Err(CoreError::Invalid(
                "label size must be 0.1..=40 cells".into(),
            ));
        }
        if label.text.chars().count() > 200 {
            return Err(CoreError::Invalid(
                "label text is capped at 200 chars".into(),
            ));
        }
        let c = label.color.as_bytes();
        if !(c.len() == 7 && c[0] == b'#' && c[1..].iter().all(u8::is_ascii_hexdigit)) {
            return Err(CoreError::Invalid("label color must be #rrggbb".into()));
        }
        if !label.tracking.is_finite() || !(0.0..=1.0).contains(&label.tracking) {
            return Err(CoreError::Invalid("label tracking must be 0..=1".into()));
        }
        if !label.rotation.is_finite() || label.rotation.abs() > 1000.0 {
            return Err(CoreError::Invalid("label rotation must be finite".into()));
        }
        if !label.path.is_empty() {
            if label.path.len() < 2 || label.path.len() > 400 {
                return Err(CoreError::Invalid("a label path has 2..=400 points".into()));
            }
            for p in &label.path {
                if !(p[0].is_finite() && p[1].is_finite())
                    || f64::from(p[0]).abs() > lim
                    || f64::from(p[1]).abs() > lim
                {
                    return Err(CoreError::OutOfBounds("label path point".into()));
                }
            }
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

    pub fn upsert_zone(&mut self, zone: crate::zone::Zone) -> CoreResult<()> {
        zone.validate()?;
        if let Some(z) = self.zones.iter_mut().find(|z| z.id == zone.id) {
            *z = zone;
        } else {
            if self.zones.len() >= crate::zone::MAX_ZONES {
                return Err(CoreError::CapExceeded(format!(
                    "at most {} zones",
                    crate::zone::MAX_ZONES
                )));
            }
            self.zones.push(zone);
        }
        Ok(())
    }

    pub fn remove_zone(&mut self, id: &str) -> CoreResult<()> {
        let before = self.zones.len();
        self.zones.retain(|z| z.id != id);
        if self.zones.len() == before {
            return Err(CoreError::NotFound(format!("zone {id}")));
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

    #[test]
    fn an_explicit_canvas_is_the_map_size_whatever_overshoots() {
        use crate::terrain::{TerrainMode, TerrainStroke};
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.terrain_stroke(&TerrainStroke {
            mode: TerrainMode::Raise,
            points: vec![[30.0, 30.0]],
            radius: 5.0,
            strength: 1.0,
            seed: 0,
        })
        .unwrap();
        let grown = f.extent();
        assert!(
            grown.0 >= 32 && grown.1 >= 32,
            "without a canvas the terrain sets the size: {grown:?}"
        );
        f.set_canvas(Some([20, 14])).unwrap();
        assert_eq!(f.extent(), (20, 14), "with one, the canvas does");
        assert!(f.set_canvas(Some([0, 5])).is_err() && f.set_canvas(Some([5, 99_999])).is_err());
        f.set_canvas(None).unwrap();
        assert_eq!(f.extent(), grown);
    }

    #[test]
    fn labels_are_validated_in_cells_and_old_json_still_loads() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.upsert_label(Label::new("a", 3.0, 4.0, "Kingdom"))
            .unwrap();
        let mut along = Label::new("r", 0.0, 0.0, "Silver River");
        along.path = vec![[1.0, 1.0], [5.0, 2.0], [9.0, 2.5]];
        along.tracking = 0.2;
        f.upsert_label(along).unwrap();
        type Mutator = fn(&mut Label);
        let bad: Vec<(&str, Mutator)> = vec![
            ("size 0", |l| l.size = 0.0),
            ("size huge", |l| l.size = 400.0),
            ("nan x", |l| l.x = f64::NAN),
            ("far y", |l| l.y = 1e9),
            ("colour", |l| l.color = "red".into()),
            ("long text", |l| l.text = "x".repeat(201)),
            ("tracking", |l| l.tracking = 5.0),
            ("rotation", |l| l.rotation = f32::INFINITY),
            ("one-point path", |l| l.path = vec![[0.0, 0.0]]),
            ("nan in path", |l| {
                l.path = vec![[0.0, 0.0], [f32::NAN, 1.0]]
            }),
            ("empty id", |l| l.id = String::new()),
        ];
        for (name, m) in bad {
            let mut l = Label::new("z", 1.0, 1.0, "x");
            m(&mut l);
            assert!(f.upsert_label(l).is_err(), "{name}");
        }
        assert_eq!(f.labels.len(), 2, "refusals stored nothing");
        // A label saved before paths, halos and tracking existed still loads, haloed and straight.
        let old =
            r##"{"id":"o","x":1.0,"y":2.0,"text":"Old","size":2.0,"color":"#112233","bold":true}"##;
        let l: Label = serde_json::from_str(old).unwrap();
        assert!(l.halo && l.path.is_empty() && l.tracking == 0.0 && !l.gm_only);
        // Non-ASCII text counts characters, not bytes.
        f.upsert_label(Label::new("u", 1.0, 1.0, &"é".repeat(150)))
            .unwrap();
    }

    #[test]
    fn extent_holds_markers_regions_routes_and_labels_too() {
        use crate::overlay::{Marker, OverlayItem as I, Region, Route};
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.put_overlay(
            I::Marker(Marker {
                id: "m".into(),
                x: 10.0,
                y: 4.0,
                label: "Port".into(),
                note: String::new(),
                color: "#e8b030".into(),
                link_schematic: None,
                links_to: None,
            }),
            None,
        )
        .unwrap();
        assert!(
            f.extent().0 >= 12 && f.extent().1 >= 5,
            "a marker and its name: {:?}",
            f.extent()
        );
        f.put_overlay(
            I::Region(Region {
                id: "r".into(),
                name: "R".into(),
                points: vec![[0.0, 0.0], [30.0, 0.0], [30.0, 22.0]],
                color: "#c0504d".into(),
                opacity: 0.3,
                hatch: false,
            }),
            None,
        )
        .unwrap();
        assert_eq!(f.extent(), (30, 22));
        f.put_overlay(
            I::Route(Route {
                id: "t".into(),
                name: "T".into(),
                points: vec![[1.0, 1.0], [40.0, 31.0]],
                color: "#6b3f1d".into(),
                width: 0.2,
                dashed: false,
            }),
            None,
        )
        .unwrap();
        assert_eq!(f.extent(), (41, 32));
        let mut l = Label::new("l", 50.0, 3.0, "A long label");
        l.size = 2.0;
        f.upsert_label(l).unwrap();
        assert!(
            f.extent().0 >= 57,
            "text extends past its anchor: {:?}",
            f.extent()
        );
        let mut p = Label::new("p", 0.0, 0.0, "x");
        p.path = vec![[2.0, 2.0], [3.0, 60.0]];
        f.upsert_label(p).unwrap();
        assert!(f.extent().1 >= 60);
    }
}
