//! In-memory document store with optional persistence.
//!
//! Layout under `data_dir` (when set): one folder per document —
//! `manifest.json`, `floors.json` (snapshot), `journal.ndjson` (every floor's edits, tagged by floor) and `structure.ndjson` (floor add/rename/move/remove). The journal
//! is rewritten whole on save for M0 (documents are small); append-only
//! segments arrive with the real `.ndmap` work (plan §5).

use ndcart_core::doc::{floors_by_id, DocKind, FloorDoc, Manifest};
use ndcart_core::ids::new_id;
use ndcart_core::journal::{apply_op, Journal, Op};
use ndcart_core::{CoreResult, DocOp, StructJournal};
use std::collections::BTreeMap;
use std::path::Path;

pub struct StoredDoc {
    pub manifest: Manifest,
    pub floors: Vec<FloorDoc>,
    pub journal: Journal,
    /// Adding, renaming, moving and removing floors, with its own undo.
    pub structure: StructJournal,
    pub rev: u64,
}

impl StoredDoc {
    pub fn new(name: &str, kind: DocKind) -> Self {
        let id = new_id("doc");
        let now = ndcart_core::doc::now_iso();
        let manifest = Manifest {
            id: id.clone(),
            name: name.to_string(),
            kind,
            cell_size: 32.0,
            created_at: now.clone(),
            updated_at: now,
            nd_world: None,
        };
        let floor = FloorDoc::new(new_id("fl"), "Ground".to_string());
        StoredDoc {
            manifest,
            floors: vec![floor],
            journal: Journal::default(),
            structure: StructJournal::default(),
            rev: 0,
        }
    }

    /// Apply ops to one floor as journal entries (each op keeps its inverse).
    pub fn apply_ops(&mut self, floor_id: &str, ops: &[Op]) -> CoreResult<()> {
        let idx = floors_by_id(&self.floors, floor_id)?;
        // All or nothing: a bad op in the middle must not leave the ones before it applied
        // and journaled while the request is refused.
        let (backup, mark) = (self.floors[idx].clone(), self.journal.len());
        for op in ops {
            // Journal::push applies and journals; run against the right floor.
            if let Err(e) = self.journal.push(&mut self.floors[idx], op) {
                self.floors[idx] = backup;
                self.journal.truncate(mark);
                return Err(e);
            }
        }
        self.rev += 1;
        self.manifest.updated_at = ndcart_core::doc::now_iso();
        Ok(())
    }

    fn touched(&mut self) {
        self.rev += 1;
        self.manifest.updated_at = ndcart_core::doc::now_iso();
    }

    /// Add a floor — empty, or a copy of `copy_from` — and return its id.
    pub fn add_floor(&mut self, name: &str, copy_from: Option<&str>) -> CoreResult<String> {
        let id = new_id("fl");
        let floor = match copy_from {
            Some(src) => {
                let i = floors_by_id(&self.floors, src)?;
                let mut f = self.floors[i].clone();
                f.id = id.clone();
                f.name = name.trim().to_string();
                f
            }
            None => FloorDoc::new(id.clone(), name.trim().to_string()),
        };
        // A new floor goes right after the one it was copied from, else at the end.
        let index = copy_from
            .and_then(|s| self.floors.iter().position(|f| f.id == s))
            .map(|i| i + 1);
        self.structure.push(
            &mut self.floors,
            DocOp::AddFloor {
                floor: Box::new(floor),
                index,
            },
        )?;
        self.touched();
        Ok(id)
    }

    pub fn remove_floor(&mut self, id: &str) -> CoreResult<()> {
        self.structure
            .push(&mut self.floors, DocOp::RemoveFloor { id: id.to_string() })?;
        self.touched();
        Ok(())
    }

    pub fn rename_floor(&mut self, id: &str, name: &str) -> CoreResult<()> {
        self.structure.push(
            &mut self.floors,
            DocOp::RenameFloor {
                id: id.to_string(),
                name: name.to_string(),
            },
        )?;
        self.touched();
        Ok(())
    }

    pub fn move_floor(&mut self, id: &str, index: usize) -> CoreResult<()> {
        self.structure.push(
            &mut self.floors,
            DocOp::MoveFloor {
                id: id.to_string(),
                index,
            },
        )?;
        self.touched();
        Ok(())
    }

