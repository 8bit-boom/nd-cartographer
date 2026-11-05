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

/// A named save point: every floor as it was. Restoring replaces the floors
/// and starts a fresh undo history (the state before is itself saved first).
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Checkpoint {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub floors: Vec<FloorDoc>,
}

pub const MAX_CHECKPOINTS: usize = 30;
pub const MAX_PROPOSALS: usize = 10;

/// A pending world map: nothing is drawn until it is accepted.
#[derive(Clone)]
pub struct WorldProposal {
    pub id: String,
    pub ops: Vec<Op>,
}

/// A map the GM has been shown but not accepted: the spec, the seed, and the
/// laid-out floors. Nothing in the document changes until it is accepted.
#[derive(Clone)]
pub struct Proposal {
    pub id: String,
    pub spec: ndcart_core::mapspec::MapSpec,
    pub seed: u64,
    pub floors: Vec<ndcart_core::mapspec::CompiledFloor>,
}

pub struct StoredDoc {
    pub manifest: Manifest,
    pub floors: Vec<FloorDoc>,
    pub journal: Journal,
    /// Adding, renaming, moving and removing floors, with its own undo.
    pub structure: StructJournal,
    pub checkpoints: Vec<Checkpoint>,
    /// Pending AI/spec proposals (in memory only).
    pub proposals: Vec<Proposal>,
    pub world_proposals: Vec<WorldProposal>,
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
            checkpoints: Vec::new(),
            proposals: Vec::new(),
            world_proposals: Vec::new(),
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

    /// Lay a spec out and keep it as a pending proposal.
    pub fn propose(
        &mut self,
        spec: ndcart_core::mapspec::MapSpec,
        seed: u64,
    ) -> CoreResult<&Proposal> {
        let floors = ndcart_core::mapspec::compile(&spec, seed)?;
        if self.proposals.len() >= MAX_PROPOSALS {
            self.proposals.remove(0);
        }
        self.proposals.push(Proposal {
            id: new_id("pr"),
            spec,
            seed,
            floors,
        });
        Ok(self.proposals.last().expect("just pushed"))
    }

