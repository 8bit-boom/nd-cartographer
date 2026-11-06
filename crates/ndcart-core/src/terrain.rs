//! Heightmap terrain for region maps (plan §5.3, §7.2).
//!
//! A sparse grid of 16-bit elevations in 64x64-sample chunks; 4 samples per
//! document cell, so a 256x256-cell map is 1024x1024 samples. Absent chunks are
//! the `fill` level (open ocean until painted). Every edit is deterministic: a
//! brush stroke is a list of points + radius + strength + seed, a river is a
//! polyline that carves a channel — applied here, and undone exactly by
//! snapshotting the chunks the edit can touch (see `journal.rs`).
//!
//! Only `+ - * /`, `floor`, `sqrt` and integer hashing are used, so a heightmap
//! is bit-identical across platforms (the same rule as `ndcart-paint`).

use crate::base64;
use crate::error::{CoreError, CoreResult, MAX_CELL};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Samples per document cell, per axis.
pub const RES: i32 = 4;
/// Samples per chunk edge.
pub const CHUNK: i32 = 64;
pub const MAX_CHUNKS: usize = 16_384;
pub const MAX_POINTS: usize = 4_000;
pub const MAX_RIVERS: usize = 2_000;
/// The default sea level: heights below it are water.
pub const DEFAULT_SEA: u16 = 20_000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TerrainMode {
    /// Push the land up.
    Raise,
    /// Push it down (below `sea_level` it becomes water).
    Lower,
    /// Blend toward the local average.
    Smooth,
    /// Level toward the height under the first point (plateaus, terraces).
    Flatten,
    /// Roughen: seeded noise, for hand-made ruggedness.
    Roughen,
}

/// Tonal grading applied to the finished map (Wonderdraft-style global tint).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct TerrainStyle {
    #[serde(default = "default_sea")]
    pub sea_level: u16,
    /// `#rrggbb` mixed over the whole map...
    #[serde(default = "no_tint")]
    pub tint: String,
    /// ...by this much (0..=1).
    #[serde(default)]
    pub tint_amount: f32,
    /// 0..=2, 1 = unchanged.
    #[serde(default = "one")]
    pub saturation: f32,
    #[serde(default = "one")]
    pub contrast: f32,
    #[serde(default = "one")]
    pub brightness: f32,
    /// 0..=2 — strength of the relief shading; 0 turns it off.
    #[serde(default = "one")]
    pub hillshade: f32,
    /// Contour line spacing in elevation units (0 = none).
    #[serde(default)]
    pub contour_interval: u16,
}

fn default_sea() -> u16 {
    DEFAULT_SEA
}
fn no_tint() -> String {
    "#ffffff".into()
}
fn one() -> f32 {
    1.0
}

impl Default for TerrainStyle {
    fn default() -> Self {
        TerrainStyle {
            sea_level: DEFAULT_SEA,
            tint: no_tint(),
            tint_amount: 0.0,
            saturation: 1.0,
            contrast: 1.0,
            brightness: 1.0,
            hillshade: 1.0,
            contour_interval: 0,
        }
    }
}

impl TerrainStyle {
    pub fn validate(&self) -> CoreResult<()> {
        let b = self.tint.as_bytes();
        if !(b.len() == 7 && b[0] == b'#' && b[1..].iter().all(u8::is_ascii_hexdigit)) {
            return Err(CoreError::Invalid("tint must be #rrggbb".into()));
        }
        let ok = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        if !(ok(self.tint_amount, 0.0, 1.0)
            && ok(self.saturation, 0.0, 2.0)
            && ok(self.contrast, 0.2, 2.0)
            && ok(self.brightness, 0.2, 2.0)
            && ok(self.hillshade, 0.0, 2.0))
        {
            return Err(CoreError::Invalid("a style value is out of range".into()));
        }
        Ok(())
    }
}

