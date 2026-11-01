//! The live notebook library in the app's private data directory.
//!
//! ```text
//! <root>/library.json                       format marker {"format":"penroam-library","version":1}
//! <root>/notebooks/<nb>/notebook.json       canonical notebook (atomic writes)
//! <root>/notebooks/<nb>/pages/<page>.json   canonical page snapshot (atomic writes)
//! <root>/notebooks/<nb>/pages/<page>.journal  fsynced operation batches since the snapshot (JSON lines)
//! <root>/notebooks/<nb>/assets/<sha256>     immutable attachments
//! <root>/notebooks/<nb>/thumbs/<page>.png   derived thumbnail cache (never exported)
//! <root>/notebooks/<nb>/orphans/            files found unreferenced at start-up (kept, not deleted)
//! <root>/staging/                           temporary import/export work area
//! ```
//!
//! There is no database: the canonical files *are* the index, so nothing can
//! get out of sync. Listing reads each small `notebook.json`.
//!
//! Durability: an operation batch is acknowledged only after its journal line
//! is fsynced. Journals are folded into the page snapshot (atomic rename) when
//! they grow, on page eviction and on [`Library::flush_all`]. A torn final
//! journal line (crash mid-append) is truncated on load; entries at or below
//! the snapshot revision (crash between snapshot and truncate) are skipped.

use crate::error::{Result, StorageError};
use crate::fsutil::{append_durable, write_atomic, TMP_MARKER};
use penroam_core::migrate::{notebook_from_slice, page_from_slice};
use penroam_core::validate::{validate_notebook, validate_page, validate_title};
use penroam_core::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use ts_rs::TS;
use uuid::Uuid;

