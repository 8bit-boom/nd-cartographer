//! Edit a painted floor by instruction (plan 3.8): a model (or an agent) proposes typed edits
//! instead of a whole new layout, and this module checks every id and value against the real
//! floor and turns the plan into ONE batch op, so applying it is one undo step. Nothing here
//! changes a floor; the caller previews the plan and applies the batch through the normal ops.

use crate::doc::{FloorDoc, Label};
use crate::grid::{CellKey, Edge, EdgeKey, Mark, MarkKind};
use crate::journal::Op;
use crate::mapspec::LinkKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A plan is at most this many edits.
pub const MAX_EDITS: usize = 50;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EditPlan {
    pub edits: Vec<Edit>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    RenameRoom {
        room: String,
        name: String,
    },
    SetLit {
        room: String,
        lit: bool,
    },
    SetLink {
        a: String,
        b: String,
        kind: LinkKind,
    },
    SetMaterial {
        room: String,
        material: String,
    },
    Furnish {
        rooms: Vec<String>,
    },
    Note {
        room: String,
        text: String,
    },
}

/// One line per edit, for the preview.
pub fn summary(plan: &EditPlan) -> Vec<String> {
    plan.edits
        .iter()
        .map(|e| match e {
            Edit::RenameRoom { room, name } => format!("Rename {room} to \"{name}\""),
            Edit::SetLit { room, lit } => {
                format!("{} {room}", if *lit { "Light" } else { "Darken" })
            }
            Edit::SetLink { a, b, kind } => {
                format!("Make the link {a} - {b} a {kind:?}").to_lowercase()
            }
            Edit::SetMaterial { room, material } => format!("Floor of {room}: {material}"),
            Edit::Furnish { rooms } => format!("Furnish {}", rooms.join(", ")),
            Edit::Note { room, text } => format!("GM note in {room}: {text}"),
        })
        .collect()
}

/// What the model is shown: every room (real id, name, kind, material, lit) and which pairs
/// share a wall with what on it.
pub fn describe(floor: &FloorDoc, light_cell: f64) -> serde_json::Value {
    let lit = lit_rooms(floor, light_cell);
    let rooms: Vec<_> = floor
        .grid
        .spaces
        .values()
        .map(|s| {
            serde_json::json!({
                "id": s.id, "name": s.name, "kind": s.kind,
                "material": s.material, "lit": lit.contains(s.id.as_str()),
            })
        })
        .collect();
    let mut links = Vec::new();
    for (a, b, kind) in shared_walls(floor) {
        links.push(serde_json::json!({"a": a, "b": b, "kind": kind}));
    }
    serde_json::json!({"floor": floor.name, "rooms": rooms, "links": links})
}

fn lit_rooms(floor: &FloorDoc, cell: f64) -> BTreeSet<&str> {
    floor
        .lights
        .iter()
        .filter_map(|l| {
            floor.grid.cell_space(CellKey {
                x: (l.x / cell).floor() as i32,
                y: (l.y / cell).floor() as i32,
            })
        })
        .collect()
}

fn mark_name(k: MarkKind) -> &'static str {
    match k {
        MarkKind::Door => "door",
        MarkKind::Opening => "opening",
        MarkKind::Secret => "secret",
        MarkKind::Window => "window",
    }
}

/// Pairs of rooms that touch, with the strongest thing marked on their shared wall (or "wall").
fn shared_walls(floor: &FloorDoc) -> Vec<(String, String, &'static str)> {
    let mut out: std::collections::BTreeMap<(String, String), &'static str> = Default::default();
    for (c, sp) in floor.grid.cells() {
        for e in [Edge::East, Edge::South] {
            let n = match e {
                Edge::East => CellKey { x: c.x + 1, y: c.y },
                _ => CellKey { x: c.x, y: c.y + 1 },
            };
            let Some(other) = floor.grid.cell_space(n) else {
                continue;
            };
            if other == sp {
                continue;
            }
            let key = if sp < other {
                (sp.to_string(), other.to_string())
            } else {
                (other.to_string(), sp.to_string())
            };
            let kind = floor
                .grid
                .marks
                .get(&EdgeKey { cell: c, edge: e })
                .map_or("wall", |m| mark_name(m.kind));
            let slot = out.entry(key).or_insert("wall");
            if kind != "wall" {
                *slot = kind;
            }
        }
    }
    out.into_iter().map(|((a, b), k)| (a, b, k)).collect()
}

