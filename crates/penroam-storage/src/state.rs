//! Small library-level state files, each written atomically:
//!
//! * `library.json` — format marker; the version is checked on open.
//! * `device.json` — this installation's device id (stable, random).
//! * `checkpoint.json` (+ `checkpoint.prev.json`) — checkpoint manifest:
//!   every operation with sequence `<= floor` is reflected in the page
//!   snapshots and `notebook.json` files. Journal segments are deleted only
//!   once they are `<= prev_floor` (covered by this *and* the previous
//!   checkpoint), so a page can always fall back to its previous snapshot
//!   (`<page>.prp.prev`) and replay the journal from there.
//! * `outbox.json` — sync outbox counters, derivable from the journal.

use crate::fsutil::write_atomic;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

/// 1: per-page JSON-lines journals. 2: device operation journal, notebook schema 2.
pub const LIBRARY_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryMarker {
    pub format: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceFile {
    pub format: String,
    pub device_id: Uuid,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointManifest {
    pub format: String,
    pub version: u32,
    pub device_id: Uuid,
    /// Highest sequence ever allocated (persisted so it survives journal
    /// truncation; sequences never go backwards).
    pub last_seq: u64,
    /// Every operation `<= floor` is in a snapshot.
    pub floor: u64,
    /// `floor` of the previous checkpoint.
    pub prev_floor: u64,
    /// Per notebook: the journal sequence a merged sync install covered.
    /// Replay skips notebook-level transactions at or below it (the
    /// installed snapshot already contains them, merged).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub installed: std::collections::BTreeMap<Uuid, u64>,
}

impl CheckpointManifest {
    pub fn new(device_id: Uuid) -> Self {
        Self {
            format: "penroam-checkpoint".into(),
            version: 1,
            device_id,
            last_seq: 0,
            floor: 0,
            prev_floor: 0,
            installed: Default::default(),
        }
    }
}

/// Sync outbox counters (the sync engine drives `uploaded_through`/`remote_seen`).
///
/// Derivable from the journal: if this file is lost or damaged it is rebuilt
/// with `uploaded_through = 0`, which only causes re-uploads (the server
/// deduplicates by operation id) — never lost operations. Server
/// acknowledgements are persisted here atomically *after* the server
/// acknowledged, so a crash in between also only causes a re-upload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxState {
    pub format: String,
    pub version: u32,
    pub device_id: Uuid,
    /// Highest local sequence durably journaled.
    pub enqueued_through: u64,
    /// Highest local sequence the server acknowledged as stored.
    pub uploaded_through: u64,
    /// Opaque server cursor of remote operations durably applied locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_seen: Option<String>,
    /// While false, the journal is truncated after checkpoints regardless of
    /// `uploaded_through` (sync is off).
    #[serde(default)]
    pub sync_enabled: bool,
}

impl OutboxState {
    pub fn new(device_id: Uuid) -> Self {
        Self {
            format: "penroam-outbox".into(),
            version: 1,
            device_id,
            enqueued_through: 0,
            uploaded_through: 0,
            remote_seen: None,
            sync_enabled: false,
        }
    }
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn write_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    write_atomic(
        path,
        &serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?,
    )
}

/// Reads the manifest, falling back to the previous copy.
pub fn read_manifest(root: &Path) -> (Option<CheckpointManifest>, Option<String>) {
    let ok = |m: &CheckpointManifest| m.format == "penroam-checkpoint" && m.version == 1;
    if let Some(m) = read_json::<CheckpointManifest>(&root.join("checkpoint.json")).filter(ok) {
        return (Some(m), None);
    }
    let prev = read_json::<CheckpointManifest>(&root.join("checkpoint.prev.json")).filter(ok);
    let warn = (root.join("checkpoint.json").exists() && prev.is_some()).then(|| {
        "checkpoint manifest was damaged; used the previous one and replayed the journal".into()
    });
    (prev, warn)
}

/// Persists a new manifest. The current file becomes the previous one by
/// rename (atomic, no rewrite), then the new one is written atomically. A
/// crash in between leaves only the previous manifest, which is valid: a
/// lower floor only means more (idempotent) replay.
pub fn write_manifest(
    root: &Path,
    old: &CheckpointManifest,
    new: &CheckpointManifest,
) -> std::io::Result<()> {
    let cur = root.join("checkpoint.json");
    let prev = root.join("checkpoint.prev.json");
    if cur.exists() {
        crate::fsutil::rename_durable(&cur, &prev)?;
    } else {
        write_json(&prev, old)?;
    }
    write_json(&cur, new)
}
