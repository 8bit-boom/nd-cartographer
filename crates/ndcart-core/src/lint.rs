//! The linter ("Problems panel", plan §17.2): deterministic checks, no AI.
//! Catches what nobody sees by eye at 300 rooms — and what nd-world would
//! refuse. Each problem can carry a `fix` that is a normal undoable [`Op`].

use crate::doc::FloorDoc;
use crate::grid::{CellKey, Edge, MarkKind};
use crate::journal::Op;
use crate::walls::{floor_walls, ND_WORLD_WALL_CAP};
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

/// Stairs, ladders and lifts across the floors of one document: where a link
/// points nowhere (a floor was removed) or nothing leads back.
pub fn lint_links(floor: &FloorDoc, floors: &[FloorDoc]) -> Vec<Problem> {
    let mut out = Vec::new();
    for m in &floor.markers {
        let Some(l) = &m.links_to else { continue };
        let name = if m.label.is_empty() { &m.id } else { &m.label };
        let Some(target) = floors.iter().find(|f| f.id == l.floor) else {
            out.push(problem(
                Severity::Warning,
                "link_to_missing_floor",
                format!("'{name}' leads to a floor that no longer exists."),
            ));
            continue;
        };
        if target.id == floor.id {
            out.push(problem(
                Severity::Warning,
                "link_to_same_floor",
                format!("'{name}' leads to the floor it is on."),
            ));
        } else if !target
            .markers
            .iter()
            .any(|t| t.links_to.as_ref().is_some_and(|b| b.floor == floor.id))
        {
            out.push(problem(
                Severity::Info,
                "link_not_returned",
                format!(
                    "'{name}' leads to '{}', but nothing there leads back.",
                    target.name
                ),
            ));
        }
    }
    out
}