/// A river: drawn upstream -> downstream, it carves a channel and is drawn as
/// water on top. Coordinates are cells.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct River {
    pub id: String,
    pub name: String,
    pub points: Vec<[f32; 2]>,
    /// Width at the mouth, in cells (it tapers to a third of this at the source).
    pub width: f32,
}

impl River {
    pub fn validate(&self) -> CoreResult<()> {
        if self.id.is_empty() || self.id.len() > 64 {
            return Err(CoreError::Invalid("river id must be 1-64 chars".into()));
        }
        if self.name.len() > 80 {
            return Err(CoreError::Invalid(
                "river name is capped at 80 chars".into(),
            ));
        }
        if self.points.len() < 2 || self.points.len() > MAX_POINTS {
            return Err(CoreError::Invalid(format!(
                "a river has 2..={MAX_POINTS} points"
            )));
        }
        check_points(&self.points)?;
        if !self.width.is_finite() || !(0.05..=16.0).contains(&self.width) {
            return Err(CoreError::Invalid(
                "river width must be 0.05..=16 cells".into(),
            ));
        }
        Ok(())
    }
}

fn check_points(points: &[[f32; 2]]) -> CoreResult<()> {
    let lim = MAX_CELL as f32;
    for p in points {
        if !(p[0].is_finite() && p[1].is_finite()) || p[0].abs() > lim || p[1].abs() > lim {
            return Err(CoreError::OutOfBounds("terrain point".into()));
        }
    }
    Ok(())
}

/// One chunk on the wire: coordinates and 4096 little-endian u16 as base64.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ChunkData {
    pub cx: i32,
    pub cy: i32,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Terrain {
    /// Height of any sample with no chunk.
    pub fill: u16,
    pub style: TerrainStyle,
    chunks: BTreeMap<(i32, i32), Vec<u16>>,
}

impl Default for Terrain {
    fn default() -> Self {
        Terrain {
            fill: DEFAULT_SEA - 6_000,
            style: TerrainStyle::default(),
            chunks: BTreeMap::new(),
        }
    }
}

// ---- serde: chunks as {cx, cy, data(base64)} so JSON stays compact ----

#[derive(Serialize, Deserialize)]
struct TerrainSerde {
    fill: u16,
    #[serde(default)]
    style: TerrainStyle,
    #[serde(default)]
    chunks: Vec<ChunkData>,
}

fn encode_chunk(d: &[u16]) -> String {
    let mut bytes = Vec::with_capacity(d.len() * 2);
    for v in d {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    base64::encode(&bytes)
}

#[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
fn decode_chunk(s: &str) -> CoreResult<Vec<u16>> {
    let bytes =
        base64::decode(s).ok_or_else(|| CoreError::Invalid("chunk data is not base64".into()))?;
    if bytes.len() != (CHUNK * CHUNK) as usize * 2 {
        return Err(CoreError::Invalid("a chunk is 64x64 samples".into()));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect())
}

impl Serialize for Terrain {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        TerrainSerde {
            fill: self.fill,
            style: self.style.clone(),
            chunks: self
                .chunks
                .iter()
                .map(|(&(cx, cy), d)| ChunkData {
                    cx,
                    cy,
                    data: encode_chunk(d),
                })
                .collect(),
        }
        .serialize(ser)
    }
}

impl<'de> Deserialize<'de> for Terrain {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let t = TerrainSerde::deserialize(de)?;
        let mut terrain = Terrain {
            fill: t.fill,
            style: t.style,
            chunks: BTreeMap::new(),
        };
        terrain
            .set_chunks(&t.chunks)
            .map_err(serde::de::Error::custom)?;
        Ok(terrain)
    }
}

/// A rectangle of samples copied out for fast reading (the renderer works on
/// these instead of doing a chunk lookup per sample).
pub struct HeightGrid {
    pub x0: i32,
    pub y0: i32,
    pub w: i32,
    pub h: i32,
    data: Vec<u16>,
}

