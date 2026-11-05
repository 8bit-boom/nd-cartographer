//! Integer-hash value noise: deterministic, seamless in world coordinates.

/// A well-mixed 32-bit hash of a lattice point and a seed.
pub fn hash2(ix: i32, iy: i32, seed: u32) -> u32 {
    let mut h = (ix as u32).wrapping_mul(0x85eb_ca6b)
        ^ (iy as u32).wrapping_mul(0xc2b2_ae35)
        ^ seed.wrapping_mul(0x27d4_eb2f);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 15;
    h
}

/// Uniform in `[0, 1)`.
pub fn rand01(ix: i32, iy: i32, seed: u32) -> f32 {
    (hash2(ix, iy, seed) >> 8) as f32 / 16_777_216.0
}

/// Smooth value noise in `[0, 1)`; lattice spacing 1.
pub fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (fx, fy) = (x.floor(), y.floor());
    let (ix, iy) = (fx as i32, fy as i32);
    let (tx, ty) = (x - fx, y - fy);
    let s = |t: f32| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (s(tx), s(ty));
    let a = rand01(ix, iy, seed);
    let b = rand01(ix + 1, iy, seed);
    let c = rand01(ix, iy + 1, seed);
    let d = rand01(ix + 1, iy + 1, seed);
    let top = a + (b - a) * sx;
    let bot = c + (d - c) * sx;
    top + (bot - top) * sy
}

/// Two-octave fractal noise in `[0, 1)`.
pub fn fbm(x: f32, y: f32, seed: u32) -> f32 {
    (value_noise(x, y, seed) * 0.65
        + value_noise(x * 2.1 + 17.3, y * 2.1 - 5.7, seed ^ 0x51ed) * 0.35)
        .clamp(0.0, 0.999_999)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        for i in -50..50 {
            let (x, y) = (i as f32 * 0.731, i as f32 * -0.419);
            assert_eq!(value_noise(x, y, 3), value_noise(x, y, 3));
            let n = fbm(x, y, 3);
            assert!((0.0..1.0).contains(&n), "{n}");
            assert!((0.0..1.0).contains(&rand01(i, -i, 9)));
        }
    }

    #[test]
    fn the_seed_changes_the_noise() {
        let diff = (0..40)
            .filter(|i| {
                value_noise(*i as f32 * 0.7, 1.3, 1) != value_noise(*i as f32 * 0.7, 1.3, 2)
            })
            .count();
        assert!(diff > 30);
    }

    #[test]
    fn continuous_across_lattice_lines() {
        // Value noise must not jump where the lattice cell changes.
        let e = 1e-3;
        for k in 1..20 {
            let x = k as f32;
            assert!((value_noise(x - e, 0.4, 5) - value_noise(x + e, 0.4, 5)).abs() < 0.02);
        }
    }

    #[test]
    fn roughly_uniform() {
        let mean: f32 = (0..2000).map(|i| rand01(i, i / 7, 1)).sum::<f32>() / 2000.0;
        assert!((mean - 0.5).abs() < 0.03, "{mean}");
    }
}
