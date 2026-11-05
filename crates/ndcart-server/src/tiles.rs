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
pub fn bake_tile(floor: &FloorDoc, walls: &[Wall], z: u32, tx: i64, ty: i64) -> Vec<u8> {
    let z = z.min(MAX_Z);
    let ppc = PPC0 << z; // pixels per cell at this zoom (fits i64: 16<<4=256)
    let tile_px = TILE as i64;
    let x0 = tx * tile_px;
    let y0 = ty * tile_px;

    let mut pixmap = tiny_skia::Pixmap::new(TILE, TILE).expect("256x256 pixmap");
    pixmap.fill(tiny_skia::Color::from_rgba8(
        VOID[0], VOID[1], VOID[2], 0xff,
    ));

    // Cell range intersecting the tile (in cell coordinates).
    let cx_from = x0.div_euclid(ppc);
    let cx_to = (x0 + tile_px - 1).div_euclid(ppc);
    let cy_from = y0.div_euclid(ppc);
    let cy_to = (y0 + tile_px - 1).div_euclid(ppc);

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
        let rx2 = ((wx + ppc) - x0).min(tile_px);
        let ry2 = ((wy + ppc) - y0).min(tile_px);
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
    for w in walls {
        if w.pts.len() != 2 {
            continue;
        }
        let ax = (w.pts[0][0] as f32) * (ppc as f32) - x0 as f32;
        let ay = (w.pts[0][1] as f32) * (ppc as f32) - y0 as f32;
        let bx = (w.pts[1][0] as f32) * (ppc as f32) - x0 as f32;
        let by = (w.pts[1][1] as f32) * (ppc as f32) - y0 as f32;
        // Cheap reject: segment fully outside the tile.
        if (ax < -width && bx < -width)
            || (ax > tile_px as f32 + width && bx > tile_px as f32 + width)
            || (ay < -width && by < -width)
            || (ay > tile_px as f32 + width && by > tile_px as f32 + width)
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

    pixmap.encode_png().expect("png encode")
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
    fn tiles_are_png_of_the_right_size() {
        let floor = painted_floor();
        let walls = derive_walls(&floor, 32.0);
        let png = bake_tile(&floor, &walls, 0, 0, 0);
        assert!(png.len() > 100);
        // PNG magic.
        assert_eq!(&png[0..4], &[0x89, b'P', b'N', b'G']);
        // A tile far away from any content must be pure background but still valid.
        let empty = bake_tile(&floor, &walls, 0, -9, -9);
        assert_eq!(&empty[0..4], &[0x89, b'P', b'N', b'G']);
    }

    #[test]
    fn zoom_levels_all_bake() {
        let floor = painted_floor();
        let walls = derive_walls(&floor, 32.0);
        for z in 0..=MAX_Z {
            let png = bake_tile(&floor, &walls, z, 0, 0);
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
