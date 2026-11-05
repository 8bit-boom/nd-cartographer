//! What a token can see: an exact line-of-sight sweep over the map's walls.
//! Walls, closed doors and secret doors stop sight; windows, open doors and
//! archways do not. The player preview uses it to draw the fog, and the light
//! radii use the same sweep so a lamp does not shine through a wall.

use crate::walls::{Wall, WallKind, WallState};

/// A wall segment in cells.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seg {
    pub a: [f64; 2],
    pub b: [f64; 2],
}

/// The sight-blocking segments of a floor's walls (`cell_size` px per cell).
pub fn blockers(walls: &[Wall], cell_size: f64) -> Vec<Seg> {
    let mut out = Vec::new();
    for w in walls {
        let blocks = match w.kind {
            WallKind::Wall => true,
            WallKind::Window => false,
            WallKind::Door | WallKind::Secret => w.state != Some(WallState::Open),
        };
        if !blocks {
            continue;
        }
        for pair in w.pts.windows(2) {
            out.push(Seg {
                a: [pair[0][0] / cell_size, pair[0][1] / cell_size],
                b: [pair[1][0] / cell_size, pair[1][1] / cell_size],
            });
        }
    }
    out
}

/// Distance along the ray `o + t*d` (|d| = 1) to segment `s`, if it hits.
fn hit(o: [f64; 2], d: [f64; 2], s: &Seg) -> Option<f64> {
    let (ex, ey) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
    let den = d[0] * ey - d[1] * ex;
    if den.abs() < 1e-12 {
        return None; // parallel
    }
    let (ax, ay) = (s.a[0] - o[0], s.a[1] - o[1]);
    let t = (ax * ey - ay * ex) / den;
    let u = (ax * d[1] - ay * d[0]) / den;
    (t > 1e-9 && (-1e-9..=1.0 + 1e-9).contains(&u)).then_some(t)
}

fn near(s: &Seg, o: [f64; 2], r: f64) -> bool {
    // Distance from the point to the segment, squared.
    let (ex, ey) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
    let len2 = ex * ex + ey * ey;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((o[0] - s.a[0]) * ex + (o[1] - s.a[1]) * ey) / len2).clamp(0.0, 1.0)
    };
    let (px, py) = (s.a[0] + t * ex - o[0], s.a[1] + t * ey - o[1]);
    px * px + py * py <= r * r
}

/// The area visible from `origin` out to `radius` cells, as a polygon (cells).
/// An origin inside a closed wall loop sees only that room.
pub fn visible_polygon(origin: [f64; 2], segs: &[Seg], radius: f64) -> Vec<[f64; 2]> {
    let local: Vec<Seg> = segs
        .iter()
        .copied()
        .filter(|s| near(s, origin, radius))
        .collect();
    let mut angles: Vec<f64> = (0..96)
        .map(|i| i as f64 * std::f64::consts::TAU / 96.0)
        .collect();
    for s in &local {
        for p in [s.a, s.b] {
            let a = (p[1] - origin[1]).atan2(p[0] - origin[0]);
            angles.extend([a - 1e-4, a, a + 1e-4].map(|v| v.rem_euclid(std::f64::consts::TAU)));
        }
    }
    angles.sort_by(|x, y| x.total_cmp(y));
    angles.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    angles
        .into_iter()
        .map(|a| {
            let d = [a.cos(), a.sin()];
            let t = local
                .iter()
                .filter_map(|s| hit(origin, d, s))
                .fold(radius, f64::min);
            [origin[0] + d[0] * t, origin[1] + d[1] * t]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall(a: [f64; 2], b: [f64; 2], kind: WallKind, state: Option<WallState>) -> Wall {
        Wall {
            id: "w".into(),
            pts: vec![a, b],
            kind,
            state,
        }
    }
    fn area(p: &[[f64; 2]]) -> f64 {
        let mut s = 0.0;
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            s += a[0] * b[1] - b[0] * a[1];
        }
        s.abs() / 2.0
    }
    fn contains(p: &[[f64; 2]], x: f64, y: f64) -> bool {
        let mut inside = false;
        let mut j = p.len() - 1;
        for i in 0..p.len() {
            if (p[i][1] > y) != (p[j][1] > y)
                && x < (p[j][0] - p[i][0]) * (y - p[i][1]) / (p[j][1] - p[i][1]) + p[i][0]
            {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    #[test]
    fn open_ground_is_a_disc_and_a_wall_casts_a_shadow() {
        let disc = visible_polygon([0.0, 0.0], &[], 10.0);
        let pi = std::f64::consts::PI;
        assert!(
            (area(&disc) - pi * 100.0).abs() < pi * 100.0 * 0.01,
            "{}",
            area(&disc)
        );
        // A wall at x = 3 from y = -2 to 2 hides what lies behind it, not what is beside it.
        let w = [wall([3.0, -2.0], [3.0, 2.0], WallKind::Wall, None)];
        let segs = blockers(&w, 1.0);
        let p = visible_polygon([0.0, 0.0], &segs, 10.0);
        assert!(contains(&p, 2.0, 0.0), "in front is visible");
        assert!(!contains(&p, 6.0, 0.0), "behind the wall is hidden");
        assert!(contains(&p, 6.0, 5.0), "past its end is visible");
        assert!(contains(&p, 0.0, 6.0));
    }

    #[test]
    fn doors_windows_and_secrets_follow_their_state() {
        let seen = |kind, state| {
            let w = [wall([3.0, -5.0], [3.0, 5.0], kind, state)];
            contains(
                &visible_polygon([0.0, 0.0], &blockers(&w, 1.0), 10.0),
                6.0,
                0.0,
            )
        };
        assert!(!seen(WallKind::Door, Some(WallState::Closed)));
        assert!(seen(WallKind::Door, Some(WallState::Open)));
        assert!(seen(WallKind::Window, None));
        assert!(!seen(WallKind::Secret, Some(WallState::Closed)));
        assert!(!seen(WallKind::Wall, None));
    }

    #[test]
    fn a_closed_room_sees_only_itself_and_cell_size_scales() {
        // A 4x4 box around the origin, drawn at 32 px per cell.
        let c = 32.0;
        let s = |a: [f64; 2], b: [f64; 2]| {
            wall(
                [a[0] * c, a[1] * c],
                [b[0] * c, b[1] * c],
                WallKind::Wall,
                None,
            )
        };
        let walls = [
            s([-2.0, -2.0], [2.0, -2.0]),
            s([2.0, -2.0], [2.0, 2.0]),
            s([2.0, 2.0], [-2.0, 2.0]),
            s([-2.0, 2.0], [-2.0, -2.0]),
        ];
        let p = visible_polygon([0.0, 0.0], &blockers(&walls, c), 20.0);
        assert!((area(&p) - 16.0).abs() < 0.2, "{}", area(&p));
        assert!(!contains(&p, 5.0, 0.0));
    }
}
