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

pub(crate) fn parse_hex(color: &str) -> [u8; 3] {
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
#[cfg(test)]
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

/// A tile with the floor `onion` shown faintly underneath it.
pub fn bake_tile_onion(
    floor: &FloorDoc,
    walls: &[Wall],
    cell_size: f64,
    z: u32,
    tx: i64,
    ty: i64,
    onion: Option<(&FloorDoc, &[Wall])>,
) -> Vec<u8> {
    let ppc = PPC0 << z.min(MAX_Z);
    let t = i64::from(TILE);
    render_pixmap_onion(
        floor,
        walls,
        cell_size,
        ppc,
        tx * t,
        ty * t,
        TILE,
        TILE,
        onion,
    )
    .encode_png()
    .expect("png encode")
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

/// What a space paints: a pack material, a picture-based custom material, or a flat colour.
#[derive(Clone)]
enum Surface {
    Mat(&'static ndcart_paint::Material),
    /// A picture repeated every `tile` cells (world coordinates, so tiles and
    /// the full render agree), looked up per pixel.
    Img(std::sync::Arc<tiny_skia::Pixmap>, f32, String),
    Flat([u8; 3]),
}

impl Surface {
    fn sample(&self, u: f32, v: f32) -> [u8; 3] {
        match self {
            Surface::Mat(m) => m.sample(u, v, PAINT_SEED),
            Surface::Img(pm, tile, _) => {
                let (w, h) = (pm.width() as i64, pm.height() as i64);
                let x = ((u / tile) * w as f32).floor() as i64;
                let y = ((v / tile) * h as f32).floor() as i64;
                let p = pm
                    .pixel(x.rem_euclid(w) as u32, y.rem_euclid(h) as u32)
                    .map(|p| p.demultiply());
                p.map(|c| [c.red(), c.green(), c.blue()])
                    .unwrap_or([0x33, 0x44, 0x55])
            }
            Surface::Flat(c) => *c,
        }
    }
    fn same_as(&self, o: &Surface) -> bool {
        match (self, o) {
            (Surface::Mat(a), Surface::Mat(b)) => a.id == b.id,
            (Surface::Img(_, _, a), Surface::Img(_, _, b)) => a == b,
            (Surface::Flat(a), Surface::Flat(b)) => a == b,
            _ => false,
        }
    }
}

/// The surface a space paints: its pack material, else a custom picture
/// material, else its flat colour (an unknown material id falls back to the colour).
fn surface_for(material: Option<&str>, colour: &str) -> Surface {
    if let Some(id) = material {
        if let Some(m) = ndcart_paint::find(id) {
            return Surface::Mat(m);
        }
        let assets = crate::assets::global();
        if let Some(c) = assets.find_custom(id, "material") {
            if let Some(pm) = assets.get_pixmap(&c.asset) {
                return Surface::Img(pm, c.size.max(0.25), c.id);
            }
        }
    }
    Surface::Flat(parse_hex(colour))
}

/// Resolve every space once per bake.
fn surfaces_of(floor: &FloorDoc) -> std::collections::HashMap<&str, Surface> {
    floor
        .grid
        .spaces
        .iter()
        .map(|(id, s)| (id.as_str(), surface_for(s.material.as_deref(), &s.color)))
        .collect()
}

#[cfg(test)]
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

/// A picture-based (custom) stamp: its picture, scaled to `size` cells wide,
/// rotated and flipped like a built-in one, with a soft drop shadow. Same
/// sprite-at-an-integer-offset scheme, so a tile equals the full render's crop.
/// (A tint is not applied to pictures.)
fn apply_custom_stamp(
    pixmap: &mut tiny_skia::Pixmap,
    st: &ndcart_core::Stamp,
    scale: f32,
    x0: i64,
    y0: i64,
) {
    let assets = crate::assets::global();
    let Some(c) = assets.find_custom(&st.asset, "stamp") else {
        return;
    };
    let Some(img) = assets.get_pixmap(&c.asset) else {
        return;
    };
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let pw = c.size * st.scale * scale;
    let ph = pw * ih / iw;
    let throw = if st.shadow { pw.min(ph) * 0.1 } else { 0.0 };
    let reach = (pw * pw + ph * ph).sqrt() * 0.5 + throw * 2.0 + 2.0;
    let (wx, wy) = (st.x * scale, st.y * scale);
    let (ox, oy) = ((wx - reach).floor() as i64, (wy - reach).floor() as i64);
    let side = (reach * 2.0).ceil() as i64 + 2;
    let (w, h) = (i64::from(pixmap.width()), i64::from(pixmap.height()));
    if ox + side < x0
        || oy + side < y0
        || ox > x0 + w
        || oy > y0 + h
        || side * side > MAX_SPRITE_PIXELS
    {
        return;
    }
    let Some(mut sprite) = tiny_skia::Pixmap::new(side as u32, side as u32) else {
        return;
    };
    let (cx, cy) = (wx - ox as f32, wy - oy as f32);
    let sx = pw / iw * if st.flip { -1.0 } else { 1.0 };
    let place = |dx: f32, dy: f32| {
        tiny_skia::Transform::from_translate(cx + dx, cy + dy)
            .pre_rotate(st.rotation.to_degrees())
            .pre_scale(sx, ph / ih)
            .pre_translate(-iw / 2.0, -ih / 2.0)
    };
    let mut paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    if throw > 0.0 {
        // The silhouette in soft black, thrown down and to the right.
        if let Some(mut shadow) = tiny_skia::Pixmap::new(img.width(), img.height()) {
            for (d, p) in shadow.pixels_mut().iter_mut().zip(img.pixels()) {
                *d =
                    tiny_skia::ColorU8::from_rgba(0, 0, 0, (u16::from(p.alpha()) * 70 / 255) as u8)
                        .premultiply();
            }
            sprite.draw_pixmap(
                0,
                0,
                shadow.as_ref(),
                &paint,
                place(throw * 0.6, throw),
                None,
            );
        }
    }
    paint.opacity = 1.0;
    sprite.draw_pixmap(0, 0, (*img).as_ref(), &paint, place(0.0, 0.0), None);
    pixmap.draw_pixmap(
        (ox - x0) as i32,
        (oy - y0) as i32,
        sprite.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        tiny_skia::Transform::identity(),
        None,
    );
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
            if st.asset.starts_with("stp_") {
                apply_custom_stamp(pixmap, st, scale, x0, y0);
            }
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
/// A square icon of a picture (a custom stamp), fitted with a margin.
pub fn picture_icon(img: &tiny_skia::Pixmap, px: u32) -> Vec<u8> {
    let mut pm = tiny_skia::Pixmap::new(px, px).expect("icon pixmap");
    let k = px as f32 * 0.86 / (img.width().max(img.height()) as f32);
    let t = tiny_skia::Transform::from_translate(
        (px as f32 - img.width() as f32 * k) / 2.0,
        (px as f32 - img.height() as f32 * k) / 2.0,
    )
    .pre_scale(k, k);
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    pm.draw_pixmap(0, 0, img.as_ref(), &paint, t, None);
    pm.encode_png().expect("png")
}

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

/// The square or hex lattice over the map. Geometry is built in WORLD pixels
/// and shifted by an exact integer, all lines go into ONE path (so crossings
/// and shared hex edges never blend twice), and only lines inside the floor's
/// extent are drawn: a tile and the crop of the full render agree.
fn draw_map_grid(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    use ndcart_core::MapGridKind;
    use tiny_skia::{Paint, PathBuilder, Stroke, Transform};
    const SQRT_3: f32 = 1.732_050_8;
    let Some(g) = &floor.map_grid else { return };
    let (cols, rows) = grid_extent(floor);
    let s = ppc as f32;
    let (maxx, maxy) = (cols as f32 * s, rows as f32 * s);
    let (w, h) = (pixmap.width() as f32, pixmap.height() as f32);
    let (tx0, ty0) = (x0 as f32, y0 as f32);
    let pad = 2.0 + s; // a hex reaches a little beyond its centre cell
    let mut pb = PathBuilder::new();
    let mut count = 0usize;
    let step = g.size * s;
    match g.kind {
        MapGridKind::Square => {
            let first = ((tx0 - pad) / step).floor().max(0.0) as i64;
            let last = (((tx0 + w + pad) / step).ceil() as i64).max(first);
            for i in first..=last.min(first + 4_000) {
                let x = i as f32 * step;
                if x > maxx {
                    break;
                }
                pb.move_to(x, 0.0);
                pb.line_to(x, maxy);
                count += 1;
            }
            let first = ((ty0 - pad) / step).floor().max(0.0) as i64;
            let last = (((ty0 + h + pad) / step).ceil() as i64).max(first);
            for j in first..=last.min(first + 4_000) {
                let y = j as f32 * step;
                if y > maxy {
                    break;
                }
                pb.move_to(0.0, y);
                pb.line_to(maxx, y);
                count += 1;
            }
            // The closing edges of the canvas (unless a line already sits on them).
            if maxx % step > 0.5 {
                pb.move_to(maxx, 0.0);
                pb.line_to(maxx, maxy);
            }
            if maxy % step > 0.5 {
                pb.move_to(0.0, maxy);
                pb.line_to(maxx, maxy);
            }
        }
        MapGridKind::Hex => {
            // Pointy-top hexes, `step` wide flat to flat; rows nest 1.5 radii apart.
            let rad = step / SQRT_3;
            let row_h = rad * 1.5;
            let row_lo = ((ty0 - pad - rad) / row_h).floor().max(0.0) as i64;
            let row_hi = ((ty0 + h + pad + rad) / row_h).ceil() as i64;
            for r in row_lo..=row_hi.min(row_lo + 4_000) {
                let cy = r as f32 * row_h;
                if cy > maxy + rad {
                    break;
                }
                let off = if r % 2 == 0 { 0.0 } else { step / 2.0 };
                let col_lo = ((tx0 - pad - off - step) / step).floor().max(0.0) as i64;
                let col_hi = ((tx0 + w + pad - off + step) / step).ceil() as i64;
                for c in col_lo..=col_hi.min(col_lo + 4_000) {
                    let cx = c as f32 * step + off;
                    if cx > maxx + step / 2.0 {
                        break;
                    }
                    let hw = step / 2.0;
                    let v = [
                        (cx, cy - rad),
                        (cx + hw, cy - rad / 2.0),
                        (cx + hw, cy + rad / 2.0),
                        (cx, cy + rad),
                        (cx - hw, cy + rad / 2.0),
                        (cx - hw, cy - rad / 2.0),
                    ];
                    // Only hexes whose centre lies on the map.
                    if cx > maxx || cy > maxy {
                        continue;
                    }
                    // Each hex draws only three of its six edges (upper-left, upper-right
                    // and right); its neighbours draw the rest, so no edge is stroked
                    // twice (two coincident anti-aliased strokes would blend darker).
                    pb.move_to(v[5].0, v[5].1);
                    pb.line_to(v[0].0, v[0].1);
                    pb.line_to(v[1].0, v[1].1);
                    pb.line_to(v[2].0, v[2].1);
                    count += 1;
                }
            }
        }
    }
    if count == 0 {
        return;
    }
    let Some(path) = pb.finish() else { return };
    let rgb = parse_hex(&g.color);
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        rgb[0],
        rgb[1],
        rgb[2],
        (g.opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
    );
    paint.anti_alias = true;
    let stroke = Stroke {
        width: (s / 24.0).max(1.0),
        ..Default::default()
    };
    pixmap.stroke_path(
        &path,
        &paint,
        &stroke,
        Transform::from_translate(-tx0, -ty0),
        None,
    );
}

/// Regions, then routes, then marker pins, over everything. Paths are built in
/// WORLD pixels and shifted by an exact integer translation, so a tile and the
/// matching crop of the full render rasterize identical geometry.
fn draw_overlay(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    use tiny_skia::{
        FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Stroke, StrokeDash, Transform,
    };
    if floor.regions.is_empty() && floor.routes.is_empty() && floor.markers.is_empty() {
        return;
    }
    let s = ppc as f32;
    let (w, h) = (pixmap.width(), pixmap.height());
    let shift = Transform::from_translate(-(x0 as f32), -(y0 as f32));
    let world = |p: &[f32; 2]| (p[0] * s, p[1] * s);
    let on_screen = |lo: (f32, f32), hi: (f32, f32), pad: f32| {
        hi.0 + pad >= x0 as f32
            && hi.1 + pad >= y0 as f32
            && lo.0 - pad <= (x0 + i64::from(w)) as f32
            && lo.1 - pad <= (y0 + i64::from(h)) as f32
    };
    let bounds = |pts: &[[f32; 2]]| {
        let (mut lo, mut hi) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
        for p in pts {
            let (x, y) = world(p);
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        }
        (lo, hi)
    };
    let paint_of = |rgb: [u8; 3], alpha: f32| {
        let mut p = Paint::default();
        p.set_color_rgba8(
            rgb[0],
            rgb[1],
            rgb[2],
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        );
        p.anti_alias = true;
        p
    };

    for r in &floor.regions {
        let (lo, hi) = bounds(&r.points);
        if !on_screen(lo, hi, s) {
            continue;
        }
        let mut pb = PathBuilder::new();
        let (fx, fy) = world(&r.points[0]);
        pb.move_to(fx, fy);
        for p in &r.points[1..] {
            let (x, y) = world(p);
            pb.line_to(x, y);
        }
        pb.close();
        let Some(path) = pb.finish() else { continue };
        let rgb = parse_hex(&r.color);
        pixmap.fill_path(
            &path,
            &paint_of(rgb, r.opacity),
            FillRule::Winding,
            shift,
            None,
        );
        if r.hatch {
            // Diagonal lines on a world-fixed grid, clipped to the polygon.
            if let Some(mut mask) = Mask::new(w, h) {
                mask.fill_path(&path, FillRule::Winding, true, shift);
                let step = (0.35 * s).max(4.0);
                let mut hb = PathBuilder::new();
                let mut k = ((lo.0 + lo.1) / step).floor() * step;
                while k <= hi.0 + hi.1 {
                    // the line x + y = k, from the top edge of the region to its bottom edge
                    hb.move_to(k - lo.1, lo.1);
                    hb.line_to(k - hi.1, hi.1);
                    k += step;
                }
                if let Some(lines) = hb.finish() {
                    let st = Stroke {
                        width: (0.04 * s).max(1.0),
                        ..Default::default()
                    };
                    pixmap.stroke_path(
                        &lines,
                        &paint_of(rgb, (r.opacity + 0.35).min(0.9)),
                        &st,
                        shift,
                        Some(&mask),
                    );
                }
            }
        }
        let border = Stroke {
            width: (0.07 * s).max(1.5),
            line_join: LineJoin::Round,
            ..Default::default()
        };
        pixmap.stroke_path(
            &path,
            &paint_of(ndcart_paint::shade(rgb, 0.7), 0.95),
            &border,
            shift,
            None,
        );
    }

    for r in &floor.routes {
        let pts = r.drawn_points();
        let (lo, hi) = bounds(&pts);
        if !on_screen(lo, hi, s) {
            continue;
        }
        let mut pb = PathBuilder::new();
        let (fx, fy) = world(&pts[0]);
        pb.move_to(fx, fy);
        for p in &pts[1..] {
            let (x, y) = world(p);
            pb.line_to(x, y);
        }
        let Some(path) = pb.finish() else { continue };
        let width = (r.width * s).max(1.5);
        let mut st = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Default::default()
        };
        if r.dashed {
            st.dash = StrokeDash::new(vec![width * 3.0, width * 2.0], 0.0);
        }
        let rgb = parse_hex(&r.color);
        pixmap.stroke_path(&path, &paint_of(rgb, 0.95), &st, shift, None);
        // Waymarks: a dot every 3 cells along the route, measured from its start.
        let every = 3.0 * s;
        let (mut walked, mut next) = (0.0f32, every);
        for seg in pts.windows(2) {
            let (a, b) = (world(&seg[0]), world(&seg[1]));
            let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
            while next <= walked + len && len > 0.0 {
                let t = (next - walked) / len;
                let (cx, cy) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                let mut dot = PathBuilder::new();
                dot.push_circle(cx, cy, (width * 0.9).max(2.0));
                if let Some(d) = dot.finish() {
                    pixmap.fill_path(
                        &d,
                        &paint_of(ndcart_paint::shade(rgb, 0.8), 1.0),
                        FillRule::Winding,
                        shift,
                        None,
                    );
                }
                next += every;
            }
            walked += len;
        }
    }

    for m in &floor.markers {
        let (cx, cy) = world(&[m.x, m.y]);
        let rad = (0.3 * s).max(4.5);
        if !on_screen((cx, cy), (cx, cy), rad * 2.0) {
            continue;
        }
        let rgb = parse_hex(&m.color);
        let mut ring = PathBuilder::new();
        ring.push_circle(cx, cy, rad + 1.5);
        if let Some(p) = ring.finish() {
            pixmap.fill_path(
                &p,
                &paint_of([255, 255, 255], 0.95),
                FillRule::Winding,
                shift,
                None,
            );
        }
        let mut body = PathBuilder::new();
        body.push_circle(cx, cy, rad);
        if let Some(p) = body.finish() {
            pixmap.fill_path(&p, &paint_of(rgb, 1.0), FillRule::Winding, shift, None);
        }
        let mut core = PathBuilder::new();
        core.push_circle(cx, cy, rad * 0.35);
        if let Some(p) = core.finish() {
            pixmap.fill_path(
                &p,
                &paint_of(ndcart_paint::shade(rgb, 0.45), 1.0),
                FillRule::Winding,
                shift,
                None,
            );
        }
    }
}

/// What a piece of map text is anchored to.
enum Anchor {
    Straight {
        x: f32,
        y: f32,
        rotation: f32,
        left: bool,
    },
    Path(Vec<[f32; 2]>),
}

struct TextItem {
    text: String,
    size_cells: f32,
    color: [u8; 3],
    bold: bool,
    halo: bool,
    tracking: f32,
    anchor: Anchor,
    min_scale: f32,
    /// Lower is painted earlier (so under) and placed first when two names collide.
    rank: u8,
    /// A GM's own label: always drawn, never moved aside for a generated name.
    user: bool,
    /// Never wider than this many cells (a region's name stays inside the region).
    fit_width_cells: Option<f32>,
}

fn polygon_centroid(pts: &[[f32; 2]]) -> ([f32; 2], f32) {
    let (mut a, mut cx, mut cy) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        let cross = f64::from(p[0]) * f64::from(q[1]) - f64::from(q[0]) * f64::from(p[1]);
        a += cross;
        cx += (f64::from(p[0]) + f64::from(q[0])) * cross;
        cy += (f64::from(p[1]) + f64::from(q[1])) * cross;
    }
    if a.abs() < 1e-9 {
        let n = pts.len() as f32;
        return (
            [
                pts.iter().map(|p| p[0]).sum::<f32>() / n,
                pts.iter().map(|p| p[1]).sum::<f32>() / n,
            ],
            0.0,
        );
    }
    (
        [(cx / (3.0 * a)) as f32, (cy / (3.0 * a)) as f32],
        (a.abs() / 2.0) as f32,
    )
}

/// Every piece of text on the floor, in placement priority order: the GM's own
/// labels, then marker names, region names, river names, route names.
fn text_items(floor: &FloorDoc) -> Vec<TextItem> {
    let mut v = Vec::new();
    for l in &floor.labels {
        if l.text.trim().is_empty() {
            continue;
        }
        v.push(TextItem {
            text: l.text.clone(),
            size_cells: l.size as f32,
            color: parse_hex(&l.color),
            bold: l.bold,
            halo: l.halo,
            tracking: l.tracking,
            anchor: if l.path.is_empty() {
                Anchor::Straight {
                    x: l.x as f32,
                    y: l.y as f32,
                    rotation: l.rotation,
                    left: false,
                }
            } else {
                Anchor::Path(l.path.clone())
            },
            min_scale: 0.3,
            rank: 9,
            user: true,
            fit_width_cells: None,
        });
    }
    for m in &floor.markers {
        if m.label.trim().is_empty() {
            continue;
        }
        v.push(TextItem {
            text: m.label.clone(),
            size_cells: 0.5,
            color: [0x1b, 0x1b, 0x1b],
            bold: false,
            halo: true,
            tracking: 0.0,
            anchor: Anchor::Straight {
                x: m.x + 0.55,
                y: m.y,
                rotation: 0.0,
                left: true,
            },
            min_scale: 1.0,
            rank: 8,
            user: false,
            fit_width_cells: None,
        });
    }
    for r in &floor.regions {
        if r.name.trim().is_empty() {
            continue;
        }
        let (c, area) = polygon_centroid(&r.points);
        v.push(TextItem {
            text: r.name.clone(),
            size_cells: (0.16 * area.sqrt()).clamp(0.5, 2.4),
            color: ndcart_paint::shade(parse_hex(&r.color), 0.55),
            bold: true,
            halo: true,
            tracking: 0.3,
            anchor: Anchor::Straight {
                x: c[0],
                y: c[1],
                rotation: 0.0,
                left: false,
            },
            min_scale: 1.0,
            rank: 1,
            user: false,
            fit_width_cells: {
                let xs = r.points.iter().map(|p| p[0]);
                let (lo, hi) = (
                    xs.clone().fold(f32::MAX, f32::min),
                    xs.fold(f32::MIN, f32::max),
                );
                Some((hi - lo) * 0.9)
            },
        });
    }
    for r in &floor.rivers {
        if r.name.trim().is_empty() {
            continue;
        }
        v.push(TextItem {
            text: r.name.clone(),
            size_cells: (r.width * 0.65).clamp(0.35, 0.9),
            color: [0x1d, 0x3f, 0x66],
            bold: false,
            halo: true,
            tracking: 0.1,
            anchor: Anchor::Path(r.points.clone()),
            min_scale: 0.45,
            rank: 3,
            user: false,
            fit_width_cells: None,
        });
    }
    for r in &floor.routes {
        if r.name.trim().is_empty() {
            continue;
        }
        v.push(TextItem {
            text: r.name.clone(),
            size_cells: 0.38,
            color: ndcart_paint::shade(parse_hex(&r.color), 0.7),
            bold: false,
            halo: true,
            tracking: 0.1,
            anchor: Anchor::Path(r.drawn_points()),
            min_scale: 0.5,
            rank: 2,
            user: false,
            fit_width_cells: None,
        });
    }
    v
}

/// Typography: the floor's labels and the names of its markers, regions, rivers
/// and routes. Each is laid out in WORLD pixels, so collisions (generated names
/// yield to ones already placed) are decided the same way in every tile; each is
/// drawn into its own sprite and blitted at an integer offset, so a tile equals
/// the matching crop of the full render exactly. Text under 5 px is not drawn.
fn draw_labels(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    let items = text_items(floor);
    if items.is_empty() {
        return;
    }
    let s = ppc as f32;
    let (w, h) = (i64::from(pixmap.width()), i64::from(pixmap.height()));
    let mut taken: Vec<[f32; 4]> = Vec::new();
    type Drawn = (u8, usize, Vec<crate::text::Placed>, [f32; 4], f32); // rank, item, glyphs, box, size
    let mut drawn: Vec<Drawn> = Vec::new();
    for (i, it) in items.iter().enumerate() {
        // A generated name that does not fit where it wants to go is tried smaller
        // before it is dropped; a GM's own label is always placed at full size.
        let tries: &[f32] = if it.user { &[1.0] } else { &[1.0, 0.8, 0.65] };
        let mut chosen = None;
        for &shrink in tries {
            let mut size_px = it.size_cells * s * shrink;
            if let Some(max_w) = it.fit_width_cells {
                let natural = crate::text::layout(&it.text, it.bold, size_px, it.tracking).width;
                if natural > max_w * s && natural > 0.0 {
                    size_px *= max_w * s / natural; // shrink to the allowed width
                }
            }
            if size_px < 5.0 {
                continue;
            }
            let lay = crate::text::layout(&it.text, it.bold, size_px, it.tracking);
            let placed = match &it.anchor {
                Anchor::Straight {
                    x,
                    y,
                    rotation,
                    left,
                } => crate::text::place_straight(&lay, x * s, y * s, *rotation, *left),
                Anchor::Path(pts) => {
                    let px: Vec<(f32, f32)> = pts.iter().map(|p| (p[0] * s, p[1] * s)).collect();
                    match crate::text::place_along(&lay, &px, it.min_scale) {
                        Some(p) => p,
                        None => continue,
                    }
                }
            };
            let Some(b) = crate::text::bounds(&placed, size_px, size_px * 0.2) else {
                continue;
            };
            let overlaps = |o: &[f32; 4]| b[0] < o[2] && b[2] > o[0] && b[1] < o[3] && b[3] > o[1];
            if it.user || !taken.iter().any(overlaps) {
                chosen = Some((placed, b, size_px));
                break;
            }
        }
        let Some((placed, b, size_px)) = chosen else {
            continue;
        };
        taken.push(b);
        drawn.push((it.rank, i, placed, b, size_px));
    }
    drawn.sort_by_key(|(rank, i, ..)| (*rank, *i));
    for (_, i, placed, b, size_px) in drawn {
        let it = &items[i];
        let (ox, oy) = (b[0].floor() as i64, b[1].floor() as i64);
        let (sw, sh) = (
            (b[2].ceil() as i64 - ox + 2).max(1),
            (b[3].ceil() as i64 - oy + 2).max(1),
        );
        if ox + sw < x0 || oy + sh < y0 || ox > x0 + w || oy > y0 + h || sw * sh > MAX_SPRITE_PIXELS
        {
            continue;
        }
        let Some(mut sprite) = tiny_skia::Pixmap::new(sw as u32, sh as u32) else {
            continue;
        };
        let local: Vec<crate::text::Placed> = placed
            .iter()
            .map(|p| crate::text::Placed {
                x: p.x - ox as f32,
                y: p.y - oy as f32,
                ..p.clone()
            })
            .collect();
        crate::text::draw(&mut sprite, &local, it.bold, size_px, it.color, it.halo);
        pixmap.draw_pixmap(
            (ox - x0) as i32,
            (oy - y0) as i32,
            sprite.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
    }
}

/// Shade the heightmap into the region: every pixel is a pure function of its
/// world position, so tiles and the full render agree exactly.
fn draw_terrain(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    use ndcart_core::terrain::RES;
    let Some(t) = &floor.terrain else { return };
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    let k = RES as f32 / ppc as f32; // samples per pixel
                                     // Margin: bilinear (+1) around a central difference (+1) around the pixel.
    let (sx0, sy0) = (
        (x0 as f32 * k).floor() as i32 - 4,
        (y0 as f32 * k).floor() as i32 - 4,
    );
    let (sw, sh) = (
        (w as f32 * k).ceil() as i32 + 9,
        (h as f32 * k).ceil() as i32 + 9,
    );
    let grid = t.extract(sx0, sy0, sw, sh);
    let shader = ndcart_terrain::Shader::new(&grid, &t.style, PAINT_SEED);
    let stride = w as usize * 4;
    let data = pixmap.data_mut();
    for py in 0..h {
        let sv = ((y0 + i64::from(py)) as f32 + 0.5) * k;
        for px in 0..w {
            let su = ((x0 + i64::from(px)) as f32 + 0.5) * k;
            let c = shader.color(su, sv);
            let i = py as usize * stride + px as usize * 4;
            data[i] = c[0];
            data[i + 1] = c[1];
            data[i + 2] = c[2];
            data[i + 3] = 0xff;
        }
    }
}

/// Rivers are variable-width soft strokes (the brush engine again): the width
/// tapers from a third at the source to full at the mouth.
fn draw_rivers(pixmap: &mut tiny_skia::Pixmap, floor: &FloorDoc, ppc: i64, x0: i64, y0: i64) {
    if floor.rivers.is_empty() {
        return;
    }
    let scale = ppc as f32;
    let (w, h) = (i64::from(pixmap.width()), i64::from(pixmap.height()));
    let region = [
        x0 as f32 / scale,
        y0 as f32 / scale,
        (x0 + w) as f32 / scale,
        (y0 + h) as f32 / scale,
    ];
    let stride = w as usize * 4;
    for r in &floor.rivers {
        let n = r.points.len();
        let stroke = ndcart_core::Stroke {
            id: r.id.clone(),
            kind: ndcart_core::StrokeKind::Paint,
            brush: ndcart_core::BrushKind::Round,
            material: None,
            color: "#4f8fc2".into(),
            size: r.width,
            opacity: 1.0,
            hardness: 0.55,
            roughness: 0.15,
            density: 0.5,
            seed: 0x7213,
            points: r
                .points
                .iter()
                .enumerate()
                .map(|(i, p)| ndcart_core::StrokePoint {
                    x: p[0],
                    y: p[1],
                    pressure: i as f32 / (n - 1) as f32,
                    tilt: 0.0,
                })
                .collect(),
        };
        let b = stroke.point_bounds();
        if b[0] - r.width > region[2]
            || b[2] + r.width < region[0]
            || b[1] - r.width > region[3]
            || b[3] + r.width < region[1]
        {
            continue;
        }
        let p = ndcart_paint::Prepared::new(&stroke);
        if !p.touches(region) {
            continue;
        }
        let px0 = ((p.bounds[0] * scale).floor() as i64 - x0).clamp(0, w);
        let px1 = ((p.bounds[2] * scale).ceil() as i64 - x0 + 1).clamp(0, w);
        let py0 = ((p.bounds[1] * scale).floor() as i64 - y0).clamp(0, h);
        let py1 = ((p.bounds[3] * scale).ceil() as i64 - y0 + 1).clamp(0, h);
        let data = pixmap.data_mut();
        for py in py0..py1 {
            for pxl in px0..px1 {
                let (u, v) = (
                    ((x0 + pxl) as f32 + 0.5) / scale,
                    ((y0 + py) as f32 + 0.5) / scale,
                );
                let cov = p.coverage(u, v);
                if cov <= 0.0 {
                    continue;
                }
                let i = py as usize * stride + pxl as usize * 4;
                // Darker in the middle of the channel, lighter toward the banks.
                let water = ndcart_paint::mix([0x3f, 0x7c, 0xb0], [0x6f, 0xaa, 0xd2], 1.0 - cov);
                let c = ndcart_paint::mix([data[i], data[i + 1], data[i + 2]], water, cov * 0.92);
                data[i] = c[0];
                data[i + 1] = c[1];
                data[i + 2] = c[2];
            }
        }
    }
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
        let surface = surface_for(s.material.as_deref(), &s.color);
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

/// What the table sees from one viewpoint: the floor as the players get it
/// (secrets already stripped by the caller), under fog. Everything outside the
/// viewer's line of sight is black; with `darkness` on, what the viewer can
/// see but no lamp reaches is only dimly shown. Lamps are blocked by walls too.
pub fn bake_preview(
    floor: &FloorDoc,
    cell_size: f64,
    ppc: i64,
    viewer: [f64; 2],
    radius: f64,
    darkness: bool,
) -> Result<(Vec<u8>, u32, u32), String> {
    use tiny_skia::{BlendMode, FillRule, Mask, Paint, PathBuilder, Transform};
    let walls = derive_walls(floor, cell_size);
    let (cols, rows) = grid_extent(floor);
    let (w, h) = (i64::from(cols) * ppc, i64::from(rows) * ppc);
    if !(1..=64).contains(&ppc) || w > MAX_SIDE_PX || h > MAX_SIDE_PX || w * h > MAX_FULL_PX {
        return Err(format!(
            "preview {w}x{h}px is outside the limits (ppc 1-64)"
        ));
    }
    let mut base = render_pixmap(floor, &walls, cell_size, ppc, 0, 0, w as u32, h as u32);
    let segs = ndcart_core::visibility::blockers(&walls, cell_size);
    let s = ppc as f32;
    let poly = |pts: &[[f64; 2]]| {
        let mut pb = PathBuilder::new();
        for (i, p) in pts.iter().enumerate() {
            let (x, y) = (p[0] as f32 * s, p[1] as f32 * s);
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        pb.close();
        pb.finish()
    };
    let (fw, fh) = (base.width(), base.height());
    let mut fog = tiny_skia::Pixmap::new(fw, fh).ok_or("preview too large")?;
    let mut black = Paint::default();
    black.set_color_rgba8(0, 0, 0, 240);
    let rect = tiny_skia::Rect::from_xywh(0.0, 0.0, fw as f32, fh as f32).ok_or("empty preview")?;
    fog.fill_rect(rect, &black, Transform::identity(), None);
    let mut cut = |alpha: u8, path: &tiny_skia::Path, mask: Option<&Mask>| {
        let mut p = Paint::default();
        p.set_color_rgba8(0, 0, 0, alpha);
        p.blend_mode = BlendMode::DestinationOut;
        p.anti_alias = true;
        fog.fill_path(path, &p, FillRule::Winding, Transform::identity(), mask);
    };
    let sight = visible_or_empty(&poly, viewer, &segs, radius);
    if let Some(sight_path) = sight {
        if darkness {
            cut(150, &sight_path, None);
            let mut mask = Mask::new(fw, fh).ok_or("preview too large")?;
            mask.fill_path(&sight_path, FillRule::Winding, true, Transform::identity());
            for l in floor.lights.iter().filter(|l| l.on) {
                let o = [l.x / cell_size, l.y / cell_size];
                let lp = ndcart_core::visibility::visible_polygon(o, &segs, l.range.max(0.5));
                if let Some(path) = poly(&lp) {
                    cut(255, &path, Some(&mask));
                }
            }
        } else {
            cut(255, &sight_path, None);
        }
    }
    base.draw_pixmap(
        0,
        0,
        fog.as_ref(),
        &tiny_skia::PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    let png = encode(&base, ImageFormat::Png, 90)?;
    Ok((png, fw, fh))
}

fn visible_or_empty(
    poly: &dyn Fn(&[[f64; 2]]) -> Option<tiny_skia::Path>,
    viewer: [f64; 2],
    segs: &[ndcart_core::visibility::Seg],
    radius: f64,
) -> Option<tiny_skia::Path> {
    poly(&ndcart_core::visibility::visible_polygon(
        viewer, segs, radius,
    ))
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
    render_pixmap_onion(floor, walls, cell_size, ppc, x0, y0, w, h, None)
}

/// How strongly the neighbouring floor shows through (the "onion skin").
const ONION_OPACITY: f32 = 0.3;

/// [`render_pixmap`] with an optional second floor drawn faintly *underneath*, so
/// stairs and walls on the floor below line up with what is being edited. The
/// other floor is rendered whole (rooms, strokes, props, text) and composited at
/// [`ONION_OPACITY`] before the current floor is drawn over it.
#[allow(clippy::too_many_arguments)]
fn render_pixmap_onion(
    floor: &FloorDoc,
    walls: &[Wall],
    cell_size: f64,
    ppc: i64,
    x0: i64,
    y0: i64,
    w: u32,
    h: u32,
    onion: Option<(&FloorDoc, &[Wall])>,
) -> tiny_skia::Pixmap {
    let tile_px_w = i64::from(w);
    let tile_px_h = i64::from(h);
    let k = ppc as f32 / cell_size.max(1.0) as f32; // wall pixels -> image pixels

    let mut pixmap = tiny_skia::Pixmap::new(w, h).expect("pixmap");
    pixmap.fill(tiny_skia::Color::from_rgba8(
        VOID[0], VOID[1], VOID[2], 0xff,
    ));

    if let Some((other, other_walls)) = onion {
        let ghost = render_pixmap(other, other_walls, cell_size, ppc, x0, y0, w, h);
        pixmap.draw_pixmap(
            0,
            0,
            ghost.as_ref(),
            &tiny_skia::PixmapPaint {
                opacity: ONION_OPACITY,
                ..Default::default()
            },
            tiny_skia::Transform::identity(),
            None,
        );
    }

    // Cell range intersecting the tile (in cell coordinates).
    let cx_from = x0.div_euclid(ppc);
    let cx_to = (x0 + tile_px_w - 1).div_euclid(ppc);
    let cy_from = y0.div_euclid(ppc);
    let cy_to = (y0 + tile_px_h - 1).div_euclid(ppc);

    draw_terrain(&mut pixmap, floor, ppc, x0, y0);
    draw_rivers(&mut pixmap, floor, ppc, x0, y0);
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
        let own = surfaces.get(space_id).cloned().unwrap_or(FALLBACK);
        // The four neighbours' surfaces, only when painted and different.
        let nb = |c: CellKey| {
            floor
                .grid
                .cell_space(c)
                .and_then(|id| surfaces.get(id).cloned())
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
                    (west.as_ref(), fx, Side::Far)
                } else {
                    (east.as_ref(), 1.0 - fx, Side::Near)
                };
                if let Some(n) = hn.filter(|_| hd < BLEND_WIDTH) {
                    c = edge_mix(c, n.sample(u, v), hd, hs, u, v, PAINT_SEED);
                }
                let (vn, vd, vs) = if fy < 0.5 {
                    (north.as_ref(), fy, Side::Far)
                } else {
                    (south.as_ref(), 1.0 - fy, Side::Near)
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
    draw_overlay(&mut pixmap, floor, ppc, x0, y0);
    draw_map_grid(&mut pixmap, floor, ppc, x0, y0);
    draw_labels(&mut pixmap, floor, ppc, x0, y0);

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

    fn two_rooms() -> FloorDoc {
        use ndcart_core::grid::{default_space, Edge, EdgeKey, Mark, MarkKind};
        let mut f = FloorDoc::new("f".into(), "G".into());
        for (id, name) in [("a", "West"), ("b", "East")] {
            let mut sp = default_space(id, name, SpaceKind::Room);
            sp.color = "#d0d0d0".into();
            f.grid.upsert_space(sp).unwrap();
        }
        let west: Vec<CellKey> = (0..6)
            .flat_map(|x| (0..6).map(move |y| CellKey::new(x, y)))
            .collect();
        let east: Vec<CellKey> = (6..12)
            .flat_map(|x| (0..6).map(move |y| CellKey::new(x, y)))
            .collect();
        f.grid.paint(&west, Some("a")).unwrap();
        f.grid.paint(&east, Some("b")).unwrap();
        // A closed door between them, high up (y = 0); everything else is wall.
        f.grid
            .set_mark(
                EdgeKey {
                    cell: CellKey::new(5, 0),
                    edge: Edge::East,
                },
                Some(Mark::new(MarkKind::Door)),
            )
            .unwrap();
        f
    }

    #[test]
    fn the_preview_hides_what_a_closed_door_and_walls_hide() {
        let f = two_rooms();
        let lum = |pm: &tiny_skia::Pixmap, cx: f32, cy: f32| {
            let c = rgb(pm, (cx * 16.0) as u32, (cy * 16.0) as u32);
            u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2])
        };
        let png = |darkness| {
            decode(
                &bake_preview(&f, 32.0, 16, [2.5, 3.5], 30.0, darkness)
                    .unwrap()
                    .0,
            )
        };
        let lit = png(false);
        assert!(lum(&lit, 3.0, 3.0) > 300, "own room visible");
        assert!(
            lum(&lit, 9.0, 3.0) < 60,
            "the room behind the closed door is black"
        );
        // With the lamp-less darkness rule, the same room is only dimly shown.
        let dark = png(true);
        let (a, b) = (lum(&dark, 3.0, 3.0), lum(&lit, 3.0, 3.0));
        assert!(a < b && a > 40, "dim, not black or full: {a} vs {b}");
        // Open the door: the east room comes into view through the doorway line.
        let mut open = f.clone();
        open.grid
            .set_mark(
                ndcart_core::grid::EdgeKey {
                    cell: CellKey::new(5, 0),
                    edge: ndcart_core::grid::Edge::East,
                },
                Some(ndcart_core::grid::Mark {
                    kind: ndcart_core::grid::MarkKind::Door,
                    state: ndcart_core::grid::MarkState::Open,
                }),
            )
            .unwrap();
        let seen = decode(
            &bake_preview(&open, 32.0, 16, [5.0, 0.6], 30.0, false)
                .unwrap()
                .0,
        );
        assert!(lum(&seen, 8.0, 0.5) > 300, "visible through the open door");
    }

    #[test]
    fn a_lamp_lights_its_own_room_only() {
        let mut f = two_rooms();
        // Open the door so both rooms are in sight, with a lamp only in the east room.
        f.grid
            .set_mark(
                ndcart_core::grid::EdgeKey {
                    cell: CellKey::new(5, 0),
                    edge: ndcart_core::grid::Edge::East,
                },
                Some(ndcart_core::grid::Mark {
                    kind: ndcart_core::grid::MarkKind::Door,
                    state: ndcart_core::grid::MarkState::Open,
                }),
            )
            .unwrap();
        f.lights.push(ndcart_core::Light {
            id: "l".into(),
            x: 9.5 * 32.0,
            y: 3.0 * 32.0,
            range: 2.5,
            color: "#ffaa00".into(),
            intensity: 0.9,
            on: true,
            label: None,
        });
        let pm = decode(
            &bake_preview(&f, 32.0, 16, [5.0, 0.6], 30.0, true)
                .unwrap()
                .0,
        );
        let lum = |cx: f32, cy: f32| {
            let c = rgb(&pm, (cx * 16.0) as u32, (cy * 16.0) as u32);
            u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2])
        };
        // The lamp's pool: east room near the lamp is bright where the viewer can see it.
        assert!(
            lum(8.5, 0.8) > lum(11.0, 5.0),
            "{} vs {}",
            lum(8.5, 0.8),
            lum(11.0, 5.0)
        );
        assert!(bake_preview(&f, 32.0, 100, [1.0, 1.0], 5.0, true).is_err());
    }

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
    fn terrain_and_rivers_bake_the_same_in_tiles_and_in_the_full_render() {
        let g = ndcart_terrain::generate(&ndcart_terrain::GenParams {
            seed: 5,
            cols: 24,
            rows: 20,
            land: 0.5,
            continent: 0.5,
            ridges: 0.6,
            rivers: 3,
        });
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.set_canvas(Some([24, 20])).unwrap();
        f.terrain_set(
            g.terrain.fill,
            &g.terrain
                .chunk_keys()
                .into_iter()
                .map(|(a, b)| g.terrain.snapshot_chunk(a, b))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        f.set_terrain_style(g.terrain.style.clone()).unwrap();
        for r in g.rivers {
            f.add_river(r, false, None).unwrap();
        }
        let (png, w, _) = bake_full_stroked(&f, 64);
        assert!(w >= 24 * 64);
        let full = decode(&png);
        for (tx, ty) in [(0, 0), (1, 0), (2, 1), (3, 2), (5, 4)] {
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

    fn overlay_floor() -> FloorDoc {
        use ndcart_core::overlay::{Marker, OverlayItem as I, Region, Route};
        let mut f = light_floor(); // 8x8 light-grey room
        f.put_overlay(
            I::Region(Region {
                id: "r".into(),
                name: String::new(), // unnamed: the old probes measure the fill, not text
                points: vec![[1.0, 1.0], [4.0, 1.0], [4.0, 4.0], [1.0, 4.0]],
                color: "#c0504d".into(),
                opacity: 0.5,
                hatch: false,
            }),
            None,
        )
        .unwrap();
        f.put_overlay(
            I::Route(Route {
                id: "t".into(),
                name: "Road".into(),
                points: vec![[0.5, 6.0], [7.5, 6.0]],
                color: "#202020".into(),
                width: 0.2,
                dashed: true,
                smooth: false,
            }),
            None,
        )
        .unwrap();
        f.put_overlay(
            I::Marker(Marker {
                id: "m".into(),
                x: 6.0,
                y: 2.0,
                label: "Port".into(),
                note: String::new(),
                color: "#e8b030".into(),
                link_schematic: None,
                links_to: None,
            }),
            None,
        )
        .unwrap();
        f
    }

    #[test]
    fn regions_routes_and_markers_draw_where_they_say() {
        let f = overlay_floor();
        let pm = bake_stamped(&f, 32);
        let clear = bake_stamped(&light_floor(), 32);
        // The region tints its interior (50% of #c0504d over #d0d0d0) and not outside it.
        let inside = rgb(&pm, 2 * 32 + 16, 2 * 32 + 16);
        assert!(inside[0] > inside[1] + 30, "reddish: {inside:?}");
        assert_eq!(
            rgb(&pm, 6 * 32, 2 * 32 + 3 * 32 - 40),
            rgb(&clear, 6 * 32, 2 * 32 + 3 * 32 - 40),
            "outside is untouched"
        );
        // The dashed route leaves gaps: along its line some pixels are dark, some are the floor.
        let row = 6 * 32;
        let samples: Vec<[u8; 3]> = (40..230).map(|x| rgb(&pm, x, row)).collect();
        let dark = samples.iter().filter(|c| c[0] < 90).count();
        let light = samples.iter().filter(|c| c[0] > 190).count();
        assert!(
            dark > 20 && light > 20,
            "dashed: {dark} dark, {light} light"
        );
        // The marker pin: gold body, with a darker core at the centre.
        // the pin is 9.6 px in radius at 32 px/cell and its dark core 3.4 px
        let body = rgb(&pm, 6 * 32 + 6, 2 * 32);
        assert!(body[0] > 200 && body[2] < 90, "gold: {body:?}");
        let core = rgb(&pm, 6 * 32, 2 * 32);
        assert!(core[0] < body[0], "darker core: {core:?}");
    }

    #[test]
    fn hatching_adds_lines_inside_the_region_only() {
        use ndcart_core::overlay::{OverlayItem as I, Region};
        let mk = |hatch: bool| {
            let mut f = light_floor();
            f.put_overlay(
                I::Region(Region {
                    id: "r".into(),
                    name: "R".into(),
                    points: vec![[1.0, 1.0], [5.0, 1.0], [5.0, 5.0], [1.0, 5.0]],
                    color: "#3050c0".into(),
                    opacity: 0.2,
                    hatch,
                }),
                None,
            )
            .unwrap();
            bake_stamped(&f, 32)
        };
        let (plain, hatched) = (mk(false), mk(true));
        let (mut differing_inside, mut differing_outside) = (0, 0);
        for y in 0..256u32 {
            for x in 0..256u32 {
                if plain.pixel(x, y) != hatched.pixel(x, y) {
                    // The polygon spans 32..160 px. Its border band (a few px either side) is
                    // where the anti-aliased clip meets the line ends, so judge "inside" by the
                    // interior and "outside" by clear air beyond a margin.
                    let inside = (34..158).contains(&x) && (34..158).contains(&y);
                    let clear_outside = !(28..164).contains(&x) || !(28..164).contains(&y);
                    if !inside && !clear_outside {
                        continue;
                    }
                    if inside {
                        differing_inside += 1
                    } else {
                        differing_outside += 1
                    }
                }
            }
        }
        assert!(differing_inside > 400, "lines inside: {differing_inside}");
        assert_eq!(
            differing_outside, 0,
            "no hatching outside the polygon (border pixels aside)"
        );
    }

    #[test]
    fn overlays_bake_the_same_in_tiles_and_in_the_full_render() {
        let f = overlay_floor();
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
            assert_eq!(worst, 0, "tile ({tx},{ty}) differs by {worst}");
        }
    }

    fn grid_floor(kind: ndcart_core::MapGridKind, size: f32) -> FloorDoc {
        let mut f = light_floor();
        f.set_map_grid(Some(ndcart_core::MapGrid {
            kind,
            size,
            color: "#000000".into(),
            opacity: 0.8,
        }))
        .unwrap();
        f
    }

    #[test]
    fn a_map_grid_bakes_the_same_in_tiles_and_in_the_full_render() {
        use ndcart_core::MapGridKind::{Hex, Square};
        // Square lines are axis-aligned: exactly equal. Hex edges are diagonal and
        // cross tile borders, where the rasterizer's fixed-point edge walk can tip a
        // pixel's coverage by one sample (a few per tile, at most 1/16 of the ink).
        for (f, exact) in [
            (grid_floor(Square, 1.5), true),
            (grid_floor(Hex, 2.0), false),
            (grid_floor(Hex, 0.75), false),
        ] {
            let (png, ..) = bake_full_stroked(&f, 64);
            let full = decode(&png);
            for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let tile = decode(&bake_tile(&f, &[], 32.0, 2, tx, ty));
                let (mut differing, mut worst) = (0usize, 0u8);
                for y in 0..256u32 {
                    for x in 0..256u32 {
                        let (a, b) = (
                            rgb(&tile, x, y),
                            rgb(&full, tx as u32 * 256 + x, ty as u32 * 256 + y),
                        );
                        if a != b {
                            differing += 1;
                            worst = worst.max(a[0].abs_diff(b[0]));
                        }
                    }
                }
                if exact {
                    assert_eq!(differing, 0, "{:?} tile ({tx},{ty})", f.map_grid);
                } else {
                    assert!(
                        differing <= 40 && worst <= 24,
                        "{:?} tile ({tx},{ty}): {differing} pixels, worst {worst}",
                        f.map_grid
                    );
                }
            }
        }
    }

    #[test]
    fn a_smooth_route_curves_through_its_points_and_still_tiles_exactly() {
        use ndcart_core::overlay::{OverlayItem as I, Route};
        let mk = |smooth: bool| {
            let mut f = light_floor();
            f.put_overlay(
                I::Route(Route {
                    id: "r".into(),
                    name: String::new(),
                    points: vec![[0.5, 4.0], [3.0, 1.0], [5.0, 6.5], [7.5, 3.0]],
                    color: "#000000".into(),
                    width: 0.2,
                    dashed: false,
                    smooth,
                }),
                None,
            )
            .unwrap();
            f
        };
        let (straight, curved) = (mk(false), mk(true));
        assert_ne!(
            bake_stamped(&straight, 32).data(),
            bake_stamped(&curved, 32).data()
        );
        // A curve is many small segments with round joins; where it crosses a tile
        // border the rasterizer clips and re-flattens those curves, so a handful of
        // anti-aliased edge pixels may differ in coverage. Everything else is exact.
        let (png, ..) = bake_full_stroked(&curved, 64);
        let full = decode(&png);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let tile = decode(&bake_tile(&curved, &[], 32.0, 2, tx, ty));
            let (mut differing, mut worst) = (0usize, 0u8);
            for y in 0..256u32 {
                for x in 0..256u32 {
                    let (a, b) = (
                        rgb(&tile, x, y),
                        rgb(&full, tx as u32 * 256 + x, ty as u32 * 256 + y),
                    );
                    if a != b {
                        differing += 1;
                        worst = worst.max(a[0].abs_diff(b[0]));
                    }
                }
            }
            assert!(
                differing <= 40 && worst <= 32,
                "tile ({tx},{ty}): {differing} pixels, worst {worst}"
            );
        }
    }

    #[test]
    fn a_map_grid_draws_lines_where_it_says_and_not_without_it() {
        use ndcart_core::MapGridKind::{Hex, Square};
        let plain = bake_stamped(&light_floor(), 32);
        let sq = bake_stamped(&grid_floor(Square, 2.0), 32);
        // A vertical line at x = 2 cells (64 px) is dark; the middle of a square is not.
        assert!(rgb(&sq, 64, 20)[0] < 120, "{:?}", rgb(&sq, 64, 20));
        assert_eq!(rgb(&sq, 32, 32), rgb(&plain, 32, 32));
        let hx = bake_stamped(&grid_floor(Hex, 2.0), 32);
        let dark = |pm: &tiny_skia::Pixmap| {
            (0..pm.height())
                .flat_map(|y| (0..pm.width()).map(move |x| (x, y)))
                .filter(|(x, y)| rgb(pm, *x, *y)[0] < 120)
                .count()
        };
        let base = dark(&plain);
        assert!(
            dark(&sq) > base + 300 && dark(&hx) > base + 300,
            "{base} {} {}",
            dark(&sq),
            dark(&hx)
        );
    }

    /// Register a custom material/stamp from a generated picture; returns its id.
    fn custom_from(kind: &str, pm: &tiny_skia::Pixmap, size: f32) -> String {
        let a = crate::assets::global();
        let asset = a.put(&pm.encode_png().unwrap()).unwrap();
        let id = format!(
            "{}_{}",
            if kind == "material" { "mat" } else { "stp" },
            &asset[..12]
        );
        a.register(crate::assets::Custom {
            id: id.clone(),
            name: "test".into(),
            kind: kind.into(),
            asset,
            size,
            tags: Vec::new(),
            origin: crate::assets::Provenance {
                source: "imported".into(),
                prompt: String::new(),
                seed: -1,
                date: "now".into(),
            },
        })
        .unwrap();
        id
    }

    #[test]
    fn a_picture_material_and_a_picture_stamp_paint_and_tile_exactly() {
        // A 4x4 picture: red/blue checks, repeated every 2 cells.
        let mut pic = tiny_skia::Pixmap::new(4, 4).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let c = if (x / 2 + y / 2) % 2 == 0 {
                    (200, 20, 20)
                } else {
                    (20, 20, 200)
                };
                pic.pixels_mut()[y * 4 + x] =
                    tiny_skia::ColorU8::from_rgba(c.0, c.1, c.2, 255).premultiply();
            }
        }
        let mat = custom_from("material", &pic, 2.0);
        // A prop: a green disc with a transparent corner.
        let mut prop = tiny_skia::Pixmap::new(16, 16).unwrap();
        let mut pb = tiny_skia::PathBuilder::new();
        pb.push_circle(8.0, 8.0, 7.0);
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(20, 180, 20, 255);
        prop.fill_path(
            &pb.finish().unwrap(),
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
        let stp = custom_from("stamp", &prop, 2.0);

        let mut f = light_floor();
        f.grid
            .upsert_space(ndcart_core::grid::Space {
                id: "a".into(),
                name: "A".into(),
                color: "#d0d0d0".into(),
                kind: SpaceKind::Room,
                material: Some(mat.clone()),
            })
            .unwrap();
        f.add_stamp(mk_stamp("p", &stp, 3.5, 3.5), None).unwrap();
        let pm = bake_stamped(&f, 32);
        // 2 cells = 64 px per repeat of 4 picture px: the first 32 px are red, the next blue.
        let c = rgb(&pm, 8, 8);
        assert!(c[0] > 150 && c[2] < 80, "red check: {c:?}");
        let c = rgb(&pm, 40, 8);
        assert!(c[2] > 150 && c[0] < 80, "blue check: {c:?}");
        // The prop is green at its centre (3.5 cells = 112 px), and the ground shows beyond it.
        let g = rgb(&pm, 112, 112);
        assert!(g[1] > 120 && g[0] < 90, "green prop: {g:?}");
        // Tiles equal the crop of the full render.
        let (png, ..) = bake_full_stroked(&f, 64);
        let full = decode(&png);
        for (tx, ty) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let tile = decode(&bake_tile(&f, &[], 32.0, 2, tx, ty));
            for y in 0..256u32 {
                for x in 0..256u32 {
                    assert_eq!(
                        rgb(&tile, x, y),
                        rgb(&full, tx as u32 * 256 + x, ty as u32 * 256 + y),
                        "tile ({tx},{ty}) at ({x},{y})"
                    );
                }
            }
        }
        // Without its registry entry a custom id falls back to the flat colour / draws nothing.
        let mut g2 = light_floor();
        g2.grid
            .upsert_space(ndcart_core::grid::Space {
                id: "a".into(),
                name: "A".into(),
                color: "#d0d0d0".into(),
                kind: SpaceKind::Room,
                material: Some("mat_000000000000".into()),
            })
            .unwrap();
        g2.add_stamp(mk_stamp("q", "stp_000000000000", 3.5, 3.5), None)
            .unwrap();
        let flat = bake_stamped(&g2, 32);
        assert_eq!(rgb(&flat, 112, 112), [0xd0, 0xd0, 0xd0]);
    }

    /// Count pixels close to `want` inside a pixel rectangle `[x0, y0, x1, y1)`.
    fn ink(pm: &tiny_skia::Pixmap, want: [u8; 3], r: [u32; 4]) -> usize {
        let mut n = 0;
        for y in r[1]..r[3].min(pm.height()) {
            for x in r[0]..r[2].min(pm.width()) {
                let c = rgb(pm, x, y);
                if (0..3).all(|k| c[k].abs_diff(want[k]) < 40) {
                    n += 1;
                }
            }
        }
        n
    }

    fn red_label(id: &str, x: f64, y: f64, text: &str) -> ndcart_core::Label {
        let mut l = ndcart_core::Label::new(id, x, y, text);
        l.color = "#c00000".into();
        l.size = 1.2;
        l.bold = true;
        l
    }

    #[test]
    fn a_straight_label_is_drawn_at_its_anchor_with_a_halo_and_nowhere_else() {
        let mut f = light_floor(); // 8x8 light-grey room
        f.upsert_label(red_label("t", 4.0, 3.0, "Hue")).unwrap();
        let pm = bake_stamped(&f, 48);
        // Text is centred on (4, 3) cells = (192, 144) px; ~1.2 cells high = 58 px.
        assert!(
            ink(&pm, [0xc0, 0, 0], [150, 110, 240, 180]) > 200,
            "the letters are there"
        );
        assert_eq!(
            ink(&pm, [0xc0, 0, 0], [0, 0, 100, 100]),
            0,
            "and not elsewhere"
        );
        // A dark-red label gets a light halo: near-white pixels appear around the letters.
        let halo = ink(&pm, [0xfd, 0xfa, 0xef], [150, 110, 240, 180]);
        assert!(halo > 80, "halo pixels: {halo}");
        let mut plain = light_floor();
        let mut l = red_label("t", 4.0, 3.0, "Hue");
        l.halo = false;
        plain.upsert_label(l).unwrap();
        assert!(
            ink(
                &bake_stamped(&plain, 48),
                [0xfd, 0xfa, 0xef],
                [150, 110, 240, 180]
            ) < halo,
            "halo: false draws none"
        );
        // Empty text draws nothing at all.
        let mut e = light_floor();
        e.upsert_label(red_label("t", 4.0, 3.0, "  ")).unwrap();
        assert_eq!(
            bake_stamped(&e, 48).data(),
            bake_stamped(&light_floor(), 48).data()
        );
    }

    #[test]
    fn marker_names_region_names_and_path_names_appear_where_they_belong() {
        use ndcart_core::overlay::{Marker, OverlayItem as I, Region, Route};
        let mut f = light_floor();
        f.put_overlay(
            I::Marker(Marker {
                id: "m".into(),
                x: 2.0,
                y: 1.5,
                label: "Port".into(),
                note: String::new(),
                color: "#e8b030".into(),
                link_schematic: None,
                links_to: None,
            }),
            None,
        )
        .unwrap();
        f.put_overlay(
            I::Region(Region {
                id: "r".into(),
                name: "Kingdom".into(),
                points: vec![[3.0, 3.0], [7.0, 3.0], [7.0, 6.0], [3.0, 6.0]],
                color: "#8040c0".into(),
                opacity: 0.0,
                hatch: false,
            }),
            None,
        )
        .unwrap();
        f.put_overlay(
            I::Route(Route {
                id: "t".into(),
                name: "Road".into(),
                points: vec![[0.5, 7.5], [7.5, 7.5]],
                color: "#c00000".into(),
                width: 0.05,
                dashed: false,
                smooth: false,
            }),
            None,
        )
        .unwrap();
        let near = bake_stamped(&f, 48);
        // The marker name sits right of the pin (pin at x=96..., name starts ~0.55 cells right).
        let dark = [0x1b, 0x1b, 0x1b];
        assert!(
            ink(&near, dark, [120, 56, 220, 92]) > 60,
            "marker name right of the pin"
        );
        assert_eq!(ink(&near, dark, [0, 56, 60, 92]), 0, "none to its left");
        // The region name is centred in the polygon (centre 5, 4.5 cells = 240, 216).
        let purple = ndcart_paint::shade([0x80, 0x40, 0xc0], 0.55);
        assert!(
            ink(&near, purple, [170, 190, 310, 240]) > 80,
            "region name at the centroid"
        );
        // The route name follows the route: a reddish line along y = 7.5 cells (360 px).
        assert!(
            ink(
                &near,
                ndcart_paint::shade([0xc0, 0, 0], 0.7),
                [60, 330, 330, 380]
            ) > 40,
            "route name along the road"
        );
        // Zoomed far out the text would be unreadable, so it is not drawn at all.
        let far = bake_stamped(&f, 8);
        assert_eq!(
            // (inside the 8x8-cell floor; below it is void, which is also "dark")
            ink(&far, dark, [0, 0, 64, 64]),
            0,
            "marker text under 5 px is hidden"
        );
    }

    #[test]
    fn a_users_label_wins_a_collision_and_generated_names_yield() {
        use ndcart_core::overlay::{Marker, OverlayItem as I};
        let mk = |id: &str, y: f32, label: &str| {
            I::Marker(Marker {
                id: id.into(),
                x: 2.0,
                y,
                label: label.into(),
                note: String::new(),
                color: "#e8b030".into(),
                link_schematic: None,
                links_to: None,
            })
        };
        let mut f = light_floor();
        f.put_overlay(mk("a", 2.0, "Alpha"), None).unwrap();
        f.put_overlay(mk("b", 2.1, "Bravo"), None).unwrap(); // its name would sit on Alpha's
        let pm = bake_stamped(&f, 48);
        let dark = [0x1b, 0x1b, 0x1b];
        let both = ink(&pm, dark, [110, 70, 330, 140]);
        let mut only_a = light_floor();
        only_a.put_overlay(mk("a", 2.0, "Alpha"), None).unwrap();
        let alone = ink(&bake_stamped(&only_a, 48), dark, [110, 70, 330, 140]);
        assert!(
            both <= alone + 5,
            "the second name was skipped, not drawn over the first: {both} vs {alone}"
        );
        // A GM's own label in the same place is drawn, and pushes the marker names aside.
        f.upsert_label(red_label("mine", 4.0, 2.05, "Mine"))
            .unwrap();
        let pm = bake_stamped(&f, 48);
        assert!(
            ink(&pm, [0xc0, 0, 0], [150, 50, 270, 150]) > 150,
            "the user's label is always drawn"
        );
    }

    #[test]
    fn a_region_name_stays_inside_its_region_and_retries_smaller_beside_a_label() {
        use ndcart_core::overlay::{OverlayItem as I, Region};
        let purple = ndcart_paint::shade([0x80, 0x40, 0xc0], 0.55);
        let region = |pts: Vec<[f32; 2]>| {
            I::Region(Region {
                id: "r".into(),
                name: "Kingdom of Hue".into(),
                points: pts,
                color: "#8040c0".into(),
                opacity: 0.0,
                hatch: false,
            })
        };
        // A narrow region (3 cells wide at 48 px = 144 px): the long name is shrunk to fit it.
        let mut f = light_floor();
        f.put_overlay(
            region(vec![[2.0, 1.0], [5.0, 1.0], [5.0, 7.0], [2.0, 7.0]]),
            None,
        )
        .unwrap();
        let pm = bake_stamped(&f, 48);
        let (inside, left_of, right_of) = (
            ink(&pm, purple, [96, 0, 240, 384]),
            ink(&pm, purple, [0, 0, 80, 384]),
            ink(&pm, purple, [256, 0, 384, 384]),
        );
        assert!(inside > 150, "the name is drawn: {inside}");
        assert_eq!(
            (left_of, right_of),
            (0, 0),
            "and does not spill out of the region"
        );
        // A user label crowding the right end: the region name is still there, only smaller.
        let mut g = light_floor();
        g.put_overlay(
            region(vec![[1.0, 2.0], [7.0, 2.0], [7.0, 6.0], [1.0, 6.0]]),
            None,
        )
        .unwrap();
        let alone = ink(&bake_stamped(&g, 48), purple, [0, 0, 400, 400]);
        g.upsert_label(red_label("crowd", 6.2, 4.0, "Crowd"))
            .unwrap();
        let crowded = ink(&bake_stamped(&g, 48), purple, [0, 0, 400, 400]);
        assert!(
            crowded > 0,
            "the name survived the collision (retried smaller)"
        );
        assert!(crowded < alone, "…at a smaller size: {crowded} vs {alone}");
    }

    #[test]
    fn a_label_on_a_path_follows_it_and_reads_left_to_right() {
        let mut f = light_floor();
        let mut l = red_label("p", 0.0, 0.0, "Silverwater");
        l.size = 0.9;
        l.path = vec![[1.0, 6.5], [3.0, 5.0], [6.0, 2.0]]; // rising to the right
        f.upsert_label(l).unwrap();
        let pm = bake_stamped(&f, 48);
        // Ink hugs the path: lower left near (1..3, 6..5) and upper right near (5..6, 3..2),
        // and none in the opposite corners.
        assert!(
            ink(&pm, [0xc0, 0, 0], [40, 220, 190, 340]) > 40,
            "lower left of the path"
        );
        assert!(
            ink(&pm, [0xc0, 0, 0], [220, 90, 340, 190]) > 40,
            "upper right of the path"
        );
        assert_eq!(
            ink(&pm, [0xc0, 0, 0], [250, 250, 380, 380]),
            0,
            "none off the path"
        );
        // The same path given in the other direction reads identically (not mirrored).
        let mut g = light_floor();
        let mut r = red_label("p", 0.0, 0.0, "Silverwater");
        r.size = 0.9;
        r.path = vec![[6.0, 2.0], [3.0, 5.0], [1.0, 6.5]];
        g.upsert_label(r).unwrap();
        assert_eq!(
            bake_stamped(&g, 48).data(),
            pm.data(),
            "path direction does not change the text"
        );
    }

    #[test]
    fn typography_bakes_the_same_in_tiles_and_in_the_full_render() {
        use ndcart_core::overlay::{Marker, OverlayItem as I, Region};
        let mut f = light_floor();
        f.upsert_label(red_label("a", 2.2, 2.0, "Alpha Keep"))
            .unwrap();
        let mut p = red_label("b", 0.0, 0.0, "Winding river name");
        p.path = vec![[0.5, 7.0], [3.0, 4.5], [7.5, 5.0]];
        f.upsert_label(p).unwrap();
        f.put_overlay(
            I::Marker(Marker {
                id: "m".into(),
                x: 5.5,
                y: 1.5,
                label: "Port Neon".into(),
                note: String::new(),
                color: "#e8b030".into(),
                link_schematic: None,
                links_to: None,
            }),
            None,
        )
        .unwrap();
        f.put_overlay(
            I::Region(Region {
                id: "r".into(),
                name: "Hue".into(),
                points: vec![[1.0, 1.0], [7.0, 1.0], [7.0, 7.0]],
                color: "#3050c0".into(),
                opacity: 0.2,
                hatch: false,
            }),
            None,
        )
        .unwrap();
        let (png, ..) = bake_full_stroked(&f, 64);
        let full = decode(&png);
        // (only tiles that lie inside the image: Pixmap::pixel does not bounds-check x)
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
    fn the_floor_below_shows_faintly_underneath_and_the_current_floor_wins() {
        use ndcart_core::grid::default_space;
        let room = |name: &str, color: &str, x0: i32, x1: i32| {
            let mut f = FloorDoc::new(name.into(), name.into());
            let mut sp = default_space("a", "A", SpaceKind::Room);
            sp.color = color.into();
            f.grid.upsert_space(sp).unwrap();
            let cells: Vec<CellKey> = (x0..x1)
                .flat_map(|x| (0..4).map(move |y| CellKey::new(x, y)))
                .collect();
            f.grid.paint(&cells, Some("a")).unwrap();
            f
        };
        let upper = room("upper", "#d0d0d0", 0, 2); // the floor being edited: cells 0..2
        let below = room("below", "#ff0000", 2, 4); // the floor under it: cells 2..4
        let (wu, wb) = (derive_walls(&upper, 32.0), derive_walls(&below, 32.0));
        // zoom 0 is 16 px per cell: the room cells span x 0..32 and 32..64 of tile (0,0).
        let plain = decode(&bake_tile_onion(&upper, &wu, 32.0, 0, 0, 0, None));
        let ghosted = decode(&bake_tile_onion(
            &upper,
            &wu,
            32.0,
            0,
            0,
            0,
            Some((&below, &wb)),
        ));
        // Where only the lower floor has a room: void without onion, ~30% red with it.
        assert_eq!(rgb(&plain, 48, 32), VOID[..3]);
        let g = rgb(&ghosted, 48, 32);
        assert!(
            g[0] > VOID[0] + 40 && g[0] < 140 && g[1] < 60,
            "a faint red: {g:?}"
        );
        // Where the current floor has a room, it is exactly what it was.
        assert_eq!(rgb(&ghosted, 16, 32), rgb(&plain, 16, 32));
        assert_eq!(rgb(&ghosted, 16, 32), [0xd0, 0xd0, 0xd0]);
        // With no onion floor the bytes are the old tile's, exactly.
        assert_eq!(
            bake_tile_onion(&upper, &wu, 32.0, 0, 0, 0, None),
            bake_tile(&upper, &wu, 32.0, 0, 0, 0)
        );
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