/// The edges two rooms share.
fn shared_edges(floor: &FloorDoc, a: &str, b: &str) -> Vec<EdgeKey> {
    let mut out = Vec::new();
    for (c, sp) in floor.grid.cells() {
        for e in [Edge::East, Edge::South] {
            let n = match e {
                Edge::East => CellKey { x: c.x + 1, y: c.y },
                _ => CellKey { x: c.x, y: c.y + 1 },
            };
            let other = floor.grid.cell_space(n);
            if (sp == a && other == Some(b)) || (sp == b && other == Some(a)) {
                out.push(EdgeKey { cell: c, edge: e });
            }
        }
    }
    out.sort();
    out
}

/// Check `plan` against `floor` and build the one batch op. Every problem found comes back
/// (not just the first), phrased for the model that wrote the plan.
/// `materials` are the ids the active pack offers.
pub fn compile(
    floor: &FloorDoc,
    light_cell: f64,
    materials: &[&str],
    plan: &EditPlan,
) -> Result<Op, Vec<String>> {
    let mut bad: Vec<String> = Vec::new();
    let mut ops: Vec<Op> = Vec::new();
    if plan.edits.is_empty() {
        return Err(vec!["The plan has no edits.".into()]);
    }
    if plan.edits.len() > MAX_EDITS {
        return Err(vec![format!(
            "The plan has {} edits; send at most {MAX_EDITS}.",
            plan.edits.len()
        )]);
    }
    let known: Vec<&str> = floor.grid.spaces.keys().map(String::as_str).collect();
    let room = |id: &str, bad: &mut Vec<String>| -> bool {
        let ok = floor.grid.spaces.contains_key(id);
        if !ok {
            bad.push(format!(
                "'{id}' is not a room of this floor. Rooms: {}.",
                known.join(", ")
            ));
        }
        ok
    };
    let lit = lit_rooms(floor, light_cell);
    let mut lights_after = floor.lights.len();
    for (i, e) in plan.edits.iter().enumerate() {
        match e {
            Edit::RenameRoom { room: r, name } => {
                if !room(r, &mut bad) {
                    continue;
                }
                let n = name.trim().chars().count();
                if n == 0 || n > 60 {
                    bad.push(format!("edit {}: a room name is 1-60 characters.", i + 1));
                    continue;
                }
                let mut s = floor.grid.spaces[r].clone();
                s.name = name.trim().to_string();
                ops.push(Op::UpsertSpace { space: s });
            }
            Edit::SetMaterial { room: r, material } => {
                if !room(r, &mut bad) {
                    continue;
                }
                if !materials.contains(&material.as_str()) {
                    let ids = materials;
                    bad.push(format!(
                        "edit {}: '{material}' is not a material. Use one of: {}.",
                        i + 1,
                        ids.join(", ")
                    ));
                    continue;
                }
                let mut s = floor.grid.spaces[r].clone();
                s.material = Some(material.clone());
                ops.push(Op::UpsertSpace { space: s });
            }
            Edit::SetLit { room: r, lit: want } => {
                if !room(r, &mut bad) {
                    continue;
                }
                let is_lit = lit.contains(r.as_str());
                if *want && !is_lit {
                    let cells: Vec<CellKey> = floor
                        .grid
                        .cells()
                        .filter(|(_, s)| s == r)
                        .map(|(c, _)| c)
                        .collect();
                    if lights_after >= crate::MAX_LIGHTS {
                        bad.push(format!(
                            "edit {}: the floor already has the most lights ({}).",
                            i + 1,
                            crate::MAX_LIGHTS
                        ));
                        continue;
                    }
                    lights_after += 1;
                    let n = cells.len().max(1) as f64;
                    let (cx, cy) = (
                        cells.iter().map(|c| c.x as f64).sum::<f64>() / n + 0.5,
                        cells.iter().map(|c| c.y as f64).sum::<f64>() / n + 0.5,
                    );
                    ops.push(Op::AddLight {
                        light: crate::doc::Light {
                            id: crate::journal::new_light(),
                            x: cx * light_cell,
                            y: cy * light_cell,
                            range: 6.0,
                            color: "#ffd9a0".into(),
                            intensity: 0.8,
                            on: true,
                            label: None,
                        },
                    });
                } else if !*want && is_lit {
                    for l in &floor.lights {
                        let here = floor.grid.cell_space(CellKey {
                            x: (l.x / light_cell).floor() as i32,
                            y: (l.y / light_cell).floor() as i32,
                        });
                        if here == Some(r.as_str()) {
                            lights_after -= 1;
                            ops.push(Op::RemoveLight { id: l.id.clone() });
                        }
                    }
                }
            }
            Edit::SetLink { a, b, kind } => {
                let (ok_a, ok_b) = (room(a, &mut bad), room(b, &mut bad));
                if !(ok_a && ok_b) {
                    continue;
                }
                let edges = shared_edges(floor, a, b);
                if edges.is_empty() {
                    bad.push(format!(
                        "edit {}: '{a}' and '{b}' do not share a wall, so there is nothing to put a door in.",
                        i + 1
                    ));
                    continue;
                }
                let mk = match kind {
                    LinkKind::Door => MarkKind::Door,
                    LinkKind::Opening => MarkKind::Opening,
                    LinkKind::Secret => MarkKind::Secret,
                };
                let marked: Vec<&EdgeKey> = edges
                    .iter()
                    .filter(|e| floor.grid.marks.contains_key(e))
                    .collect();
                let targets: Vec<EdgeKey> = if marked.is_empty() {
                    vec![edges[edges.len() / 2]]
                } else {
                    marked.into_iter().cloned().collect()
                };
                for edge in targets {
                    ops.push(Op::SetMark {
                        edge,
                        mark: Some(Mark::new(mk)),
                    });
                }
            }
            Edit::Furnish { rooms } => {
                let mut ok = !rooms.is_empty();
                if rooms.is_empty() {
                    bad.push(format!("edit {}: furnish needs at least one room.", i + 1));
                }
                for r in rooms {
                    ok &= room(r, &mut bad);
                }
                if !ok {
                    continue;
                }
                match crate::furnish::furnish(floor, rooms, 1, true) {
                    Ok(o) => ops.extend(o),
                    Err(e) => bad.push(format!("edit {}: {e}", i + 1)),
                }
            }
            Edit::Note { room: r, text } => {
                if !room(r, &mut bad) {
                    continue;
                }
                let t = text.trim();
                if t.is_empty() || t.chars().count() > 200 {
                    bad.push(format!("edit {}: a note is 1-200 characters.", i + 1));
                    continue;
                }
                let cells: Vec<CellKey> = floor
                    .grid
                    .cells()
                    .filter(|(_, s)| s == r)
                    .map(|(c, _)| c)
                    .collect();
                let n = cells.len().max(1) as f64;
                let (cx, cy) = (
                    cells.iter().map(|c| c.x as f64).sum::<f64>() / n + 0.5,
                    cells.iter().map(|c| c.y as f64).sum::<f64>() / n + 0.5,
                );
                let mut label = Label::new(&crate::journal::new_label(), cx, cy, t);
                label.gm_only = true;
                ops.push(Op::UpsertLabel { label });
            }
        }
    }
    if !bad.is_empty() {
        return Err(bad);
    }
    Ok(Op::Batch { ops })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::apply_with_inverse;
    use crate::mapspec::{compile as lay_out, FloorSpec, LinkSpec, MapSpec, RoomSpec};

    /// A floor laid out from a two-room spec, so the rooms really touch through a corridor-free wall? The compiler
    /// puts a corridor between rooms, so take whatever floor comes out and use its real ids.
    fn floor() -> FloorDoc {
        let room = |id: &str, name: &str| RoomSpec {
            id: id.into(),
            name: name.into(),
            kind: crate::SpaceKind::Room,
            w: 6,
            h: 5,
            lit: false,
        };
        let spec = MapSpec {
            name: "T".into(),
            floors: vec![FloorSpec {
                name: "G".into(),
                rooms: vec![room("hall", "Hall"), room("store", "Store room")],
                links: vec![LinkSpec {
                    a: "hall".into(),
                    b: "store".into(),
                    kind: LinkKind::Door,
                }],
                stairs: vec![],
            }],
        };
        let laid = lay_out(&spec, 1).unwrap();
        let mut f = FloorDoc::new("f".into(), "G".into());
        for op in &laid[0].ops {
            crate::apply_op(&mut f, op).unwrap();
        }
        f
    }

    fn ids(f: &FloorDoc) -> Vec<String> {
        f.grid.spaces.keys().cloned().collect()
    }

    #[test]
    fn an_unknown_room_is_a_complaint_that_names_it_and_lists_the_real_ones() {
        let f = floor();
        let plan = EditPlan {
            edits: vec![
                Edit::RenameRoom {
                    room: "nope".into(),
                    name: "X".into(),
                },
                Edit::SetMaterial {
                    room: ids(&f)[0].clone(),
                    material: "lava".into(),
                },
            ],
        };
        let e = compile(&f, 32.0, &["stone"], &plan).unwrap_err();
        assert!(
            e[0].contains("'nope'") && e[0].contains(&ids(&f)[0]),
            "{e:?}"
        );
        assert!(e[1].contains("'lava' is not a material"), "{e:?}");
    }

    #[test]
    fn a_valid_plan_is_one_batch_and_undo_restores_the_floor_exactly() {
        let mut f = floor();
        let rooms: Vec<String> = f
            .grid
            .spaces
            .values()
            .filter(|s| s.kind == crate::SpaceKind::Room)
            .map(|s| s.id.clone())
            .collect();
        let (a, b) = (rooms[0].clone(), rooms[1].clone());
        let material = "stone".to_string();
        let plan = EditPlan {
            edits: vec![
                Edit::RenameRoom {
                    room: a.clone(),
                    name: "Great Hall".into(),
                },
                Edit::SetLit {
                    room: a.clone(),
                    lit: true,
                },
                Edit::SetMaterial {
                    room: b.clone(),
                    material,
                },
                Edit::Furnish {
                    rooms: vec![b.clone()],
                },
                Edit::Note {
                    room: a.clone(),
                    text: "the lamp is cursed".into(),
                },
            ],
        };
        let before = serde_json::to_string(&f).unwrap();
        let op = compile(&f, 32.0, &["stone"], &plan).unwrap();
        assert!(matches!(op, Op::Batch { .. }));
        let inverse = apply_with_inverse(&mut f, &op).unwrap();
        assert_eq!(f.grid.spaces[&a].name, "Great Hall");
        assert_ne!(serde_json::to_string(&f).unwrap(), before);
        assert!(f
            .labels
            .iter()
            .any(|l| l.gm_only && l.text.contains("cursed")));
        apply_with_inverse(&mut f, &inverse).unwrap();
        assert_eq!(serde_json::to_string(&f).unwrap(), before);
        assert_eq!(summary(&plan).len(), 5);
    }

    #[test]
    fn set_link_needs_a_shared_wall_and_the_plan_size_is_capped() {
        let f = floor();
        let rooms: Vec<String> = f.grid.spaces.keys().cloned().collect();
        // Find two rooms with a shared wall, if the layout has any; otherwise every pair is refused.
        let mut tried = 0;
        for a in &rooms {
            for b in &rooms {
                if a >= b {
                    continue;
                }
                let plan = EditPlan {
                    edits: vec![Edit::SetLink {
                        a: a.clone(),
                        b: b.clone(),
                        kind: LinkKind::Secret,
                    }],
                };
                match compile(&f, 32.0, &["stone"], &plan) {
                    Ok(Op::Batch { ops }) => {
                        assert!(ops.iter().all(|o| matches!(o, Op::SetMark { .. })));
                    }
                    Err(e) => assert!(e[0].contains("do not share a wall"), "{e:?}"),
                    other => panic!("{other:?}"),
                }
                tried += 1;
            }
        }
        assert!(tried > 0);
        let big = EditPlan {
            edits: (0..51)
                .map(|_| Edit::Furnish {
                    rooms: vec![rooms[0].clone()],
                })
                .collect(),
        };
        assert!(compile(&f, 32.0, &["stone"], &big).unwrap_err()[0].contains("at most 50"));
        assert!(
            serde_json::from_str::<EditPlan>(r#"{"edits":[{"op":"explode","room":"a"}]}"#).is_err()
        );
        assert!(serde_json::from_str::<EditPlan>(
            r#"{"edits":[{"op":"set_lit","room":"a","lit":true,"x":1}]}"#
        )
        .is_err());
    }
}