impl HeightGrid {
    /// Height at sample `(sx, sy)`, clamped to the rectangle's edge.
    pub fn at(&self, sx: i32, sy: i32) -> f32 {
        let x = (sx - self.x0).clamp(0, self.w - 1);
        let y = (sy - self.y0).clamp(0, self.h - 1);
        f32::from(self.data[(y * self.w + x) as usize])
    }

    /// Bilinear height at a position in samples.
    pub fn bilinear(&self, u: f32, v: f32) -> f32 {
        let (fx, fy) = (u.floor(), v.floor());
        let (ix, iy) = (fx as i32, fy as i32);
        let (tx, ty) = (u - fx, v - fy);
        let a = self.at(ix, iy);
        let b = self.at(ix + 1, iy);
        let c = self.at(ix, iy + 1);
        let d = self.at(ix + 1, iy + 1);
        let top = a + (b - a) * tx;
        let bot = c + (d - c) * tx;
        top + (bot - top) * ty
    }
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn hash(ix: i32, iy: i32, seed: u32) -> f32 {
    let mut h = (ix as u32).wrapping_mul(0x85eb_ca6b)
        ^ (iy as u32).wrapping_mul(0xc2b2_ae35)
        ^ seed.wrapping_mul(0x27d4_eb2f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    (h >> 8) as f32 / 16_777_216.0
}

impl Terrain {
    pub fn new(sea_level: u16) -> Terrain {
        Terrain {
            fill: sea_level.saturating_sub(6_000),
            style: TerrainStyle {
                sea_level,
                ..TerrainStyle::default()
            },
            chunks: BTreeMap::new(),
        }
    }

    /// The coordinates of every chunk that exists, in order.
    pub fn chunk_keys(&self) -> Vec<(i32, i32)> {
        self.chunks.keys().copied().collect()
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn sea_level(&self) -> u16 {
        self.style.sea_level
    }

    /// The height at an integer sample.
    pub fn get(&self, sx: i32, sy: i32) -> u16 {
        let (cx, cy) = (sx.div_euclid(CHUNK), sy.div_euclid(CHUNK));
        match self.chunks.get(&(cx, cy)) {
            Some(d) => d[((sy.rem_euclid(CHUNK)) * CHUNK + sx.rem_euclid(CHUNK)) as usize],
            None => self.fill,
        }
    }

    fn chunk_mut(&mut self, cx: i32, cy: i32) -> &mut Vec<u16> {
        let fill = self.fill;
        self.chunks
            .entry((cx, cy))
            .or_insert_with(|| vec![fill; (CHUNK * CHUNK) as usize])
    }

    pub fn set(&mut self, sx: i32, sy: i32, v: u16) {
        let (cx, cy) = (sx.div_euclid(CHUNK), sy.div_euclid(CHUNK));
        if v == self.fill && !self.chunks.contains_key(&(cx, cy)) {
            return; // writing the fill into nothing changes nothing
        }
        let c = self.chunk_mut(cx, cy);
        c[(sy.rem_euclid(CHUNK) * CHUNK + sx.rem_euclid(CHUNK)) as usize] = v;
    }

    /// Drop chunks that hold nothing but the fill (keeps equality and storage tidy).
    pub fn prune(&mut self) {
        let fill = self.fill;
        self.chunks.retain(|_, d| d.iter().any(|v| *v != fill));
    }

    /// A copy of the chunk `(cx, cy)` as it is now (the fill if absent).
    pub fn snapshot_chunk(&self, cx: i32, cy: i32) -> ChunkData {
        let data = match self.chunks.get(&(cx, cy)) {
            Some(d) => encode_chunk(d),
            None => encode_chunk(&vec![self.fill; (CHUNK * CHUNK) as usize]),
        };
        ChunkData { cx, cy, data }
    }

    /// Replace chunks with raw data (undo, generators, import). Absent-equivalent
    /// chunks are pruned so an undone edit restores the exact previous state.
    pub fn set_chunks(&mut self, chunks: &[ChunkData]) -> CoreResult<()> {
        let lim = (MAX_CELL * RES / CHUNK) + 1;
        let mut staged = Vec::with_capacity(chunks.len());
        for c in chunks {
            if c.cx.abs() > lim || c.cy.abs() > lim {
                return Err(CoreError::OutOfBounds(format!(
                    "chunk ({}, {})",
                    c.cx, c.cy
                )));
            }
            staged.push(((c.cx, c.cy), decode_chunk(&c.data)?));
        }
        let new_keys = staged
            .iter()
            .filter(|(k, _)| !self.chunks.contains_key(k))
            .count();
        if self.chunks.len() + new_keys > MAX_CHUNKS {
            return Err(CoreError::CapExceeded(format!(
                "at most {MAX_CHUNKS} terrain chunks"
            )));
        }
        for (k, d) in staged {
            self.chunks.insert(k, d);
        }
        self.prune();
        Ok(())
    }

    /// The chunk coordinates a rectangle of cells (padded by `pad` cells) can touch.
    pub fn chunks_for_cells(min: [f32; 2], max: [f32; 2], pad: f32) -> Vec<(i32, i32)> {
        let f = |v: f32| (v * RES as f32).floor() as i32;
        let (x0, y0) = (
            f(min[0] - pad).div_euclid(CHUNK),
            f(min[1] - pad).div_euclid(CHUNK),
        );
        let (x1, y1) = (
            f(max[0] + pad).div_euclid(CHUNK),
            f(max[1] + pad).div_euclid(CHUNK),
        );
        let mut out = Vec::new();
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                out.push((cx, cy));
            }
        }
        out
    }

    /// Copy out the samples `[x0, x0+w) x [y0, y0+h)`.
    pub fn extract(&self, x0: i32, y0: i32, w: i32, h: i32) -> HeightGrid {
        let (w, h) = (w.max(1), h.max(1));
        let mut data = Vec::with_capacity((w * h) as usize);
        type Cached<'a> = Option<((i32, i32), Option<&'a Vec<u16>>)>;
        let mut cache: Cached = None;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let key = (x.div_euclid(CHUNK), y.div_euclid(CHUNK));
                let chunk = match cache {
                    Some((k, c)) if k == key => c,
                    _ => {
                        let c = self.chunks.get(&key);
                        cache = Some((key, c));
                        c
                    }
                };
                data.push(match chunk {
                    Some(d) => d[(y.rem_euclid(CHUNK) * CHUNK + x.rem_euclid(CHUNK)) as usize],
                    None => self.fill,
                });
            }
        }
        HeightGrid { x0, y0, w, h, data }
    }

    /// Apply a brush stroke. `points` are cell coordinates; `radius` is in cells;
    /// `strength` is 0..=1 per point. Deterministic from `seed`.
    pub fn stroke(
        &mut self,
        mode: TerrainMode,
        points: &[[f32; 2]],
        radius: f32,
        strength: f32,
        seed: u32,
    ) {
        let rs = radius * RES as f32; // radius in samples
        let target = points.first().map(|p| {
            f32::from(self.get(
                (p[0] * RES as f32).floor() as i32,
                (p[1] * RES as f32).floor() as i32,
            ))
        });
        for p in points {
            let (px, py) = (p[0] * RES as f32, p[1] * RES as f32);
            let (x0, x1) = ((px - rs).floor() as i32, (px + rs).ceil() as i32);
            let (y0, y1) = ((py - rs).floor() as i32, (py + rs).ceil() as i32);
            for sy in y0..=y1 {
                for sx in x0..=x1 {
                    let d =
                        ((sx as f32 + 0.5 - px).powi(2) + (sy as f32 + 0.5 - py).powi(2)).sqrt();
                    if d >= rs {
                        continue;
                    }
                    let w = smooth(1.0 - d / rs) * strength;
                    let cur = f32::from(self.get(sx, sy));
                    let new = match mode {
                        TerrainMode::Raise => cur + w * 900.0,
                        TerrainMode::Lower => cur - w * 900.0,
                        TerrainMode::Smooth => {
                            let mut sum = 0.0;
                            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (0, 0)] {
                                sum += f32::from(self.get(sx + dx, sy + dy));
                            }
                            cur + (sum / 5.0 - cur) * w.min(1.0)
                        }
                        TerrainMode::Flatten => cur + (target.unwrap_or(cur) - cur) * w.min(1.0),
                        TerrainMode::Roughen => cur + (hash(sx, sy, seed) - 0.5) * w * 1400.0,
                    };
                    self.set(sx, sy, new.clamp(0.0, 65_535.0) as u16);
                }
            }
        }
        self.prune();
    }

    /// Carve a river channel. The bed only ever descends toward the mouth (it
    /// follows the lowest ground seen so far, minus a little), so a river stays
    /// in its valley and never runs uphill; banks blend smoothly into the land.
    pub fn carve_river(&mut self, river: &River) {
        let n = river.points.len();
        let mut bed = f32::MAX;
        for i in 0..n - 1 {
            let (a, b) = (river.points[i], river.points[i + 1]);
            let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            let steps = ((len * RES as f32).ceil() as i32).max(1);
            for s in 0..steps {
                let t = s as f32 / steps as f32;
                let (cx, cy) = (a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
                // Width grows toward the mouth: a third at the source, full at the end.
                let along = (i as f32 + t) / (n - 1) as f32;
                let half = river.width * (0.33 + 0.67 * along) * 0.5;
                let (sx, sy) = (
                    (cx * RES as f32).floor() as i32,
                    (cy * RES as f32).floor() as i32,
                );
                let here = f32::from(self.get(sx, sy));
                bed = bed.min(here - 60.0);
                self.carve_disc(cx, cy, half, bed);
            }
        }
        // The mouth: carve the last point too.
        let last = river.points[n - 1];
        self.carve_disc(last[0], last[1], river.width * 0.5, bed);
        self.prune();
    }

    fn carve_disc(&mut self, cx: f32, cy: f32, half_cells: f32, bed: f32) {
        let r = half_cells * RES as f32;
        let bank = r * 2.2 + 1.0; // banks blend out to this radius
        let (px, py) = (cx * RES as f32, cy * RES as f32);
        for sy in (py - bank).floor() as i32..=(py + bank).ceil() as i32 {
            for sx in (px - bank).floor() as i32..=(px + bank).ceil() as i32 {
                let d = ((sx as f32 + 0.5 - px).powi(2) + (sy as f32 + 0.5 - py).powi(2)).sqrt();
                if d >= bank {
                    continue;
                }
                let cur = f32::from(self.get(sx, sy));
                // Full depth in the channel, easing back to the original on the banks.
                let k = if d <= r {
                    1.0
                } else {
                    1.0 - smooth((d - r) / (bank - r))
                };
                let target = cur + (bed.min(cur) - cur) * k;
                if target < cur {
                    self.set(sx, sy, target.max(0.0) as u16);
                }
            }
        }
    }

    /// Bounds of everything painted, in cells (`None` if empty).
    pub fn bounds_cells(&self) -> Option<[f32; 4]> {
        let mut it = self.chunks.keys();
        let first = it.next()?;
        let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
        for &(cx, cy) in it {
            x0 = x0.min(cx);
            y0 = y0.min(cy);
            x1 = x1.max(cx);
            y1 = y1.max(cy);
        }
        let k = (CHUNK / RES) as f32;
        Some([
            x0 as f32 * k,
            y0 as f32 * k,
            (x1 + 1) as f32 * k,
            (y1 + 1) as f32 * k,
        ])
    }
}