/// Lint one floor. `cell_size` is the lattice size lights are stored in.
pub fn lint(floor: &FloorDoc, cell_size: f64) -> Vec<Problem> {
    let grid = &floor.grid;
    let mut out = Vec::new();

    // "Empty" means nothing at all: a map opened from a picture has no rooms
    // but is not empty.
    let has_art = floor.underlay.is_some()
        || !floor.strokes.is_empty()
        || !floor.stamps.is_empty()
        || !floor.imported_walls.is_empty()
        || floor.terrain.is_some()
        || !floor.rivers.is_empty();
    if grid.cell_count() == 0 && !has_art {
        out.push(problem(
            Severity::Warning,
            "empty_floor",
            "Nothing is painted on this floor yet.".into(),
        ));
    }

    // nd-world's publish caps.
    if let Ok(walls) = floor_walls(floor, cell_size) {
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
        // A light is "in the void" only if neither a room nor the underlay is there.
        let on_underlay = floor.underlay.as_ref().is_some_and(|u| {
            let (cx, cy) = (l.x as f32 / cell_size as f32, l.y as f32 / cell_size as f32);
            cx >= u.x && cx <= u.x + u.w && cy >= u.y && cy <= u.y + u.h
        });
        if grid.cell_space(cell).is_none() && !on_underlay {
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

    extra_checks(floor, &mut out);

    out.sort_by(|a, b| {
        (a.severity, a.code, a.at.map(|c| (c.y, c.x))).cmp(&(
            b.severity,
            b.code,
            b.at.map(|c| (c.y, c.x)),
        ))
    });
    out
}

/// The finer checks: zones, tiny rooms, dead ends, secret-only rooms, repeated names.
fn extra_checks(floor: &FloorDoc, out: &mut Vec<Problem>) {
    let grid = &floor.grid;
    // Zones that point at rooms that are gone, or at nothing.
    for z in &floor.zones {
        let live: Vec<String> = z
            .spaces
            .iter()
            .filter(|id| grid.spaces.contains_key(id.as_str()))
            .cloned()
            .collect();
        if live.len() != z.spaces.len() && !live.is_empty() {
            let mut fixed = z.clone();
            fixed.spaces = live;
            out.push(Problem {
                severity: Severity::Warning,
                code: "zone_stale_room",
                message: format!("Zone '{}' lists rooms that no longer exist.", z.name),
                at: None,
                fix: Some(Op::UpsertZone { zone: fixed }),
            });
        } else if live.is_empty() {
            out.push(Problem {
                severity: Severity::Info,
                code: "zone_empty",
                message: format!("Zone '{}' has no rooms.", z.name),
                at: None,
                fix: Some(Op::RemoveZone { id: z.id.clone() }),
            });
        }
    }

    for l in &floor.entity_links {
        if !grid.spaces.contains_key(&l.space) {
            out.push(Problem {
                severity: Severity::Info,
                code: "entity_link_stale",
                message: format!(
                    "The link to '{}' points at a room that no longer exists.",
                    l.name
                ),
                at: None,
                fix: Some(Op::UnlinkEntity {
                    space: l.space.clone(),
                }),
            });
        }
    }

    let cells: Vec<(CellKey, &str)> = grid.cells().collect();
    let mut per_space: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, s) in &cells {
        *per_space.entry(s).or_default() += 1;
    }
    // Repeated names make "go to the Armory" ambiguous for the GM and for AI.
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for s in grid.spaces.values() {
        if s.kind == crate::grid::SpaceKind::Room && per_space.contains_key(s.id.as_str()) {
            *by_name.entry(s.name.to_lowercase()).or_default() += 1;
        }
    }
    for (name, n) in by_name.iter().filter(|(_, n)| **n > 1) {
        out.push(problem(
            Severity::Info,
            "duplicate_room_name",
            format!("{n} rooms are called '{name}'."),
        ));
    }

    // Dead-end corridors: a corridor that joins only one other space.
    let mut links: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (c, space) in &cells {
        for edge in [Edge::North, Edge::East, Edge::South, Edge::West] {
            let Some(other) = grid.cell_space(neighbor(*c, edge)) else {
                continue;
            };
            if other != *space
                && matches!(
                    grid.mark_between(*c, edge).map(|m| m.kind),
                    Some(MarkKind::Door | MarkKind::Secret | MarkKind::Opening)
                )
            {
                links.entry(space).or_default().insert(other);
            }
        }
    }
    for (id, s) in &grid.spaces {
        if s.kind == crate::grid::SpaceKind::Corridor
            && per_space.contains_key(id.as_str())
            && links.get(id.as_str()).map(|l| l.len()) == Some(1)
        {
            out.push(problem(
                Severity::Info,
                "corridor_dead_end",
                format!("Corridor '{}' leads to only one place.", s.name),
            ));
        }
    }

    // Rooms that can be reached only through a secret door.
    if cells.len() > 1 {
        let root_of = |allow_secret: bool| -> Vec<usize> {
            let index: BTreeMap<CellKey, usize> = cells
                .iter()
                .enumerate()
                .map(|(i, (c, _))| (*c, i))
                .collect();
            let mut dsu = Dsu((0..cells.len()).collect());
            for (i, (c, space)) in cells.iter().enumerate() {
                for edge in [Edge::East, Edge::South] {
                    let Some(&j) = index.get(&neighbor(*c, edge)) else {
                        continue;
                    };
                    let through = match grid.mark_between(*c, edge).map(|m| m.kind) {
                        Some(MarkKind::Door | MarkKind::Opening) => true,
                        Some(MarkKind::Secret) => allow_secret,
                        _ => false,
                    };
                    if cells[j].1 == *space || through {
                        dsu.union(i, j);
                    }
                }
            }
            (0..cells.len()).map(|i| dsu.find(i)).collect()
        };
        let (open, all) = (root_of(false), root_of(true));
        let biggest = |roots: &[usize]| -> usize {
            let mut n: BTreeMap<usize, usize> = BTreeMap::new();
            for r in roots {
                *n.entry(*r).or_default() += 1;
            }
            n.into_iter()
                .max_by_key(|(_, k)| *k)
                .map(|(r, _)| r)
                .unwrap_or(0)
        };
        let (main_open, main_all) = (biggest(&open), biggest(&all));
        let mut reported: BTreeSet<usize> = BTreeSet::new();
        for i in 0..cells.len() {
            // In the main map once secret doors count, but cut off without them.
            if all[i] == main_all && open[i] != main_open && reported.insert(open[i]) {
                let name = grid
                    .spaces
                    .get(cells[i].1)
                    .map(|s| s.name.as_str())
                    .unwrap_or("An area");
                out.push(Problem {
                    severity: Severity::Info,
                    code: "secret_only_room",
                    message: format!("{name} can be reached only through a secret door."),
                    at: Some(cells[i].0),
                    fix: None,
                });
            }
        }
    }
}

/// True when nd-world would refuse the publish.
pub fn blocks_publish(problems: &[Problem]) -> bool {
    problems.iter().any(|p| p.severity == Severity::Error)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finer_checks_find_zones_dead_ends_secret_rooms_and_repeats() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        let sp = |id: &str, name: &str, kind: SpaceKind| default_space(id, name, kind);
        for (id, name, kind) in [
            ("a", "Hall", SpaceKind::Room),
            ("b", "Vault", SpaceKind::Room),
            ("c", "Vault", SpaceKind::Room),
            ("k", "Passage", SpaceKind::Corridor),
        ] {
            f.grid.upsert_space(sp(id, name, kind)).unwrap();
        }
        // Row y=0:  a a | k | b ... and a secret door to c below b.
        let at = |x, y| CellKey { x, y };
        f.grid.paint(&[at(0, 0), at(1, 0)], Some("a")).unwrap();
        f.grid.paint(&[at(2, 0)], Some("k")).unwrap();
        f.grid.paint(&[at(3, 0)], Some("b")).unwrap();
        f.grid.paint(&[at(3, 1)], Some("c")).unwrap();
        let door = |g: &mut crate::grid::RoomGrid, x, y, edge, kind| {
            g.set_mark(
                EdgeKey {
                    cell: at(x, y),
                    edge,
                },
                Some(Mark::new(kind)),
            )
            .unwrap()
        };
        door(&mut f.grid, 1, 0, Edge::East, MarkKind::Door); // Hall - Passage
        door(&mut f.grid, 2, 0, Edge::East, MarkKind::Door); // Passage - Vault(b)
        door(&mut f.grid, 3, 0, Edge::South, MarkKind::Secret); // Vault(b) - Vault(c)
        f.zones.push(crate::zone::Zone {
            id: "z".into(),
            name: "East".into(),
            color: "#aabbcc".into(),
            spaces: vec!["b".into(), "gone".into()],
        });
        f.zones.push(crate::zone::Zone {
            id: "e".into(),
            name: "Empty".into(),
            color: "#aabbcc".into(),
            spaces: vec![],
        });
        let codes: Vec<&str> = lint(&f, 32.0).iter().map(|p| p.code).collect();
        for want in [
            "zone_stale_room",
            "zone_empty",
            "duplicate_room_name",
            "corridor_dead_end",
            "secret_only_room",
        ] {
            // Passage joins two spaces (Hall and Vault), so it is NOT a dead end.
            if want == "corridor_dead_end" {
                assert!(!codes.contains(&want), "{codes:?}");
            } else {
                assert!(codes.contains(&want), "{want} missing from {codes:?}");
            }
        }
        // Removing the Hall door makes the passage a dead end.
        f.grid
            .set_mark(
                EdgeKey {
                    cell: at(1, 0),
                    edge: Edge::East,
                },
                None,
            )
            .unwrap();
        let codes: Vec<&str> = lint(&f, 32.0).iter().map(|p| p.code).collect();
        assert!(codes.contains(&"corridor_dead_end"), "{codes:?}");
        // The stale zone's fix is a normal op that cleans the list.
        let fix = lint(&f, 32.0)
            .into_iter()
            .find(|p| p.code == "zone_stale_room")
            .and_then(|p| p.fix)
            .unwrap();
        apply_op(&mut f, &fix).unwrap();
        assert_eq!(f.zones[0].spaces, ["b"]);
    }

    #[test]
    fn stairs_are_checked_against_the_other_floors() {
        let stair = |to: &str| -> crate::overlay::Marker {
            serde_json::from_value(serde_json::json!({
                "id":"st","x":1.0,"y":1.0,"label":"Stairs","links_to":{"floor":to}
            }))
            .unwrap()
        };
        let mut a = FloorDoc::new("fl_a".into(), "Ground".into());
        let mut b = FloorDoc::new("fl_b".into(), "Upper".into());
        a.markers.push(stair("fl_b"));
        let codes = |f: &FloorDoc, all: &[FloorDoc]| {
            lint_links(f, all)
                .iter()
                .map(|p| p.code)
                .collect::<Vec<_>>()
        };
        assert_eq!(codes(&a, &[a.clone(), b.clone()]), ["link_not_returned"]);
        assert_eq!(codes(&a, &[a.clone()]), ["link_to_missing_floor"]);
        b.markers.push(stair("fl_a"));
        assert!(codes(&a, &[a.clone(), b.clone()]).is_empty());
        a.markers[0] = stair("fl_a");
        assert_eq!(codes(&a, &[a.clone(), b]), ["link_to_same_floor"]);
    }

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

    #[test]
    fn a_map_opened_from_a_picture_is_not_empty_and_its_lights_are_fine() {
        use crate::doc::{ImportedWall, Underlay};
        use crate::walls::WallKind;
        let mut f = FloorDoc::new("f".into(), "G".into());
        assert_eq!(codes(&lint(&f, 32.0)), ["empty_floor"]);
        f.set_underlay(Some(Underlay {
            asset: crate::hash::sha256_hex(b"x"),
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
            opacity: 1.0,
        }))
        .unwrap();
        f.set_imported_walls(vec![ImportedWall {
            pts: vec![[0.0, 0.0], [5.0, 0.0]],
            kind: WallKind::Wall,
            state: None,
        }])
        .unwrap();
        f.add_light(crate::doc::Light {
            id: "l".into(),
            x: 100.0,
            y: 100.0,
            range: 4.0,
            color: "#ffffff".into(),
            intensity: 1.0,
            on: true,
            label: None,
        })
        .unwrap();
        assert!(lint(&f, 32.0).is_empty(), "{:?}", lint(&f, 32.0));
        // A light off the picture is still flagged.
        f.add_light(crate::doc::Light {
            id: "far".into(),
            x: 9000.0,
            y: 100.0,
            range: 4.0,
            color: "#ffffff".into(),
            intensity: 1.0,
            on: true,
            label: None,
        })
        .unwrap();
        assert_eq!(codes(&lint(&f, 32.0)), ["light_in_void"]);
    }

    #[test]
    fn a_world_map_with_terrain_is_not_empty() {
        let mut f = FloorDoc::new("f".into(), "G".into());
        assert_eq!(codes(&lint(&f, 32.0)), ["empty_floor"]);
        f.terrain_stroke(&crate::terrain::TerrainStroke {
            mode: crate::terrain::TerrainMode::Raise,
            points: vec![[3.0, 3.0]],
            radius: 2.0,
            strength: 1.0,
            seed: 0,
        })
        .unwrap();
        assert!(lint(&f, 32.0).is_empty(), "{:?}", lint(&f, 32.0));
    }
}
