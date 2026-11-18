//! Lint what an AI (or a person) proposes before the GM sees it: lay a MapSpec out, apply the
//! ops to a scratch floor, run the floor linter, and map each problem back to the room ids
//! the spec used. `validate()` and `compile()` catch a spec that cannot be drawn; this
//! catches one that draws badly (a room nobody can reach, two rooms with one name).

use crate::doc::FloorDoc;
use crate::grid::SpaceKind;
use crate::journal::apply_op;
use crate::lint::{lint, Severity};
use crate::mapspec::{compile, CompiledFloor, MapSpec};
use crate::CoreResult;
use serde::Serialize;

/// The lattice size the linter assumes for lights (the studio default).
const CELL: f64 = 32.0;

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct SpecProblem {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub floor: String,
    /// The spec's room ids the problem is about (empty when it is about the floor as a whole).
    pub rooms: Vec<String>,
}

/// Lay `spec` out with `seed` and lint every floor. Errors are the spec's own (it did not
/// validate or fit); problems are the linter's findings.
pub fn lint_spec(spec: &MapSpec, seed: u64) -> CoreResult<Vec<SpecProblem>> {
    let compiled = compile(spec, seed)?;
    lint_compiled(spec, &compiled)
}

/// Like [`lint_spec`], for floors already laid out by `compile` (same order as `spec.floors`).
pub fn lint_compiled(spec: &MapSpec, compiled: &[CompiledFloor]) -> CoreResult<Vec<SpecProblem>> {
    let mut out = Vec::new();
    for (fs, cf) in spec.floors.iter().zip(compiled) {
        let mut scratch = FloorDoc::new("speclint".into(), cf.name.clone());
        for op in &cf.ops {
            apply_op(&mut scratch, op)?;
        }
        // space id -> room id
        let room_of = |space: &str| -> Option<String> {
            cf.spaces
                .iter()
                .find(|(_, s)| s.as_str() == space)
                .map(|(r, _)| r.clone())
        };
        for p in lint(&scratch, CELL) {
            let mut rooms: Vec<String> = Vec::new();
            if let Some(r) =
                p.at.and_then(|c| scratch.grid.cell_space(c))
                    .and_then(room_of)
            {
                rooms.push(r);
            }
            // Problems that name rooms by their name: 'x' is the first quoted word group.
            if matches!(p.code, "duplicate_room_name" | "corridor_dead_end") {
                if let (Some(a), Some(b)) = (p.message.find('\''), p.message.rfind('\'')) {
                    if b > a {
                        let name = p.message[a + 1..b].to_lowercase();
                        let corridor = p.code == "corridor_dead_end";
                        for r in &fs.rooms {
                            let kind_ok = (r.kind == SpaceKind::Corridor) == corridor;
                            if kind_ok
                                && r.name.to_lowercase() == name
                                && (corridor || r.kind == SpaceKind::Room)
                                && !rooms.contains(&r.id)
                            {
                                rooms.push(r.id.clone());
                            }
                        }
                    }
                }
            }
            out.push(SpecProblem {
                severity: p.severity,
                code: p.code,
                message: p.message,
                floor: cf.name.clone(),
                rooms,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapspec::*;

    fn room(id: &str, name: &str) -> RoomSpec {
        RoomSpec {
            id: id.into(),
            name: name.into(),
            kind: SpaceKind::Room,
            w: 5,
            h: 5,
            lit: true,
        }
    }
    fn one_floor(rooms: Vec<RoomSpec>, links: Vec<LinkSpec>) -> MapSpec {
        MapSpec {
            name: "T".into(),
            floors: vec![FloorSpec {
                name: "G".into(),
                rooms,
                links,
                stairs: vec![],
            }],
        }
    }
    fn door(a: &str, b: &str, kind: LinkKind) -> LinkSpec {
        LinkSpec {
            a: a.into(),
            b: b.into(),
            kind,
        }
    }

    #[test]
    fn repeated_room_names_come_back_with_their_room_ids() {
        let spec = one_floor(
            vec![room("a", "Cell"), room("b", "Cell"), room("c", "Warden")],
            vec![
                door("a", "b", LinkKind::Door),
                door("b", "c", LinkKind::Door),
            ],
        );
        let problems = lint_spec(&spec, 1).unwrap();
        let dup = problems
            .iter()
            .find(|p| p.code == "duplicate_room_name")
            .unwrap_or_else(|| panic!("{problems:?}"));
        assert_eq!(dup.rooms, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(dup.floor, "G");
        assert_eq!(dup.severity, Severity::Info);
        // The same spec with distinct names is clean of that problem.
        let clean = one_floor(
            vec![room("a", "Cell"), room("b", "Cellar"), room("c", "Warden")],
            vec![
                door("a", "b", LinkKind::Door),
                door("b", "c", LinkKind::Door),
            ],
        );
        assert!(lint_spec(&clean, 1)
            .unwrap()
            .iter()
            .all(|p| p.code != "duplicate_room_name"));
    }

    #[test]
    fn a_room_behind_a_secret_door_is_named() {
        let spec = one_floor(
            vec![room("a", "Hall"), room("b", "Vault")],
            vec![door("a", "b", LinkKind::Secret)],
        );
        let problems = lint_spec(&spec, 1).unwrap();
        let p = problems
            .iter()
            .find(|p| p.code == "secret_only_room")
            .unwrap_or_else(|| panic!("{problems:?}"));
        assert_eq!(p.rooms.len(), 1, "{p:?}");
    }

    #[test]
    fn a_spec_that_cannot_be_drawn_is_an_error_not_a_problem() {
        let spec = one_floor(vec![room("a", "A"), room("b", "B")], vec![]);
        assert!(lint_spec(&spec, 1).is_err());
    }
}
