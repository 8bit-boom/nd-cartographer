//! Prefabs: a piece of a map (some rooms with their doors, lights and notes)
//! kept in a library shared by every project, and placed anywhere, rotated
//! if wanted. Copying between projects is the same thing: save to the
//! library from one, place into another. Placing is one undoable batch.

use crate::doc::{FloorDoc, Label, Light};
use crate::error::{CoreError, CoreResult};
use crate::grid::{CellKey, Edge, EdgeKey, Mark, Space};
use crate::ids::new_id;
use crate::journal::Op;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_PREFAB_CELLS: usize = 20_000;
pub const MAX_PREFABS: usize = 500;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Prefab {
    pub id: String,
    pub name: String,
    /// Bounding box in cells; everything is stored relative to its top-left.
    pub w: i32,
    pub h: i32,
    pub spaces: Vec<Space>,
    /// `(dx, dy, index into spaces)`.
    pub cells: Vec<(i32, i32, usize)>,
    pub marks: Vec<(i32, i32, Edge, Mark)>,
    /// Positions in cells (not the pixel lattice), so a prefab does not care
    /// what cell size its source project used.
    pub lights: Vec<PrefabLight>,
    pub labels: Vec<PrefabLabel>,
    /// Props whose centre cell falls inside the captured rooms, positions
    /// relative to the box top-left (plan: prefabs carry props).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stamps: Vec<PrefabStamp>,
    /// Freehand brush strokes whose points all fall inside, stored relative.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strokes: Vec<PrefabStroke>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PrefabStamp {
    pub dx: f64,
    pub dy: f64,
    pub asset: String,
    pub scale: f32,
    pub rotation: f32,
    pub flip: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<String>,
    pub shadow: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PrefabStroke {
    pub points: Vec<PrefabStrokePoint>,
    pub material: Option<String>,
    pub color: String,
    pub size: f32,
    pub opacity: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PrefabStrokePoint {
    pub dx: f32,
    pub dy: f32,
    pub pressure: f32,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PrefabLight {
    pub dx: f64,
    pub dy: f64,
    pub range: f64,
    pub color: String,
    pub intensity: f64,
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct PrefabLabel {
    pub dx: f64,
    pub dy: f64,
    pub text: String,
    pub size: f64,
    pub color: String,
    pub bold: bool,
    pub gm_only: bool,
}

/// Capture the named rooms (cells, door marks on their cells, lights and notes
/// that sit on them) of a floor. `cell_size` is the lattice lights are stored in.
pub fn capture(
    floor: &FloorDoc,
    name: &str,
    space_ids: &[String],
    cell_size: f64,
) -> CoreResult<Prefab> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(CoreError::Invalid(
            "a prefab name is 1-80 characters".into(),
        ));
    }
    let mut spaces: Vec<Space> = Vec::new();
    for id in space_ids {
        let s = floor
            .grid
            .spaces
            .get(id)
            .ok_or_else(|| CoreError::NotFound(format!("room {id}")))?;
        if !spaces.iter().any(|x| x.id == *id) {
            spaces.push(s.clone());
        }
    }
    let picked: Vec<(CellKey, usize)> = floor
        .grid
        .cells()
        .filter_map(|(c, sid)| spaces.iter().position(|s| s.id == sid).map(|i| (c, i)))
        .collect();
    if picked.is_empty() {
        return Err(CoreError::Invalid(
            "those rooms have no squares to copy".into(),
        ));
    }
    if picked.len() > MAX_PREFAB_CELLS {
        return Err(CoreError::CapExceeded(format!(
            "a prefab holds at most {MAX_PREFAB_CELLS} squares"
        )));
    }
    let (x0, y0) = (
        picked.iter().map(|p| p.0.x).min().unwrap_or(0),
        picked.iter().map(|p| p.0.y).min().unwrap_or(0),
    );
    let (x1, y1) = (
        picked.iter().map(|p| p.0.x).max().unwrap_or(0),
        picked.iter().map(|p| p.0.y).max().unwrap_or(0),
    );
    let inside = |c: CellKey| picked.iter().any(|p| p.0 == c);
    let cells: Vec<(i32, i32, usize)> = picked
        .iter()
        .map(|(c, i)| (c.x - x0, c.y - y0, *i))
        .collect();
    let marks = floor
        .grid
        .marks
        .iter()
        .filter(|(k, _)| inside(k.cell))
        .map(|(k, m)| (k.cell.x - x0, k.cell.y - y0, k.edge, *m))
        .collect();
    let on_cell = |x: f64, y: f64, scale: f64| {
        inside(CellKey {
            x: (x / scale).floor() as i32,
            y: (y / scale).floor() as i32,
        })
    };
    let lights = floor
        .lights
        .iter()
        .filter(|l| on_cell(l.x, l.y, cell_size))
        .map(|l| PrefabLight {
            dx: l.x / cell_size - x0 as f64,
            dy: l.y / cell_size - y0 as f64,
            range: l.range,
            color: l.color.clone(),
            intensity: l.intensity,
            label: l.label.clone(),
        })
        .collect();
    let labels = floor
        .labels
        .iter()
        .filter(|l| l.path.is_empty() && on_cell(l.x, l.y, 1.0))
        .map(|l| PrefabLabel {
            dx: l.x - x0 as f64,
            dy: l.y - y0 as f64,
            text: l.text.clone(),
            size: l.size,
            color: l.color.clone(),
            bold: l.bold,
            gm_only: l.gm_only,
        })
        .collect();
    // Props: centre cell inside the captured rooms. Positions are relative
    // to the box top-left in cells, so a prefab does not care what cell size
    // its source used.
    let stamps = floor
        .stamps
        .iter()
        .filter(|s| on_cell(f64::from(s.x), f64::from(s.y), 1.0))
        .map(|s| PrefabStamp {
            dx: f64::from(s.x) - x0 as f64,
            dy: f64::from(s.y) - y0 as f64,
            asset: s.asset.clone(),
            scale: s.scale,
            rotation: s.rotation,
            flip: s.flip,
            tint: s.tint.clone(),
            shadow: s.shadow,
        })
        .collect();
    // Strokes: only fully-inside ones (a half-copied stroke is worse than
    // none); points shift like everything else.
    let strokes = floor
        .strokes
        .iter()
        .filter(|st| {
            !st.points.is_empty()
                && st
                    .points
                    .iter()
                    .all(|pt| on_cell(f64::from(pt.x), f64::from(pt.y), 1.0))
        })
        .map(|st| PrefabStroke {
            points: st
                .points
                .iter()
                .map(|pt| PrefabStrokePoint {
                    dx: pt.x - x0 as f32,
                    dy: pt.y - y0 as f32,
                    pressure: pt.pressure,
                })
                .collect(),
            material: st.material.clone(),
            color: st.color.clone(),
            size: st.size,
            opacity: st.opacity,
        })
        .collect();
    Ok(Prefab {
        id: new_id("pf"),
        name: name.to_string(),
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
        spaces,
        cells,
        marks,
        lights,
        labels,
        stamps,
        strokes,
    })
}

fn turn_edge(e: Edge, quarter_turns: u8) -> Edge {
    let ring = [Edge::North, Edge::East, Edge::South, Edge::West];
    let i = ring.iter().position(|x| *x == e).unwrap_or(0);
    ring[(i + quarter_turns as usize) % 4]
}

/// Rotate a cell offset `quarter_turns` clockwise inside a `w` x `h` box;
/// returns the new offset and the new box size.
fn turn_cell(dx: i32, dy: i32, w: i32, h: i32, q: u8) -> (i32, i32, i32, i32) {
    match q % 4 {
        0 => (dx, dy, w, h),
        1 => (h - 1 - dy, dx, h, w),
        2 => (w - 1 - dx, h - 1 - dy, w, h),
        _ => (dy, w - 1 - dx, h, w),
    }
}

/// The ops that place a prefab with its top-left at cell `(x, y)`, turned
/// `quarter_turns` clockwise. Rooms get fresh ids, so placing twice never
/// merges the copies.
pub fn place(p: &Prefab, x: i32, y: i32, quarter_turns: u8, cell_size: f64) -> CoreResult<Vec<Op>> {
    let q = quarter_turns % 4;
    let tag = new_id("p");
    let mut remap: BTreeMap<usize, String> = BTreeMap::new();
    let mut ops: Vec<Op> = Vec::new();
    for (i, s) in p.spaces.iter().enumerate() {
        let mut ns = s.clone();
        ns.id = format!("{tag}_{}", s.id);
        remap.insert(i, ns.id.clone());
        ops.push(Op::UpsertSpace { space: ns });
    }
    let (_, _, nw, nh) = turn_cell(0, 0, p.w, p.h, q);
    let mut per_space: BTreeMap<usize, Vec<CellKey>> = BTreeMap::new();
    for (dx, dy, si) in &p.cells {
        let (tx, ty, _, _) = turn_cell(*dx, *dy, p.w, p.h, q);
        per_space.entry(*si).or_default().push(CellKey {
            x: x + tx,
            y: y + ty,
        });
    }
    for (si, cells) in per_space {
        if cells.iter().any(|c| !c.in_bounds()) {
            return Err(CoreError::OutOfBounds(
                "the prefab would leave the map".into(),
            ));
        }
        ops.push(Op::PaintCells {
            cells,
            space: remap.get(&si).cloned(),
        });
    }
    for (dx, dy, edge, mark) in &p.marks {
        let (tx, ty, _, _) = turn_cell(*dx, *dy, p.w, p.h, q);
        ops.push(Op::SetMark {
            edge: EdgeKey {
                cell: CellKey {
                    x: x + tx,
                    y: y + ty,
                },
                edge: turn_edge(*edge, q),
            },
            mark: Some(*mark),
        });
    }
    // Points turn about the box centre.
    let turn_pt = |px: f64, py: f64| -> (f64, f64) {
        let (w, h) = (p.w as f64, p.h as f64);
        match q {
            0 => (px, py),
            1 => (h - py, px),
            2 => (w - px, h - py),
            _ => (py, w - px),
        }
    };
    let _ = (nw, nh);
    for l in &p.lights {
        let (tx, ty) = turn_pt(l.dx, l.dy);
        ops.push(Op::AddLight {
            light: Light {
                id: new_id("gl"),
                x: (x as f64 + tx) * cell_size,
                y: (y as f64 + ty) * cell_size,
                range: l.range,
                color: l.color.clone(),
                intensity: l.intensity,
                on: true,
                label: l.label.clone(),
            },
        });
    }
    for l in &p.labels {
        let (tx, ty) = turn_pt(l.dx, l.dy);
        let mut label = Label::new(&new_id("lb"), x as f64 + tx, y as f64 + ty, &l.text);
        label.size = l.size;
        label.color = l.color.clone();
        label.bold = l.bold;
        label.gm_only = l.gm_only;
        ops.push(Op::UpsertLabel { label });
    }
    // Props: re-placed with fresh ids; a quarter turn rotates the offset and
    // adds PI to the rotation so the prop faces the way it was drawn.
    for s in &p.stamps {
        let (tx, ty) = turn_pt(s.dx, s.dy);
        ops.push(Op::AddStamp {
            stamp: crate::stamp::Stamp {
                id: new_id("gp"),
                asset: s.asset.clone(),
                x: (x as f64 + tx) as f32,
                y: (y as f64 + ty) as f32,
                scale: s.scale,
                rotation: s.rotation
                    + if q % 2 == 1 {
                        std::f32::consts::FRAC_PI_2
                    } else {
                        0.0
                    },
                flip: s.flip != (q % 2 == 1),
                tint: s.tint.clone(),
                shadow: s.shadow,
                gen: None,
            },
            index: None,
        });
    }
    // Strokes: points shift; quarter turns rotate about the box centre.
    for st in &p.strokes {
        ops.push(Op::AddStroke {
            stroke: crate::stroke::Stroke {
                id: new_id("gs"),
                kind: crate::stroke::StrokeKind::Paint,
                brush: crate::stroke::BrushKind::Round,
                material: st.material.clone(),
                color: st.color.clone(),
                size: st.size,
                opacity: st.opacity,
                hardness: 0.8,
                density: 1.0,
                roughness: 0.0,
                points: st
                    .points
                    .iter()
                    .map(|pt| {
                        let (tx, ty) = turn_pt(f64::from(pt.dx), f64::from(pt.dy));
                        crate::stroke::StrokePoint {
                            x: (x as f64 + tx) as f32,
                            y: (y as f64 + ty) as f32,
                            pressure: pt.pressure,
                            tilt: 0.0,
                        }
                    })
                    .collect(),
                seed: 1,
            },
            index: None,
        });
    }
    Ok(vec![Op::Batch { ops }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{default_space, MarkKind, SpaceKind};
    use crate::journal::Journal;

    fn source() -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.grid
            .upsert_space(default_space("a", "Hall", SpaceKind::Room))
            .unwrap();
        f.grid
            .upsert_space(default_space("b", "Other", SpaceKind::Room))
            .unwrap();
        let at = |x, y| CellKey { x, y };
        // A 3x2 hall at (10,5), a door on its east edge, a light and a note inside.
        f.grid
            .paint(
                &[
                    at(10, 5),
                    at(11, 5),
                    at(12, 5),
                    at(10, 6),
                    at(11, 6),
                    at(12, 6),
                ],
                Some("a"),
            )
            .unwrap();
        f.grid.paint(&[at(0, 0)], Some("b")).unwrap();
        f.grid
            .set_mark(
                EdgeKey {
                    cell: at(12, 5),
                    edge: Edge::East,
                },
                Some(Mark::new(MarkKind::Opening)),
            )
            .unwrap();
        f.lights.push(Light {
            id: "l".into(),
            x: 11.0 * 32.0 + 16.0,
            y: 5.0 * 32.0 + 16.0,
            range: 4.0,
            color: "#ffaa00".into(),
            intensity: 0.8,
            on: true,
            label: None,
        });
        f.labels.push(Label::new("t", 11.5, 5.5, "Hall"));
        f
    }

    fn place_into(p: &Prefab, x: i32, y: i32, q: u8) -> FloorDoc {
        let mut f = FloorDoc::new("g".into(), "G".into());
        let mut j = Journal::default();
        for op in place(p, x, y, q, 32.0).unwrap() {
            j.push(&mut f, &op).unwrap();
        }
        f
    }

    #[test]
    fn a_captured_piece_places_elsewhere_with_its_door_light_and_note() {
        let p = capture(&source(), "Hall", &["a".into()], 32.0).unwrap();
        assert_eq!((p.w, p.h, p.cells.len()), (3, 2, 6));
        assert_eq!((p.lights.len(), p.labels.len(), p.marks.len()), (1, 1, 1));
        let f = place_into(&p, 20, 30, 0);
        assert_eq!(f.grid.cell_count(), 6);
        let cells: Vec<_> = f.grid.cells().map(|(c, _)| (c.x, c.y)).collect();
        assert!(cells.contains(&(20, 30)) && cells.contains(&(22, 31)));
        assert_eq!(f.grid.marks.len(), 1);
        assert_eq!(f.lights.len(), 1);
        assert_eq!(
            (f.lights[0].x, f.lights[0].y),
            (21.0 * 32.0 + 16.0, 30.0 * 32.0 + 16.0)
        );
        assert_eq!((f.labels[0].x, f.labels[0].y), (21.5, 30.5));
        // The other room was not taken.
        assert_eq!(f.grid.spaces.len(), 1);
    }

    #[test]
    fn placing_twice_makes_two_rooms_and_turning_swaps_the_box() {
        let p = capture(&source(), "Hall", &["a".into()], 32.0).unwrap();
        let mut f = place_into(&p, 0, 0, 0);
        let mut j = Journal::default();
        for op in place(&p, 10, 10, 1, 32.0).unwrap() {
            j.push(&mut f, &op).unwrap();
        }
        assert_eq!(f.grid.spaces.len(), 2);
        assert_eq!(f.grid.cell_count(), 12);
        // Turned a quarter: the 3x2 hall is now 2 wide and 3 tall.
        let turned: Vec<_> = f
            .grid
            .cells()
            .map(|(c, _)| (c.x, c.y))
            .filter(|(x, y)| *x >= 10 && *y >= 10)
            .collect();
        let (maxx, maxy) = (
            turned.iter().map(|c| c.0).max().unwrap(),
            turned.iter().map(|c| c.1).max().unwrap(),
        );
        assert_eq!((maxx - 10 + 1, maxy - 10 + 1), (2, 3));
        // The east door now faces south.
        assert!(f
            .grid
            .marks
            .keys()
            .any(|k| k.edge == Edge::South && k.cell.x >= 10));
    }

    #[test]
    fn bad_captures_and_out_of_bounds_placements_are_refused() {
        let s = source();
        assert!(capture(&s, " ", &["a".into()], 32.0).is_err());
        assert!(capture(&s, "x", &["nope".into()], 32.0).is_err());
        let p = capture(&s, "Hall", &["a".into()], 32.0).unwrap();
        assert!(place(&p, crate::error::MAX_CELL, 0, 0, 32.0).is_err());
    }

    #[test]
    fn four_quarter_turns_come_home() {
        for (w, h) in [(3, 2), (1, 5), (4, 4)] {
            for (dx, dy) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
                let (mut x, mut y, mut cw, mut ch) = (dx, dy, w, h);
                for _ in 0..4 {
                    let r = turn_cell(x, y, cw, ch, 1);
                    (x, y, cw, ch) = r;
                }
                assert_eq!((x, y, cw, ch), (dx, dy, w, h));
            }
        }
    }
    #[test]
    fn prefabs_carry_props_and_strokes() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"a","name":"Barracks","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":0,"y":1},{"x":1,"y":1}],"space":"a"},
            {"t":"add_stamp","stamp":{"id":"sp1","asset":"bed","x":0.5,"y":0.5,"scale":1.0,"rotation":0.0,"flip":false,"tint":null,"shadow":true}},
            {"t":"add_stamp","stamp":{"id":"sp_out","asset":"crate","x":9.5,"y":9.5,"scale":1.0,"rotation":0.0,"flip":false,"tint":null,"shadow":true}}
        ]))
        .unwrap();
        for op in ops {
            j.push(&mut f, &op).unwrap();
        }
        // A stroke fully inside, and one straddling the boundary.
        let inside = crate::stroke::Stroke {
            id: "st_in".into(),
            kind: crate::stroke::StrokeKind::Paint,
            brush: crate::stroke::BrushKind::Round,
            material: None,
            color: "#ff0000".into(),
            size: 2.0,
            opacity: 0.8,
            hardness: 0.8,
            density: 1.0,
            roughness: 0.0,
            seed: 1,
            points: vec![
                crate::stroke::StrokePoint {
                    x: 0.5,
                    y: 0.5,
                    pressure: 1.0,
                    tilt: 0.0,
                },
                crate::stroke::StrokePoint {
                    x: 1.2,
                    y: 0.6,
                    pressure: 0.8,
                    tilt: 0.0,
                },
            ],
        };
        let straddle = crate::stroke::Stroke {
            id: "st_out".into(),
            kind: crate::stroke::StrokeKind::Paint,
            brush: crate::stroke::BrushKind::Round,
            material: None,
            color: "#00ff00".into(),
            size: 2.0,
            opacity: 0.8,
            hardness: 0.8,
            density: 1.0,
            roughness: 0.0,
            seed: 1,
            points: vec![
                crate::stroke::StrokePoint {
                    x: 0.5,
                    y: 0.5,
                    pressure: 1.0,
                    tilt: 0.0,
                },
                crate::stroke::StrokePoint {
                    x: 8.2,
                    y: 8.6,
                    pressure: 0.8,
                    tilt: 0.0,
                },
            ],
        };
        j.push(
            &mut f,
            &Op::AddStroke {
                stroke: inside,
                index: None,
            },
        )
        .unwrap();
        j.push(
            &mut f,
            &Op::AddStroke {
                stroke: straddle,
                index: None,
            },
        )
        .unwrap();

        let p = capture(&f, "Barracks kit", &["a".into()], 32.0).unwrap();
        assert_eq!(p.stamps.len(), 1, "the outside prop stays out");
        assert_eq!(p.stamps[0].asset, "bed");
        assert_eq!(p.strokes.len(), 1, "the straddling stroke stays out");
        assert_eq!(p.strokes[0].points.len(), 2);

        // Placement: the prop and the stroke land, fresh ids.
        let ops = place(&p, 10, 10, 0, 32.0).unwrap();
        let mut stamps_in = 0;
        let mut strokes_in = 0;
        if let Op::Batch { ops } = &ops[0] {
            for o in ops {
                match o {
                    Op::AddStamp { stamp, .. } => {
                        stamps_in += 1;
                        assert!(stamp.id.starts_with("gp"), "fresh id");
                    }
                    Op::AddStroke { .. } => strokes_in += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(stamps_in, 1);
        assert_eq!(strokes_in, 1);
    }
}