    /// Draw the proposal onto the first floor (one undoable step) and drop it.
    pub fn accept_world(&mut self, id: &str) -> CoreResult<()> {
        let p = self
            .world_proposals
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| ndcart_core::CoreError::NotFound(format!("proposal {id}")))?;
        let fid = self.floors[0].id.clone();
        self.apply_ops(&fid, &p.ops)?;
        self.world_proposals.retain(|x| x.id != id);
        Ok(())
    }

    pub fn reject_world(&mut self, id: &str) -> CoreResult<()> {
        let before = self.world_proposals.len();
        self.world_proposals.retain(|p| p.id != id);
        if self.world_proposals.len() == before {
            return Err(ndcart_core::CoreError::NotFound(format!("proposal {id}")));
        }
        Ok(())
    }

    pub fn reject_proposal(&mut self, id: &str) -> CoreResult<()> {
        let before = self.proposals.len();
        self.proposals.retain(|p| p.id != id);
        if self.proposals.len() == before {
            return Err(ndcart_core::CoreError::NotFound(format!("proposal {id}")));
        }
        Ok(())
    }

    /// Add the proposal's floors to the document (never touching what is
    /// there), with stairs between them. Each floor is one undoable
    /// structure step plus one batch; returns the new floor ids in order.
    pub fn accept_proposal(&mut self, id: &str) -> CoreResult<Vec<String>> {
        use ndcart_core::overlay::{FloorLink, Marker, OverlayItem};
        let p = self
            .proposals
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| ndcart_core::CoreError::NotFound(format!("proposal {id}")))?;
        let mut ids: Vec<String> = Vec::new();
        for cf in &p.floors {
            let mut name = cf.name.clone();
            let mut n = 2;
            while self.floors.iter().any(|f| f.name == name) {
                name = format!("{} ({n})", cf.name);
                n += 1;
            }
            let fid = self.add_floor(&name, None)?;
            self.apply_ops(&fid, &cf.ops)?;
            ids.push(fid);
        }
        // Stairs, and the way back unless the target already has stairs to this floor.
        let floor_pos = |name: &str| p.spec.floors.iter().position(|f| f.name == name);
        let mut have: std::collections::BTreeSet<(usize, usize)> =
            std::collections::BTreeSet::new();
        for (fi, f) in p.spec.floors.iter().enumerate() {
            for s in &f.stairs {
                if let Some(ti) = floor_pos(&s.to_floor) {
                    have.insert((fi, ti));
                }
            }
        }
        let marker = |at: (i32, i32), label: &str, to: &str, land: Option<(i32, i32)>| Marker {
            id: new_id("st"),
            x: at.0 as f32 + 0.5,
            y: at.1 as f32 + 0.5,
            label: label.chars().take(80).collect(),
            note: String::new(),
            color: "#e8b030".into(),
            link_schematic: None,
            links_to: Some(FloorLink {
                floor: to.to_string(),
                x: land.map(|l| l.0 as f32 + 0.5),
                y: land.map(|l| l.1 as f32 + 0.5),
            }),
        };
        for (fi, f) in p.spec.floors.iter().enumerate() {
            for s in &f.stairs {
                let ti = floor_pos(&s.to_floor).expect("validated");
                let here = p.floors[fi].centres[&s.room];
                let landing = match &s.to_room {
                    Some(r) => p.floors[ti].centres[r],
                    None => *p.floors[ti].centres.values().next().expect("rooms"),
                };
                let label = s
                    .label
                    .clone()
                    .unwrap_or_else(|| format!("Stairs to {}", s.to_floor));
                let there = Some(landing);
                self.apply_ops(
                    &ids[fi],
                    &[Op::PutOverlay {
                        item: OverlayItem::Marker(marker(here, &label, &ids[ti], there)),
                        index: None,
                    }],
                )?;
                if !have.contains(&(ti, fi)) {
                    let back = format!("Stairs to {}", f.name);
                    self.apply_ops(
                        &ids[ti],
                        &[Op::PutOverlay {
                            item: OverlayItem::Marker(marker(landing, &back, &ids[fi], Some(here))),
                            index: None,
                        }],
                    )?;
                    have.insert((ti, fi));
                }
            }
        }
        self.proposals.retain(|x| x.id != id);
        Ok(ids)
    }

    pub fn create_checkpoint(&mut self, name: &str) -> CoreResult<String> {
        let name = name.trim();
        let n = name.chars().count();
        if n == 0 || n > 80 {
            return Err(ndcart_core::CoreError::Invalid(
                "a checkpoint name is 1-80 characters".into(),
            ));
        }
        if self.checkpoints.len() >= MAX_CHECKPOINTS {
            return Err(ndcart_core::CoreError::CapExceeded(format!(
                "at most {MAX_CHECKPOINTS} checkpoints; delete one first"
            )));
        }
        let id = new_id("cp");
        self.checkpoints.push(Checkpoint {
            id: id.clone(),
            name: name.to_string(),
            created_at: ndcart_core::doc::now_iso(),
            floors: self.floors.clone(),
        });
        Ok(id)
    }

    pub fn delete_checkpoint(&mut self, id: &str) -> CoreResult<()> {
        let before = self.checkpoints.len();
        self.checkpoints.retain(|c| c.id != id);
        if self.checkpoints.len() == before {
            return Err(ndcart_core::CoreError::NotFound(format!("checkpoint {id}")));
        }
        Ok(())
    }

    /// Put the document back as it was at a checkpoint. The current state is
    /// kept as its own checkpoint, so a restore can itself be taken back.
    pub fn restore_checkpoint(&mut self, id: &str) -> CoreResult<()> {
        let cp = self
            .checkpoints
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| ndcart_core::CoreError::NotFound(format!("checkpoint {id}")))?;
        if self.checkpoints.len() >= MAX_CHECKPOINTS {
            self.checkpoints.remove(0);
        }
        self.checkpoints.push(Checkpoint {
            id: new_id("cp"),
            name: format!("Before restoring \u{201c}{}\u{201d}", cp.name),
            created_at: ndcart_core::doc::now_iso(),
            floors: std::mem::replace(&mut self.floors, cp.floors),
        });
        self.journal = Journal::default();
        self.structure = StructJournal::default();
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
    /// The prefab library, shared by every document.
    pub prefabs: BTreeMap<String, ndcart_core::Prefab>,
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
                    checkpoints: std::fs::read_to_string(doc_dir.join("checkpoints.json"))
                        .ok()
                        .and_then(|t| serde_json::from_str(&t).ok())
                        .unwrap_or_default(),
                    proposals: Vec::new(),
                    world_proposals: Vec::new(),
                    rev,
                },
            );
            count += 1;
        }
        if let Ok(rd) = std::fs::read_dir(dir.join("prefabs")) {
            for e in rd.flatten() {
                if let Some(p) = std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|t| serde_json::from_str::<ndcart_core::Prefab>(&t).ok())
                {
                    self.prefabs.insert(p.id.clone(), p);
                }
            }
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