impl TerrainStroke {
    pub fn validate(&self) -> CoreResult<()> {
        if self.points.is_empty() || self.points.len() > MAX_POINTS {
            return Err(CoreError::Invalid(format!(
                "a terrain stroke has 1..={MAX_POINTS} points"
            )));
        }
        check_points(&self.points)?;
        if !self.radius.is_finite() || !(0.2..=64.0).contains(&self.radius) {
            return Err(CoreError::Invalid(
                "terrain brush radius must be 0.2..=64 cells".into(),
            ));
        }
        if !self.strength.is_finite() || !(0.0..=1.0).contains(&self.strength) {
            return Err(CoreError::Invalid(
                "terrain brush strength must be 0..=1".into(),
            ));
        }
        Ok(())
    }

    /// The chunks this stroke can touch.
    pub fn touched(&self) -> Vec<(i32, i32)> {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in &self.points {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        // +1 sample of slack for the Smooth neighbourhood.
        Terrain::chunks_for_cells(lo, hi, self.radius + 0.5)
    }
}

/// A terrain brush stroke as a journal payload.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct TerrainStroke {
    pub mode: TerrainMode,
    pub points: Vec<[f32; 2]>,
    pub radius: f32,
    pub strength: f32,
    #[serde(default)]
    pub seed: u32,
}

