//! The device-wide operation journal: the durability source for every
//! acknowledged local edit until a checkpoint (page snapshot +
//! `notebook.json` + checkpoint manifest) supersedes it. Normative format:
//! `docs/formats/penroam-binary-v1.md` §7.
//!
//! ```text
//! <library>/journal/<first seq, 20 digits>.pjl     segments, append-only
//! <library>/journal/quarantine/                   copies of damaged segments
//!
//! segment = header, record*
//! header  = "PRJL" u16 version(=1) u16 flags(=0) u64 first_seq u32 crc32(previous 16 bytes)
//! record  = "PRJR" u32 len u32 crc32(payload) payload[len]
//! ```
//!
//! A record is one atomic transaction (all of an editor batch, or one
//! notebook-level change) of operations with consecutive sequences. It is
//! acknowledged only after `write + fsync` returned. On open:
//!
//! * a damaged or short record with **no valid record after it** at the end
//!   of the last segment is a torn write and is truncated away;
//! * a damaged record **followed by valid records** is corruption: the
//!   segment is copied to `quarantine/`, every valid record (before and
//!   after) is still returned, the corruption is reported and new records go
//!   to a fresh segment — nothing is truncated or deleted.
//!
//! Records from a newer operation schema stop the open with
//! [`JournalError::NewerSchema`] (fail safe; nothing is modified).
//!
//! The journal only stores and returns transactions; deciding what to
//! replay (everything after the checkpoint floor, skipping what a sync
//! install covered) and when to delete segments is
//! [`crate::library::Library`]'s job. The same record encoding is reused as
//! the plaintext payload of sync envelopes ([`encode_txn`] / [`decode_txn`]).

use crate::fsutil::{self, append_durable, create_durable};
use penroam_core::binary::{decode_point_block, encode_point_block};
use penroam_core::operation::*;
use penroam_core::*;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// First four bytes of every segment file.
pub const SEGMENT_MAGIC: &[u8; 4] = b"PRJL";
/// First four bytes of every record (also used to resynchronize after damage).
pub const RECORD_MAGIC: &[u8; 4] = b"PRJR";
/// Segment format version written in the header; other versions are refused.
pub const JOURNAL_FORMAT_VERSION: u16 = 1;
const HEADER_LEN: usize = 20;
const RECORD_HEADER_LEN: usize = 12;
/// Roll to a new segment once the active one exceeds this.
pub const SEGMENT_TARGET_BYTES: u64 = 8 << 20;
/// Largest record accepted when reading (hostile-input guard).
pub const MAX_RECORD_BYTES: usize = 1 << 30;
const MAX_OPS_PER_RECORD: usize = 1 << 20;
const MAX_DEPS: usize = 4096;

/// What a transaction applies to.
#[derive(Debug, Clone, PartialEq)]
pub enum TxnTarget {
    /// Notebook-level operations (notebook and page list changes).
    Notebook {
        /// The notebook changed.
        notebook_id: Uuid,
    },
    /// One editor batch on one page; `revision` is the page revision after it.
    Page {
        /// Notebook of the page.
        notebook_id: Uuid,
        /// The page edited.
        page_id: Uuid,
        /// Page revision after this batch. Replay applies a batch only when
        /// it is exactly one above the snapshot's revision.
        revision: u64,
        /// Editor batch id; a retried batch with the same id is not applied twice.
        batch_id: Uuid,
    },
}

impl TxnTarget {
    /// The notebook this transaction belongs to.
    pub fn notebook_id(&self) -> Uuid {
        match self {
            TxnTarget::Notebook { notebook_id } | TxnTarget::Page { notebook_id, .. } => {
                *notebook_id
            }
        }
    }
}

/// One journal record: an atomic group of operations with consecutive
/// sequence numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct Txn {
    /// What the operations apply to.
    pub target: TxnTarget,
    /// The operations, in sequence order (never empty when decoded).
    pub ops: Vec<LocalOperation>,
}

