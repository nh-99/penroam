//! Notebook version history: every retained state of a page, stored as the
//! semantic operations that produced it with only sparse full snapshots.
//!
//! ```text
//! <library>/notebooks/<nb>/history/<page>.phl    one page's chain (below)
//! <library>/notebooks/<nb>/history/notebook.jsonl  title and page order over time
//!
//! chain    = entry*
//! entry    = delta | keyframe
//! delta    = a journal record (journal::encode_txn) for one page revision
//! keyframe = "PRHK" u32 len u32 crc32(payload) payload   (binary page encoding)
//! ```
//!
//! Format notes: `docs/formats/portability.md`.
//!
//! **Size comes first.** History is the part of a library that grows without
//! bound, so restore speed is traded for bytes: a keyframe is written only when
//! a page's deltas outgrow the larger of 8 MiB and four times the last keyframe,
//! and reading an old version may replay a long run of operations.
//!
//! **Invariants.**
//! * History is never needed to open or recover a library. Appends are not
//!   fsynced and a failure never fails an edit. The `.prp.prev` fallback and the
//!   journal stay independent of it, so a damaged chain costs old versions and
//!   nothing else.
//! * A chain is contiguous from a keyframe. If a revision is missing (a crash
//!   before an append, an install with no earlier page) the next write starts
//!   again from a keyframe. Damage is skipped by resynchronising on the next
//!   entry; versions between the damage and the next keyframe are unavailable.
//! * Restoring is a new forward batch through
//!   [`Library::apply_page_operations`]. It never rewrites anything, so it syncs
//!   like any edit and can itself be undone.
//! * These chains stay on this device and are not in archives or backups. With
//!   sync on, the versions a device didn't record come from the account's
//!   operation log, which `penroam_sync` replays on request, so nothing is
//!   stored twice.
//! * Erased content stays in history until retention drops it or
//!   [`Library::clear_history`] is called. Purging a notebook removes its
//!   history with the rest of its folder.

use crate::error::{Result, StorageError};
use crate::fsutil::write_atomic;
use crate::journal::{decode_txn, encode_txn, Txn, TxnTarget, RECORD_MAGIC};
use crate::library::{decode_page_bytes, ApplyOutcome, Library};
use penroam_core::operation::{apply_to_page, DocumentOperation};
use penroam_core::ops::{page_diff, OperationBatch, PageOperation};
use penroam_core::{Page, Timestamp};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use ts_rs::TS;
use uuid::Uuid;

const KEYFRAME_MAGIC: &[u8; 4] = b"PRHK";
const ENTRY_HEADER: usize = 12;
/// Offset of a delta's revision: target tag, notebook id, page id.
const DELTA_REVISION_AT: usize = 1 + 16 + 16;
const DAY_MS: i64 = 86_400_000;
/// Deltas never trigger a keyframe below this much data.
const MIN_KEYFRAME_INTERVAL: u64 = 8 << 20;

/// Bytes of deltas after which a page of `keyframe_bytes` gets a new keyframe.
pub(crate) fn keyframe_threshold(keyframe_bytes: u64) -> u64 {
    MIN_KEYFRAME_INTERVAL.max(keyframe_bytes.saturating_mul(4))
}

/// How long old versions are kept. Younger periods keep more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryRetention {
    /// Every version for this many days.
    pub keep_all_days: u32,
    /// Then the last version of each day, up to this age.
    pub daily_days: u32,
    /// Then the last version of each week, up to this age. Older versions go.
    pub weekly_days: u32,
}

impl Default for HistoryRetention {
    fn default() -> Self {
        Self {
            keep_all_days: 7,
            daily_days: 30,
            weekly_days: 365,
        }
    }
}

/// One restorable version of a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryPoint {
    /// Page this version belongs to.
    pub page_id: Uuid,
    /// Page revision on this device. Revisions don't match across devices.
    #[ts(type = "number")]
    pub revision: u64,
    /// When the version was made (ms since the epoch).
    #[ts(type = "number")]
    pub at: Timestamp,
    /// Elements added by this version.
    pub added: u32,
    /// Elements removed by this version.
    pub removed: u32,
}

/// Size of a notebook's history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct HistoryStats {
    /// Bytes on disk.
    #[ts(type = "number")]
    pub bytes: u64,
    /// Pages with history.
    pub pages: u32,
}

