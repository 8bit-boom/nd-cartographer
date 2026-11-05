//! Colour of the ground at a point: hypsometric tints that follow altitude and
//! a moisture field (dry lowlands tan, wet ones green), bare rock on slopes,
//! snow on the heights, depth-shaded water with a pale shoreline, relief
//! shading from a north-west sun, optional contour lines, and the map's global
//! grading (tint, saturation, contrast, brightness).

use ndcart_core::terrain::{HeightGrid, TerrainStyle, RES};
use ndcart_paint::{fbm, mix, shade, Rgb};

const SHALLOW: Rgb = [0x5f, 0xa8, 0xc8];
const DEEP: Rgb = [0x1b, 0x3c, 0x68];
const FOAM: Rgb = [0xe6, 0xf1, 0xee];
const SAND: Rgb = [0xd8, 0xc9, 0x95];
const DRY: Rgb = [0xc4, 0xb4, 0x70];
const WET: Rgb = [0x6a, 0x98, 0x48];
const UPLAND: Rgb = [0x56, 0x7e, 0x3c];
const HILL: Rgb = [0x9c, 0x8e, 0x5e];
const ROCK: Rgb = [0x84, 0x7e, 0x76];
const HIGH: Rgb = [0x9a, 0x96, 0x92];
const SNOW: Rgb = [0xf3, 0xf3, 0xf5];

/// Elevation range above sea level that spans sand to snow.
const LAND_RANGE: f32 = 26_000.0;

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

pub struct Shader<'a> {
    grid: &'a HeightGrid,
    style: &'a TerrainStyle,
    seed: u32,
    tint: Rgb,
}

fn parse_hex(s: &str) -> Rgb {
    let b = s.as_bytes();
    let h = |i: usize| {
        u8::from_str_radix(std::str::from_utf8(&b[i..i + 2]).unwrap_or("ff"), 16).unwrap_or(255)
    };
    if b.len() == 7 {
        [h(1), h(3), h(5)]
    } else {
        [255, 255, 255]
    }
}

