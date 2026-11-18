//! Which part of a floor's picture an edit can change, so the server and the
//! studio redraw only the tiles that touch it instead of the whole map.
//!
//! The answer is deliberately conservative: only ops whose pixels stay near
//! their own cells are given a rectangle, and only on floors where nothing
//! else is laid out globally (text collisions, sunlight, relief, eras).
//! Everything else is [`Dirty::All`].

use crate::doc::FloorDoc;
use crate::journal::Op;
use crate::stamp::Stamp;
use std::collections::HashMap;

/// Cells added on every side of a dirty rectangle: the edge blend between
/// differently painted neighbours and the wall strokes reach about a cell.
pub const DIRTY_MARGIN: i64 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dirty {
    /// The edit changes no tile pixel (a light, a space nobody is painted in).
    Nothing,
    /// Cell coordinates, inclusive on both ends.
    Rect { x0: i64, y0: i64, x1: i64, y1: i64 },
    /// Anything may have changed.
    All,
}

impl Dirty {
    fn point(x: f64, y: f64, pad: f64) -> Dirty {
        Dirty::Rect {
            x0: (x - pad).floor() as i64,
            y0: (y - pad).floor() as i64,
            x1: (x + pad).ceil() as i64,
            y1: (y + pad).ceil() as i64,
        }
    }

    pub fn merge(self, other: Dirty) -> Dirty {
        match (self, other) {
            (Dirty::All, _) | (_, Dirty::All) => Dirty::All,
            (Dirty::Nothing, o) | (o, Dirty::Nothing) => o,
            (
                Dirty::Rect { x0, y0, x1, y1 },
                Dirty::Rect {
                    x0: a0,
                    y0: b0,
                    x1: a1,
                    y1: b1,
                },
            ) => Dirty::Rect {
                x0: x0.min(a0),
                y0: y0.min(b0),
                x1: x1.max(a1),
                y1: y1.max(b1),
            },
        }
    }

    /// Grow a rectangle by `by` cells on every side.
    pub fn inflate(self, by: i64) -> Dirty {
        match self {
            Dirty::Rect { x0, y0, x1, y1 } => Dirty::Rect {
                x0: x0 - by,
                y0: y0 - by,
                x1: x1 + by,
                y1: y1 + by,
            },
            o => o,
        }
    }

    /// Whether the rectangle touches cells `[cx0, cx1] x [cy0, cy1]` (inclusive).
    pub fn touches(self, cx0: i64, cy0: i64, cx1: i64, cy1: i64) -> bool {
        match self {
            Dirty::Nothing => false,
            Dirty::All => true,
            Dirty::Rect { x0, y0, x1, y1 } => x0 <= cx1 && x1 >= cx0 && y0 <= cy1 && y1 >= cy0,
        }
    }
}

/// Floors where text, sunlight, relief or an era make a local edit change
/// pixels far away (label collisions are decided across the whole floor).
fn is_local(floor: &FloorDoc) -> bool {
    floor.labels.is_empty()
        && floor.markers.is_empty()
        && floor.regions.is_empty()
        && floor.rivers.is_empty()
        && floor.routes.is_empty()
        && floor.sun.is_none()
        && floor.relief.is_none()
        && floor.active_era.is_none()
}

/// The cells of `ops` (applied in order to `floor`) whose pixels can change.
/// `stamp_reach` gives a stamp's drawn radius in cells (None when unknown,
/// which makes the answer [`Dirty::All`]). `floor` is the state *before* the
/// ops; later ops of a batch may only refer to things that exist there or that
/// an earlier op of the batch itself covers.
pub fn dirty_for_ops(
    floor: &FloorDoc,
    ops: &[Op],
    stamp_reach: &dyn Fn(&Stamp) -> Option<f32>,
) -> Dirty {
    if !is_local(floor) {
        return Dirty::All;
    }
    let mut spaces: HashMap<String, Dirty> = HashMap::new();
    let mut acc = Dirty::Nothing;
    for op in ops {
        acc = acc.merge(one(floor, op, stamp_reach, &mut spaces));
        if acc == Dirty::All {
            return Dirty::All;
        }
    }
    acc.inflate(DIRTY_MARGIN)
}