impl Txn {
    /// Sequence of the first operation (0 for an empty transaction).
    pub fn first_seq(&self) -> u64 {
        self.ops.first().map_or(0, |o| o.local_sequence)
    }
    /// Sequence of the last operation (0 for an empty transaction).
    pub fn last_seq(&self) -> u64 {
        self.ops.last().map_or(0, |o| o.local_sequence)
    }
}

/// Errors that stop the journal from being opened or appended to.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// File-system failure (including a full disk).
    #[error("journal: {0}")]
    Io(#[from] io::Error),
    /// A record uses an operation schema this build does not understand.
    /// Opening stops without modifying anything; the user must update.
    #[error("the journal was written by a newer Penroam (operation schema {0}); please update")]
    NewerSchema(u16),
    /// A segment has an unknown version/flags, or a damaged header and no
    /// readable records.
    #[error("journal segment {0} has an unsupported format")]
    UnsupportedSegment(String),
}

/// What [`Journal::open`] found and repaired (for the recovery report).
#[derive(Debug, Default)]
pub struct ScanReport {
    /// Incomplete records/segments at the very end discarded (crash mid-write).
    pub truncated_tails: u32,
    /// Damaged records with valid data after them (segment quarantined).
    pub corrupt_records: u32,
    /// Human-readable descriptions of the above.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct Segment {
    path: PathBuf,
    first_seq: u64,
    last_seq: Option<u64>,
}

/// The open journal directory. Single writer: the library holds an
/// exclusive lock while it is open.
pub struct Journal {
    dir: PathBuf,
    /// Sorted by first_seq; the last one is the active segment unless sealed.
    segments: Vec<Segment>,
    active: Option<(File, u64)>,
}

// --------------------------------------------------------------- encode --

fn put_u32(o: &mut Vec<u8>, v: u32) {
    o.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(o: &mut Vec<u8>, v: u64) {
    o.extend_from_slice(&v.to_le_bytes());
}

/// Shallow copy of a body with ink points left out (they go to blocks).
fn body_without_points(body: &DocumentOperation) -> DocumentOperation {
    let strip = |els: &[Element]| -> Vec<Element> {
        els.iter()
            .map(|e| match e {
                Element::Ink(s) => Element::Ink(InkStroke {
                    id: s.id,
                    z: s.z,
                    tool: s.tool,
                    color: s.color.clone(),
                    opacity: s.opacity,
                    width: s.width,
                    pressure: s.pressure,
                    source: s.source.clone(),
                    input: s.input.clone(),
                    points: Vec::new(),
                    extra: s.extra.clone(),
                }),
                other => other.clone(),
            })
            .collect()
    };
    match body {
        DocumentOperation::ElementsAdded {
            notebook_id,
            page_id,
            elements,
        } => DocumentOperation::ElementsAdded {
            notebook_id: *notebook_id,
            page_id: *page_id,
            elements: strip(elements),
        },
        DocumentOperation::ElementsReplaced {
            notebook_id,
            page_id,
            elements,
            changed,
        } => DocumentOperation::ElementsReplaced {
            notebook_id: *notebook_id,
            page_id: *page_id,
            elements: strip(elements),
            changed: changed.clone(),
        },
        other => other.clone(),
    }
}

/// Encodes a transaction as one complete record (header + CRC + payload).
///
/// Payload layout: target (kind byte + ids, and for pages the revision and
/// batch id), operation count, then per operation its id, device, sequence,
/// schema version, timestamp, causal dependencies, the body as JSON *without*
/// ink points, and the points of each stroke as compact column blocks
/// (`penroam_core::binary`), which keeps ink-heavy records small.
pub fn encode_txn(txn: &Txn) -> Vec<u8> {
    let mut p = Vec::new();
    match &txn.target {
        TxnTarget::Notebook { notebook_id } => {
            p.push(0);
            p.extend_from_slice(notebook_id.as_bytes());
        }
        TxnTarget::Page {
            notebook_id,
            page_id,
            revision,
            batch_id,
        } => {
            p.push(1);
            p.extend_from_slice(notebook_id.as_bytes());
            p.extend_from_slice(page_id.as_bytes());
            put_u64(&mut p, *revision);
            p.extend_from_slice(batch_id.as_bytes());
        }
    }
    put_u32(&mut p, txn.ops.len() as u32);
    for op in &txn.ops {
        p.extend_from_slice(op.id.as_bytes());
        p.extend_from_slice(op.device_id.as_bytes());
        put_u64(&mut p, op.local_sequence);
        p.extend_from_slice(&op.schema_version.to_le_bytes());
        p.extend_from_slice(&op.at.to_le_bytes());
        put_u32(&mut p, op.causal_dependencies.len() as u32);
        for d in &op.causal_dependencies {
            p.extend_from_slice(d.device_id.as_bytes());
            put_u64(&mut p, d.device_seq);
        }
        let json = serde_json::to_vec(&body_without_points(&op.body)).expect("op serialises");
        put_u32(&mut p, json.len() as u32);
        p.extend_from_slice(&json);
        let strokes = op.body.ink_strokes();
        put_u32(&mut p, strokes.len() as u32);
        for s in strokes {
            encode_point_block(&mut p, &s.points);
        }
    }
    let mut out = Vec::with_capacity(p.len() + RECORD_HEADER_LEN);
    out.extend_from_slice(RECORD_MAGIC);
    put_u32(&mut out, p.len() as u32);
    put_u32(&mut out, crc32fast::hash(&p));
    out.extend_from_slice(&p);
    out
}

/// Errors of [`decode_txn`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RecordError {
    /// Bad header, length, checksum or payload.
    #[error("malformed operation record: {0}")]
    Malformed(String),
    /// The record is intact but uses a newer operation schema.
    #[error("operation schema {0} is newer than this build supports")]
    NewerSchema(u16),
}

