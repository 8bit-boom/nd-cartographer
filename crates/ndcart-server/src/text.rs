//! Map typography (plan §7.2): bundled OFL fonts (Crimson Pro, see
//! `assets/fonts/CrimsonPro-OFL.txt`), glyph layout with kerning and tracking,
//! text set straight or **along a path** (a river's name follows the river), and
//! a halo so letters read over any ground. Layout is pure geometry; drawing
//! turns glyph outlines into vector paths. Everything is deterministic: the
//! same text, font size and path give the same pixels on every platform.

use ab_glyph::{Font, FontRef, GlyphId, OutlineCurve};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const REGULAR: &[u8] = include_bytes!("../assets/fonts/CrimsonPro-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../assets/fonts/CrimsonPro-Bold.ttf");

pub struct Fonts {
    regular: FontRef<'static>,
    bold: FontRef<'static>,
}

pub fn fonts() -> &'static Fonts {
    static F: OnceLock<Fonts> = OnceLock::new();
    F.get_or_init(|| Fonts {
        regular: FontRef::try_from_slice(REGULAR).expect("bundled regular font parses"),
        bold: FontRef::try_from_slice(BOLD).expect("bundled bold font parses"),
    })
}

impl Fonts {
    fn pick(&self, bold: bool) -> &FontRef<'static> {
        if bold {
            &self.bold
        } else {
            &self.regular
        }
    }
}

/// Glyphs laid out on a baseline, in pixels from the start of the line.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// (glyph, x offset of its origin, advance), in pixels.
    pub glyphs: Vec<(GlyphId, f32, f32)>,
    pub width: f32,
    pub bold: bool,
    pub size_px: f32,
}

/// Lay `text` out at `size_px` (the font's em height) with `tracking` em of extra
/// letter spacing. Kerning is applied; a character the font lacks keeps its
/// place as a space-width gap rather than drawing the missing-glyph box.
pub fn layout(text: &str, bold: bool, size_px: f32, tracking: f32) -> Layout {
    let font = fonts().pick(bold);
    let upm = font.units_per_em().unwrap_or(1000.0);
    let k = size_px / upm;
    let space = font.h_advance_unscaled(font.glyph_id(' ')) * k;
    let mut glyphs = Vec::new();
    let (mut x, mut prev): (f32, Option<GlyphId>) = (0.0, None);
    for c in text.chars().filter(|c| !c.is_control()) {
        let id = font.glyph_id(c);
        let missing = id.0 == 0 && c != ' ';
        let adv = if missing {
            space
        } else {
            font.h_advance_unscaled(id) * k
        };
        if let Some(p) = prev {
            x += font.kern_unscaled(p, id) * k;
        }
        if !missing && c != ' ' {
            glyphs.push((id, x, adv));
        }
        x += adv + tracking * size_px;
        prev = if missing { None } else { Some(id) };
    }
    // The trailing tracking is not part of the visible width.
    let width = (x - if text.is_empty() {
        0.0
    } else {
        tracking * size_px
    })
    .max(0.0);
    Layout {
        glyphs,
        width,
        bold,
        size_px,
    }
}

/// One glyph placed in pixel space: its baseline origin and its angle.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub id: GlyphId,
    pub x: f32,
    pub y: f32,
    /// Radians, clockwise on screen.
    pub angle: f32,
    /// 1.0 unless the text was shrunk to fit a short path.
    pub scale: f32,
}

/// Centre the line on `(cx, cy)` (vertically on the x-height) and turn it by `rotation`.
pub fn place_straight(
    l: &Layout,
    cx: f32,
    cy: f32,
    rotation: f32,
    left_aligned: bool,
) -> Vec<Placed> {
    let (sn, cs) = rotation.sin_cos();
    let start = if left_aligned { 0.0 } else { -l.width / 2.0 };
    let drop = 0.3 * l.size_px; // baseline sits below the centre so the x-height is centred
    l.glyphs
        .iter()
        .map(|&(id, gx, _)| {
            let along = start + gx;
            Placed {
                id,
                x: cx + along * cs - drop * sn,
                y: cy + along * sn + drop * cs,
                angle: rotation,
                scale: 1.0,
            }
        })
        .collect()
}