const JOURNAL_MAX_ENTRIES: usize = 512;
const JOURNAL_MIN_COMPACT_BYTES: u64 = 1 << 20;
const PAGE_CACHE: usize = 8;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NotebookSummary {
    pub id: Uuid,
    pub title: String,
    pub cover: String,
    pub page_count: u32,
    #[ts(type = "number")]
    pub created_at: Timestamp,
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    #[ts(optional, type = "number")]
    pub deleted_at: Option<Timestamp>,
    #[ts(optional)]
    pub first_page_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RecoveryReport {
    pub removed_temp_files: u32,
    pub orphaned_files: u32,
    pub truncated_journals: u32,
    pub replayed_batches: u32,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ApplyOutcome {
    #[ts(type = "number")]
    pub revision: u64,
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    /// The batch is on disk (journal fsynced).
    pub durable: bool,
}

#[derive(Serialize, Deserialize)]
struct JournalEntry {
    revision: u64,
    at: Timestamp,
    batch: OperationBatch,
}

struct CachedPage {
    page: Page,
    journal_entries: usize,
    journal_bytes: u64,
    snapshot_bytes: u64,
    last_used: u64,
}

pub struct Library {
    root: PathBuf,
    notebooks: HashMap<Uuid, Notebook>,
    pages: HashMap<(Uuid, Uuid), CachedPage>,
    tick: u64,
    pub recovery: RecoveryReport,
}

pub const COVERS: &[&str] = &["#3b5b7a", "#7a3b3b", "#3b7a52", "#6b4f8a", "#8a6f3b", "#3b6f7a", "#545454"];

impl Library {
    /// Opens (or creates) the library and runs start-up recovery.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("notebooks"))?;
        let marker = root.join("library.json");
        if !marker.exists() {
            write_atomic(&marker, br#"{"format":"penroam-library","version":1}"#)?;
        }
        let staging = root.join("staging");
        if staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        fs::create_dir_all(&staging)?;
        let mut lib = Library { root, notebooks: HashMap::new(), pages: HashMap::new(), tick: 0, recovery: RecoveryReport::default() };
        lib.recover()?;
        Ok(lib)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    fn nb_dir(&self, nb: Uuid) -> PathBuf {
        self.root.join("notebooks").join(nb.to_string())
    }
    fn page_path(&self, nb: Uuid, page: Uuid) -> PathBuf {
        self.nb_dir(nb).join("pages").join(format!("{page}.json"))
    }
    fn journal_path(&self, nb: Uuid, page: Uuid) -> PathBuf {
        self.nb_dir(nb).join("pages").join(format!("{page}.journal"))
    }
    pub(crate) fn asset_path(&self, nb: Uuid, hash: &str) -> PathBuf {
        self.nb_dir(nb).join("assets").join(hash)
    }
    fn thumb_path(&self, nb: Uuid, page: Uuid) -> PathBuf {
        self.nb_dir(nb).join("thumbs").join(format!("{page}.png"))
    }

    // ------------------------------------------------------------ recovery --

    fn recover(&mut self) -> Result<()> {
        let dir = self.root.join("notebooks");
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            let Some(id) = path.file_name().and_then(|n| n.to_str()).and_then(|n| Uuid::parse_str(n).ok()) else {
                continue;
            };
            self.remove_temp_files(&path);
            match fs::read(path.join("notebook.json")).map_err(StorageError::from).and_then(|b| {
                let nb = notebook_from_slice(&b).map_err(|e| StorageError::Corrupt(e.to_string()))?;
                validate_notebook(&nb)?;
                Ok(nb)
            }) {
                Ok(nb) if nb.id == id => {
                    self.collect_orphans(&nb);
                    self.notebooks.insert(id, nb);
                }
                Ok(_) => self.recovery.warnings.push(format!("notebook {id}: id does not match its folder; skipped")),
                Err(e) => self.recovery.warnings.push(format!("notebook {id} could not be opened and was left untouched: {e}")),
            }
        }
        Ok(())
    }

    fn remove_temp_files(&mut self, dir: &Path) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                self.remove_temp_files(&p);
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.contains(TMP_MARKER)) && fs::remove_file(&p).is_ok() {
                self.recovery.removed_temp_files += 1;
            }
        }
    }

    /// Moves page files and assets that the notebook does not reference into
    /// `orphans/` (e.g. a crash between writing a new page and listing it).
    fn collect_orphans(&mut self, nb: &Notebook) {
        let dir = self.nb_dir(nb.id);
        let known: std::collections::HashSet<String> = nb.all_page_ids().map(|p| p.to_string()).collect();
        let assets: std::collections::HashSet<&str> = nb.assets.iter().map(|a| a.sha256.as_str()).collect();
        let orphans = dir.join("orphans");
        let mut moved = 0;
        if let Ok(rd) = fs::read_dir(dir.join("pages")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let stem = name.split('.').next().unwrap_or("");
                if !known.contains(stem) {
                    let _ = fs::create_dir_all(&orphans);
                    if fs::rename(e.path(), orphans.join(&name)).is_ok() {
                        moved += 1;
                    }
                }
            }
        }
        if let Ok(rd) = fs::read_dir(dir.join("assets")) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !assets.contains(name.as_str()) {
                    let _ = fs::create_dir_all(&orphans);
                    if fs::rename(e.path(), orphans.join(&name)).is_ok() {
                        moved += 1;
                    }
                }
            }
        }
        if moved > 0 {
            self.recovery.orphaned_files += moved;
            self.recovery.warnings.push(format!("notebook {}: {moved} unreferenced file(s) moved to orphans/", nb.id));
        }
    }

    // ----------------------------------------------------------- notebooks --

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
            })
            .collect();
        v.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.title.cmp(&b.title)));
        v
    }

    pub fn notebook(&self, id: Uuid) -> Result<&Notebook> {
        self.notebooks.get(&id).ok_or_else(|| StorageError::NotFound(format!("notebook {id}")))
    }

    fn live_notebook(&self, id: Uuid) -> Result<&Notebook> {
        let nb = self.notebook(id)?;
        if nb.deleted_at.is_some() {
            return Err(StorageError::Conflict("this notebook is in the trash; restore it first".into()));
        }
        Ok(nb)
    }

    fn save_notebook(&mut self, nb: Notebook) -> Result<Notebook> {
        validate_notebook(&nb)?;
        let path = self.nb_dir(nb.id).join("notebook.json");
        write_atomic(&path, &serde_json::to_vec_pretty(&nb).map_err(|e| StorageError::Io(e.to_string()))?)?;
        self.notebooks.insert(nb.id, nb.clone());
        Ok(nb)
    }

    pub fn create_notebook(&mut self, title: &str, defaults: PageDefaults, cover: Option<String>) -> Result<(Notebook, Page)> {
        validate_title(title)?;
        let now = now_ms();
        let id = Uuid::new_v4();
        let cover = cover.unwrap_or_else(|| COVERS[self.notebooks.len() % COVERS.len()].to_string());
        let mut nb = Notebook::new(id, title.trim().to_string(), defaults, cover, now);
        validate_notebook(&nb)?;
        fs::create_dir_all(self.nb_dir(id).join("pages"))?;
        fs::create_dir_all(self.nb_dir(id).join("assets"))?;
        let page = self.new_page_for(&nb, None, None, now)?;
        nb.page_order.push(page.id);
        let nb = self.save_notebook(nb)?;
        Ok((nb, page))
    }

    pub fn rename_notebook(&mut self, id: Uuid, title: &str) -> Result<Notebook> {
        validate_title(title)?;
        let mut nb = self.notebook(id)?.clone();
        nb.title = title.trim().to_string();
        nb.updated_at = now_ms();
        self.save_notebook(nb)
    }

    pub fn set_notebook_cover(&mut self, id: Uuid, cover: &str) -> Result<Notebook> {
        let mut nb = self.notebook(id)?.clone();
        nb.cover = cover.to_lowercase();
        self.save_notebook(nb)
    }

    pub fn delete_notebook(&mut self, id: Uuid) -> Result<Notebook> {
        self.flush_notebook(id)?;
        let mut nb = self.notebook(id)?.clone();
        nb.deleted_at = Some(now_ms());
        self.save_notebook(nb)
    }

    pub fn restore_notebook(&mut self, id: Uuid) -> Result<Notebook> {
        let mut nb = self.notebook(id)?.clone();
        nb.deleted_at = None;
        nb.updated_at = now_ms();
        self.save_notebook(nb)
    }

    /// Permanently removes a notebook that is already in the trash.
    pub fn purge_notebook(&mut self, id: Uuid) -> Result<()> {
        if self.notebook(id)?.deleted_at.is_none() {
            return Err(StorageError::Conflict("only notebooks in the trash can be deleted permanently".into()));
        }
        self.pages.retain(|(n, _), _| *n != id);
        self.notebooks.remove(&id);
        fs::remove_dir_all(self.nb_dir(id))?;
        Ok(())
    }

    // --------------------------------------------------------------- pages --

    fn new_page_for(&mut self, nb: &Notebook, template: Option<Template>, paper: Option<Paper>, now: Timestamp) -> Result<Page> {
        let paper = paper.unwrap_or(nb.defaults.paper);
        let size = paper.size().unwrap_or(Size { width: 612.0, height: 792.0 });
        let page = Page::new(Uuid::new_v4(), paper, size, template.unwrap_or_else(|| nb.defaults.template.clone()), now);
        self.write_page_snapshot(nb.id, &page)?;
        Ok(page)
    }

    /// Creates a page and inserts it at `index` (default: end).
    pub fn create_page(&mut self, nb_id: Uuid, index: Option<usize>, template: Option<Template>, paper: Option<Paper>) -> Result<(Notebook, Page)> {
        let nb = self.live_notebook(nb_id)?.clone();
        if let Some(t) = &template {
            penroam_core::validate::validate_template(t)?;
        }
        let now = now_ms();
        let page = self.new_page_for(&nb, template, paper, now)?;
        let mut nb = nb;
        let at = index.unwrap_or(nb.page_order.len()).min(nb.page_order.len());
        nb.page_order.insert(at, page.id);
        nb.updated_at = now;
        let nb = self.save_notebook(nb)?;
        Ok((nb, page))
    }

    /// Creates pages (e.g. one per PDF page) with explicit sizes/backgrounds.
    pub fn create_pages(&mut self, nb_id: Uuid, index: Option<usize>, pages: Vec<(Paper, Size, Template, Option<PdfBackgroundRef>)>) -> Result<(Notebook, Vec<Page>)> {
        let mut nb = self.live_notebook(nb_id)?.clone();
        let now = now_ms();
        let mut created = Vec::new();
        for (paper, size, template, bg) in pages {
            let mut p = Page::new(Uuid::new_v4(), paper, size, template, now);
            if let Some(bg) = &bg {
                if !nb.assets.iter().any(|a| a.sha256 == bg.asset_hash) {
                    return Err(StorageError::Invalid(format!("asset {} is not part of this notebook", bg.asset_hash)));
                }
            }
            p.background = bg;
            validate_page(&p)?;
            self.write_page_snapshot(nb_id, &p)?;
            created.push(p);
        }
        let at = index.unwrap_or(nb.page_order.len()).min(nb.page_order.len());
        for (i, p) in created.iter().enumerate() {
            nb.page_order.insert(at + i, p.id);
        }
        nb.updated_at = now;
        let nb = self.save_notebook(nb)?;
        Ok((nb, created))
    }

    pub fn update_page_order(&mut self, nb_id: Uuid, order: Vec<Uuid>) -> Result<Notebook> {
        let mut nb = self.live_notebook(nb_id)?.clone();
        let mut a = nb.page_order.clone();
        let mut b = order.clone();
        a.sort();
        b.sort();
        if a != b {
            return Err(StorageError::Conflict("page order must contain exactly the notebook's pages".into()));
        }
        nb.page_order = order;
        nb.updated_at = now_ms();
        self.save_notebook(nb)
    }

    pub fn delete_page(&mut self, nb_id: Uuid, page: Uuid) -> Result<Notebook> {
        let mut nb = self.live_notebook(nb_id)?.clone();
        let idx = nb.page_order.iter().position(|p| *p == page).ok_or_else(|| StorageError::NotFound(format!("page {page}")))?;
        self.flush_page(nb_id, page)?;
        nb.page_order.remove(idx);
        nb.trashed_pages.push(TrashedPage { id: page, deleted_at: now_ms(), former_index: idx as u32 });
        nb.updated_at = now_ms();
        self.save_notebook(nb)
    }

    pub fn restore_page(&mut self, nb_id: Uuid, page: Uuid) -> Result<Notebook> {
        let mut nb = self.live_notebook(nb_id)?.clone();
        let idx = nb.trashed_pages.iter().position(|p| p.id == page).ok_or_else(|| StorageError::NotFound(format!("trashed page {page}")))?;
        let t = nb.trashed_pages.remove(idx);
        let at = (t.former_index as usize).min(nb.page_order.len());
        nb.page_order.insert(at, page);
        nb.updated_at = now_ms();
        self.save_notebook(nb)
    }

    pub fn load_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<Page> {
        self.cached_page(nb_id, page_id).map(|c| c.page.clone())
    }

    /// Runs `f` with a borrowed page (avoids cloning large pages).
    pub fn with_page<T>(&mut self, nb_id: Uuid, page_id: Uuid, f: impl FnOnce(&Page) -> T) -> Result<T> {
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
        let c = self.pages.get_mut(&(nb_id, page_id)).expect("just inserted");
        c.last_used = tick;
        Ok(c)
    }

    fn evict_if_needed(&mut self) -> Result<()> {
        while self.pages.len() >= PAGE_CACHE {
            let key = *self.pages.iter().min_by_key(|(_, c)| c.last_used).map(|(k, _)| k).expect("non-empty");
            self.compact(key.0, key.1)?;
            self.pages.remove(&key);
        }
        Ok(())
    }

    fn read_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<CachedPage> {
        let snap = fs::read(self.page_path(nb_id, page_id))?;
        let mut page = page_from_slice(&snap).map_err(|e| StorageError::Corrupt(format!("page {page_id}: {e}")))?;
        validate_page(&page).map_err(|e| StorageError::Corrupt(format!("page {page_id}: {}", e.0)))?;
        if page.id != page_id {
            return Err(StorageError::Corrupt(format!("page file {page_id} contains page {}", page.id)));
        }
        let jpath = self.journal_path(nb_id, page_id);
        let mut entries = 0;
        let mut good_len: u64 = 0;
        if let Ok(bytes) = fs::read(&jpath) {
            let mut offset = 0usize;
            for line in bytes.split_inclusive(|b| *b == b'\n') {
                let complete = line.ends_with(b"\n");
                let parsed: Option<JournalEntry> = if complete { serde_json::from_slice(line).ok() } else { None };
                let Some(entry) = parsed else {
                    // torn or damaged tail: everything after the last good line is dropped
                    self.recovery.truncated_journals += 1;
                    self.recovery.warnings.push(format!("page {page_id}: discarded an incomplete journal record (crash during write)"));
                    break;
                };
                offset += line.len();
                if entry.revision <= page.revision {
                    good_len = offset as u64;
                    continue; // already folded into the snapshot
                }
                if entry.revision != page.revision + 1 {
                    self.recovery.warnings.push(format!("page {page_id}: journal gap at revision {}", entry.revision));
                    break;
                }
                if let Err(e) = penroam_core::ops::replay_batch(&mut page, &entry.batch, entry.revision, entry.at) {
                    self.recovery.warnings.push(format!("page {page_id}: journal record {} could not be replayed: {e}", entry.revision));
                    break;
                }
                entries += 1;
                self.recovery.replayed_batches += 1;
                good_len = offset as u64;
            }
            if good_len < bytes.len() as u64 {
                let f = fs::OpenOptions::new().write(true).open(&jpath)?;
                f.set_len(good_len)?;
                f.sync_all()?;
            }
        }
        Ok(CachedPage { page, journal_entries: entries, journal_bytes: good_len, snapshot_bytes: snap.len() as u64, last_used: 0 })
    }

    fn write_page_snapshot(&self, nb_id: Uuid, page: &Page) -> Result<u64> {
        let bytes = serde_json::to_vec(page).map_err(|e| StorageError::Io(e.to_string()))?;
        write_atomic(&self.page_path(nb_id, page.id), &bytes)?;
        Ok(bytes.len() as u64)
    }

    /// Applies an operation batch and returns once it is durable on disk.
    pub fn apply_page_operations(&mut self, nb_id: Uuid, page_id: Uuid, batch: &OperationBatch) -> Result<ApplyOutcome> {
        self.live_notebook(nb_id)?;
        // asset references must resolve inside this notebook
        let assets: Vec<String> = self.notebook(nb_id)?.assets.iter().map(|a| a.sha256.clone()).collect();
        for op in &batch.ops {
            if let OpKind::AddElements { elements } | OpKind::ReplaceElements { elements } = &op.kind {
                for e in elements {
                    if let Element::Image(i) = e {
                        if !assets.contains(&i.asset_hash) {
                            return Err(StorageError::Invalid(format!("image asset {} is not in this notebook", i.asset_hash)));
                        }
                    }
                }
            }
        }
        let jpath = self.journal_path(nb_id, page_id);
        let now = now_ms();
        let c = self.cached_page(nb_id, page_id)?;
        let mut next = c.page.clone();
        let revision = apply_batch(&mut next, batch, now)?;
        let mut line = serde_json::to_vec(&JournalEntry { revision, at: now, batch: batch.clone() }).map_err(|e| StorageError::Io(e.to_string()))?;
        line.push(b'\n');
        append_durable(&jpath, &line)?;
        c.page = next;
        c.journal_entries += 1;
        c.journal_bytes += line.len() as u64;
        let needs_compact = c.journal_entries >= JOURNAL_MAX_ENTRIES || c.journal_bytes > JOURNAL_MIN_COMPACT_BYTES.max(c.snapshot_bytes / 4);
        if let Some(nb) = self.notebooks.get_mut(&nb_id) {
            nb.updated_at = now;
        }
        if needs_compact {
            // best effort: the batch is already durable in the journal
            if let Err(e) = self.compact(nb_id, page_id) {
                self.recovery.warnings.push(format!("compaction of page {page_id} failed: {e}"));
            }
        }
        Ok(ApplyOutcome { revision, updated_at: now, durable: true })
    }

    /// Folds the journal into the snapshot.
    fn compact(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<()> {
        let Some(c) = self.pages.get(&(nb_id, page_id)) else { return Ok(()) };
        if c.journal_entries == 0 && c.journal_bytes == 0 {
            return Ok(());
        }
        let size = self.write_page_snapshot(nb_id, &c.page)?;
        let jpath = self.journal_path(nb_id, page_id);
        let f = fs::OpenOptions::new().create(true).write(true).truncate(true).open(&jpath)?;
        f.sync_all()?;
        let c = self.pages.get_mut(&(nb_id, page_id)).expect("present");
        c.journal_entries = 0;
        c.journal_bytes = 0;
        c.snapshot_bytes = size;
        // persist the notebook's updatedAt lazily, together with compaction
        if let Some(nb) = self.notebooks.get(&nb_id).cloned() {
            self.save_notebook(nb)?;
        }
        Ok(())
    }

    pub fn flush_page(&mut self, nb_id: Uuid, page_id: Uuid) -> Result<()> {
        self.compact(nb_id, page_id)
    }

    fn flush_notebook(&mut self, nb_id: Uuid) -> Result<()> {
        let keys: Vec<_> = self.pages.keys().filter(|(n, _)| *n == nb_id).copied().collect();
        for (n, p) in keys {
            self.compact(n, p)?;
        }
        Ok(())
    }

    /// Compacts every journal (app close / background).
    pub fn flush_all(&mut self) -> Result<()> {
        let keys: Vec<_> = self.pages.keys().copied().collect();
        let mut first_err = None;
        for (n, p) in keys {
            if let Err(e) = self.compact(n, p) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    // -------------------------------------------------------------- assets --

    pub fn add_asset(&mut self, nb_id: Uuid, bytes: &[u8], media_type: &str, name: Option<String>) -> Result<Asset> {
        let mut nb = self.live_notebook(nb_id)?.clone();
        let hash = sha256_hex(bytes);
        if let Some(a) = nb.assets.iter().find(|a| a.sha256 == hash) {
            return Ok(a.clone());
        }
        let path = self.asset_path(nb_id, &hash);
        if !path.exists() {
            write_atomic(&path, bytes)?;
        }
        let asset = Asset { sha256: hash, media_type: media_type.to_string(), byte_size: bytes.len() as u64, original_name: name };
        nb.assets.push(asset.clone());
        self.save_notebook(nb)?;
        Ok(asset)
    }

    pub fn read_asset(&self, nb_id: Uuid, hash: &str) -> Result<Vec<u8>> {
        let nb = self.notebook(nb_id)?;
        if !nb.assets.iter().any(|a| a.sha256 == hash) {
            return Err(StorageError::NotFound(format!("asset {hash}")));
        }
        let bytes = fs::read(self.asset_path(nb_id, hash))?;
        if sha256_hex(&bytes) != hash {
            return Err(StorageError::Corrupt(format!("asset {hash} does not match its checksum")));
        }
        Ok(bytes)
    }

    // ---------------------------------------------------------- thumbnails --

    pub fn save_thumbnail(&self, nb_id: Uuid, page_id: Uuid, png: &[u8]) -> Result<()> {
        self.notebook(nb_id)?;
        if png.len() > 4 << 20 || !png.starts_with(b"\x89PNG") {
            return Err(StorageError::Invalid("thumbnail must be a PNG under 4 MiB".into()));
        }
        write_atomic(&self.thumb_path(nb_id, page_id), png)?;
        Ok(())
    }

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
        crate::fsutil::sync_dir(&self.root.join("notebooks"))?;
        self.notebooks.insert(nb.id, nb.clone());
        Ok(nb)
    }

    pub(crate) fn contains_notebook(&self, id: Uuid) -> bool {
        self.notebooks.contains_key(&id)
    }

    /// Test/benchmark hook: drop in-memory state without flushing (simulates a crash).
    #[doc(hidden)]
    pub fn crash_for_tests(self) {
        std::mem::forget(self);
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