    /// Undo the last floor-level change (add / remove / rename / move).
    pub fn undo_structure(&mut self) -> CoreResult<()> {
        self.structure.undo_last(&mut self.floors)?;
        self.touched();
        Ok(())
    }

    pub fn undo_last(&mut self, floor_id: &str) -> CoreResult<()> {
        let idx = floors_by_id(&self.floors, floor_id)?;
        let floor = &mut self.floors[idx];
        self.journal.undo_last(floor)?;
        self.rev += 1;
        self.manifest.updated_at = ndcart_core::doc::now_iso();
        Ok(())
    }
}

#[derive(Default)]
pub struct Store {
    pub docs: BTreeMap<String, StoredDoc>,
}

impl Store {
    pub fn create(&mut self, name: &str, kind: DocKind) -> &Manifest {
        let doc = StoredDoc::new(name, kind);
        let id = doc.manifest.id.clone();
        self.docs.insert(id.clone(), doc);
        &self.docs[&id].manifest
    }

    pub fn load_dir(&mut self, dir: &Path) -> CoreResult<()> {
        let mut count = 0usize;
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let doc_dir = entry.path();
            if !doc_dir.is_dir() {
                continue;
            }
            let manifest_path = doc_dir.join("manifest.json");
            let floors_path = doc_dir.join("floors.json");
            let journal_path = doc_dir.join("journal.ndjson");
            if !(manifest_path.is_file() && floors_path.is_file()) {
                continue;
            }
            let manifest: Manifest =
                serde_json::from_str(&std::fs::read_to_string(&manifest_path).map_err(|e| {
                    ndcart_core::CoreError::Invalid(format!(
                        "reading {}: {e}",
                        manifest_path.display()
                    ))
                })?)
                .map_err(|e| {
                    ndcart_core::CoreError::Invalid(format!(
                        "parsing {}: {e}",
                        manifest_path.display()
                    ))
                })?;
            let mut floors: Vec<FloorDoc> =
                serde_json::from_str(&std::fs::read_to_string(&floors_path).map_err(|e| {
                    ndcart_core::CoreError::Invalid(format!(
                        "reading {}: {e}",
                        floors_path.display()
                    ))
                })?)
                .map_err(|e| {
                    ndcart_core::CoreError::Invalid(format!(
                        "parsing {}: {e}",
                        floors_path.display()
                    ))
                })?;
            let journal_text = std::fs::read_to_string(&journal_path).unwrap_or_default();
            let journal = Journal::from_ndjson(&journal_text)?;
            let structure = StructJournal::from_ndjson(
                &std::fs::read_to_string(doc_dir.join("structure.ndjson")).unwrap_or_default(),
            )?;
            for f in &mut floors {
                f.grid.fixup();
            }
            let rev = journal.len() as u64;
            let id = manifest.id.clone();
            self.docs.insert(
                id,
                StoredDoc {
                    manifest,
                    floors,
                    journal,
                    structure,
                    rev,
                },
            );
            count += 1;
        }
        if count > 0 {
            println!("ndcart: loaded {count} document(s) from {}", dir.display());
        }
        Ok(())
    }
}