fn one(
    floor: &FloorDoc,
    op: &Op,
    stamp_reach: &dyn Fn(&Stamp) -> Option<f32>,
    spaces: &mut HashMap<String, Dirty>,
) -> Dirty {
    match op {
        Op::Batch { ops } => {
            let mut acc = Dirty::Nothing;
            for o in ops {
                acc = acc.merge(one(floor, o, stamp_reach, spaces));
                if acc == Dirty::All {
                    break;
                }
            }
            acc
        }
        Op::PaintCells { cells, .. } => cells
            .iter()
            .map(|c| Dirty::Rect {
                x0: i64::from(c.x),
                y0: i64::from(c.y),
                x1: i64::from(c.x),
                y1: i64::from(c.y),
            })
            .fold(Dirty::Nothing, Dirty::merge),
        Op::SetMark { edge, .. } => {
            let (x, y) = (i64::from(edge.cell.x), i64::from(edge.cell.y));
            Dirty::Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            }
        }
        // A space's look changes every cell painted in it.
        Op::UpsertSpace { space } => *spaces.entry(space.id.clone()).or_insert_with(|| {
            floor
                .grid
                .cells()
                .filter(|(_, s)| *s == space.id)
                .map(|(c, _)| Dirty::Rect {
                    x0: i64::from(c.x),
                    y0: i64::from(c.y),
                    x1: i64::from(c.x),
                    y1: i64::from(c.y),
                })
                .fold(Dirty::Nothing, Dirty::merge)
        }),
        // Lights only show in the player preview, never in a map tile.
        Op::AddLight { .. } | Op::RemoveLight { .. } => Dirty::Nothing,
        Op::AddStamp { stamp, .. } => match stamp_reach(stamp) {
            Some(r) => Dirty::point(f64::from(stamp.x), f64::from(stamp.y), f64::from(r) + 1.0),
            None => Dirty::All,
        },
        Op::UpdateStamp { stamp } => {
            let old = floor.stamps.iter().find(|s| s.id == stamp.id);
            match (old, stamp_reach(stamp)) {
                (Some(old), Some(r)) => match stamp_reach(old) {
                    Some(ro) => {
                        Dirty::point(f64::from(old.x), f64::from(old.y), f64::from(ro) + 1.0).merge(
                            Dirty::point(
                                f64::from(stamp.x),
                                f64::from(stamp.y),
                                f64::from(r) + 1.0,
                            ),
                        )
                    }
                    None => Dirty::All,
                },
                _ => Dirty::All,
            }
        }
        Op::RemoveStamp { id } => match floor.stamps.iter().find(|s| s.id == *id) {
            Some(old) => match stamp_reach(old) {
                Some(r) => Dirty::point(f64::from(old.x), f64::from(old.y), f64::from(r) + 1.0),
                None => Dirty::All,
            },
            None => Dirty::All,
        },
        Op::AddStroke { stroke, .. } => stroke_rect(stroke),
        Op::RemoveStroke { id } => match floor.strokes.iter().find(|s| s.id == *id) {
            Some(s) => stroke_rect(s),
            None => Dirty::All,
        },
        _ => Dirty::All,
    }
}

