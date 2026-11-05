//! In-memory document store with optional persistence.
//!
//! Layout under `data_dir` (when set): one folder per document —
//! `manifest.json`, `floors.json` (snapshot), `journal.ndjson`. The journal
//! is rewritten whole on save for M0 (documents are small); append-only
//! segments arrive with the real `.ndmap` work (plan §5).

use ndcart_core::doc::{floors_by_id, DocKind, FloorDoc, Manifest};
use ndcart_core::ids::new_id;
use ndcart_core::journal::{apply_op, Journal, Op};
use ndcart_core::CoreResult;
use std::collections::BTreeMap;
use std::path::Path;

pub struct StoredDoc {
    pub manifest: Manifest,
    pub floors: Vec<FloorDoc>,
    pub journal: Journal,
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
        };
        let floor = FloorDoc::new(new_id("fl"), "Ground".to_string());
        StoredDoc {
            manifest,
            floors: vec![floor],
            journal: Journal::default(),
            rev: 0,
        }
    }

    /// Apply ops to one floor as journal entries (each op keeps its inverse).
    pub fn apply_ops(&mut self, floor_id: &str, ops: &[Op]) -> CoreResult<()> {
        let idx = floors_by_id(&self.floors, floor_id)?;
        for op in ops {
            // Journal::push applies and journals; run against the right floor.
            let floor = &mut self.floors[idx];
            self.journal.push(floor, op)?;
        }
        self.rev += 1;
        self.manifest.updated_at = ndcart_core::doc::now_iso();
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
    let manifest = serde_json::to_string_pretty(&doc.manifest).expect("manifest serializes");
    let floors = serde_json::to_string_pretty(&doc.floors).expect("floors serialize");
    let journal = doc.journal.to_ndjson();
    let _ = std::fs::write(dir.join("manifest.json"), manifest);
    let _ = std::fs::write(dir.join("floors.json"), floors);
    let _ = std::fs::write(dir.join("journal.ndjson"), journal);
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
