//! ndcart-paint — materials, not colours (plan §7.1).
//!
//! A material is a procedural pattern sampled in **world cell coordinates**
//! (`u`, `v` in cells, not pixels), so the same spot looks the same at every
//! zoom, in every tile and in a full-map export: no tiling seams, no fixed-size
//! textures to ship. Everything is deterministic from `(material, u, v, seed)`;
//! only `+ - * /`, `floor` and integer hashing are used (no `sin`/`powf`), so
//! the output does not drift between platforms.
//!
//! Adjacent materials blend automatically ([`edge_mix`]): both sides of a
//! shared edge compute the *same* mix (0.5 exactly on the edge), so the join is
//! continuous and the jitter follows world coordinates, not the cell.

mod brush;
mod noise;
mod pack;

pub use brush::{stroke_salt, Prepared, MAX_DOTS};
pub use noise::{fbm, hash2, rand01, value_noise};
pub use pack::{find, starter, Material, Pattern};

/// A colour, 8 bits per channel.
pub type Rgb = [u8; 3];

/// Linear mix in 8-bit space, `t` clamped to 0..=1.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t + 0.5) as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

/// Scale brightness (`k` around 1.0), clamped.
pub fn shade(c: Rgb, k: f32) -> Rgb {
    let f = |x: u8| (f32::from(x) * k).clamp(0.0, 255.0) as u8;
    [f(c[0]), f(c[1]), f(c[2])]
}

/// Width of the blend zone either side of an edge, in cells.
pub const BLEND_WIDTH: f32 = 0.22;

/// Which side of a shared edge a pixel is on. The two sides must disagree so
/// the boundary jitter moves the *join*, not each side independently: that is
/// what makes the blended colour continuous across the edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    /// The cell west of / north of the edge (its neighbour is east / south).
    Near,
    /// The cell east of / south of the edge (its neighbour is west / north).
    Far,
}

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Weight of the pixel's *own* material at distance `d` cells (>= 0) from the
/// shared edge: 0.5 on the edge, 1.0 from [`BLEND_WIDTH`] inward. `(u, v)` is
/// the pixel's world position; the boundary is jittered by world-space noise so
/// it reads as hand-painted. The jitter enters with opposite signs on the two
/// sides, so `own_weight(d, Near) + own_weight(d', Far)` meet exactly at the
/// edge and the join has no seam.
pub fn own_weight(d: f32, side: Side, u: f32, v: f32, seed: u32) -> f32 {
    let j = (value_noise(u * 7.0, v * 7.0, seed ^ 0x9e37) - 0.5) * 0.14;
    let s = match side {
        Side::Near => d + j,
        Side::Far => d - j,
    };
    smooth((s + BLEND_WIDTH) / (2.0 * BLEND_WIDTH))
}

/// The blended colour for a pixel of material colour `own` with neighbouring
/// material colour `other` (sampled at the same spot), `d` cells from the edge.
pub fn edge_mix(own: Rgb, other: Rgb, d: f32, side: Side, u: f32, v: f32, seed: u32) -> Rgb {
    mix(other, own, own_weight(d, side, u, v, seed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_endpoints_and_clamp() {
        assert_eq!(mix([0, 0, 0], [200, 100, 50], 0.0), [0, 0, 0]);
        assert_eq!(mix([0, 0, 0], [200, 100, 50], 1.0), [200, 100, 50]);
        assert_eq!(mix([0, 0, 0], [200, 100, 50], 9.0), [200, 100, 50]);
        assert_eq!(mix([10, 20, 30], [10, 20, 30], 0.5), [10, 20, 30]);
    }

    #[test]
    fn the_weight_is_half_on_the_edge_and_full_beyond_the_zone() {
        for i in 0..200 {
            let (u, v) = (i as f32 * 0.37, i as f32 * 0.11);
            for side in [Side::Near, Side::Far] {
                let w0 = own_weight(0.0, side, u, v, 1);
                assert!((0.25..=0.75).contains(&w0), "{w0}"); // 0.5 +- the jitter (+-0.07 cells of boundary shift)
                assert_eq!(own_weight(BLEND_WIDTH + 0.1, side, u, v, 1), 1.0);
                // monotone: moving inward never lets the neighbour back in
                assert!(own_weight(0.05, side, u, v, 1) >= w0 - 1e-6);
            }
        }
    }

    #[test]
    fn the_join_is_continuous_across_the_edge() {
        // A pixel on the Near side and one on the Far side, both exactly on the
        // edge (d = 0) at the same world spot, must blend to the same colour —
        // for any jitter. This failed when the jitter was added to one weight.
        let (a, b) = ([200u8, 20, 20], [20u8, 20, 200]);
        for i in 0..400 {
            let (u, v) = (3.0 + i as f32 * 0.0137, 4.37 + i as f32 * 0.0291);
            let near = edge_mix(a, b, 0.0, Side::Near, u, v, 7);
            let far = edge_mix(b, a, 0.0, Side::Far, u, v, 7);
            for c in 0..3 {
                assert!(
                    (i32::from(near[c]) - i32::from(far[c])).abs() <= 1,
                    "i={i}: {near:?} vs {far:?}"
                );
            }
        }
    }

    #[test]
    fn far_from_an_edge_the_colour_is_the_materials_own() {
        let (a, b) = ([200u8, 20, 20], [20u8, 20, 200]);
        assert_eq!(edge_mix(a, b, 0.4, Side::Near, 1.0, 1.0, 3), a);
        assert_eq!(edge_mix(a, b, 0.4, Side::Far, 1.0, 1.0, 3), a);
    }
}