/// Decodes exactly one record as written by [`encode_txn`] (magic, length,
/// CRC-32, payload, nothing after it). Sync payloads use this encoding.
pub fn decode_txn(record: &[u8]) -> Result<Txn, RecordError> {
    let bad = |m: &str| RecordError::Malformed(m.to_string());
    if record.len() < RECORD_HEADER_LEN || &record[..4] != RECORD_MAGIC {
        return Err(bad("bad record header"));
    }
    let len = u32::from_le_bytes(record[4..8].try_into().unwrap()) as usize;
    let crc = u32::from_le_bytes(record[8..12].try_into().unwrap());
    if len > MAX_RECORD_BYTES || record.len() != RECORD_HEADER_LEN + len {
        return Err(bad("record length does not match"));
    }
    let payload = &record[RECORD_HEADER_LEN..];
    if crc32fast::hash(payload) != crc {
        return Err(bad("record checksum mismatch"));
    }
    decode_payload(payload).map_err(|e| match e {
        DecodeError::NewerSchema(v) => RecordError::NewerSchema(v),
        DecodeError::Malformed(m) => RecordError::Malformed(m),
    })
}

// --------------------------------------------------------------- decode --

#[derive(Debug)]
enum DecodeError {
    Malformed(#[allow(dead_code)] String),
    NewerSchema(u16),
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .i
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or_else(|| DecodeError::Malformed("truncated record".into()))?;
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn uuid(&mut self) -> Result<Uuid, DecodeError> {
        Ok(Uuid::from_bytes(self.take(16)?.try_into().unwrap()))
    }
}

fn decode_payload(p: &[u8]) -> Result<Txn, DecodeError> {
    let mut r = Rd { b: p, i: 0 };
    let target = match r.u8()? {
        0 => TxnTarget::Notebook {
            notebook_id: r.uuid()?,
        },
        1 => TxnTarget::Page {
            notebook_id: r.uuid()?,
            page_id: r.uuid()?,
            revision: r.u64()?,
            batch_id: r.uuid()?,
        },
        k => return Err(DecodeError::Malformed(format!("unknown target kind {k}"))),
    };
    let n = r.u32()? as usize;
    if n == 0 || n > MAX_OPS_PER_RECORD {
        return Err(DecodeError::Malformed("bad operation count".into()));
    }
    let mut ops = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let id = r.uuid()?;
        let device_id = r.uuid()?;
        let local_sequence = r.u64()?;
        let schema_version = r.u16()?;
        let at = i64::from_le_bytes(r.take(8)?.try_into().unwrap());
        if schema_version > OP_SCHEMA_VERSION {
            return Err(DecodeError::NewerSchema(schema_version));
        }
        let nd = r.u32()? as usize;
        if nd > MAX_DEPS {
            return Err(DecodeError::Malformed("too many dependencies".into()));
        }
        let mut deps = Vec::with_capacity(nd);
        for _ in 0..nd {
            deps.push(OperationRef {
                device_id: r.uuid()?,
                device_seq: r.u64()?,
            });
        }
        let jl = r.u32()? as usize;
        let mut body: DocumentOperation = serde_json::from_slice(r.take(jl)?)
            .map_err(|e| DecodeError::Malformed(format!("operation body: {e}")))?;
        let ns = r.u32()? as usize;
        let mut strokes = body.ink_strokes_mut();
        if strokes.len() != ns {
            return Err(DecodeError::Malformed("stroke count mismatch".into()));
        }
        for s in strokes.iter_mut() {
            s.points = decode_point_block(p, &mut r.i)
                .map_err(|e| DecodeError::Malformed(e.to_string()))?;
        }
        ops.push(LocalOperation {
            id,
            device_id,
            local_sequence,
            schema_version,
            causal_dependencies: deps,
            at,
            body,
        });
    }
    if r.i != p.len() {
        return Err(DecodeError::Malformed("trailing bytes in record".into()));
    }
    if ops
        .windows(2)
        .any(|w| w[1].local_sequence != w[0].local_sequence + 1)
    {
        return Err(DecodeError::Malformed("non-consecutive sequences".into()));
    }
    Ok(Txn { target, ops })
}

