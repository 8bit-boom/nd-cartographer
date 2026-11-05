//! Furnishing: props for a room chosen from what the room IS (its name and
//! kind), never blocking a doorway. Deterministic for a seed; one batch of
//! `AddStamp` ops, so furnishing a whole wing is a single undo step. The GM
//! accepts per room by choosing which rooms to furnish.

use crate::doc::FloorDoc;
use crate::error::{CoreError, CoreResult};
use crate::grid::{CellKey, Edge, SpaceKind};
use crate::journal::Op;
use crate::stamp::{Stamp, MAX_STAMPS};
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Where {
    /// Against a wall.
    Wall,
    /// In a corner (two walls).
    Corner,
    /// Away from the walls.
    Middle,
}

/// One kind of prop: where it goes and one per this many squares (at least `min`).
struct Rule {
    asset: &'static str,
    at: Where,
    per: usize,
    min: usize,
    scale: (f32, f32),
}

const fn r(asset: &'static str, at: Where, per: usize, min: usize, scale: (f32, f32)) -> Rule {
    Rule {
        asset,
        at,
        per,
        min,
        scale,
    }
}

/// The recipe for a room: the first keyword its lowercase name contains wins,
/// else its kind decides.
fn recipe(name: &str, kind: SpaceKind) -> &'static [Rule] {
    use Where::*;
    const BARRACKS: &[Rule] = &[
        r("bed", Wall, 5, 2, (0.9, 1.0)),
        r("chest", Corner, 30, 1, (0.8, 1.0)),
        r("torch", Wall, 40, 1, (1.0, 1.0)),
    ];
    const KITCHEN: &[Rule] = &[
        r("table", Middle, 25, 1, (1.0, 1.2)),
        r("barrel", Corner, 12, 1, (0.8, 1.0)),
        r("crate", Wall, 20, 1, (0.8, 1.0)),
    ];
    const LIBRARY: &[Rule] = &[
        r("table", Middle, 20, 1, (0.9, 1.1)),
        r("chair", Middle, 12, 2, (0.8, 0.9)),
        r("chest", Wall, 25, 1, (0.8, 1.0)),
    ];
    const STORE: &[Rule] = &[
        r("crate", Wall, 6, 3, (0.8, 1.1)),
        r("barrel", Corner, 10, 2, (0.8, 1.0)),
        r("chest", Wall, 30, 1, (0.8, 1.0)),
    ];
    const SHRINE: &[Rule] = &[
        r("rug", Middle, 25, 1, (1.3, 1.8)),
        r("torch", Wall, 14, 2, (1.0, 1.0)),
        r("pillar", Corner, 20, 2, (0.9, 1.0)),
    ];
    const THRONE: &[Rule] = &[
        r("pillar", Wall, 10, 4, (1.0, 1.2)),
        r("rug", Middle, 30, 1, (1.8, 2.4)),
        r("torch", Wall, 18, 2, (1.0, 1.0)),
    ];
    const CRYPT: &[Rule] = &[
        r("chest", Wall, 18, 1, (0.9, 1.0)),
        r("pillar", Middle, 28, 1, (0.9, 1.1)),
        r("torch", Wall, 40, 1, (1.0, 1.0)),
    ];
    const CELL: &[Rule] = &[
        r("bed", Wall, 12, 1, (0.8, 0.9)),
        r("barrel", Corner, 20, 1, (0.7, 0.9)),
    ];
    const ARMORY: &[Rule] = &[
        r("crate", Wall, 5, 3, (0.8, 1.0)),
        r("barrel", Corner, 12, 1, (0.8, 1.0)),
        r("chest", Wall, 20, 1, (0.9, 1.0)),
    ];
    const GENERIC: &[Rule] = &[
        r("crate", Corner, 24, 1, (0.8, 1.0)),
        r("barrel", Corner, 30, 1, (0.8, 1.0)),
        r("table", Middle, 36, 0, (0.9, 1.0)),
        r("torch", Wall, 30, 1, (1.0, 1.0)),
    ];
    const CORRIDOR: &[Rule] = &[
        r("torch", Wall, 14, 1, (1.0, 1.0)),
        r("crate", Corner, 40, 0, (0.8, 0.9)),
    ];
    const AREA: &[Rule] = &[
        r("rock", Middle, 14, 1, (0.8, 1.4)),
        r("bush", Middle, 10, 1, (0.8, 1.1)),
    ];
    let n = name.to_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| n.contains(k));
    if has(&["barrack", "dorm", "quarters", "bedroom", "crew"]) {
        BARRACKS
    } else if has(&["kitchen", "mess", "galley", "canteen"]) {
        KITCHEN
    } else if has(&["library", "study", "office", "lab", "archive"]) {
        LIBRARY
    } else if has(&["store", "cargo", "pantry", "warehouse", "locker"]) {
        STORE
    } else if has(&["shrine", "temple", "chapel", "altar"]) {
        SHRINE
    } else if has(&["throne", "hall", "audience"]) {
        THRONE
    } else if has(&["crypt", "tomb", "ossuary", "vault"]) {
        CRYPT
    } else if has(&["cell", "prison", "jail"]) {
        CELL
    } else if has(&["armory", "armoury", "workshop", "forge"]) {
        ARMORY
    } else {
        match kind {
            SpaceKind::Corridor => CORRIDOR,
            SpaceKind::Area => AREA,
            SpaceKind::Room => GENERIC,
        }
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / 16_777_216.0
    }
}