fn stroke_rect(s: &crate::stroke::Stroke) -> Dirty {
    if s.points.is_empty() {
        return Dirty::Nothing;
    }
    let b = s.point_bounds();
    let pad = f64::from(s.size.max(0.0)) + 1.0;
    Dirty::Rect {
        x0: (f64::from(b[0]) - pad).floor() as i64,
        y0: (f64::from(b[1]) - pad).floor() as i64,
        x1: (f64::from(b[2]) + pad).ceil() as i64,
        y1: (f64::from(b[3]) + pad).ceil() as i64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{CellKey, Space, SpaceKind};

    fn space(id: &str) -> Space {
        Space {
            id: id.into(),
            name: id.into(),
            color: "#aabbcc".into(),
            kind: SpaceKind::Room,
            material: None,
            tint: None,
        }
    }

    fn no_stamps(_: &Stamp) -> Option<f32> {
        None
    }

    fn rect(d: Dirty) -> [i64; 4] {
        match d {
            Dirty::Rect { x0, y0, x1, y1 } => [x0, y0, x1, y1],
            o => panic!("expected a rect, got {o:?}"),
        }
    }

    #[test]
    fn a_painted_cell_dirties_its_neighbourhood() {
        let f = FloorDoc::new("f1".into(), "Ground".into());
        let op = Op::PaintCells {
            cells: vec![CellKey::new(100, 100)],
            space: Some("a".into()),
        };
        let d = dirty_for_ops(&f, &[op], &no_stamps);
        assert_eq!(rect(d), [98, 98, 102, 102]);
    }

    #[test]
    fn batches_merge_and_lights_are_free() {
        let f = FloorDoc::new("f1".into(), "Ground".into());
        let paint = |x, y| Op::PaintCells {
            cells: vec![CellKey::new(x, y)],
            space: None,
        };
        let b = Op::Batch {
            ops: vec![paint(0, 0), paint(10, 4)],
        };
        assert_eq!(rect(dirty_for_ops(&f, &[b], &no_stamps)), [-2, -2, 12, 6]);
        let light = Op::RemoveLight { id: "l".into() };
        assert_eq!(dirty_for_ops(&f, &[light], &no_stamps), Dirty::Nothing);
    }

    #[test]
    fn upsert_space_covers_the_cells_already_in_it() {
        let mut f = FloorDoc::new("f1".into(), "Ground".into());
        f.grid.upsert_space(space("a")).unwrap();
        f.grid
            .paint(&[CellKey::new(3, 3), CellKey::new(8, 5)], Some("a"))
            .unwrap();
        let d = dirty_for_ops(&f, &[Op::UpsertSpace { space: space("a") }], &no_stamps);
        assert_eq!(rect(d), [1, 1, 10, 7]);
        let empty = dirty_for_ops(&f, &[Op::UpsertSpace { space: space("b") }], &no_stamps);
        assert_eq!(empty, Dirty::Nothing);
    }

    #[test]
    fn global_layouts_and_other_ops_are_all() {
        let mut f = FloorDoc::new("f1".into(), "Ground".into());
        let paint = Op::PaintCells {
            cells: vec![CellKey::new(0, 0)],
            space: None,
        };
        assert_eq!(
            dirty_for_ops(&f, &[Op::SetActiveEra { id: None }], &no_stamps),
            Dirty::All
        );
        f.sun = Some(crate::doc::Sun {
            azimuth_deg: 135.0,
            elevation_deg: 45.0,
            strength: 0.5,
        });
        assert_eq!(dirty_for_ops(&f, &[paint], &no_stamps), Dirty::All);
    }

    #[test]
    fn stamps_with_unknown_reach_are_all() {
        let f = FloorDoc::new("f1".into(), "Ground".into());
        let st: Stamp =
            serde_json::from_str(r#"{"id":"s1","asset":"crate","x":5.0,"y":5.0}"#).expect("stamp");
        let add = Op::AddStamp {
            stamp: st.clone(),
            index: None,
        };
        assert_eq!(
            dirty_for_ops(&f, std::slice::from_ref(&add), &no_stamps),
            Dirty::All
        );
        let d = dirty_for_ops(&f, &[add], &|_| Some(1.0));
        assert_eq!(rect(d), [1, 1, 9, 9]);
    }
}