/// What [`Library::restore_notebook_version`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct NotebookRestore {
    /// Pages whose content went back to the chosen time.
    pub pages_reverted: u32,
    /// Pages taken out of the trash.
    pub pages_recovered: u32,
    /// Pages that didn't exist then, moved to the trash.
    pub pages_trashed: u32,
    /// Pages that existed then but can't be brought back, because they were
    /// erased or their history is gone.
    pub pages_unavailable: u32,
}

/// One line of `notebook.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Structure {
    pub(crate) at: Timestamp,
    pub(crate) title: String,
    pub(crate) page_order: Vec<Uuid>,
}

/// What the writer needs to know about a chain without reading it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ChainMeta {
    pub(crate) last_revision: Option<u64>,
    pub(crate) delta_bytes: u64,
    pub(crate) keyframe_bytes: u64,
}

pub(crate) fn dir(nb_dir: &Path) -> PathBuf {
    nb_dir.join("history")
}

pub(crate) fn chain_path(nb_dir: &Path, page: Uuid) -> PathBuf {
    dir(nb_dir).join(format!("{page}.phl"))
}

fn structure_path(nb_dir: &Path) -> PathBuf {
    dir(nb_dir).join("notebook.jsonl")
}

fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let open = || OpenOptions::new().create(true).append(true).open(path);
    let mut f = match open() {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            open()?
        }
        other => other?,
    };
    f.write_all(bytes)
}

fn keyframe_entry(page_bytes: &[u8]) -> Vec<u8> {
    let mut e = Vec::with_capacity(page_bytes.len() + ENTRY_HEADER);
    e.extend_from_slice(KEYFRAME_MAGIC);
    e.extend_from_slice(&(page_bytes.len() as u32).to_le_bytes());
    e.extend_from_slice(&crc32fast::hash(page_bytes).to_le_bytes());
    e.extend_from_slice(page_bytes);
    e
}

/// Appends a keyframe holding `page_bytes` of revision `revision`.
pub(crate) fn append_keyframe(
    path: &Path,
    meta: &mut ChainMeta,
    revision: u64,
    page_bytes: &[u8],
) -> std::io::Result<()> {
    append(path, &keyframe_entry(page_bytes))?;
    meta.last_revision = Some(revision);
    meta.delta_bytes = 0;
    meta.keyframe_bytes = page_bytes.len() as u64;
    Ok(())
}

/// Appends the delta for one page revision.
pub(crate) fn append_delta(path: &Path, meta: &mut ChainMeta, txn: &Txn) -> std::io::Result<()> {
    let e = encode_txn(txn);
    append(path, &e)?;
    if let TxnTarget::Page { revision, .. } = txn.target {
        meta.last_revision = Some(revision);
    }
    meta.delta_bytes += e.len() as u64;
    Ok(())
}

/// Reads a chain's bookkeeping from its entry headers, without decoding.
pub(crate) fn scan_meta(path: &Path) -> ChainMeta {
    let mut meta = ChainMeta::default();
    let Ok(file) = File::open(path) else {
        return meta;
    };
    let total = file.metadata().map_or(0, |m| m.len());
    let mut r = BufReader::new(file);
    let mut head = [0u8; ENTRY_HEADER + DELTA_REVISION_AT + 8];
    let mut start = 0u64;
    while start < total {
        let want = head.len().min((total - start) as usize);
        if want < ENTRY_HEADER || r.read_exact(&mut head[..want]).is_err() {
            meta.last_revision = None;
            return meta;
        }
        let len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as u64;
        let keyframe = &head[..4] == KEYFRAME_MAGIC;
        let next = start + ENTRY_HEADER as u64 + len;
        if (!keyframe && &head[..4] != RECORD_MAGIC) || next > total {
            // Damage or a torn append. Anything written after this must start
            // from a keyframe, or it couldn't be reached.
            meta.last_revision = None;
            return meta;
        }
        if keyframe {
            meta.keyframe_bytes = len;
            meta.delta_bytes = 0;
            meta.last_revision = keyframe_revision(path, start);
        } else if want == head.len() {
            let at = ENTRY_HEADER + DELTA_REVISION_AT;
            meta.last_revision = Some(u64::from_le_bytes(head[at..at + 8].try_into().unwrap()));
            meta.delta_bytes += next - start;
        }
        if r.seek_relative(next as i64 - (start + want as u64) as i64)
            .is_err()
        {
            meta.last_revision = None;
            return meta;
        }
        start = next;
    }
    meta
}

