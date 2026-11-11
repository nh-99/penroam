//! The live notebook library in the app's private data directory.
//!
//! ```text
//! <root>/library.json                       format marker {"format":"penroam-library","version":2}
//! <root>/.lock                              exclusive lock held while the library is open
//! <root>/device.json                        this installation's device id
//! <root>/journal/<seq>.pjl                  device operation journal (durability source)
//! <root>/checkpoint.json (+ .prev.json)     checkpoint manifest (what the snapshots cover)
//! <root>/outbox.json                        sync outbox counters (derivable)
//! <root>/folders.json                       library folders, with deleted ones (atomic writes)
//! <root>/notebooks/<nb>/notebook.json       notebook snapshot (schema 2, atomic writes)
//! <root>/notebooks/<nb>/pages/<page>.prp    page snapshot, binary (atomic writes)
//! <root>/notebooks/<nb>/pages/<page>.prp.prev  previous page snapshot (fallback)
//! <root>/notebooks/<nb>/assets/<sha256>     immutable attachments
//! <root>/notebooks/<nb>/thumbs/<page>.png   derived thumbnail cache (never exported)
//! <root>/notebooks/<nb>/orphans/            files found unreferenced at start-up (kept)
//! <root>/notebooks/<nb>/history/            restorable versions (see `crate::history`)
//! <root>/migration-backups/                 original bytes of migrated legacy files
//! <root>/staging/                           temporary import/export work area
//! ```
//!
//! **Durability contract.** Every change is a transaction of
//! [`LocalOperation`]s appended to the journal and fsynced *before* the call
//! returns; that is what "Saved locally" means. Snapshots (`.prp`,
//! `notebook.json`) are checkpoints that may lag the journal. A checkpoint
//! writes the snapshot atomically (keeping the previous one as `.prev`),
//! then the manifest; journal segments are deleted only when both the
//! current and the previous checkpoint cover them. On open: the journal is
//! validated (torn tail truncated, damage quarantined), then every
//! transaction after the checkpoint floor is replayed — idempotently, so
//! replaying more than necessary is harmless.
//!
//! Operation order comes from the per-device `local_sequence` only — never
//! from wall-clock time.
//!
//! **Bookkeeping that makes this safe.** Each cached page tracks the first
//! journal sequence not yet in its snapshot; notebooks whose `notebook.json`
//! lags do the same; transactions for pages that are not loaded wait in
//! `pending`; and journal data of a notebook that failed to open is *held*.
//! The checkpoint floor is the minimum of all of these, so a segment is
//! never deleted while anything it contains exists only in the journal.
//! Page transactions replay only when their revision is exactly one above
//! the snapshot's; a gap stops replay for that page (data is kept, never
//! guessed).
//!
//! **Sync.** [`Library::install_merged`] writes a merged result from the sync
//! engine as snapshots without journaling; it bumps page revisions past any
//! journaled local batch and records an install mark in the manifest so that
//! replay skips notebook-level transactions the merged snapshot already
//! contains. With sync enabled, journal segments are also kept until the
//! engine has copied them ([`Library::mark_uploaded`]).
//!
//! **Legacy libraries** (per-page JSON-lines journals) are migrated on open
//! by the private `legacy` module, with the original bytes backed up.

use crate::error::{Result, StorageError};
use crate::fsutil::{self, write_atomic, TMP_MARKER};
use crate::history::{self, ChainMeta, Structure};
use crate::journal::{Journal, JournalError, Txn, TxnTarget};
use crate::state::*;
use penroam_core::folder::{apply_folder_op, Folder, FolderSet, LIBRARY_STREAM_ID};
use penroam_core::migrate::{notebook_from_slice, page_from_slice};
use penroam_core::operation::*;
use penroam_core::validate::{validate_notebook, validate_page, validate_title};
use penroam_core::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use ts_rs::TS;
use uuid::Uuid;

/// Space between pages gathered onto one infinite canvas
/// ([`Library::collapse_pages_into_canvas`]), before rounding up to a whole
/// row of the paper.
pub const CANVAS_PAGE_GAP_PT: f64 = 48.0;

/// A PDF page drawn as an element at (`x`, `y`), at its displayed size.
fn pdf_page_element(id: Uuid, pdf: PdfBackgroundRef, x: f64, y: f64) -> Element {
    let size = pdf.display_size();
    Element::Image(ImageElement {
        id,
        z: 0,
        x,
        y,
        width: size.width,
        height: size.height,
        asset_hash: pdf.asset_hash.clone(),
        pdf_page: Some(pdf),
        extra: Default::default(),
    })
}

fn set_element_id(e: &mut Element, id: Uuid) {
    match e {
        Element::Ink(s) => s.id = id,
        Element::Text(t) => t.id = id,
        Element::Image(i) => i.id = id,
    }
}

const JOURNAL_MAX_ENTRIES: usize = 512;
const JOURNAL_MIN_COMPACT_BYTES: u64 = 1 << 20;
// Pages kept parsed in memory (LRU). Small on purpose: a 10k-stroke page is
// ~20 MB of Rust structs and the editor shows one page at a time.
const PAGE_CACHE: usize = 3;
/// Batch ids remembered for retry deduplication.
const RECENT_BATCHES: usize = 4096;

/// One row of the library view (cheap: read from `notebook.json` only).
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NotebookSummary {
    /// Notebook id.
    pub id: Uuid,
    /// Title as shown.
    pub title: String,
    /// Cover colour (`#rrggbb`).
    pub cover: String,
    /// Number of live (not trashed) pages.
    pub page_count: u32,
    /// Creation time (ms since the epoch; informational).
    #[ts(type = "number")]
    pub created_at: Timestamp,
    /// Last change (ms since the epoch); the list is sorted by it.
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    /// Set when the notebook is in the trash.
    #[ts(optional, type = "number")]
    pub deleted_at: Option<Timestamp>,
    /// First live page, used for the cover thumbnail.
    #[ts(optional)]
    pub first_page_id: Option<Uuid>,
    /// The folder to show it in; absent at the top level. Already accounts
    /// for deleted folders.
    #[ts(optional)]
    pub folder: Option<Uuid>,
}

/// An attachment for [`Library::create_notebook_with`].
#[derive(Debug, Clone)]
pub struct NewAsset {
    /// The bytes; stored by their SHA-256.
    pub bytes: Vec<u8>,
    /// MIME type.
    pub media_type: String,
    /// Original file name, if any.
    pub name: Option<String>,
}

/// A page for [`Library::create_notebook_with`].
#[derive(Debug, Clone)]
pub struct NewPage {
    /// Size class.
    pub paper: Paper,
    /// Size in points.
    pub size: Size,
    /// Background template.
    pub template: Template,
    /// Original PDF page behind this page.
    pub background: Option<PdfBackgroundRef>,
    /// Initial content, with z values assigned.
    pub elements: Vec<Element>,
}

/// What start-up recovery found and did. Shown to the user only when
/// something noteworthy happened (warnings are never silent).
#[derive(Debug, Clone, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecoveryReport {
    /// Leftover temporary files from interrupted atomic writes (removed).
    pub removed_temp_files: u32,
    /// Unreferenced page/asset files moved to `orphans/` (kept, not deleted).
    pub orphaned_files: u32,
    /// Incomplete trailing records discarded (crash during a write).
    pub truncated_journals: u32,
    /// Damaged journal records found (segment quarantined, nothing deleted).
    pub corrupt_journal_records: u32,
    /// Journal transactions applied on top of snapshots.
    pub replayed_batches: u32,
    /// Pages recovered from their previous snapshot.
    pub fallback_snapshots: u32,
    /// Notebooks migrated from the legacy storage layout.
    pub migrated_notebooks: u32,
    /// Human-readable descriptions of everything above and of anything that
    /// could not be repaired.
    pub warnings: Vec<String>,
}

/// Result of [`Library::apply_page_operations`].
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyOutcome {
    /// The page revision after the batch; the editor builds its next batch on it.
    #[ts(type = "number")]
    pub revision: u64,
    /// Time of the change (ms since the epoch; informational).
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    /// The batch is on disk (journal fsynced): "Saved locally".
    pub durable: bool,
}

/// Outbox status (see [`OutboxState`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxStatus {
    /// This library's device id.
    pub device_id: Uuid,
    /// Highest journaled local sequence.
    pub enqueued_through: u64,
    /// Highest sequence the sync engine has taken over.
    pub uploaded_through: u64,
    /// `enqueued_through - uploaded_through`.
    pub pending: u64,
    /// Whether journal retention waits for the sync engine.
    pub sync_enabled: bool,
}

struct CachedPage {
    page: Page,
    journal_entries: usize,
    journal_bytes: u64,
    snapshot_bytes: u64,
    last_used: u64,
    /// Lowest journal sequence applied to this page that no snapshot holds yet.
    first_dirty_seq: Option<u64>,
}

/// Test/benchmark knobs.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct Tuning {
    /// Checkpoint a page after this many journaled batches.
    pub max_entries: usize,
    /// ... or once its journal bytes exceed max(this, snapshot size / 4).
    pub min_compact_bytes: u64,
}

/// The open notebook library (single writer: holds an exclusive lock file).
///
/// Every mutating method journals its operations durably before returning
/// (see the module docs). Pages are loaded lazily and kept in a small LRU
/// cache; evicting a page checkpoints it.
pub struct Library {
    root: PathBuf,
    notebooks: HashMap<Uuid, Notebook>,
    pages: HashMap<(Uuid, Uuid), CachedPage>,
    tick: u64,
    /// What start-up recovery found (and later non-fatal problems).
    pub recovery: RecoveryReport,
    journal: Journal,
    device_id: Uuid,
    last_seq: u64,
    manifest: CheckpointManifest,
    outbox: OutboxState,
    /// Page content transactions not in any snapshot, for pages not loaded yet.
    pending: HashMap<(Uuid, Uuid), Vec<Txn>>,
    /// Notebooks whose `notebook.json` lags the journal (first unsaved seq).
    /// [`LIBRARY_STREAM_ID`] stands for `folders.json`.
    dirty_notebooks: HashMap<Uuid, u64>,
    /// The library's folders, deleted ones included.
    folders: FolderSet,
    /// Lowest sequence of journal data for notebooks that could not be
    /// opened; the checkpoint floor never passes it (nothing is discarded).
    held_seq: Option<u64>,
    recent_batches: HashMap<Uuid, (Uuid, Uuid, ApplyOutcome)>,
    recent_order: std::collections::VecDeque<Uuid>,
    purged: HashSet<Uuid>,
    /// See [`CheckpointManifest::installed`].
    installed: std::collections::BTreeMap<Uuid, u64>,
    tuning: Tuning,
    id_counter: Option<u128>,
    /// Per-page history bookkeeping, filled lazily.
    history_chains: HashMap<(Uuid, Uuid), ChainMeta>,
    /// Last recorded title and page order per notebook.
    history_structure: HashMap<Uuid, Structure>,
    _lock: File,
}

/// Default cover colours, assigned in turn to new notebooks.
pub const COVERS: &[&str] = &[
    "#3b5b7a", "#7a3b3b", "#3b7a52", "#6b4f8a", "#8a6f3b", "#3b6f7a", "#545454",
];

fn ioerr(e: impl std::fmt::Display) -> StorageError {
    StorageError::Io(e.to_string())
}

