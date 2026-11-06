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
    pub persisted: Persisted,
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
            persisted: Persisted::default(),
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
        self.compact_journal();
        Ok(())
    }

    /// Enforce the undo-window cap (see [`JOURNAL_COMPACT_AT`]). Purely
    /// in-memory: the next save reconciles the files, because a shrunken
    /// journal makes `save_doc_fast` fall back to a full rewrite.
    fn compact_journal(&mut self) {
        let len = self.journal.len();
        if len >= JOURNAL_COMPACT_AT {
            self.journal.drop_front(len - JOURNAL_KEEP);
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
        self.touched();
        Ok(())
    }

    pub fn undo_last(&mut self, floor_id: &str) -> CoreResult<()> {
        let idx = floors_by_id(&self.floors, floor_id)?;
        let floor = &mut self.floors[idx];
        self.journal.undo_last(floor)?;
        self.rev += 1;
        self.manifest.updated_at = ndcart_core::doc::now_iso();
        self.compact_journal();
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
            let snapshot_path = doc_dir.join("snapshot.json");
            let floors_path = doc_dir.join("floors.json"); // before snapshots existed
            let journal_path = doc_dir.join("journal.ndjson");
            if !(manifest_path.is_file() && (snapshot_path.is_file() || floors_path.is_file())) {
                continue;
            }
            let read = |p: &Path| {
                std::fs::read_to_string(p).map_err(|e| {
                    ndcart_core::CoreError::Invalid(format!("reading {}: {e}", p.display()))
                })
            };
            let bad = |p: &Path, e: serde_json::Error| {
                ndcart_core::CoreError::Invalid(format!("parsing {}: {e}", p.display()))
            };
            let manifest: Manifest =
                serde_json::from_str(&read(&manifest_path)?).map_err(|e| bad(&manifest_path, e))?;
            // The snapshot says how much of the journal it already contains; older
            // projects had a plain `floors.json` that always held everything.
            let (mut floors, snapshot_len): (Vec<FloorDoc>, Option<usize>) = if snapshot_path
                .is_file()
            {
                #[derive(serde::Deserialize)]
                struct Snap {
                    journal_len: usize,
                    floors: Vec<FloorDoc>,
                }
                let snap: Snap = serde_json::from_str(&read(&snapshot_path)?)
                    .map_err(|e| bad(&snapshot_path, e))?;
                (snap.floors, Some(snap.journal_len))
            } else {
                (
                    serde_json::from_str(&read(&floors_path)?).map_err(|e| bad(&floors_path, e))?,
                    None,
                )
            };
            let journal_text = std::fs::read_to_string(&journal_path).unwrap_or_default();
            let journal = Journal::from_ndjson(&journal_text)?;
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
                            "replaying the journal of {}: {err}",
                            doc_dir.display()
                        ))
                    })?;
                }
            }
            let structure = StructJournal::from_ndjson(
                &std::fs::read_to_string(doc_dir.join("structure.ndjson")).unwrap_or_default(),
            )?;
            for f in &mut floors {
                f.grid.fixup();
            }
            let rev = journal.len() as u64;
            let jlen = journal.len();
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
                    table: None,
                    rev,
                    persisted: {
                        let p = Persisted::default();
                        p.appended.store(jlen, std::sync::atomic::Ordering::Relaxed);
                        // The snapshot is older than the journal when a tail was replayed.
                        p.snapshot_len
                            .store(snapshot_len, std::sync::atomic::Ordering::Relaxed);
                        p.snapshot_at
                            .store(now_secs(), std::sync::atomic::Ordering::Relaxed);
                        p
                    },
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
    use std::sync::atomic::Ordering::Relaxed;
    let Some(root) = data_dir else { return };
    let dir = root.join(&doc.manifest.id);
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
    }
}

/// Save after an edit without rewriting the whole project: the new journal
/// entries are appended (and synced), and the big snapshot is rewritten only
/// every few hundred edits or minute. Anything that is not a plain edit (floors
/// added, a checkpoint restored…) uses [`save_doc`], which rewrites everything.
pub fn save_doc_fast(data_dir: Option<&Path>, doc: &StoredDoc) {
    use std::io::Write;
    use std::sync::atomic::Ordering::Relaxed;
    let Some(root) = data_dir else { return };
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
        return save_doc(data_dir, doc);
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
                    return save_doc(data_dir, doc);
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
                return save_doc(data_dir, doc);
            }
        }
    }
    let age = now_secs().saturating_sub(doc.persisted.snapshot_at.load(Relaxed));
    if len.saturating_sub(snap) >= SNAPSHOT_EVERY_OPS || (len > snap && age >= SNAPSHOT_EVERY_SECS)
    {
        save_doc(data_dir, doc);
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

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ndcart-fast-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
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
