//! The linter ("Problems panel", plan §17.2): deterministic checks, no AI.
//! Catches what nobody sees by eye at 300 rooms — and what nd-world would
//! refuse. Each problem can carry a `fix` that is a normal undoable [`Op`].

use crate::doc::FloorDoc;
use crate::grid::{CellKey, Edge, MarkKind};
use crate::journal::Op;
use crate::walls::{walls_from_grid, ND_WORLD_WALL_CAP};
use crate::MAX_LIGHTS;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// nd-world would refuse the publish.
    Error,
    Warning,
    Info,
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Problem {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    /// A cell to zoom to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<CellKey>,
    /// A normal undoable command that resolves it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<Op>,
}

fn problem(severity: Severity, code: &'static str, message: String) -> Problem {
    Problem {
        severity,
        code,
        message,
        at: None,
        fix: None,
    }
}

struct Dsu(Vec<usize>);
impl Dsu {
    fn find(&mut self, x: usize) -> usize {
        if self.0[x] != x {
            let r = self.find(self.0[x]);
            self.0[x] = r;
        }
        self.0[x]
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[b] = a;
        }
    }
}

fn neighbor(c: CellKey, e: Edge) -> CellKey {
    match e {
        Edge::West => c.west(),
        Edge::East => c.east(),
        Edge::North => c.north(),
        Edge::South => c.south(),
    }
}

