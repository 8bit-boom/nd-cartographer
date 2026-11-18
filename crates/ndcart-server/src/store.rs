//! In-memory document store with optional persistence.
//!
//! Layout under `data_dir` (when set): one folder per document —
//! `manifest.json`, `snapshot.json` (every floor, plus how many journal entries it
//! already contains), `journal.ndjson` (every floor's edits, tagged by floor,
//! append-only) and `structure.ndjson` (floor add/rename/move/remove). An edit
//! appends to the journal; the snapshot is rewritten every few hundred edits (or a
//! minute), and loading replays the journal's tail over it. Older projects with a
//! plain `floors.json` still open and are upgraded by their next full save.

use ndcart_core::doc::{floors_by_id, DocKind, FloorDoc, Manifest};
use ndcart_core::ids::new_id;
use ndcart_core::journal::{apply_op, Journal, Op};
use ndcart_core::{CoreResult, DocOp, StructJournal};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// What the table screen shows right now (in memory only): the player view
/// from one spot. `n` changes whenever the GM moves or changes it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TableState {
    pub floor: String,
    pub x: f64,
    pub y: f64,
    pub radius: f64,
    pub darkness: bool,
    pub n: u64,
}

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

/// How much of the journal is on disk, and how much of it the snapshot already
/// contains. Atomics so saving can take `&StoredDoc`.
#[derive(Default)]
pub struct Persisted {
    /// Serializes writes to a project's files: a snapshot written off the
    /// store lock (see [`SnapshotJob`]) must not interleave with a full save.
    io: std::sync::Mutex<()>,
    /// Write tickets, handed out under the store lock in state order. A
    /// snapshot job holding an older ticket than the last write is stale
    /// and is dropped instead of overwriting newer files.
    next_gen: std::sync::atomic::AtomicU64,
    written_gen: std::sync::atomic::AtomicU64,
    /// A snapshot job has been prepared and has not finished yet.
    pub snapshot_in_flight: std::sync::atomic::AtomicBool,
    /// Journal entries written to `journal.ndjson`.
    pub appended: std::sync::atomic::AtomicUsize,
    /// Journal length the snapshot (`snapshot.json`) reflects.
    pub snapshot_len: std::sync::atomic::AtomicUsize,
    /// When the snapshot was last written (seconds since the Unix epoch).
    pub snapshot_at: std::sync::atomic::AtomicU64,
    /// When the journal was last flushed to the disk (milliseconds since the epoch).
    pub synced_at: std::sync::atomic::AtomicU64,
}

/// Write a fresh snapshot after this many journal entries, or this many seconds.
const SNAPSHOT_EVERY_OPS: usize = 200;
const SNAPSHOT_EVERY_SECS: u64 = 60;

/// Compaction: the undo journal is capped. A document edited for months must
/// not carry every edit's inverse forever (in memory or in `journal.ndjson`);
/// checkpoints are the long-term history. At `JOURNAL_COMPACT_AT` entries the
/// oldest are dropped so `JOURNAL_KEEP` remain; the next save rewrites the
/// files once (`save_doc_fast`'s "the journal got shorter" path) and normal
/// appends resume. Undo reaches back `JOURNAL_KEEP` edits — the undo error
/// message says so.
pub const JOURNAL_COMPACT_AT: usize = 4096;
pub const JOURNAL_KEEP: usize = 2048;

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A journal append is always handed to the operating system (so a crashed or
/// killed studio loses nothing), but it is forced to the physical disk at most
/// this often: flushing after every stroke made each edit cost whole seconds on
/// some Windows machines. A power cut can lose the last second of edits.
const SYNC_EVERY_MS: u64 = 1_000;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    /// The shared table view, if the GM turned it on.
    pub table: Option<TableState>,
    pub rev: u64,
    /// Per floor, the journal indexes of undo entries that can be redone,
    /// newest last. In memory only: redo does not survive a restart. Any new
    /// edit of a floor forgets that floor's redo history.
    pub redo: std::collections::HashMap<String, Vec<usize>>,
    pub persisted: Arc<Persisted>,
}

impl StoredDoc {
    pub fn new(name: &str, kind: DocKind) -> Self {
        let id = new_id("doc");
        let now = ndcart_core::doc::now_iso();
        let manifest = Manifest {
            id: id.clone(),
            name: name.to_string(),
            kind,
            schema_version: 1,
            cell_size: 32.0,
            created_at: now.clone(),
            updated_at: now,
            nd_world: None,
            style: None,
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
            table: None,
            rev: 0,
            redo: Default::default(),
            persisted: Arc::new(Persisted::default()),
        }
    }

