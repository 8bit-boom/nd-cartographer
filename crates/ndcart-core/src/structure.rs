//! Document structure: the list of floors (plan §5.1 multi-floor, §14 M3). Adding,
//! duplicating, renaming, reordering and removing a floor are ops with exact
//! inverses in their own append-only linear history, parallel to each floor's
//! journal: removing a floor snapshots it, so undoing the removal brings back
//! every stroke, stamp and wall. A floor's own edits stay in the floor journal
//! (tagged by floor id), so a removed-then-restored floor keeps its undo history.

use crate::doc::FloorDoc;
use crate::error::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};

pub const MAX_FLOORS: usize = 64;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DocOp {
    /// Insert a floor at `index` (or at the end).
    AddFloor {
        floor: Box<FloorDoc>,
        index: Option<usize>,
    },
    RemoveFloor {
        id: String,
    },
    RenameFloor {
        id: String,
        name: String,
    },
    /// Move a floor to a position in the list.
    MoveFloor {
        id: String,
        index: usize,
    },
}

pub fn valid_floor_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn valid_floor_name(name: &str) -> bool {
    let n = name.trim().chars().count();
    (1..=80).contains(&n)
}

fn position(floors: &[FloorDoc], id: &str) -> CoreResult<usize> {
    floors
        .iter()
        .position(|f| f.id == id)
        .ok_or_else(|| CoreError::NotFound(format!("floor {id}")))
}

impl DocOp {
    /// The op that undoes this one, computed against the floors as they are now.
    pub fn inverse_against(&self, floors: &[FloorDoc]) -> CoreResult<DocOp> {
        match self {
            DocOp::AddFloor { floor, .. } => Ok(DocOp::RemoveFloor {
                id: floor.id.clone(),
            }),
            DocOp::RemoveFloor { id } => {
                let i = position(floors, id)?;
                Ok(DocOp::AddFloor {
                    floor: Box::new(floors[i].clone()),
                    index: Some(i),
                })
            }
            DocOp::RenameFloor { id, .. } => {
                let i = position(floors, id)?;
                Ok(DocOp::RenameFloor {
                    id: id.clone(),
                    name: floors[i].name.clone(),
                })
            }
            DocOp::MoveFloor { id, .. } => {
                let i = position(floors, id)?;
                Ok(DocOp::MoveFloor {
                    id: id.clone(),
                    index: i,
                })
            }
        }
    }
}

