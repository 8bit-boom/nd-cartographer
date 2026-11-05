//! Tile baking for M0: the floor's room grid rendered into 256px PNG tiles
//! on a zoom pyramid (z0 = 16 px/cell, doubling per level; the plan's
//! tiny-skia + rayon baker grows into `ndcart-render`; this module proves
//! the pipeline end to end and serves the browser's pan/zoom).
//!
//! Geometry only: occupied cells fill with their space color, derived walls
//! stroke on top, the background is the void color. Golden-tile tests arrive
//! with `ndcart-render` (plan §12); this module stays deterministic anyway.

use ndcart_core::walls::{floor_walls, Wall};
use ndcart_core::CellKey;
use ndcart_core::FloorDoc;
use ndcart_paint::{edge_mix, Side, BLEND_WIDTH};

/// Pixels per cell at zoom 0. Zoom z doubles it (z capped at 4).
pub const PPC0: i64 = 16;
pub const TILE: u32 = 256;
pub const MAX_Z: u32 = 4;

const VOID: [u8; 4] = [0x14, 0x16, 0x1a, 0xff];
const WALL_RGB: [u8; 3] = [0xd8, 0xe2, 0xec];
const DOOR_RGB: [u8; 3] = [0xe8, 0xa8, 0x30];
const WINDOW_RGB: [u8; 3] = [0x4f, 0xc3, 0xe8];
const SECRET_RGB: [u8; 3] = [0xb0, 0x6a, 0xd8];

/// Doors, windows and secret doors read at a glance: each kind its own colour
/// and a heavier stroke than a plain wall.
fn wall_style(kind: ndcart_core::WallKind) -> ([u8; 3], f32) {
    use ndcart_core::WallKind::*;
    match kind {
        Wall => (WALL_RGB, 1.0),
        Door => (DOOR_RGB, 2.0),
        Window => (WINDOW_RGB, 1.5),
        Secret => (SECRET_RGB, 2.0),
    }
}

fn parse_hex(color: &str) -> [u8; 3] {
    let h = color.trim_start_matches('#');
    if h.len() != 7 && h.len() != 6 {
        return [0x33, 0x44, 0x55];
    }
    let digits = if h.len() == 7 { &h[1..] } else { h };
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return [0x33, 0x44, 0x55];
    }
    let r = u8::from_str_radix(&digits[0..2], 16).unwrap_or(0x33);
    let g = u8::from_str_radix(&digits[2..4], 16).unwrap_or(0x44);
    let b = u8::from_str_radix(&digits[4..6], 16).unwrap_or(0x55);
    [r, g, b]
}

/// Bake one tile. `z`, `tx`, `ty` are the pyramid coordinates; the tile at
/// (z, tx, ty) covers world pixels `[tx*256, tx*256+256) x [ty*256, ...)`
/// in the z-scaled image. PNG bytes.
pub fn bake_tile(
    floor: &FloorDoc,
    walls: &[Wall],
    cell_size: f64,
    z: u32,
    tx: i64,
    ty: i64,
) -> Vec<u8> {
    let z = z.min(MAX_Z);
    let ppc = PPC0 << z; // pixels per cell at this zoom (fits i64: 16<<4=256)
    let t = i64::from(TILE);
    render_region(floor, walls, cell_size, ppc, tx * t, ty * t, TILE, TILE)
}

/// The whole floor as one image at `ppc` pixels per cell (cols x rows is the
/// painted bounding box, at least 1x1), in the requested format. Refuses
/// images over the size limits. Returns `(bytes, width, height)`.
pub fn bake_full_as(
    floor: &FloorDoc,
    walls: &[Wall],
    cell_size: f64,
    ppc: i64,
    format: ImageFormat,
    quality: u8,
) -> Result<(Vec<u8>, u32, u32), String> {
    let (cols, rows) = grid_extent(floor);
    let (w, h) = (i64::from(cols) * ppc, i64::from(rows) * ppc);
    if !(1..=256).contains(&ppc) || w > MAX_SIDE_PX || h > MAX_SIDE_PX || w * h > MAX_FULL_PX {
        return Err(format!(
            "image {w}x{h}px is outside the limits (side <= {MAX_SIDE_PX}, area <= {MAX_FULL_PX})"
        ));
    }
    let pm = render_pixmap(floor, walls, cell_size, ppc, 0, 0, w as u32, h as u32);
    let bytes = encode(&pm, format, quality)?;
    Ok((bytes, w as u32, h as u32))
}

/// PNG convenience for tests.
#[cfg(test)]
pub fn bake_full(
    floor: &FloorDoc,
    walls: &[Wall],
    cell_size: f64,
    ppc: i64,
) -> Result<(Vec<u8>, u32, u32), String> {
    bake_full_as(floor, walls, cell_size, ppc, ImageFormat::Png, 90)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageFormat {
    Png,
    Webp,
    Avif,
}

impl ImageFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "webp" => Some(Self::Webp),
            "avif" => Some(Self::Avif),
            _ => None,
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Avif => "avif",
        }
    }
}

/// Encode a baked pixmap. The map is fully opaque (the void is painted), so
/// premultiplied == straight alpha and the bytes can go to the encoders as-is.
/// WebP is lossless (the pure-Rust encoder has no lossy mode; flat-colour maps
/// compress well); AVIF is lossy at `quality` 1..=100.
fn encode(pm: &tiny_skia::Pixmap, format: ImageFormat, quality: u8) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let (w, h) = (pm.width(), pm.height());
    let mut out = Vec::new();
    match format {
        ImageFormat::Png => return pm.encode_png().map_err(|e| e.to_string()),
        ImageFormat::Webp => image::codecs::webp::WebPEncoder::new_lossless(&mut out).write_image(
            pm.data(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        ),
        ImageFormat::Avif => image::codecs::avif::AvifEncoder::new_with_speed_quality(
            &mut out,
            8,
            quality.clamp(1, 100),
        )
        .write_image(pm.data(), w, h, image::ExtendedColorType::Rgba8),
    }
    .map_err(|e| format!("{} encode failed: {e}", format.ext()))?;
    Ok(out)
}

pub const MAX_SIDE_PX: i64 = 20_000; // nd-world's schematic canvas limit
pub const MAX_FULL_PX: i64 = 40_000_000; // nd-world's import-uvtt pixel budget

/// Everything on the floor in cells (rooms, underlay, strokes, stamps), min 1x1.
pub fn grid_extent(floor: &FloorDoc) -> (u32, u32) {
    floor.extent()
}

/// Seed for every pattern and blend jitter. A document-level seed can replace
/// this later; fixed for now so renders are reproducible.
const PAINT_SEED: u32 = 0x6e64;
const FALLBACK: Surface = Surface::Flat([0x33, 0x44, 0x55]);