impl From<JournalError> for StorageError {
    fn from(e: JournalError) -> Self {
        match e {
            JournalError::Io(e) => e.into(),
            other => StorageError::Corrupt(other.to_string()),
        }
    }
}

impl From<OpError> for StorageError {
    fn from(e: OpError) -> Self {
        match e {
            OpError::NoNotebook(id) => StorageError::NotFound(format!("notebook {id}")),
            OpError::NoPage(id) => StorageError::NotFound(format!("page {id}")),
            OpError::Apply(a) => a.into(),
            other => StorageError::Invalid(other.to_string()),
        }
    }
}

impl Library {
    /// Opens (or creates) the library and runs start-up recovery.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("notebooks"))?;
        // one writer per library (double-launch, second window, sync helper)
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(StorageError::Conflict(
                    "the library is already open in another Penroam process".into(),
                ))
            }
            Err(fs::TryLockError::Error(e)) => return Err(e.into()),
        }
        let marker_path = root.join("library.json");
        let marker_version = read_json::<LibraryMarker>(&marker_path).map(|m| m.version);
        if let Some(v) = marker_version {
            if v > LIBRARY_FORMAT_VERSION {
                return Err(StorageError::Corrupt(format!(
                    "this library was written by a newer Penroam (library format {v}); please update"
                )));
            }
        }
        let staging = root.join("staging");
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        fs::create_dir_all(&staging)?;

        let mut recovery = RecoveryReport::default();
        let (journal, txns, scan) = Journal::open(&root.join("journal"))?;
        recovery.truncated_journals += scan.truncated_tails;
        recovery.corrupt_journal_records += scan.corrupt_records;
        recovery.warnings.extend(scan.warnings);

        let (manifest, warn) = read_manifest(&root);
        recovery.warnings.extend(warn);
        let journal_device = txns.iter().flat_map(|t| &t.ops).map(|o| o.device_id).next();
        let device_id = match read_json::<DeviceFile>(&root.join("device.json")) {
            Some(d) => d.device_id,
            None => {
                let id = manifest
                    .as_ref()
                    .map(|m| m.device_id)
                    .or(journal_device)
                    .unwrap_or_else(Uuid::new_v4);
                write_json(
                    &root.join("device.json"),
                    &DeviceFile {
                        format: "penroam-device".into(),
                        device_id: id,
                    },
                )?;
                id
            }
        };
        let manifest = manifest.unwrap_or_else(|| CheckpointManifest::new(device_id));
        let last_seq = manifest
            .last_seq
            .max(journal.max_seq().unwrap_or(0))
            .max(txns.last().map_or(0, Txn::last_seq));
        let outbox = read_json::<OutboxState>(&root.join("outbox.json"))
            .filter(|o| o.device_id == device_id && o.format == "penroam-outbox")
            .unwrap_or_else(|| {
                // derivable: unknown upload state only causes re-uploads
                let mut o = OutboxState::new(device_id);
                o.enqueued_through = last_seq;
                o
            });

        let mut lib = Library {
            root,
            notebooks: HashMap::new(),
            pages: HashMap::new(),
            tick: 0,
            recovery,
            journal,
            device_id,
            last_seq,
            manifest,
            outbox,
            pending: HashMap::new(),
            dirty_notebooks: HashMap::new(),
            folders: FolderSet::default(),
            held_seq: None,
            recent_batches: HashMap::new(),
            recent_order: Default::default(),
            purged: HashSet::new(),
            installed: Default::default(),
            tuning: Tuning {
                max_entries: JOURNAL_MAX_ENTRIES,
                min_compact_bytes: JOURNAL_MIN_COMPACT_BYTES,
            },
            id_counter: None,
            history_chains: HashMap::new(),
            history_structure: HashMap::new(),
            _lock: lock,
        };
        lib.installed = lib.manifest.installed.clone();
        lib.load_notebooks()?;
        lib.load_folders();
        lib.replay(txns)?;
        lib.outbox.enqueued_through = lib.last_seq;
        if marker_version != Some(LIBRARY_FORMAT_VERSION) {
            write_json(
                &marker_path,
                &LibraryMarker {
                    format: "penroam-library".into(),
                    version: LIBRARY_FORMAT_VERSION,
                },
            )?;
        }
        Ok(lib)
    }

    /// The library folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Scratch folder for import/export (emptied on every open).
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    /// This installation's device id (recorded in every operation).
    pub fn device_id(&self) -> Uuid {
        self.device_id
    }

    /// Highest journaled local sequence.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub(crate) fn nb_dir(&self, nb: Uuid) -> PathBuf {
        self.root.join("notebooks").join(nb.to_string())
    }
    fn page_path(&self, nb: Uuid, page: Uuid) -> PathBuf {
        self.nb_dir(nb).join("pages").join(format!("{page}.prp"))
    }
    fn prev_page_path(&self, nb: Uuid, page: Uuid) -> PathBuf {
        self.nb_dir(nb)
            .join("pages")
            .join(format!("{page}.prp.prev"))
    }
    /// Pre-binary snapshots (JSON); migrated on open, still readable.
    fn legacy_page_path(&self, nb: Uuid, page: Uuid) -> PathBuf {
        self.nb_dir(nb).join("pages").join(format!("{page}.json"))
    }
    pub(crate) fn asset_path(&self, nb: Uuid, hash: &str) -> PathBuf {
        self.nb_dir(nb).join("assets").join(hash)
    }
    fn thumb_path(&self, nb: Uuid, page: Uuid) -> PathBuf {
        self.nb_dir(nb).join("thumbs").join(format!("{page}.png"))
    }

    fn new_id(&mut self) -> Uuid {
        match &mut self.id_counter {
            Some(c) => {
                *c += 1;
                Uuid::from_u128(*c)
            }
            None => Uuid::new_v4(),
        }
    }

    // ------------------------------------------------------------ recovery --

    fn load_notebooks(&mut self) -> Result<()> {
        let dir = self.root.join("notebooks");
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| Uuid::parse_str(n).ok())
            else {
                continue;
            };
            self.load_notebook_dir(&path, id);
        }
        Ok(())
    }

    /// Reads `folders.json`. A damaged file is set aside and reported rather
    /// than overwritten by the next save.
    fn load_folders(&mut self) {
        let path = self.root.join("folders.json");
        let Ok(raw) = fs::read(&path) else {
            return; // never had any
        };
        match serde_json::from_slice(&raw) {
            Ok(folders) => self.folders = folders,
            Err(e) => {
                let _ = fs::rename(&path, self.root.join("folders.json.damaged"));
                self.recovery
                    .warnings
                    .push(format!("folders could not be read and were set aside: {e}"));
            }
        }
    }

    /// Loads one notebook folder into memory. A notebook that can't be read is reported in
    /// [`Library::recovery`] and left on disk untouched.
    fn load_notebook_dir(&mut self, path: &Path, id: Uuid) -> bool {
        {
            self.remove_temp_files(path);
            let loaded = fs::read(path.join("notebook.json"))
                .map_err(StorageError::from)
                .and_then(|raw| {
                    if crate::legacy::needed(path, &raw) {
                        let (nb, out) = crate::legacy::migrate(&self.root, path, &raw)?;
                        self.recovery.migrated_notebooks += 1;
                        self.recovery.replayed_batches += out.replayed;
                        self.recovery.truncated_journals += out.truncated;
                        self.recovery.warnings.extend(out.warnings);
                        Ok(nb)
                    } else {
                        notebook_from_slice(&raw).map_err(|e| StorageError::Corrupt(e.to_string()))
                    }
                })
                .and_then(|mut nb| {
                    validate_notebook(&nb)?;
                    if !nb.order_is_consistent() {
                        nb.rebuild_order_keys();
                        self.recovery.warnings.push(format!(
                            "notebook {}: page order keys were inconsistent and were rebuilt",
                            nb.id
                        ));
                        write_atomic(
                            &path.join("notebook.json"),
                            &serde_json::to_vec_pretty(&nb).map_err(ioerr)?,
                        )?;
                    }
                    Ok(nb)
                });
            match loaded {
                Ok(nb) if nb.id == id => {
                    self.collect_orphans(&nb);
                    self.notebooks.insert(id, nb);
                    true
                }
                Ok(_) => {
                    self.recovery.warnings.push(format!(
                        "notebook {id}: id does not match its folder; skipped"
                    ));
                    false
                }
                Err(e) => {
                    self.recovery.warnings.push(format!(
                        "notebook {id} could not be opened and was left untouched: {e}"
                    ));
                    false
                }
            }
        }
    }

    /// Re-applies journal transactions after the checkpoint floor.
    fn replay(&mut self, txns: Vec<Txn>) -> Result<()> {
        // a purge erases everything before it for that notebook
        let mut purge_seq: HashMap<Uuid, u64> = HashMap::new();
        for t in &txns {
            for op in &t.ops {
                if let DocumentOperation::NotebookPurged { notebook_id } = op.body {
                    purge_seq.insert(notebook_id, op.local_sequence);
                }
            }
        }
        // a sync install after the purge brought the notebook back; the journal
        // keeps the old purge for a while, and it must not erase it a second time
        purge_seq.retain(|id, seq| self.installed.get(id).is_none_or(|s| s < seq));
        self.purged.extend(purge_seq.keys());
        let floor = self.manifest.floor;
        for t in txns {
            let nb_id = t.target.notebook_id();
            if let TxnTarget::Page {
                notebook_id,
                page_id,
                revision,
                batch_id,
            } = &t.target
            {
                self.remember_batch(
                    *batch_id,
                    *notebook_id,
                    *page_id,
                    ApplyOutcome {
                        revision: *revision,
                        updated_at: t.ops.last().map_or(0, |o| o.at),
                        durable: true,
                    },
                );
            }
            if purge_seq.get(&nb_id).is_some_and(|p| t.first_seq() < *p) {
                continue;
            }
            if t.last_seq() <= floor {
                continue; // covered by the snapshots
            }
            if matches!(t.target, TxnTarget::Notebook { .. })
                && self
                    .manifest
                    .installed
                    .get(&nb_id)
                    .is_some_and(|s| t.last_seq() <= *s)
            {
                continue; // contained, merged, in a sync install
            }
            match &t.target {
                TxnTarget::Page {
                    notebook_id,
                    page_id,
                    ..
                } => {
                    if !self.notebooks.contains_key(notebook_id) {
                        self.hold(t.first_seq(), format!("page {page_id}"));
                        continue;
                    }
                    self.pending
                        .entry((*notebook_id, *page_id))
                        .or_default()
                        .push(t);
                }
                TxnTarget::Notebook { notebook_id } => {
                    let first = t.first_seq();
                    for op in &t.ops {
                        if let Err(e) = self.apply_notebook_op(op, true) {
                            self.recovery.warnings.push(format!(
                                "journal {} ({}) for notebook {notebook_id} could not be replayed: {e}",
                                op.local_sequence,
                                op.body.name()
                            ));
                            if !self.notebooks.contains_key(notebook_id)
                                && *notebook_id != LIBRARY_STREAM_ID
                                && !matches!(op.body, DocumentOperation::NotebookPurged { .. })
                            {
                                self.hold(first, format!("notebook {notebook_id}"));
                            }
                        }
                    }
                    if self.notebooks.contains_key(notebook_id) {
                        if let Err(e) = self.save_notebook_snapshot(*notebook_id) {
                            self.dirty_notebooks.entry(*notebook_id).or_insert(first);
                            self.recovery
                                .warnings
                                .push(format!("notebook {notebook_id}: {e}"));
                        }
                    } else if *notebook_id == LIBRARY_STREAM_ID {
                        if let Err(e) = self.save_folders() {
                            self.dirty_notebooks.entry(*notebook_id).or_insert(first);
                            self.recovery.warnings.push(format!("folders: {e}"));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn hold(&mut self, seq: u64, what: String) {
        if self.held_seq.is_none_or(|h| seq < h) {
            self.held_seq = Some(seq);
        }
        self.recovery.warnings.push(format!(
            "journal entries for {what} were kept but not applied: its notebook could not be opened"
        ));
    }

    fn remove_temp_files(&mut self, dir: &Path) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                self.remove_temp_files(&p);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.contains(TMP_MARKER))
                && fs::remove_file(&p).is_ok()
            {
                self.recovery.removed_temp_files += 1;
            }
        }
    }

    /// Moves page files and assets that the notebook does not reference into
    /// `orphans/` (e.g. a crash between writing a new page and listing it).
    fn collect_orphans(&mut self, nb: &Notebook) {
        let dir = self.nb_dir(nb.id);
        let known: HashSet<String> = nb.all_page_ids().map(|p| p.to_string()).collect();
        let assets: HashSet<&str> = nb.assets.iter().map(|a| a.sha256.as_str()).collect();
        let orphans = dir.join("orphans");
        let mut moved = 0;
        let keep_page = |n: &str| known.contains(n.split('.').next().unwrap_or(""));
        let keep_asset = |n: &str| assets.contains(n);
        type Keep<'a> = &'a dyn Fn(&str) -> bool;
        let subs: [(&str, Keep); 2] = [("pages", &keep_page), ("assets", &keep_asset)];
        for (sub, keep) in subs {
            if let Ok(rd) = fs::read_dir(dir.join(sub)) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !keep(&name) {
                        let _ = fs::create_dir_all(&orphans);
                        if fs::rename(e.path(), orphans.join(&name)).is_ok() {
                            moved += 1;
                        }
                    }
                }
            }
        }
        if moved > 0 {
            self.recovery.orphaned_files += moved;
            self.recovery.warnings.push(format!(
                "notebook {}: {moved} unreferenced file(s) moved to orphans/",
                nb.id
            ));
        }
    }

    // ---------------------------------------------------------- operations --

    fn make_ops(
        &self,
        bodies: Vec<(OperationId, DocumentOperation)>,
        at: Timestamp,
    ) -> Vec<LocalOperation> {
        bodies
            .into_iter()
            .enumerate()
            .map(|(i, (id, body))| {
                let seq = self.last_seq + 1 + i as u64;
                LocalOperation {
                    id,
                    device_id: self.device_id,
                    local_sequence: seq,
                    schema_version: op_schema_for(&body),
                    causal_dependencies: if seq > 1 {
                        vec![OperationRef {
                            device_id: self.device_id,
                            device_seq: seq - 1,
                        }]
                    } else {
                        vec![]
                    },
                    at,
                    body,
                }
            })
            .collect()
    }

    /// Journals a transaction (durable on return) and advances the sequence.
    fn journal_txn(&mut self, txn: &Txn) -> Result<u64> {
        let bytes = self.journal.append(txn)?;
        self.last_seq = txn.last_seq();
        self.outbox.enqueued_through = self.last_seq;
        Ok(bytes)
    }

    /// Validates, journals and applies notebook-level operations. Returns
    /// once they are durable; snapshot writes after that are checkpoints.
    fn commit_notebook_ops(&mut self, nb_id: Uuid, bodies: Vec<DocumentOperation>) -> Result<()> {
        let at = now_ms();
        let bodies: Vec<(OperationId, DocumentOperation)> =
            bodies.into_iter().map(|b| (self.new_id(), b)).collect();
        // dry run on a copy: nothing is journaled unless everything applies
        let mut probe = match &bodies[0].1 {
            DocumentOperation::NotebookCreated { .. } => None,
            _ => Some(self.notebook(nb_id)?.clone()),
        };
        let ops = self.make_ops(bodies, at);
        for op in &ops {
            validate_body(&op.body)?;
            match (&op.body, probe.as_mut()) {
                (DocumentOperation::NotebookCreated { .. }, None) => {
                    probe = Some(notebook_from_create(op)?);
                }
                (DocumentOperation::NotebookPurged { .. }, _) => {}
                (_, Some(nb)) => {
                    apply_to_notebook(nb, op)?;
                }
                (_, None) => return Err(StorageError::NotFound(format!("notebook {nb_id}"))),
            }
        }
        if let Some(nb) = &probe {
            validate_notebook(nb)?;
        }
        let txn = Txn {
            target: TxnTarget::Notebook { notebook_id: nb_id },
            ops,
        };
        self.journal_txn(&txn)?;
        // durable from here on: failures below are recovered by replay
        for op in &txn.ops {
            if let Err(e) = self.apply_notebook_op(op, false) {
                self.recovery.warnings.push(format!(
                    "applying {} failed after journaling: {e}",
                    op.body.name()
                ));
            }
        }
        if self.notebooks.contains_key(&nb_id) {
            if let Err(e) = self.save_notebook_snapshot(nb_id) {
                self.dirty_notebooks.entry(nb_id).or_insert(txn.first_seq());
                self.recovery
                    .warnings
                    .push(format!("notebook {nb_id} snapshot deferred: {e}"));
            }
        }
        Ok(())
    }

    /// Applies one journaled notebook-level operation to memory and files
    /// (page creation, purge). Idempotent (also used by replay).
    fn apply_notebook_op(&mut self, op: &LocalOperation, replay: bool) -> Result<()> {
        let nb_id = op.body.notebook_id();
        if op.body.is_folder_op() {
            apply_folder_op(&mut self.folders, op)?;
            return Ok(());
        }
        match &op.body {
            DocumentOperation::NotebookCreated { .. } => {
                if self.notebooks.contains_key(&nb_id) {
                    return Ok(());
                }
                let nb = notebook_from_create(op)?;
                fs::create_dir_all(self.nb_dir(nb_id).join("pages"))?;
                fs::create_dir_all(self.nb_dir(nb_id).join("assets"))?;
                self.notebooks.insert(nb_id, nb);
                self.dirty_notebooks
                    .entry(nb_id)
                    .or_insert(op.local_sequence);
            }
            DocumentOperation::NotebookPurged { .. } => {
                self.pages.retain(|(n, _), _| *n != nb_id);
                self.pending.retain(|(n, _), _| *n != nb_id);
                self.forget_history(nb_id);
                self.dirty_notebooks.remove(&nb_id);
                self.notebooks.remove(&nb_id);
                self.purged.insert(nb_id);
                fsutil::remove_dir_all_durable(&self.nb_dir(nb_id))?;
            }
            _ => {
                let nb = self
                    .notebooks
                    .get_mut(&nb_id)
                    .ok_or_else(|| StorageError::NotFound(format!("notebook {nb_id}")))?;
                let effect = apply_to_notebook(nb, op)?;
                let changed = !matches!(effect, NotebookEffect::Unchanged);
                let page_id = match &op.body {
                    DocumentOperation::PageCreated { page_id, .. } => Some(*page_id),
                    _ => None,
                };
                let new_page = match effect {
                    NotebookEffect::PageCreated(p) => Some(*p),
                    // replay of a creation whose snapshot never got written
                    NotebookEffect::Unchanged
                        if replay
                            && page_id.is_some_and(|p| {
                                !self.page_path(nb_id, p).exists()
                                    && !self.prev_page_path(nb_id, p).exists()
                                    && !self.legacy_page_path(nb_id, p).exists()
                            }) =>
                    {
                        let DocumentOperation::PageCreated {
                            page_id,
                            paper,
                            size,
                            template,
                            background,
                            ..
                        } = &op.body
                        else {
                            unreachable!()
                        };
                        let mut p = Page::new(*page_id, *paper, *size, template.clone(), op.at);
                        p.background = background.clone();
                        Some(p)
                    }
                    _ => None,
                };
                if changed {
                    self.dirty_notebooks
                        .entry(nb_id)
                        .or_insert(op.local_sequence);
                }
                if let Some(page) = new_page {
                    self.install_new_page(nb_id, page, op.local_sequence)?;
                }
            }
        }
        Ok(())
    }

    /// Caches a freshly created page as dirty and tries to snapshot it.
    fn install_new_page(&mut self, nb_id: Uuid, page: Page, seq: u64) -> Result<()> {
        let key = (nb_id, page.id);
        if self.pages.contains_key(&key) {
            return Ok(());
        }
        self.evict_if_needed()?;
        self.tick += 1;
        self.pages.insert(
            key,
            CachedPage {
                page,
                journal_entries: 1,
                journal_bytes: 0,
                snapshot_bytes: 0,
                last_used: self.tick,
                first_dirty_seq: Some(seq),
            },
        );
        if let Err(e) = self.snapshot_page(nb_id, key.1) {
            self.recovery
                .warnings
                .push(format!("page {} snapshot deferred: {e}", key.1));
        }
        Ok(())
    }

    fn save_notebook_snapshot(&mut self, nb_id: Uuid) -> Result<()> {
        let nb = self.notebook(nb_id)?;
        let bytes = serde_json::to_vec_pretty(nb).map_err(ioerr)?;
        let structure = Structure {
            at: nb.updated_at,
            title: nb.title.clone(),
            page_order: nb.page_order.clone(),
        };
        write_atomic(&self.nb_dir(nb_id).join("notebook.json"), &bytes)?;
        self.dirty_notebooks.remove(&nb_id);
        let nb_dir = self.nb_dir(nb_id);
        let mut last = self.history_structure.remove(&nb_id);
        history::record_structure(&nb_dir, &mut last, structure);
        if let Some(l) = last {
            self.history_structure.insert(nb_id, l);
        }
        Ok(())
    }

    // ----------------------------------------------------------- notebooks --

    /// All notebooks (live and trashed), most recently changed first.
    pub fn list_notebooks(&self) -> Vec<NotebookSummary> {
        let mut v: Vec<NotebookSummary> = self
            .notebooks
            .values()
            .map(|n| NotebookSummary {
                id: n.id,
                title: n.title.clone(),
                cover: n.cover.clone(),
                page_count: n.page_order.len() as u32,
                created_at: n.created_at,
                updated_at: n.updated_at,
                deleted_at: n.deleted_at,
                first_page_id: n.page_order.first().copied(),
                folder: self.folders.shown_folder(n.folder),
            })
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.title.cmp(&b.title)));
        v
    }

    /// A notebook, live or trashed. `NotFound` if it does not exist.
    pub fn notebook(&self, id: Uuid) -> Result<&Notebook> {
        self.notebooks
            .get(&id)
            .ok_or_else(|| StorageError::NotFound(format!("notebook {id}")))
    }

    fn live_notebook(&self, id: Uuid) -> Result<&Notebook> {
        let nb = self.notebook(id)?;
        if nb.deleted_at.is_some() {
            return Err(StorageError::Conflict(
                "this notebook is in the trash; restore it first".into(),
            ));
        }
        Ok(nb)
    }

    /// Creates a notebook with one blank page (from `defaults`). Without a
    /// `cover`, the next colour of [`COVERS`] is used. Returns the notebook
    /// and its first page; both are durable on return.
    pub fn create_notebook(
        &mut self,
        title: &str,
        defaults: PageDefaults,
        cover: Option<String>,
    ) -> Result<(Notebook, Page)> {
        validate_title(title)?;
        let id = self.new_id();
        let page_id = self.new_id();
        let cover =
            cover.unwrap_or_else(|| COVERS[self.notebooks.len() % COVERS.len()].to_string());
        let paper = defaults.paper;
        let size = paper.size().unwrap_or(Size {
            width: 612.0,
            height: 792.0,
        });
        let template = defaults.template.clone();
        self.commit_notebook_ops(
            id,
            vec![
                DocumentOperation::NotebookCreated {
                    notebook_id: id,
                    title: title.trim().to_string(),
                    cover,
                    defaults,
                },
                DocumentOperation::PageCreated {
                    notebook_id: id,
                    page_id,
                    order_key: order_key::between(None, None),
                    paper,
                    size,
                    template,
                    background: None,
                },
            ],
        )?;
        let nb = self.notebook(id)?.clone();
        let page = self.load_page(id, page_id)?;
        Ok((nb, page))
    }

    /// Creates a notebook that starts out holding imported content: `assets`
    /// first, so pages can refer to them, then one page per entry of `pages`.
    /// Everything goes through the ordinary operations, so it syncs like any
    /// other notebook. On failure the partial notebook is erased, so an
    /// import either arrives whole or leaves no trace.
    pub fn create_notebook_with(
        &mut self,
        title: &str,
        assets: Vec<NewAsset>,
        pages: Vec<NewPage>,
    ) -> Result<Notebook> {
        validate_title(title)?;
        if pages.is_empty() {
            return Err(StorageError::Invalid("nothing to import".into()));
        }
        let id = self.new_id();
        let cover = COVERS[self.notebooks.len() % COVERS.len()].to_string();
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookCreated {
                notebook_id: id,
                title: title.trim().to_string(),
                cover,
                defaults: PageDefaults::default(),
            }],
        )?;
        let res = self.fill_notebook(id, assets, pages);
        if let Err(e) = res {
            if let Err(cleanup) = self
                .delete_notebook(id)
                .and_then(|_| self.purge_notebook(id))
            {
                self.recovery.warnings.push(format!(
                    "a failed import of \"{title}\" could not be cleaned up: {cleanup}"
                ));
            }
            return Err(e);
        }
        Ok(self.notebook(id)?.clone())
    }

    fn fill_notebook(
        &mut self,
        id: Uuid,
        assets: Vec<NewAsset>,
        pages: Vec<NewPage>,
    ) -> Result<()> {
        for a in assets {
            self.add_asset(id, &a.bytes, &a.media_type, a.name)?;
        }
        let specs = pages
            .iter()
            .map(|p| (p.paper, p.size, p.template.clone(), p.background.clone()))
            .collect();
        let (_, created) = self.create_pages(id, None, specs)?;
        for (page, new) in created.into_iter().zip(pages) {
            if new.elements.is_empty() {
                continue;
            }
            let batch = OperationBatch {
                batch_id: self.new_id(),
                base_revision: page.revision,
                ops: vec![PageOperation {
                    op_id: self.new_id(),
                    kind: OpKind::AddElements {
                        elements: new.elements,
                    },
                }],
            };
            self.apply_page_operations(id, page.id, &batch)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------- folders --

    /// The folders as shown: live ones only, each under the parent to show it
    /// in, sorted by name.
    pub fn list_folders(&self) -> Vec<Folder> {
        self.folders.tree()
    }

    /// The raw folder state, deleted folders included. Sync seeds from this,
    /// and backups carry it.
    pub fn folders(&self) -> &FolderSet {
        &self.folders
    }

    /// Fails unless `id` is a live folder.
    fn require_folder(&self, id: Uuid) -> Result<()> {
        if self.folders.is_live(id) {
            Ok(())
        } else {
            Err(StorageError::NotFound(format!("folder {id}")))
        }
    }

    fn save_folders(&mut self) -> Result<()> {
        write_atomic(
            &self.root.join("folders.json"),
            &serde_json::to_vec_pretty(&self.folders).map_err(ioerr)?,
        )?;
        self.dirty_notebooks.remove(&LIBRARY_STREAM_ID);
        Ok(())
    }

    /// Validates, journals and applies one folder operation. Durable when it
    /// returns; a failed write of `folders.json` is retried at the next flush
    /// and meanwhile holds the journal back.
    fn commit_folder_op(&mut self, body: DocumentOperation) -> Result<()> {
        let at = now_ms();
        let id = self.new_id();
        let ops = self.make_ops(vec![(id, body)], at);
        let mut probe = self.folders.clone();
        for op in &ops {
            validate_body(&op.body)?;
            apply_folder_op(&mut probe, op)?;
        }
        let txn = Txn {
            target: TxnTarget::Notebook {
                notebook_id: LIBRARY_STREAM_ID,
            },
            ops,
        };
        self.journal_txn(&txn)?;
        self.folders = probe;
        if let Err(e) = self.save_folders() {
            self.dirty_notebooks
                .entry(LIBRARY_STREAM_ID)
                .or_insert(txn.first_seq());
            self.recovery
                .warnings
                .push(format!("folders snapshot deferred: {e}"));
        }
        Ok(())
    }

    /// Creates a folder, inside `parent` or at the top level.
    pub fn create_folder(&mut self, name: &str, parent: Option<Uuid>) -> Result<Folder> {
        validate_title(name)?;
        if let Some(p) = parent {
            self.require_folder(p)?;
        }
        let folder_id = self.new_id();
        self.commit_folder_op(DocumentOperation::FolderCreated {
            folder_id,
            name: name.trim().to_string(),
            parent,
        })?;
        Ok(self.folders.0[&folder_id].clone())
    }

    /// Renames a folder (the name is validated and trimmed).
    pub fn rename_folder(&mut self, id: Uuid, name: &str) -> Result<()> {
        validate_title(name)?;
        self.require_folder(id)?;
        self.commit_folder_op(DocumentOperation::FolderRenamed {
            folder_id: id,
            name: name.trim().to_string(),
        })
    }

    /// Moves a folder under `parent`, or to the top level. `Invalid` if that
    /// would put it inside itself.
    pub fn move_folder(&mut self, id: Uuid, parent: Option<Uuid>) -> Result<()> {
        self.require_folder(id)?;
        if let Some(p) = parent {
            self.require_folder(p)?;
            if self.folders.would_cycle(id, p) {
                return Err(StorageError::Invalid(
                    "a folder cannot be moved inside itself".into(),
                ));
            }
        }
        self.commit_folder_op(DocumentOperation::FolderMoved {
            folder_id: id,
            parent,
        })
    }

    /// Deletes a folder. Its notebooks and subfolders aren't touched; they
    /// show in its parent from now on.
    pub fn delete_folder(&mut self, id: Uuid) -> Result<()> {
        self.require_folder(id)?;
        self.commit_folder_op(DocumentOperation::FolderDeleted { folder_id: id })
    }

    /// Files a notebook in a folder, or at the top level with `None`. Doesn't
    /// count as changing the notebook, so it keeps its place in the list.
    pub fn file_notebook(&mut self, id: Uuid, folder: Option<Uuid>) -> Result<Notebook> {
        self.notebook(id)?;
        if let Some(f) = folder {
            self.require_folder(f)?;
        }
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookFiled {
                notebook_id: id,
                folder,
            }],
        )?;
        Ok(self.notebook(id)?.clone())
    }

    /// Adds folders from a backup that this library doesn't have. They go
    /// through the journal like any new folder, so sync picks them up. Call
    /// before restoring notebooks, so those find their folders.
    pub fn restore_folders(&mut self, folders: &FolderSet) -> Result<()> {
        for f in folders.0.values() {
            if self.folders.0.contains_key(&f.id) {
                continue;
            }
            self.commit_folder_op(DocumentOperation::FolderCreated {
                folder_id: f.id,
                name: f.name.clone(),
                parent: f.parent,
            })?;
            if f.deleted_at.is_some() {
                self.commit_folder_op(DocumentOperation::FolderDeleted { folder_id: f.id })?;
            }
        }
        Ok(())
    }

    /// Installs the merged folders produced by the sync engine. As with
    /// [`Library::install_merged`], every local folder operation must already
    /// be part of them.
    pub fn install_folders(&mut self, folders: FolderSet) -> Result<()> {
        self.folders = folders;
        self.save_folders()?;
        self.installed.insert(LIBRARY_STREAM_ID, self.last_seq);
        self.write_checkpoint()
    }

    /// Renames a notebook (the title is validated and trimmed).
    pub fn rename_notebook(&mut self, id: Uuid, title: &str) -> Result<Notebook> {
        validate_title(title)?;
        self.notebook(id)?;
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookRenamed {
                notebook_id: id,
                title: title.trim().to_string(),
            }],
        )?;
        Ok(self.notebook(id)?.clone())
    }

    /// Changes the cover colour (stored lowercase).
    pub fn set_notebook_cover(&mut self, id: Uuid, cover: &str) -> Result<Notebook> {
        self.notebook(id)?;
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookCoverChanged {
                notebook_id: id,
                cover: cover.to_lowercase(),
            }],
        )?;
        Ok(self.notebook(id)?.clone())
    }

    /// Shows the notebook's pages as infinite canvases, or as sheets again.
    /// Nothing on the pages changes either way.
    pub fn set_infinite_canvas(&mut self, id: Uuid, infinite: bool) -> Result<Notebook> {
        if self.live_notebook(id)?.infinite_canvas == infinite {
            return Ok(self.notebook(id)?.clone());
        }
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookCanvasSet {
                notebook_id: id,
                infinite,
            }],
        )?;
        Ok(self.notebook(id)?.clone())
    }

    /// Gathers every live page onto the first one, stacked top to bottom in
    /// reading order with [`CANVAS_PAGE_GAP_PT`] between them, then moves the
    /// emptied pages to the notebook's trash (so they can still be restored).
    /// A page's PDF background comes along as a PDF page element below its
    /// content. The first page keeps its template; the others' are dropped.
    ///
    /// Each step is an ordinary journaled operation: the content is added to
    /// the first page before any page is trashed, so a failure part way
    /// through can leave content twice, never lose it. Returns the notebook
    /// and the first page.
    pub fn collapse_pages_into_canvas(&mut self, nb_id: Uuid) -> Result<(Notebook, Page)> {
        let order = self.live_notebook(nb_id)?.page_order.clone();
        let Some((&first_id, rest)) = order.split_first() else {
            return Err(StorageError::Invalid("the notebook has no pages".into()));
        };
        let first = self.load_page(nb_id, first_id)?;
        if rest.is_empty() {
            return Ok((self.notebook(nb_id)?.clone(), first));
        }
        // Gaps are whole rows of the first page's paper, so locked text on the
        // pages below still sits on its rows.
        let pitch = first.template.spacing;
        let snap = |y: f64| (y / pitch).ceil() * pitch;
        let mut y = snap(
            first
                .size
                .height
                .max(first.content_bounds().map_or(0.0, |b| b[3]))
                + CANVAS_PAGE_GAP_PT,
        );
        let mut pdfs = Vec::new();
        let mut added = Vec::new();
        let mut z = first.max_z();
        for &pid in rest {
            let page = self.load_page(nb_id, pid)?;
            // a page that was a canvas already can have content above its top edge
            let (top, bottom) = page.content_bounds().map_or((0.0, page.size.height), |b| {
                (b[1].min(0.0), b[3].max(page.size.height))
            });
            let dy = y - top;
            if let Some(bg) = &page.background {
                pdfs.push(pdf_page_element(self.new_id(), bg.clone(), 0.0, dy));
            }
            let mut elements = page.elements.clone();
            elements.sort_by_key(|e| (e.z(), e.id()));
            for mut e in elements {
                e.translate(0.0, dy);
                z += 1;
                *e.z_mut() = z;
                set_element_id(&mut e, self.new_id());
                added.push(e);
            }
            y = snap(dy + bottom + CANVAS_PAGE_GAP_PT);
        }
        // PDF pages go underneath everything, so ink written over them shows.
        let floor = first.min_z();
        let n = pdfs.len() as i64;
        for (i, e) in pdfs.iter_mut().enumerate() {
            *e.z_mut() = floor - n + i as i64;
        }
        pdfs.extend(added);
        if !pdfs.is_empty() {
            let batch = OperationBatch {
                batch_id: self.new_id(),
                base_revision: first.revision,
                ops: vec![PageOperation {
                    op_id: self.new_id(),
                    kind: OpKind::AddElements { elements: pdfs },
                }],
            };
            self.apply_page_operations(nb_id, first_id, &batch)?;
        }
        for &pid in rest {
            self.flush_page(nb_id, pid)?;
        }
        self.commit_notebook_ops(
            nb_id,
            rest.iter()
                .map(|&page_id| DocumentOperation::PageDeleted {
                    notebook_id: nb_id,
                    page_id,
                })
                .collect(),
        )?;
        Ok((
            self.notebook(nb_id)?.clone(),
            self.load_page(nb_id, first_id)?,
        ))
    }

    /// Moves a notebook to the trash (a tombstone; nothing is erased).
    pub fn delete_notebook(&mut self, id: Uuid) -> Result<Notebook> {
        // checkpoint first so a trashed notebook has no journal-only content
        self.flush_notebook(id)?;
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookDeleted { notebook_id: id }],
        )?;
        Ok(self.notebook(id)?.clone())
    }

    /// Takes a notebook out of the trash.
    pub fn restore_notebook(&mut self, id: Uuid) -> Result<Notebook> {
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookRestored { notebook_id: id }],
        )?;
        Ok(self.notebook(id)?.clone())
    }

    /// Permanently removes a notebook that is already in the trash. With sync
    /// on this reaches every device, and the account can restore it.
    pub fn purge_notebook(&mut self, id: Uuid) -> Result<()> {
        if self.notebook(id)?.deleted_at.is_none() {
            return Err(StorageError::Conflict(
                "only notebooks in the trash can be deleted permanently".into(),
            ));
        }
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookPurged { notebook_id: id }],
        )
    }

    // --------------------------------------------------------------- pages --

    /// Creates a page and inserts it at `index` (default: end).
    pub fn create_page(
        &mut self,
        nb_id: Uuid,
        index: Option<usize>,
        template: Option<Template>,
        paper: Option<Paper>,
    ) -> Result<(Notebook, Page)> {
        let nb = self.live_notebook(nb_id)?;
        let paper = paper.unwrap_or(nb.defaults.paper);
        let template = template.unwrap_or_else(|| nb.defaults.template.clone());
        let size = paper.size().unwrap_or(Size {
            width: 612.0,
            height: 792.0,
        });
        let (nb, mut pages) =
            self.create_pages(nb_id, index, vec![(paper, size, template, None)])?;
        Ok((nb, pages.remove(0)))
    }

    /// Creates pages (e.g. one per PDF page) with explicit sizes/backgrounds.
    pub fn create_pages(
        &mut self,
        nb_id: Uuid,
        index: Option<usize>,
        pages: Vec<(Paper, Size, Template, Option<PdfBackgroundRef>)>,
    ) -> Result<(Notebook, Vec<Page>)> {
        let nb = self.live_notebook(nb_id)?.clone();
        if pages.is_empty() {
            return Ok((nb, Vec::new()));
        }
        for (_, _, t, bg) in &pages {
            penroam_core::validate::validate_template(t)?;
            if let Some(bg) = bg {
                if !nb.assets.iter().any(|a| a.sha256 == bg.asset_hash) {
                    return Err(StorageError::Invalid(format!(
                        "asset {} is not part of this notebook",
                        bg.asset_hash
                    )));
                }
            }
        }
        let at = index
            .unwrap_or(nb.page_order.len())
            .min(nb.page_order.len());
        let keys = keys_for_insert(&nb, at, pages.len());
        let mut ids = Vec::new();
        let bodies = pages
            .into_iter()
            .zip(keys)
            .map(|((paper, size, template, background), order_key)| {
                let page_id = self.new_id();
                ids.push(page_id);
                DocumentOperation::PageCreated {
                    notebook_id: nb_id,
                    page_id,
                    order_key,
                    paper,
                    size,
                    template,
                    background,
                }
            })
            .collect();
        self.commit_notebook_ops(nb_id, bodies)?;
        let mut created = Vec::new();
        for id in ids {
            created.push(self.load_page(nb_id, id)?);
        }
        Ok((self.notebook(nb_id)?.clone(), created))
    }

    /// Reorders the live pages. `order` must contain exactly the current
    /// live pages (else `Conflict`). Only pages that actually move get new
    /// order keys, so concurrent reorders on other devices merge well.
    pub fn update_page_order(&mut self, nb_id: Uuid, order: Vec<Uuid>) -> Result<Notebook> {
        let nb = self.live_notebook(nb_id)?.clone();
        let mut a = nb.page_order.clone();
        let mut b = order.clone();
        a.sort();
        b.sort();
        if a != b {
            return Err(StorageError::Conflict(
                "page order must contain exactly the notebook's pages".into(),
            ));
        }
        let moves = reorder_keys(&nb, &order);
        if moves.is_empty() {
            return Ok(nb);
        }
        self.commit_notebook_ops(
            nb_id,
            moves
                .into_iter()
                .map(|(page_id, order_key)| DocumentOperation::PageReordered {
                    notebook_id: nb_id,
                    page_id,
                    order_key,
                })
                .collect(),
        )?;
        let nb = self.notebook(nb_id)?.clone();
        debug_assert_eq!(nb.page_order, order);
        Ok(nb)
    }

    /// Moves a page to the notebook's trash (recoverable with
    /// [`Library::restore_page`]).
    pub fn delete_page(&mut self, nb_id: Uuid, page: Uuid) -> Result<Notebook> {
        if !self.live_notebook(nb_id)?.page_order.contains(&page) {
            return Err(StorageError::NotFound(format!("page {page}")));
        }
        self.flush_page(nb_id, page)?;
        self.commit_notebook_ops(
            nb_id,
            vec![DocumentOperation::PageDeleted {
                notebook_id: nb_id,
                page_id: page,
            }],
        )?;
        Ok(self.notebook(nb_id)?.clone())
    }

    /// Restores a trashed page to its former position.
    pub fn restore_page(&mut self, nb_id: Uuid, page: Uuid) -> Result<Notebook> {
        if !self
            .live_notebook(nb_id)?
            .trashed_pages
            .iter()
            .any(|t| t.id == page)
        {
            return Err(StorageError::NotFound(format!("trashed page {page}")));
        }
        self.commit_notebook_ops(
            nb_id,
            vec![DocumentOperation::PageRestored {
                notebook_id: nb_id,
                page_id: page,
            }],
        )?;
        Ok(self.notebook(nb_id)?.clone())
    }

    /// A copy of a page (live or trashed), with every journaled batch
    /// applied. Use [`Library::with_page`] to avoid copying large pages.
    pub fn load_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<Page> {
        self.cached_page(nb_id, page_id).map(|c| c.page.clone())
    }

    /// A copy of a page for background readers (search indexing) that leaves
    /// the page cache alone: a page that is not cached is decoded from its
    /// snapshot plus the journaled batches not yet in it, and then dropped.
    /// Reading through [`Library::load_page`] instead would evict, and so
    /// checkpoint, the pages the editor is working on.
    ///
    /// Errors: `NotFound` for an unknown notebook or page, `Corrupt` when the
    /// snapshot is unreadable or the journal cannot be applied cleanly (the
    /// caller may fall back to [`Library::load_page`], which runs the full
    /// recovery path and reports it).
    pub fn read_page_detached(&self, nb_id: Uuid, page_id: Uuid) -> Result<Page> {
        let nb = self.notebook(nb_id)?;
        if !nb.all_page_ids().any(|p| p == page_id) {
            return Err(StorageError::NotFound(format!("page {page_id}")));
        }
        if let Some(c) = self.pages.get(&(nb_id, page_id)) {
            return Ok(c.page.clone());
        }
        let (mut page, _) = self
            .read_snapshot(nb_id, page_id, &self.page_path(nb_id, page_id))
            .map_err(|e| StorageError::Corrupt(format!("page {page_id}: {e}")))?;
        for t in self.pending.get(&(nb_id, page_id)).into_iter().flatten() {
            let TxnTarget::Page { revision, .. } = t.target else {
                continue;
            };
            if revision <= page.revision {
                continue;
            }
            if revision != page.revision + 1 {
                return Err(StorageError::Corrupt(format!(
                    "page {page_id}: journal gap at revision {revision}"
                )));
            }
            let bodies: Vec<&DocumentOperation> = t.ops.iter().map(|o| &o.body).collect();
            apply_to_page(&mut page, &bodies)
                .map_err(|e| StorageError::Corrupt(format!("page {page_id}: {e}")))?;
            page.revision = revision;
            page.updated_at = t.ops.last().map_or(page.updated_at, |o| o.at);
        }
        Ok(page)
    }

    /// Runs `f` with a borrowed page (avoids cloning large pages).
    pub fn with_page<T>(
        &mut self,
        nb_id: Uuid,
        page_id: Uuid,
        f: impl FnOnce(&Page) -> T,
    ) -> Result<T> {
        Ok(f(&self.cached_page(nb_id, page_id)?.page))
    }

    fn cached_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<&mut CachedPage> {
        let nb = self.notebook(nb_id)?;
        if !nb.all_page_ids().any(|p| p == page_id) {
            return Err(StorageError::NotFound(format!("page {page_id}")));
        }
        self.tick += 1;
        let tick = self.tick;
        if !self.pages.contains_key(&(nb_id, page_id)) {
            let loaded = self.read_page(nb_id, page_id)?;
            self.evict_if_needed()?;
            self.pages.insert((nb_id, page_id), loaded);
        }
        let c = self
            .pages
            .get_mut(&(nb_id, page_id))
            .expect("just inserted");
        c.last_used = tick;
        Ok(c)
    }

    fn evict_if_needed(&mut self) -> Result<()> {
        while self.pages.len() >= PAGE_CACHE {
            let key = *self
                .pages
                .iter()
                .min_by_key(|(_, c)| c.last_used)
                .map(|(k, _)| k)
                .expect("non-empty");
            self.checkpoint_page(key.0, key.1)?;
            self.pages.remove(&key);
        }
        Ok(())
    }

    fn read_snapshot(
        &self,
        nb_id: Uuid,
        page_id: Uuid,
        path: &Path,
    ) -> std::result::Result<(Page, u64), String> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && path == self.page_path(nb_id, page_id) =>
            {
                fs::read(self.legacy_page_path(nb_id, page_id)).map_err(|e| e.to_string())?
            }
            Err(e) => return Err(e.to_string()),
        };
        let page = decode_page_bytes(&bytes)?;
        validate_page(&page).map_err(|e| e.0)?;
        if page.id != page_id {
            return Err(format!("file contains page {}", page.id));
        }
        Ok((page, bytes.len() as u64))
    }

    fn read_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<CachedPage> {
        let mut from_prev = false;
        let (mut page, snapshot_bytes) = match self.read_snapshot(
            nb_id,
            page_id,
            &self.page_path(nb_id, page_id),
        ) {
            Ok(v) => v,
            Err(e) => {
                // fall back to the previous snapshot + journal
                match self.read_snapshot(nb_id, page_id, &self.prev_page_path(nb_id, page_id)) {
                    Ok(v) => {
                        from_prev = true;
                        self.recovery.fallback_snapshots += 1;
                        self.recovery.warnings.push(format!(
                                "page {page_id}: snapshot unreadable ({e}); recovered from the previous snapshot and the journal"
                            ));
                        v
                    }
                    Err(_) => return Err(StorageError::Corrupt(format!("page {page_id}: {e}"))),
                }
            }
        };
        let mut txns = self.pending.remove(&(nb_id, page_id)).unwrap_or_default();
        if from_prev {
            // the journal still holds everything after the previous checkpoint
            txns = self
                .journal
                .read_all()?
                .into_iter()
                .filter(|t| {
                    matches!(&t.target, TxnTarget::Page { notebook_id, page_id: p, .. }
                        if *notebook_id == nb_id && *p == page_id)
                })
                .collect();
        }
        let mut first_dirty_seq = from_prev.then_some(self.last_seq.max(1));
        let mut entries = 0;
        for t in txns {
            let TxnTarget::Page { revision, .. } = t.target else {
                continue;
            };
            if revision <= page.revision {
                continue; // already in the snapshot
            }
            if revision != page.revision + 1 {
                self.recovery.warnings.push(format!(
                    "page {page_id}: journal gap at revision {revision}; later entries were kept in the journal but not applied"
                ));
                self.hold(t.first_seq(), format!("page {page_id}"));
                break;
            }
            let bodies: Vec<&DocumentOperation> = t.ops.iter().map(|o| &o.body).collect();
            match apply_to_page(&mut page, &bodies) {
                Ok(()) => {
                    page.revision = revision;
                    page.updated_at = t.ops.last().map_or(page.updated_at, |o| o.at);
                    entries += 1;
                    self.recovery.replayed_batches += 1;
                    first_dirty_seq.get_or_insert(t.first_seq());
                    let f = first_dirty_seq.as_mut().unwrap();
                    *f = (*f).min(t.first_seq());
                }
                Err(e) => {
                    self.recovery.warnings.push(format!(
                        "page {page_id}: journal entry {} could not be replayed: {e}",
                        t.first_seq()
                    ));
                    self.hold(t.first_seq(), format!("page {page_id}"));
                    break;
                }
            }
        }
        Ok(CachedPage {
            page,
            journal_entries: entries,
            journal_bytes: 0,
            snapshot_bytes,
            last_used: 0,
            first_dirty_seq,
        })
    }

    /// Applies an operation batch and returns once it is durable on disk.
    /// Retrying a batch that was already applied (same `batch_id`) returns
    /// the original outcome without applying it again.
    pub fn apply_page_operations(
        &mut self,
        nb_id: Uuid,
        page_id: Uuid,
        batch: &OperationBatch,
    ) -> Result<ApplyOutcome> {
        if let Some((n, p, out)) = self.recent_batches.get(&batch.batch_id) {
            if *n == nb_id && *p == page_id {
                return Ok(out.clone());
            }
        }
        self.live_notebook(nb_id)?;
        // asset references must resolve inside this notebook
        let assets: HashSet<String> = self
            .notebook(nb_id)?
            .assets
            .iter()
            .map(|a| a.sha256.clone())
            .collect();
        for op in &batch.ops {
            if let OpKind::AddElements { elements } | OpKind::ReplaceElements { elements } =
                &op.kind
            {
                for e in elements {
                    if let Element::Image(i) = e {
                        if !assets.contains(&i.asset_hash) {
                            return Err(StorageError::Invalid(format!(
                                "image asset {} is not in this notebook",
                                i.asset_hash
                            )));
                        }
                    }
                }
            }
        }
        let now = now_ms();
        let c = self.cached_page(nb_id, page_id)?;
        if let Some(reason) = c.page.read_only_reason() {
            return Err(StorageError::Conflict(reason));
        }
        // validate first, then journal (fsync), then apply in place: no page copy
        penroam_core::ops::check_batch(&c.page, batch)?;
        let revision = c.page.revision + 1;
        let bodies = bodies_from_batch(nb_id, &c.page, batch);
        self.history_before_edit(nb_id, page_id);
        let ops = self.make_ops(bodies, now);
        let txn = Txn {
            target: TxnTarget::Page {
                notebook_id: nb_id,
                page_id,
                revision,
                batch_id: batch.batch_id,
            },
            ops,
        };
        let bytes = self.journal_txn(&txn)?;
        self.history_after_edit(nb_id, page_id, &txn);
        let c = self.pages.get_mut(&(nb_id, page_id)).expect("cached");
        penroam_core::ops::apply_checked(&mut c.page, batch, Some(revision), now);
        c.journal_entries += 1;
        c.journal_bytes += bytes;
        c.first_dirty_seq.get_or_insert(txn.first_seq());
        let needs_checkpoint = c.journal_entries >= self.tuning.max_entries
            || c.journal_bytes > self.tuning.min_compact_bytes.max(c.snapshot_bytes / 4);
        if let Some(nb) = self.notebooks.get_mut(&nb_id) {
            nb.updated_at = now;
        }
        let outcome = ApplyOutcome {
            revision,
            updated_at: now,
            durable: true,
        };
        self.remember_batch(batch.batch_id, nb_id, page_id, outcome.clone());
        if needs_checkpoint {
            // best effort: the batch is already durable in the journal
            if let Err(e) = self.checkpoint_page(nb_id, page_id) {
                self.recovery
                    .warnings
                    .push(format!("checkpoint of page {page_id} failed: {e}"));
            }
        }
        Ok(outcome)
    }

    // ------------------------------------------------------------- history --

    fn history_meta(&mut self, nb_id: Uuid, page_id: Uuid) -> &mut ChainMeta {
        let path = history::chain_path(&self.nb_dir(nb_id), page_id);
        self.history_chains
            .entry((nb_id, page_id))
            .or_insert_with(|| history::scan_meta(&path))
    }

    /// Drops cached history bookkeeping after the files changed underneath it.
    pub(crate) fn forget_history(&mut self, nb_id: Uuid) {
        self.history_chains.retain(|(n, _), _| *n != nb_id);
        self.history_structure.remove(&nb_id);
    }

    /// Records a page arriving from sync as a version of its own. A page new
    /// to this device gets no chain until it is edited, so a first download
    /// doesn't store everything twice.
    fn history_install(&mut self, nb_id: Uuid, old: Option<&Page>, new: &Page) {
        let Some(old) = old else { return };
        let Some(mut txn) = history::install_txn(nb_id, old, new) else {
            return;
        };
        // a delta follows the old revision, even if journal batches were folded in
        if let TxnTarget::Page { revision, .. } = &mut txn.target {
            *revision = old.revision + 1;
        }
        let path = history::chain_path(&self.nb_dir(nb_id), new.id);
        let meta = self.history_meta(nb_id, new.id);
        let res = if meta.last_revision == Some(old.revision) {
            Ok(())
        } else {
            let bytes = penroam_core::binary::encode_page(old);
            history::append_keyframe(&path, meta, old.revision, &bytes)
        }
        .and_then(|()| history::append_delta(&path, meta, &txn));
        if let Err(e) = res {
            self.history_failed(nb_id, e);
        }
    }

    /// Reports a history failure once. History is a convenience: it never
    /// fails an edit.
    fn history_failed(&mut self, nb_id: Uuid, e: std::io::Error) {
        let w = format!("version history of notebook {nb_id} is not being updated: {e}");
        if !self.recovery.warnings.contains(&w) {
            self.recovery.warnings.push(w);
        }
    }

    /// Starts a new keyframe from the page as it stands if its chain doesn't
    /// end at this revision. Must run before the edit is applied.
    fn history_before_edit(&mut self, nb_id: Uuid, page_id: Uuid) {
        let Some(c) = self.pages.get(&(nb_id, page_id)) else {
            return;
        };
        let revision = c.page.revision;
        if self.history_meta(nb_id, page_id).last_revision == Some(revision) {
            return;
        }
        let c = &self.pages[&(nb_id, page_id)];
        let bytes = penroam_core::binary::encode_page(&c.page);
        let path = history::chain_path(&self.nb_dir(nb_id), page_id);
        let meta = self.history_meta(nb_id, page_id);
        if let Err(e) = history::append_keyframe(&path, meta, revision, &bytes) {
            self.history_failed(nb_id, e);
        }
    }

    fn history_after_edit(&mut self, nb_id: Uuid, page_id: Uuid, txn: &Txn) {
        let path = history::chain_path(&self.nb_dir(nb_id), page_id);
        let meta = self.history_meta(nb_id, page_id);
        if let Err(e) = history::append_delta(&path, meta, txn) {
            self.history_failed(nb_id, e);
        }
    }

    /// Adds a keyframe to a chain whose deltas have grown enough, reusing
    /// the bytes of a snapshot that was just written.
    fn history_maybe_keyframe(&mut self, nb_id: Uuid, page_id: Uuid, revision: u64, bytes: &[u8]) {
        let meta = *self.history_meta(nb_id, page_id);
        if meta.last_revision != Some(revision)
            || meta.delta_bytes < history::keyframe_threshold(meta.keyframe_bytes)
        {
            return;
        }
        let path = history::chain_path(&self.nb_dir(nb_id), page_id);
        let meta = self.history_meta(nb_id, page_id);
        if let Err(e) = history::append_keyframe(&path, meta, revision, bytes) {
            self.history_failed(nb_id, e);
        }
    }

    fn remember_batch(&mut self, batch: Uuid, nb: Uuid, page: Uuid, out: ApplyOutcome) {
        if self.recent_batches.insert(batch, (nb, page, out)).is_none() {
            self.recent_order.push_back(batch);
            if self.recent_order.len() > RECENT_BATCHES {
                if let Some(old) = self.recent_order.pop_front() {
                    self.recent_batches.remove(&old);
                }
            }
        }
    }

    // --------------------------------------------------------- checkpoints --

    /// Writes the page snapshot (keeping the previous one as fallback).
    fn snapshot_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<()> {
        let Some(c) = self.pages.get(&(nb_id, page_id)) else {
            return Ok(());
        };
        if c.first_dirty_seq.is_none() {
            return Ok(());
        }
        let bytes = penroam_core::binary::encode_page(&c.page);
        let path = self.page_path(nb_id, page_id);
        if path.exists() {
            fsutil::rename_durable(&path, &self.prev_page_path(nb_id, page_id))?;
        }
        write_atomic(&path, &bytes)?;
        // read back: the bytes on disk must be the bytes we meant
        if fs::read(&path).ok().map(|b| crc32fast::hash(&b)) != Some(crc32fast::hash(&bytes)) {
            return Err(StorageError::Io(format!(
                "page {page_id} snapshot did not read back correctly"
            )));
        }
        let legacy = self.legacy_page_path(nb_id, page_id);
        if legacy.exists() {
            fsutil::remove_durable(&legacy)?;
        }
        let c = self.pages.get_mut(&(nb_id, page_id)).expect("present");
        c.journal_entries = 0;
        c.journal_bytes = 0;
        c.snapshot_bytes = bytes.len() as u64;
        c.first_dirty_seq = None;
        let revision = c.page.revision;
        self.history_maybe_keyframe(nb_id, page_id, revision, &bytes);
        Ok(())
    }

    /// Snapshot + notebook + manifest for one page.
    fn checkpoint_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<()> {
        let dirty = self
            .pages
            .get(&(nb_id, page_id))
            .is_some_and(|c| c.first_dirty_seq.is_some());
        if !dirty {
            return Ok(());
        }
        self.snapshot_page(nb_id, page_id)?;
        if self.notebooks.contains_key(&nb_id) {
            // persists the notebook's updatedAt lazily, together with the page
            self.save_notebook_snapshot(nb_id)?;
        }
        self.write_checkpoint()
    }

    /// Everything `<= floor` is in a snapshot.
    fn floor(&self) -> u64 {
        let mut f = self.last_seq;
        let mut lower = |s: u64| f = f.min(s.saturating_sub(1));
        for c in self.pages.values() {
            if let Some(s) = c.first_dirty_seq {
                lower(s);
            }
        }
        for txns in self.pending.values() {
            if let Some(t) = txns.first() {
                lower(t.first_seq());
            }
        }
        for s in self.dirty_notebooks.values() {
            lower(*s);
        }
        if let Some(s) = self.held_seq {
            lower(s);
        }
        f
    }

    /// Persists the checkpoint manifest and releases journal segments that
    /// both this and the previous checkpoint cover.
    fn write_checkpoint(&mut self) -> Result<()> {
        let floor = self.floor();
        let new = CheckpointManifest {
            last_seq: self.last_seq,
            floor,
            prev_floor: self.manifest.floor,
            installed: self.installed.clone(),
            ..self.manifest.clone()
        };
        if new == self.manifest {
            return Ok(());
        }
        // the manifest must be durable before any segment it covers is
        // deleted: otherwise a crash could leave a manifest that points at
        // replay data that no longer exists
        write_manifest(&self.root, &self.manifest, &new)?;
        self.manifest = new;
        // keep what the previous checkpoint still needs: if a new snapshot
        // turns out damaged, its `.prev` snapshot plus this journal rebuild it
        let mut release = self.manifest.prev_floor.min(self.manifest.floor);
        if self.outbox.sync_enabled {
            release = release.min(self.outbox.uploaded_through);
        }
        self.journal.delete_through(release)?;
        if self.journal.active_is_covered_by(floor) {
            // start a fresh segment so the covered one can be deleted next time
            self.journal.seal();
        }
        Ok(())
    }

    /// Checkpoints one page now (snapshot + notebook + manifest). Not
    /// needed for durability; it shortens replay and lets the journal shrink.
    pub fn flush_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<()> {
        self.checkpoint_page(nb_id, page_id)
    }

    fn flush_notebook(&mut self, nb_id: Uuid) -> Result<()> {
        let keys: Vec<_> = self
            .pages
            .keys()
            .filter(|(n, _)| *n == nb_id)
            .copied()
            .collect();
        for (n, p) in keys {
            self.checkpoint_page(n, p)?;
        }
        let pending: Vec<_> = self
            .pending
            .keys()
            .filter(|(n, _)| *n == nb_id)
            .copied()
            .collect();
        for (n, p) in pending {
            self.cached_page(n, p)?;
            self.checkpoint_page(n, p)?;
        }
        Ok(())
    }

    /// Checkpoints everything (app close / background): afterwards every
    /// journaled operation is in a snapshot.
    pub fn flush_all(&mut self) -> Result<()> {
        let mut first_err = None;
        let pending: Vec<_> = self.pending.keys().copied().collect();
        for (n, p) in pending {
            if let Err(e) = self.cached_page(n, p).map(|_| ()) {
                first_err.get_or_insert(e);
            }
        }
        let keys: Vec<_> = self.pages.keys().copied().collect();
        for (n, p) in keys {
            if let Err(e) = self.snapshot_page(n, p) {
                first_err.get_or_insert(e);
            }
        }
        let dirty: Vec<Uuid> = self
            .dirty_notebooks
            .keys()
            .chain(self.pages.keys().map(|(n, _)| n))
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        for nb in dirty {
            if self.notebooks.contains_key(&nb) {
                if let Err(e) = self.save_notebook_snapshot(nb) {
                    first_err.get_or_insert(e);
                }
            }
        }
        if self.dirty_notebooks.contains_key(&LIBRARY_STREAM_ID) {
            if let Err(e) = self.save_folders() {
                first_err.get_or_insert(e);
            }
        }
        if let Err(e) = self.write_checkpoint() {
            first_err.get_or_insert(e);
        }
        if let Err(e) = write_json(&self.root.join("outbox.json"), &self.outbox) {
            first_err.get_or_insert(e.into());
        }
        first_err.map_or(Ok(()), Err)
    }

    // -------------------------------------------------------------- outbox --

    /// Outbox counters (see [`OutboxStatus`]).
    pub fn outbox_status(&self) -> OutboxStatus {
        OutboxStatus {
            device_id: self.device_id,
            enqueued_through: self.outbox.enqueued_through,
            uploaded_through: self.outbox.uploaded_through,
            pending: self
                .outbox
                .enqueued_through
                .saturating_sub(self.outbox.uploaded_through),
            sync_enabled: self.outbox.sync_enabled,
        }
    }

    /// Journaled operations after `seq`, in order. Fails with `Conflict`
    /// when some of them were already released by a checkpoint (the caller
    /// must then start from a snapshot).
    pub fn operations_after(&self, seq: u64) -> Result<Vec<LocalOperation>> {
        if self
            .journal
            .min_retained_seq()
            .is_none_or(|first| first > seq + 1)
            && self.last_seq > seq
        {
            return Err(StorageError::Conflict(format!(
                "operations after {seq} were already checkpointed; a snapshot is required"
            )));
        }
        Ok(self
            .journal
            .read_all()?
            .into_iter()
            .flat_map(|t| t.ops)
            .filter(|o| o.local_sequence > seq)
            .collect())
    }

    /// Records a server acknowledgement (used by the sync engine). Persisted atomically before
    /// returning; monotone.
    pub fn mark_uploaded(&mut self, through: u64) -> Result<()> {
        if through > self.last_seq {
            return Err(StorageError::Invalid(
                "cannot acknowledge operations that do not exist".into(),
            ));
        }
        self.outbox.uploaded_through = self.outbox.uploaded_through.max(through);
        write_json(&self.root.join("outbox.json"), &self.outbox)?;
        Ok(())
    }

    /// Enables outbox retention: journal segments are then kept until
    /// uploaded as well as checkpointed.
    pub fn set_sync_enabled(&mut self, on: bool) -> Result<()> {
        self.outbox.sync_enabled = on;
        write_json(&self.root.join("outbox.json"), &self.outbox)?;
        Ok(())
    }

    // ---------------------------------------------------------------- sync --

    /// Installs a merged notebook state produced by the sync engine (the
    /// notebook and every page whose content changed). Nothing is journaled:
    /// the sync operation log is the durability source for remote changes,
    /// and the engine re-installs after a crash.
    ///
    /// Contract: every local operation journaled for this notebook must
    /// already be part of the merged state (the engine ingests local
    /// operations before merging). Snapshots are written atomically; page
    /// revisions move past any journaled local revision so that journal
    /// replay never re-applies an edit the snapshot already contains.
    pub fn install_merged(&mut self, nb: Notebook, pages: Vec<Page>) -> Result<()> {
        let nb_id = nb.id;
        if self.purged.contains(&nb_id) {
            // sync lifts a purge first, when the account has the notebook back
            return Ok(());
        }
        validate_notebook(&nb)?;
        for p in &pages {
            validate_page(p)?;
            if !nb.all_page_ids().any(|id| id == p.id) {
                return Err(StorageError::Invalid(format!(
                    "page {} is not part of notebook {nb_id}",
                    p.id
                )));
            }
        }
        if !self.notebooks.contains_key(&nb_id) {
            fs::create_dir_all(self.nb_dir(nb_id).join("pages"))?;
            fs::create_dir_all(self.nb_dir(nb_id).join("assets"))?;
        }
        self.notebooks.insert(nb_id, nb);
        for mut page in pages {
            let key = (nb_id, page.id);
            let mut base = self.pending.get(&key).map_or(0, |txns| {
                txns.iter()
                    .filter_map(|t| match t.target {
                        TxnTarget::Page { revision, .. } => Some(revision),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0)
            });
            let old: Option<Page> = if let Some(c) = self.pages.get(&key) {
                Some(c.page.clone())
            } else {
                self.read_snapshot(nb_id, page.id, &self.page_path(nb_id, page.id))
                    .ok()
                    .map(|(p, _)| p)
            };
            if let Some(old) = &old {
                base = base.max(old.revision);
            }
            page.revision = base + 1;
            self.history_install(nb_id, old.as_ref(), &page);
            self.pending.remove(&key);
            if !self.pages.contains_key(&key) {
                self.evict_if_needed()?;
            }
            self.tick += 1;
            self.pages.insert(
                key,
                CachedPage {
                    page,
                    journal_entries: 0,
                    journal_bytes: 0,
                    snapshot_bytes: 0,
                    last_used: self.tick,
                    first_dirty_seq: Some(self.last_seq.max(1)),
                },
            );
            self.snapshot_page(nb_id, key.1)?;
        }
        self.save_notebook_snapshot(nb_id)?;
        self.dirty_notebooks.remove(&nb_id);
        self.installed.insert(nb_id, self.last_seq);
        self.write_checkpoint()
    }

    /// Stores asset bytes received through sync (verified by hash) without
    /// creating an operation; the merged notebook lists the asset.
    pub fn install_asset(&mut self, nb_id: Uuid, bytes: &[u8]) -> Result<String> {
        let hash = sha256_hex(bytes);
        let path = self.asset_path(nb_id, &hash);
        if !path.exists() {
            fs::create_dir_all(self.nb_dir(nb_id).join("assets"))?;
            write_atomic(&path, bytes)?;
        }
        Ok(hash)
    }

    /// Whether the bytes of an asset are present locally.
    pub fn has_asset(&self, nb_id: Uuid, hash: &str) -> bool {
        self.asset_path(nb_id, hash).exists()
    }

    /// Ids of every notebook in the library (live and trashed).
    pub fn notebook_ids(&self) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = self.notebooks.keys().copied().collect();
        v.sort();
        v
    }

    // -------------------------------------------------------------- assets --

    /// Stores an immutable attachment (e.g. a PDF) and references it from
    /// the notebook. Assets are addressed by SHA-256, so adding the same
    /// bytes twice returns the existing entry.
    pub fn add_asset(
        &mut self,
        nb_id: Uuid,
        bytes: &[u8],
        media_type: &str,
        name: Option<String>,
    ) -> Result<Asset> {
        let nb = self.live_notebook(nb_id)?;
        let hash = sha256_hex(bytes);
        if let Some(a) = nb.assets.iter().find(|a| a.sha256 == hash) {
            return Ok(a.clone());
        }
        let path = self.asset_path(nb_id, &hash);
        // bytes first, reference second: a crash in between leaves an
        // unreferenced file (moved to orphans/ on open), never a dangling reference
        if !path.exists() {
            write_atomic(&path, bytes)?;
        }
        let asset = Asset {
            sha256: hash,
            media_type: media_type.to_string(),
            byte_size: bytes.len() as u64,
            original_name: name,
        };
        self.commit_notebook_ops(
            nb_id,
            vec![DocumentOperation::AssetReferenced {
                notebook_id: nb_id,
                asset: asset.clone(),
            }],
        )?;
        Ok(asset)
    }

    /// Reads an asset of a notebook, verifying its checksum (`Corrupt` on mismatch).
    pub fn read_asset(&self, nb_id: Uuid, hash: &str) -> Result<Vec<u8>> {
        let nb = self.notebook(nb_id)?;
        if !nb.assets.iter().any(|a| a.sha256 == hash) {
            return Err(StorageError::NotFound(format!("asset {hash}")));
        }
        let bytes = fs::read(self.asset_path(nb_id, hash))?;
        if sha256_hex(&bytes) != hash {
            return Err(StorageError::Corrupt(format!(
                "asset {hash} does not match its checksum"
            )));
        }
        Ok(bytes)
    }

    // ---------------------------------------------------------- thumbnails --

    /// Caches a page thumbnail (PNG under 4 MiB). Thumbnails are derived
    /// data: never journaled and never exported.
    pub fn save_thumbnail(&self, nb_id: Uuid, page_id: Uuid, png: &[u8]) -> Result<()> {
        self.notebook(nb_id)?;
        if png.len() > 4 << 20 || !png.starts_with(b"\x89PNG") {
            return Err(StorageError::Invalid(
                "thumbnail must be a PNG under 4 MiB".into(),
            ));
        }
        write_atomic(&self.thumb_path(nb_id, page_id), png)?;
        Ok(())
    }

    /// The cached thumbnail of a page, if any.
    pub fn thumbnail(&self, nb_id: Uuid, page_id: Uuid) -> Option<Vec<u8>> {
        fs::read(self.thumb_path(nb_id, page_id)).ok()
    }

    // ------------------------------------------------------------- archive --

    pub(crate) fn insert_imported(&mut self, nb: Notebook, staged_dir: &Path) -> Result<Notebook> {
        let dest = self.nb_dir(nb.id);
        if dest.exists() {
            return Err(StorageError::Conflict("notebook already exists".into()));
        }
        fs::rename(staged_dir, &dest)?;
        fsutil::sync_dir(&self.root.join("notebooks"))?;
        self.notebooks.insert(nb.id, nb.clone());
        Ok(nb)
    }

    /// Notebooks erased on this device while their journal entries are still
    /// retained, sorted. Sync installs nothing for these.
    pub fn purged_notebooks(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self.purged.iter().copied().collect();
        ids.sort();
        ids
    }

    /// Whether `id` is one of [`Library::purged_notebooks`].
    pub fn is_purged(&self, id: Uuid) -> bool {
        self.purged.contains(&id)
    }

    /// Erases a notebook that was permanently deleted on another device. It may
    /// not be in the trash here yet, and sync won't send this purge back out.
    pub fn erase_from_sync(&mut self, id: Uuid) -> Result<()> {
        self.commit_notebook_ops(
            id,
            vec![DocumentOperation::NotebookPurged { notebook_id: id }],
        )
    }

    /// Lets sync install a notebook that was erased on this device. Lasts until
    /// the install, which is what makes it durable: if the app stops first, the
    /// journal restores the purge and nothing has changed.
    pub fn forget_purge(&mut self, id: Uuid) -> bool {
        self.purged.remove(&id)
    }

    // -------------------------------------------------------- detach/attach --

    /// Takes notebooks out of the open library without deleting anything, so their folders can
    /// be moved away (a vault being locked). Afterwards:
    ///
    /// * every journaled change of theirs is in their snapshots, and the journal no longer holds
    ///   any of their transactions ([`Journal::drop_notebooks`]), so nothing of them is left
    ///   outside `notebooks/<id>/`;
    /// * the library forgets them: they aren't listed, and nothing writes to their folders.
    ///
    /// Anything that reads the journal for these notebooks (a sync engine) must have copied it
    /// first: what was dropped can't be read again. Ids that aren't open are ignored. On error
    /// the notebooks stay open; the journal may already be without some of their transactions,
    /// which is harmless because their snapshots cover them by then.
    ///
    /// [`Journal::drop_notebooks`]: crate::journal::Journal::drop_notebooks
    pub fn detach_notebooks(&mut self, ids: &HashSet<Uuid>) -> Result<()> {
        let ids: HashSet<Uuid> = ids
            .iter()
            .copied()
            .filter(|id| self.notebooks.contains_key(id) || self.purged.contains(id))
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        // the snapshots first: once a transaction is dropped, the snapshot is all there is
        for id in &ids {
            if self.notebooks.contains_key(id) {
                self.flush_notebook(*id)?;
                self.save_notebook_snapshot(*id)?;
            }
        }
        // the manifest must record the highest sequence before transactions are dropped, or a
        // reopened library could hand out sequences that sync engines have already seen
        self.write_checkpoint()?;
        if self.manifest.last_seq != self.last_seq {
            return Err(StorageError::Io(
                "the checkpoint manifest could not be brought up to date".into(),
            ));
        }
        self.journal.drop_notebooks(&ids)?;
        self.notebooks.retain(|id, _| !ids.contains(id));
        self.pages.retain(|(nb, _), _| !ids.contains(nb));
        self.pending.retain(|(nb, _), _| !ids.contains(nb));
        self.dirty_notebooks.retain(|nb, _| !ids.contains(nb));
        self.history_chains.retain(|(nb, _), _| !ids.contains(nb));
        self.history_structure.retain(|nb, _| !ids.contains(nb));
        self.recent_batches
            .retain(|_, (nb, _, _)| !ids.contains(nb));
        self.purged.retain(|nb| !ids.contains(nb));
        self.installed.retain(|nb, _| !ids.contains(nb));
        self.write_checkpoint()
    }

    /// Opens notebook folders that were put back under `notebooks/` (a vault being unlocked),
    /// as [`Library::open`] would. Returns the ids that opened; one that can't be read is left
    /// on disk and reported in [`Library::recovery`]. Ids already open are skipped.
    ///
    /// The folders must come from [`Library::detach_notebooks`] or equivalent: the journal holds
    /// nothing for them, so their snapshots are taken as complete.
    pub fn attach_notebooks(&mut self, ids: &[Uuid]) -> Vec<Uuid> {
        let mut out = Vec::new();
        for id in ids {
            if self.notebooks.contains_key(id) {
                continue;
            }
            let path = self.nb_dir(*id);
            if path.join("notebook.json").exists() && self.load_notebook_dir(&path, *id) {
                out.push(*id);
            }
        }
        out
    }

    /// Whether an id is taken (including notebooks purged while their
    /// journal entries are still retained).
    pub(crate) fn contains_notebook(&self, id: Uuid) -> bool {
        self.notebooks.contains_key(&id) || self.purged.contains(&id)
    }

    // --------------------------------------------------------------- tests --

    /// Test/benchmark hook: drop in-memory state without flushing (simulates a crash).
    #[doc(hidden)]
    pub fn crash_for_tests(self) {
        // the OS releases the lock of a dead process
        let _ = self._lock.unlock();
        std::mem::forget(self);
    }

    /// Test hook: checkpoint thresholds.
    #[doc(hidden)]
    pub fn set_tuning(&mut self, t: Tuning) {
        self.tuning = t;
    }

    /// Test hook: deterministic ids for new notebooks/pages/operations.
    #[doc(hidden)]
    pub fn set_deterministic_ids(&mut self, start: u128) {
        self.id_counter = Some(start);
    }

    /// Test hook: journal segment files currently retained.
    #[doc(hidden)]
    pub fn journal_segments(&self) -> usize {
        self.journal.segment_count()
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // the lock file handle closes with the struct; nothing to flush here
        // (callers flush explicitly; acknowledged edits are already durable)
    }
}

