//! Durable sync metadata under `<library>/sync/`.
//!
//! ```text
//! sync/state.json        SyncState (atomic writes)
//! sync/oplog.pjs         every operation of every synced notebook, local and remote
//! sync/pending/<op>.json verified envelopes waiting for dependencies, keys or an upgrade
//! sync/quarantine/<op>.json envelopes that failed authentication (never applied)
//! sync/blobs/<sha256>    downloaded assets waiting to be installed
//! sync/secrets/          file secret store (fallback when no OS keychain is used)
//! ```
//!
//! The operation log is append-only: `u32 len ‖ u32 crc32 ‖ payload` records
//! after an 8-byte header. A torn tail (crash mid-append) is truncated on
//! open; a damaged record in the middle stops the scan and is reported
//! (nothing is deleted).
//!
//! **Durability ordering.** Appends may be buffered
//! ([`OpLog::append_buffered`]); [`SyncStore::save`] always flushes the log
//! before writing `state.json`, so the state never claims (cursor, ingestion
//! position, acks) more than the log durably holds. After a crash between the
//! two, the engine repairs the counters from the log on open. Every other file
//! is written atomically (temp file, fsync, rename).
//!
//! The log holds both local operations (re-sequenced as the sync device's) and
//! remote ones; it is the source the merge model (`Replica`) is rebuilt from
//! and the durability source for remote changes installed into the library.

use penroam_storage::fsutil::write_atomic;
use penroam_sync_protocol::EnvelopeHeader;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::keylog::Pin;

/// File magic of `oplog.pjs`.
const OPLOG_MAGIC: &[u8; 4] = b"PRSL";
/// Log format version; newer files are refused, never rewritten.
const OPLOG_VERSION: u16 = 1;
/// Magic (4) + version (2) + reserved (2).
const OPLOG_HEADER: usize = 8;
/// Upper bound on one record, so a corrupt length cannot trigger a huge read.
const MAX_OPLOG_RECORD: usize = 64 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
/// Everything the engine must remember between runs (`sync/state.json`).
pub struct SyncState {
    /// Always `"penroam-sync-state"`.
    pub format: String,
    /// State format version (1).
    pub version: u32,
    /// Server base URL.
    pub server_url: String,
    /// Account id on that server.
    pub account_id: String,
    /// The sync device id (not the library device id).
    pub device_id: String,
    /// User-chosen device label (also in this device's key-log certificate).
    pub label: String,
    /// The device's keys were granted epoch keys in the verified key log.
    pub enrolled: bool,
    /// The key-log prefix this device verified last (see [`Pin`]).
    pub pin: Option<Pin>,
    /// Highest sync sequence assigned to a local operation.
    pub next_seq: u64,
    /// Highest library journal sequence copied into the operation log.
    pub ingested_lib_seq: u64,
    /// Whether creation operations were synthesized for pre-existing notebooks.
    pub seeded: bool,
    /// Highest contiguous sync sequence the server acknowledged.
    pub pushed: u64,
    /// Acknowledged sequences above `pushed` (acks are per operation).
    #[serde(default)]
    pub acked_above: BTreeSet<u64>,
    /// Server cursor after the last pulled page whose envelopes are all durably
    /// in the operation log or parked. Never advanced before that.
    pub cursor: Option<String>,
    /// Version vector (device id → highest sequence) the library's
    /// materialized view reflects. New local operations depend on exactly this.
    #[serde(default)]
    pub installed: BTreeMap<String, u64>,
    /// Pages (`notebook/page` keys) that received remote content operations or
    /// were created remotely and must be reinstalled from the merge model.
    #[serde(default)]
    pub dirty_pages: BTreeSet<String>,
    /// Notebooks with delivered remote operations not yet installed.
    #[serde(default)]
    pub dirty: BTreeSet<Uuid>,
    /// Blob ids the server confirmed, so assets are uploaded once.
    #[serde(default)]
    pub uploaded_blobs: BTreeSet<String>,
    /// User paused sync; reconciles do nothing until resumed.
    #[serde(default)]
    pub paused: bool,
    /// Wall-clock time of the last successful reconcile (display only; it is
    /// persisted lazily with the next real state change).
    #[serde(default)]
    pub last_success_ms: Option<i64>,
}

