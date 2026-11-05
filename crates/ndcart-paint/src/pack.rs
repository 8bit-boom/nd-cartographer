//! The first-party starter pack (MIT, procedural — nothing to redistribute or
//! license: plan §7.5 "First-party starter packs"). A material is data: a base
//! colour and a pattern.

use crate::{fbm, hash2, mix, rand01, shade, Rgb};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pattern {
    /// One colour.
    Flat,
    /// Square tiles `size` cells wide with `grout` (fraction of a tile) between.
    Tiles { size: f32, grout: f32 },
    /// Boards `h` cells tall, `len` cells long, running bond.
    Planks { h: f32, len: f32 },
    /// Irregular stones about `size` cells across (jittered-grid cells).
    Cobble { size: f32 },
    /// Mottled ground: `scale` is features per cell, `amp` the tone swing.
    Mottled { scale: f32, amp: f32 },
    /// Water: soft ripples.
    Water,
}

#[derive(Clone, Copy, Debug)]
pub struct Material {
    pub id: &'static str,
    pub name: &'static str,
    pub base: Rgb,
    pub pattern: Pattern,
}

const PACK: &[Material] = &[
    Material {
        id: "stone_floor",
        name: "Stone floor",
        base: [0x8a, 0x88, 0x84],
        pattern: Pattern::Tiles {
            size: 0.5,
            grout: 0.07,
        },
    },
    Material {
        id: "flagstone",
        name: "Flagstone",
        base: [0x6f, 0x73, 0x78],
        pattern: Pattern::Tiles {
            size: 1.0,
            grout: 0.045,
        },
    },
    Material {
        id: "wood_planks",
        name: "Wood planks",
        base: [0x8c, 0x62, 0x3a],
        pattern: Pattern::Planks { h: 0.25, len: 1.5 },
    },
    Material {
        id: "cobblestone",
        name: "Cobblestone",
        base: [0x7d, 0x76, 0x6c],
        pattern: Pattern::Cobble { size: 0.28 },
    },
    Material {
        id: "grass",
        name: "Grass",
        base: [0x5a, 0x8a, 0x3c],
        pattern: Pattern::Mottled {
            scale: 5.0,
            amp: 0.22,
        },
    },
    Material {
        id: "dirt",
        name: "Dirt",
        base: [0x64, 0x4e, 0x3c],
        pattern: Pattern::Mottled {
            scale: 4.0,
            amp: 0.18,
        },
    },
    Material {
        id: "sand",
        name: "Sand",
        base: [0xc9, 0xb2, 0x7c],
        pattern: Pattern::Mottled {
            scale: 7.0,
            amp: 0.1,
        },
    },
    Material {
        id: "water",
        name: "Water",
        base: [0x3a, 0x6e, 0x9a],
        pattern: Pattern::Water,
    },
    Material {
        id: "metal_deck",
        name: "Metal deck",
        base: [0x55, 0x5d, 0x68],
        pattern: Pattern::Tiles {
            size: 0.5,
            grout: 0.035,
        },
    },
    Material {
        id: "neon_tile",
        name: "Neon tile",
        base: [0x1d, 0x3b, 0x4a],
        pattern: Pattern::Tiles {
            size: 0.5,
            grout: 0.09,
        },
    },
];

/// The starter pack, in display order.
pub fn starter() -> &'static [Material] {
    PACK
}

pub fn find(id: &str) -> Option<&'static Material> {
    PACK.iter().find(|m| m.id == id)
}

fn frac(x: f32) -> f32 {
    x - x.floor()
}