/// Result of probing for a record at `pos`.
enum Probe {
    Ok(Txn, usize),
    /// Bytes are damaged or incomplete.
    Bad,
    Newer(u16),
}

fn probe(b: &[u8], pos: usize) -> Probe {
    let Some(h) = b.get(pos..pos + RECORD_HEADER_LEN) else {
        return Probe::Bad;
    };
    if &h[..4] != RECORD_MAGIC {
        return Probe::Bad;
    }
    let len = u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize;
    let crc = u32::from_le_bytes(h[8..12].try_into().unwrap());
    if len > MAX_RECORD_BYTES {
        return Probe::Bad;
    }
    let start = pos + RECORD_HEADER_LEN;
    let Some(payload) = b.get(start..start + len) else {
        return Probe::Bad;
    };
    if crc32fast::hash(payload) != crc {
        return Probe::Bad;
    }
    match decode_payload(payload) {
        Ok(t) => Probe::Ok(t, start + len),
        Err(DecodeError::NewerSchema(v)) => Probe::Newer(v),
        // checksum-valid but undecodable: treat as damaged
        Err(DecodeError::Malformed(_)) => Probe::Bad,
    }
}

fn quarantine(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let q = dir.join("quarantine");
    fs::create_dir_all(&q)?;
    let qp = q.join(format!("{name}-{}", now_ms()));
    if !qp.exists() {
        fsutil::write_atomic(&qp, bytes)?;
    }
    Ok(())
}

fn segment_header(first_seq: u64) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(SEGMENT_MAGIC);
    h.extend_from_slice(&JOURNAL_FORMAT_VERSION.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes());
    put_u64(&mut h, first_seq);
    let crc = crc32fast::hash(&h);
    put_u32(&mut h, crc);
    h
}

fn segment_name(first_seq: u64) -> String {
    format!("{first_seq:020}.pjl")
}