/// Lint one floor. `cell_size` is the lattice size lights are stored in.
pub fn lint(floor: &FloorDoc, cell_size: f64) -> Vec<Problem> {
    let grid = &floor.grid;
    let mut out = Vec::new();

    if grid.cell_count() == 0 {
        out.push(problem(
            Severity::Warning,
            "empty_floor",
            "Nothing is painted on this floor yet.".into(),
        ));
    }

    // nd-world's publish caps.
    if let Ok(walls) = walls_from_grid(grid, cell_size) {
        if walls.len() > ND_WORLD_WALL_CAP {
            out.push(problem(
                Severity::Error,
                "walls_over_cap",
                format!(
                    "{} wall segments; nd-world stores at most {ND_WORLD_WALL_CAP}, so a publish would be refused.",
                    walls.len()
                ),
            ));
        }
    }
    if floor.lights.len() > MAX_LIGHTS {
        out.push(problem(
            Severity::Error,
            "lights_over_cap",
            format!(
                "{} lights; nd-world stores at most {MAX_LIGHTS}.",
                floor.lights.len()
            ),
        ));
    }

    // Marks: dedupe the mirrored pair (we store a mark on both sides of a shared edge).
    let mut seen: BTreeSet<(i32, i32, bool)> = BTreeSet::new(); // (x, y, horizontal)
    for (key, mark) in &grid.marks {
        let (x, y) = (key.cell.x, key.cell.y);
        let canon = match key.edge {
            Edge::West => (x, y, false),
            Edge::East => (x + 1, y, false),
            Edge::North => (x, y, true),
            Edge::South => (x, y + 1, true),
        };
        if !seen.insert(canon) {
            continue;
        }
        let here = grid.cell_space(key.cell);
        let there = grid.cell_space(neighbor(key.cell, key.edge));
        let what = match mark.kind {
            MarkKind::Opening => "opening",
            MarkKind::Door => "door",
            MarkKind::Window => "window",
            MarkKind::Secret => "secret door",
        };
        let remove = Some(Op::SetMark {
            edge: *key,
            mark: None,
        });
        let msg = match (here, there) {
            (None, None) => Some((
                "mark_on_nothing",
                format!("A {what} at ({x},{y}) has no room on either side."),
            )),
            (Some(a), Some(b)) if a == b => Some((
                "mark_inside_room",
                format!("A {what} at ({x},{y}) sits inside one room, where there is no wall to put it in."),
            )),
            (Some(_), None) | (None, Some(_)) if mark.kind != MarkKind::Window => Some((
                "door_to_void",
                format!("A {what} at ({x},{y}) opens into empty space."),
            )),
            _ => None,
        };
        if let Some((code, message)) = msg {
            out.push(Problem {
                severity: Severity::Warning,
                code,
                message,
                at: Some(key.cell),
                fix: remove,
            });
        }
    }

    // Reachability: cells of one space are joined; different spaces join only
    // across a door, secret door or opening (a window does not let anyone through).
    let cells: Vec<(CellKey, &str)> = grid.cells().collect();
    if cells.len() > 1 {
        let index: BTreeMap<CellKey, usize> = cells
            .iter()
            .enumerate()
            .map(|(i, (c, _))| (*c, i))
            .collect();
        let mut dsu = Dsu((0..cells.len()).collect());
        for (i, (c, space)) in cells.iter().enumerate() {
            for edge in [Edge::East, Edge::South] {
                let n = neighbor(*c, edge);
                let Some(&j) = index.get(&n) else { continue };
                let passable = cells[j].1 == *space
                    || matches!(
                        grid.mark_between(*c, edge).map(|m| m.kind),
                        Some(MarkKind::Door | MarkKind::Secret | MarkKind::Opening)
                    );
                if passable {
                    dsu.union(i, j);
                }
            }
        }
        let mut comps: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..cells.len() {
            let r = dsu.find(i);
            comps.entry(r).or_default().push(i);
        }
        if comps.len() > 1 {
            let main = comps
                .iter()
                .max_by_key(|(_, v)| v.len())
                .map(|(r, _)| *r)
                .expect("non-empty");
            for (r, members) in &comps {
                if *r == main {
                    continue;
                }
                let names: BTreeSet<&str> = members
                    .iter()
                    .filter_map(|&i| grid.spaces.get(cells[i].1).map(|s| s.name.as_str()))
                    .collect();
                let first = cells[members[0]].0;
                out.push(Problem {
                    severity: Severity::Warning,
                    code: "unreachable_room",
                    message: format!(
                        "{} ({} squares) cannot be reached: no door or opening connects it to the rest of the map.",
                        if names.is_empty() {
                            "An area".to_string()
                        } else {
                            names.into_iter().collect::<Vec<_>>().join(", ")
                        },
                        members.len()
                    ),
                    at: Some(first),
                    fix: None,
                });
            }
        }
    }

    // Lights in the dark, and spaces nobody painted.
    for l in &floor.lights {
        let cell = CellKey::new(
            (l.x / cell_size).floor() as i32,
            (l.y / cell_size).floor() as i32,
        );
        if grid.cell_space(cell).is_none() {
            out.push(Problem {
                severity: Severity::Warning,
                code: "light_in_void",
                message: format!(
                    "Light {} at ({}, {}) is outside every room.",
                    l.label.as_deref().unwrap_or(&l.id),
                    cell.x,
                    cell.y
                ),
                at: Some(cell),
                fix: Some(Op::RemoveLight { id: l.id.clone() }),
            });
        }
    }
    let used: BTreeSet<&str> = grid.cells().map(|(_, s)| s).collect();
    for (id, s) in &grid.spaces {
        if !used.contains(id.as_str()) {
            out.push(Problem {
                severity: Severity::Info,
                code: "space_unused",
                message: format!("Space '{}' has no painted squares.", s.name),
                at: None,
                fix: Some(Op::RemoveSpace { id: id.clone() }),
            });
        }
    }

    out.sort_by(|a, b| {
        (a.severity, a.code, a.at.map(|c| (c.y, c.x))).cmp(&(
            b.severity,
            b.code,
            b.at.map(|c| (c.y, c.x)),
        ))
    });
    out
}