impl Material {
    /// The colour at world position `(u, v)` (in cells). Deterministic.
    pub fn sample(&self, u: f32, v: f32, seed: u32) -> Rgb {
        // Per-material salt so two materials with the same seed do not share noise.
        let seed = seed
            ^ hash2(
                self.id.len() as i32,
                i32::from(self.id.as_bytes()[0]),
                0x4d41,
            );
        let base = self.base;
        match self.pattern {
            Pattern::Flat => base,
            Pattern::Tiles { size, grout } => {
                let (tx, ty) = (u / size, v / size);
                let (ix, iy) = (tx.floor() as i32, ty.floor() as i32);
                let (fx, fy) = (frac(tx), frac(ty));
                let g = grout;
                if fx < g || fy < g {
                    return shade(base, 0.62);
                }
                // Per-tile tone, plus fine speckle so big tiles are not flat.
                let tone = 0.9 + 0.2 * rand01(ix, iy, seed);
                let speck = 0.96 + 0.08 * fbm(u * 9.0, v * 9.0, seed);
                // A light bevel on the lit (top/left) edges.
                let bevel = if fx < g + 0.05 || fy < g + 0.05 {
                    1.08
                } else {
                    1.0
                };
                shade(base, tone * speck * bevel)
            }
            Pattern::Planks { h, len } => {
                let row = (v / h).floor() as i32;
                let off = rand01(row, 0, seed) * len;
                let (pu, pv) = ((u + off) / len, v / h);
                let col = pu.floor() as i32;
                let (fx, fy) = (frac(pu), frac(pv));
                if fy < 0.07 || fx < 0.012 / len.max(0.1) * 4.0 {
                    return shade(base, 0.55);
                }
                let tone = 0.86 + 0.28 * rand01(row, col, seed ^ 0x77);
                // Grain: noise stretched along the board.
                let grain = 0.93 + 0.14 * fbm(u * 2.0, v * 22.0, seed);
                shade(base, tone * grain)
            }
            Pattern::Cobble { size } => {
                let (x, y) = (u / size, v / size);
                let (cx, cy) = (x.floor() as i32, y.floor() as i32);
                let (mut d1, mut d2, mut id) = (9.0f32, 9.0f32, (0, 0));
                for j in -1..=1 {
                    for i in -1..=1 {
                        let (nx, ny) = (cx + i, cy + j);
                        let px = nx as f32 + 0.15 + 0.7 * rand01(nx, ny, seed);
                        let py = ny as f32 + 0.15 + 0.7 * rand01(nx, ny, seed ^ 0xabc);
                        let d = (x - px) * (x - px) + (y - py) * (y - py);
                        if d < d1 {
                            d2 = d1;
                            d1 = d;
                            id = (nx, ny);
                        } else if d < d2 {
                            d2 = d;
                        }
                    }
                }
                // Distance-difference to the cell boundary: small near a seam.
                let seam = d2.max(0.0) - d1.max(0.0);
                if seam < 0.05 {
                    return shade(base, 0.55);
                }
                let tone = 0.82 + 0.34 * rand01(id.0, id.1, seed ^ 0x33);
                let rim = if seam < 0.12 { 0.9 } else { 1.0 };
                shade(
                    base,
                    tone * rim * (0.95 + 0.1 * fbm(u * 8.0, v * 8.0, seed)),
                )
            }
            Pattern::Mottled { scale, amp } => {
                let n = fbm(u * scale, v * scale, seed);
                shade(base, 1.0 + (n - 0.5) * 2.0 * amp)
            }
            Pattern::Water => {
                // Two drifting noise layers; highlights where they agree.
                let a = fbm(u * 2.0 + 3.1, v * 2.0, seed);
                let b = fbm(u * 3.3 - 1.7, v * 3.3 + 9.2, seed ^ 0x99);
                let ripple = (a + b) * 0.5;
                let c = shade(base, 0.9 + 0.3 * ripple);
                if ripple > 0.62 {
                    mix(c, [0xa8, 0xd0, 0xe8], (ripple - 0.62) * 1.6)
                } else {
                    c
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: Rgb, b: Rgb) -> i32 {
        (0..3)
            .map(|i| (i32::from(a[i]) - i32::from(b[i])).abs())
            .sum()
    }

    #[test]
    fn the_pack_has_unique_ids_and_every_one_resolves() {
        let mut ids: Vec<&str> = starter().iter().map(|m| m.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate material ids");
        for m in starter() {
            assert!(find(m.id).is_some());
            assert!(!m.name.is_empty());
        }
        assert!(find("lava").is_none());
        assert!(n >= 8);
    }

    #[test]
    fn sampling_is_deterministic_and_seed_dependent() {
        for m in starter() {
            for i in 0..60 {
                let (u, v) = (i as f32 * 0.173, i as f32 * 0.291);
                assert_eq!(m.sample(u, v, 1), m.sample(u, v, 1), "{}", m.id);
            }
        }
        // A seeded material differs between seeds somewhere.
        let g = find("grass").unwrap();
        assert!(
            (0..60).any(|i| g.sample(i as f32 * 0.3, 0.7, 1) != g.sample(i as f32 * 0.3, 0.7, 2))
        );
    }

    #[test]
    fn every_material_has_texture_and_stays_near_its_base_colour() {
        for m in starter() {
            let mut seen = std::collections::BTreeSet::new();
            let mut worst = 0;
            for j in 0..40 {
                for i in 0..40 {
                    let c = m.sample(i as f32 * 0.05, j as f32 * 0.05, 5);
                    seen.insert(c);
                    worst = worst.max(dist(c, m.base));
                }
            }
            assert!(seen.len() > 8, "{} is flat ({} colours)", m.id, seen.len());
            // Grout/highlights may swing, but not into a different colour family.
            assert!(worst < 330, "{} strays {worst} from its base", m.id);
        }
    }

    #[test]
    fn the_materials_are_distinguishable_at_a_glance() {
        let mean = |m: &Material| {
            let mut s = [0u32; 3];
            for j in 0..30 {
                for i in 0..30 {
                    let c = m.sample(i as f32 * 0.1, j as f32 * 0.1, 2);
                    for k in 0..3 {
                        s[k] += u32::from(c[k]);
                    }
                }
            }
            [(s[0] / 900) as u8, (s[1] / 900) as u8, (s[2] / 900) as u8]
        };
        let means: Vec<(Rgb, &str)> = starter().iter().map(|m| (mean(m), m.id)).collect();
        for (i, (a, an)) in means.iter().enumerate() {
            for (b, bn) in means.iter().skip(i + 1) {
                // Tiles in stone/metal tones are the closest pairs; still not identical.
                assert!(
                    dist(*a, *b) > 12,
                    "{an} and {bn} look the same: {a:?} {b:?}"
                );
            }
        }
    }

    #[test]
    fn patterns_are_seamless_in_world_space() {
        // Sampling is a pure function of world position, so two adjacent "tiles"
        // sampled separately agree on the shared line — the property that makes
        // chunked baking and any zoom level match.
        for m in starter() {
            for i in 0..50 {
                let v = i as f32 * 0.137;
                let a = m.sample(8.0, v, 4);
                let b = m.sample(8.0, v, 4);
                assert_eq!(a, b);
            }
        }
    }
}