impl<'a> Shader<'a> {
    pub fn new(grid: &'a HeightGrid, style: &'a TerrainStyle, seed: u32) -> Shader<'a> {
        Shader {
            grid,
            style,
            seed,
            tint: parse_hex(&style.tint),
        }
    }

    /// The finished colour at a position given in *samples* (continuous; the
    /// height of sample `i` sits at `i + 0.5`).
    pub fn color(&self, su: f32, sv: f32) -> Rgb {
        let (gu, gv) = (su - 0.5, sv - 0.5);
        let h = self.grid.bilinear(gu, gv);
        let sea = f32::from(self.style.sea_level);
        // Central differences, in height units per sample.
        let dx = (self.grid.bilinear(gu + 1.0, gv) - self.grid.bilinear(gu - 1.0, gv)) * 0.5;
        let dy = (self.grid.bilinear(gu, gv + 1.0) - self.grid.bilinear(gu, gv - 1.0)) * 0.5;
        let slope = (dx * dx + dy * dy).sqrt();
        let (cu, cv) = (su / RES as f32, sv / RES as f32);

        let base = if h < sea {
            self.water(h, sea)
        } else {
            self.land(h, sea, slope, cu, cv, dx, dy)
        };
        let base = self.contours(base, h, slope);
        self.grade(base)
    }

    fn water(&self, h: f32, sea: f32) -> Rgb {
        let depth = ((sea - h) / 9_000.0).clamp(0.0, 1.0);
        let c = mix(SHALLOW, DEEP, smooth(depth));
        // A pale rim where land meets water.
        let shore = 1.0 - ((sea - h) / 280.0).clamp(0.0, 1.0);
        mix(c, FOAM, shore * shore * 0.65)
    }

    #[allow(clippy::too_many_arguments)]
    fn land(&self, h: f32, sea: f32, slope: f32, cu: f32, cv: f32, dx: f32, dy: f32) -> Rgb {
        let e = ((h - sea) / LAND_RANGE).clamp(0.0, 1.0);
        let moisture = fbm(cu * 0.11, cv * 0.11, self.seed ^ 0x1234);
        let low = mix(DRY, WET, smooth((moisture - 0.3) / 0.4));
        // Altitude ramp: sand -> lowland -> upland -> hills -> high rock -> snow.
        let mut c = SAND;
        c = mix(c, low, smooth((e - 0.012) / 0.05));
        c = mix(c, UPLAND, smooth((e - 0.12) / 0.2) * (0.4 + 0.6 * moisture));
        c = mix(c, HILL, smooth((e - 0.3) / 0.22));
        c = mix(c, HIGH, smooth((e - 0.52) / 0.2));
        c = mix(c, SNOW, smooth((e - 0.8) / 0.14));
        // Steep ground shows bare rock.
        let rock = smooth((slope - 260.0) / 520.0);
        c = mix(c, ROCK, rock * 0.85);
        // Relief: light from the north-west.
        if self.style.hillshade > 0.0 {
            let k = 0.0024;
            let (nx, ny, nz) = (-dx * k, -dy * k, 1.0f32);
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            let (lx, ly, lz) = (-0.55f32, -0.55f32, 0.63f32);
            let ll = (lx * lx + ly * ly + lz * lz).sqrt();
            let dot = (nx * lx + ny * ly + nz * lz) / (len * ll);
            let flat = lz / ll;
            let lit = 1.0 + (dot - flat) * 1.6 * self.style.hillshade;
            c = shade(c, lit.clamp(0.6, 1.4));
        }
        c
    }

    fn contours(&self, c: Rgb, h: f32, slope: f32) -> Rgb {
        let step = f32::from(self.style.contour_interval);
        if step < 1.0 {
            return c;
        }
        let f = (h / step) - (h / step).floor();
        let dist = f.min(1.0 - f) * step; // height units to the nearest contour
                                          // A line about one sample wide on any slope, thinner on the flats.
        let width = (slope * 0.5).max(18.0);
        if dist < width {
            shade(c, 0.78 + 0.22 * (dist / width))
        } else {
            c
        }
    }

    fn grade(&self, c: Rgb) -> Rgb {
        let s = self.style;
        let mut c = c;
        if s.tint_amount > 0.0 {
            c = mix(c, self.tint, s.tint_amount);
        }
        if (s.saturation - 1.0).abs() > 1e-3 {
            let l = 0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2]);
            let f = |v: u8| (l + (f32::from(v) - l) * s.saturation).clamp(0.0, 255.0) as u8;
            c = [f(c[0]), f(c[1]), f(c[2])];
        }
        if (s.contrast - 1.0).abs() > 1e-3 {
            let f = |v: u8| ((f32::from(v) - 128.0) * s.contrast + 128.0).clamp(0.0, 255.0) as u8;
            c = [f(c[0]), f(c[1]), f(c[2])];
        }
        if (s.brightness - 1.0).abs() > 1e-3 {
            c = shade(c, s.brightness);
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::terrain::{Terrain, TerrainMode, DEFAULT_SEA};

    /// A world: open sea, a gentle island, and one steep mountain.
    fn world() -> Terrain {
        let mut t = Terrain::new(DEFAULT_SEA);
        for sy in 0..160 {
            for sx in 0..160 {
                t.set(sx, sy, DEFAULT_SEA - 4_000);
            }
        }
        for _ in 0..30 {
            t.stroke(TerrainMode::Raise, &[[20.0, 20.0]], 14.0, 1.0, 0);
        }
        for _ in 0..70 {
            t.stroke(TerrainMode::Raise, &[[28.0, 20.0]], 4.0, 1.0, 0);
        }
        t
    }

    fn color(t: &Terrain, cell_x: f32, cell_y: f32) -> Rgb {
        let g = t.extract(0, 0, 160, 160);
        Shader::new(&g, &t.style, 9).color(cell_x * RES as f32, cell_y * RES as f32)
    }

    fn dist(a: Rgb, b: Rgb) -> i32 {
        (0..3)
            .map(|i| (i32::from(a[i]) - i32::from(b[i])).abs())
            .sum()
    }

    #[test]
    fn sea_is_blue_and_darker_with_depth_land_is_not() {
        let mut t = world();
        let shallow = color(&t, 39.0, 5.0);
        t.stroke(TerrainMode::Lower, &[[5.0, 35.0]], 6.0, 1.0, 0);
        for _ in 0..12 {
            t.stroke(TerrainMode::Lower, &[[5.0, 35.0]], 6.0, 1.0, 0);
        }
        let deep = color(&t, 5.0, 35.0);
        for c in [shallow, deep] {
            assert!(c[2] > c[0] && c[2] > 90, "water is blue: {c:?}");
        }
        let lum = |c: Rgb| i32::from(c[0]) + i32::from(c[1]) + i32::from(c[2]);
        assert!(
            lum(deep) < lum(shallow),
            "deep water is darker: {deep:?} vs {shallow:?}"
        );
        let land = color(&t, 20.0, 20.0);
        assert!(land[1] >= land[2], "land is not blue: {land:?}");
    }

    #[test]
    fn altitude_walks_from_sand_through_green_to_rock_and_snow() {
        let t = world();
        let beach = color(&t, 20.0 + 9.2, 20.0); // near the shore
        let mid = color(&t, 20.0 + 5.0, 20.0);
        let peak = color(&t, 28.0, 20.0);
        assert!(
            dist(beach, mid) > 15 && dist(mid, peak) > 15,
            "{beach:?} {mid:?} {peak:?}"
        );
        // The summit is pale (snow/high rock), the lowlands are not.
        let lum = |c: Rgb| i32::from(c[0]) + i32::from(c[1]) + i32::from(c[2]);
        assert!(
            lum(peak) > lum(mid),
            "{peak:?} should be paler than {mid:?}"
        );
    }

    #[test]
    fn relief_is_lit_from_the_north_west() {
        // A ridge running north-south: the west face is lit, the east face is in shade.
        let mut t = Terrain::new(DEFAULT_SEA);
        for sy in 0..160 {
            for sx in 0..160 {
                t.set(sx, sy, DEFAULT_SEA + 3_000);
            }
        }
        for _ in 0..25 {
            let pts: Vec<[f32; 2]> = (0..30).map(|i| [20.0, 5.0 + i as f32]).collect();
            t.stroke(TerrainMode::Raise, &pts, 3.0, 1.0, 0);
        }
        let west = color(&t, 17.8, 20.0);
        let east = color(&t, 22.2, 20.0);
        let lum = |c: Rgb| i32::from(c[0]) + i32::from(c[1]) + i32::from(c[2]);
        assert!(lum(west) > lum(east) + 20, "west {west:?} east {east:?}");
        // With hillshade off the two faces are no longer distinguished by light.
        t.style.hillshade = 0.0;
        let (w0, e0) = (color(&t, 17.8, 20.0), color(&t, 22.2, 20.0));
        assert!((lum(w0) - lum(e0)).abs() < (lum(west) - lum(east)).abs());
    }

    #[test]
    fn grading_tints_desaturates_and_brightens() {
        let mut t = world();
        let plain = color(&t, 24.0, 20.0);
        t.style.tint = "#0000ff".into();
        t.style.tint_amount = 0.5;
        let tinted = color(&t, 24.0, 20.0);
        assert!(tinted[2] > plain[2] && tinted[0] < plain[0]);
        t.style.tint_amount = 0.0;
        t.style.saturation = 0.0;
        let grey = color(&t, 24.0, 20.0);
        assert!(
            grey[0].abs_diff(grey[1]) <= 1 && grey[1].abs_diff(grey[2]) <= 1,
            "{grey:?}"
        );
        t.style.saturation = 1.0;
        t.style.brightness = 1.4;
        let bright = color(&t, 24.0, 20.0);
        assert!(
            i32::from(bright[0]) + i32::from(bright[1]) > i32::from(plain[0]) + i32::from(plain[1])
        );
    }

    #[test]
    fn contours_draw_lines_at_the_interval_and_nowhere_else_when_off() {
        let mut t = world();
        t.style.contour_interval = 1_500;
        let g = t.extract(0, 0, 160, 160);
        let sh = Shader::new(&g, &t.style, 9);
        let mut t2 = t.clone();
        t2.style.contour_interval = 0;
        let g2 = t2.extract(0, 0, 160, 160);
        let sh2 = Shader::new(&g2, &t2.style, 9);
        let (mut dark, mut same) = (0, 0);
        for i in 0..600 {
            let (u, v) = (60.0 + i as f32 * 0.15, 80.0 + (i % 7) as f32);
            let (a, b) = (sh.color(u, v), sh2.color(u, v));
            if a == b {
                same += 1
            } else {
                dark += 1
            }
        }
        assert!(dark > 5, "some pixels are on contour lines");
        assert!(same > dark, "most pixels are not");
    }

    #[test]
    fn shading_is_deterministic_and_depends_on_the_seed_only_through_moisture() {
        let t = world();
        let g = t.extract(0, 0, 160, 160);
        let a = Shader::new(&g, &t.style, 1);
        let b = Shader::new(&g, &t.style, 1);
        let c = Shader::new(&g, &t.style, 2);
        let mut seed_changes = 0;
        for i in 0..300 {
            let (u, v) = (50.0 + i as f32 * 0.31, 70.0 + (i * 3 % 40) as f32);
            assert_eq!(a.color(u, v), b.color(u, v));
            if a.color(u, v) != c.color(u, v) {
                seed_changes += 1;
            }
        }
        assert!(seed_changes > 0, "moisture noise follows the seed");
    }
}