/// Apply one structural op (validated; nothing invalid is ever journaled).
pub fn apply(floors: &mut Vec<FloorDoc>, op: &DocOp) -> CoreResult<()> {
    match op {
        DocOp::AddFloor { floor, index } => {
            if !valid_floor_id(&floor.id) {
                return Err(CoreError::Invalid(
                    "floor id must be 1-64 chars of a-z, 0-9, _ -".into(),
                ));
            }
            if !valid_floor_name(&floor.name) {
                return Err(CoreError::Invalid(
                    "floor name must be 1-80 characters".into(),
                ));
            }
            if floors.iter().any(|f| f.id == floor.id) {
                return Err(CoreError::Invalid(format!("duplicate floor {}", floor.id)));
            }
            if floors.len() >= MAX_FLOORS {
                return Err(CoreError::CapExceeded(format!(
                    "at most {MAX_FLOORS} floors"
                )));
            }
            let at = index.unwrap_or(floors.len()).min(floors.len());
            floors.insert(at, (**floor).clone());
        }
        DocOp::RemoveFloor { id } => {
            let i = position(floors, id)?;
            if floors.len() == 1 {
                return Err(CoreError::Invalid("a map needs at least one floor".into()));
            }
            floors.remove(i);
        }
        DocOp::RenameFloor { id, name } => {
            if !valid_floor_name(name) {
                return Err(CoreError::Invalid(
                    "floor name must be 1-80 characters".into(),
                ));
            }
            let i = position(floors, id)?;
            floors[i].name = name.trim().to_string();
        }
        DocOp::MoveFloor { id, index } => {
            let i = position(floors, id)?;
            let f = floors.remove(i);
            let at = (*index).min(floors.len());
            floors.insert(at, f);
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct StructEntry {
    pub op: DocOp,
    pub inverse: DocOp,
    /// Set on an entry that is itself an undo: the index it reverses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<usize>,
}

/// Linear history of structural changes (same rules as the floor journal:
/// append-only, forward entries never rewritten, repeated undo walks back).
#[derive(Default, Clone, Debug)]
pub struct StructJournal {
    entries: Vec<StructEntry>,
}

impl StructJournal {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn entries(&self) -> &[StructEntry] {
        &self.entries
    }

    pub fn push(&mut self, floors: &mut Vec<FloorDoc>, op: DocOp) -> CoreResult<()> {
        let inverse = op.inverse_against(floors)?;
        apply(floors, &op)?;
        self.entries.push(StructEntry {
            op,
            inverse,
            undoes: None,
        });
        Ok(())
    }

    pub fn undo_last(&mut self, floors: &mut Vec<FloorDoc>) -> CoreResult<()> {
        let undone: std::collections::BTreeSet<usize> =
            self.entries.iter().filter_map(|e| e.undoes).collect();
        let target = (0..self.entries.len())
            .rev()
            .find(|i| self.entries[*i].undoes.is_none() && !undone.contains(i))
            .ok_or_else(|| CoreError::NotFound("no floor change to undo".into()))?;
        let inverse = self.entries[target].inverse.clone();
        let redo = inverse.inverse_against(floors)?;
        apply(floors, &inverse)?;
        self.entries.push(StructEntry {
            op: inverse,
            inverse: redo,
            undoes: Some(target),
        });
        Ok(())
    }

    pub fn to_ndjson(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            out.push_str(&serde_json::to_string(e).expect("structure entry serializes"));
            out.push('\n');
        }
        out
    }

    pub fn from_ndjson(text: &str) -> CoreResult<StructJournal> {
        let mut entries = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            entries.push(
                serde_json::from_str(line)
                    .map_err(|e| CoreError::Invalid(format!("structure line {}: {e}", n + 1)))?,
            );
        }
        Ok(StructJournal { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::{default_space, CellKey, SpaceKind};

    fn floor(id: &str, name: &str) -> FloorDoc {
        FloorDoc::new(id.into(), name.into())
    }
    fn names(f: &[FloorDoc]) -> Vec<String> {
        f.iter().map(|x| x.name.clone()).collect()
    }

    #[test]
    fn add_rename_move_remove_and_undo_each_exactly() {
        let mut floors = vec![floor("g", "Ground")];
        let mut j = StructJournal::default();
        j.push(
            &mut floors,
            DocOp::AddFloor {
                floor: Box::new(floor("u", "Upper")),
                index: None,
            },
        )
        .unwrap();
        j.push(
            &mut floors,
            DocOp::AddFloor {
                floor: Box::new(floor("b", "Basement")),
                index: Some(0),
            },
        )
        .unwrap();
        assert_eq!(names(&floors), ["Basement", "Ground", "Upper"]);
        j.push(
            &mut floors,
            DocOp::RenameFloor {
                id: "g".into(),
                name: "  Street level ".into(),
            },
        )
        .unwrap();
        assert_eq!(floors[1].name, "Street level", "names are trimmed");
        j.push(
            &mut floors,
            DocOp::MoveFloor {
                id: "u".into(),
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(names(&floors), ["Upper", "Basement", "Street level"]);
        // Undo walks back through all four, restoring each exactly.
        j.undo_last(&mut floors).unwrap();
        assert_eq!(names(&floors), ["Basement", "Street level", "Upper"]);
        j.undo_last(&mut floors).unwrap();
        assert_eq!(names(&floors), ["Basement", "Ground", "Upper"]);
        j.undo_last(&mut floors).unwrap();
        j.undo_last(&mut floors).unwrap();
        assert_eq!(names(&floors), ["Ground"]);
        assert!(j.undo_last(&mut floors).is_err(), "nothing left to undo");
    }

    #[test]
    fn removing_a_floor_snapshots_it_so_undo_brings_back_its_content() {
        let mut floors = vec![floor("g", "Ground"), floor("u", "Upper")];
        floors[1]
            .grid
            .upsert_space(default_space("a", "Hall", SpaceKind::Room))
            .unwrap();
        floors[1]
            .grid
            .paint(&[CellKey::new(2, 3), CellKey::new(3, 3)], Some("a"))
            .unwrap();
        floors[1]
            .upsert_label(crate::doc::Label::new("l", 1.0, 1.0, "Armoury"))
            .unwrap();
        let before = floors.clone();
        let mut j = StructJournal::default();
        j.push(&mut floors, DocOp::RemoveFloor { id: "u".into() })
            .unwrap();
        assert_eq!(floors.len(), 1);
        j.undo_last(&mut floors).unwrap();
        assert_eq!(
            floors, before,
            "the whole floor is back, content and position"
        );
    }

    #[test]
    fn the_last_floor_cannot_be_removed_and_bad_input_is_refused() {
        let mut floors = vec![floor("g", "Ground")];
        let mut j = StructJournal::default();
        assert!(j
            .push(&mut floors, DocOp::RemoveFloor { id: "g".into() })
            .is_err());
        let bad: Vec<DocOp> = vec![
            DocOp::AddFloor {
                floor: Box::new(floor("g", "Dup")),
                index: None,
            },
            DocOp::AddFloor {
                floor: Box::new(floor("a b", "Space")),
                index: None,
            },
            DocOp::AddFloor {
                floor: Box::new(floor("x", "   ")),
                index: None,
            },
            DocOp::AddFloor {
                floor: Box::new(floor("x", &"n".repeat(81))),
                index: None,
            },
            DocOp::RenameFloor {
                id: "g".into(),
                name: String::new(),
            },
            DocOp::RenameFloor {
                id: "ghost".into(),
                name: "x".into(),
            },
            DocOp::MoveFloor {
                id: "ghost".into(),
                index: 0,
            },
            DocOp::RemoveFloor { id: "ghost".into() },
        ];
        for op in bad {
            let before = floors.clone();
            assert!(j.push(&mut floors, op.clone()).is_err(), "{op:?}");
            assert_eq!(floors, before, "a refused op changes nothing");
        }
        assert!(j.is_empty(), "and journals nothing");
        // The floor cap.
        for i in 0..MAX_FLOORS - 1 {
            j.push(
                &mut floors,
                DocOp::AddFloor {
                    floor: Box::new(floor(&format!("f{i}"), "F")),
                    index: None,
                },
            )
            .unwrap();
        }
        assert!(j
            .push(
                &mut floors,
                DocOp::AddFloor {
                    floor: Box::new(floor("one-more", "F")),
                    index: None
                }
            )
            .is_err());
    }

    #[test]
    fn the_history_round_trips_through_ndjson() {
        let mut floors = vec![floor("g", "Ground")];
        let mut j = StructJournal::default();
        j.push(
            &mut floors,
            DocOp::AddFloor {
                floor: Box::new(floor("u", "Upper")),
                index: None,
            },
        )
        .unwrap();
        j.push(
            &mut floors,
            DocOp::RenameFloor {
                id: "u".into(),
                name: "Attic".into(),
            },
        )
        .unwrap();
        j.undo_last(&mut floors).unwrap();
        let back = StructJournal::from_ndjson(&j.to_ndjson()).unwrap();
        assert_eq!(back.entries(), j.entries());
        // Undo keeps working from a reloaded history.
        let mut again = floors.clone();
        let mut b = back;
        b.undo_last(&mut again).unwrap();
        assert_eq!(names(&again), ["Ground"]);
        assert!(StructJournal::from_ndjson("not json").is_err());
    }
}