/// Position of a path at arc length `s`, with its unit tangent.
fn at_arc(path: &[(f32, f32)], cum: &[f32], s: f32) -> ((f32, f32), (f32, f32)) {
    let s = s.clamp(0.0, *cum.last().unwrap_or(&0.0));
    let i = cum.partition_point(|&c| c < s).clamp(1, path.len() - 1);
    let (a, b) = (path[i - 1], path[i]);
    let seg = (cum[i] - cum[i - 1]).max(1e-6);
    let t = (s - cum[i - 1]) / seg;
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    ((a.0 + dx * t, a.1 + dy * t), (dx / len, dy / len))
}

/// Set the line along `path` (pixels), centred on its middle. The text is never
/// upside down (a path running right-to-left is read the other way), and if the
/// line is longer than the path it is shrunk to fit — returning `None` when it
/// would have to shrink below `min_scale`.
pub fn place_along(l: &Layout, path: &[(f32, f32)], min_scale: f32) -> Option<Vec<Placed>> {
    if path.len() < 2 || l.glyphs.is_empty() {
        return None;
    }
    let mut pts: Vec<(f32, f32)> = path.to_vec();
    // Read left to right: if the path ends left of where it starts, reverse it.
    if pts[pts.len() - 1].0 < pts[0].0 {
        pts.reverse();
    }
    let mut cum = vec![0.0f32];
    for w in pts.windows(2) {
        cum.push(
            cum.last().copied().unwrap_or(0.0)
                + ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt(),
        );
    }
    let total = *cum.last()?;
    if total < 1e-3 {
        return None;
    }
    // 8% breathing room at each end.
    let usable = total * 0.84;
    let scale = if l.width > usable {
        usable / l.width
    } else {
        1.0
    };
    if scale < min_scale {
        return None;
    }
    let start = (total - l.width * scale) / 2.0;
    let drop = 0.3 * l.size_px * scale;
    Some(
        l.glyphs
            .iter()
            .map(|&(id, gx, adv)| {
                let mid = start + (gx + adv / 2.0) * scale;
                let (p, t) = at_arc(&pts, &cum, mid);
                // The glyph's origin is half an advance back along the tangent, and the
                // baseline sits `drop` to the glyph's "down" side (screen-clockwise normal).
                let half = adv * scale / 2.0;
                let (nx, ny) = (-t.1, t.0);
                Placed {
                    id,
                    x: p.0 - t.0 * half + nx * drop,
                    y: p.1 - t.1 * half + ny * drop,
                    angle: t.1.atan2(t.0),
                    scale,
                }
            })
            .collect(),
    )
}