impl SyncState {
    /// State for a freshly set-up device (not yet enrolled, nothing synced).
    pub fn new(server_url: &str, account_id: &str, device_id: &str, label: &str) -> Self {
        Self {
            format: "penroam-sync-state".into(),
            version: 1,
            server_url: server_url.into(),
            account_id: account_id.into(),
            device_id: device_id.into(),
            label: label.into(),
            enrolled: false,
            pin: None,
            next_seq: 0,
            ingested_lib_seq: 0,
            seeded: false,
            pushed: 0,
            acked_above: BTreeSet::new(),
            cursor: None,
            installed: BTreeMap::new(),
            dirty_pages: BTreeSet::new(),
            dirty: BTreeSet::new(),
            uploaded_blobs: BTreeSet::new(),
            paused: false,
            last_success_ms: None,
        }
    }

    /// Records a server ack for one local sequence. Acks can arrive out of
    /// order (per-operation results in a batch); `pushed` only advances over a
    /// contiguous run so "everything up to `pushed` is uploaded" stays true.
    pub fn ack(&mut self, seq: u64) {
        if seq <= self.pushed {
            return;
        }
        self.acked_above.insert(seq);
        while self.acked_above.remove(&(self.pushed + 1)) {
            self.pushed += 1;
        }
    }

    /// Whether the server confirmed local sequence `seq`.
    pub fn is_acked(&self, seq: u64) -> bool {
        seq <= self.pushed || self.acked_above.contains(&seq)
    }
}

/// Where an operation in the log came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// Made on this device (copied from the library journal or synthesized
    /// when seeding); must be uploaded.
    #[serde(rename = "local")]
    Local,
    /// Received from another device, verified and decrypted.
    #[serde(rename = "remote")]
    Remote,
}

/// Metadata stored next to each operation record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpMeta {
    /// Local or remote.
    pub origin: Origin,
    /// Notebook the operation belongs to.
    pub nb: Uuid,
    /// Operation id (deduplication key).
    pub op: Uuid,
    /// Writing sync device.
    pub dev: Uuid,
    /// That device's sequence number for the operation.
    pub seq: u64,
    /// SHA-256 of the ciphertext as received (remote) — used to detect an
    /// operation id reused with different content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ct: Option<String>,
    /// Library journal sequence this local operation came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lib: Option<u64>,
    /// Content key epoch the operation is sealed with. Fixed when the
    /// operation enters the log, so retries produce identical ciphertext even
    /// if a device removal starts a new epoch in between.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<u32>,
}

/// An indexed log entry: metadata in memory, record bytes on disk.
#[derive(Debug, Clone)]
pub struct OpEntry {
    /// The entry's metadata.
    pub meta: OpMeta,
    offset: u64,
    len: u32,
}

/// The append-only operation log with in-memory indexes.
pub struct OpLog {
    file: File,
    path: PathBuf,
    end: u64,
    entries: Vec<OpEntry>,
    by_op: HashMap<Uuid, usize>,
    by_nb: HashMap<Uuid, Vec<usize>>,
    /// Highest sequence per device. Because operations are only appended in
    /// causal order (each device contiguously), this is also the delivered
    /// version vector the engine checks dependencies against.
    pub frontier: BTreeMap<Uuid, u64>,
    /// Set when a damaged record stopped the scan on open; the log then refuses
    /// appends and the engine reports an integrity problem.
    pub damaged: Option<String>,
    unsynced: bool,
}

/// CRC-32 of a record payload (detects torn and damaged records).
fn crc(b: &[u8]) -> u32 {
    crc32fast::hash(b)
}

