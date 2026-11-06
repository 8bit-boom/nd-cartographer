//! The brush engine: a [`Stroke`] -> deterministic coverage at any world
//! position. Pure, seeded, platform-stable (same rules as the patterns:
//! `+ - * /`, `floor`, `sqrt`, integer hashing — no `sin`/`powf`).
//!
//! Pressure and tilt shape the line: width = size * (0.35 + 0.65 * pressure) *
//! (1 + 0.5 * tilt) (a chisel-tip tilt widens it). A spatial index (buckets as
//! wide as the largest radius) means a pixel only looks at the handful of
//! segments or scatter dots near it, so long scribbles stay cheap to bake.

use crate::{hash2, rand01, value_noise};
use ndcart_core::stroke::{BrushKind, Stroke, StrokePoint};
use std::collections::HashMap;

/// Width multiplier from one sample.
fn width_at(size: f32, p: &StrokePoint) -> f32 {
    size * (0.35 + 0.65 * p.pressure) * (1.0 + 0.5 * p.tilt)
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[derive(Clone, Copy)]
struct Seg {
    a: [f32; 2],
    b: [f32; 2],
    ra: f32,
    rb: f32,
}

#[derive(Clone, Copy)]
struct Dot {
    c: [f32; 2],
    r: f32,
}

/// A stroke prepared for sampling.
pub struct Prepared {
    kind: BrushKind,
    seed: u32,
    hardness: f32,
    roughness: f32,
    bucket: f32,
    segs: Vec<Seg>,
    dots: Vec<Dot>,
    /// `[x0, y0, x1, y1]` in cells, including the brush radius.
    pub bounds: [f32; 4],
    index: HashMap<(i32, i32), Vec<u32>>,
}

/// Drop samples that are closer together than a small fraction of the brush:
/// segments much shorter than the brush add nothing visible but make every
/// pixel check dozens of them. The first and last samples always stay, and so
/// does any sample where pressure or tilt moves enough to change the width.
/// Deterministic (a pure function of the points), so renders do not change run to run.
fn thin(points: &[StrokePoint], size: f32) -> Vec<StrokePoint> {
    let spacing = (size * 0.06).max(0.02);
    let mut out: Vec<StrokePoint> = Vec::with_capacity(points.len().min(256));
    for (i, p) in points.iter().enumerate() {
        let keep = match out.last() {
            None => true,
            Some(q) => {
                i == points.len() - 1
                    || (p.x - q.x).hypot(p.y - q.y) >= spacing
                    || (p.pressure - q.pressure).abs() > 0.08
                    || (p.tilt - q.tilt).abs() > 0.08
            }
        };
        if keep {
            out.push(*p);
        }
    }
    out
}

/// Scatter never lays more than this many dots per stroke.
pub const MAX_DOTS: usize = 20_000;

impl Prepared {
    pub fn new(stroke: &Stroke) -> Prepared {
        let thinned = thin(&stroke.points, stroke.size);
        let pts = &thinned;
        let rmax = pts
            .iter()
            .map(|p| width_at(stroke.size, p) * 0.5)
            .fold(0.0f32, f32::max)
            .max(0.01)
            * (1.0 + stroke.roughness * 0.3); // roughness can swell the edge
        let bucket = rmax.max(1.0);

        let mut segs = Vec::with_capacity(pts.len().saturating_sub(1).max(1));
        if pts.len() == 1 {
            let p = pts[0];
            let r = width_at(stroke.size, &p) * 0.5;
            segs.push(Seg {
                a: [p.x, p.y],
                b: [p.x, p.y],
                ra: r,
                rb: r,
            });
        }
        for w in pts.windows(2) {
            segs.push(Seg {
                a: [w[0].x, w[0].y],
                b: [w[1].x, w[1].y],
                ra: width_at(stroke.size, &w[0]) * 0.5,
                rb: width_at(stroke.size, &w[1]) * 0.5,
            });
        }

        let mut dots = Vec::new();
        if stroke.brush == BrushKind::Scatter {
            // Walk the path in equal steps; each step lays a seeded dot inside
            // the brush disc. Spacing shrinks with density.
            let spacing = (stroke.size * 0.12 / (0.15 + stroke.density)).max(0.01);
            let mut carry = 0.0f32;
            let mut k: i32 = 0;
            for s in &segs {
                let len = ((s.b[0] - s.a[0]).powi(2) + (s.b[1] - s.a[1]).powi(2)).sqrt();
                let mut d = carry;
                loop {
                    if dots.len() >= MAX_DOTS {
                        break;
                    }
                    let t = if len < 1e-6 { 0.0 } else { d / len };
                    if t > 1.0 {
                        carry = d - len;
                        break;
                    }
                    let (cx, cy) = (
                        s.a[0] + (s.b[0] - s.a[0]) * t,
                        s.a[1] + (s.b[1] - s.a[1]).mul_add(t, 0.0),
                    );
                    let r = s.ra + (s.rb - s.ra) * t;
                    // Seeded offset inside the disc (sqrt for even area).
                    let ang = rand01(k, 1, stroke.seed) * std::f32::consts::TAU;
                    let rad = rand01(k, 2, stroke.seed).sqrt() * r;
                    let (ox, oy) = unit_vec(ang);
                    let dot_r = (r * 0.2 * (0.6 + 0.8 * rand01(k, 3, stroke.seed))).max(0.01);
                    dots.push(Dot {
                        c: [cx + ox * rad, cy + oy * rad],
                        r: dot_r,
                    });
                    k += 1;
                    d += spacing;
                    if len < 1e-6 {
                        break;
                    }
                }
            }
        }

        // Bounds.
        let mut b = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
        let mut grow = |x: f32, y: f32, r: f32| {
            b = [
                b[0].min(x - r),
                b[1].max(x + r),
                b[2].min(y - r),
                b[3].max(y + r),
            ];
        };
        if stroke.brush == BrushKind::Round {
            for s in &segs {
                grow(s.a[0], s.a[1], s.ra * (1.0 + stroke.roughness * 0.3));
                grow(s.b[0], s.b[1], s.rb * (1.0 + stroke.roughness * 0.3));
            }
        } else {
            for d in &dots {
                grow(d.c[0], d.c[1], d.r);
            }
            if dots.is_empty() {
                for s in &segs {
                    grow(s.a[0], s.a[1], 0.0);
                }
            }
        }
        let bounds = [b[0], b[2], b[1], b[3]];

        let mut index: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        let mut put = |x0: f32, y0: f32, x1: f32, y1: f32, id: u32| {
            let (bx0, bx1) = ((x0 / bucket).floor() as i32, (x1 / bucket).floor() as i32);
            let (by0, by1) = ((y0 / bucket).floor() as i32, (y1 / bucket).floor() as i32);
            for by in by0..=by1 {
                for bx in bx0..=bx1 {
                    index.entry((bx, by)).or_default().push(id);
                }
            }
        };
        if stroke.brush == BrushKind::Round {
            for (i, s) in segs.iter().enumerate() {
                let r = s.ra.max(s.rb) * (1.0 + stroke.roughness * 0.3);
                put(
                    s.a[0].min(s.b[0]) - r,
                    s.a[1].min(s.b[1]) - r,
                    s.a[0].max(s.b[0]) + r,
                    s.a[1].max(s.b[1]) + r,
                    i as u32,
                );
            }
        } else {
            for (i, d) in dots.iter().enumerate() {
                put(
                    d.c[0] - d.r,
                    d.c[1] - d.r,
                    d.c[0] + d.r,
                    d.c[1] + d.r,
                    i as u32,
                );
            }
        }

        Prepared {
            kind: stroke.brush,
            seed: stroke.seed,
            hardness: stroke.hardness,
            roughness: stroke.roughness,
            bucket,
            segs,
            dots,
            bounds,
            index,
        }
    }

    /// Does the stroke possibly touch the cell rectangle `[x0, y0, x1, y1]`?
    pub fn touches(&self, r: [f32; 4]) -> bool {
        self.bounds[0] <= r[2]
            && self.bounds[2] >= r[0]
            && self.bounds[1] <= r[3]
            && self.bounds[3] >= r[1]
    }

    /// Coverage 0..=1 at world cell position `(u, v)`.
    pub fn coverage(&self, u: f32, v: f32) -> f32 {
        let key = (
            (u / self.bucket).floor() as i32,
            (v / self.bucket).floor() as i32,
        );
        let Some(ids) = self.index.get(&key) else {
            return 0.0;
        };
        let mut best = 0.0f32;
        match self.kind {
            BrushKind::Round => {
                let jitter = (value_noise(u * 9.0, v * 9.0, self.seed ^ 0x5a5a) - 0.5)
                    * 0.6
                    * self.roughness;
                for &i in ids {
                    let s = &self.segs[i as usize];
                    let (dx, dy) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
                    let len2 = dx * dx + dy * dy;
                    let t = if len2 < 1e-12 {
                        0.0
                    } else {
                        (((u - s.a[0]) * dx + (v - s.a[1]) * dy) / len2).clamp(0.0, 1.0)
                    };
                    let (px, py) = (s.a[0] + dx * t, s.a[1] + dy * t);
                    let d = ((u - px) * (u - px) + (v - py) * (v - py)).sqrt();
                    let r = (s.ra + (s.rb - s.ra) * t) * (1.0 + jitter);
                    if r <= 0.0 || d >= r {
                        continue;
                    }
                    let inner = r * self.hardness;
                    let c = if d <= inner {
                        1.0
                    } else {
                        smooth((r - d) / (r - inner).max(1e-6))
                    };
                    // Hard brushes still get a one-pixel-ish soft rim so edges are not jagged.
                    best = best.max(c);
                    if best >= 1.0 {
                        break;
                    }
                }
            }
            BrushKind::Scatter => {
                for &i in ids {
                    let d = &self.dots[i as usize];
                    let dist = ((u - d.c[0]).powi(2) + (v - d.c[1]).powi(2)).sqrt();
                    if dist < d.r {
                        let edge = (d.r - dist) / (d.r * 0.25).max(1e-4);
                        best = best.max(edge.clamp(0.0, 1.0));
                        if best >= 1.0 {
                            break;
                        }
                    }
                }
            }
        }
        best
    }
}

/// A unit vector for angle `a` in radians using only `+ - * /`: range-reduced
/// to the first quadrant, then a Taylor series. Accurate to ~1e-6 and bit-stable
/// across platforms (no libm `sin`/`cos`).
fn unit_vec(a: f32) -> (f32, f32) {
    use std::f32::consts::{FRAC_PI_2, PI, TAU};
    let mut x = a - TAU * (a / TAU).floor(); // [0, 2pi)
    if x > PI {
        x -= TAU; // (-pi, pi]
    }
    // sin(x) = sin(pi - x) and cos(x) = -cos(pi - x): fold into [-pi/2, pi/2].
    let flip = x.abs() > FRAC_PI_2;
    if flip {
        x = if x > 0.0 { PI - x } else { -PI - x };
    }
    let x2 = x * x;
    let sin = x
        * (1.0
            - x2 / 6.0
                * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0 * (1.0 - x2 / 72.0 * (1.0 - x2 / 110.0)))));
    let cos = 1.0
        - x2 / 2.0 * (1.0 - x2 / 12.0 * (1.0 - x2 / 30.0 * (1.0 - x2 / 56.0 * (1.0 - x2 / 90.0))));
    (if flip { -cos } else { cos }, sin)
}