/// Bounding box `[x0, y0, x1, y1]` of placed glyphs, padded by `pad` pixels.
pub fn bounds(placed: &[Placed], size_px: f32, pad: f32) -> Option<[f32; 4]> {
    let first = placed.first()?;
    let mut b = [first.x, first.y, first.x, first.y];
    for p in placed {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    let r = size_px * 1.15 + pad;
    Some([b[0] - r, b[1] - r, b[2] + r, b[3] + r])
}

// ---- rasterizing ----

type PathCache = Mutex<HashMap<(bool, u16), Option<tiny_skia::Path>>>;

/// A glyph's outline as a vector path in em units (1.0 = the font's em), y down.
fn glyph_path(bold: bool, id: GlyphId) -> Option<tiny_skia::Path> {
    static CACHE: OnceLock<PathCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(p) = cache.lock().expect("glyph cache").get(&(bold, id.0)) {
        return p.clone();
    }
    let font = fonts().pick(bold);
    let k = 1.0 / font.units_per_em().unwrap_or(1000.0);
    let built = font.outline(id).and_then(|o| {
        let mut pb = tiny_skia::PathBuilder::new();
        let mut last: Option<(f32, f32)> = None;
        let pt = |p: ab_glyph::Point| (p.x * k, -p.y * k);
        for c in &o.curves {
            let start = match c {
                OutlineCurve::Line(a, _)
                | OutlineCurve::Quad(a, _, _)
                | OutlineCurve::Cubic(a, _, _, _) => pt(*a),
            };
            // A new contour begins where a curve does not start at the previous end.
            if last.is_none_or(|l| (l.0 - start.0).abs() > 1e-5 || (l.1 - start.1).abs() > 1e-5) {
                if last.is_some() {
                    pb.close();
                }
                pb.move_to(start.0, start.1);
            }
            last = Some(match c {
                OutlineCurve::Line(_, b) => {
                    let b = pt(*b);
                    pb.line_to(b.0, b.1);
                    b
                }
                OutlineCurve::Quad(_, b, e) => {
                    let (b, e) = (pt(*b), pt(*e));
                    pb.quad_to(b.0, b.1, e.0, e.1);
                    e
                }
                OutlineCurve::Cubic(_, b, c2, e) => {
                    let (b, c2, e) = (pt(*b), pt(*c2), pt(*e));
                    pb.cubic_to(b.0, b.1, c2.0, c2.1, e.0, e.1);
                    e
                }
            });
        }
        if last.is_some() {
            pb.close();
        }
        pb.finish()
    });
    cache
        .lock()
        .expect("glyph cache")
        .insert((bold, id.0), built.clone());
    built
}

/// Draw placed glyphs into `pixmap` (pixel coordinates), halo first.
pub fn draw(
    pixmap: &mut tiny_skia::Pixmap,
    placed: &[Placed],
    bold: bool,
    size_px: f32,
    color: [u8; 3],
    halo: bool,
) {
    use tiny_skia::{FillRule, LineJoin, Paint, Stroke, Transform};
    let xf = |p: &Placed| {
        let s = size_px * p.scale;
        let (sn, cs) = p.angle.sin_cos();
        Transform::from_row(cs * s, sn * s, -sn * s, cs * s, p.x, p.y)
    };
    if halo {
        // Light text gets a dark halo and the reverse.
        let lum =
            0.299 * f32::from(color[0]) + 0.587 * f32::from(color[1]) + 0.114 * f32::from(color[2]);
        let hc = if lum > 140.0 {
            [16u8, 20, 28]
        } else {
            [255u8, 252, 240]
        };
        let mut paint = Paint::default();
        paint.set_color_rgba8(hc[0], hc[1], hc[2], 215);
        paint.anti_alias = true;
        // Width is in em units because the glyph path is (the transform scales it).
        let stroke = Stroke {
            width: 0.2,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        for p in placed {
            if let Some(path) = glyph_path(bold, p.id) {
                pixmap.stroke_path(&path, &paint, &stroke, xf(p), None);
            }
        }
    }
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], 255);
    paint.anti_alias = true;
    for p in placed {
        if let Some(path) = glyph_path(bold, p.id) {
            pixmap.fill_path(&path, &paint, FillRule::Winding, xf(p), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_measures_text_and_scales_with_size_and_tracking() {
        let a = layout("Neon Harbour", false, 20.0, 0.0);
        let b = layout("Neon Harbour", false, 40.0, 0.0);
        assert!(a.width > 60.0 && a.width < 200.0, "{}", a.width);
        assert!(
            (b.width / a.width - 2.0).abs() < 0.02,
            "width is linear in size: {} {}",
            a.width,
            b.width
        );
        // Spaces make gaps but no glyphs; there are 11 visible letters.
        assert_eq!(a.glyphs.len(), 11);
        let spaced = layout("Neon Harbour", false, 20.0, 0.3);
        assert!(
            (spaced.width - a.width - 11.0 * 0.3 * 20.0).abs() < 0.5,
            "tracking adds 11 gaps: {} vs {}",
            spaced.width,
            a.width
        );
        // Bold is wider than regular.
        assert!(
            layout("Harbour", true, 20.0, 0.0).width > layout("Harbour", false, 20.0, 0.0).width
        );
        // Glyph offsets increase.
        assert!(a.glyphs.windows(2).all(|w| w[1].1 > w[0].1));
        // Empty and control-only text are harmless.
        assert!(layout("", false, 20.0, 0.0).glyphs.is_empty());
        assert_eq!(layout("\n\t", false, 20.0, 0.0).glyphs.len(), 0);
    }

    #[test]
    fn accented_and_unknown_characters_do_not_panic_or_draw_boxes() {
        let acc = layout("Ærø Ångström Zürich", false, 20.0, 0.0);
        assert!(acc.glyphs.len() >= 15, "Latin-extended letters have glyphs");
        let odd = layout("a\u{1F409}b", false, 20.0, 0.0); // a dragon emoji the font lacks
        assert_eq!(
            odd.glyphs.len(),
            2,
            "the missing glyph leaves a gap, not a box"
        );
        assert!(
            odd.glyphs[1].1 > odd.glyphs[0].1 + odd.glyphs[0].2,
            "the gap is wider than 'a' alone"
        );
    }

    #[test]
    fn straight_text_is_centred_and_rotates_about_its_centre() {
        let l = layout("Kingdom", true, 30.0, 0.1);
        let p = place_straight(&l, 100.0, 50.0, 0.0, false);
        let (first, last) = (p.first().unwrap(), p.last().unwrap());
        let mid = (first.x + last.x + l.glyphs.last().unwrap().2) / 2.0;
        assert!((mid - 100.0).abs() < 2.0, "centred on x: {mid}");
        assert!(
            p.iter().all(|g| (g.y - (50.0 + 9.0)).abs() < 1e-3),
            "one baseline, 0.3 em below the centre"
        );
        // A quarter turn: the line runs down the page instead.
        let q = place_straight(&l, 100.0, 50.0, std::f32::consts::FRAC_PI_2, false);
        assert!(q.last().unwrap().y > q.first().unwrap().y + 50.0);
        assert!((q.first().unwrap().x - q.last().unwrap().x).abs() < 1.0);
        // Left-aligned text starts at the anchor.
        let left = place_straight(&l, 100.0, 50.0, 0.0, true);
        assert!((left[0].x - 100.0).abs() < 1e-3);
    }

    #[test]
    fn text_along_a_path_follows_it_and_never_reads_upside_down() {
        let l = layout("River", false, 14.0, 0.0);
        // A straight diagonal run, left to right and down-right.
        let down: Vec<(f32, f32)> = vec![(0.0, 0.0), (200.0, 200.0)];
        let p = place_along(&l, &down, 0.5).unwrap();
        for g in &p {
            assert!(
                (g.angle - std::f32::consts::FRAC_PI_4).abs() < 1e-3,
                "tilted 45 degrees: {}",
                g.angle
            );
        }
        assert!(
            p.windows(2).all(|w| w[1].x > w[0].x && w[1].y > w[0].y),
            "advancing along the path"
        );
        // The same path drawn the other way gives the same reading direction (not upside down).
        let up: Vec<(f32, f32)> = down.iter().rev().cloned().collect();
        let q = place_along(&l, &up, 0.5).unwrap();
        assert!(
            q[0].angle.abs() < std::f32::consts::FRAC_PI_2,
            "never past vertical"
        );
        assert!(q.windows(2).all(|w| w[1].x > w[0].x));
        // The text is centred on the path's middle.
        let mid_x = (p.first().unwrap().x + p.last().unwrap().x) / 2.0;
        assert!((mid_x - 100.0).abs() < 25.0, "{mid_x}");
        // Around a bend the glyph angles change smoothly.
        let bend: Vec<(f32, f32)> = vec![(0.0, 100.0), (100.0, 100.0), (180.0, 20.0)];
        let l2 = layout("Winding Water", false, 14.0, 0.0);
        let b = place_along(&l2, &bend, 0.5).unwrap();
        assert!(
            b.first().unwrap().angle.abs() < 0.2 && b.last().unwrap().angle < -0.5,
            "turns with the path"
        );
    }

    #[test]
    fn long_text_shrinks_to_fit_a_short_path_or_is_declined() {
        let l = layout("A very long river name indeed", false, 20.0, 0.0);
        let short: Vec<(f32, f32)> = vec![(0.0, 0.0), (150.0, 0.0)];
        let p = place_along(&l, &short, 0.3).unwrap();
        assert!(p[0].scale < 0.6 && p[0].scale > 0.3, "{}", p[0].scale);
        let (a, z) = (p.first().unwrap(), p.last().unwrap());
        assert!(
            a.x >= -1.0 && z.x <= 151.0,
            "it stays on the path: {}..{}",
            a.x,
            z.x
        );
        assert!(
            place_along(&l, &short, 0.9).is_none(),
            "too small to be worth drawing"
        );
        assert!(place_along(&l, &[(0.0, 0.0)], 0.1).is_none());
        assert!(
            place_along(&l, &[(5.0, 5.0), (5.0, 5.0)], 0.1).is_none(),
            "a zero-length path"
        );
        assert!(place_along(&layout("", false, 20.0, 0.0), &short, 0.1).is_none());
    }

    #[test]
    fn glyphs_rasterize_with_a_halo_and_the_right_colours() {
        let l = layout("Mm", true, 60.0, 0.0);
        let placed = place_straight(&l, 60.0, 50.0, 0.0, false);
        let mut with = tiny_skia::Pixmap::new(140, 100).unwrap();
        with.fill(tiny_skia::Color::from_rgba8(120, 160, 90, 255));
        let mut without = with.clone();
        draw(&mut with, &placed, true, 60.0, [20, 20, 30], true);
        draw(&mut without, &placed, true, 60.0, [20, 20, 30], false);
        let dark = |pm: &tiny_skia::Pixmap| {
            pm.pixels()
                .iter()
                .filter(|p| p.red() < 40 && p.green() < 40)
                .count()
        };
        assert!(
            dark(&without) > 300,
            "the letters are drawn: {}",
            dark(&without)
        );
        // The halo is a light ring (dark text gets a light halo) around the letters.
        let light = |pm: &tiny_skia::Pixmap| {
            pm.pixels()
                .iter()
                .filter(|p| p.red() > 220 && p.green() > 220)
                .count()
        };
        assert!(
            light(&with) > light(&without) + 200,
            "halo pixels: {} vs {}",
            light(&with),
            light(&without)
        );
        // Edge pixels blend with the halo instead of the ground, so a sliver fewer count
        // as pure ink; the letters' bodies are untouched.
        let (a, b) = (dark(&with) as f32, dark(&without) as f32);
        assert!(
            a > b * 0.95 && a <= b,
            "the halo never covers the letters themselves: {a} vs {b}"
        );
        // Light text gets a dark halo instead.
        let mut inv = tiny_skia::Pixmap::new(140, 100).unwrap();
        inv.fill(tiny_skia::Color::from_rgba8(120, 160, 90, 255));
        draw(&mut inv, &placed, true, 60.0, [250, 250, 245], true);
        assert!(
            inv.pixels()
                .iter()
                .filter(|p| p.red() < 45 && p.green() < 55)
                .count()
                > 200
        );
    }

    #[test]
    fn the_whole_alphabet_has_outlines() {
        // Every printable ASCII letter must produce a vector outline (no silent blanks).
        for c in ('A'..='Z').chain('a'..='z').chain('0'..='9') {
            for bold in [false, true] {
                let id = fonts().pick(bold).glyph_id(c);
                assert!(
                    id.0 != 0 && glyph_path(bold, id).is_some(),
                    "{c} bold={bold}"
                );
            }
        }
        assert!(
            glyph_path(false, fonts().regular.glyph_id(' ')).is_none(),
            "space has no outline"
        );
    }
}