impl OpLog {
    /// Opens or creates the log, scans it, rebuilds the indexes and truncates a
    /// torn tail. A damaged record before the end stops the scan without
    /// deleting anything (see [`OpLog::damaged`]).
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes.len() < OPLOG_HEADER {
            let mut h = OPLOG_MAGIC.to_vec();
            h.extend_from_slice(&OPLOG_VERSION.to_le_bytes());
            h.extend_from_slice(&[0, 0]);
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&h)?;
            file.sync_all()?;
            bytes = h;
        }
        if &bytes[..4] != OPLOG_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a sync operation log",
            ));
        }
        let ver = u16::from_le_bytes([bytes[4], bytes[5]]);
        if ver > OPLOG_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the sync operation log was written by a newer Penroam",
            ));
        }
        let mut log = Self {
            file,
            path: path.to_path_buf(),
            end: OPLOG_HEADER as u64,
            entries: Vec::new(),
            by_op: HashMap::new(),
            by_nb: HashMap::new(),
            frontier: BTreeMap::new(),
            damaged: None,
            unsynced: false,
        };
        let mut i = OPLOG_HEADER;
        while i + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
            let sum = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap());
            if len > MAX_OPLOG_RECORD || i + 8 + len > bytes.len() {
                break; // torn tail
            }
            let payload = &bytes[i + 8..i + 8 + len];
            if crc(payload) != sum {
                if i + 8 + len == bytes.len() {
                    break; // torn tail written out of order
                }
                log.damaged = Some(format!(
                    "damaged record at byte {i}; later records kept on disk"
                ));
                break;
            }
            let Some(meta) = parse_meta(payload) else {
                log.damaged = Some(format!("unreadable record metadata at byte {i}"));
                break;
            };
            log.index(meta, i as u64, (8 + len) as u32);
            i += 8 + len;
        }
        if log.damaged.is_none() && (i as u64) < bytes.len() as u64 {
            log.file.set_len(i as u64)?;
            log.file.sync_all()?;
        }
        log.end = i as u64;
        Ok(log)
    }

    /// Adds an entry to the in-memory indexes and the frontier.
    fn index(&mut self, meta: OpMeta, offset: u64, len: u32) {
        let idx = self.entries.len();
        self.by_op.insert(meta.op, idx);
        self.by_nb.entry(meta.nb).or_default().push(idx);
        let f = self.frontier.entry(meta.dev).or_insert(0);
        *f = (*f).max(meta.seq);
        self.entries.push(OpEntry { meta, offset, len });
    }

    /// Appends one operation and fsyncs before returning.
    pub fn append(&mut self, meta: OpMeta, record: &[u8]) -> io::Result<()> {
        self.append_buffered(meta, record)?;
        self.flush()
    }

    /// Makes every buffered append durable.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.unsynced {
            self.file.sync_data()?;
            self.unsynced = false;
        }
        Ok(())
    }

    /// Appends without fsync; callers must [`OpLog::flush`] before relying
    /// on durability (saving state, the cursor, or deleting a parked copy).
    pub fn append_buffered(&mut self, meta: OpMeta, record: &[u8]) -> io::Result<()> {
        if self.damaged.is_some() {
            return Err(io::Error::other(
                "the sync operation log is damaged; not appending",
            ));
        }
        let m = serde_json::to_vec(&meta).map_err(io::Error::other)?;
        let mut payload = Vec::with_capacity(4 + m.len() + record.len());
        payload.extend_from_slice(&(m.len() as u32).to_le_bytes());
        payload.extend_from_slice(&m);
        payload.extend_from_slice(record);
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&crc(&payload).to_le_bytes());
        out.extend_from_slice(&payload);
        self.file.seek(SeekFrom::Start(self.end))?;
        if let Err(e) = self.file.write_all(&out) {
            // roll back a partial append so the log stays consistent
            let _ = self.file.set_len(self.end);
            return Err(e);
        }
        let offset = self.end;
        self.unsynced = true;
        self.end += out.len() as u64;
        self.index(meta, offset, out.len() as u32);
        Ok(())
    }

    /// Whether an operation id is already in the log (deduplication).
    pub fn contains(&self, op: &Uuid) -> bool {
        self.by_op.contains_key(op)
    }

    /// Metadata of an operation, by id.
    pub fn meta_of(&self, op: &Uuid) -> Option<&OpMeta> {
        self.by_op.get(op).map(|i| &self.entries[*i].meta)
    }

    /// Number of operations in the log.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, in log order.
    pub fn entries(&self) -> &[OpEntry] {
        &self.entries
    }

    /// Ids of every notebook with operations, sorted.
    pub fn notebooks(&self) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = self.by_nb.keys().copied().collect();
        v.sort();
        v
    }

    /// Indexes of a notebook's entries, in log order.
    pub fn indexes_for(&self, nb: &Uuid) -> &[usize] {
        self.by_nb.get(nb).map_or(&[], Vec::as_slice)
    }

    /// Entry `idx` (an index from [`OpLog::indexes_for`] or [`OpLog::entries`]).
    pub fn entry(&self, idx: usize) -> &OpEntry {
        &self.entries[idx]
    }

    /// The operation record bytes (journal encoding) of entry `idx`.
    pub fn record(&self, idx: usize) -> io::Result<Vec<u8>> {
        let e = &self.entries[idx];
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(e.offset + 8))?;
        let mut payload = vec![0u8; e.len as usize - 8];
        f.read_exact(&mut payload)?;
        let ml = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
        Ok(payload[4 + ml..].to_vec())
    }
}