impl Journal {
    /// Opens (creating if needed) the journal and returns every committed
    /// transaction in sequence order.
    pub fn open(dir: &Path) -> Result<(Journal, Vec<Txn>, ScanReport), JournalError> {
        fs::create_dir_all(dir)?;
        let mut files: Vec<(u64, PathBuf)> = fs::read_dir(dir)?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_name()?.to_str()?;
                let seq = name.strip_suffix(".pjl")?.parse::<u64>().ok()?;
                Some((seq, p))
            })
            .collect();
        files.sort();
        let mut report = ScanReport::default();
        let mut txns: Vec<Txn> = Vec::new();
        let mut segments = Vec::new();
        let n_files = files.len();
        let mut last_is_damaged = false;
        for (idx, (first_seq, path)) in files.into_iter().enumerate() {
            let is_last = idx + 1 == n_files;
            let bytes = fs::read(&path)?;
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let has_records = || {
                (HEADER_LEN..bytes.len())
                    .filter(|i| bytes[*i..].starts_with(RECORD_MAGIC))
                    .any(|i| matches!(probe(&bytes, i), Probe::Ok(..) | Probe::Newer(_)))
            };
            let header_ok = bytes.len() >= HEADER_LEN
                && bytes[..4] == *SEGMENT_MAGIC
                && crc32fast::hash(&bytes[..16])
                    == u32::from_le_bytes(bytes[16..20].try_into().unwrap());
            let mut damaged = false;
            if !header_ok {
                if is_last && !has_records() {
                    // crash while creating the segment: nothing was committed in it
                    report.truncated_tails += 1;
                    report
                        .warnings
                        .push(format!("journal: removed incomplete segment {name}"));
                    fsutil::remove_durable(&path)?;
                    continue;
                }
                if !has_records() {
                    return Err(JournalError::UnsupportedSegment(name));
                }
                // keep every readable record of a segment with a damaged header
                damaged = true;
                report.corrupt_records += 1;
                report.warnings.push(format!(
                    "journal: segment {name} has a damaged header; readable records were kept"
                ));
                quarantine(dir, &name, &bytes)?;
            } else {
                let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
                let flags = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
                if version != JOURNAL_FORMAT_VERSION || flags != 0 {
                    return Err(JournalError::UnsupportedSegment(format!(
                        "{name} (version {version}, flags {flags:#x})"
                    )));
                }
            }
            let mut pos = HEADER_LEN;
            let mut last_seq = None;
            while pos < bytes.len() {
                match probe(&bytes, pos) {
                    Probe::Ok(t, next) => {
                        last_seq = Some(t.last_seq());
                        txns.push(t);
                        pos = next;
                    }
                    Probe::Newer(v) => return Err(JournalError::NewerSchema(v)),
                    Probe::Bad => {
                        // is there any valid record after this point?
                        let resume = (pos + 1..bytes.len().saturating_sub(RECORD_HEADER_LEN - 1))
                            .filter(|i| bytes[*i..].starts_with(RECORD_MAGIC))
                            .find(|i| matches!(probe(&bytes, *i), Probe::Ok(..) | Probe::Newer(_)));
                        match resume {
                            None if is_last => {
                                report.truncated_tails += 1;
                                report.warnings.push(format!(
                                    "journal: discarded {} bytes of an incomplete record at the end of {name} (crash during write)",
                                    bytes.len() - pos
                                ));
                                fsutil::truncate_durable(&path, pos as u64)?;
                                break;
                            }
                            other => {
                                report.corrupt_records += 1;
                                damaged = true;
                                report.warnings.push(format!(
                                    "journal: damaged data in {name} at byte {pos}; the segment was copied to quarantine/ and all readable records were kept"
                                ));
                                quarantine(dir, &name, &bytes)?;
                                match other {
                                    Some(i) => pos = i,
                                    None => break,
                                }
                            }
                        }
                    }
                }
            }
            if is_last {
                last_is_damaged = damaged;
            }
            segments.push(Segment {
                path,
                first_seq,
                last_seq,
            });
        }
        txns.sort_by_key(Txn::first_seq);
        let mut j = Journal {
            dir: dir.to_path_buf(),
            segments,
            active: None,
        };
        // never append after damage: a damaged last segment stays as it is
        // (and quarantined), and the next append starts a fresh segment
        if !last_is_damaged {
            if let Some(s) = j.segments.last() {
                let f = OpenOptions::new().read(true).append(true).open(&s.path)?;
                let len = f.metadata()?.len();
                j.active = Some((f, len));
            }
        }
        Ok((j, txns, report))
    }

    /// The journal directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Highest sequence present in the journal.
    pub fn max_seq(&self) -> Option<u64> {
        self.segments.iter().filter_map(|s| s.last_seq).max()
    }

    /// Lowest first sequence still retained.
    pub fn min_retained_seq(&self) -> Option<u64> {
        self.segments.first().map(|s| s.first_seq)
    }

    /// Appends one transaction durably. On error nothing was committed (a
    /// partial record is rolled back where possible; otherwise it is a torn
    /// tail that the next open discards).
    pub fn append(&mut self, txn: &Txn) -> Result<u64, JournalError> {
        let rec = encode_txn(txn);
        let need_new = match &self.active {
            None => true,
            Some((_, len)) => *len >= SEGMENT_TARGET_BYTES,
        };
        if need_new {
            // segments are named by their first sequence, so retention can
            // decide what to delete from file names and headers alone
            let first = txn.first_seq();
            let path = self.dir.join(segment_name(first));
            let f = create_durable(&path, &segment_header(first))?;
            self.segments.push(Segment {
                path,
                first_seq: first,
                last_seq: None,
            });
            self.active = Some((f, HEADER_LEN as u64));
        }
        let (f, len) = self.active.as_mut().expect("active segment");
        append_durable(f, *len, &rec)?;
        *len += rec.len() as u64;
        self.segments.last_mut().expect("segment").last_seq = Some(txn.last_seq());
        Ok(rec.len() as u64)
    }

    /// The next append starts a new segment (called once everything in the
    /// active segment is covered by a checkpoint, so it can be deleted later).
    pub fn seal(&mut self) {
        self.active = None;
    }

    /// Whether every record in the newest segment is `<= seq` (then it can
    /// be sealed and later deleted).
    pub fn active_is_covered_by(&self, seq: u64) -> bool {
        self.segments
            .last()
            .is_some_and(|s| s.last_seq.is_none_or(|l| l <= seq))
    }

    /// Deletes sealed segments whose every record is `<= seq`. Never deletes
    /// the active segment.
    pub fn delete_through(&mut self, seq: u64) -> io::Result<u32> {
        let active = self.active.as_ref().map(|_| self.segments.len() - 1);
        let mut deleted = 0;
        let mut keep = Vec::new();
        for (i, s) in std::mem::take(&mut self.segments).into_iter().enumerate() {
            let removable = Some(i) != active && s.last_seq.is_none_or(|l| l <= seq);
            if removable {
                if let Err(e) = fsutil::remove_durable(&s.path) {
                    keep.push(s);
                    self.segments.extend(keep);
                    return Err(e);
                }
                deleted += 1;
            } else {
                keep.push(s);
            }
        }
        self.segments = keep;
        Ok(deleted)
    }

    /// Re-reads every committed transaction (used for fallback recovery and
    /// the outbox). Damaged regions are skipped.
    pub fn read_all(&self) -> Result<Vec<Txn>, JournalError> {
        let mut out = Vec::new();
        for s in &self.segments {
            let bytes = match fs::read(&s.path) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            let mut pos = HEADER_LEN;
            while pos < bytes.len() {
                match probe(&bytes, pos) {
                    Probe::Ok(t, next) => {
                        out.push(t);
                        pos = next;
                    }
                    Probe::Newer(v) => return Err(JournalError::NewerSchema(v)),
                    Probe::Bad => {
                        match (pos + 1..bytes.len())
                            .filter(|i| bytes[*i..].starts_with(RECORD_MAGIC))
                            .find(|i| matches!(probe(&bytes, *i), Probe::Ok(..)))
                        {
                            Some(i) => pos = i,
                            None => break,
                        }
                    }
                }
            }
        }
        out.sort_by_key(Txn::first_seq);
        Ok(out)
    }

    /// Number of segment files.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }
}
