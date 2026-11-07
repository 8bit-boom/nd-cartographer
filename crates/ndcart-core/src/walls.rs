//! Walls from rooms — the research-proven rule: *an edge between two
//! different spaces is a wall*, exact, integer, instantly recomputable.
//! Doors/windows/secrets are marks on edges; an `Opening` mark removes the
//! segment (an archway). Collinear runs of the same kind+state merge into
//! one segment (a 100x70-cell dungeon lands far under nd-world's 5,000-wall
//! publish cap).
//!
//! Output shape matches nd-world's `walls` route exactly:
//! `{id, pts: [[x,y],...], kind: wall|door|window|secret, state?: open|closed}`
//! in map pixels (cell lattice * cell_size, origin top-left).

use crate::error::CoreResult;
use crate::grid::{Edge, MarkKind, MarkState, RoomGrid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// nd-world's publish cap (`map_walls.py` refuses more).
pub const ND_WORLD_WALL_CAP: usize = 5_000;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum WallKind {
    Wall,
    Door,
    Window,
    Secret,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum WallState {
    Open,
    Closed,
}

impl From<MarkState> for WallState {
    fn from(s: MarkState) -> Self {
        match s {
            MarkState::Open => WallState::Open,
            MarkState::Closed => WallState::Closed,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Wall {
    pub id: String,
    pub pts: Vec<[f64; 2]>,
    pub kind: WallKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<WallState>,
}

/// One candidate segment in *cell* coordinates before scaling.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Seg {
    /// For horizontal segments: the row y the edge sits on; vertical: col x.
    line: i32,
    /// For horizontal: start x (the segment spans [from, to)); vertical: start y.
    from: i32,
    to: i32,
    kind: WallKind,
    state: Option<WallState>,
}

fn mark_to_wall(m: crate::grid::Mark) -> Option<(WallKind, Option<WallState>)> {
    match m.kind {
        MarkKind::Opening => None, // deliberate hole: no wall at all
        MarkKind::Door => Some((WallKind::Door, Some(WallState::from(m.state)))),
        MarkKind::Window => Some((WallKind::Window, None)),
        MarkKind::Secret => Some((WallKind::Secret, Some(WallState::from(m.state)))),
    }
}

/// Derive the walls for a grid. `cell_size` is pixels per cell (the document
/// lattice), so output coordinates are the map's own pixels, exactly what
/// nd-world stores.
pub fn walls_from_grid(grid: &RoomGrid, cell_size: f64) -> CoreResult<Vec<Wall>> {
    let mut horiz: Vec<Seg> = Vec::new(); // west/east edges (vertical segments, on a column x)
    let mut vert: Vec<Seg> = Vec::new(); // north/south edges (horizontal segments, on a row y)

    for (cell, space) in grid.cells() {
        // Emission rule (no duplicates): West/North emit when the neighbor is
        // empty OR a different space; East/South emit only when the neighbor
        // is empty (a boundary) — an interior wall between two spaces is
        // emitted exactly once, by the eastern/southern space's own
        // West/North emission.
        let w = grid.cell_space(cell.west());
        if w != Some(space) {
            push_seg(
                &mut horiz,
                cell.x,
                cell.y,
                cell.y + 1,
                grid,
                cell,
                Edge::West,
            );
        }
        let n = grid.cell_space(cell.north());
        if n != Some(space) {
            push_seg(
                &mut vert,
                cell.y,
                cell.x,
                cell.x + 1,
                grid,
                cell,
                Edge::North,
            );
        }
        let e = grid.cell_space(cell.east());
        if e.is_none() {
            push_seg(
                &mut horiz,
                cell.x + 1,
                cell.y,
                cell.y + 1,
                grid,
                cell,
                Edge::East,
            );
        }
        let s = grid.cell_space(cell.south());
        if s.is_none() {
            push_seg(
                &mut vert,
                cell.y + 1,
                cell.x,
                cell.x + 1,
                grid,
                cell,
                Edge::South,
            );
        }
    }

    Ok(merge_and_render(horiz, vert, cell_size))
}

fn push_seg(
    bucket: &mut Vec<Seg>,
    line: i32,
    from: i32,
    to: i32,
    grid: &RoomGrid,
    cell: crate::grid::CellKey,
    edge: Edge,
) {
    let (kind, state) = match grid.mark_between(cell, edge) {
        Some(m) => match mark_to_wall(m) {
            Some(pair) => pair,
            None => return, // opening: no wall
        },
        None => (WallKind::Wall, None),
    };
    bucket.push(Seg {
        line,
        from,
        to,
        kind,
        state,
    });
}

/// Sort, then merge collinear runs with the same kind+state.
fn merge_and_render(mut horiz: Vec<Seg>, mut vert: Vec<Seg>, s: f64) -> Vec<Wall> {
    horiz.sort();
    vert.sort();

    let mut walls = Vec::new();
    for (bucket, horizontal) in [(horiz, false), (vert, true)] {
        let mut run: Option<Seg> = None;
        for seg in bucket {
            match &mut run {
                Some(prev)
                    if prev.line == seg.line
                        && prev.to == seg.from
                        && prev.kind == seg.kind
                        && prev.state == seg.state
                        // Doors and secrets are individual portals with
                        // their own open/closed state: two doors side by
                        // side are two doors, never one wide one.
                        && !matches!(seg.kind, WallKind::Door | WallKind::Secret) =>
                {
                    prev.to = seg.to;
                }
                _ => {
                    if let Some(done) = run.take() {
                        emit(&mut walls, done, horizontal, s);
                    }
                    run = Some(seg);
                }
            }
        }
        if let Some(done) = run {
            emit(&mut walls, done, horizontal, s);
        }
    }
    walls
}

fn emit(walls: &mut Vec<Wall>, seg: Seg, horizontal: bool, s: f64) {
    let pts = if horizontal {
        vec![
            [seg.from as f64 * s, seg.line as f64 * s],
            [seg.to as f64 * s, seg.line as f64 * s],
        ]
    } else {
        vec![
            [seg.line as f64 * s, seg.from as f64 * s],
            [seg.line as f64 * s, seg.to as f64 * s],
        ]
    };
    // Stable ids: derived from where the run starts and what it is, so a
    // re-derivation (and so a re-publish) gives the same id and nd-world keeps
    // the runtime state of a door the table has opened. Doors never merge with
    // their neighbours, so a door's id only changes if the door itself moves.
    let kind = match seg.kind {
        WallKind::Wall => "w",
        WallKind::Door => "d",
        WallKind::Window => "n",
        WallKind::Secret => "s",
    };
    walls.push(Wall {
        id: format!(
            "{}{}{}_{}_{}",
            if horizontal { "h" } else { "v" },
            kind,
            seg.line,
            seg.from,
            seg.to - seg.from
        ),
        pts,
        kind: seg.kind,
        state: seg.state,
    });
}

/// Every wall on a floor, in lattice pixels at `cell_size`: the walls derived
/// from rooms, then the imported ones (cells -> pixels). This is the one place
/// the two sources meet, so exports, publishing and the linter all agree.
/// Imported ids are `iw<index>`, stable for an unchanged import.
pub fn floor_walls(floor: &crate::doc::FloorDoc, cell_size: f64) -> CoreResult<Vec<Wall>> {
    let mut walls = walls_from_grid(&floor.grid, cell_size)?;
    for (i, w) in floor.imported_walls.iter().enumerate() {
        walls.push(Wall {
            id: format!("iw{i}"),
            pts: w
                .pts
                .iter()
                .map(|p| [f64::from(p[0]) * cell_size, f64::from(p[1]) * cell_size])
                .collect(),
            kind: w.kind,
            state: w.state,
        });
    }
    Ok(walls)
}

/// A helper for the linter: how far over nd-world's publish cap a
/// derivation sits (`walls.len()`, once rendered).
pub fn over_wall_cap(walls: &[Wall]) -> bool {
    walls.len() > ND_WORLD_WALL_CAP
}

/// Counts per kind — the linter's "doors into the void" input lives here
/// later; M0 exposes the raw tally.
pub fn tally(walls: &[Wall]) -> BTreeMap<&'static str, usize> {
    let mut t = BTreeMap::new();
    for w in walls {
        let k = match w.kind {
            WallKind::Wall => "wall",
            WallKind::Door => "door",
            WallKind::Window => "window",
            WallKind::Secret => "secret",
        };
        *t.entry(k).or_insert(0) += 1;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{EdgeKey, Mark, Space, SpaceKind};
    use crate::CellKey;

    fn grid_with(cells: &[(i32, i32)], space: &str) -> RoomGrid {
        let mut g = RoomGrid::default();
        g.upsert_space(Space {
            id: space.to_string(),
            name: space.to_string(),
            color: "#112233".into(),
            kind: SpaceKind::Room,
            material: None,
            tint: None,
        })
        .unwrap();
        let keys: Vec<_> = cells.iter().map(|(x, y)| CellKey::new(*x, *y)).collect();
        g.paint(&keys, Some(space)).unwrap();
        g
    }

    fn pts_of_wall_at(walls: &[Wall], axis: usize, at: f64) -> Option<&Wall> {
        walls.iter().find(|w| (w.pts[0][axis] - at).abs() < 1e-9)
    }

    #[test]
    fn one_room_is_four_walls() {
        let g = grid_with(&[(0, 0), (1, 0), (0, 1), (1, 1)], "a");
        let walls = walls_from_grid(&g, 32.0).unwrap();
        // West col x=0 (2 cells), East col x=2, North row y=0, South row y=2.
        assert_eq!(walls.len(), 4, "{walls:?}");
        assert!(pts_of_wall_at(&walls, 0, 0.0).is_some());
        assert!(pts_of_wall_at(&walls, 0, 64.0).is_some());
        assert!(pts_of_wall_at(&walls, 1, 0.0).is_some());
        assert!(pts_of_wall_at(&walls, 1, 64.0).is_some());
        assert_eq!(tally(&walls)["wall"], 4);
    }

    #[test]
    fn two_rooms_share_one_wall_derived_once() {
        let mut g = grid_with(&[(0, 0)], "a");
        g.upsert_space(Space {
            id: "b".into(),
            name: "b".into(),
            color: "#445566".into(),
            kind: SpaceKind::Room,
            material: None,
            tint: None,
        })
        .unwrap();
        g.paint(&[CellKey::new(1, 0)], Some("b")).unwrap();
        let walls = walls_from_grid(&g, 32.0).unwrap();
        // Outline: x=0, x=2 verticals; y=0, y=1 horizontals (merged) — plus
        // the a|b divider at x=1, emitted once by b's west emission.
        assert_eq!(walls.len(), 5, "{walls:?}");
    }

    #[test]
    fn adjacent_same_space_has_no_inner_wall() {
        let g = grid_with(&[(0, 0), (1, 0)], "a");
        let walls = walls_from_grid(&g, 32.0).unwrap();
        assert_eq!(walls.len(), 4); // just the outline
    }

    #[test]
    fn door_mark_becomes_door_wall_with_state() {
        let mut g = grid_with(&[(0, 0)], "a");
        let key = EdgeKey {
            cell: CellKey::new(0, 0),
            edge: Edge::North,
        };
        g.set_mark(key, Some(crate::grid::Mark::new(MarkKind::Door)))
            .unwrap();
        let walls = walls_from_grid(&g, 32.0).unwrap();
        let north = walls
            .iter()
            .find(|w| w.pts[0][1] == 0.0 && w.pts[1][1] == 0.0) // horizontal at row 0
            .expect("north wall");
        assert_eq!(north.kind, WallKind::Door);
        assert_eq!(north.state, Some(WallState::Closed));
    }

    #[test]
    fn opening_mark_removes_the_segment() {
        let mut g = grid_with(&[(0, 0)], "a");
        let key = EdgeKey {
            cell: CellKey::new(0, 0),
            edge: Edge::North,
        };
        g.set_mark(key, Some(crate::grid::Mark::new(MarkKind::Opening)))
            .unwrap();
        let walls = walls_from_grid(&g, 32.0).unwrap();
        // No horizontal wall along row 0 remains (verticals may still start there).
        assert!(walls
            .iter()
            .all(|w| !(w.pts[0][1] == 0.0 && w.pts[1][1] == 0.0)));
        assert_eq!(walls.len(), 3);
    }

    #[test]
    fn collinear_runs_merge_and_doors_split_runs() {
        // A 1x4 row: plain walls at both ends, a door in the middle.
        let mut g = grid_with(&[(0, 0), (1, 0), (2, 0), (3, 0)], "a");
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(1, 0),
                edge: Edge::North,
            },
            Some(crate::grid::Mark::new(MarkKind::Door)),
        )
        .unwrap();
        let walls = walls_from_grid(&g, 32.0).unwrap();
        // North edge row y=0 splits into: wall [0..1], door [1..2], wall [2..4].
        let north: Vec<_> = walls
            .iter()
            .filter(|w| w.pts[0][1] == 0.0 && w.pts[1][1] == 0.0)
            .collect();
        let mut spans: Vec<(f64, f64)> = north.iter().map(|w| (w.pts[0][0], w.pts[1][0])).collect();
        spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        assert_eq!(spans, vec![(0.0, 32.0), (32.0, 64.0), (64.0, 128.0)]);
        let door = walls.iter().find(|w| w.kind == WallKind::Door).unwrap();
        assert_eq!(door.state, Some(WallState::Closed));
    }

    #[test]
    fn long_corridor_merges_into_two_segments() {
        let cells: Vec<(i32, i32)> = (0..40).map(|x| (x, 0)).collect();
        let g = grid_with(&cells, "a");
        let walls = walls_from_grid(&g, 32.0).unwrap();
        // Two vertical end walls (merged over 1 cell each), two horizontal
        // rails merged over 40 cells.
        assert_eq!(walls.len(), 4);
        assert!(!over_wall_cap(&walls));
    }

    #[test]
    fn mark_on_either_side_counts() {
        let mut g = grid_with(&[(0, 0)], "a");
        // Mark stored on the *neighbor* side of the same edge (neighbor
        // unpainted → no mirror sync happened, but between-check finds it).
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(-1, 0),
                edge: Edge::East,
            },
            Some(crate::grid::Mark::new(MarkKind::Secret)),
        )
        .unwrap();
        let walls = walls_from_grid(&g, 32.0).unwrap();
        let west = walls.iter().find(|w| w.pts[0][0] == 0.0).unwrap();
        assert_eq!(west.kind, WallKind::Secret);
        assert_eq!(west.state, Some(WallState::Closed));
    }

    #[test]
    fn wall_ids_are_stable_and_a_doors_id_survives_unrelated_edits() {
        let mut g = RoomGrid::default();
        g.upsert_space(Space {
            id: "a".into(),
            name: "A".into(),
            color: "#112233".into(),
            kind: SpaceKind::Room,
            material: None,
            tint: None,
        })
        .unwrap();
        let cells: Vec<CellKey> = (0..4)
            .flat_map(|x| (0..3).map(move |y| CellKey::new(x, y)))
            .collect();
        g.paint(&cells, Some("a")).unwrap();
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(1, 0),
                edge: Edge::North,
            },
            Some(Mark::new(MarkKind::Door)),
        )
        .unwrap();
        let ids = |g: &RoomGrid| -> Vec<String> {
            let mut v: Vec<String> = walls_from_grid(g, 32.0)
                .unwrap()
                .into_iter()
                .map(|w| w.id)
                .collect();
            v.sort();
            v
        };
        let a = ids(&g);
        assert_eq!(a, ids(&g), "two derivations agree");
        let mut set = a.clone();
        set.dedup();
        assert_eq!(set.len(), a.len(), "ids are unique");
        let door = |g: &RoomGrid| {
            walls_from_grid(g, 32.0)
                .unwrap()
                .into_iter()
                .find(|w| w.kind == WallKind::Door)
                .unwrap()
                .id
        };
        let before = door(&g);
        // Paint a far-away room: the door's id must not change.
        g.paint(&[CellKey::new(20, 20)], Some("a")).unwrap();
        assert_eq!(door(&g), before);
        // Move the door: its id changes.
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(1, 0),
                edge: Edge::North,
            },
            None,
        )
        .unwrap();
        g.set_mark(
            EdgeKey {
                cell: CellKey::new(2, 0),
                edge: Edge::North,
            },
            Some(Mark::new(MarkKind::Door)),
        )
        .unwrap();
        assert_ne!(door(&g), before);
    }
}
