//! Our room grid -> nd-world's room-tool state
//! `{cell, ox, oy, cols, rows, cells:[id,run,...], spaces:[{id,name,kind}],
//!   marks:[[x,y,"h"|"v",kind]]}` (nd-world `app/map_rooms.py`).
//! This is a translation, not a pass-through: nd-world's space ids are small
//! integers from a fixed kind list, cells are run-length coded, and an edge
//! mark names the cell it is the TOP ("h") or LEFT ("v") edge of.

use ndcart_core::grid::{CellKey, Edge, MarkKind, RoomGrid, SpaceKind};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const MAX_SIDE: i32 = 400;
pub const MAX_CELLS: i64 = 40_000;
pub const MAX_SPACES: usize = 80;
pub const MAX_MARKS: usize = 5_000;

fn kind_name(k: SpaceKind) -> &'static str {
    match k {
        SpaceKind::Room => "hall",
        SpaceKind::Corridor => "corridor",
        SpaceKind::Area => "other",
    }
}

/// `cell_px` is the lattice size the picture was baked at (nd-world: 8..=400).
pub fn to_nd_world(grid: &RoomGrid, cell_px: f64) -> Result<Value, String> {
    if !(8.0..=400.0).contains(&cell_px) {
        return Err("cell size must be 8..=400 px for nd-world".into());
    }
    let (mut cols, mut rows) = (1, 1);
    for (c, _) in grid.cells() {
        cols = cols.max(c.x + 1);
        rows = rows.max(c.y + 1);
    }
    if cols > MAX_SIDE || rows > MAX_SIDE || i64::from(cols) * i64::from(rows) > MAX_CELLS {
        return Err(format!(
            "nd-world's room grid holds at most {MAX_SIDE} squares a side and {MAX_CELLS} in all (this one is {cols}x{rows})"
        ));
    }
    if grid.spaces.len() > MAX_SPACES {
        return Err(format!("nd-world keeps at most {MAX_SPACES} spaces"));
    }
    // Stable small-integer ids in key order.
    let ids: std::collections::BTreeMap<&str, u32> = grid
        .spaces
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i as u32 + 1))
        .collect();
    let spaces: Vec<Value> = grid
        .spaces
        .values()
        .map(|s| json!({"id": ids[s.id.as_str()], "name": s.name, "kind": kind_name(s.kind)}))
        .collect();

    // Row-major runs; id 0 is empty.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for y in 0..rows {
        for x in 0..cols {
            let id = grid
                .cell_space(CellKey::new(x, y))
                .and_then(|s| ids.get(s).copied())
                .unwrap_or(0);
            match runs.last_mut() {
                Some((last, n)) if *last == id => *n += 1,
                _ => runs.push((id, 1)),
            }
        }
    }
    let cells: Vec<Value> = runs
        .iter()
        .flat_map(|(i, n)| [json!(i), json!(n)])
        .collect();

    // Marks: every edge normalised to the cell whose TOP (h) / LEFT (v) it is,
    // de-duplicated (we store a mark on both sides of a shared edge).
    let mut marks: BTreeSet<(i32, i32, &str, &str)> = BTreeSet::new();
    for (key, m) in &grid.marks {
        let (x, y) = (key.cell.x, key.cell.y);
        let (mx, my, axis) = match key.edge {
            Edge::West => (x, y, "v"),
            Edge::North => (x, y, "h"),
            Edge::East => (x + 1, y, "v"),
            Edge::South => (x, y + 1, "h"),
        };
        let kind = match m.kind {
            MarkKind::Opening => "open",
            MarkKind::Door => "door",
            MarkKind::Window => "window",
            MarkKind::Secret => "secret",
        };
        marks.insert((mx, my, axis, kind));
    }
    if marks.len() > MAX_MARKS {
        return Err(format!("nd-world keeps at most {MAX_MARKS} edge marks"));
    }
    let marks: Vec<Value> = marks
        .into_iter()
        .map(|(x, y, a, k)| json!([x, y, a, k]))
        .collect();

    Ok(json!({
        "cell": cell_px, "ox": 0, "oy": 0, "cols": cols, "rows": rows,
        "cells": cells, "spaces": spaces, "marks": marks,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::grid::{default_space, EdgeKey, Mark};

    fn grid() -> RoomGrid {
        let mut g = RoomGrid::default();
        g.upsert_space(default_space("a", "Hall", SpaceKind::Room))
            .unwrap();
        g.upsert_space(default_space("b", "Passage", SpaceKind::Corridor))
            .unwrap();
        g.paint(&[CellKey::new(0, 0), CellKey::new(1, 0)], Some("a"))
            .unwrap();
        g.paint(&[CellKey::new(2, 0)], Some("b")).unwrap();
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(1, 0),
                edge: Edge::East,
            },
            Some(Mark::new(MarkKind::Door)),
        )
        .unwrap();
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(0, 0),
                edge: Edge::South,
            },
            Some(Mark::new(MarkKind::Window)),
        )
        .unwrap();
        g
    }

    #[test]
    fn runs_spaces_and_marks_match_nd_worlds_shape() {
        let v = to_nd_world(&grid(), 32.0).unwrap();
        assert_eq!((v["cols"].as_i64(), v["rows"].as_i64()), (Some(3), Some(1)));
        // a a b -> id1 x2, id2 x1
        assert_eq!(v["cells"], json!([1, 2, 2, 1]));
        assert_eq!(
            v["spaces"][0],
            json!({"id": 1, "name": "Hall", "kind": "hall"})
        );
        assert_eq!(v["spaces"][1]["kind"], "corridor");
        // East edge of (1,0) = LEFT edge of (2,0); South of (0,0) = TOP of (0,1).
        let marks: Vec<String> = v["marks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.to_string())
            .collect();
        assert!(
            marks.contains(&r#"[2,0,"v","door"]"#.to_string()),
            "{marks:?}"
        );
        assert!(
            marks.contains(&r#"[0,1,"h","window"]"#.to_string()),
            "{marks:?}"
        );
    }

    #[test]
    fn shared_edge_marks_are_not_duplicated() {
        let mut g = grid();
        // The door's mirror (West of (2,0)) was stored too; it must collapse.
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(2, 0),
                edge: Edge::West,
            },
            Some(Mark::new(MarkKind::Door)),
        )
        .unwrap();
        let v = to_nd_world(&g, 32.0).unwrap();
        let doors = v["marks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m[3] == "door")
            .count();
        assert_eq!(doors, 1);
    }

    #[test]
    fn nd_world_limits_are_enforced_before_any_request() {
        assert!(to_nd_world(&grid(), 4.0).is_err());
        let mut g = RoomGrid::default();
        g.upsert_space(default_space("a", "A", SpaceKind::Room))
            .unwrap();
        g.paint(&[CellKey::new(500, 0)], Some("a")).unwrap();
        assert!(to_nd_world(&g, 32.0).unwrap_err().contains("400"));
    }
}