fn keyframe_revision(path: &Path, at: u64) -> Option<u64> {
    let mut f = File::open(path).ok()?;
    f.seek(SeekFrom::Start(at)).ok()?;
    let mut head = [0u8; ENTRY_HEADER];
    f.read_exact(&mut head).ok()?;
    let len = u32::from_le_bytes(head[4..8].try_into().ok()?) as usize;
    let mut body = vec![0; len];
    f.read_exact(&mut body).ok()?;
    decode_page_bytes(&body).ok().map(|p| p.revision)
}

enum Entry {
    Delta {
        revision: u64,
        at: Timestamp,
        txn: Txn,
    },
    Keyframe(Box<Page>),
    /// Unreadable bytes. Deltas after this need a new keyframe.
    Gap,
}

/// Parses a chain, skipping damage.
fn read_entries(path: &Path) -> Result<Vec<Entry>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    let mut i = 0;
    let mut damaged = false;
    while i + ENTRY_HEADER <= bytes.len() {
        let magic = &bytes[i..i + 4];
        let keyframe = magic == KEYFRAME_MAGIC;
        let entry = (keyframe || magic == RECORD_MAGIC)
            .then(|| u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize)
            .filter(|len| i + ENTRY_HEADER + len <= bytes.len())
            .and_then(|len| {
                let raw = &bytes[i..i + ENTRY_HEADER + len];
                let parsed = if keyframe {
                    let crc = u32::from_le_bytes(raw[8..12].try_into().unwrap());
                    (crc32fast::hash(&raw[ENTRY_HEADER..]) == crc)
                        .then(|| decode_page_bytes(&raw[ENTRY_HEADER..]).ok())
                        .flatten()
                        .map(|p| Entry::Keyframe(Box::new(p)))
                } else {
                    decode_txn(raw).ok().and_then(|txn| match txn.target {
                        TxnTarget::Page { revision, .. } => Some(Entry::Delta {
                            revision,
                            at: txn.ops.last().map_or(0, |o| o.at),
                            txn,
                        }),
                        _ => None,
                    })
                };
                parsed.map(|e| (e, raw.len()))
            });
        match entry {
            Some((e, n)) => {
                if damaged {
                    out.push(Entry::Gap);
                    damaged = false;
                }
                out.push(e);
                i += n;
            }
            None => {
                damaged = true;
                i += 1;
            }
        }
    }
    if damaged || i < bytes.len() {
        out.push(Entry::Gap);
    }
    Ok(out)
}

fn apply_delta(page: &mut Page, revision: u64, at: Timestamp, txn: &Txn) -> bool {
    if revision != page.revision + 1 {
        return false;
    }
    let bodies: Vec<&DocumentOperation> = txn.ops.iter().map(|o| &o.body).collect();
    if apply_to_page(page, &bodies).is_err() {
        return false;
    }
    page.revision = revision;
    page.updated_at = at;
    true
}

/// Walks a chain, calling `visit` with every reachable version in order.
/// `visit` returns false to stop.
fn walk(entries: &[Entry], mut visit: impl FnMut(&Page, Option<&Txn>) -> bool) {
    let mut cur: Option<Page> = None;
    for e in entries {
        match e {
            Entry::Keyframe(p) => {
                // A keyframe's revision may repeat the delta before it.
                let repeat = cur.as_ref().is_some_and(|c| c.revision == p.revision);
                if !repeat && !visit(p, None) {
                    return;
                }
                cur = Some((**p).clone());
            }
            Entry::Gap => cur = None,
            Entry::Delta { revision, at, txn } => {
                let Some(page) = cur.as_mut() else { continue };
                if *revision <= page.revision {
                    continue;
                }
                if !apply_delta(page, *revision, *at, txn) {
                    cur = None;
                    continue;
                }
                if !visit(page, Some(txn)) {
                    return;
                }
            }
        }
    }
}

fn point(page: &Page, txn: Option<&Txn>) -> HistoryPoint {
    let (mut added, mut removed) = (0, 0);
    for op in txn.iter().flat_map(|t| &t.ops) {
        match &op.body {
            DocumentOperation::ElementsAdded { elements, .. } => added += elements.len() as u32,
            DocumentOperation::ElementsRemoved { ids, .. } => removed += ids.len() as u32,
            _ => {}
        }
    }
    HistoryPoint {
        page_id: page.id,
        revision: page.revision,
        at: page.updated_at,
        added,
        removed,
    }
}