/// Keys for `n` new pages inserted at live index `at`.
fn keys_for_insert(nb: &Notebook, at: usize, n: usize) -> Vec<String> {
    let before = at.checked_sub(1).and_then(|i| nb.key_of(nb.page_order[i]));
    let after = nb.page_order.get(at).and_then(|p| nb.key_of(*p));
    match (before, after) {
        (Some(a), Some(b)) if a >= b => order_key::n_between(Some(a), None, n),
        (a, b) => order_key::n_between(a, b, n),
    }
}

/// New keys for the pages that must move so that sorting by key yields
/// `order`. Keeps the longest run of pages whose keys are already
/// increasing in the new order (fewest operations).
fn reorder_keys(nb: &Notebook, order: &[Uuid]) -> Vec<(Uuid, String)> {
    let keys: Vec<&str> = order.iter().map(|p| nb.key_of(*p).unwrap_or("")).collect();
    // longest subsequence with strictly increasing keys, O(n log n). Equal
    // keys (from concurrent inserts on two devices) cannot keep their place:
    // nothing fits strictly between them, so those pages get new keys.
    let cmp_key = |i: usize| keys[i];
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; order.len()];
    for i in 0..order.len() {
        if keys[i].is_empty() {
            continue;
        }
        let pos = tails.partition_point(|&t| cmp_key(t) < cmp_key(i));
        prev[i] = pos.checked_sub(1).map(|p| tails[p]);
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut keep = vec![false; order.len()];
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        keep[i] = true;
        cur = prev[i];
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < order.len() {
        if keep[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < order.len() && !keep[i] {
            i += 1;
        }
        let before = start.checked_sub(1).map(|j| keys[j]);
        let before = before.map(|b| {
            // the previous page may itself have been re-keyed in this pass
            out.iter()
                .rev()
                .find(|(p, _): &&(Uuid, String)| *p == order[start - 1])
                .map_or(b.to_string(), |(_, k)| k.clone())
        });
        let after = order.get(i).map(|_| keys[i].to_string());
        let new = match (&before, &after) {
            (Some(a), Some(b)) if a >= b => order_key::n_between(Some(a), None, i - start),
            (a, b) => order_key::n_between(a.as_deref(), b.as_deref(), i - start),
        };
        out.extend(order[start..i].iter().copied().zip(new));
    }
    out
}

/// Decodes a page snapshot in either encoding: binary `.prp` or legacy JSON.
pub fn decode_page_bytes(bytes: &[u8]) -> std::result::Result<Page, String> {
    if penroam_core::binary::is_binary_page(bytes) {
        penroam_core::binary::decode_page(bytes).map_err(|e| e.to_string())
    } else {
        page_from_slice(bytes).map_err(|e| e.to_string())
    }
}

/// Lowercase hex SHA-256 (the asset address format).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
