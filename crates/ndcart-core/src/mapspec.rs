//! MapSpec: a map described as a room graph, not as pixels (plan §8). Names,
//! sizes and which rooms connect; the compiler lays it out deterministically
//! and returns ordinary ops. An AI (or a person) writes the spec; the layout
//! is always the same code, so a spec can be re-rolled with another seed, is
//! validated before anything is drawn, and can never place a wall wrongly.

use crate::error::{CoreError, CoreResult};
use crate::gen::{Canvas, Rng};
use crate::grid::{MarkKind, SpaceKind};
use crate::journal::Op;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_FLOORS: usize = 12;
pub const MAX_ROOMS: usize = 60;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct MapSpec {
    pub name: String,
    pub floors: Vec<FloorSpec>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct FloorSpec {
    pub name: String,
    pub rooms: Vec<RoomSpec>,
    #[serde(default)]
    pub links: Vec<LinkSpec>,
    #[serde(default)]
    pub stairs: Vec<StairSpec>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct RoomSpec {
    pub id: String,
    pub name: String,
    #[serde(default = "room_kind")]
    pub kind: SpaceKind,
    /// Size in squares, 2..=30.
    pub w: i32,
    pub h: i32,
    /// A lamp in the room (default true).
    #[serde(default = "yes")]
    pub lit: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct LinkSpec {
    pub a: String,
    pub b: String,
    #[serde(default = "door")]
    pub kind: LinkKind,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Door,
    Opening,
    Secret,
}

/// Stairs in `room` leading to `to_floor` (by name); `to_room` lands in that room.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct StairSpec {
    pub room: String,
    pub to_floor: String,
    #[serde(default)]
    pub to_room: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

fn yes() -> bool {
    true
}
fn room_kind() -> SpaceKind {
    SpaceKind::Room
}
fn door() -> LinkKind {
    LinkKind::Door
}

/// A laid-out floor: ops to apply, and where each room's centre ended up
/// (cells), so stairs can be placed once floor ids exist.
#[derive(Clone, Debug)]
pub struct CompiledFloor {
    pub name: String,
    pub ops: Vec<Op>,
    pub centres: BTreeMap<String, (i32, i32)>,
}

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl MapSpec {
    /// Every rule a spec must meet before it is laid out. The messages are
    /// written to be handed back to an AI that produced a bad spec.
    pub fn validate(&self) -> CoreResult<()> {
        let bad = |m: String| Err(CoreError::Invalid(m));
        let n = self.name.chars().count();
        if n == 0 || n > 80 {
            return bad("name must be 1-80 characters".into());
        }
        if self.floors.is_empty() || self.floors.len() > MAX_FLOORS {
            return bad(format!("floors must hold 1-{MAX_FLOORS} floors"));
        }
        let names: BTreeSet<&str> = self.floors.iter().map(|f| f.name.as_str()).collect();
        if names.len() != self.floors.len()
            || names.iter().any(|n| n.is_empty() || n.chars().count() > 40)
        {
            return bad("floor names must be distinct and 1-40 characters".into());
        }
        for f in &self.floors {
            if f.rooms.is_empty() || f.rooms.len() > MAX_ROOMS {
                return bad(format!("floor '{}' needs 1-{MAX_ROOMS} rooms", f.name));
            }
            let mut ids = BTreeSet::new();
            for r in &f.rooms {
                if !id_ok(&r.id) || !ids.insert(r.id.as_str()) {
                    return bad(format!(
                        "floor '{}': room ids must be unique, 1-40 chars of a-z 0-9 _ -",
                        f.name
                    ));
                }
                let n = r.name.chars().count();
                if n == 0 || n > 60 {
                    return bad(format!("room '{}' needs a name of 1-60 characters", r.id));
                }
                if !(2..=30).contains(&r.w) || !(2..=30).contains(&r.h) {
                    return bad(format!("room '{}' must be 2-30 squares each way", r.id));
                }
            }
            for l in &f.links {
                if l.a == l.b {
                    return bad(format!("a room cannot link to itself ('{}')", l.a));
                }
                for end in [&l.a, &l.b] {
                    if !ids.contains(end.as_str()) {
                        return bad(format!(
                            "floor '{}': link names an unknown room '{end}'",
                            f.name
                        ));
                    }
                }
            }
            for s in &f.stairs {
                if !ids.contains(s.room.as_str()) {
                    return bad(format!(
                        "floor '{}': stairs in an unknown room '{}'",
                        f.name, s.room
                    ));
                }
                let Some(target) = self.floors.iter().find(|x| x.name == s.to_floor) else {
                    return bad(format!("stairs lead to an unknown floor '{}'", s.to_floor));
                };
                if target.name == f.name {
                    return bad("stairs must lead to a different floor".into());
                }
                if let Some(tr) = &s.to_room {
                    if !target.rooms.iter().any(|r| &r.id == tr) {
                        return bad(format!(
                            "stairs land in an unknown room '{tr}' on '{}'",
                            target.name
                        ));
                    }
                }
            }
            // Every room must be reachable from the first through links.
            let mut seen: BTreeSet<&str> = BTreeSet::from([f.rooms[0].id.as_str()]);
            let mut grew = true;
            while grew {
                grew = false;
                for l in &f.links {
                    let (a, b) = (seen.contains(l.a.as_str()), seen.contains(l.b.as_str()));
                    if a != b {
                        seen.insert(if a { l.b.as_str() } else { l.a.as_str() });
                        grew = true;
                    }
                }
            }
            if let Some(r) = f.rooms.iter().find(|r| !seen.contains(r.id.as_str())) {
                return bad(format!(
                    "floor '{}': room '{}' is not linked to the rest; every room needs a path from '{}'",
                    f.name, r.id, f.rooms[0].id
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Placed {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn overlaps(a: &Placed, b: &Placed, margin: i32) -> bool {
    a.x < b.x + b.w + margin
        && a.x + a.w + margin > b.x
        && a.y < b.y + b.h + margin
        && a.y + a.h + margin > b.y
}

/// Lay out one floor. Rooms are placed outward from the first along the link
/// tree, a corridor of 2-6 squares between neighbours; extra links get an
/// L-shaped corridor.
pub fn compile_floor(f: &FloorSpec, seed: u64) -> CoreResult<CompiledFloor> {
    compile_floor_styled(f, seed, &[])
}

/// Like [`compile_floor`], with the project's room palette.
pub fn compile_floor_styled(
    f: &FloorSpec,
    seed: u64,
    palette: &[String],
) -> CoreResult<CompiledFloor> {
    let mut rng = Rng(seed ^ 0xA5A5_5A5A_1234_8765);
    let idx: BTreeMap<&str, usize> = f
        .rooms
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let mut placed: Vec<Option<Placed>> = vec![None; f.rooms.len()];
    placed[0] = Some(Placed {
        x: 0,
        y: 0,
        w: f.rooms[0].w,
        h: f.rooms[0].h,
    });
    // (parent, child, link kind) in placement order = a spanning tree by BFS.
    let mut tree: Vec<(usize, usize, LinkKind)> = Vec::new();
    let mut tree_links: BTreeSet<usize> = BTreeSet::new();
    let mut queue = vec![0usize];
    let mut qi = 0;
    while qi < queue.len() {
        let p = queue[qi];
        qi += 1;
        for (li, l) in f.links.iter().enumerate() {
            let other = if idx[l.a.as_str()] == p {
                idx[l.b.as_str()]
            } else if idx[l.b.as_str()] == p {
                idx[l.a.as_str()]
            } else {
                continue;
            };
            if placed[other].is_some() {
                continue;
            }
            let (w, h) = (f.rooms[other].w, f.rooms[other].h);
            let par = placed[p].expect("placed");
            let all: Vec<Placed> = placed.iter().flatten().copied().collect();
            let start = rng.range(0, 3);
            let mut spot = None;
            'search: for gap in 3..=9 {
                for turn in 0..4 {
                    let dir = (start + turn) % 4;
                    // Offsets along the shared side, best-aligned first.
                    let span = if dir % 2 == 0 { par.h + h } else { par.w + w };
                    let mut offs: Vec<i32> = (1..span).map(|o| o - h.max(w)).collect();
                    offs.sort_by_key(|o| {
                        (o + (if dir % 2 == 0 { h } else { w }) / 2
                            - (if dir % 2 == 0 { par.h } else { par.w }) / 2)
                            .abs()
                    });
                    for o in offs {
                        let c = match dir {
                            0 => Placed {
                                x: par.x + par.w + gap,
                                y: par.y + o,
                                w,
                                h,
                            },
                            1 => Placed {
                                x: par.x + o,
                                y: par.y + par.h + gap,
                                w,
                                h,
                            },
                            2 => Placed {
                                x: par.x - gap - w,
                                y: par.y + o,
                                w,
                                h,
                            },
                            _ => Placed {
                                x: par.x + o,
                                y: par.y - gap - h,
                                w,
                                h,
                            },
                        };
                        let shared = if dir % 2 == 0 {
                            c.y.max(par.y) < (c.y + h).min(par.y + par.h)
                        } else {
                            c.x.max(par.x) < (c.x + w).min(par.x + par.w)
                        };
                        if shared && !all.iter().any(|a| overlaps(a, &c, 1)) {
                            spot = Some((c, dir));
                            break 'search;
                        }
                    }
                }
            }
            let Some((c, _)) = spot else {
                return Err(CoreError::Invalid(format!(
                    "could not fit room '{}' next to '{}'; make rooms smaller or fewer",
                    f.rooms[other].id, f.rooms[p].id
                )));
            };
            placed[other] = Some(c);
            tree.push((p, other, l.kind));
            tree_links.insert(li);
            queue.push(other);
        }
    }
    let rects: Vec<Placed> = placed
        .iter()
        .map(|p| p.expect("every room is linked"))
        .collect();
    let (minx, miny) = (
        rects.iter().map(|r| r.x).min().unwrap_or(0),
        rects.iter().map(|r| r.y).min().unwrap_or(0),
    );
    let (maxx, maxy) = (
        rects.iter().map(|r| r.x + r.w).max().unwrap_or(0),
        rects.iter().map(|r| r.y + r.h).max().unwrap_or(0),
    );
    let (ox, oy) = (2 - minx, 2 - miny);
    let (cw, ch) = (maxx - minx + 4, maxy - miny + 4);
    if cw > 4000 || ch > 4000 {
        return Err(CoreError::Invalid("the layout came out too large".into()));
    }
    let mut c = Canvas::new(cw, ch);
    c.palette = palette.to_vec();
    let ids: Vec<u16> = f
        .rooms
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let colour = if r.kind == SpaceKind::Corridor {
                "#4a4e57".to_string()
            } else {
                c.pal(i)
            };
            let s = c.space(&r.name, &colour, r.kind);
            let p = rects[i];
            c.rect(p.x + ox, p.y + oy, p.x + p.w + ox, p.y + p.h + oy, s, true);
            s
        })
        .collect();
    let corridor = c.space("Passage", "#4a4e57", SpaceKind::Corridor);
    let mark_of = |k: LinkKind| match k {
        LinkKind::Door => MarkKind::Door,
        LinkKind::Opening => MarkKind::Opening,
        LinkKind::Secret => MarkKind::Secret,
    };
    // Every link (tree or extra): an L corridor between the two room centres,
    // with a mark where it leaves one room and enters the next.
    let all_links: Vec<(usize, usize, LinkKind)> = f
        .links
        .iter()
        .map(|l| (idx[l.a.as_str()], idx[l.b.as_str()], l.kind))
        .collect();
    let _ = (&tree, &tree_links);
    for (a, b, kind) in all_links {
        let (ra, rb) = (rects[a], rects[b]);
        let (ax, ay) = (ra.x + ra.w / 2 + ox, ra.y + ra.h / 2 + oy);
        let (bx, by) = (rb.x + rb.w / 2 + ox, rb.y + rb.h / 2 + oy);
        let mut path = vec![(ax, ay)];
        let (mut x, mut y) = (ax, ay);
        while x != bx {
            x += (bx - x).signum();
            path.push((x, y));
        }
        while y != by {
            y += (by - y).signum();
            path.push((x, y));
        }
        // A mark wherever the path crosses a room boundary (a room it passes
        // through gets plain doors, so nothing is cut off). The link's own kind
        // goes where it leaves room `a` and where it enters room `b`.
        let is_room = |s: u16| ids.contains(&s);
        let (mut leaving, mut entered) = (false, false);
        for w in path.windows(2) {
            let ((px, py), (qx, qy)) = (w[0], w[1]);
            if c.at(qx, qy) == 0 {
                c.set(qx, qy, corridor);
            }
            let (sp, sq) = (c.at(px, py), c.at(qx, qy));
            let crossing = is_room(sp) != is_room(sq) || (is_room(sp) && is_room(sq) && sp != sq);
            if !crossing {
                continue;
            }
            let mut kind_here = MarkKind::Door;
            if sp == ids[a] && !leaving {
                kind_here = mark_of(kind);
                leaving = true;
            } else if sq == ids[b] && !entered {
                kind_here = mark_of(kind);
                entered = true;
            }
            c.mark(px, py, qx, qy, kind_here);
        }
    }
    let mut centres = BTreeMap::new();
    for (i, r) in f.rooms.iter().enumerate() {
        let p = rects[i];
        centres.insert(r.id.clone(), (p.x + p.w / 2 + ox, p.y + p.h / 2 + oy));
        if r.lit && r.kind != SpaceKind::Corridor {
            c.label_room(ids[i], 0);
        } else {
            // A name without a lamp.
            let lights = c.lights.len();
            c.label_room(ids[i], usize::MAX);
            debug_assert_eq!(lights, c.lights.len());
        }
    }
    let ops = c.into_ops();
    Ok(CompiledFloor {
        name: f.name.clone(),
        ops,
        centres,
    })
}

/// Describe an existing floor as a spec (for refining it): each room-kind
/// space becomes a room sized by its bounding box; two rooms are linked when a
/// door, opening or secret door joins them, directly or through corridors.
/// Spaces too big for a spec (over 30 squares across) are left out, and so
/// are the links that depend on them.
pub fn floor_to_spec(floor: &crate::doc::FloorDoc, light_cell: f64) -> FloorSpec {
    use crate::grid::{CellKey, Edge, MarkKind};
    let grid = &floor.grid;
    let mut bbox: BTreeMap<&str, (i32, i32, i32, i32)> = BTreeMap::new();
    for (c, sp) in grid.cells() {
        let e = bbox.entry(sp).or_insert((c.x, c.y, c.x, c.y));
        *e = (e.0.min(c.x), e.1.min(c.y), e.2.max(c.x), e.3.max(c.y));
    }
    let lit: BTreeSet<&str> = floor
        .lights
        .iter()
        .filter_map(|l| {
            grid.cell_space(CellKey {
                x: (l.x / light_cell).floor() as i32,
                y: (l.y / light_cell).floor() as i32,
            })
        })
        .collect();
    let clean = |id: &str| -> String {
        let t: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .take(40)
            .collect();
        if t.is_empty() {
            "room".into()
        } else {
            t
        }
    };
    let mut rooms: Vec<RoomSpec> = Vec::new();
    let mut ids: BTreeMap<&str, String> = BTreeMap::new();
    for (id, sp) in &grid.spaces {
        let Some(b) = bbox.get(id.as_str()) else {
            continue;
        };
        let (w, h) = (b.2 - b.0 + 1, b.3 - b.1 + 1);
        if sp.kind == SpaceKind::Corridor || w > 30 || h > 30 || rooms.len() >= MAX_ROOMS {
            continue;
        }
        let rid = clean(id);
        ids.insert(id.as_str(), rid.clone());
        rooms.push(RoomSpec {
            id: rid,
            name: sp.name.chars().take(60).collect(),
            kind: sp.kind,
            w: w.max(2),
            h: h.max(2),
            lit: lit.contains(id.as_str()),
        });
    }
    // Space-to-space joins through marks, then rooms joined through corridor chains.
    let mut joins: BTreeMap<&str, BTreeMap<&str, LinkKind>> = BTreeMap::new();
    for (c, sp) in grid.cells() {
        for e in [Edge::East, Edge::South] {
            let n = match e {
                Edge::East => c.east(),
                _ => c.south(),
            };
            let Some(other) = grid.cell_space(n) else {
                continue;
            };
            if other == sp {
                continue;
            }
            let kind = match grid.mark_between(c, e).map(|m| m.kind) {
                Some(MarkKind::Door) => LinkKind::Door,
                Some(MarkKind::Opening) => LinkKind::Opening,
                Some(MarkKind::Secret) => LinkKind::Secret,
                _ => continue,
            };
            joins.entry(sp).or_default().entry(other).or_insert(kind);
            joins.entry(other).or_default().entry(sp).or_insert(kind);
        }
    }
    let corridor = |id: &str| {
        grid.spaces
            .get(id)
            .is_some_and(|s| s.kind == SpaceKind::Corridor)
    };
    let mut links: Vec<LinkSpec> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for (start, rid) in &ids {
        // Walk out through corridor spaces only.
        let mut stack: Vec<(&str, LinkKind)> = joins
            .get(start)
            .map(|m| m.iter().map(|(k, v)| (*k, *v)).collect())
            .unwrap_or_default();
        let mut visited: BTreeSet<&str> = BTreeSet::from([*start]);
        while let Some((id, kind)) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            if corridor(id) {
                if let Some(m) = joins.get(id) {
                    stack.extend(m.keys().map(|k| (*k, kind)));
                }
            } else if let Some(other) = ids.get(id) {
                let key = if rid < other {
                    (rid.clone(), other.clone())
                } else {
                    (other.clone(), rid.clone())
                };
                if seen.insert(key.clone()) {
                    links.push(LinkSpec {
                        a: key.0,
                        b: key.1,
                        kind,
                    });
                }
            }
        }
    }
    links.sort_by(|a, b| (&a.a, &a.b).cmp(&(&b.a, &b.b)));
    FloorSpec {
        name: floor.name.chars().take(40).collect(),
        rooms,
        links,
        stairs: Vec::new(),
    }
}

/// Compile every floor.
pub fn compile(spec: &MapSpec, seed: u64) -> CoreResult<Vec<CompiledFloor>> {
    compile_styled(spec, seed, &[])
}

/// [`compile`] with the project's room palette.
pub fn compile_styled(
    spec: &MapSpec,
    seed: u64,
    palette: &[String],
) -> CoreResult<Vec<CompiledFloor>> {
    spec.validate()?;
    spec.floors
        .iter()
        .enumerate()
        .map(|(i, f)| compile_floor_styled(f, seed.wrapping_add(i as u64 * 7919), palette))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::FloorDoc;
    use crate::journal::Journal;

    fn room(id: &str, name: &str, w: i32, h: i32) -> RoomSpec {
        RoomSpec {
            id: id.into(),
            name: name.into(),
            kind: SpaceKind::Room,
            w,
            h,
            lit: true,
        }
    }
    fn link(a: &str, b: &str, kind: LinkKind) -> LinkSpec {
        LinkSpec {
            a: a.into(),
            b: b.into(),
            kind,
        }
    }
    fn station() -> MapSpec {
        MapSpec {
            name: "Metro".into(),
            floors: vec![
                FloorSpec {
                    name: "Platform".into(),
                    rooms: vec![
                        room("hall", "Ticket Hall", 8, 6),
                        room("plat", "Platform", 14, 4),
                        room("office", "Control Office", 5, 5),
                        room("store", "Store", 4, 4),
                    ],
                    links: vec![
                        link("hall", "plat", LinkKind::Opening),
                        link("hall", "office", LinkKind::Door),
                        link("office", "store", LinkKind::Secret),
                    ],
                    stairs: vec![StairSpec {
                        room: "plat".into(),
                        to_floor: "Tunnels".into(),
                        to_room: Some("tun".into()),
                        label: None,
                    }],
                },
                FloorSpec {
                    name: "Tunnels".into(),
                    rooms: vec![
                        room("tun", "Service Tunnel", 12, 3),
                        room("pump", "Pump Room", 6, 6),
                    ],
                    links: vec![link("tun", "pump", LinkKind::Door)],
                    stairs: vec![],
                },
            ],
        }
    }

    fn apply(c: &CompiledFloor) -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), c.name.clone());
        let mut j = Journal::default();
        for op in &c.ops {
            j.push(&mut f, op).unwrap();
        }
        f
    }

    #[test]
    fn a_valid_spec_lays_out_every_named_room_connected_with_the_right_marks() {
        let floors = compile(&station(), 11).unwrap();
        assert_eq!(floors.len(), 2);
        let f = apply(&floors[0]);
        let names: BTreeSet<&str> = f.grid.spaces.values().map(|s| s.name.as_str()).collect();
        for n in ["Ticket Hall", "Platform", "Control Office", "Store"] {
            assert!(names.contains(n), "{n} in {names:?}");
        }
        // Room sizes are exact.
        let hall = f
            .grid
            .spaces
            .values()
            .find(|s| s.name == "Ticket Hall")
            .unwrap();
        assert_eq!(f.grid.cells().filter(|(_, s)| *s == hall.id).count(), 48);
        // The map is one connected piece, including the secret link (counts as passable).
        let problems = crate::lint::lint(&f, 32.0);
        assert!(
            !problems.iter().any(|p| p.code == "unreachable_room"),
            "{problems:?}"
        );
        assert!(!crate::lint::blocks_publish(&problems));
        // Marks: one opening, doors, one secret pair.
        let kinds: Vec<MarkKind> = f.grid.marks.values().map(|m| m.kind).collect();
        assert!(
            kinds.contains(&MarkKind::Opening)
                && kinds.contains(&MarkKind::Door)
                && kinds.contains(&MarkKind::Secret)
        );
        assert_eq!(floors[0].centres.len(), 4);
    }

    #[test]
    fn an_existing_floor_describes_itself_as_a_spec_that_lays_out_again() {
        let floors = compile(&station(), 11).unwrap();
        let f = apply(&floors[0]);
        let spec = floor_to_spec(&f, 32.0);
        let names: BTreeSet<&str> = spec.rooms.iter().map(|r| r.name.as_str()).collect();
        for n in ["Ticket Hall", "Platform", "Control Office", "Store"] {
            assert!(names.contains(n), "{n} in {names:?}");
        }
        let hall = spec.rooms.iter().find(|r| r.name == "Ticket Hall").unwrap();
        assert_eq!((hall.w, hall.h), (8, 6));
        // The hall-to-office door and the office-to-store secret door survive as links.
        let by_name = |id: &str| spec.rooms.iter().find(|r| r.id == id).unwrap().name.clone();
        let pairs: BTreeSet<(String, String, LinkKind)> = spec
            .links
            .iter()
            .map(|l| {
                let (a, b) = (by_name(&l.a), by_name(&l.b));
                let (a, b) = if a < b { (a, b) } else { (b, a) };
                (a, b, l.kind)
            })
            .collect();
        assert!(
            pairs.contains(&(
                "Control Office".into(),
                "Ticket Hall".into(),
                LinkKind::Door
            )),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&("Control Office".into(), "Store".into(), LinkKind::Secret)),
            "{pairs:?}"
        );
        // And it round-trips: the described floor is a valid spec that lays out.
        let again = MapSpec {
            name: "again".into(),
            floors: vec![spec],
        };
        again.validate().unwrap();
        compile(&again, 3).unwrap();
    }

    #[test]
    fn the_project_palette_colours_the_rooms_in_order() {
        let pal = vec!["#112233".to_string(), "#445566".to_string()];
        let c = compile_styled(&station(), 2, &pal).unwrap();
        let f = apply(&c[0]);
        let cols: BTreeSet<&str> = f
            .grid
            .spaces
            .values()
            .filter(|s| s.kind == SpaceKind::Room)
            .map(|s| s.color.as_str())
            .collect();
        assert_eq!(cols, BTreeSet::from(["#112233", "#445566"]));
        // No palette: the built-in colours as before.
        let d = apply(&compile(&station(), 2).unwrap()[0]);
        assert!(d.grid.spaces.values().any(|s| s.color == "#7f5539"));
    }

    #[test]
    fn layout_is_deterministic_and_the_seed_rerolls_it() {
        let a = apply(&compile(&station(), 5).unwrap()[0]);
        assert_eq!(a, apply(&compile(&station(), 5).unwrap()[0]));
        let mut differs = false;
        for seed in 6..16 {
            if apply(&compile(&station(), seed).unwrap()[0]) != a {
                differs = true;
            }
        }
        assert!(differs, "another seed gives another arrangement");
    }

    #[test]
    fn bad_specs_are_refused_with_a_message_an_ai_can_act_on() {
        let msg = |mut m: MapSpec, edit: &dyn Fn(&mut MapSpec)| {
            edit(&mut m);
            m.validate().unwrap_err().to_string()
        };
        assert!(
            msg(station(), &|m| m.floors[0].links.pop().map(|_| ()).unwrap())
                .contains("not linked")
        );
        assert!(msg(station(), &|m| m.floors[0].links[0].b = "nope".into())
            .contains("unknown room 'nope'"));
        assert!(msg(station(), &|m| m.floors[0].rooms[0].w = 99).contains("2-30"));
        assert!(msg(station(), &|m| m.floors[0].rooms[1].id = "hall".into()).contains("unique"));
        assert!(msg(station(), &|m| m.floors[0].stairs[0].to_floor =
            "Roof".into())
        .contains("unknown floor 'Roof'"));
        assert!(msg(station(), &|m| m.floors[0].stairs[0].to_room =
            Some("zzz".into()))
        .contains("unknown room 'zzz'"));
        assert!(msg(station(), &|m| m.floors[1].name = "Platform".into()).contains("distinct"));
        assert!(msg(station(), &|m| m.floors.clear()).contains("1-12"));
        // And the JSON shape an AI would send parses, with defaults filled in.
        let spec: MapSpec = serde_json::from_str(
            r#"{"name":"T","floors":[{"name":"G","rooms":[{"id":"a","name":"A","w":4,"h":4},{"id":"b","name":"B","w":4,"h":4}],"links":[{"a":"a","b":"b"}]}]}"#,
        )
        .unwrap();
        spec.validate().unwrap();
        assert!(spec.floors[0].rooms[0].lit && spec.floors[0].links[0].kind == LinkKind::Door);
        assert!(serde_json::from_str::<MapSpec>(r#"{"name":"T","floors":[{"name":"G","rooms":[{"id":"a","name":"A","w":"big","h":4}]}]}"#).is_err());
    }

    #[test]
    fn a_crowded_floor_still_lays_out_or_says_so() {
        let rooms: Vec<RoomSpec> = (0..30)
            .map(|i| room(&format!("r{i}"), &format!("Cell {i}"), 5, 4))
            .collect();
        let links: Vec<LinkSpec> = (1..30)
            .map(|i| link(&format!("r{}", i / 2), &format!("r{i}"), LinkKind::Door))
            .collect();
        let spec = MapSpec {
            name: "Warren".into(),
            floors: vec![FloorSpec {
                name: "G".into(),
                rooms,
                links,
                stairs: vec![],
            }],
        };
        let f = apply(&compile(&spec, 3).unwrap()[0]);
        assert_eq!(
            f.grid
                .spaces
                .values()
                .filter(|s| s.name.starts_with("Cell"))
                .count(),
            30
        );
        let problems = crate::lint::lint(&f, 32.0);
        assert!(
            !problems.iter().any(|p| p.code == "unreachable_room"),
            "{problems:?}"
        );
    }
}