/// Which of the versions (with timestamps in order) a policy keeps. The newest
/// is always kept.
fn keep_set(times: &[Timestamp], now: Timestamp, r: &HistoryRetention) -> Vec<bool> {
    let age_days = |t: Timestamp| (now - t).max(0) / DAY_MS;
    let mut keep = vec![false; times.len()];
    let mut seen_bucket = HashMap::new();
    for (i, t) in times.iter().enumerate().rev() {
        let age = age_days(*t);
        keep[i] = if age < r.keep_all_days as i64 {
            true
        } else if age < r.daily_days as i64 {
            seen_bucket.insert((0, t / DAY_MS), ()).is_none()
        } else if age < r.weekly_days as i64 {
            seen_bucket.insert((1, t / (7 * DAY_MS)), ()).is_none()
        } else {
            false
        };
    }
    if let Some(last) = keep.last_mut() {
        *last = true;
    }
    keep
}

/// Which of the `times` (oldest first) a policy keeps at time `now`, for versions
/// held somewhere retention can't rewrite: a viewer shows the same thinning
/// as local chains get.
pub fn retained(times: &[Timestamp], now: Timestamp, retention: &HistoryRetention) -> Vec<bool> {
    keep_set(times, now, retention)
}

fn compact_chain(path: &Path, now: Timestamp, r: &HistoryRetention) -> Result<u64> {
    let before = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(_) => return Ok(0),
    };
    let entries = read_entries(path)?;
    let mut times = Vec::new();
    walk(&entries, |p, _| {
        times.push(p.updated_at);
        true
    });
    if times.is_empty() {
        return Ok(0);
    }
    let keep = keep_set(&times, now, r);
    if keep.iter().all(|k| *k) {
        return Ok(0);
    }
    // Rebuild: the oldest kept version becomes the keyframe, and each later
    // kept version is its diff from the one before. Content that was added
    // and erased entirely between two kept versions disappears here.
    let mut out = Vec::new();
    let mut prev: Option<Page> = None;
    let mut since_keyframe = 0u64;
    let mut keyframe_bytes = 0u64;
    let mut i = 0;
    walk(&entries, |p, txn| {
        let kept = keep[i];
        i += 1;
        if !kept {
            return true;
        }
        match &prev {
            None => {
                let b = penroam_core::binary::encode_page(p);
                keyframe_bytes = b.len() as u64;
                out.extend_from_slice(&keyframe_entry(&b));
            }
            Some(q) => {
                let exact = txn.filter(|_| q.revision + 1 == p.revision).cloned();
                let delta = exact.unwrap_or_else(|| synthetic_txn(Uuid::nil(), q, p));
                let e = encode_txn(&delta);
                since_keyframe += e.len() as u64;
                out.extend_from_slice(&e);
                if since_keyframe > keyframe_threshold(keyframe_bytes) {
                    let b = penroam_core::binary::encode_page(p);
                    keyframe_bytes = b.len() as u64;
                    since_keyframe = 0;
                    out.extend_from_slice(&keyframe_entry(&b));
                }
            }
        }
        prev = Some(p.clone());
        true
    });
    write_atomic(path, &out)?;
    Ok(before.saturating_sub(out.len() as u64))
}

/// A delta that turns `from` into `to`, for versions no recorded batch connects.
fn synthetic_txn(nb: Uuid, from: &Page, to: &Page) -> Txn {
    let batch = batch_between(from, to);
    let bodies = penroam_core::operation::bodies_from_batch(nb, from, &batch);
    Txn {
        target: TxnTarget::Page {
            notebook_id: nb,
            page_id: to.id,
            revision: to.revision,
            batch_id: batch.batch_id,
        },
        ops: bodies
            .into_iter()
            .enumerate()
            .map(|(i, (id, body))| penroam_core::operation::LocalOperation {
                id,
                device_id: Uuid::nil(),
                local_sequence: i as u64 + 1,
                schema_version: penroam_core::operation::op_schema_for(&body),
                causal_dependencies: Vec::new(),
                at: to.updated_at,
                body,
            })
            .collect(),
    }
}

fn batch_between(from: &Page, to: &Page) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: from.revision,
        ops: page_diff(from, to)
            .into_iter()
            .map(|kind| PageOperation {
                op_id: Uuid::new_v4(),
                kind,
            })
            .collect(),
    }
}

