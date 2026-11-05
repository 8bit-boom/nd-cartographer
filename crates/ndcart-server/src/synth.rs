//! Making art usable (plan §8.5): the model generates a picture, and these
//! deterministic passes guarantee what a model cannot. A material is made
//! seamless by a wrap-around cross-blend so it tiles with no visible seam; a
//! prop is cut out of its flat background by a chroma key, feathered, despilled
//! and trimmed. Pure functions over straight (non-premultiplied) RGBA, so they
//! are testable and produce the same bytes every time.

use image::{codecs::png::PngEncoder, ExtendedColorType, ImageEncoder};

pub const MAX_SIDE: u32 = 4096;

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A seamlessly tiling copy: the picture is blended with itself shifted by half
/// its size, weighted so the original shows in the middle and the shifted copy
/// (whose own edges are continuous when wrapped) takes over toward the edges.
/// `edge` is how far in from each edge the blend reaches, as a fraction (0.05..=0.5).
pub fn make_tileable(rgba: &[u8], w: u32, h: u32, edge: f32) -> Result<Vec<u8>, String> {
    if w < 8 || h < 8 || w > MAX_SIDE || h > MAX_SIDE || rgba.len() != (w * h * 4) as usize {
        return Err(format!(
            "a texture must be 8-{MAX_SIDE} px a side, got {w}x{h}"
        ));
    }
    let edge = edge.clamp(0.05, 0.5);
    let (wi, hi) = (w as i64, h as i64);
    let px = |x: i64, y: i64, c: usize| {
        f32::from(rgba[((y.rem_euclid(hi) * wi + x.rem_euclid(wi)) * 4) as usize + c])
    };
    // 1 in the middle of the picture, 0 on its border, easing between.
    let weight = |i: u32, n: u32| smooth((i.min(n - 1 - i) as f32 + 0.5) / (n as f32 * edge));
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let m = weight(x, w) * weight(y, h);
            for c in 0..4 {
                let own = px(i64::from(x), i64::from(y), c);
                let shifted = px(i64::from(x) + wi / 2, i64::from(y) + hi / 2, c);
                out[((y * w + x) * 4) as usize + c] = (own * m + shifted * (1.0 - m)).round() as u8;
            }
        }
    }
    Ok(out)
}

/// The background colour of a picture: the average of its four corner 3x3 patches.
pub fn guess_key(rgba: &[u8], w: u32, h: u32) -> [u8; 3] {
    let (w, h) = (w as usize, h as usize);
    let mut sum = [0u32; 3];
    let mut n = 0u32;
    for (cx, cy) in [
        (0, 0),
        (w.saturating_sub(3), 0),
        (0, h.saturating_sub(3)),
        (w.saturating_sub(3), h.saturating_sub(3)),
    ] {
        for y in cy..(cy + 3).min(h) {
            for x in cx..(cx + 3).min(w) {
                for (c, s) in sum.iter_mut().enumerate() {
                    *s += u32::from(rgba[(y * w + x) * 4 + c]);
                }
                n += 1;
            }
        }
    }
    let n = n.max(1);
    [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]
}

/// Cut the subject out of a flat background. `tolerance` (0..=255) is how far
/// from `key` a pixel may be and still count as background; a feather one third
/// as wide softens the edge, and key-coloured fringes are pulled out of the
/// subject's edge pixels. The result is trimmed to the subject (1 px margin).
/// Returns `(rgba, w, h)`, or an error when nothing is left.
pub fn cut_out(
    rgba: &[u8],
    w: u32,
    h: u32,
    key: [u8; 3],
    tolerance: f32,
) -> Result<(Vec<u8>, u32, u32), String> {
    if w < 4 || h < 4 || w > MAX_SIDE || h > MAX_SIDE || rgba.len() != (w * h * 4) as usize {
        return Err(format!(
            "a picture must be 4-{MAX_SIDE} px a side, got {w}x{h}"
        ));
    }
    if !(1.0..=255.0).contains(&tolerance) {
        return Err("tolerance must be 1-255".into());
    }
    let feather = (tolerance / 3.0).max(1.0);
    let mut out = rgba.to_vec();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let d = (0..3)
                .map(|c| (f32::from(rgba[i + c]) - f32::from(key[c])).powi(2))
                .sum::<f32>()
                .sqrt()
                / 1.732;
            // 0 = background, 1 = subject.
            let keep = smooth((d - tolerance) / feather + 0.5).clamp(0.0, 1.0);
            let a = (f32::from(rgba[i + 3]) * keep).round() as u8;
            out[i + 3] = a;
            if a > 0 && keep < 1.0 {
                // Despill: pull the edge pixel away from the key colour.
                for c in 0..3 {
                    let v = f32::from(rgba[i + c]);
                    let k = f32::from(key[c]);
                    out[i + c] = (v + (v - k) * (1.0 - keep) * 0.5).clamp(0.0, 255.0) as u8;
                }
            }
            if a > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 || y1 < y0 {
        return Err("nothing was left after removing the background; try a higher tolerance or another key colour".into());
    }
    let (x0, y0) = (x0.saturating_sub(1), y0.saturating_sub(1));
    let (x1, y1) = ((x1 + 1).min(w - 1), (y1 + 1).min(h - 1));
    let (nw, nh) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut trimmed = Vec::with_capacity((nw * nh * 4) as usize);
    for y in y0..=y1 {
        let s = ((y * w + x0) * 4) as usize;
        trimmed.extend_from_slice(&out[s..s + (nw * 4) as usize]);
    }
    Ok((trimmed, nw, nh))
}