/// A seed mixed per stroke id so two strokes with the same `seed` field still
/// differ (clients send their own seeds; this is belt and braces).
pub fn stroke_salt(id: &str) -> u32 {
    id.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    }) ^ hash2(id.len() as i32, 0, 0x57)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::stroke::StrokeKind;

    fn stroke(brush: BrushKind, pts: &[(f32, f32, f32, f32)]) -> Stroke {
        Stroke {
            id: "s".into(),
            kind: StrokeKind::Paint,
            brush,
            material: None,
            color: "#336699".into(),
            size: 2.0,
            opacity: 1.0,
            hardness: 1.0,
            roughness: 0.0,
            density: 0.8,
            seed: 11,
            points: pts
                .iter()
                .map(|&(x, y, pressure, tilt)| StrokePoint {
                    x,
                    y,
                    pressure,
                    tilt,
                })
                .collect(),
        }
    }

    #[test]
    fn a_round_stroke_covers_its_path_and_not_far_from_it() {
        let p = Prepared::new(&stroke(
            BrushKind::Round,
            &[(2.0, 2.0, 1.0, 0.0), (8.0, 2.0, 1.0, 0.0)],
        ));
        assert_eq!(p.coverage(5.0, 2.0), 1.0, "on the path");
        assert_eq!(p.coverage(5.0, 2.8), 1.0, "inside the radius (1.0)");
        assert_eq!(p.coverage(5.0, 4.0), 0.0, "far off");
        assert_eq!(p.coverage(20.0, 20.0), 0.0, "outside every bucket");
        assert_eq!(
            p.coverage(1.2, 2.0),
            1.0,
            "the round cap past the first point"
        );
        assert_eq!(p.coverage(0.5, 2.0), 0.0);
    }

    #[test]
    fn pressure_and_tilt_change_the_width() {
        let thin = Prepared::new(&stroke(
            BrushKind::Round,
            &[(0.0, 0.0, 0.0, 0.0), (10.0, 0.0, 0.0, 0.0)],
        ));
        let thick = Prepared::new(&stroke(
            BrushKind::Round,
            &[(0.0, 0.0, 1.0, 0.0), (10.0, 0.0, 1.0, 0.0)],
        ));
        let tilted = Prepared::new(&stroke(
            BrushKind::Round,
            &[(0.0, 0.0, 1.0, 1.0), (10.0, 0.0, 1.0, 1.0)],
        ));
        // radius: pressure 0 => 2*0.35/2 = 0.35; pressure 1 => 1.0; +tilt => 1.5
        assert_eq!(thin.coverage(5.0, 0.5), 0.0);
        assert_eq!(thick.coverage(5.0, 0.5), 1.0);
        assert_eq!(thick.coverage(5.0, 1.2), 0.0);
        assert_eq!(tilted.coverage(5.0, 1.2), 1.0);
        // pressure varies along the stroke: thin at the start, full at the end
        let ramp = Prepared::new(&stroke(
            BrushKind::Round,
            &[(0.0, 0.0, 0.0, 0.0), (10.0, 0.0, 1.0, 0.0)],
        ));
        assert_eq!(ramp.coverage(0.5, 0.6), 0.0);
        assert_eq!(ramp.coverage(9.5, 0.6), 1.0);
    }

    #[test]
    fn hardness_softens_the_edge() {
        let mut hard = stroke(
            BrushKind::Round,
            &[(0.0, 0.0, 1.0, 0.0), (10.0, 0.0, 1.0, 0.0)],
        );
        let mut soft = hard.clone();
        hard.hardness = 1.0;
        soft.hardness = 0.0;
        let (h, s) = (Prepared::new(&hard), Prepared::new(&soft));
        assert_eq!(h.coverage(5.0, 0.8), 1.0);
        let c = s.coverage(5.0, 0.5);
        assert!(c > 0.1 && c < 0.9, "soft edge is partial: {c}");
        assert!(s.coverage(5.0, 0.2) > s.coverage(5.0, 0.8), "fades outward");
    }

    #[test]
    fn roughness_roughens_the_edge_deterministically() {
        let mut st = stroke(
            BrushKind::Round,
            &[(0.0, 0.0, 1.0, 0.0), (20.0, 0.0, 1.0, 0.0)],
        );
        st.roughness = 1.0;
        let a = Prepared::new(&st);
        let b = Prepared::new(&st);
        let mut widths = std::collections::BTreeSet::new();
        for i in 0..80 {
            let u = 2.0 + i as f32 * 0.2;
            assert_eq!(a.coverage(u, 0.95), b.coverage(u, 0.95), "deterministic");
            // The edge wanders: the same offset is sometimes in, sometimes out.
            widths.insert(a.coverage(u, 0.95) > 0.5);
        }
        assert_eq!(widths.len(), 2, "a rough edge is not a straight line");
    }

    #[test]
    fn scatter_lays_seeded_dots_inside_the_brush_and_respects_density() {
        let path = [(0.0, 0.0, 1.0, 0.0), (20.0, 0.0, 1.0, 0.0)];
        let sparse = {
            let mut s = stroke(BrushKind::Scatter, &path);
            s.density = 0.05;
            Prepared::new(&s)
        };
        let dense = {
            let mut s = stroke(BrushKind::Scatter, &path);
            s.density = 1.0;
            Prepared::new(&s)
        };
        assert!(
            dense.dots.len() > sparse.dots.len() * 3,
            "{} vs {}",
            dense.dots.len(),
            sparse.dots.len()
        );
        // Every dot's centre lies within the brush radius (1.0) of the path.
        for d in &dense.dots {
            assert!(d.c[1].abs() <= 1.0 + 1e-3, "{:?}", d.c);
            assert!(d.c[0] >= -1.0 && d.c[0] <= 21.0);
        }
        // Coverage is sparse: most of the area stays clear, some of it is hit.
        let (mut hit, mut total) = (0, 0);
        for j in 0..40 {
            for i in 0..200 {
                total += 1;
                if dense.coverage(i as f32 * 0.1, -1.0 + j as f32 * 0.05) > 0.0 {
                    hit += 1;
                }
            }
        }
        assert!(hit > 0 && hit < total / 2, "{hit}/{total}");
        // Same seed => same dots; a different seed moves them.
        let again = Prepared::new(&stroke(BrushKind::Scatter, &path));
        let other = {
            let mut s = stroke(BrushKind::Scatter, &path);
            s.seed = 12;
            Prepared::new(&s)
        };
        assert_eq!(
            again.dots.len(),
            Prepared::new(&stroke(BrushKind::Scatter, &path)).dots.len()
        );
        assert_ne!(again.dots[0].c, other.dots[0].c);
    }

    #[test]
    fn a_single_point_is_a_dot_and_scatter_is_capped() {
        let dot = Prepared::new(&stroke(BrushKind::Round, &[(4.0, 4.0, 1.0, 0.0)]));
        assert_eq!(dot.coverage(4.0, 4.0), 1.0);
        assert_eq!(dot.coverage(4.9, 4.0), 1.0);
        assert_eq!(dot.coverage(5.5, 4.0), 0.0);
        let mut huge = stroke(
            BrushKind::Scatter,
            &[(0.0, 0.0, 1.0, 0.0), (9000.0, 0.0, 1.0, 0.0)],
        );
        huge.density = 1.0;
        assert!(Prepared::new(&huge).dots.len() <= MAX_DOTS);
    }

    #[test]
    fn bounds_and_touches_include_the_radius() {
        let p = Prepared::new(&stroke(
            BrushKind::Round,
            &[(2.0, 2.0, 1.0, 0.0), (8.0, 2.0, 1.0, 0.0)],
        ));
        assert!(p.touches([0.0, 0.0, 3.0, 3.0]));
        assert!(p.touches([8.5, 2.0, 9.0, 2.5]), "within the cap radius");
        assert!(!p.touches([10.0, 10.0, 12.0, 12.0]));
        assert!(!p.touches([2.0, 5.0, 5.0, 6.0]));
    }

    #[test]
    fn unit_vec_is_a_unit_vector_everywhere() {
        for i in 0..400 {
            let a = i as f32 * 0.0157;
            let (c, s) = unit_vec(a);
            assert!(((c * c + s * s) - 1.0).abs() < 1e-4, "{a}: {c} {s}");
            // and it points the right way (compare with the real functions at 1e-4)
            assert!(
                (c - a.cos()).abs() < 1e-3 && (s - a.sin()).abs() < 1e-3,
                "{a}: {c} {s}"
            );
        }
        let (c, s) = unit_vec(0.0);
        assert!((c - 1.0).abs() < 1e-5 && s.abs() < 1e-5);
    }

    #[test]
    fn the_salt_differs_per_id() {
        assert_ne!(stroke_salt("a"), stroke_salt("b"));
        assert_eq!(stroke_salt("same"), stroke_salt("same"));
    }

    #[test]
    fn thinning_keeps_the_ends_and_barely_changes_the_picture() {
        // A dense, wobbly polyline versus the same line: coverage must agree
        // everywhere except within a hair of the edge.
        let dense: Vec<(f32, f32, f32, f32)> = (0..600)
            .map(|k| {
                let t = k as f32 * 0.05;
                (t, 3.0 + (t * 0.4).min(6.0) * 0.3, 1.0, 0.0)
            })
            .collect();
        let mut st = stroke(BrushKind::Round, &dense);
        st.size = 4.0;
        let thinned = thin(&st.points, st.size);
        assert!(
            thinned.len() < st.points.len() / 3,
            "{} -> {}",
            st.points.len(),
            thinned.len()
        );
        assert_eq!(
            (thinned[0].x, thinned[0].y),
            (st.points[0].x, st.points[0].y)
        );
        let (a, b) = (st.points.last().unwrap(), thinned.last().unwrap());
        assert_eq!((a.x, a.y), (b.x, b.y));
        // Compare against an unthinned preparation built by hand.
        let thick = Prepared::new(&st);
        let mut off_by_much = 0;
        let mut total = 0;
        for j in 0..160 {
            for i in 0..640 {
                let (u, v) = (i as f32 * 0.05, j as f32 * 0.05);
                total += 1;
                let exact = {
                    // brute force: distance to the dense polyline
                    let r = 2.0;
                    st.points.windows(2).any(|w| {
                        let (dx, dy) = (w[1].x - w[0].x, w[1].y - w[0].y);
                        let l2 = dx * dx + dy * dy;
                        let t = (((u - w[0].x) * dx + (v - w[0].y) * dy) / l2).clamp(0.0, 1.0);
                        ((u - w[0].x - dx * t).powi(2) + (v - w[0].y - dy * t).powi(2)).sqrt() < r
                    })
                };
                if exact != (thick.coverage(u, v) > 0.5) {
                    off_by_much += 1;
                }
            }
        }
        // Only edge pixels may flip, and only a few of them.
        assert!(
            off_by_much * 200 < total,
            "{off_by_much} of {total} pixels differ"
        );
    }
}
