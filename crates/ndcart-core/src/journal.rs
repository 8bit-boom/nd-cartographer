//! The command journal: every edit is an [`Op`]; the inverse is computed
//! *against the state the op was applied to* and stored alongside it, so
//! undo is exact and replay is a straight fold. Journal entries serialize to
//! NdJSON (one op per line) — the crash-safe file format the plan's `.ndmap`
//! folder uses (`ops/000123.ndjson`).

use crate::doc::FloorDoc;
use crate::error::{CoreError, CoreResult};
use crate::grid::{CellKey, EdgeKey, Mark, Space};
use crate::ids::new_id;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Op {
    /// Paint cells into a space id (or clear them when `space` is null).
    PaintCells {
        cells: Vec<CellKey>,
        space: Option<String>,
    },
    /// Create or update a space definition.
    UpsertSpace {
        space: Space,
    },
    /// Remove a space and detach its cells.
    RemoveSpace {
        id: String,
    },
    /// Set or clear the mark on an edge.
    SetMark {
        edge: EdgeKey,
        mark: Option<Mark>,
    },
    AddLight {
        light: crate::doc::Light,
    },
    RemoveLight {
        id: String,
    },
    UpsertLabel {
        label: crate::doc::Label,
    },
    RemoveLabel {
        id: String,
    },
    /// Add a paint stroke on top of the z-order (or at `index`, which is how
    /// an undone removal puts it back where it was).
    AddStroke {
        stroke: crate::stroke::Stroke,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    RemoveStroke {
        id: String,
    },
    /// Several ops as one undoable step.
    Batch {
        ops: Vec<Op>,
    },
}

impl Op {
    /// The op that undoes this one, computed against the state *before*
    /// application. (Callers apply ops one at a time through
    /// [`apply_op`], which is where inverses are captured.)
    pub fn inverse_against(&self, floor: &FloorDoc) -> CoreResult<Op> {
        match self {
            Op::PaintCells { cells, .. } => {
                let prev: Vec<_> = cells
                    .iter()
                    .filter_map(|c| floor.grid.cell_space(*c).map(|s| (*c, s.to_string())))
                    .collect();
                Ok(Op::Batch {
                    ops: prev
                        .into_iter()
                        .map(|(c, s)| Op::PaintCells {
                            cells: vec![c],
                            space: Some(s),
                        })
                        .chain(
                            cells
                                .iter()
                                .filter(|c| floor.grid.cell_space(**c).is_none())
                                .map(|c| Op::PaintCells {
                                    cells: vec![*c],
                                    space: None,
                                }),
                        )
                        .collect(),
                })
            }
            Op::UpsertSpace { space } => {
                if let Some(prev) = floor.grid.spaces.get(&space.id) {
                    Ok(Op::UpsertSpace {
                        space: prev.clone(),
                    })
                } else {
                    Ok(Op::RemoveSpace {
                        id: space.id.clone(),
                    })
                }
            }
            Op::RemoveSpace { id } => {
                let space = floor
                    .grid
                    .spaces
                    .get(id)
                    .ok_or_else(|| CoreError::NotFound(format!("space {id}")))?;
                // Also restore any cells that pointed at it (the apply will
                // have detached them).
                let cells: Vec<CellKey> = floor
                    .grid
                    .cells()
                    .filter(|(_, s)| *s == id)
                    .map(|(c, _)| c)
                    .collect();
                Ok(Op::Batch {
                    ops: vec![
                        Op::PaintCells {
                            cells,
                            space: Some(id.clone()),
                        },
                        Op::UpsertSpace {
                            space: space.clone(),
                        },
                    ],
                })
            }
            Op::SetMark { edge, .. } => Ok(Op::SetMark {
                edge: *edge,
                mark: floor.grid.marks.get(edge).copied(),
            }),
            Op::AddLight { light } => Ok(Op::RemoveLight {
                id: light.id.clone(),
            }),
            Op::RemoveLight { id } => {
                let light = floor
                    .lights
                    .iter()
                    .find(|l| l.id == *id)
                    .ok_or_else(|| CoreError::NotFound(format!("light {id}")))?;
                Ok(Op::AddLight {
                    light: light.clone(),
                })
            }
            Op::UpsertLabel { label } => {
                if let Some(prev) = floor.labels.iter().find(|l| l.id == label.id) {
                    Ok(Op::UpsertLabel {
                        label: prev.clone(),
                    })
                } else {
                    Ok(Op::RemoveLabel {
                        id: label.id.clone(),
                    })
                }
            }
            Op::RemoveLabel { id } => {
                let label = floor
                    .labels
                    .iter()
                    .find(|l| l.id == *id)
                    .ok_or_else(|| CoreError::NotFound(format!("label {id}")))?;
                Ok(Op::UpsertLabel {
                    label: label.clone(),
                })
            }
            Op::AddStroke { stroke, .. } => Ok(Op::RemoveStroke {
                id: stroke.id.clone(),
            }),
            Op::RemoveStroke { id } => {
                let index = floor
                    .strokes
                    .iter()
                    .position(|s| s.id == *id)
                    .ok_or_else(|| CoreError::NotFound(format!("stroke {id}")))?;
                Ok(Op::AddStroke {
                    stroke: floor.strokes[index].clone(),
                    index: Some(index),
                })
            }
            Op::Batch { ops } => {
                // Compute the batch inverse by walking a scratch copy of the
                // floor forward through the batch.
                let mut scratch = floor.clone();
                let mut inv = Vec::with_capacity(ops.len());
                for op in ops {
                    inv.push(op.inverse_against(&scratch)?);
                    apply_op(&mut scratch, op)?;
                }
                inv.reverse();
                Ok(Op::Batch { ops: inv })
            }
        }
    }
}

/// Apply one op to a floor document. Validation happens here — nothing
/// invalid is ever journaled (the D1 lesson: refuse at the boundary).
pub fn apply_op(floor: &mut FloorDoc, op: &Op) -> CoreResult<()> {
    match op {
        Op::PaintCells { cells, space } => floor.grid.paint(cells, space.as_deref())?,
        Op::UpsertSpace { space } => floor.grid.upsert_space(space.clone())?,
        Op::RemoveSpace { id } => floor.grid.remove_space(id)?,
        Op::SetMark { edge, mark } => floor.grid.set_mark(*edge, *mark)?,
        Op::AddLight { light } => floor.add_light(light.clone())?,
        Op::RemoveLight { id } => floor.remove_light(id)?,
        Op::UpsertLabel { label } => floor.upsert_label(label.clone())?,
        Op::RemoveLabel { id } => floor.remove_label(id)?,
        Op::AddStroke { stroke, index } => floor.add_stroke(stroke.clone(), *index)?,
        Op::RemoveStroke { id } => floor.remove_stroke(id)?,
        Op::Batch { ops } => {
            for op in ops {
                apply_op(floor, op)?;
            }
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct JournalEntry {
    pub op: Op,
    /// The inverse as computed at journal time; undo applies this as a new
    /// op (so redo stays possible by journal order, and nothing is deleted).
    pub inverse: Op,
}

#[derive(Default)]
pub struct Journal {
    entries: Vec<JournalEntry>,
}

impl Journal {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Append one op: computes the inverse against the current floor state
    /// and applies the op.
    pub fn push(&mut self, floor: &mut FloorDoc, op: &Op) -> CoreResult<()> {
        let inverse = op.inverse_against(floor)?;
        apply_op(floor, op)?;
        self.entries.push(JournalEntry {
            op: op.clone(),
            inverse,
        });
        Ok(())
    }

    /// Low-level append for callers that apply ops themselves (the store's
    /// batch path, the bench). Validation is the caller's duty.
    pub fn push_entry(&mut self, op: Op, inverse: Op) {
        self.entries.push(JournalEntry { op, inverse });
    }

    /// Undo the last entry: apply its inverse as a NEW entry (the journal is
    /// append-only; redo = journal position, which M0's server exposes by
    /// refusing to undo past the start).
    pub fn undo_last(&mut self, floor: &mut FloorDoc) -> CoreResult<bool> {
        let last = self
            .entries
            .len()
            .checked_sub(1)
            .ok_or_else(|| CoreError::NotFound("nothing to undo".to_string()))?;
        let inverse = self.entries[last].inverse.clone();
        let forward = self.entries[last].op.clone();
        let redo_inverse = inverse.inverse_against(floor)?;
        apply_op(floor, &inverse)?;
        // Swap the entry so the journal stays consistent going forward.
        self.entries[last] = JournalEntry {
            op: inverse,
            inverse: redo_inverse,
        };
        let _ = forward; // kept above only for clarity
        Ok(true)
    }

    pub fn to_ndjson(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&serde_json::to_string(e).expect("journal entry serializes"));
            out.push('\n');
        }
        out
    }

    pub fn from_ndjson(text: &str) -> CoreResult<Journal> {
        let mut entries = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let e: JournalEntry = serde_json::from_str(line)
                .map_err(|e| CoreError::Invalid(format!("journal line {}: {e}", n + 1)))?;
            entries.push(e);
        }
        Ok(Journal { entries })
    }

    /// Replay the whole journal into a fresh floor.
    pub fn replay_into(&self, floor: &mut FloorDoc) -> CoreResult<()> {
        for e in &self.entries {
            apply_op(floor, &e.op)?;
        }
        Ok(())
    }
}

/// Convenience: a batch op from paint strokes.
pub fn paint_batch(cells: Vec<CellKey>, space: Option<String>) -> Op {
    Op::Batch {
        ops: vec![Op::PaintCells { cells, space }],
    }
}

/// Convenience used by the UI: ensure a space exists, then paint (one undoable step).
pub fn paint_with_space(space: Space, cells: Vec<CellKey>) -> Op {
    let sid = space.id.clone();
    Op::Batch {
        ops: vec![
            Op::UpsertSpace { space },
            Op::PaintCells {
                cells,
                space: Some(sid),
            },
        ],
    }
}

/// New light id.
pub fn new_light() -> String {
    new_id("lt")
}

/// New label id.
pub fn new_label() -> String {
    new_id("lb")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{Space, SpaceKind};

    fn space_a() -> Space {
        Space {
            id: "a".into(),
            name: "A".into(),
            color: "#112233".into(),
            kind: SpaceKind::Room,
            material: None,
        }
    }

    fn painted_floor() -> (FloorDoc, Journal) {
        let mut floor = FloorDoc::new("f1".into(), "Ground".into());
        let mut journal = Journal::default();
        journal
            .push(&mut floor, &Op::UpsertSpace { space: space_a() })
            .unwrap();
        (floor, journal)
    }

    #[test]
    fn paint_and_undo_roundtrip() {
        let (mut floor, mut journal) = painted_floor();
        let op = Op::PaintCells {
            cells: vec![CellKey::new(0, 0), CellKey::new(1, 1)],
            space: Some("a".into()),
        };
        journal.push(&mut floor, &op).unwrap();
        assert_eq!(floor.grid.cell_count(), 2);
        journal.undo_last(&mut floor).unwrap();
        assert_eq!(floor.grid.cell_count(), 0);
    }

    #[test]
    fn undo_is_exact_for_every_op_kind() {
        let (mut floor, mut journal) = painted_floor();
        let before = floor.clone();
        let batch = Op::Batch {
            ops: vec![
                Op::PaintCells {
                    cells: vec![CellKey::new(0, 0), CellKey::new(1, 0), CellKey::new(2, 0)],
                    space: Some("a".into()),
                },
                Op::SetMark {
                    edge: EdgeKey {
                        cell: CellKey::new(0, 0),
                        edge: crate::grid::Edge::East,
                    },
                    mark: Some(Mark::new(crate::grid::MarkKind::Door)),
                },
                Op::AddLight {
                    light: crate::doc::Light {
                        id: new_light(),
                        x: 32.0,
                        y: 32.0,
                        range: 6.0,
                        color: "#ffcc88".into(),
                        intensity: 0.8,
                        on: true,
                        label: None,
                    },
                },
                Op::UpsertLabel {
                    label: crate::doc::Label {
                        id: new_label(),
                        x: 10.0,
                        y: 10.0,
                        text: "Guard post".into(),
                        size: 14.0,
                        color: "#ffffff".into(),
                        bold: false,
                        gm_only: false,
                    },
                },
            ],
        };
        journal.push(&mut floor, &batch).unwrap();
        assert!(floor.grid.cell_count() == 3 && floor.lights.len() == 1 && floor.labels.len() == 1);
        journal.undo_last(&mut floor).unwrap();
        assert_eq!(floor, before, "undo must restore the exact prior state");
    }

    #[test]
    fn replay_matches_incremental_application() {
        let (mut floor, mut journal) = painted_floor();
        let ops = vec![
            Op::PaintCells {
                cells: (0..10).map(|x| CellKey::new(x, 0)).collect(),
                space: Some("a".into()),
            },
            Op::SetMark {
                edge: EdgeKey {
                    cell: CellKey::new(3, 0),
                    edge: crate::grid::Edge::North,
                },
                mark: Some(Mark::new(crate::grid::MarkKind::Secret)),
            },
        ];
        for op in &ops {
            journal.push(&mut floor, op).unwrap();
        }
        let after = floor.clone();
        let mut fresh = FloorDoc::new("f1".into(), "Ground".into());
        journal
            .push(&mut fresh, &Op::UpsertSpace { space: space_a() })
            .unwrap();
        journal.replay_into(&mut fresh).unwrap();
        // Replay must not have appended: fresh contains only the seed + replay.
        assert_eq!(fresh, after);
    }

    #[test]
    fn ndjson_roundtrip() {
        let (mut floor, mut journal) = painted_floor();
        journal
            .push(
                &mut floor,
                &Op::PaintCells {
                    cells: vec![CellKey::new(5, 5)],
                    space: Some("a".into()),
                },
            )
            .unwrap();
        let text = journal.to_ndjson();
        assert_eq!(text.lines().count(), 2);
        let back = Journal::from_ndjson(&text).unwrap();
        assert_eq!(back.len(), 2);
        let mut fresh = FloorDoc::new("f1".into(), "Ground".into());
        back.replay_into(&mut fresh).unwrap();
        assert_eq!(fresh.grid.cell_count(), 1);
    }

    #[test]
    fn invalid_ops_are_refused_and_not_journaled() {
        let (mut floor, mut journal) = painted_floor();
        let bad = Op::PaintCells {
            cells: vec![CellKey::new(999_999_999, 0)],
            space: Some("a".into()),
        };
        assert!(journal.push(&mut floor, &bad).is_err());
        assert_eq!(journal.len(), 1, "refused op must not be journaled");
    }

    #[test]
    fn light_cap_is_enforced() {
        let (mut floor, mut _j) = painted_floor();
        for i in 0..crate::error::MAX_LIGHTS {
            floor
                .add_light(crate::doc::Light {
                    id: format!("l{i}"),
                    x: i as f64,
                    y: 0.0,
                    range: 5.0,
                    color: "#ffffff".into(),
                    intensity: 0.5,
                    on: true,
                    label: None,
                })
                .unwrap();
        }
        assert!(floor
            .add_light(crate::doc::Light {
                id: "over".into(),
                x: 0.0,
                y: 0.0,
                range: 5.0,
                color: "#ffffff".into(),
                intensity: 0.5,
                on: true,
                label: None,
            })
            .is_err());
    }

    #[test]
    fn nonfinite_light_refused() {
        let (mut floor, mut _j) = painted_floor();
        let nan = crate::doc::Light {
            id: "nan".into(),
            x: f64::NAN,
            y: 0.0,
            range: 5.0,
            color: "#ffffff".into(),
            intensity: 0.5,
            on: true,
            label: None,
        };
        assert!(floor.add_light(nan).is_err());
    }

    fn stroke(id: &str) -> crate::stroke::Stroke {
        use crate::stroke::{BrushKind, Stroke, StrokeKind, StrokePoint};
        Stroke {
            id: id.into(),
            kind: StrokeKind::Paint,
            brush: BrushKind::Round,
            material: None,
            color: "#336699".into(),
            size: 1.5,
            opacity: 1.0,
            hardness: 1.0,
            roughness: 0.0,
            density: 0.5,
            seed: 3,
            points: vec![
                StrokePoint {
                    x: 0.0,
                    y: 0.0,
                    pressure: 1.0,
                    tilt: 0.0,
                },
                StrokePoint {
                    x: 3.0,
                    y: 1.0,
                    pressure: 1.0,
                    tilt: 0.0,
                },
            ],
        }
    }

    #[test]
    fn strokes_undo_exactly_and_keep_their_z_order() {
        let mut floor = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        for id in ["a", "b", "c"] {
            j.push(
                &mut floor,
                &Op::AddStroke {
                    stroke: stroke(id),
                    index: None,
                },
            )
            .unwrap();
        }
        let ids = |f: &FloorDoc| f.strokes.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&floor), ["a", "b", "c"]);

        // Remove the middle one, then undo: it returns to the middle, not the top.
        j.push(&mut floor, &Op::RemoveStroke { id: "b".into() })
            .unwrap();
        assert_eq!(ids(&floor), ["a", "c"]);
        j.undo_last(&mut floor).unwrap();
        assert_eq!(ids(&floor), ["a", "b", "c"]);

        // Undo an add: gone.
        j.push(
            &mut floor,
            &Op::AddStroke {
                stroke: stroke("d"),
                index: None,
            },
        )
        .unwrap();
        j.undo_last(&mut floor).unwrap();
        assert_eq!(ids(&floor), ["a", "b", "c"]);
    }

    #[test]
    fn refused_strokes_are_never_journaled() {
        let mut floor = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        j.push(
            &mut floor,
            &Op::AddStroke {
                stroke: stroke("a"),
                index: None,
            },
        )
        .unwrap();
        // duplicate id, bad number, unknown removal: all refused, journal unchanged
        let mut bad = stroke("z");
        bad.size = f32::NAN;
        for op in [
            Op::AddStroke {
                stroke: stroke("a"),
                index: None,
            },
            Op::AddStroke {
                stroke: bad,
                index: None,
            },
            Op::RemoveStroke { id: "nope".into() },
        ] {
            assert!(j.push(&mut floor, &op).is_err());
        }
        assert_eq!((j.len(), floor.strokes.len()), (1, 1));
    }

    #[test]
    fn strokes_survive_ndjson_replay_into_a_fresh_floor() {
        let mut floor = FloorDoc::new("f".into(), "G".into());
        let mut j = Journal::default();
        for id in ["a", "b"] {
            j.push(
                &mut floor,
                &Op::AddStroke {
                    stroke: stroke(id),
                    index: None,
                },
            )
            .unwrap();
        }
        j.push(&mut floor, &Op::RemoveStroke { id: "a".into() })
            .unwrap();
        let back = Journal::from_ndjson(&j.to_ndjson()).unwrap();
        let mut fresh = FloorDoc::new("f".into(), "G".into());
        back.replay_into(&mut fresh).unwrap();
        assert_eq!(fresh.strokes, floor.strokes);
        // A batch of strokes is one undo step.
        let mut j2 = Journal::default();
        let mut f2 = FloorDoc::new("f".into(), "G".into());
        let batch = Op::Batch {
            ops: vec![
                Op::AddStroke {
                    stroke: stroke("x"),
                    index: None,
                },
                Op::AddStroke {
                    stroke: stroke("y"),
                    index: None,
                },
            ],
        };
        j2.push(&mut f2, &batch).unwrap();
        j2.undo_last(&mut f2).unwrap();
        assert!(f2.strokes.is_empty());
    }
}