impl River {
    /// The chunks carving this river can touch.
    pub fn touched(&self) -> Vec<(i32, i32)> {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in &self.points {
            lo = [lo[0].min(p[0]), lo[1].min(p[1])];
            hi = [hi[0].max(p[0]), hi[1].max(p[1])];
        }
        Terrain::chunks_for_cells(lo, hi, self.width * 1.1 + 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn land() -> Terrain {
        // A flat plateau well above sea over 20x20 cells.
        let mut t = Terrain::new(DEFAULT_SEA);
        for sy in 0..80 {
            for sx in 0..80 {
                t.set(sx, sy, 30_000);
            }
        }
        t
    }

    #[test]
    fn absent_chunks_read_as_the_fill_and_writes_allocate_lazily() {
        let mut t = Terrain::new(DEFAULT_SEA);
        assert_eq!(t.get(123, -456), t.fill);
        assert_eq!(t.chunk_count(), 0);
        t.set(5, 5, t.fill); // no-op: nothing to allocate
        assert_eq!(t.chunk_count(), 0);
        t.set(5, 5, 40_000);
        t.set(-1, -1, 41_000); // negative coordinates live in chunk (-1,-1)
        assert_eq!(
            (t.get(5, 5), t.get(-1, -1), t.chunk_count()),
            (40_000, 41_000, 2)
        );
        t.set(5, 5, t.fill);
        t.prune();
        assert_eq!(t.chunk_count(), 1, "an all-fill chunk is dropped");
    }

    #[test]
    fn serde_round_trips_exactly_and_compactly() {
        let mut t = land();
        t.style.tint = "#aa8844".into();
        t.style.tint_amount = 0.25;
        let j = serde_json::to_string(&t).unwrap();
        let back: Terrain = serde_json::from_str(&j).unwrap();
        assert_eq!(back, t);
        // 4 chunks of 8 KiB of u16 is ~11 KiB each as base64, not 4096 numbers each.
        assert!(j.len() < 60_000, "{} bytes", j.len());
        // Bad chunk data is refused, not trusted.
        let bad = r#"{"fill":1,"chunks":[{"cx":0,"cy":0,"data":"AAAA"}]}"#;
        assert!(serde_json::from_str::<Terrain>(bad).is_err());
        let wild = format!(
            r#"{{"fill":1,"chunks":[{{"cx":99999999,"cy":0,"data":"{}"}}]}}"#,
            encode_chunk(&vec![0; 4096])
        );
        assert!(serde_json::from_str::<Terrain>(&wild).is_err());
    }

    #[test]
    fn brushes_raise_lower_smooth_flatten_and_roughen_deterministically() {
        let mut t = land();
        let centre = [10.0, 10.0];
        let h = |t: &Terrain| t.get(40, 40);
        let before = h(&t);
        t.stroke(TerrainMode::Raise, &[centre], 3.0, 1.0, 1);
        assert!(h(&t) > before + 500, "raise: {} -> {}", before, h(&t));
        let raised = h(&t);
        t.stroke(TerrainMode::Lower, &[centre], 3.0, 1.0, 1);
        assert!(h(&t) < raised - 500, "lower");
        // Outside the radius nothing moves.
        assert_eq!(t.get(40 + 3 * RES + 4, 40), 30_000);
        // Flatten pulls a bump back toward the height under the first point.
        let mut f = land();
        f.set(40, 40, 36_000);
        // The first point sets the target (level ground at 30,000); the bump is then
        // inside the second point's disc and is pulled down to it.
        f.stroke(
            TerrainMode::Flatten,
            &[[12.5, 10.0], [10.0, 10.0]],
            2.5,
            1.0,
            0,
        );
        assert!(f.get(40, 40) < 36_000 - 3_000, "{}", f.get(40, 40));
        assert!(f.get(40, 40) >= 30_000);
        // Smooth reduces a spike.
        let mut s = land();
        s.set(40, 40, 50_000);
        s.stroke(TerrainMode::Smooth, &[[10.0, 10.0]], 1.0, 1.0, 0);
        assert!(s.get(40, 40) < 50_000);
        // Roughen is seeded: same seed same ground, different seed different ground.
        let (mut a, mut b, mut c) = (land(), land(), land());
        a.stroke(TerrainMode::Roughen, &[centre], 3.0, 1.0, 7);
        b.stroke(TerrainMode::Roughen, &[centre], 3.0, 1.0, 7);
        c.stroke(TerrainMode::Roughen, &[centre], 3.0, 1.0, 8);
        assert_eq!(a, b);
        assert_ne!(a, c);
        // Heights clamp instead of wrapping.
        let mut m = land();
        for _ in 0..200 {
            m.stroke(TerrainMode::Raise, &[centre], 2.0, 1.0, 0);
        }
        assert_eq!(m.get(40, 40), 65_535);
        let mut z = land();
        for _ in 0..200 {
            z.stroke(TerrainMode::Lower, &[centre], 2.0, 1.0, 0);
        }
        assert_eq!(z.get(40, 40), 0);
    }

    #[test]
    fn a_river_carves_a_channel_that_never_runs_uphill_and_leaves_far_land_alone() {
        let mut t = land();
        // Make the ground slope down to the east so "downstream" is meaningful.
        for sy in 0..80 {
            for sx in 0..80 {
                t.set(sx, sy, 34_000 - (sx as u16) * 60);
            }
        }
        let untouched = t.get(10, 70);
        let r = River {
            id: "r".into(),
            name: "Silver".into(),
            points: vec![[2.0, 8.0], [8.0, 9.0], [14.0, 8.5], [18.0, 10.0]],
            width: 1.5,
        };
        let before_line: Vec<u16> = (0..18).map(|c| t.get(c * RES, 34)).collect();
        t.carve_river(&r);
        // The channel centre is lower than the land beside it.
        let (mid_x, mid_y) = (8 * RES, 9 * RES);
        assert!(
            t.get(mid_x, mid_y) + 100 < t.get(mid_x, mid_y + 4 * RES),
            "channel below the banks"
        );
        // The bed never rises going downstream along the path.
        let mut last = u16::MAX;
        for i in 0..r.points.len() - 1 {
            let (a, b) = (r.points[i], r.points[i + 1]);
            for s in 0..20 {
                let f = s as f32 / 20.0;
                let (x, y) = (a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f);
                let h = t.get((x * RES as f32) as i32, (y * RES as f32) as i32);
                assert!(
                    h <= last.saturating_add(70),
                    "uphill at ({x},{y}): {h} after {last}"
                );
                last = last.min(h);
            }
        }
        assert_eq!(t.get(10, 70), untouched, "far ground is untouched");
        let after_line: Vec<u16> = (0..18).map(|c| t.get(c * RES, 34)).collect();
        assert!(
            after_line.iter().zip(&before_line).all(|(a, b)| a <= b),
            "carving only ever lowers"
        );
        // Deterministic.
        let mut u = land();
        for sy in 0..80 {
            for sx in 0..80 {
                u.set(sx, sy, 34_000 - (sx as u16) * 60);
            }
        }
        u.carve_river(&r);
        assert_eq!(u, t);
    }

    #[test]
    fn snapshots_restore_exactly_even_for_chunks_that_did_not_exist() {
        let mut t = land();
        let stroke = TerrainStroke {
            mode: TerrainMode::Raise,
            points: vec![[30.0, 30.0], [31.0, 30.5]],
            radius: 4.0,
            strength: 1.0,
            seed: 0,
        };
        let before = t.clone();
        let snaps: Vec<ChunkData> = stroke
            .touched()
            .into_iter()
            .map(|(cx, cy)| t.snapshot_chunk(cx, cy))
            .collect();
        t.stroke(
            stroke.mode,
            &stroke.points,
            stroke.radius,
            stroke.strength,
            stroke.seed,
        );
        assert_ne!(t, before);
        t.set_chunks(&snaps).unwrap();
        assert_eq!(
            t, before,
            "undo restores the exact terrain, including which chunks exist"
        );
    }

    #[test]
    fn extract_matches_get_and_bilinear_is_continuous() {
        let mut t = land();
        t.stroke(TerrainMode::Raise, &[[5.0, 5.0]], 4.0, 1.0, 0);
        let g = t.extract(-3, -2, 90, 70);
        for (sx, sy) in [(-3, -2), (0, 0), (30, 20), (86, 67), (40, 40)] {
            assert_eq!(g.at(sx, sy), f32::from(t.get(sx, sy)));
        }
        // Between samples the value is between its neighbours; at a sample it is exact.
        assert_eq!(g.bilinear(30.0, 20.0), g.at(30, 20));
        let (a, b) = (g.at(30, 20), g.at(31, 20));
        let mid = g.bilinear(30.5, 20.0);
        assert!(mid >= a.min(b) - 0.01 && mid <= a.max(b) + 0.01);
        // Outside the rectangle clamps to the edge rather than panicking.
        let _ = g.at(10_000, -10_000);
    }

    #[test]
    fn validation_refuses_hostile_input() {
        let ok = TerrainStroke {
            mode: TerrainMode::Raise,
            points: vec![[1.0, 1.0]],
            radius: 2.0,
            strength: 0.5,
            seed: 0,
        };
        ok.validate().unwrap();
        for bad in [
            TerrainStroke {
                points: vec![],
                ..ok.clone()
            },
            TerrainStroke {
                points: vec![[f32::NAN, 0.0]],
                ..ok.clone()
            },
            TerrainStroke {
                points: vec![[1e9, 0.0]],
                ..ok.clone()
            },
            TerrainStroke {
                radius: 0.0,
                ..ok.clone()
            },
            TerrainStroke {
                radius: 1e6,
                ..ok.clone()
            },
            TerrainStroke {
                strength: 2.0,
                ..ok.clone()
            },
            TerrainStroke {
                points: vec![[0.0, 0.0]; MAX_POINTS + 1],
                ..ok.clone()
            },
        ] {
            assert!(bad.validate().is_err());
        }
        let r = River {
            id: "r".into(),
            name: "n".into(),
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            width: 1.0,
        };
        r.validate().unwrap();
        for bad in [
            River {
                points: vec![[0.0, 0.0]],
                ..r.clone()
            },
            River {
                width: 0.0,
                ..r.clone()
            },
            River {
                width: f32::NAN,
                ..r.clone()
            },
            River {
                id: String::new(),
                ..r.clone()
            },
            River {
                points: vec![[0.0, 0.0], [f32::INFINITY, 1.0]],
                ..r.clone()
            },
        ] {
            assert!(bad.validate().is_err());
        }
        assert!(TerrainStyle {
            saturation: 9.0,
            ..TerrainStyle::default()
        }
        .validate()
        .is_err());
        assert!(TerrainStyle {
            tint: "red".into(),
            ..TerrainStyle::default()
        }
        .validate()
        .is_err());
        TerrainStyle::default().validate().unwrap();
    }
}