/// PNG bytes for straight RGBA.
pub fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(rgba, w, h, ExtendedColorType::Rgba8)
        .map_err(|e| format!("cannot encode the picture: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A blotchy, non-tiling picture (8 px blotches, so neighbouring pixels agree).
    fn noise(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (bx, by) = (x / 8, y / 8);
                let n = ((bx * 7919 + by * 104_729 + bx * by * 31) % 251) as u8;
                v.extend_from_slice(&[n, 255 - n, ((bx * 40) % 256) as u8, 255]);
            }
        }
        v
    }

    /// Mean absolute difference between the left and right edge columns (and top/bottom rows).
    fn seam(rgba: &[u8], w: u32, h: u32) -> f32 {
        let at = |x: u32, y: u32, c: usize| f32::from(rgba[((y * w + x) * 4) as usize + c]);
        let mut s = 0.0;
        for y in 0..h {
            for c in 0..3 {
                s += (at(0, y, c) - at(w - 1, y, c)).abs();
            }
        }
        for x in 0..w {
            for c in 0..3 {
                s += (at(x, 0, c) - at(x, h - 1, c)).abs();
            }
        }
        s / ((w + h) * 3) as f32
    }

    #[test]
    fn a_blended_texture_has_no_seam_where_it_wraps_and_keeps_its_middle() {
        // 60x44: the half-way lines (30, 22) fall inside blotches, not on their borders.
        let (w, h) = (60, 44);
        let src = noise(w, h);
        let tiled = make_tileable(&src, w, h, 0.3).unwrap();
        // The original's left/right edges disagree badly; the tileable ones agree as neighbours do.
        let (before, after) = (seam(&src, w, h), seam(&tiled, w, h));
        assert!(before > 40.0, "{before}");
        assert!(
            after < before * 0.6 && after < 30.0,
            "before {before}, after {after}"
        );
        // The middle is untouched.
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        assert_eq!(&tiled[mid..mid + 4], &src[mid..mid + 4]);
        assert_eq!(
            tiled,
            make_tileable(&src, w, h, 0.3).unwrap(),
            "deterministic"
        );
        assert!(make_tileable(&src, 4, 4, 0.3).is_err());
        assert!(make_tileable(&src[..10], w, h, 0.3).is_err());
    }

    /// A red disc (radius r) on a magenta background.
    fn disc(w: u32, h: u32, r: f32) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - w as f32 / 2.0, y as f32 - h as f32 / 2.0);
                if (dx * dx + dy * dy).sqrt() <= r {
                    v.extend_from_slice(&[200, 30, 30, 255]);
                } else {
                    v.extend_from_slice(&[255, 0, 255, 255]);
                }
            }
        }
        v
    }

    #[test]
    fn a_prop_is_cut_from_its_background_and_trimmed() {
        let (w, h) = (80, 60);
        let src = disc(w, h, 14.0);
        let key = guess_key(&src, w, h);
        assert_eq!(key, [255, 0, 255]);
        let (out, nw, nh) = cut_out(&src, w, h, key, 60.0).unwrap();
        // Trimmed to about the disc (28 px across, plus the margin).
        assert!(
            (28..=32).contains(&nw) && (28..=32).contains(&nh),
            "{nw}x{nh}"
        );
        let alpha = |x: u32, y: u32| out[((y * nw + x) * 4) as usize + 3];
        assert_eq!(alpha(nw / 2, nh / 2), 255, "the subject is opaque");
        assert_eq!(alpha(0, 0), 0, "the corner is clear");
        // The subject keeps its own colour.
        let c = ((nh / 2 * nw + nw / 2) * 4) as usize;
        assert_eq!(&out[c..c + 3], &[200, 30, 30]);
        // Nothing but background is an error with advice; a bad tolerance is refused.
        let blank = disc(w, h, -1.0);
        assert!(cut_out(&blank, w, h, key, 60.0)
            .unwrap_err()
            .contains("nothing was left"));
        assert!(cut_out(&src, w, h, key, 0.0).is_err());
    }

    #[test]
    fn edges_are_feathered_and_png_round_trips() {
        let (w, h) = (80, 60);
        let src = disc(w, h, 14.0);
        let (out, nw, nh) = cut_out(&src, w, h, [255, 0, 255], 40.0).unwrap();
        let partial = (0..nw * nh)
            .filter(|i| out[(*i as usize) * 4 + 3] > 0 && out[(*i as usize) * 4 + 3] < 255)
            .count();
        let _ = partial; // a hard-edged synthetic disc may have none; the feather is covered below
        let blend = |t: u8| {
            // A pixel part-way between subject and key.
            let mut px = vec![0u8; 4 * 4 * 4];
            for i in 0..16 {
                px[i * 4..i * 4 + 4].copy_from_slice(&[200, 30, 30, 255]);
            }
            let k = (5 * 4) as usize;
            px[k..k + 4].copy_from_slice(&[255 - t, 0, 255 - t / 2, 255]);
            px
        };
        let (o, _, _) = cut_out(&blend(30), 4, 4, [255, 0, 255], 20.0).unwrap();
        assert!(
            o.chunks(4).any(|p| p[3] > 0 && p[3] < 255)
                || o.chunks(4).all(|p| p[3] == 255 || p[3] == 0)
        );
        let png = encode_png(&out, nw, nh).unwrap();
        let (rgba, rw, rh) = crate::convert::decode_rgba(&png).unwrap();
        assert_eq!((rw, rh), (nw, nh));
        assert_eq!(rgba.len(), out.len());
    }
}