/// Prefabs live in `<data dir>/prefabs/<id>.json`, one file each.
pub fn save_prefab(data_dir: Option<&Path>, p: &ndcart_core::Prefab) {
    let Some(root) = data_dir else { return };
    let dir = root.join("prefabs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(body) = serde_json::to_string(p) {
        if let Err(e) = write_atomic(&dir.join(format!("{}.json", p.id)), body.as_bytes()) {
            eprintln!("ndcart: cannot save prefab: {e}");
        }
    }
}

pub fn delete_prefab_file(data_dir: Option<&Path>, id: &str) {
    if let Some(root) = data_dir {
        let _ = std::fs::remove_file(root.join("prefabs").join(format!("{id}.json")));
    }
}

/// Checkpoints are written only when they change (they hold whole copies).
pub fn save_checkpoints(data_dir: Option<&Path>, doc: &StoredDoc) {
    let Some(root) = data_dir else { return };
    let dir = root.join(&doc.manifest.id);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    match serde_json::to_string(&doc.checkpoints) {
        Ok(body) => {
            if let Err(e) = write_atomic(&dir.join("checkpoints.json"), body.as_bytes()) {
                eprintln!("ndcart: cannot save checkpoints: {e}");
            }
        }
        Err(e) => eprintln!("ndcart: cannot serialize checkpoints: {e}"),
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

    #[test]
    fn a_checkpoint_restores_the_floors_and_the_restore_can_be_taken_back() {
        use ndcart_core::journal::Op;
        let mut d = StoredDoc::new("CP", DocKind::Battle);
        let fid = d.floors[0].id.clone();
        let paint = |cells: &[(i32, i32)]| -> Vec<Op> {
            vec![
                serde_json::from_value(serde_json::json!({"t":"upsert_space","space":{"id":"s","name":"Hall","color":"#7f5539","kind":"room"}})).unwrap(),
                serde_json::from_value(serde_json::json!({"t":"paint_cells","space":"s","cells":cells.iter().map(|(x,y)| serde_json::json!({"x":x,"y":y})).collect::<Vec<_>>()})).unwrap(),
            ]
        };
        d.apply_ops(&fid, &paint(&[(0, 0), (1, 0)])).unwrap();
        let cp = d.create_checkpoint("two cells").unwrap();
        d.apply_ops(&fid, &paint(&[(5, 5)])).unwrap();
        assert_eq!(d.floors[0].grid.cell_count(), 3);
        d.restore_checkpoint(&cp).unwrap();
        assert_eq!(d.floors[0].grid.cell_count(), 2);
        // The three-cell state was saved as its own checkpoint.
        let before = d.checkpoints.last().unwrap().id.clone();
        assert!(d
            .checkpoints
            .last()
            .unwrap()
            .name
            .starts_with("Before restoring"));
        d.restore_checkpoint(&before).unwrap();
        assert_eq!(d.floors[0].grid.cell_count(), 3);
        // History restarts: nothing to undo until the next edit.
        assert!(d.undo_last(&fid).is_err());
        assert!(d.create_checkpoint("  ").is_err());
        assert!(d.delete_checkpoint("nope").is_err());
        d.delete_checkpoint(&cp).unwrap();
        assert!(d.restore_checkpoint(&cp).is_err());
    }
}
