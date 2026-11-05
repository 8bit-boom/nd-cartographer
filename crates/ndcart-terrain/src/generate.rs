//! A seeded world: domain-warped fractal continents, ridged mountain chains,
//! a sea level chosen so a requested fraction of the map is land, and rivers
//! traced downhill from the highest ground to the sea. Same seed, same world,
//! on every platform.

use ndcart_core::terrain::{River, Terrain, DEFAULT_SEA, RES};
use ndcart_paint::{fbm, value_noise};

#[derive(Clone, Debug)]
pub struct GenParams {
    pub seed: u32,
    /// Map size in cells.
    pub cols: u32,
    pub rows: u32,
    /// Fraction of the map that is land (0.05..=0.95).
    pub land: f32,
    /// 0 scatters landmasses over the map, 1 gathers one continent in the middle.
    pub continent: f32,
    /// Strength of mountain chains, 0..=1.
    pub ridges: f32,
    /// How many rivers to try for (0 = none).
    pub rivers: u32,
}

impl Default for GenParams {
    fn default() -> Self {
        GenParams {
            seed: 1,
            cols: 128,
            rows: 96,
            land: 0.42,
            continent: 0.6,
            ridges: 0.6,
            rivers: 6,
        }
    }
}

pub struct Generated {
    /// The finished terrain (rivers already carved into it).
    pub terrain: Terrain,
    pub rivers: Vec<River>,
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn fbm5(x: f32, y: f32, seed: u32) -> f32 {
    let (mut sum, mut amp, mut f, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..5u32 {
        sum += value_noise(
            x * f + o as f32 * 17.0,
            y * f - o as f32 * 9.0,
            seed ^ (o * 0x9e37),
        ) * amp;
        norm += amp;
        amp *= 0.5;
        f *= 2.03;
    }
    sum / norm
}

/// Raw height in roughly 0..1 at a position in cells.
fn raw_height(u: f32, v: f32, p: &GenParams) -> f32 {
    let (cols, rows) = (p.cols as f32, p.rows as f32);
    // Warp the domain so coastlines wander.
    let wu = u + (fbm(u * 0.021 + 11.0, v * 0.021, p.seed ^ 0xa1) - 0.5) * 16.0;
    let wv = v + (fbm(u * 0.021, v * 0.021 + 7.0, p.seed ^ 0xa2) - 0.5) * 16.0;
    let n = fbm5(wu * 0.024, wv * 0.024, p.seed);
    // A broad bump in the middle, falling to sea at the edges.
    let (du, dv) = ((u / cols - 0.5) * 2.0, (v / rows - 0.5) * 2.0);
    let d2 = (du * du + dv * dv).min(2.0);
    let mask = (1.0 - d2 * 0.9).clamp(0.0, 1.0);
    let mut h = n * (0.95 - 0.35 * p.continent) + mask * 0.55 * p.continent;
    // Mountain chains: ridged noise, only on land.
    let r = 1.0 - (2.0 * fbm(wu * 0.036 + 50.0, wv * 0.036, p.seed ^ 0xb7) - 1.0).abs();
    h += r * r * p.ridges * 0.32 * smooth((h - 0.42) / 0.2);
    h
}

pub fn generate(p: &GenParams) -> Generated {
    let (cols, rows) = (p.cols.clamp(8, 2048) as i32, p.rows.clamp(8, 2048) as i32);
    let p = GenParams {
        cols: cols as u32,
        rows: rows as u32,
        ..p.clone()
    };
    let (sw, sh) = (cols * RES, rows * RES);

    // Heights at every sample.
    let mut raw = Vec::with_capacity((sw * sh) as usize);
    for sy in 0..sh {
        for sx in 0..sw {
            raw.push(raw_height(
                (sx as f32 + 0.5) / RES as f32,
                (sy as f32 + 0.5) / RES as f32,
                &p,
            ));
        }
    }
    // Pick the sea level so that `land` of the samples are above it.
    let mut sorted = raw.clone();
    sorted.sort_unstable_by(|a, b| a.total_cmp(b));
    let land = p.land.clamp(0.05, 0.95);
    let sea_raw = sorted[((1.0 - land) * (sorted.len() - 1) as f32) as usize];
    let lo = sorted[0];
    // Scale land by the 99.5th percentile, not the single highest sample, and bend
    // the curve so most land is low and gentle with mountains rare: the top 0.5%
    // may overshoot, everything else sits well inside the range.
    let top = sorted[((sorted.len() - 1) as f32 * 0.995) as usize].max(sea_raw + 1e-3);
    let to_u16 = |h: f32| -> u16 {
        let v = if h >= sea_raw {
            let t = ((h - sea_raw) / (top - sea_raw)).clamp(0.0, 1.3);
            // t^1.5 (as t*sqrt(t): exact on every platform, unlike powf): a mid-height
            // raw value becomes a low hill, only the highest ground reaches mountain
            // heights and snow.
            f32::from(DEFAULT_SEA) + t * t.sqrt() * 26_000.0
        } else {
            f32::from(DEFAULT_SEA) - (sea_raw - h) / (sea_raw - lo).max(1e-4) * 18_000.0
        };
        v.clamp(0.0, 65_535.0) as u16
    };

    let mut terrain = Terrain::new(DEFAULT_SEA);
    for sy in 0..sh {
        for sx in 0..sw {
            terrain.set(sx, sy, to_u16(raw[(sy * sw + sx) as usize]));
        }
    }

    let rivers = trace_rivers(&mut terrain, &p);
    Generated { terrain, rivers }
}

/// Rivers: start on high ground spread apart, flow to the lowest neighbour at
/// one-sample-per-cell resolution until they reach the sea or can go no lower.
fn trace_rivers(terrain: &mut Terrain, p: &GenParams) -> Vec<River> {
    if p.rivers == 0 {
        return vec![];
    }
    let (cols, rows) = (p.cols as i32, p.rows as i32);
    let sea = terrain.sea_level();
    let at = |t: &Terrain, cx: i32, cy: i32| t.get(cx * RES + RES / 2, cy * RES + RES / 2);

    // Candidate sources: cells well above the sea, highest first, spaced apart.
    let mut cells: Vec<(u16, i32, i32)> = Vec::new();
    for cy in 0..rows {
        for cx in 0..cols {
            let h = at(terrain, cx, cy);
            if h > sea + 9_000 {
                cells.push((h, cx, cy));
            }
        }
    }
    cells.sort_unstable_by(|a, b| b.cmp(a));
    let mut sources: Vec<(i32, i32)> = Vec::new();
    for (_, cx, cy) in cells {
        if sources.len() >= p.rivers as usize * 3 {
            break;
        }
        if sources
            .iter()
            .all(|(sx, sy)| (sx - cx).pow(2) + (sy - cy).pow(2) > 12 * 12)
        {
            sources.push((cx, cy));
        }
    }

    let mut out: Vec<River> = Vec::new();
    for (sx, sy) in sources {
        if out.len() >= p.rivers as usize {
            break;
        }
        let mut path = vec![(sx, sy)];
        let (mut cx, mut cy) = (sx, sy);
        for _ in 0..4_000 {
            let here = at(terrain, cx, cy);
            if here < sea {
                break;
            }
            let mut best = (here, cx, cy);
            for (dx, dy) in [
                (1, 0),
                (-1, 0),
                (0, 1),
                (0, -1),
                (1, 1),
                (-1, 1),
                (1, -1),
                (-1, -1),
            ] {
                let (nx, ny) = (cx + dx, cy + dy);
                if nx < 0 || ny < 0 || nx >= cols || ny >= rows {
                    continue;
                }
                let h = at(terrain, nx, ny);
                if h < best.0 {
                    best = (h, nx, ny);
                }
            }
            if (best.1, best.2) == (cx, cy) || path.contains(&(best.1, best.2)) {
                break; // a pit or a loop: the river ends here
            }
            cx = best.1;
            cy = best.2;
            path.push((cx, cy));
        }
        let reaches_sea = at(terrain, cx, cy) < sea;
        if path.len() < 12 || (!reaches_sea && path.len() < 30) {
            continue;
        }
        // Keep every other point (smoother, fewer), always the last one.
        let mut pts: Vec<[f32; 2]> = path
            .iter()
            .step_by(2)
            .map(|&(x, y)| [x as f32 + 0.5, y as f32 + 0.5])
            .collect();
        let last = *path.last().expect("non-empty");
        if pts.last() != Some(&[last.0 as f32 + 0.5, last.1 as f32 + 0.5]) {
            pts.push([last.0 as f32 + 0.5, last.1 as f32 + 0.5]);
        }
        let i = out.len();
        let river = River {
            id: format!("gen_river_{i}"),
            name: format!("River {}", i + 1),
            width: (1.1 + path.len() as f32 * 0.03).min(3.0),
            points: pts,
        };
        terrain.carve_river(&river);
        out.push(river);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small(seed: u32) -> GenParams {
        GenParams {
            seed,
            cols: 48,
            rows: 40,
            land: 0.45,
            continent: 0.6,
            ridges: 0.6,
            rivers: 4,
        }
    }

    #[test]
    fn same_seed_same_world_different_seed_different_world() {
        let a = generate(&small(7));
        let b = generate(&small(7));
        let c = generate(&small(8));
        assert_eq!(a.terrain, b.terrain);
        assert_eq!(a.rivers, b.rivers);
        assert_ne!(a.terrain, c.terrain);
    }

    #[test]
    fn the_land_fraction_is_what_was_asked_for() {
        for land in [0.2f32, 0.45, 0.7] {
            let g = generate(&GenParams {
                land,
                rivers: 0,
                ..small(3)
            });
            let (mut up, mut total) = (0usize, 0usize);
            for sy in 0..40 * RES {
                for sx in 0..48 * RES {
                    total += 1;
                    if g.terrain.get(sx, sy) >= g.terrain.sea_level() {
                        up += 1;
                    }
                }
            }
            let got = up as f32 / total as f32;
            assert!((got - land).abs() < 0.04, "asked {land}, got {got}");
        }
    }

    #[test]
    fn the_world_has_relief_ocean_and_mountains() {
        let g = generate(&small(11));
        let sea = g.terrain.sea_level();
        let (mut lo, mut hi) = (u16::MAX, 0u16);
        for sy in 0..40 * RES {
            for sx in 0..48 * RES {
                let h = g.terrain.get(sx, sy);
                lo = lo.min(h);
                hi = hi.max(h);
            }
        }
        assert!(lo < sea - 6_000, "there is deep ocean: {lo} vs sea {sea}");
        assert!(hi > sea + 20_000, "there are mountains: {hi} vs sea {sea}");
    }

    #[test]
    fn continent_gathers_land_in_the_middle_and_islands_scatter_it() {
        let share_central = |continent: f32| {
            let g = generate(&GenParams {
                continent,
                rivers: 0,
                land: 0.3,
                ..small(5)
            });
            let (mut inner, mut all) = (0, 0);
            for sy in 0..40 * RES {
                for sx in 0..48 * RES {
                    if g.terrain.get(sx, sy) >= g.terrain.sea_level() {
                        all += 1;
                        let (u, v) = (
                            sx as f32 / (48.0 * RES as f32),
                            sy as f32 / (40.0 * RES as f32),
                        );
                        if (u - 0.5).abs() < 0.25 && (v - 0.5).abs() < 0.25 {
                            inner += 1;
                        }
                    }
                }
            }
            inner as f32 / all as f32
        };
        assert!(
            share_central(1.0) > share_central(0.0) + 0.1,
            "{} vs {}",
            share_central(1.0),
            share_central(0.0)
        );
    }

    #[test]
    fn rivers_run_downhill_to_the_sea_and_are_carved_into_the_ground() {
        let g = generate(&GenParams {
            rivers: 5,
            ..small(21)
        });
        assert!(!g.rivers.is_empty(), "a hilly world has rivers");
        let sea = g.terrain.sea_level();
        for r in &g.rivers {
            r.validate().unwrap();
            let h = |p: &[f32; 2]| {
                g.terrain
                    .get((p[0] * RES as f32) as i32, (p[1] * RES as f32) as i32)
            };
            // The source is high ground; the river never climbs by more than the carving noise.
            assert!(h(&r.points[0]) > sea + 3_000);
            let mut lowest = u16::MAX;
            for p in &r.points {
                let v = h(p);
                assert!(
                    v <= lowest.saturating_add(900),
                    "{} climbs at {p:?}",
                    r.name
                );
                lowest = lowest.min(v);
            }
            assert!(r.points.len() >= 6);
        }
        // IDs are unique so they can be added to a document.
        let mut ids: Vec<&str> = g.rivers.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), g.rivers.len());
    }

    #[test]
    fn no_rivers_when_none_are_asked_for() {
        assert!(generate(&GenParams {
            rivers: 0,
            ..small(2)
        })
        .rivers
        .is_empty());
    }
}