/// Props for the named rooms of a floor (those with no props yet are left to
/// `only_empty`; with it false every room gets new ones).
pub fn furnish(
    floor: &FloorDoc,
    space_ids: &[String],
    seed: u64,
    only_empty: bool,
) -> CoreResult<Vec<Op>> {
    if space_ids.is_empty() {
        return Err(CoreError::Invalid("choose the rooms to furnish".into()));
    }
    let grid = &floor.grid;
    let mut ops: Vec<Op> = Vec::new();
    let mut taken: BTreeSet<(i32, i32)> = floor
        .stamps
        .iter()
        .map(|s| (s.x.floor() as i32, s.y.floor() as i32))
        .collect();
    let mut room = MAX_STAMPS.saturating_sub(floor.stamps.len());
    for (si, id) in space_ids.iter().enumerate() {
        let space = grid
            .spaces
            .get(id)
            .ok_or_else(|| CoreError::NotFound(format!("room {id}")))?;
        let cells: Vec<CellKey> = grid
            .cells()
            .filter(|(_, s)| *s == id)
            .map(|(c, _)| c)
            .collect();
        if cells.is_empty() {
            continue;
        }
        let inside: BTreeSet<(i32, i32)> = cells.iter().map(|c| (c.x, c.y)).collect();
        if only_empty && inside.iter().any(|c| taken.contains(c)) {
            continue;
        }
        // A cell is "free" for props only if no door/opening touches it or a neighbour.
        let edges = [Edge::North, Edge::East, Edge::South, Edge::West];
        let door_near = |c: CellKey| {
            let touches = |cc: CellKey| {
                edges.iter().any(|e| {
                    grid.mark_between(cc, *e)
                        .is_some_and(|m| m.kind != crate::grid::MarkKind::Window)
                })
            };
            touches(c)
                || [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|(dx, dy)| {
                    touches(CellKey {
                        x: c.x + dx,
                        y: c.y + dy,
                    })
                })
        };
        // How many of its four sides are walls (edge to another space or the void).
        let walls_of = |c: &CellKey| {
            [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .filter(|(dx, dy)| !inside.contains(&(c.x + dx, c.y + dy)))
                .count()
        };
        let mut rng = Rng(seed ^ (si as u64).wrapping_mul(0x9E37_79B9) ^ 0xF0F0_1234);
        for rule in recipe(&space.name, space.kind) {
            let want = (cells.len() / rule.per.max(1)).max(rule.min);
            let mut pool: Vec<CellKey> = cells
                .iter()
                .copied()
                .filter(|c| {
                    let w = walls_of(c);
                    let ok = match rule.at {
                        Where::Corner => w >= 2,
                        Where::Wall => w == 1,
                        Where::Middle => w == 0,
                    };
                    ok && !door_near(*c) && !taken.contains(&(c.x, c.y))
                })
                .collect();
            // A rule with nothing to stand on falls back to any free cell, so a 2x2 closet still gets a crate.
            if pool.is_empty() && rule.min > 0 {
                pool = cells
                    .iter()
                    .copied()
                    .filter(|c| !door_near(*c) && !taken.contains(&(c.x, c.y)))
                    .collect();
            }
            for _ in 0..want.min(pool.len()) {
                let i = (rng.next() % pool.len() as u64) as usize;
                let c = pool.swap_remove(i);
                if room == 0 {
                    return Err(CoreError::CapExceeded(
                        "the floor holds no more props".into(),
                    ));
                }
                room -= 1;
                taken.insert((c.x, c.y));
                let scale = rule.scale.0 + (rule.scale.1 - rule.scale.0) * rng.unit();
                ops.push(Op::AddStamp {
                    stamp: Stamp {
                        id: format!("fu{:x}_{}", seed & 0xFFFF_FFFF, ops.len()),
                        asset: rule.asset.to_string(),
                        x: c.x as f32 + 0.5,
                        y: c.y as f32 + 0.5,
                        scale,
                        rotation: if rule.at == Where::Middle {
                            0.0
                        } else {
                            rng.unit() * 0.2 - 0.1
                        },
                        flip: rng.next() & 1 == 1,
                        tint: None,
                        shadow: true,
                    },
                    index: None,
                });
            }
        }
    }
    if ops.is_empty() {
        return Err(CoreError::Invalid(
            "nothing to furnish there (they may already have props)".into(),
        ));
    }
    Ok(vec![Op::Batch { ops }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{default_space, EdgeKey, Mark, MarkKind};
    use crate::journal::Journal;

    fn floor() -> FloorDoc {
        let mut f = FloorDoc::new("f".into(), "G".into());
        for (id, name, kind) in [
            ("k", "Kitchen", SpaceKind::Room),
            ("b", "Barracks", SpaceKind::Room),
            ("c", "Passage", SpaceKind::Corridor),
        ] {
            f.grid.upsert_space(default_space(id, name, kind)).unwrap();
        }
        let rect = |x0: i32, y0: i32, x1: i32, y1: i32| -> Vec<CellKey> {
            (x0..x1)
                .flat_map(|x| (y0..y1).map(move |y| CellKey { x, y }))
                .collect()
        };
        f.grid.paint(&rect(0, 0, 8, 6), Some("k")).unwrap();
        f.grid.paint(&rect(10, 0, 18, 6), Some("b")).unwrap();
        f.grid.paint(&rect(8, 2, 10, 3), Some("c")).unwrap();
        for (x, edge) in [(7, Edge::East), (9, Edge::East)] {
            f.grid
                .set_mark(
                    EdgeKey {
                        cell: CellKey { x, y: 2 },
                        edge,
                    },
                    Some(Mark::new(MarkKind::Door)),
                )
                .unwrap();
        }
        f
    }

    fn run(f: &FloorDoc, ids: &[&str], seed: u64) -> FloorDoc {
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        let mut out = f.clone();
        let mut j = Journal::default();
        for op in furnish(f, &ids, seed, true).unwrap() {
            j.push(&mut out, &op).unwrap();
        }
        out
    }

    #[test]
    fn a_room_gets_props_that_fit_its_name_and_never_block_a_door() {
        let f = floor();
        let k = run(&f, &["k"], 3);
        let kinds: BTreeSet<&str> = k.stamps.iter().map(|s| s.asset.as_str()).collect();
        assert!(
            kinds.contains("table") && kinds.contains("barrel"),
            "{kinds:?}"
        );
        assert!(!kinds.contains("bed"), "a kitchen has no beds");
        let b = run(&f, &["b"], 3);
        assert!(b.stamps.iter().any(|s| s.asset == "bed"));
        // Nothing within a cell of the doorway at (7,2) / (9,2) / (10,2).
        for s in k.stamps.iter().chain(&b.stamps) {
            let (cx, cy) = (s.x.floor() as i32, s.y.floor() as i32);
            for (dx, dy) in [(7, 2), (10, 2)] {
                assert!(
                    !((cx - dx).abs() <= 1 && (cy - dy).abs() <= 1),
                    "prop at {cx},{cy} blocks the door at {dx},{dy}"
                );
            }
        }
        // One prop per square at most, all inside the room asked for.
        let cells: BTreeSet<(i32, i32)> = k
            .stamps
            .iter()
            .map(|s| (s.x.floor() as i32, s.y.floor() as i32))
            .collect();
        assert_eq!(cells.len(), k.stamps.len());
        assert!(k.stamps.iter().all(|s| s.x < 8.0));
    }

    #[test]
    fn it_is_deterministic_one_undo_step_and_skips_rooms_already_furnished() {
        let f = floor();
        assert_eq!(run(&f, &["k", "b"], 9), run(&f, &["k", "b"], 9));
        assert_ne!(run(&f, &["k"], 9).stamps, run(&f, &["k"], 10).stamps);
        let mut g = f.clone();
        let mut j = Journal::default();
        for op in furnish(&f, &["k".into()], 1, true).unwrap() {
            j.push(&mut g, &op).unwrap();
        }
        assert!(!g.stamps.is_empty());
        // Already furnished: only_empty refuses, forcing adds more.
        assert!(furnish(&g, &["k".into()], 2, true).is_err());
        assert!(furnish(&g, &["k".into()], 2, false).is_ok());
        j.undo_last(&mut g).unwrap();
        assert!(g.stamps.is_empty());
        assert!(furnish(&f, &[], 1, true).is_err());
        assert!(furnish(&f, &["nope".into()], 1, true).is_err());
    }

    #[test]
    fn corridors_get_torches_and_tiny_rooms_still_get_something() {
        let f = floor();
        // The short passage between its two doors has nothing it can hold without blocking one.
        assert!(furnish(&f, &["c".into()], 1, true).is_err());
        let mut long = FloorDoc::new("l".into(), "L".into());
        long.grid
            .upsert_space(default_space("c", "Passage", SpaceKind::Corridor))
            .unwrap();
        let cells: Vec<CellKey> = (0..14).map(|x| CellKey { x, y: 0 }).collect();
        long.grid.paint(&cells, Some("c")).unwrap();
        let c = run(&long, &["c"], 1);
        assert!(
            !c.stamps.is_empty()
                && c.stamps
                    .iter()
                    .all(|s| s.asset == "torch" || s.asset == "crate")
        );
        let mut tiny = FloorDoc::new("t".into(), "T".into());
        tiny.grid
            .upsert_space(default_space("s", "Storeroom", SpaceKind::Room))
            .unwrap();
        tiny.grid
            .paint(
                &[
                    CellKey { x: 0, y: 0 },
                    CellKey { x: 1, y: 0 },
                    CellKey { x: 0, y: 1 },
                    CellKey { x: 1, y: 1 },
                ],
                Some("s"),
            )
            .unwrap();
        assert!(!run(&tiny, &["s"], 1).stamps.is_empty());
    }
}