/// Persist one document (snapshot + journal). No-op without a data dir.
pub fn save_doc(data_dir: Option<&Path>, doc: &StoredDoc) {
    let Some(root) = data_dir else { return };
    let dir = root.join(&doc.manifest.id);
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("ndcart: cannot create {}", dir.display());
        return;
    }
    // A failed serialize must never take the server down mid-edit: report it
    // and keep the previous files (the journal is the source of truth anyway).
    let (manifest, floors) = match (
        serde_json::to_string_pretty(&doc.manifest),
        serde_json::to_string_pretty(&doc.floors),
    ) {
        (Ok(m), Ok(f)) => (m, f),
        (m, f) => {
            eprintln!(
                "ndcart: cannot serialize {}: {}",
                doc.manifest.id,
                m.err()
                    .map(|e| e.to_string())
                    .or(f.err().map(|e| e.to_string()))
                    .unwrap_or_default()
            );
            return;
        }
    };
    let journal = doc.journal.to_ndjson();
    // Crash-safe: write to a temp file, fsync, then rename over the target, so
    // a kill -9 mid-save leaves either the old or the new file, never a torn one.
    for (name, body) in [
        ("manifest.json", manifest),
        ("floors.json", floors),
        ("journal.ndjson", journal),
        ("structure.ndjson", doc.structure.to_ndjson()),
    ] {
        if let Err(e) = write_atomic(&dir.join(name), body.as_bytes()) {
            eprintln!("ndcart: cannot save {name}: {e}");
        }
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Apply ops with the journal captured per-op (used by the bench too).
pub fn apply_ops_direct(floor: &mut FloorDoc, journal: &mut Journal, ops: &[Op]) -> CoreResult<()> {
    for op in ops {
        let inverse = op.inverse_against(floor)?;
        apply_op(floor, op)?;
        journal.push_entry(op.clone(), inverse);
    }
    Ok(())
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use ndcart_core::journal::Op;
    use ndcart_core::DocKind;

    #[test]
    fn a_document_with_doors_saves_and_reloads() {
        let dir = std::env::temp_dir().join(format!("ndcart-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut d = StoredDoc::new("Doors", DocKind::Battle);
        let fid = d.floors[0].id.clone();
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"s","name":"H","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0}],"space":"s"},
            {"t":"set_mark","edge":{"cell":{"x":0,"y":0},"edge":"east"},"mark":{"kind":"door","state":"closed"}}
        ]))
        .unwrap();
        d.apply_ops(&fid, &ops).unwrap();
        save_doc(Some(&dir), &d);
        let mut st = Store::default();
        st.load_dir(&dir).unwrap();
        let back = &st.docs[&d.manifest.id];
        assert_eq!(back.floors[0].grid, d.floors[0].grid);
        assert!(!back.floors[0].grid.marks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn three_floors_survive_a_reload_with_their_own_undo_and_structure_history() {
        let dir = std::env::temp_dir().join(format!("ndcart-floors-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut d = StoredDoc::new("Station", DocKind::Battle);
        let ground = d.floors[0].id.clone();
        let upper = d.add_floor("Upper", None).unwrap();
        let roof = d.add_floor("Roof", None).unwrap();
        // One edit = one batch = one undo step (the UI sends it the same way).
        let room = |name: &str, x: i32| -> Vec<Op> {
            serde_json::from_value(serde_json::json!([{"t":"batch","ops":[
                {"t":"upsert_space","space":{"id":"s","name":name,"color":"#7f5539","kind":"room"}},
                {"t":"paint_cells","cells":[{"x":x,"y":0}],"space":"s"}
            ]}]))
            .unwrap()
        };
        d.apply_ops(&ground, &room("Lobby", 0)).unwrap();
        d.apply_ops(&upper, &room("Suite", 3)).unwrap();
        d.apply_ops(&roof, &room("Helipad", 6)).unwrap();
        d.apply_ops(&upper, &room("Suite", 4)).unwrap(); // a second edit on the middle floor
        d.rename_floor(&roof, "Rooftop").unwrap();
        save_doc(Some(&dir), &d);

        let mut st = Store::default();
        st.load_dir(&dir).unwrap();
        let mut back = st.docs.remove(&d.manifest.id).unwrap();
        assert_eq!(
            back.floors
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            ["Ground", "Upper", "Rooftop"]
        );
        assert_eq!(back.floors, d.floors, "all three floors reload exactly");
        // Undo on the middle floor undoes ITS last edit, from the reloaded journal.
        back.undo_last(&upper).unwrap();
        let cells = |doc: &StoredDoc, i: usize| doc.floors[i].grid.cell_count();
        assert_eq!(
            (cells(&back, 0), cells(&back, 1), cells(&back, 2)),
            (1, 1, 1)
        );
        back.undo_last(&upper).unwrap();
        assert_eq!(cells(&back, 1), 0);
        assert_eq!(
            (cells(&back, 0), cells(&back, 2)),
            (1, 1),
            "the other floors keep their rooms"
        );
        // And the floor-level history reloaded too: undo the rename.
        back.undo_structure().unwrap();
        assert_eq!(back.floors[2].name, "Roof");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