/// Reads the length-prefixed JSON metadata at the start of a record payload.
fn parse_meta(payload: &[u8]) -> Option<OpMeta> {
    let ml = u32::from_le_bytes(payload.get(..4)?.try_into().ok()?) as usize;
    serde_json::from_slice(payload.get(4..4 + ml)?).ok()
}

/// A verified envelope parked until it can be applied, or a rejected one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
///
/// Kinds (subdirectories): `pending` (reason `deps` = waiting for causal
/// dependencies, `key` = epoch key not known yet, `unknown-device` = signer
/// not in the key log yet, `upgrade` = newer operation schema) and
/// `quarantine` (failed verification; the reason says why).
pub struct Parked {
    /// Server sequence the envelope arrived with (processing order).
    pub server_seq: u64,
    /// The envelope header as received.
    pub header: EnvelopeHeader,
    /// base64url ciphertext (kept encrypted at rest).
    pub ciphertext: String,
    /// Why it is parked (see the type docs).
    pub reason: String,
}

/// The sync folder of one library: state, operation log and parked envelopes.
pub struct SyncStore {
    /// `<library>/sync`.
    pub dir: PathBuf,
    /// The loaded state; persist changes with [`SyncStore::save`].
    pub state: SyncState,
    /// The operation log.
    pub oplog: OpLog,
}

impl SyncStore {
    /// The sync folder for a library root.
    pub fn dir_for(library_root: &Path) -> PathBuf {
        library_root.join("sync")
    }

    /// Whether sync is set up for this library.
    pub fn exists(library_root: &Path) -> bool {
        Self::dir_for(library_root).join("state.json").exists()
    }

    /// Creates the sync folder with an initial state (setup).
    pub fn create(library_root: &Path, state: SyncState) -> io::Result<Self> {
        let dir = Self::dir_for(library_root);
        for sub in ["pending", "quarantine", "blobs", "secrets"] {
            fs::create_dir_all(dir.join(sub))?;
        }
        write_atomic(
            &dir.join("state.json"),
            &serde_json::to_vec_pretty(&state).map_err(io::Error::other)?,
        )
        .map_err(|e| io::Error::other(e.to_string()))?;
        let oplog = OpLog::open(&dir.join("oplog.pjs"))?;
        Ok(Self { dir, state, oplog })
    }

