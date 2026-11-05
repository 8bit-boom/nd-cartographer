//! Mass scatter: fill an area with props (trees, rocks, bushes) at an even,
//! natural-looking density. A jittered grid (one prop per `spacing` cell,
//! nudged within it) gives blue-noise-like placement that never clumps or
//! leaves holes, and it is a pure function of the seed. The result is one
//! batch of `AddStamp` ops, so a whole forest is a single undo step.

use crate::error::{CoreError, CoreResult};
use crate::journal::Op;
use crate::stamp::{Stamp, MAX_STAMPS};

/// Most props one scatter may place.
pub const MAX_SCATTER: usize = 3_000;

#[derive(Clone, Debug)]
pub struct ScatterParams {
    /// The area, in cells (3 or more points).
    pub polygon: Vec<[f32; 2]>,
    /// Stamp asset ids to pick from, evenly.
    pub assets: Vec<String>,
    /// Typical distance between props, in cells (0.4..=20).
    pub spacing: f32,
    pub seed: u64,
    /// Scale of each prop is drawn from `scale_min..=scale_max`.
    pub scale_min: f32,
    pub scale_max: f32,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `0.0..1.0` (24 random bits, so exact in f32).
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / 16_777_216.0
    }
}

fn inside(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    let mut hit = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            hit = !hit;
        }
        j = i;
    }
    hit
}

/// The scatter as ops. `taken` is how many stamps the floor already has (the
/// per-floor cap counts them).
pub fn scatter(p: &ScatterParams, taken: usize) -> CoreResult<Vec<Op>> {
    if p.polygon.len() < 3 || p.polygon.len() > 500 {
        return Err(CoreError::Invalid("an area needs 3 to 500 points".into()));
    }
    if p.polygon
        .iter()
        .any(|q| !q[0].is_finite() || !q[1].is_finite())
    {
        return Err(CoreError::Invalid("area points must be numbers".into()));
    }
    if p.assets.is_empty() || p.assets.len() > 12 {
        return Err(CoreError::Invalid("pick 1 to 12 props".into()));
    }
    if !p.spacing.is_finite() || !(0.4..=20.0).contains(&p.spacing) {
        return Err(CoreError::Invalid("spacing must be 0.4-20 cells".into()));
    }
    if !(p.scale_min.is_finite() && p.scale_max.is_finite())
        || p.scale_min < 0.2
        || p.scale_max > 5.0
        || p.scale_min > p.scale_max
    {
        return Err(CoreError::Invalid(
            "scale must be within 0.2-5, min <= max".into(),
        ));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in &p.polygon {
        x0 = x0.min(q[0]);
        y0 = y0.min(q[1]);
        x1 = x1.max(q[0]);
        y1 = y1.max(q[1]);
    }
    let (cols, rows) = (
        ((x1 - x0) / p.spacing).ceil().max(1.0),
        ((y1 - y0) / p.spacing).ceil().max(1.0),
    );
    if cols * rows > 400_000.0 {
        return Err(CoreError::CapExceeded(
            "that area is too large for this spacing; use a bigger spacing".into(),
        ));
    }
    let room = MAX_STAMPS.saturating_sub(taken).min(MAX_SCATTER);
    let mut rng = Rng(p.seed ^ 0x51_7C_C1_B7_27_22_0A_95);
    let stem = format!("sc{:x}", p.seed & 0xFFFF_FFFF);
    let mut ops = Vec::new();
    for j in 0..rows as i32 {
        for i in 0..cols as i32 {
            // One prop per cell, anywhere within the middle 80% of it.
            let x = x0 + (i as f32 + 0.1 + 0.8 * rng.unit()) * p.spacing;
            let y = y0 + (j as f32 + 0.1 + 0.8 * rng.unit()) * p.spacing;
            let pick = (rng.next() % p.assets.len() as u64) as usize;
            let scale = p.scale_min + (p.scale_max - p.scale_min) * rng.unit();
            let rotation = rng.unit() * 6.283_185;
            let flip = rng.next() & 1 == 1;
            if !inside(&p.polygon, x, y) {
                continue;
            }
            if ops.len() >= room {
                return Err(CoreError::CapExceeded(format!(
                    "that would place more than {room} props; use a bigger spacing or a smaller area"
                )));
            }
            ops.push(Op::AddStamp {
                stamp: Stamp {
                    id: format!("{stem}_{}", ops.len()),
                    asset: p.assets[pick].clone(),
                    x,
                    y,
                    scale,
                    rotation,
                    flip,
                    tint: None,
                    shadow: true,
                },
                index: None,
            });
        }
    }
    if ops.is_empty() {
        return Err(CoreError::Invalid("nothing fitted in that area".into()));
    }
    Ok(vec![Op::Batch { ops }])
}

/// A named look: which props, how dense, and a faint ground tint.
pub struct Biome {
    pub name: &'static str,
    pub label: &'static str,
    pub assets: &'static [&'static str],
    pub spacing: f32,
    pub scale: (f32, f32),
    /// `#rrggbb` and opacity of the region laid under the props.
    pub tint: (&'static str, f32),
}

pub const BIOMES: [Biome; 5] = [
    Biome {
        name: "forest",
        label: "Forest",
        assets: &["tree", "tree", "bush"],
        spacing: 1.3,
        scale: (0.9, 1.4),
        tint: ("#2f5d34", 0.25),
    },
    Biome {
        name: "meadow",
        label: "Meadow",
        assets: &["bush"],
        spacing: 2.8,
        scale: (0.7, 1.1),
        tint: ("#7aa34b", 0.22),
    },
    Biome {
        name: "rocky",
        label: "Rocky ground",
        assets: &["rock", "rock", "bush"],
        spacing: 2.2,
        scale: (0.7, 1.5),
        tint: ("#8a7a63", 0.22),
    },
    Biome {
        name: "swamp",
        label: "Swamp",
        assets: &["bush", "tree"],
        spacing: 1.8,
        scale: (0.8, 1.2),
        tint: ("#3d5c55", 0.3),
    },
    Biome {
        name: "desert",
        label: "Desert",
        assets: &["rock"],
        spacing: 4.5,
        scale: (0.6, 1.2),
        tint: ("#d8bf7a", 0.25),
    },
];

pub fn biome(name: &str) -> Option<&'static Biome> {
    BIOMES.iter().find(|b| b.name == name)
}