/// A delta describing an install from sync, which isn't a journaled batch,
/// or `None` when the install changed nothing on this page.
pub(crate) fn install_txn(nb: Uuid, from: &Page, to: &Page) -> Option<Txn> {
    (!page_diff(from, to).is_empty()).then(|| synthetic_txn(nb, from, to))
}

/// Appends a line to the title and page order log, if it changed.
pub(crate) fn record_structure(nb_dir: &Path, last: &mut Option<Structure>, now: Structure) {
    if last.is_none() {
        *last = read_structure(nb_dir).pop();
    }
    if last
        .as_ref()
        .is_some_and(|l| l.title == now.title && l.page_order == now.page_order)
    {
        return;
    }
    if let Ok(mut line) = serde_json::to_vec(&now) {
        line.push(b'\n');
        if append(&structure_path(nb_dir), &line).is_ok() {
            *last = Some(now);
        }
    }
}

fn read_structure(nb_dir: &Path) -> Vec<Structure> {
    fs::read(structure_path(nb_dir))
        .map(|b| {
            b.split(|c| *c == b'\n')
                .filter_map(|l| serde_json::from_slice(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

impl Library {
    /// Every restorable version of a page, oldest first. A page never edited
    /// since history began has none yet.
    pub fn page_history(&self, nb_id: Uuid, page_id: Uuid) -> Result<Vec<HistoryPoint>> {
        self.notebook(nb_id)?;
        let path = chain_path(&self.nb_dir(nb_id), page_id);
        let mut v = Vec::new();
        walk(&read_entries(&path)?, |p, t| {
            v.push(point(p, t));
            true
        });
        Ok(v)
    }

    /// Every version of every page of a notebook, trash included, oldest first.
    pub fn notebook_history(&self, nb_id: Uuid) -> Result<Vec<HistoryPoint>> {
        let ids: Vec<Uuid> = self.notebook(nb_id)?.all_page_ids().collect();
        let mut v = Vec::new();
        for id in ids {
            v.extend(self.page_history(nb_id, id)?);
        }
        v.sort_by_key(|p| (p.at, p.page_id, p.revision));
        Ok(v)
    }

    /// A page as it was at `revision`, without changing anything. May replay
    /// a long run of operations. Errors with `NotFound` when that version
    /// isn't retained or can't be reached past damage.
    pub fn page_version(&self, nb_id: Uuid, page_id: Uuid, revision: u64) -> Result<Page> {
        self.notebook(nb_id)?;
        let entries = read_entries(&chain_path(&self.nb_dir(nb_id), page_id))?;
        let mut found = None;
        walk(&entries, |p, _| {
            if p.revision == revision {
                found = Some(p.clone());
                false
            } else {
                true
            }
        });
        found.ok_or_else(|| StorageError::NotFound(format!("version {revision} of page {page_id}")))
    }

    /// Brings a page back to `revision` with a new forward batch, which syncs
    /// and can be undone by restoring the version before it.
    pub fn restore_page_version(
        &mut self,
        nb_id: Uuid,
        page_id: Uuid,
        revision: u64,
    ) -> Result<ApplyOutcome> {
        let target = self.page_version(nb_id, page_id, revision)?;
        self.restore_page_content(nb_id, page_id, &target)
    }

    /// Makes a page's content equal `target`, wherever that version came from,
    /// with a new forward batch. Does nothing when it already is.
    pub fn restore_page_content(
        &mut self,
        nb_id: Uuid,
        page_id: Uuid,
        target: &Page,
    ) -> Result<ApplyOutcome> {
        let current = self.load_page(nb_id, page_id)?;
        let batch = batch_between(&current, target);
        if batch.ops.is_empty() {
            return Ok(ApplyOutcome {
                revision: current.revision,
                updated_at: current.updated_at,
                durable: true,
            });
        }
        self.apply_page_operations(nb_id, page_id, &batch)
    }

    /// A notebook's title and live page order as this device recorded them at time `at`, without
    /// changing anything. Before the first recorded change, the current title and order are all
    /// there is to go on, so those are returned.
    pub fn notebook_structure_at(&self, nb_id: Uuid, at: Timestamp) -> Result<(String, Vec<Uuid>)> {
        let nb = self.notebook(nb_id)?;
        Ok(read_structure(&self.nb_dir(nb_id))
            .into_iter()
            .take_while(|s| s.at <= at)
            .last()
            .map_or_else(
                || (nb.title.clone(), nb.page_order.clone()),
                |s| (s.title, s.page_order),
            ))
    }

    /// Brings a whole notebook back to how it was at time `at`: page content,
    /// page order and title. Every change is an ordinary operation. Pages made
    /// since go to the trash rather than being deleted.
    pub fn restore_notebook_version(
        &mut self,
        nb_id: Uuid,
        at: Timestamp,
    ) -> Result<NotebookRestore> {
        let (title, page_order) = self.notebook_structure_at(nb_id, at)?;
        let structure = Structure {
            at,
            title,
            page_order,
        };
        let mut targets = HashMap::new();
        for page in &structure.page_order {
            let version = self
                .page_history(nb_id, *page)?
                .into_iter()
                .take_while(|p| p.at <= at)
                .last();
            if let Some(v) = version {
                targets.insert(*page, self.page_version(nb_id, *page, v.revision)?);
            }
        }
        self.restore_notebook_content(nb_id, &structure.title, &structure.page_order, &targets)
    }

    /// Makes a notebook match a past state: `title`, `page_order`, and the content
    /// in `targets` (pages missing from it keep their content). Each step is its
    /// own ordinary operation, and pages not in `page_order` go to the trash.
    pub fn restore_notebook_content(
        &mut self,
        nb_id: Uuid,
        title: &str,
        page_order: &[Uuid],
        targets: &HashMap<Uuid, Page>,
    ) -> Result<NotebookRestore> {
        let mut report = NotebookRestore::default();
        let nb = self.notebook(nb_id)?.clone();
        let mut present = Vec::new();
        for page in page_order {
            if nb.trashed_pages.iter().any(|t| t.id == *page) {
                self.restore_page(nb_id, *page)?;
                report.pages_recovered += 1;
            } else if !nb.page_order.contains(page) {
                report.pages_unavailable += 1;
                continue;
            }
            present.push(*page);
            if let Some(target) = targets.get(page) {
                let before = self.load_page(nb_id, *page)?;
                self.restore_page_content(nb_id, *page, target)?;
                if self.load_page(nb_id, *page)?.revision != before.revision {
                    report.pages_reverted += 1;
                }
            }
        }
        if present.is_empty() {
            return Ok(report);
        }
        for page in nb.page_order.iter().filter(|p| !present.contains(p)) {
            self.delete_page(nb_id, *page)?;
            report.pages_trashed += 1;
        }
        if self.notebook(nb_id)?.page_order != present {
            self.update_page_order(nb_id, present)?;
        }
        if self.notebook(nb_id)?.title != title {
            self.rename_notebook(nb_id, title)?;
        }
        Ok(report)
    }

    /// Bytes and pages of a notebook's history on this device.
    pub fn history_stats(&self, nb_id: Uuid) -> Result<HistoryStats> {
        self.notebook(nb_id)?;
        let mut s = HistoryStats::default();
        if let Ok(rd) = fs::read_dir(dir(&self.nb_dir(nb_id))) {
            for e in rd.flatten() {
                s.bytes += e.metadata().map_or(0, |m| m.len());
                s.pages += e.path().extension().is_some_and(|x| x == "phl") as u32;
            }
        }
        Ok(s)
    }

    /// Deletes a notebook's history, so erased content is gone from this
    /// device's history straight away. The notebook itself is unchanged.
    pub fn clear_history(&mut self, nb_id: Uuid) -> Result<()> {
        self.notebook(nb_id)?;
        let path = dir(&self.nb_dir(nb_id));
        self.forget_history(nb_id);
        crate::fsutil::remove_dir_all_durable(&path)?;
        Ok(())
    }

    /// Applies a retention policy to one notebook's history and returns the
    /// bytes freed. `now` is the current time.
    pub fn compact_history(
        &mut self,
        nb_id: Uuid,
        now: Timestamp,
        retention: &HistoryRetention,
    ) -> Result<u64> {
        let ids: Vec<Uuid> = self.notebook(nb_id)?.all_page_ids().collect();
        let nb_dir = self.nb_dir(nb_id);
        let mut freed = 0;
        for id in ids {
            freed += compact_chain(&chain_path(&nb_dir, id), now, retention)?;
        }
        self.forget_history(nb_id);
        Ok(freed)
    }
}