    /// Apply ops to one floor as journal entries (each op keeps its inverse).
    pub fn apply_ops(&mut self, floor_id: &str, ops: &[Op]) -> CoreResult<()> {
        self.apply_ops_keeping_redo(floor_id, ops)?;
        // A new edit starts a new future: what was undone can no longer be redone.
        self.redo.remove(floor_id);
        Ok(())
    }

    fn apply_ops_keeping_redo(&mut self, floor_id: &str, ops: &[Op]) -> CoreResult<()> {
        let idx = floors_by_id(&self.floors, floor_id)?;
        // All or nothing: a bad op in the middle must not leave the ones before it applied
        // and journaled while the request is refused. A failed op leaves the floor as it
        // was, so the ones before it are undone with their journaled inverses (no copy of
        // the floor is taken on the happy path).
        let mark = self.journal.len();
        for op in ops {
            // Journal::push applies and journals; run against the right floor.
            if let Err(e) = self.journal.push(&mut self.floors[idx], op) {
                for entry in self.journal.entries()[mark..].iter().rev() {
                    let _ = apply_op(&mut self.floors[idx], &entry.inverse);
                }
                self.journal.truncate(mark);
                return Err(e);
            }
        }
        self.rev += 1;
        self.manifest.updated_at = ndcart_core::doc::now_iso();
        self.compact_journal();
        Ok(())
    }

    /// Enforce the undo-window cap (see [`JOURNAL_COMPACT_AT`]). Purely
    /// in-memory: the next save reconciles the files, because a shrunken
    /// journal makes `save_doc_fast` fall back to a full rewrite.
    fn compact_journal(&mut self) {
        let len = self.journal.len();
        if len >= JOURNAL_COMPACT_AT {
            let dropped = len - JOURNAL_KEEP;
            self.journal.drop_front(dropped);
            // Redo points at journal indexes: shift them, forget what fell out.
            for stack in self.redo.values_mut() {
                *stack = stack
                    .iter()
                    .filter_map(|i| i.checked_sub(dropped))
                    .collect();
            }
        }
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
        let palette = self
            .manifest
            .style
            .as_ref()
            .map(|s| s.palette.clone())
            .unwrap_or_default();
        let floors = ndcart_core::mapspec::compile_styled(&spec, seed, &palette)?;
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
        self.redo.clear();
        self.touched();
        Ok(())
    }

    /// The op an undo on this floor would apply, without applying it.
    pub fn peek_undo(&self, floor_id: &str) -> Option<ndcart_core::Op> {
        self.journal.peek_undo(floor_id).cloned()
    }

    pub fn undo_last(&mut self, floor_id: &str) -> CoreResult<()> {
        let idx = floors_by_id(&self.floors, floor_id)?;
        let floor = &mut self.floors[idx];
        self.journal.undo_last(floor)?;
        self.redo
            .entry(floor_id.to_string())
            .or_default()
            .push(self.journal.len() - 1);
        self.rev += 1;
        self.manifest.updated_at = ndcart_core::doc::now_iso();
        self.compact_journal();
        Ok(())
    }

    /// The op a redo on this floor would apply, without applying it.
    pub fn peek_redo(&self, floor_id: &str) -> Option<ndcart_core::Op> {
        let u = *self.redo.get(floor_id)?.last()?;
        self.journal.entries().get(u).map(|e| e.inverse.clone())
    }