/// A biome over an area as one batch: the tinted region, then the props.
/// `density` scales the spacing (1.0 = the biome's own; 2.0 = half as dense).
pub fn scatter_biome(
    polygon: &[[f32; 2]],
    b: &Biome,
    density: f32,
    seed: u64,
    taken: usize,
) -> CoreResult<Vec<Op>> {
    if !density.is_finite() || !(0.5..=4.0).contains(&density) {
        return Err(CoreError::Invalid("density must be 0.5-4".into()));
    }
    let mut ops = scatter(
        &ScatterParams {
            polygon: polygon.to_vec(),
            assets: b.assets.iter().map(|a| a.to_string()).collect(),
            spacing: b.spacing * density,
            seed,
            scale_min: b.scale.0,
            scale_max: b.scale.1,
        },
        taken,
    )?;
    let Some(Op::Batch { ops: inner }) = ops.pop() else {
        return Err(CoreError::Invalid("nothing fitted in that area".into()));
    };
    let region = crate::overlay::Region {
        id: format!("bm{:x}", seed & 0xFFFF_FFFF),
        name: b.label.to_string(),
        points: polygon.to_vec(),
        color: b.tint.0.to_string(),
        opacity: b.tint.1,
        hatch: false,
    };
    let mut all = vec![Op::PutOverlay {
        item: crate::overlay::OverlayItem::Region(region),
        index: None,
    }];
    all.extend(inner);
    Ok(vec![Op::Batch { ops: all }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::FloorDoc;
    use crate::journal::Journal;

    fn params(seed: u64) -> ScatterParams {
        ScatterParams {
            polygon: vec![[2.0, 2.0], [20.0, 3.0], [18.0, 16.0], [3.0, 14.0]],
            assets: vec!["tree".into(), "bush".into()],
            spacing: 1.5,
            seed,
            scale_min: 0.8,
            scale_max: 1.3,
        }
    }

    fn run(p: &ScatterParams) -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        for op in scatter(p, 0).unwrap() {
            j.push(&mut f, &op).unwrap();
        }
        f
    }

    #[test]
    fn props_fall_inside_the_area_evenly_and_the_same_every_time() {
        let a = run(&params(7));
        assert_eq!(a, run(&params(7)));
        assert_ne!(a, run(&params(8)));
        // About area / spacing^2 props, none outside the polygon, both kinds used.
        assert!(
            a.stamps.len() > 60 && a.stamps.len() < 200,
            "{}",
            a.stamps.len()
        );
        assert!(a
            .stamps
            .iter()
            .all(|s| inside(&params(7).polygon, s.x, s.y)));
        assert!(
            a.stamps.iter().any(|s| s.asset == "tree")
                && a.stamps.iter().any(|s| s.asset == "bush")
        );
        // No two props closer than the grid guarantees (>= 0.2 * spacing).
        for (i, s) in a.stamps.iter().enumerate() {
            for t in &a.stamps[i + 1..] {
                let d = ((s.x - t.x).powi(2) + (s.y - t.y).powi(2)).sqrt();
                assert!(d > 0.15 * 1.5, "{d}");
            }
        }
    }

    #[test]
    fn a_biome_lays_a_tinted_region_and_its_props_as_one_step() {
        let poly = params(1).polygon;
        let b = biome("forest").unwrap();
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        for op in scatter_biome(&poly, b, 1.0, 5, 0).unwrap() {
            j.push(&mut f, &op).unwrap();
        }
        assert_eq!(f.regions.len(), 1);
        assert_eq!(f.regions[0].name, "Forest");
        assert!(f.stamps.len() > 50);
        assert!(f
            .stamps
            .iter()
            .all(|s| s.asset == "tree" || s.asset == "bush"));
        j.undo_last(&mut f).unwrap();
        assert!(f.regions.is_empty() && f.stamps.is_empty());
        assert!(biome("lava").is_none());
        assert!(scatter_biome(&poly, b, 0.1, 5, 0).is_err());
        // Sparser means fewer props.
        let dense = scatter_biome(&poly, b, 1.0, 5, 0).unwrap();
        let sparse = scatter_biome(&poly, b, 2.0, 5, 0).unwrap();
        let n = |o: &Vec<Op>| match &o[0] {
            Op::Batch { ops } => ops.len(),
            _ => 0,
        };
        assert!(n(&sparse) * 2 < n(&dense) + 10);
    }

    #[test]
    fn one_undo_removes_the_whole_forest_and_bad_input_is_refused() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        for op in scatter(&params(1), 0).unwrap() {
            j.push(&mut f, &op).unwrap();
        }
        assert!(!f.stamps.is_empty());
        j.undo_last(&mut f).unwrap();
        assert!(f.stamps.is_empty());
        let mut bad = params(1);
        bad.spacing = 0.01;
        assert!(scatter(&bad, 0).is_err());
        let mut bad = params(1);
        bad.polygon.truncate(2);
        assert!(scatter(&bad, 0).is_err());
        let mut bad = params(1);
        bad.assets.clear();
        assert!(scatter(&bad, 0).is_err());
        let mut bad = params(1);
        bad.polygon = vec![[0.0, 0.0], [9000.0, 0.0], [9000.0, 9000.0]];
        bad.spacing = 0.5;
        assert!(scatter(&bad, 0).is_err());
        assert!(scatter(&params(1), MAX_STAMPS).is_err());
    }
}