    /// Opens an existing setup; refuses state written by a newer version.
    pub fn open(library_root: &Path) -> io::Result<Self> {
        let dir = Self::dir_for(library_root);
        let raw = fs::read(dir.join("state.json"))?;
        let state: SyncState = serde_json::from_slice(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if state.format != "penroam-sync-state" || state.version > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "sync state was written by a newer Penroam",
            ));
        }
        for sub in ["pending", "quarantine", "blobs", "secrets"] {
            fs::create_dir_all(dir.join(sub))?;
        }
        let oplog = OpLog::open(&dir.join("oplog.pjs"))?;
        Ok(Self { dir, state, oplog })
    }

    /// Persists the state atomically, after making buffered log appends durable.
    pub fn save(&mut self) -> io::Result<()> {
        // state never claims more than the operation log durably holds
        self.oplog.flush()?;
        let bytes = serde_json::to_vec_pretty(&self.state).map_err(io::Error::other)?;
        write_atomic(&self.dir.join("state.json"), &bytes)
            .map_err(|e| io::Error::other(e.to_string()))
    }

    fn parked_path(&self, kind: &str, op: &str) -> PathBuf {
        self.dir.join(kind).join(format!("{op}.json"))
    }

    /// Stores an envelope under `kind` (atomic; replaces an earlier copy).
    pub fn park(&self, kind: &str, p: &Parked) -> io::Result<()> {
        let bytes = serde_json::to_vec(p).map_err(io::Error::other)?;
        write_atomic(&self.parked_path(kind, p.header.op_id.as_str()), &bytes)
            .map_err(|e| io::Error::other(e.to_string()))
    }

    /// Removes a parked envelope (missing is fine).
    pub fn unpark(&self, kind: &str, op: &str) -> io::Result<()> {
        match fs::remove_file(self.parked_path(kind, op)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// All parked envelopes of `kind`, in server order. Unreadable files are skipped.
    pub fn parked(&self, kind: &str) -> Vec<Parked> {
        let mut v: Vec<Parked> = fs::read_dir(self.dir.join(kind))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        v.sort_by_key(|p: &Parked| p.server_seq);
        v
    }

    /// Number of files under `kind` (status display).
    pub fn parked_count(&self, kind: &str) -> usize {
        // count only finished entries, not temporary files of an atomic write
        fs::read_dir(self.dir.join(kind))
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Where a downloaded, verified asset waits until it is installed.
    pub fn blob_cache(&self, sha: &str) -> PathBuf {
        self.dir.join("blobs").join(sha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(i: u64) -> OpMeta {
        OpMeta {
            origin: Origin::Local,
            nb: Uuid::from_u128(1),
            op: Uuid::from_u128(100 + i as u128),
            dev: Uuid::from_u128(9),
            seq: i,
            ct: None,
            lib: Some(i),
            epoch: None,
        }
    }

    #[test]
    fn oplog_append_reopen_and_torn_tail() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("oplog.pjs");
        {
            let mut l = OpLog::open(&p).unwrap();
            for i in 1..=3 {
                l.append(meta(i), format!("record-{i}").as_bytes()).unwrap();
            }
        }
        // torn tail: half a record
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&[200, 0, 0, 0, 1, 2]).unwrap();
        drop(f);
        let l = OpLog::open(&p).unwrap();
        assert_eq!(l.len(), 3);
        assert!(l.damaged.is_none());
        assert_eq!(l.record(1).unwrap(), b"record-2");
        assert_eq!(l.frontier[&Uuid::from_u128(9)], 3);
        assert!(l.contains(&Uuid::from_u128(101)));
        assert_eq!(l.indexes_for(&Uuid::from_u128(1)).len(), 3);
    }

    #[test]
    fn acks_are_per_operation() {
        let mut s = SyncState::new("u", "a", "d", "l");
        s.ack(2);
        assert_eq!(s.pushed, 0);
        assert!(s.is_acked(2) && !s.is_acked(1));
        s.ack(1);
        assert_eq!(s.pushed, 2);
        assert!(s.acked_above.is_empty());
    }
}