    /// Take back the last undo on this floor: its inverse is applied as a new
    /// forward entry (so the redo can itself be undone, and the journal stays
    /// append-only).
    pub fn redo_last(&mut self, floor_id: &str) -> CoreResult<()> {
        floors_by_id(&self.floors, floor_id)?;
        let op = self
            .peek_redo(floor_id)
            .ok_or_else(|| ndcart_core::CoreError::NotFound("nothing to redo".to_string()))?;
        // Take it off the stack first: applying may compact the journal, which
        // re-indexes what is left.
        let taken = self.redo.get_mut(floor_id).and_then(|s| s.pop());
        if let Err(e) = self.apply_ops_keeping_redo(floor_id, std::slice::from_ref(&op)) {
            if let Some(u) = taken {
                self.redo.entry(floor_id.to_string()).or_default().push(u);
            }
            return Err(e);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct Store {
    pub docs: BTreeMap<String, StoredDoc>,
    /// The prefab library, shared by every document.
    pub prefabs: BTreeMap<String, ndcart_core::Prefab>,
    /// Project folders `load_dir` could not read (shown by the doctor).
    pub skipped: Vec<String>,
}

impl Store {
    pub fn create(&mut self, name: &str, kind: DocKind) -> &Manifest {
        let doc = StoredDoc::new(name, kind);
        let id = doc.manifest.id.clone();
        self.docs.insert(id.clone(), doc);
        &self.docs[&id].manifest
    }

    pub fn load_dir(&mut self, dir: &Path) -> CoreResult<()> {
        let before = self.docs.len();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let doc_dir = entry.path();
            if !doc_dir.is_dir() {
                continue;
            }
            // One unreadable project must not keep the others from loading.
            if let Err(err) = self.load_one(&doc_dir) {
                eprintln!("ndcart: skipping {}: {err}", doc_dir.display());
                self.skipped.push(
                    doc_dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| doc_dir.display().to_string()),
                );
            }
        }
        let count = self.docs.len() - before;
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

    /// Load one project directory into the store. A directory that is not a
    /// project is ignored (Ok); a broken one is an Err.
    fn load_one(&mut self, doc_dir: &Path) -> CoreResult<()> {
        let manifest_path = doc_dir.join("manifest.json");
        let snapshot_path = doc_dir.join("snapshot.json");
        let floors_path = doc_dir.join("floors.json"); // before snapshots existed
        if !(manifest_path.is_file() && (snapshot_path.is_file() || floors_path.is_file())) {
            return Ok(());
        }
        let read = |p: &Path| {
            std::fs::read_to_string(p).map_err(|e| {
                ndcart_core::CoreError::Invalid(format!("reading {}: {e}", p.display()))
            })
        };
        let files = DocFiles {
            manifest: read(&manifest_path)?,
            snapshot: if snapshot_path.is_file() {
                Some(read(&snapshot_path)?)
            } else {
                None
            },
            legacy_floors: if snapshot_path.is_file() {
                None
            } else {
                Some(read(&floors_path)?)
            },
            journal: std::fs::read_to_string(doc_dir.join("journal.ndjson")).unwrap_or_default(),
            structure: std::fs::read_to_string(doc_dir.join("structure.ndjson"))
                .unwrap_or_default(),
            checkpoints: std::fs::read_to_string(doc_dir.join("checkpoints.json")).ok(),
        };
        let doc = StoredDoc::from_files(files, &doc_dir.display().to_string())?;
        self.docs.insert(doc.manifest.id.clone(), doc);
        Ok(())
    }
}

/// The text of a project's files (what is on disk, or inside a backup).
pub struct DocFiles {
    pub manifest: String,
    /// `snapshot.json`; None for a project that only has the older `floors.json`.
    pub snapshot: Option<String>,
    pub legacy_floors: Option<String>,
    pub journal: String,
    pub structure: String,
    pub checkpoints: Option<String>,
}

impl StoredDoc {
    /// Build a project from its files: the snapshot, with the journal's tail
    /// replayed over it. `label` names the source in error messages.
    pub fn from_files(files: DocFiles, label: &str) -> CoreResult<StoredDoc> {
        let bad = |what: &str, e: serde_json::Error| {
            ndcart_core::CoreError::Invalid(format!("parsing {label}/{what}: {e}"))
        };
        let manifest: Manifest =
            serde_json::from_str(&files.manifest).map_err(|e| bad("manifest.json", e))?;
        // The snapshot says how much of the journal it already contains; older
        // projects had a plain `floors.json` that always held everything.
        let (mut floors, snapshot_len): (Vec<FloorDoc>, Option<usize>) =
            match (&files.snapshot, &files.legacy_floors) {
                (Some(text), _) => {
                    #[derive(serde::Deserialize)]
                    struct Snap {
                        journal_len: usize,
                        floors: Vec<FloorDoc>,
                    }
                    let snap: Snap =
                        serde_json::from_str(text).map_err(|e| bad("snapshot.json", e))?;
                    (snap.floors, Some(snap.journal_len))
                }
                (None, Some(text)) => (
                    serde_json::from_str(text).map_err(|e| bad("floors.json", e))?,
                    None,
                ),
                (None, None) => {
                    return Err(ndcart_core::CoreError::Invalid(format!(
                        "{label} has no snapshot"
                    )))
                }
            };
        let journal = Journal::from_ndjson(&files.journal)?;
        let snapshot_len = snapshot_len.unwrap_or(journal.len()).min(journal.len());
        // Edits made after the snapshot are in the journal's tail: apply them.
        for e in &journal.entries()[snapshot_len..] {
            let target = match &e.floor {
                Some(fid) => floors.iter_mut().find(|f| &f.id == fid),
                None => floors.first_mut(),
            };
            // A floor removed since is simply gone; its old edits have nowhere to go.
            if let Some(f) = target {
                apply_op(f, &e.op).map_err(|err| {
                    ndcart_core::CoreError::Invalid(format!(
                        "replaying the journal of {label}: {err}"
                    ))
                })?;
            }
        }
        let structure = StructJournal::from_ndjson(&files.structure)?;
        for f in &mut floors {
            f.grid.fixup();
        }
        let rev = journal.len() as u64;
        let jlen = journal.len();
        Ok(StoredDoc {
            manifest,
            floors,
            journal,
            structure,
            checkpoints: files
                .checkpoints
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default(),
            proposals: Vec::new(),
            world_proposals: Vec::new(),
            table: None,
            rev,
            redo: Default::default(),
            persisted: Arc::new({
                let p = Persisted::default();
                p.appended.store(jlen, std::sync::atomic::Ordering::Relaxed);
                // The snapshot is older than the journal when a tail was replayed.
                p.snapshot_len
                    .store(snapshot_len, std::sync::atomic::Ordering::Relaxed);
                p.snapshot_at
                    .store(now_secs(), std::sync::atomic::Ordering::Relaxed);
                p
            }),
        })
    }
}

/// Persist one document (snapshot + journal). No-op without a data dir.
pub fn save_doc(data_dir: Option<&Path>, doc: &StoredDoc) {
    use std::sync::atomic::Ordering::{Relaxed, SeqCst};
    let Some(root) = data_dir else { return };
    let dir = root.join(&doc.manifest.id);
    // The ticket is taken under the store lock (the caller holds it); the io
    // lock then waits out a snapshot job that is writing right now.
    let gen = doc.persisted.next_gen.fetch_add(1, SeqCst) + 1;
    let _io = doc.persisted.io.lock().unwrap_or_else(|e| e.into_inner());
    if std::fs::create_dir_all(&dir).is_err() {
        eprintln!("ndcart: cannot create {}", dir.display());
        return;
    }
    // A failed serialize must never take the server down mid-edit: report it
    // and keep the previous files (the journal is the source of truth anyway).
    let journal_len = doc.journal.len();
    let (manifest, snapshot) = match (
        serde_json::to_string_pretty(&doc.manifest),
        serde_json::to_string(
            &serde_json::json!({ "journal_len": journal_len, "floors": &doc.floors }),
        ),
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
    // The snapshot goes before the journal is replaced: if the journal is the one
    // that is old, replaying its tail over a newer snapshot is skipped by length.
    let mut all_ok = true;
    for (name, body) in [
        ("manifest.json", manifest),
        ("snapshot.json", snapshot),
        ("journal.ndjson", journal),
        ("structure.ndjson", doc.structure.to_ndjson()),
    ] {
        if let Err(e) = write_atomic(&dir.join(name), body.as_bytes()) {
            eprintln!("ndcart: cannot save {name}: {e}");
            all_ok = false;
        }
    }
    if all_ok {
        // The older layout's file would only confuse a reader now.
        let _ = std::fs::remove_file(dir.join("floors.json"));
        doc.persisted.appended.store(journal_len, Relaxed);
        doc.persisted.snapshot_len.store(journal_len, Relaxed);
        doc.persisted.snapshot_at.store(now_secs(), Relaxed);
        doc.persisted.written_gen.store(gen, SeqCst);
    }
}

/// The periodic snapshot of a project: the state is cloned under the store
/// lock (about a tenth of the cost of serializing it) and serialized and
/// written to disk after the lock is released, so neither the JSON encoding
/// nor the writes and fsyncs hold up other requests.
///
/// Only `manifest.json` and `snapshot.json` are written. The journal file is
/// not touched (edits keep appending to it under the lock), and the snapshot
/// records how many journal entries it contains, so a reload replays the rest.
pub struct SnapshotJob {
    dir: PathBuf,
    manifest: Manifest,
    floors: Vec<FloorDoc>,
    journal_len: usize,
    gen: u64,
    persisted: Arc<Persisted>,
}

impl SnapshotJob {
    /// Write the files (each crash-safe: temp file, fsync, rename). A job that
    /// a newer write has overtaken is dropped, never allowed to overwrite it.
    pub fn write(self) {
        use std::sync::atomic::Ordering::{Relaxed, SeqCst};
        // The encoding is the expensive part: do it before taking the io lock.
        let bodies = match (
            serde_json::to_string_pretty(&self.manifest),
            serde_json::to_string(
                &serde_json::json!({ "journal_len": self.journal_len, "floors": &self.floors }),
            ),
        ) {
            (Ok(m), Ok(f)) => Some((m, f)),
            _ => {
                eprintln!("ndcart: cannot serialize {}", self.manifest.id);
                None
            }
        };
        if let Some((manifest, snapshot)) = bodies {
            let _io = self.persisted.io.lock().unwrap_or_else(|e| e.into_inner());
            if self.gen > self.persisted.written_gen.load(SeqCst) {
                let ok = [("manifest.json", &manifest), ("snapshot.json", &snapshot)]
                    .iter()
                    .all(|(name, body)| {
                        match write_atomic(&self.dir.join(name), body.as_bytes()) {
                            Ok(()) => true,
                            Err(e) => {
                                eprintln!("ndcart: cannot save {name}: {e}");
                                false
                            }
                        }
                    });
                if ok {
                    self.persisted.snapshot_len.store(self.journal_len, Relaxed);
                    self.persisted.snapshot_at.store(now_secs(), Relaxed);
                    self.persisted.written_gen.store(self.gen, SeqCst);
                }
            }
        }
        self.persisted.snapshot_in_flight.store(false, SeqCst);
    }
}

/// Save after an edit without rewriting the whole project: the new journal
/// entries are appended (and synced), and the big snapshot is rewritten only
/// every few hundred edits or minute. Anything that is not a plain edit (floors
/// added, a checkpoint restored…) uses [`save_doc`], which rewrites everything.
#[cfg(test)]
pub fn save_doc_fast(data_dir: Option<&Path>, doc: &StoredDoc) {
    if let Some(job) = save_doc_edit(data_dir, doc) {
        job.write();
    }
}

/// [`save_doc_fast`], except that a due snapshot is handed back as a
/// [`SnapshotJob`] for the caller to write after releasing the store lock
/// (the journal append still happens here, so no edit waits on the snapshot).
#[must_use = "a snapshot job that is dropped is never written (and blocks the next one)"]
pub fn save_doc_edit(data_dir: Option<&Path>, doc: &StoredDoc) -> Option<SnapshotJob> {
    use std::io::Write;
    use std::sync::atomic::Ordering::{Relaxed, SeqCst};
    let root = data_dir?;
    let dir = root.join(&doc.manifest.id);
    let (appended, snap) = (
        doc.persisted.appended.load(Relaxed),
        doc.persisted.snapshot_len.load(Relaxed),
    );
    let len = doc.journal.len();
    // Never saved here yet, or the journal got shorter (rolled back): write it all.
    if len < appended
        || !dir.join("journal.ndjson").is_file()
        || !dir.join("snapshot.json").is_file()
    {
        save_doc(data_dir, doc);
        return None;
    }
    if len > appended {
        let mut text = String::new();
        for e in &doc.journal.entries()[appended..] {
            match serde_json::to_string(e) {
                Ok(l) => {
                    text.push_str(&l);
                    text.push('\n');
                }
                Err(e) => {
                    eprintln!("ndcart: cannot serialize a journal entry: {e}");
                    save_doc(data_dir, doc);
                    return None;
                }
            }
        }
        let wrote = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("journal.ndjson"))
            .and_then(|mut f| {
                f.write_all(text.as_bytes())?;
                let now = now_millis();
                if now.saturating_sub(doc.persisted.synced_at.load(Relaxed)) >= SYNC_EVERY_MS {
                    f.sync_data()?;
                    doc.persisted.synced_at.store(now, Relaxed);
                }
                Ok(())
            });
        match wrote {
            Ok(()) => doc.persisted.appended.store(len, Relaxed),
            Err(e) => {
                eprintln!("ndcart: cannot append to the journal: {e}");
                save_doc(data_dir, doc);
                return None;
            }
        }
    }
    let age = now_secs().saturating_sub(doc.persisted.snapshot_at.load(Relaxed));
    let due = len.saturating_sub(snap) >= SNAPSHOT_EVERY_OPS
        || (len > snap && age >= SNAPSHOT_EVERY_SECS);
    if !due || doc.persisted.snapshot_in_flight.swap(true, SeqCst) {
        return None; // not due, or one is already on its way
    }
    // Copy the state now, under the caller's lock (it must be consistent);
    // encoding and writing happen in `SnapshotJob::write`.
    Some(SnapshotJob {
        dir,
        manifest: doc.manifest.clone(),
        floors: doc.floors.clone(),
        journal_len: len,
        gen: doc.persisted.next_gen.fetch_add(1, SeqCst) + 1,
        persisted: doc.persisted.clone(),
    })
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

/// Like `write_atomic`, for secrets: the temp file is created 0600 on unix
/// (never briefly world-readable), fsynced, then renamed over the target.
pub(crate) fn write_atomic_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp); // a stale one would defeat create_new
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
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

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ndcart-fast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn failed_apply_ops_rolls_back_without_a_backup_clone() {
        let mut doc = StoredDoc::new("rb", DocKind::Battle);
        let fid = doc.floors[0].id.clone();
        doc.apply_ops(&fid, &paint(0)).unwrap();
        let before = serde_json::to_string(&doc.floors[0]).unwrap();
        let (len, rev) = (doc.journal.len(), doc.rev);
        let ops: Vec<Op> = serde_json::from_value(serde_json::json!([
            {"t":"paint_cells","space":"s","cells":[{"x":5,"y":5}]},
            {"t":"batch","ops":[
                {"t":"paint_cells","space":"s","cells":[{"x":6,"y":6}]},
                {"t":"upsert_space","space":{"id":"s2","name":"Two","color":"#112233","kind":"room"}}]},
            {"t":"paint_cells","space":"no_such_space","cells":[{"x":7,"y":7}]}
        ]))
        .unwrap();
        assert!(doc.apply_ops(&fid, &ops).is_err());
        assert_eq!(serde_json::to_string(&doc.floors[0]).unwrap(), before);
        assert_eq!(doc.journal.len(), len);
        assert_eq!(doc.rev, rev);
    }

    /// One edit = one journal entry (a batch), like a stroke in the UI.
    fn paint(x: i32) -> Vec<Op> {
        serde_json::from_value(serde_json::json!([{"t":"batch","ops":[
            {"t":"upsert_space","space":{"id":"s","name":"Hall","color":"#7f5539","kind":"room"}},
            {"t":"paint_cells","space":"s","cells":[{"x":x,"y":0}]}
        ]}]))
        .unwrap()
    }

    fn reload(dir: &std::path::Path) -> Store {
        let mut st = Store::default();
        st.load_dir(dir).unwrap();
        st
    }

    #[test]
    fn a_snapshot_written_after_the_lock_is_released_keeps_the_files_consistent() {
        let dir = tmpdir("deferred");
        let mut d = StoredDoc::new("Deferred", DocKind::Battle);
        let (id, fid) = (d.manifest.id.clone(), d.floors[0].id.clone());
        save_doc(Some(&dir), &d);
        d.persisted
            .snapshot_at
            .store(u64::MAX / 2, std::sync::atomic::Ordering::Relaxed);
        // The 200th edit makes a snapshot due: it comes back as a job.
        let mut job = None;
        for x in 0..SNAPSHOT_EVERY_OPS as i32 {
            d.apply_ops(&fid, &paint(x)).unwrap();
            assert!(job.is_none(), "no job before the snapshot is due");
            job = save_doc_edit(Some(&dir), &d);
        }
        let job = job.expect("a snapshot is due");
        let snap_before = std::fs::read(dir.join(&id).join("snapshot.json")).unwrap();
        // The write has not happened yet. More edits land meanwhile: their
        // journal entries are appended at once, and no second job is made.
        for x in 1000..1030 {
            d.apply_ops(&fid, &paint(x)).unwrap();
            assert!(save_doc_edit(Some(&dir), &d).is_none());
        }
        assert_eq!(
            std::fs::read(dir.join(&id).join("snapshot.json")).unwrap(),
            snap_before,
            "nothing was written to the snapshot under the lock"
        );
        // Crash right now: snapshot + journal still reproduce the live state.
        assert_eq!(reload(&dir).docs[&id].floors, d.floors);
        job.write();
        assert_ne!(
            std::fs::read(dir.join(&id).join("snapshot.json")).unwrap(),
            snap_before
        );
        assert_eq!(
            d.persisted
                .snapshot_len
                .load(std::sync::atomic::Ordering::Relaxed),
            SNAPSHOT_EVERY_OPS
        );
        // The snapshot is 30 entries behind the journal; a reload replays them.
        let back = reload(&dir);
        assert_eq!(back.docs[&id].floors, d.floors);
        assert_eq!(back.docs[&id].journal.len(), d.journal.len());
        // And the next due snapshot can be made again.
        for x in 2000..2000 + SNAPSHOT_EVERY_OPS as i32 {
            d.apply_ops(&fid, &paint(x)).unwrap();
            if let Some(j) = save_doc_edit(Some(&dir), &d) {
                j.write();
            }
        }
        assert_eq!(reload(&dir).docs[&id].floors, d.floors);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stale_snapshot_job_never_overwrites_a_newer_full_save() {
        let dir = tmpdir("stale-job");
        let mut d = StoredDoc::new("Stale", DocKind::Battle);
        let (id, fid) = (d.manifest.id.clone(), d.floors[0].id.clone());
        save_doc(Some(&dir), &d);
        d.persisted
            .snapshot_at
            .store(u64::MAX / 2, std::sync::atomic::Ordering::Relaxed);
        let mut job = None;
        for x in 0..SNAPSHOT_EVERY_OPS as i32 {
            d.apply_ops(&fid, &paint(x)).unwrap();
            job = save_doc_edit(Some(&dir), &d);
        }
        let job = job.expect("a snapshot is due");
        // A structural change rewrites everything before the job gets to run.
        d.add_floor("Up", None).unwrap();
        d.apply_ops(&fid, &paint(5000)).unwrap();
        save_doc(Some(&dir), &d);
        let newer = std::fs::read(dir.join(&id).join("snapshot.json")).unwrap();
        job.write();
        assert_eq!(
            std::fs::read(dir.join(&id).join("snapshot.json")).unwrap(),
            newer,
            "the older snapshot must not replace the newer one"
        );
        assert!(
            !d.persisted
                .snapshot_in_flight
                .load(std::sync::atomic::Ordering::SeqCst),
            "a dropped job still releases the in-flight flag"
        );
        let back = reload(&dir);
        assert_eq!(back.docs[&id].floors, d.floors);
        assert_eq!(back.docs[&id].floors.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redo_is_per_floor_drops_compacted_entries_and_is_forgotten_on_a_restart() {
        let dir = tmpdir("redo");
        let mut d = StoredDoc::new("Redo", DocKind::Battle);
        let (id, f1) = (d.manifest.id.clone(), d.floors[0].id.clone());
        let f2 = d.add_floor("Up", None).unwrap();
        d.apply_ops(&f1, &paint(0)).unwrap();
        d.apply_ops(&f2, &paint(50)).unwrap();
        d.undo_last(&f1).unwrap();
        // Floor 2's redo is its own: floor 1's undo does not give it one.
        assert!(d.redo_last(&f2).is_err());
        // An edit on floor 2 leaves floor 1's redo alone.
        d.apply_ops(&f2, &paint(51)).unwrap();
        d.redo_last(&f1).unwrap();
        assert_eq!(d.floors[0].grid.cell_count(), 1);
        // Compaction: undo, then enough edits elsewhere to compact the journal.
        d.undo_last(&f1).unwrap();
        for x in 0..JOURNAL_COMPACT_AT as i32 {
            d.apply_ops(&f2, &paint(100 + x)).unwrap();
        }
        // (floor 2's edits do not touch floor 1's redo, but the old undo entry
        // was dropped by the compaction: the redo is gone, cleanly.)
        assert!(d.redo_last(&f1).is_err());
        // A restart forgets redo history; the journal itself reloads.
        d.undo_last(&f2).unwrap();
        save_doc(Some(&dir), &d);
        let mut back = reload(&dir).docs.remove(&id).unwrap();
        assert!(back.redo_last(&f2).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_broken_project_is_skipped_not_fatal() {
        let dir = tmpdir("skip");
        let a = StoredDoc::new("Good", DocKind::Battle);
        let b = StoredDoc::new("Bad", DocKind::Battle);
        let (aid, bid) = (a.manifest.id.clone(), b.manifest.id.clone());
        save_doc(Some(&dir), &a);
        save_doc(Some(&dir), &b);
        std::fs::write(dir.join(&bid).join("snapshot.json"), "{").unwrap();
        let mut st = Store::default();
        st.load_dir(&dir).unwrap();
        assert_eq!(st.docs.len(), 1);
        assert!(st.docs.contains_key(&aid));
        assert_eq!(st.skipped, vec![bid]);
    }

    #[test]
    fn edits_are_appended_and_a_reload_replays_them_over_the_older_snapshot() {
        let dir = tmpdir("append");
        let mut d = StoredDoc::new("Fast", DocKind::Battle);
        let (id, fid) = (d.manifest.id.clone(), d.floors[0].id.clone());
        save_doc(Some(&dir), &d); // the first, full save
        let snap = std::fs::metadata(dir.join(&id).join("snapshot.json"))
            .unwrap()
            .len();
        for x in 0..5 {
            d.apply_ops(&fid, &paint(x)).unwrap();
            save_doc_fast(Some(&dir), &d);
        }
        // Undo is an entry too; it must survive the same way.
        d.undo_last(&fid).unwrap();
        save_doc_fast(Some(&dir), &d);
        // The snapshot was not rewritten (no 200 edits, no minute passed), only the journal grew.
        assert_eq!(
            std::fs::metadata(dir.join(&id).join("snapshot.json"))
                .unwrap()
                .len(),
            snap
        );
        assert_eq!(
            d.persisted
                .snapshot_len
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(
            !dir.join(&id).join("floors.json").exists(),
            "the old layout's file is gone"
        );
        // A fresh load (as after a crash) reproduces the live state exactly...
        let back = reload(&dir);
        let b = &back.docs[&id];
        assert_eq!(b.floors, d.floors);
        assert_eq!(b.journal.len(), d.journal.len());
        assert_eq!(b.floors[0].grid.cell_count(), 4, "five painted, one undone");
        // ...including the ability to keep undoing.
        let mut b = reload(&dir).docs.remove(&id).unwrap();
        b.undo_last(&fid).unwrap();
        assert_eq!(b.floors[0].grid.cell_count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_journal_compacts_at_the_cap_and_a_reload_still_matches() {
        let dir = tmpdir("compact");
        let mut d = StoredDoc::new("Compact", DocKind::Battle);
        let (id, fid) = (d.manifest.id.clone(), d.floors[0].id.clone());
        save_doc(Some(&dir), &d);
        let edits = JOURNAL_COMPACT_AT + 100;
        for x in 0..edits {
            d.apply_ops(&fid, &paint(x as i32)).unwrap();
            save_doc_fast(Some(&dir), &d);
        }
        // The window is capped (the oldest entries are gone, memory and disk).
        assert_eq!(d.journal.len(), JOURNAL_KEEP + 100);
        // Undo still reaches back into the retained window.
        d.undo_last(&fid).unwrap();
        // The next save rewrites the files whole; a reload reproduces the state.
        save_doc_fast(Some(&dir), &d);
        let back = reload(&dir);
        let b = &back.docs[&id];
        assert_eq!(b.floors, d.floors, "state survives compaction + reload");
        assert_eq!(b.journal.len(), d.journal.len());
        assert_eq!(
            b.floors[0].grid.cell_count(),
            edits - 1,
            "every edit painted a distinct cell, one was undone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_snapshot_is_refreshed_every_so_many_edits_and_after_a_rollback_everything_is_rewritten()
    {
        let dir = tmpdir("cadence");
        let mut d = StoredDoc::new("Cadence", DocKind::Battle);
        let (id, fid) = (d.manifest.id.clone(), d.floors[0].id.clone());
        save_doc(Some(&dir), &d);
        // The count rule is under test, not the clock: pretend the snapshot was just taken.
        d.persisted
            .snapshot_at
            .store(u64::MAX / 2, std::sync::atomic::Ordering::Relaxed);
        let snap_len = |d: &StoredDoc| {
            d.persisted
                .snapshot_len
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        for x in 0..SNAPSHOT_EVERY_OPS as i32 {
            d.apply_ops(&fid, &paint(x)).unwrap();
            save_doc_fast(Some(&dir), &d);
        }
        assert_eq!(
            snap_len(&d),
            d.journal.len(),
            "the snapshot caught up once 200 entries piled up"
        );
        let on_disk: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(&id).join("snapshot.json")).unwrap())
                .unwrap();
        assert_eq!(on_disk["journal_len"], d.journal.len());
        // A restore resets the journal (shorter than what was appended): the fast path writes it all.
        let cp = d.create_checkpoint("here").unwrap();
        d.apply_ops(&fid, &paint(500)).unwrap();
        save_doc_fast(Some(&dir), &d);
        d.restore_checkpoint(&cp).unwrap();
        save_doc_fast(Some(&dir), &d);
        let back = reload(&dir);
        assert_eq!(back.docs[&id].floors, d.floors);
        assert_eq!(back.docs[&id].journal.len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_saved_in_the_older_layout_still_opens() {
        let dir = tmpdir("legacy");
        let mut d = StoredDoc::new("Old", DocKind::Battle);
        let (id, fid) = (d.manifest.id.clone(), d.floors[0].id.clone());
        d.apply_ops(&fid, &paint(1)).unwrap();
        d.apply_ops(&fid, &paint(2)).unwrap();
        // Write what the previous version wrote: floors.json (a bare array), the whole journal, no snapshot.json.
        let p = dir.join(&id);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("manifest.json"),
            serde_json::to_string_pretty(&d.manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            p.join("floors.json"),
            serde_json::to_string_pretty(&d.floors).unwrap(),
        )
        .unwrap();
        std::fs::write(p.join("journal.ndjson"), d.journal.to_ndjson()).unwrap();
        let back = reload(&dir);
        assert_eq!(back.docs[&id].floors, d.floors);
        assert_eq!(back.docs[&id].journal.len(), d.journal.len());
        // The next full save upgrades it in place.
        save_doc(Some(&dir), &back.docs[&id]);
        assert!(p.join("snapshot.json").exists() && !p.join("floors.json").exists());
        assert_eq!(reload(&dir).docs[&id].floors, d.floors);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