/// True when nd-world would refuse the publish.
pub fn blocks_publish(problems: &[Problem]) -> bool {
    problems.iter().any(|p| p.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{default_space, EdgeKey, Mark, SpaceKind};
    use crate::journal::apply_op;

    fn floor() -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), "G".into());
        f.grid
            .upsert_space(default_space("a", "Hall", SpaceKind::Room))
            .unwrap();
        f.grid
            .upsert_space(default_space("b", "Vault", SpaceKind::Room))
            .unwrap();
        f.grid
            .paint(&[CellKey::new(0, 0), CellKey::new(1, 0)], Some("a"))
            .unwrap();
        f.grid.paint(&[CellKey::new(2, 0)], Some("b")).unwrap();
        f
    }
    fn codes(p: &[Problem]) -> Vec<&'static str> {
        p.iter().map(|p| p.code).collect()
    }
    fn mark(f: &mut FloorDoc, x: i32, y: i32, e: Edge, k: MarkKind) {
        f.grid
            .set_mark(
                EdgeKey {
                    cell: CellKey::new(x, y),
                    edge: e,
                },
                Some(Mark::new(k)),
            )
            .unwrap();
    }

    #[test]
    fn a_walled_off_room_is_unreachable_and_a_door_fixes_it() {
        let mut f = floor();
        let p = lint(&f, 32.0);
        assert_eq!(codes(&p), ["unreachable_room"]);
        assert!(p[0].message.contains("Vault"));
        mark(&mut f, 1, 0, Edge::East, MarkKind::Door);
        assert!(lint(&f, 32.0).is_empty());
    }

    #[test]
    fn a_window_does_not_connect_rooms_but_an_opening_does() {
        let mut f = floor();
        mark(&mut f, 1, 0, Edge::East, MarkKind::Window);
        assert_eq!(codes(&lint(&f, 32.0)), ["unreachable_room"]);
        mark(&mut f, 1, 0, Edge::East, MarkKind::Opening);
        assert!(lint(&f, 32.0).is_empty());
    }

    #[test]
    fn misplaced_marks_are_reported_once_with_a_working_fix() {
        let mut f = floor();
        mark(&mut f, 1, 0, Edge::East, MarkKind::Door); // fine, joins the rooms
        mark(&mut f, 0, 0, Edge::East, MarkKind::Door); // inside the Hall
        mark(&mut f, 0, 0, Edge::North, MarkKind::Door); // to the void
        mark(&mut f, 9, 9, Edge::North, MarkKind::Door); // on nothing
        mark(&mut f, 0, 0, Edge::South, MarkKind::Window); // a window to the void is fine
        let p = lint(&f, 32.0);
        assert_eq!(
            codes(&p),
            ["door_to_void", "mark_inside_room", "mark_on_nothing"]
        );
        // Applying every fix leaves a clean map.
        for pr in &p {
            let (op, _) = (pr.fix.clone().expect("fixable"), ());
            apply_op(&mut f, &op).unwrap();
        }
        assert!(lint(&f, 32.0).is_empty(), "{:?}", lint(&f, 32.0));
    }

    #[test]
    fn lights_in_void_and_unused_spaces() {
        let mut f = floor();
        mark(&mut f, 1, 0, Edge::East, MarkKind::Door);
        f.add_light(crate::doc::Light {
            id: "l1".into(),
            x: 500.0,
            y: 500.0,
            range: 4.0,
            color: "#ffffff".into(),
            intensity: 1.0,
            on: true,
            label: Some("Torch".into()),
        })
        .unwrap();
        f.grid
            .upsert_space(default_space("c", "Spare", SpaceKind::Area))
            .unwrap();
        let p = lint(&f, 32.0);
        assert_eq!(codes(&p), ["light_in_void", "space_unused"]);
        assert!(p[0].message.contains("Torch"));
        assert_eq!(p[1].severity, Severity::Info);
    }

    #[test]
    fn caps_are_errors_that_block_publishing() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        assert_eq!(codes(&lint(&f, 32.0)), ["empty_floor"]);
        f.grid
            .upsert_space(default_space("a", "A", SpaceKind::Room))
            .unwrap();
        // Isolated single cells two apart never merge into runs: 40x40 of
        // them is 1,600 cells x 4 edges = 6,400 segments, over the 5,000 cap.
        // (A checkerboard would not do: its edges line up and merge.)
        let mut cells = Vec::new();
        for y in 0..40 {
            for x in 0..40 {
                cells.push(CellKey::new(x * 2, y * 2));
            }
        }
        f.grid.paint(&cells, Some("a")).unwrap();
        let p = lint(&f, 32.0);
        assert!(p
            .iter()
            .any(|p| p.code == "walls_over_cap" && p.severity == Severity::Error));
        assert!(blocks_publish(&p));
        assert_eq!(p[0].code, "walls_over_cap", "errors sort first");
    }
}