/// What a space paints: a pack material, or a flat colour.
#[derive(Clone, Copy)]
enum Surface {
    Mat(&'static ndcart_paint::Material),
    Flat([u8; 3]),
}

impl Surface {
    fn sample(&self, u: f32, v: f32) -> [u8; 3] {
        match self {
            Surface::Mat(m) => m.sample(u, v, PAINT_SEED),
            Surface::Flat(c) => *c,
        }
    }
    fn same_as(&self, o: &Surface) -> bool {
        match (self, o) {
            (Surface::Mat(a), Surface::Mat(b)) => a.id == b.id,
            (Surface::Flat(a), Surface::Flat(b)) => a == b,
            _ => false,
        }
    }
}

/// Resolve every space once per bake. An unknown material id falls back to the
/// space's flat colour.
fn surfaces_of(floor: &FloorDoc) -> std::collections::HashMap<&str, Surface> {
    floor
        .grid
        .spaces
        .iter()
        .map(|(id, s)| {
            let surface = s
                .material
                .as_deref()
                .and_then(ndcart_paint::find)
                .map(Surface::Mat)
                .unwrap_or_else(|| Surface::Flat(parse_hex(&s.color)));
            (id.as_str(), surface)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_region(
    floor: &FloorDoc,
    walls: &[Wall],
    cell_size: f64,
    ppc: i64,
    x0: i64,
    y0: i64,
    w: u32,
    h: u32,
) -> Vec<u8> {
    render_pixmap(floor, walls, cell_size, ppc, x0, y0, w, h)
        .encode_png()
        .expect("png encode")
}

/// Rasterize one stamp definition into `pixmap`, centred at `(cx, cy)` pixels,
/// `w` x `h` pixels, rotated `rot` radians, optionally mirrored and tinted.
/// With `shadow`, a soft drop shadow (three low-alpha layers, each a little
/// bigger and further out) goes down first, lit from the top-left.
#[allow(clippy::too_many_arguments)]
fn draw_stamp(
    pixmap: &mut tiny_skia::Pixmap,
    def: &ndcart_paint::StampDef,
    (cx, cy): (f32, f32),
    (w, h): (f32, f32),
    rot: f32,
    flip: bool,
    tint: Option<[u8; 3]>,
    shadow_px: Option<f32>,
) {
    use ndcart_paint::Prim;
    use tiny_skia::{FillRule, Paint, PathBuilder, Stroke, Transform};
    let (sn, cs) = rot.sin_cos();
    let sx = if flip { -w } else { w };
    let xf =
        |dx: f32, dy: f32| Transform::from_row(cs * sx, sn * sx, -sn * h, cs * h, cx + dx, cy + dy);
    let colour = |rgb: [u8; 3]| match tint {
        Some(t) => [
            (u16::from(rgb[0]) * u16::from(t[0]) / 255) as u8,
            (u16::from(rgb[1]) * u16::from(t[1]) / 255) as u8,
            (u16::from(rgb[2]) * u16::from(t[2]) / 255) as u8,
        ],
        None => rgb,
    };
    let path_of = |p: &Prim| -> Option<(tiny_skia::Path, [u8; 3], Option<f32>)> {
        let mut pb = PathBuilder::new();
        match *p {
            Prim::Ellipse {
                cx,
                cy,
                rx,
                ry,
                rgb,
            } => {
                let r = tiny_skia::Rect::from_xywh(cx - rx, cy - ry, rx * 2.0, ry * 2.0)?;
                pb.push_oval(r);
                Some((pb.finish()?, rgb, None))
            }
            Prim::Rect { x, y, w, h, rgb } => {
                pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
                Some((pb.finish()?, rgb, None))
            }
            Prim::Poly { pts, rgb } => {
                pb.move_to(pts[0][0], pts[0][1]);
                for q in &pts[1..] {
                    pb.line_to(q[0], q[1]);
                }
                pb.close();
                Some((pb.finish()?, rgb, None))
            }
            Prim::Line { a, b, w, rgb } => {
                pb.move_to(a[0], a[1]);
                pb.line_to(b[0], b[1]);
                Some((pb.finish()?, rgb, Some(w)))
            }
        }
    };
    let prims: Vec<_> = def.prims.iter().filter_map(path_of).collect();
    if let Some(off) = shadow_px.filter(|o| *o > 0.0) {
        for (k, (grow, a)) in [(0.0f32, 0.09f32), (0.04, 0.07), (0.08, 0.05)]
            .into_iter()
            .enumerate()
        {
            let mut paint = Paint::default();
            paint.set_color_rgba8(0, 0, 0, (a * 255.0) as u8);
            paint.anti_alias = true;
            let t = xf(off * (0.55 + 0.25 * k as f32), off * (0.8 + 0.3 * k as f32));
            for (path, _, line) in &prims {
                if line.is_some() {
                    continue; // lines are detail; the silhouette is the filled shapes
                }
                pixmap.fill_path(path, &paint, FillRule::Winding, t, None);
                if grow > 0.0 {
                    let stroke = Stroke {
                        width: grow,
                        ..Default::default()
                    };
                    pixmap.stroke_path(path, &paint, &stroke, t, None);
                }
            }
        }
    }
    let t = xf(0.0, 0.0);
    for (path, rgb, line) in &prims {
        let c = colour(*rgb);
        let mut paint = Paint::default();
        paint.set_color_rgba8(c[0], c[1], c[2], 255);
        paint.anti_alias = true;
        match line {
            None => pixmap.fill_path(path, &paint, FillRule::Winding, t, None),
            Some(width) => {
                let stroke = Stroke {
                    width: *width,
                    line_cap: tiny_skia::LineCap::Round,
                    ..Default::default()
                };
                pixmap.stroke_path(path, &paint, &stroke, t, None);
            }
        }
    }
}

/// Largest sprite (pixels) rasterized for one stamp; bigger ones (a huge stamp
/// at high zoom) are drawn straight onto the region instead.
const MAX_SPRITE_PIXELS: i64 = 16_000_000;

/// Composite the floor's stamps over strokes, in z-order. A stamp whose pack
/// asset is unknown is simply not drawn.
///
/// Each stamp is rasterized once into its own sprite, in coordinates that do
/// not depend on which tile is being baked, then blitted at an integer pixel
/// offset. That is what makes a tile pixel-identical to the same crop of the
/// full render (a path's anti-aliasing would otherwise depend on its offset).
fn apply_stamps(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    if floor.stamps.is_empty() {
        return;
    }
    let scale = ppc as f32;
    let (w, h) = (i64::from(pixmap.width()), i64::from(pixmap.height()));
    for st in &floor.stamps {
        let Some(def) = ndcart_paint::find_stamp(&st.asset) else {
            continue;
        };
        let (pw, ph) = (
            def.size[0] * st.scale * scale,
            def.size[1] * st.scale * scale,
        );
        let throw = if st.shadow {
            def.height * st.scale * scale * 0.12 * 1.6
        } else {
            0.0
        };
        // Reach: half the diagonal plus the shadow's throw and its growth.
        let reach = (pw * pw + ph * ph).sqrt() * 0.5 + throw * 2.0 + 2.0;
        // The stamp's centre in WORLD pixels, independent of the region.
        let (wx, wy) = (st.x * scale, st.y * scale);
        let (ox, oy) = ((wx - reach).floor() as i64, (wy - reach).floor() as i64);
        let side = (reach * 2.0).ceil() as i64 + 2;
        if ox + side < x0 || oy + side < y0 || ox > x0 + w || oy > y0 + h {
            continue;
        }
        let tint = st.tint.as_deref().map(parse_hex);
        let shadow =
            (st.shadow && def.height > 0.0).then_some(def.height * st.scale * scale * 0.12);
        if side * side <= MAX_SPRITE_PIXELS {
            let Some(mut sprite) = tiny_skia::Pixmap::new(side as u32, side as u32) else {
                continue;
            };
            draw_stamp(
                &mut sprite,
                def,
                (wx - ox as f32, wy - oy as f32),
                (pw, ph),
                st.rotation,
                st.flip,
                tint,
                shadow,
            );
            pixmap.draw_pixmap(
                (ox - x0) as i32,
                (oy - y0) as i32,
                sprite.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                tiny_skia::Transform::identity(),
                None,
            );
        } else {
            draw_stamp(
                pixmap,
                def,
                (wx - x0 as f32, wy - y0 as f32),
                (pw, ph),
                st.rotation,
                st.flip,
                tint,
                shadow,
            );
        }
    }
}

/// A square icon of a stamp (for the palette), on a transparent background.
pub fn stamp_icon(def: &ndcart_paint::StampDef, px: u32) -> Vec<u8> {
    let mut pm = tiny_skia::Pixmap::new(px, px).expect("icon pixmap");
    let longest = def.size[0].max(def.size[1]);
    let k = px as f32 * 0.86 / longest;
    draw_stamp(
        &mut pm,
        def,
        (px as f32 / 2.0, px as f32 / 2.0),
        (def.size[0] * k, def.size[1] * k),
        0.0,
        false,
        None,
        (def.height > 0.0).then_some(px as f32 * 0.04),
    );
    pm.encode_png().expect("png")
}

/// The picture behind everything: scaled to its rectangle in cells (bilinear),
/// at the underlay's opacity, over the void and under the rooms.
fn draw_underlay(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    let Some(u) = &floor.underlay else { return };
    let Some(img) = crate::assets::global().get_pixmap(&u.asset) else {
        return;
    };
    let scale = ppc as f32;
    let (sx, sy) = (
        u.w * scale / img.width() as f32,
        u.h * scale / img.height() as f32,
    );
    let paint = tiny_skia::PixmapPaint {
        opacity: u.opacity,
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    let t = tiny_skia::Transform::from_row(
        sx,
        0.0,
        0.0,
        sy,
        u.x * scale - x0 as f32,
        u.y * scale - y0 as f32,
    );
    pixmap.draw_pixmap(0, 0, img.as_ref().as_ref(), &paint, t, None);
}

/// Composite the floor's paint strokes over the baked cells, in z-order.
/// A paint stroke mixes its material toward each pixel by `coverage * opacity`;
/// an eraser mixes back toward what was there before any stroke (the room
/// layer), so it reveals rather than cuts. Only strokes that can touch this
/// region are prepared. Deterministic: same strokes, same pixels.
fn apply_strokes(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    use ndcart_core::stroke::StrokeKind;
    if floor.strokes.is_empty() {
        return;
    }
    let (w, h) = (pixmap.width() as i64, pixmap.height() as i64);
    let scale = ppc as f32;
    let region = [
        x0 as f32 / scale,
        y0 as f32 / scale,
        (x0 + w) as f32 / scale,
        (y0 + h) as f32 / scale,
    ];
    // Cheap reject on the raw points first (a stroke's reach is under one `size`).
    let prepared: Vec<(&ndcart_core::Stroke, ndcart_paint::Prepared)> = floor
        .strokes
        .iter()
        .filter(|s| {
            let b = s.point_bounds();
            let pad = s.size;
            b[0] - pad <= region[2]
                && b[2] + pad >= region[0]
                && b[1] - pad <= region[3]
                && b[3] + pad >= region[1]
        })
        .map(|s| (s, ndcart_paint::Prepared::new(s)))
        .filter(|(_, p)| p.touches(region))
        .collect();
    if prepared.is_empty() {
        return;
    }
    // What the erasers reveal: the pixels before any stroke.
    let base: Option<Vec<u8>> = prepared
        .iter()
        .any(|(s, _)| s.kind == StrokeKind::Erase)
        .then(|| pixmap.data().to_vec());
    let stride = w as usize * 4;
    for (s, p) in &prepared {
        let surface = s
            .material
            .as_deref()
            .and_then(ndcart_paint::find)
            .map(Surface::Mat)
            .unwrap_or_else(|| Surface::Flat(parse_hex(&s.color)));
        let px0 = ((p.bounds[0] * scale).floor() as i64 - x0).clamp(0, w);
        let px1 = ((p.bounds[2] * scale).ceil() as i64 - x0 + 1).clamp(0, w);
        let py0 = ((p.bounds[1] * scale).floor() as i64 - y0).clamp(0, h);
        let py1 = ((p.bounds[3] * scale).ceil() as i64 - y0 + 1).clamp(0, h);
        let data = pixmap.data_mut();
        for py in py0..py1 {
            for px in px0..px1 {
                let u = ((x0 + px) as f32 + 0.5) / scale;
                let v = ((y0 + py) as f32 + 0.5) / scale;
                let cov = p.coverage(u, v);
                if cov <= 0.0 {
                    continue;
                }
                let a = cov * s.opacity;
                let i = py as usize * stride + px as usize * 4;
                let cur = [data[i], data[i + 1], data[i + 2]];
                let target = match s.kind {
                    StrokeKind::Paint => surface.sample(u, v),
                    StrokeKind::Erase => {
                        let b = base.as_ref().expect("base kept for erasers");
                        [b[i], b[i + 1], b[i + 2]]
                    }
                };
                let c = ndcart_paint::mix(cur, target, a);
                data[i] = c[0];
                data[i + 1] = c[1];
                data[i + 2] = c[2];
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pixmap(
    floor: &FloorDoc,
    walls: &[Wall],
    cell_size: f64,
    ppc: i64,
    x0: i64,
    y0: i64,
    w: u32,
    h: u32,
) -> tiny_skia::Pixmap {
    let tile_px_w = i64::from(w);
    let tile_px_h = i64::from(h);
    let k = ppc as f32 / cell_size.max(1.0) as f32; // wall pixels -> image pixels

    let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("pixmap");
    pixmap.fill(tiny_skia::Color::from_rgba8(
        VOID[0], VOID[1], VOID[2], 0xff,
    ));

    // Cell range intersecting the tile (in cell coordinates).
    let cx_from = x0.div_euclid(ppc);
    let cx_to = (x0 + tile_px_w - 1).div_euclid(ppc);
    let cy_from = y0.div_euclid(ppc);
    let cy_to = (y0 + tile_px_h - 1).div_euclid(ppc);

    draw_underlay(&mut pixmap, floor, ppc, x0, y0);

    // Cells: each painted cell is filled pixel by pixel from its material (or
    // flat colour), blending toward a differently-painted neighbour near the
    // shared edge. Everything is sampled in world cell coordinates, so tiles,
    // zoom levels and the full-map export all agree.
    let surfaces = surfaces_of(floor);
    let stride = w as usize * 4;
    for (cell, space_id) in floor.grid.cells() {
        let cx = i64::from(cell.x);
        let cy = i64::from(cell.y);
        if cx < cx_from || cx > cx_to || cy < cy_from || cy > cy_to {
            continue;
        }
        let own = surfaces.get(space_id).copied().unwrap_or(FALLBACK);
        // The four neighbours' surfaces, only when painted and different.
        let nb = |c: CellKey| {
            floor
                .grid
                .cell_space(c)
                .and_then(|id| surfaces.get(id).copied())
                .filter(|n| !own.same_as(n))
        };
        let (west, east) = (nb(cell.west()), nb(cell.east()));
        let (north, south) = (nb(cell.north()), nb(cell.south()));
        let (wx, wy) = (cx * ppc, cy * ppc);
        let (rx, ry) = ((wx - x0).max(0), (wy - y0).max(0));
        let (rx2, ry2) = (
            ((wx + ppc) - x0).min(tile_px_w),
            ((wy + ppc) - y0).min(tile_px_h),
        );
        if rx >= rx2 || ry >= ry2 {
            continue;
        }
        let data = pixmap.data_mut();
        for py in ry..ry2 {
            for px in rx..rx2 {
                let u = (x0 + px) as f32 + 0.5;
                let v = (y0 + py) as f32 + 0.5;
                let (u, v) = (u / ppc as f32, v / ppc as f32);
                let (fx, fy) = (u - cell.x as f32, v - cell.y as f32);
                let mut c = own.sample(u, v);
                // Horizontal edge pair, then vertical; near a corner both apply.
                let (hn, hd, hs) = if fx < 0.5 {
                    (west, fx, Side::Far)
                } else {
                    (east, 1.0 - fx, Side::Near)
                };
                if let Some(n) = hn.filter(|_| hd < BLEND_WIDTH) {
                    c = edge_mix(c, n.sample(u, v), hd, hs, u, v, PAINT_SEED);
                }
                let (vn, vd, vs) = if fy < 0.5 {
                    (north, fy, Side::Far)
                } else {
                    (south, 1.0 - fy, Side::Near)
                };
                if let Some(n) = vn.filter(|_| vd < BLEND_WIDTH) {
                    c = edge_mix(c, n.sample(u, v), vd, vs, u, v, PAINT_SEED);
                }
                let i = py as usize * stride + px as usize * 4;
                data[i] = c[0];
                data[i + 1] = c[1];
                data[i + 2] = c[2];
                data[i + 3] = 0xff;
            }
        }
    }

    apply_strokes(&mut pixmap, floor, ppc, x0, y0);
    apply_stamps(&mut pixmap, floor, ppc, x0, y0);

    // Walls on top: stroke width grows with zoom, min 1px, so z0 reads.
    let mut wall_paint = tiny_skia::Paint {
        anti_alias: true,
        ..Default::default()
    };
    let base_width = ((ppc as f32) / 16.0).max(1.0);
    let width = base_width * 2.0; // the largest stroke, for the cheap reject below
    for wall in walls {
        if wall.pts.len() != 2 {
            continue;
        }
        let ax = (wall.pts[0][0] as f32) * k - x0 as f32;
        let ay = (wall.pts[0][1] as f32) * k - y0 as f32;
        let bx = (wall.pts[1][0] as f32) * k - x0 as f32;
        let by = (wall.pts[1][1] as f32) * k - y0 as f32;
        // Cheap reject: segment fully outside the tile.
        if (ax < -width && bx < -width)
            || (ax > tile_px_w as f32 + width && bx > tile_px_w as f32 + width)
            || (ay < -width && by < -width)
            || (ay > tile_px_h as f32 + width && by > tile_px_h as f32 + width)
        {
            continue;
        }
        let (rgb, mult) = wall_style(wall.kind);
        wall_paint.set_color_rgba8(rgb[0], rgb[1], rgb[2], 0xff);
        let stroke = tiny_skia::Stroke {
            width: base_width * mult,
            ..Default::default()
        };
        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(ax, ay);
        pb.line_to(bx, by);
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(
                &path,
                &wall_paint,
                &stroke,
                tiny_skia::Transform::identity(),
                None,
            );
        }
    }

    pixmap
}

/// Derive walls once per revision (callers cache the result).
pub fn derive_walls(floor: &FloorDoc, cell_size: f64) -> Vec<Wall> {
    floor_walls(floor, cell_size).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::grid::{Space, SpaceKind};
    use ndcart_core::journal::Op;
    use ndcart_core::{CellKey, Journal};

    fn painted_floor() -> FloorDoc {
        let mut floor = FloorDoc::new("f".into(), "Ground".into());
        let mut j = Journal::default();
        j.push(
            &mut floor,
            &Op::UpsertSpace {
                space: Space {
                    id: "a".into(),
                    name: "A".into(),
                    color: "#7f5539".into(),
                    kind: SpaceKind::Room,
                    material: None,
                },
            },
        )
        .unwrap();
        j.push(
            &mut floor,
            &Op::PaintCells {
                cells: (0..4).map(|x| CellKey::new(x, 0)).collect(),
                space: Some("a".into()),
            },
        )
        .unwrap();
        floor
    }

    #[test]
    fn walls_land_on_cell_edges() {
        // A wall at 1 cell (32 px lattice) must draw at ppc, not 2*ppc: the
        // pre-fix tiles scaled lattice pixels by pixels-per-cell.
        let floor = painted_floor();
        let walls = derive_walls(&floor, 32.0);
        let (png, w, h) = bake_full(&floor, &walls, 32.0, 16).unwrap();
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let px = |x: u32, y: u32| {
            let p = pm.pixel(x, y).unwrap();
            [p.red(), p.green(), p.blue()]
        };
        assert_eq!(px(8, 8), [0x7f, 0x55, 0x39], "cell interior is floor");
        // The stroke is anti-aliased and half-clipped at the image edge, so look
        // for "clearly brighter than floor (0x7f)" rather than the exact colour.
        let edge = (0..h).any(|y| px(w - 1, y)[0] > 0x90 || px(w - 2, y)[0] > 0x90);
        assert!(edge, "the outer wall sits on the image edge");
    }

    #[test]
    fn webp_and_avif_encode_and_decode_back() {
        let floor = painted_floor();
        let walls = derive_walls(&floor, 32.0);
        let (png, w, h) = bake_full(&floor, &walls, 32.0, 8).unwrap();
        let reference = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let (webp, ww, wh) = bake_full_as(&floor, &walls, 32.0, 8, ImageFormat::Webp, 90).unwrap();
        assert_eq!((ww, wh), (w, h));
        assert_eq!(&webp[0..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");
        // Lossless: pixels come back exactly.
        let back = image::load_from_memory_with_format(&webp, image::ImageFormat::WebP)
            .unwrap()
            .to_rgba8();
        assert_eq!(back.as_raw().as_slice(), reference.data());
        let (avif, ..) = bake_full_as(&floor, &walls, 32.0, 8, ImageFormat::Avif, 80).unwrap();
        assert_eq!(&avif[4..12], b"ftypavif");
    }

    #[test]
    fn doors_and_windows_are_drawn_in_their_own_colours() {
        use ndcart_core::grid::{Edge, EdgeKey, Mark, MarkKind};
        let mut floor = painted_floor(); // cells (0..4, 0)
        floor
            .grid
            .set_mark(
                EdgeKey {
                    cell: CellKey::new(1, 0),
                    edge: Edge::North,
                },
                Some(Mark::new(MarkKind::Door)),
            )
            .unwrap();
        floor
            .grid
            .set_mark(
                EdgeKey {
                    cell: CellKey::new(3, 0),
                    edge: Edge::South,
                },
                Some(Mark::new(MarkKind::Window)),
            )
            .unwrap();
        let walls = derive_walls(&floor, 32.0);
        let (png, ..) = bake_full(&floor, &walls, 32.0, 32).unwrap();
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let near = |x: u32, y: u32, want: [u8; 3]| {
            (y.saturating_sub(2)..=y + 2).any(|yy| {
                let p = pm.pixel(x, yy).unwrap();
                let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs();
                d(p.red(), want[0]) < 12 && d(p.green(), want[1]) < 12 && d(p.blue(), want[2]) < 12
            })
        };
        assert!(near(48, 0, DOOR_RGB), "door on the north edge of cell 1");
        assert!(
            near(112, 32, WINDOW_RGB),
            "window on the south edge of cell 3"
        );
        assert!(near(16, 0, WALL_RGB), "plain wall keeps the wall colour");
    }

    /// Two adjacent rooms in different materials, an unknown-material room and a flat one.
    fn material_floor() -> FloorDoc {
        use ndcart_core::grid::default_space;
        let mut f = FloorDoc::new("f".into(), "G".into());
        for (id, mat, color) in [
            ("a", Some("wood_planks"), "#111111"),
            ("b", Some("cobblestone"), "#222222"),
            ("c", Some("no_such_material"), "#336699"),
            ("d", None, "#aa5500"),
        ] {
            let mut sp = default_space(id, id, SpaceKind::Room);
            sp.material = mat.map(String::from);
            sp.color = color.into();
            f.grid.upsert_space(sp).unwrap();
        }
        let rect = |x0: i32, x1: i32, y0: i32, y1: i32| -> Vec<CellKey> {
            (x0..=x1)
                .flat_map(|x| (y0..=y1).map(move |y| CellKey::new(x, y)))
                .collect()
        };
        f.grid.paint(&rect(0, 3, 0, 3), Some("a")).unwrap();
        f.grid.paint(&rect(4, 7, 0, 3), Some("b")).unwrap();
        f.grid.paint(&rect(0, 3, 4, 7), Some("c")).unwrap();
        f.grid.paint(&rect(4, 7, 4, 7), Some("d")).unwrap();
        f
    }

    #[test]
    fn a_tile_is_the_matching_crop_of_the_full_render() {
        // Materials are sampled in world coordinates, so baking in 256 px chunks
        // and baking the whole map agree pixel for pixel — no seams at tile joins.
        let f = material_floor();
        let walls: Vec<Wall> = vec![];
        let (full, w, _h) = bake_full(&f, &walls, 32.0, 64).unwrap(); // 8x8 cells = 512 px
        let full = tiny_skia::Pixmap::decode_png(&full).unwrap();
        assert_eq!(w, 512);
        // z2 is 64 px/cell: tile (1,0) covers x 256..512, y 0..256 of the same image.
        let tile = bake_tile(&f, &walls, 32.0, 2, 1, 0);
        let tile = tiny_skia::Pixmap::decode_png(&tile).unwrap();
        for y in 0..256u32 {
            for x in 0..256u32 {
                assert_eq!(tile.pixel(x, y), full.pixel(256 + x, y), "({x},{y})");
            }
        }
    }

    #[test]
    fn materials_blend_continuously_across_a_shared_edge() {
        let f = material_floor();
        let (png, ..) = bake_full(&f, &[], 32.0, 64).unwrap();
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        let px = |x: u32, y: u32| {
            let p = pm.pixel(x, y).unwrap();
            [
                i32::from(p.red()),
                i32::from(p.green()),
                i32::from(p.blue()),
            ]
        };
        let step = |a: [i32; 3], b: [i32; 3]| (0..3).map(|i| (a[i] - b[i]).abs()).sum::<i32>();
        // The wood|cobble edge is x = 256. Across it, row by row, the largest
        // jump between neighbouring pixels must not stand out from the jumps
        // *inside* the textures (a hard seam would be far bigger).
        let mut at_edge = 0;
        let mut inside = 0;
        for y in 8..120u32 {
            at_edge = at_edge.max(step(px(255, y), px(256, y)));
            inside = inside.max(step(px(120, y), px(121, y)).max(step(px(400, y), px(401, y))));
        }
        assert!(
            at_edge <= inside.max(60) + 40,
            "a seam at the edge: {at_edge} vs {inside} inside the textures"
        );
        // And the blend is real: a few pixels in, the colour differs from the
        // far-inside colour of the same row band (the neighbour bleeds in).
        let near = px(256 - 3, 30);
        let far = px(256 - 40, 30);
        assert_ne!(near, far);
    }

    #[test]
    fn unknown_material_and_flat_space_keep_their_colour() {
        let f = material_floor();
        let (png, ..) = bake_full(&f, &[], 32.0, 32).unwrap();
        let pm = tiny_skia::Pixmap::decode_png(&png).unwrap();
        // Room c (unknown id) centre: flat #336699. Room d centre: flat #aa5500.
        let at = |cx: u32, cy: u32| {
            let p = pm.pixel(cx * 32 + 16, cy * 32 + 16).unwrap();
            [p.red(), p.green(), p.blue()]
        };
        assert_eq!(at(1, 5), [0x33, 0x66, 0x99]);
        assert_eq!(at(5, 5), [0xaa, 0x55, 0x00]);
        // A material room is textured, not flat.
        let wood: std::collections::BTreeSet<_> = (0..32u32)
            .flat_map(|y| (0..32u32).map(move |x| (x, y)))
            .map(|(x, y)| {
                pm.pixel(32 + x, 32 + y)
                    .map(|p| (p.red(), p.green(), p.blue()))
            })
            .collect();
        assert!(wood.len() > 6, "wood planks should have texture");
    }

    fn mk_stroke(
        id: &str,
        kind: ndcart_core::StrokeKind,
        brush: ndcart_core::BrushKind,
        color: &str,
        size: f32,
        pts: &[(f32, f32)],
    ) -> ndcart_core::Stroke {
        ndcart_core::Stroke {
            id: id.into(),
            kind,
            brush,
            material: None,
            color: color.into(),
            size,
            opacity: 1.0,
            hardness: 1.0,
            roughness: 0.0,
            density: 0.8,
            seed: 5,
            points: pts
                .iter()
                .map(|&(x, y)| ndcart_core::StrokePoint {
                    x,
                    y,
                    pressure: 1.0,
                    tilt: 0.0,
                })
                .collect(),
        }
    }

    fn decode(png: &[u8]) -> tiny_skia::Pixmap {
        tiny_skia::Pixmap::decode_png(png).unwrap()
    }

    fn rgb(pm: &tiny_skia::Pixmap, x: u32, y: u32) -> [u8; 3] {
        let p = pm.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue()]
    }

    #[test]
    fn a_paint_stroke_lays_colour_along_its_path_even_over_the_void() {
        use ndcart_core::{BrushKind, StrokeKind};
        let mut f = material_floor(); // 8x8 cells
        let st = mk_stroke(
            "s",
            StrokeKind::Paint,
            BrushKind::Round,
            "#ff0000",
            2.0,
            &[(1.0, 9.5), (6.0, 9.5)],
        );
        f.add_stroke(st, None).unwrap();
        let (png, w, h) = bake_full_stroked(&f, 32);
        // y = 9.5 cells is below the painted rooms (rows 0..8): over the void.
        assert!(h >= 10 * 32 && w >= 8 * 32);
        let pm = decode(&png);
        assert_eq!(
            rgb(&pm, 3 * 32, (9.5 * 32.0) as u32),
            [255, 0, 0],
            "on the path"
        );
        assert_eq!(
            // 8.25 cells: below the rooms (rows 0..8) and 1.25 from the path.
            rgb(&pm, 3 * 32, (9.5 * 32.0) as u32 - 40),
            VOID[..3],
            "off the path stays void"
        );
        assert_eq!(
            rgb(&pm, 3 * 32, 3 * 32 + 16),
            rgb(
                &decode(&bake_full_stroked(&material_floor(), 32).0),
                3 * 32,
                3 * 32 + 16
            ),
            "cells elsewhere untouched"
        );
    }

    #[test]
    fn opacity_and_material_strokes_behave() {
        use ndcart_core::{BrushKind, StrokeKind};
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut half = mk_stroke(
            "h",
            StrokeKind::Paint,
            BrushKind::Round,
            "#ff0000",
            2.0,
            &[(1.0, 1.0), (5.0, 1.0)],
        );
        half.opacity = 0.5;
        f.add_stroke(half, None).unwrap();
        let mut grass = mk_stroke(
            "g",
            StrokeKind::Paint,
            BrushKind::Round,
            "#00ff00",
            2.0,
            &[(1.0, 4.0), (5.0, 4.0)],
        );
        grass.material = Some("grass".into());
        f.add_stroke(grass, None).unwrap();
        // Make the floor large enough to bake (needs at least a cell painted).
        let (png, ..) = bake_full_stroked(&f, 32);
        let pm = decode(&png);
        let mid = rgb(&pm, 96, 32);
        // 50% red over the void (0x14,0x16,0x1a) is about (137, 11, 13).
        assert!(
            (i32::from(mid[0]) - 137).abs() <= 2 && mid[1] < 20,
            "{mid:?}"
        );
        // A material stroke is textured, not the flat fallback colour.
        let seen: std::collections::BTreeSet<_> =
            (0..40u32).map(|i| rgb(&pm, 70 + i * 2, 4 * 32)).collect();
        assert!(
            seen.len() > 5,
            "grass should vary along the stroke: {}",
            seen.len()
        );
        assert!(
            !seen.contains(&[0, 255, 0]),
            "material wins over the fallback colour"
        );
    }

    #[test]
    fn an_eraser_reveals_the_room_layer_and_later_strokes_draw_on_top() {
        use ndcart_core::{BrushKind, StrokeKind};
        let mut f = material_floor();
        let before = decode(&bake_full_stroked(&f, 32).0);
        f.add_stroke(
            mk_stroke(
                "red",
                StrokeKind::Paint,
                BrushKind::Round,
                "#ff0000",
                3.0,
                &[(1.0, 1.0), (3.0, 1.0)],
            ),
            None,
        )
        .unwrap();
        // Erase the right half of the red stroke only.
        f.add_stroke(
            mk_stroke(
                "erase",
                StrokeKind::Erase,
                BrushKind::Round,
                "#000000",
                1.0,
                &[(3.2, 1.0), (3.5, 1.0)],
            ),
            None,
        )
        .unwrap();
        let after = decode(&bake_full_stroked(&f, 32).0);
        let (left, right) = (rgb(&after, 48, 32), rgb(&after, 3 * 32 + 16, 32));
        assert_eq!(left, [255, 0, 0], "unerased part keeps the paint");
        assert_eq!(
            right,
            rgb(&before, 3 * 32 + 16, 32),
            "erased part is exactly the room layer again"
        );
        // z-order: a later stroke over the same spot wins.
        let mut g = material_floor();
        g.add_stroke(
            mk_stroke(
                "a",
                StrokeKind::Paint,
                BrushKind::Round,
                "#ff0000",
                3.0,
                &[(1.0, 1.0), (3.0, 1.0)],
            ),
            None,
        )
        .unwrap();
        g.add_stroke(
            mk_stroke(
                "b",
                StrokeKind::Paint,
                BrushKind::Round,
                "#0000ff",
                3.0,
                &[(1.0, 1.0), (3.0, 1.0)],
            ),
            None,
        )
        .unwrap();
        assert_eq!(
            rgb(&decode(&bake_full_stroked(&g, 32).0), 64, 32),
            [0, 0, 255]
        );
        // Undo-by-removal restores the exact picture.
        g.remove_stroke("b").unwrap();
        g.remove_stroke("a").unwrap();
        assert_eq!(decode(&bake_full_stroked(&g, 32).0).data(), before.data());
    }

    #[test]
    fn strokes_bake_identically_in_tiles_and_in_the_full_render() {
        use ndcart_core::{BrushKind, StrokeKind};
        let mut f = material_floor();
        // A long diagonal that crosses the tile boundary at 64 px/cell (x = 4 cells).
        let mut s = mk_stroke(
            "d",
            StrokeKind::Paint,
            BrushKind::Round,
            "#ff8800",
            1.5,
            &[(0.5, 0.5), (7.5, 7.0)],
        );
        s.roughness = 0.6;
        s.hardness = 0.5;
        f.add_stroke(s, None).unwrap();
        f.add_stroke(
            mk_stroke(
                "sc",
                StrokeKind::Paint,
                BrushKind::Scatter,
                "#112233",
                3.0,
                &[(0.5, 6.0), (7.5, 2.0)],
            ),
            None,
        )
        .unwrap();
        let (png, w, _) = bake_full_stroked(&f, 64);
        // 8 cells of rooms plus the stroke's reach: the export holds everything.
        assert!(w >= 512, "{w}");
        let full = decode(&png);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let tile = decode(&bake_tile(&f, &[], 32.0, 2, tx, ty));
            for y in 0..256u32 {
                for x in 0..256u32 {
                    assert_eq!(
                        tile.pixel(x, y),
                        full.pixel(tx as u32 * 256 + x, ty as u32 * 256 + y),
                        "tile ({tx},{ty}) pixel ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn a_busy_floor_bakes_in_reasonable_time() {
        use ndcart_core::{BrushKind, StrokeKind};
        let mut f = material_floor();
        for i in 0..300 {
            let (cx, cy) = ((i % 20) as f32 * 2.0, (i / 20) as f32 * 2.5);
            let pts: Vec<(f32, f32)> = (0..100)
                .map(|k| {
                    (
                        cx + (k as f32 * 0.13).min(1.6),
                        cy + ((k * 7 % 11) as f32) * 0.05,
                    )
                })
                .collect();
            let brush = if i % 5 == 0 {
                BrushKind::Scatter
            } else {
                BrushKind::Round
            };
            f.add_stroke(
                mk_stroke(
                    &format!("s{i}"),
                    StrokeKind::Paint,
                    brush,
                    "#aa3344",
                    1.0,
                    &pts,
                ),
                None,
            )
            .unwrap();
        }
        // Plus the heavy case: long, wide, rough strokes with many samples, as a
        // hand would draw them (a smooth wave, ~13 samples per cell).
        for i in 0..40 {
            let pts: Vec<(f32, f32)> = (0..500)
                .map(|k| {
                    let t = k as f32 / 499.0;
                    let x = 1.0 + t * 38.0;
                    (x, 2.0 + i as f32 * 0.9 + (x * 0.35).sin() * 0.8)
                })
                .collect();
            let mut st = mk_stroke(
                &format!("long{i}"),
                StrokeKind::Paint,
                BrushKind::Round,
                "#3355aa",
                4.0,
                &pts,
            );
            st.roughness = 0.5;
            st.hardness = 0.6;
            f.add_stroke(st, None).unwrap();
        }
        let t = std::time::Instant::now();
        let (png, ..) = bake_full_stroked(&f, 16);
        let ms = t.elapsed().as_millis();
        assert!(png.len() > 100);
        assert!(ms < 20_000, "300 strokes x 100 points took {ms} ms");
        println!("300 small + 40 long rough strokes (500 points), 40x40 cells at 16 px: {ms} ms");
    }

    /// Bake the whole floor, tall enough to hold strokes below the rooms.
    fn bake_full_stroked(f: &FloorDoc, ppc: i64) -> (Vec<u8>, u32, u32) {
        // No walls: these tests are about strokes. grid_extent only sees painted
        // cells, so bake through render_region to keep room for strokes below them.
        let walls: Vec<Wall> = vec![];
        let (cols, rows) = grid_extent(f);
        let (cols, rows) = (cols.max(8), rows.max(10));
        let (w, h) = (i64::from(cols) * ppc, i64::from(rows) * ppc);
        let png = render_region(f, &walls, 32.0, ppc, 0, 0, w as u32, h as u32);
        (png, w as u32, h as u32)
    }

    fn light_floor() -> FloorDoc {
        use ndcart_core::grid::default_space;
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut sp = default_space("a", "A", SpaceKind::Room);
        sp.color = "#d0d0d0".into();
        f.grid.upsert_space(sp).unwrap();
        let cells: Vec<CellKey> = (0..8)
            .flat_map(|x| (0..8).map(move |y| CellKey::new(x, y)))
            .collect();
        f.grid.paint(&cells, Some("a")).unwrap();
        f
    }

    fn mk_stamp(id: &str, asset: &str, x: f32, y: f32) -> ndcart_core::Stamp {
        ndcart_core::Stamp {
            id: id.into(),
            asset: asset.into(),
            x,
            y,
            scale: 1.0,
            rotation: 0.0,
            flip: false,
            tint: None,
            shadow: true,
        }
    }

    fn bake_stamped(f: &FloorDoc, ppc: i64) -> tiny_skia::Pixmap {
        let (png, ..) = bake_full_stroked(f, ppc);
        decode(&png)
    }

    fn lum(p: [u8; 3]) -> i32 {
        i32::from(p[0]) + i32::from(p[1]) + i32::from(p[2])
    }

    #[test]
    fn a_stamp_draws_in_place_and_leaves_the_rest_alone() {
        let mut f = light_floor();
        let before = bake_stamped(&f, 32);
        f.add_stamp(mk_stamp("c", "crate", 3.5, 3.5), None).unwrap();
        let after = bake_stamped(&f, 32);
        // The crate's centre is wood-coloured (the X braces cross at the middle, so look just off it).
        let c = rgb(&after, 3 * 32 + 16, 3 * 32 + 6);
        assert!(c[0] > c[2] + 30, "wood is warm: {c:?}");
        assert_eq!(
            rgb(&after, 6 * 32, 6 * 32),
            rgb(&before, 6 * 32, 6 * 32),
            "far away is untouched"
        );
        // An unknown asset is not drawn, and is not an error.
        let mut g = light_floor();
        g.add_stamp(mk_stamp("x", "no_such_prop", 3.5, 3.5), None)
            .unwrap();
        assert_eq!(bake_stamped(&g, 32).data(), before.data());
    }

    #[test]
    fn shadows_fall_down_and_right_and_can_be_switched_off() {
        let mut f = light_floor();
        f.add_stamp(mk_stamp("p", "pillar", 3.5, 3.5), None)
            .unwrap();
        let with = bake_stamped(&f, 48);
        let mut g = light_floor();
        let mut no = mk_stamp("p", "pillar", 3.5, 3.5);
        no.shadow = false;
        g.add_stamp(no, None).unwrap();
        let without = bake_stamped(&g, 48);
        let clear = bake_stamped(&light_floor(), 48);
        // The pillar is ~1 cell (48 px) wide centred at 168,168: just outside its
        // lower-right edge there is shadow; just outside its upper-left there is none.
        // (the pillar's radius is ~22 px; the shadow reaches ~6 px past it, down and right)
        let (lr, ul) = ((168 + 6, 168 + 22 + 3), (168 - 30, 168 - 30));
        assert!(
            lum(rgb(&with, lr.0, lr.1)) < lum(rgb(&clear, lr.0, lr.1)),
            "shadow darkens the lower right"
        );
        assert_eq!(
            rgb(&with, ul.0, ul.1),
            rgb(&clear, ul.0, ul.1),
            "no shadow on the lit side"
        );
        assert_eq!(
            rgb(&without, lr.0, lr.1),
            rgb(&clear, lr.0, lr.1),
            "shadow: false draws none"
        );
        // A flat prop (rug, height 0) throws no shadow even when asked.
        let mut r = light_floor();
        r.add_stamp(mk_stamp("r", "rug", 3.5, 3.5), None).unwrap();
        let rug = bake_stamped(&r, 48);
        assert_eq!(
            rgb(&rug, 168 + 60, 168 + 40 + 24),
            rgb(&clear, 168 + 60, 168 + 40 + 24)
        );
    }

    #[test]
    fn rotation_flip_and_tint_do_what_they_say() {
        // Rotate a table (2x1) a quarter turn: it becomes tall.
        let mut f = light_floor();
        let mut t = mk_stamp("t", "table", 4.0, 4.0);
        t.shadow = false;
        f.add_stamp(t.clone(), None).unwrap();
        let flat = bake_stamped(&f, 32);
        let mut g = light_floor();
        t.rotation = std::f32::consts::FRAC_PI_2;
        g.add_stamp(t, None).unwrap();
        let tall = bake_stamped(&g, 32);
        let clear = bake_stamped(&light_floor(), 32);
        let changed = |pm: &tiny_skia::Pixmap, x: u32, y: u32| pm.pixel(x, y) != clear.pixel(x, y);
        // flat: covers x 3..5 cells around y=4, not 0.8 cells above; tall: the reverse.
        assert!(changed(&flat, 4 * 32 - 28, 4 * 32) && !changed(&flat, 4 * 32, 4 * 32 - 26 - 8));
        assert!(changed(&tall, 4 * 32, 4 * 32 - 28) && !changed(&tall, 4 * 32 - 26 - 8, 4 * 32));
        // Tint multiplies: a black tint blacks the prop out.
        let mut b = light_floor();
        let mut bt = mk_stamp("b", "crate", 3.5, 3.5);
        bt.shadow = false;
        bt.tint = Some("#000000".into());
        b.add_stamp(bt, None).unwrap();
        assert_eq!(
            rgb(&bake_stamped(&b, 32), 3 * 32 + 16, 3 * 32 + 6),
            [0, 0, 0]
        );
        // Flip mirrors an asymmetric prop (the rock's highlight sits left of centre).
        let rock = |flip: bool| {
            let mut h = light_floor();
            let mut r = mk_stamp("r", "rock", 4.0, 4.0);
            r.shadow = false;
            r.flip = flip;
            h.add_stamp(r, None).unwrap();
            bake_stamped(&h, 64)
        };
        let (a, m) = (rock(false), rock(true));
        let mut mirrored_equal = true;
        for y in (200..320).step_by(7) {
            for x in (200..320).step_by(7) {
                let mx = 2 * 256 - x;
                if lum(rgb(&a, x, y)) != lum(rgb(&m, mx, y)) {
                    mirrored_equal = false;
                }
            }
        }
        assert!(
            !mirrored_equal || a.data() == m.data(),
            "flip must change an asymmetric prop"
        );
        assert_ne!(a.data(), m.data());
    }

    #[test]
    fn stamps_sit_above_strokes_and_in_z_order() {
        use ndcart_core::{BrushKind, StrokeKind};
        let mut f = light_floor();
        f.add_stamp(mk_stamp("c", "crate", 3.5, 3.5), None).unwrap();
        f.add_stroke(
            mk_stroke(
                "s",
                StrokeKind::Paint,
                BrushKind::Round,
                "#ff0000",
                3.0,
                &[(1.0, 3.5), (6.0, 3.5)],
            ),
            None,
        )
        .unwrap();
        let pm = bake_stamped(&f, 32);
        // The red stroke runs right through the crate, but the crate is on top.
        let on_crate = rgb(&pm, 3 * 32 + 16, 3 * 32 + 6);
        assert!(
            !(on_crate[0] > 240 && on_crate[1] < 20),
            "crate covers the stroke: {on_crate:?}"
        );
        assert_eq!(
            rgb(&pm, 6 * 32 - 4, 3 * 32 + 16),
            [255, 0, 0],
            "stroke visible beside it"
        );
        // Two overlapping stamps: the later one is drawn over the earlier.
        let mut g = light_floor();
        let mut a = mk_stamp("a", "chest", 3.5, 3.5);
        a.shadow = false;
        a.tint = Some("#ff0000".into());
        let mut b = mk_stamp("b", "chest", 3.5, 3.5);
        b.shadow = false;
        b.tint = Some("#0000ff".into());
        g.add_stamp(a, None).unwrap();
        g.add_stamp(b, None).unwrap();
        let top = rgb(&bake_stamped(&g, 32), 3 * 32 + 16, 3 * 32 + 8);
        assert!(
            top[2] >= top[0],
            "the later (blue) chest is on top: {top:?}"
        );
    }

    #[test]
    fn stamps_bake_the_same_in_tiles_and_in_the_full_render() {
        let mut f = light_floor();
        for (i, a) in ["tree", "table", "bed", "rock", "barrel", "torch"]
            .iter()
            .enumerate()
        {
            let mut s = mk_stamp(
                &format!("s{i}"),
                a,
                1.2 + i as f32 * 1.1,
                1.0 + (i % 3) as f32 * 2.1,
            );
            s.rotation = i as f32 * 0.5;
            f.add_stamp(s, None).unwrap();
        }
        let (png, ..) = bake_full_stroked(&f, 64);
        let full = decode(&png);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let tile = decode(&bake_tile(&f, &[], 32.0, 2, tx, ty));
            let mut worst = 0u8;
            for y in 0..256u32 {
                for x in 0..256u32 {
                    let (a, b) = (
                        rgb(&tile, x, y),
                        rgb(&full, tx as u32 * 256 + x, ty as u32 * 256 + y),
                    );
                    for k in 0..3 {
                        worst = worst.max(a[k].abs_diff(b[k]));
                    }
                }
            }
            // Sprites make this exact: no tolerance.
            assert_eq!(worst, 0, "tile ({tx},{ty}) differs by {worst}");
        }
    }

    #[test]
    fn the_icon_is_a_64px_png_with_a_drawn_centre() {
        for def in ndcart_paint::stamps() {
            let png = stamp_icon(def, 64);
            let pm = decode(&png);
            assert_eq!((pm.width(), pm.height()), (64, 64), "{}", def.id);
            assert!(
                pm.pixel(32, 32).unwrap().alpha() > 0,
                "{} has a centre",
                def.id
            );
            assert_eq!(
                pm.pixel(0, 0).unwrap().alpha(),
                0,
                "{} corner is clear",
                def.id
            );
        }
    }

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut pm = tiny_skia::Pixmap::new(w, h).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(rgb[0], rgb[1], rgb[2], 255));
        pm.encode_png().unwrap()
    }

    #[test]
    fn an_underlay_fills_its_rectangle_under_the_rooms_with_opacity() {
        use ndcart_core::Underlay;
        let id = crate::assets::global()
            .put(&solid_png(4, 4, [255, 0, 0]))
            .unwrap();
        let mut f = light_floor(); // a light room fills cells 0..8
        let u = |opacity: f32| Underlay {
            asset: id.clone(),
            x: 6.0,
            y: 6.0,
            w: 6.0,
            h: 6.0,
            opacity,
        };
        f.set_underlay(Some(u(1.0))).unwrap();
        let pm = bake_stamped(&f, 32);
        assert_eq!(
            (pm.width(), pm.height()),
            (12 * 32, 12 * 32),
            "the export grows to hold the underlay"
        );
        // Outside the room, inside the underlay: pure red. Inside the room: the room wins.
        assert_eq!(rgb(&pm, 10 * 32, 10 * 32), [255, 0, 0]);
        assert_eq!(
            rgb(&pm, 7 * 32, 7 * 32),
            [0xd0, 0xd0, 0xd0],
            "rooms draw over the picture"
        );
        // Outside the underlay and the room: the void.
        assert_eq!(rgb(&pm, 10 * 32, 2 * 32), VOID[..3]);
        // Half opacity mixes with the void behind it.
        f.set_underlay(Some(u(0.5))).unwrap();
        let half = rgb(&bake_stamped(&f, 32), 10 * 32, 10 * 32);
        assert!(
            (i32::from(half[0]) - 137).abs() <= 2 && half[1] < 20,
            "{half:?}"
        );
        // An asset the store does not have draws nothing and is not an error.
        f.set_underlay(Some(Underlay {
            asset: "0".repeat(64),
            ..u(1.0)
        }))
        .unwrap();
        assert_eq!(rgb(&bake_stamped(&f, 32), 10 * 32, 10 * 32), VOID[..3]);
    }

    #[test]
    fn an_underlay_bakes_the_same_in_tiles_and_in_the_full_render() {
        use ndcart_core::Underlay;
        // A gradient so any misplacement shows.
        let mut src = tiny_skia::Pixmap::new(64, 48).unwrap();
        for (i, p) in src.pixels_mut().iter_mut().enumerate() {
            let (x, y) = ((i % 64) as u8, (i / 64) as u8);
            *p = tiny_skia::ColorU8::from_rgba(x * 4, y * 5, 128, 255).premultiply();
        }
        let id = crate::assets::global()
            .put(&src.encode_png().unwrap())
            .unwrap();
        let mut f = light_floor();
        f.set_underlay(Some(Underlay {
            asset: id,
            x: 1.3,
            y: 0.7,
            w: 9.1,
            h: 6.9,
            opacity: 0.9,
        }))
        .unwrap();
        let (png, ..) = bake_full_stroked(&f, 64);
        let full = decode(&png);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let tile = decode(&bake_tile(&f, &[], 32.0, 2, tx, ty));
            for y in 0..256u32 {
                for x in 0..256u32 {
                    assert_eq!(
                        tile.pixel(x, y),
                        full.pixel(tx as u32 * 256 + x, ty as u32 * 256 + y),
                        "tile ({tx},{ty}) pixel ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn tiles_are_png_of_the_right_size() {
        let floor = painted_floor();
        let walls = derive_walls(&floor, 32.0);
        let png = bake_tile(&floor, &walls, 32.0, 0, 0, 0);
        assert!(png.len() > 100);
        // PNG magic.
        assert_eq!(&png[0..4], &[0x89, b'P', b'N', b'G']);
        // A tile far away from any content must be pure background but still valid.
        let empty = bake_tile(&floor, &walls, 32.0, 0, -9, -9);
        assert_eq!(&empty[0..4], &[0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn zoom_levels_all_bake() {
        let floor = painted_floor();
        let walls = derive_walls(&floor, 32.0);
        for z in 0..=MAX_Z {
            let png = bake_tile(&floor, &walls, 32.0, z, 0, 0);
            assert_eq!(&png[0..4], &[0x89, b'P', b'N', b'G'], "z={z}");
        }
    }

    #[test]
    fn hex_parse() {
        assert_eq!(parse_hex("#7f5539"), [0x7f, 0x55, 0x39]);
        assert_eq!(parse_hex("junk"), [0x33, 0x44, 0x55]);
        assert_eq!(parse_hex(""), [0x33, 0x44, 0x55]);
    }
}
