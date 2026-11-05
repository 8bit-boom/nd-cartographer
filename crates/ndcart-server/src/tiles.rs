//! Tile baking for M0: the floor's room grid rendered into 256px PNG tiles
//! on a zoom pyramid (z0 = 16 px/cell, doubling per level; the plan's
//! tiny-skia + rayon baker grows into `ndcart-render`; this module proves
//! the pipeline end to end and serves the browser's pan/zoom).
//!
//! Geometry only: occupied cells fill with their space color, derived walls
//! stroke on top, the background is the void color. Golden-tile tests arrive
//! with `ndcart-render` (plan §12); this module stays deterministic anyway.

use ndcart_core::walls::{walls_from_grid, Wall};
use ndcart_core::FloorDoc;

/// Pixels per cell at zoom 0. Zoom z doubles it (z capped at 4).
pub const PPC0: i64 = 16;
pub const TILE: u32 = 256;
pub const MAX_Z: u32 = 4;

const VOID: [u8; 4] = [0x14, 0x16, 0x1a, 0xff];
const WALL_RGB: [u8; 3] = [0xd8, 0xe2, 0xec];

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

/// Painted bounding box in cells, min 1x1 (negative cells are out of bounds).
pub fn grid_extent(floor: &FloorDoc) -> (u32, u32) {
    let (mut cols, mut rows) = (1, 1);
    for (c, _) in floor.grid.cells() {
        cols = cols.max(c.x + 1);
        rows = rows.max(c.y + 1);
    }
    (cols as u32, rows as u32)
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

    let mut paint = tiny_skia::Paint {
        anti_alias: false,
        ..Default::default()
    };

    for (cell, space_id) in floor.grid.cells() {
        let cx = i64::from(cell.x);
        let cy = i64::from(cell.y);
        if cx < cx_from || cx > cx_to || cy < cy_from || cy > cy_to {
            continue;
        }
        let rgb = floor
            .grid
            .spaces
            .get(space_id)
            .map(|s| parse_hex(&s.color))
            .unwrap_or([0x33, 0x44, 0x55]);
        paint.set_color_rgba8(rgb[0], rgb[1], rgb[2], 0xff);
        // Cell rect in world px, clipped to the tile.
        let wx = cell.x as i64 * ppc;
        let wy = cell.y as i64 * ppc;
        let rx = (wx - x0).max(0);
        let ry = (wy - y0).max(0);
        let rx2 = ((wx + ppc) - x0).min(tile_px_w);
        let ry2 = ((wy + ppc) - y0).min(tile_px_h);
        if rx >= rx2 || ry >= ry2 {
            continue;
        }
        let rect =
            tiny_skia::Rect::from_xywh(rx as f32, ry as f32, (rx2 - rx) as f32, (ry2 - ry) as f32);
        if let Some(r) = rect {
            pixmap.fill_rect(r, &paint, tiny_skia::Transform::identity(), None);
        }
    }

    // Walls on top: stroke width grows with zoom, min 1px, so z0 reads.
    let mut wall_paint = tiny_skia::Paint {
        anti_alias: true,
        ..Default::default()
    };
    wall_paint.set_color_rgba8(WALL_RGB[0], WALL_RGB[1], WALL_RGB[2], 0xff);
    let width = ((ppc as f32) / 16.0).max(1.0);
    let stroke = tiny_skia::Stroke {
        width,
        ..Default::default()
    };
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
    walls_from_grid(&floor.grid, cell_size).unwrap_or_default()
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
