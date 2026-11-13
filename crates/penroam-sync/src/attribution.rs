//! Who wrote each operation: server attribution, stored and checked locally (`history.v1`).
//!
//! The server reports, per operation of a stream, the account whose session pushed it, the device
//! and when it accepted it (`GET /v1/attribution`, `/v1/comments/attribution`; protocol §12i).
//! That is the server's word, not proof. This module:
//!
//! * fetches it page by page from where the last fetch stopped (the server's last
//!   `next_from_seq`, kept in [`STATE_FILE`]) through an [`AttributionTransport`] ([`fetch`]), or
//!   from the start ([`refresh`]);
//! * keeps it durably in an append-only [`AttributionStore`] in the scope's sync folder
//!   (`attribution.jsonl`), keyed by stream and operation id;
//! * classifies each logged operation's author ([`classify`]): the server's `device_id` must equal
//!   the operation's own signing device (verified when it entered the log), and the device must
//!   still belong to the account it was first seen with ([`AttributionStore`] pins it), otherwise
//!   it is `unverified`; operations the server attributes to the space owner that were re-published from
//!   the account log when the notebook was shared (or, on other members' devices, whose own time
//!   is before the space's creation) are `beforeSharing`; erased accounts (`erased-` tombstones)
//!   are `erased`; then current or removed members; anything without attribution is `unknown`.
//!   Only the owner's operations can be `beforeSharing`, because an operation's own time is the
//!   author's claim: a member who backdates one is still shown as its author;
//! * derives what the history commands need: per-element writers since a version
//!   ([`writers_since`]) and attributed operations for the activity list ([`notebook_ops`]).
//!
//! Authors travel to the app as [`AuthorKey`] strings: a verified account id, or a `~`-prefixed
//! marker (`~unverified:<account>`, `~before-sharing`, `~unknown`). `~` never occurs in account
//! ids, so the two can't be confused.
//!
//! Durability: a fetched page is appended and fsynced before the next one is requested, and
//! records are deduplicated on load, so a crash mid-fetch re-fetches at most one page. A torn last
//! line is ignored and overwritten by the next append.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use penroam_core::activity::OpAttribution;
use penroam_core::operation::{DocumentOperation, LocalOperation};
use penroam_storage::restore::WritersSinceBase;
use penroam_sync_protocol::history::{is_erased_account, AttributionEntry, AttributionPage};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::store::OpMeta;
use crate::transport::TResult;

/// File name of the store inside a scope's sync folder.
pub const ATTRIBUTION_FILE: &str = "attribution.jsonl";
/// File name of the store's state next to it, written atomically: the device pins and, per
/// stream, where the next fetch resumes (`{"pins": {device: account}, "next": {stream: seq}}`).
pub const STATE_FILE: &str = "attribution-state.json";

/// The contents of [`STATE_FILE`].
#[derive(Debug, Default, Serialize, Deserialize)]
struct StateDoc {
    #[serde(default)]
    pins: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    next: std::collections::BTreeMap<String, u64>,
}

fn stream_name(s: Stream) -> &'static str {
    match s {
        Stream::Ops => "ops",
        Stream::Comments => "comments",
    }
}
/// Entries requested per page.
pub const FETCH_PAGE: u32 = 1000;
/// Author key of an operation whose server attribution names a device other than its signer.
pub const UNVERIFIED_PREFIX: &str = "~unverified:";
/// Author key of an operation made before the notebook was shared.
pub const BEFORE_SHARING: &str = "~before-sharing";
/// Author key of an operation without attribution.
pub const UNKNOWN: &str = "~unknown";

/// A string naming an author: a verified account id, or one of the `~` markers above.
pub type AuthorKey = String;

/// Which stream of a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    /// The document operation log.
    Ops,
    /// A space's comment stream.
    Comments,
}

/// One stored attribution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributionRecord {
    /// The stream.
    pub stream: Stream,
    /// Position in the stream.
    pub seq: u64,
    /// Operation id, as the server reported it.
    pub op: String,
    /// Account the server attributes it to (possibly an `erased-` tombstone).
    pub account: String,
    /// Device the server says pushed it.
    pub device: String,
    /// When the server accepted it, Unix ms (0 when not recorded).
    pub accepted_at_ms: u64,
    /// Set by the store when `device` was first seen with another account (see
    /// [`AttributionStore`]); [`classify`] then treats the record as unverified. Not stored.
    #[serde(skip)]
    pub pin_mismatch: bool,
}

impl AttributionRecord {
    /// The same attribution, ignoring the store's [`AttributionRecord::pin_mismatch`] flag.
    fn same(&self, other: &Self) -> bool {
        (
            self.stream,
            self.seq,
            &self.op,
            &self.account,
            &self.device,
            self.accepted_at_ms,
        ) == (
            other.stream,
            other.seq,
            &other.op,
            &other.account,
            &other.device,
            other.accepted_at_ms,
        )
    }

    fn from_entry(stream: Stream, e: &AttributionEntry) -> Self {
        Self {
            stream,
            seq: e.server_seq,
            op: e.op_id.to_string(),
            account: e.account_id.to_string(),
            device: e.device_id.to_string(),
            accepted_at_ms: e.accepted_at_ms,
            pin_mismatch: false,
        }
    }
}

/// The scope's attributions, loaded from and appended to `attribution.jsonl`.
///
/// **Device pins.** The first time the store sees a device, it pins it to the account the server
/// named. A later record naming another account for that device is kept but flagged
/// ([`AttributionRecord::pin_mismatch`], shown as unverified) and listed in
/// [`AttributionStore::pin_violations`], so the caller can raise a security event: a device belongs
/// to one member, and a server that moves it to another is lying about one of them. Pins are kept
/// in the state file ([`STATE_FILE`], written atomically whenever one is added or changes) and are
/// never rebuilt from the records, so a lie stays flagged across refreshes, compactions and
/// restarts instead of becoming the pin. The one change accepted is a pin moving to an `erased-`
/// tombstone (from a real account), because erasing an account rewrites its attribution that way;
/// the pin then holds the tombstone, so the erased id leaves the disk too.
pub struct AttributionStore {
    path: PathBuf,
    by_op: HashMap<(Stream, String), AttributionRecord>,
    last_seq: HashMap<Stream, u64>,
    pins: HashMap<String, String>,
    state_path: PathBuf,
    /// Per stream, the server's last `next_from_seq`: where the next fetch resumes.
    next_from: HashMap<Stream, u64>,
    /// Pins or resume points changed since they were last written.
    pins_dirty: bool,
    violations: Vec<PinViolation>,
}

/// A record whose device the store had pinned to another account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinViolation {
    /// The stream.
    pub stream: Stream,
    /// The operation.
    pub op: String,
    /// The device.
    pub device: String,
    /// The account the device was first seen with.
    pub pinned: String,
    /// The account this record names.
    pub claimed: String,
}

impl PinViolation {
    /// A security event line for the sync log (ids only).
    pub fn event(&self) -> String {
        format!(
            "security: the server attributed operation {} to account {} through device {}, which \
             belongs to account {}; shown as unverified",
            self.op, self.claimed, self.device, self.pinned
        )
    }
}

impl AttributionStore {
    /// Opens (or starts) the store in `scope_dir` (the engine's `store.dir`).
    pub fn open(scope_dir: &Path) -> io::Result<Self> {
        let path = scope_dir.join(ATTRIBUTION_FILE);
        let state_path = scope_dir.join(STATE_FILE);
        let doc = match std::fs::read(&state_path) {
            Ok(b) => serde_json::from_slice::<StateDoc>(&b)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StateDoc::default(),
            Err(e) => return Err(e),
        };
        let pins = doc.pins.into_iter().collect();
        let next_from = [Stream::Ops, Stream::Comments]
            .into_iter()
            .filter_map(|st| doc.next.get(stream_name(st)).map(|n| (st, *n)))
            .collect();
        let mut s = Self {
            path,
            by_op: HashMap::new(),
            last_seq: HashMap::new(),
            pins,
            state_path,
            next_from,
            pins_dirty: false,
            violations: Vec::new(),
        };
        match File::open(&s.path) {
            Ok(f) => {
                for line in BufReader::new(f).lines() {
                    let line = line?;
                    // a torn or damaged line is skipped; its page is fetched again
                    if let Ok(r) = serde_json::from_str::<AttributionRecord>(&line) {
                        s.insert(r);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // a store from before the pins file existed pins what it has now
        s.save_pins()?;
        Ok(s)
    }

    /// Writes the state file if a pin or resume point changed.
    fn save_pins(&mut self) -> io::Result<()> {
        if !self.pins_dirty {
            return Ok(());
        }
        let doc = StateDoc {
            pins: self
                .pins
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            next: self
                .next_from
                .iter()
                .map(|(st, n)| (stream_name(*st).to_string(), *n))
                .collect(),
        };
        let bytes = serde_json::to_vec(&doc).map_err(io::Error::other)?;
        penroam_storage::fsutil::write_atomic(&self.state_path, &bytes)?;
        self.pins_dirty = false;
        Ok(())
    }

    /// Where the next [`fetch`] of `stream` starts: the server's last `next_from_seq`, or after
    /// the highest stored sequence if that is further. Operations at the end of the stream that
    /// have no entry are therefore not scanned again on every sync.
    pub fn resume_from(&self, stream: Stream) -> u64 {
        let after_stored = self.last_seq(stream) + 1;
        self.next_from
            .get(&stream)
            .copied()
            .unwrap_or(1)
            .max(after_stored)
    }

    /// Records the server's `next_from_seq` after a page of `stream` (only ever moves forward)
    /// and writes it.
    fn advance(&mut self, stream: Stream, next_from_seq: u64) -> io::Result<()> {
        let n = self.next_from.entry(stream).or_insert(1);
        if next_from_seq > *n {
            *n = next_from_seq;
            self.pins_dirty = true;
        }
        self.save_pins()
    }

    fn insert(&mut self, mut r: AttributionRecord) {
        let last = self.last_seq.entry(r.stream).or_insert(0);
        *last = (*last).max(r.seq);
        match self.pins.get(&r.device) {
            None => {
                self.pins.insert(r.device.clone(), r.account.clone());
                self.pins_dirty = true;
            }
            Some(p) if *p == r.account => {}
            // the pinned account was erased: the pin follows to its tombstone, once
            Some(p) if !is_tombstone(p) && is_tombstone(&r.account) => {
                self.pins.insert(r.device.clone(), r.account.clone());
                self.pins_dirty = true;
            }
            Some(p) => {
                r.pin_mismatch = true;
                self.violations.push(PinViolation {
                    stream: r.stream,
                    op: r.op.clone(),
                    device: r.device.clone(),
                    pinned: p.clone(),
                    claimed: r.account.clone(),
                });
            }
        }
        self.by_op.insert((r.stream, r.op.clone()), r);
    }

    /// Rewrites the file with only the current record of each operation (both streams, in stream
    /// and sequence order), atomically: a temporary file is written and fsynced, then renamed
    /// over the old one. Records that other records replaced, such as an account's attribution
    /// before it was erased, are gone from disk afterwards. The pins are kept as they are: records
    /// that conflict with them stay flagged.
    pub fn compact(&mut self) -> io::Result<()> {
        let mut records: Vec<AttributionRecord> = self.by_op.values().cloned().collect();
        records.sort_by(|a, b| (a.stream as u8, a.seq, &a.op).cmp(&(b.stream as u8, b.seq, &b.op)));
        let mut buf = Vec::new();
        for r in &records {
            serde_json::to_writer(&mut buf, r).map_err(io::Error::other)?;
            buf.push(b'\n');
        }
        penroam_storage::fsutil::write_atomic(&self.path, &buf)?;
        self.by_op.clear();
        for r in records {
            let mut r = r;
            r.pin_mismatch = false;
            self.insert(r);
        }
        self.save_pins()
    }

    /// Records whose device was pinned to another account, oldest first (from this run: the
    /// file's on open, then each append). Raise a security event for each.
    pub fn pin_violations(&self) -> &[PinViolation] {
        &self.violations
    }

    /// The account `device` is pinned to.
    pub fn pinned_account(&self, device: &str) -> Option<&str> {
        self.pins.get(device).map(String::as_str)
    }

    /// Highest sequence stored for `stream` (0 = nothing yet).
    pub fn last_seq(&self, stream: Stream) -> u64 {
        self.last_seq.get(&stream).copied().unwrap_or(0)
    }

    /// The attribution of operation `op` in `stream`.
    pub fn get(&self, stream: Stream, op: Uuid) -> Option<&AttributionRecord> {
        self.by_op.get(&(stream, op.hyphenated().to_string()))
    }

    /// How many attributions are stored.
    pub fn len(&self) -> usize {
        self.by_op.len()
    }

    /// Whether nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.by_op.is_empty()
    }

    /// Appends `records` durably (written and fsynced) and indexes them. Records already stored
    /// unchanged are skipped.
    pub fn append(&mut self, records: Vec<AttributionRecord>) -> io::Result<()> {
        let new: Vec<AttributionRecord> = records
            .into_iter()
            .filter(|r| {
                !self
                    .by_op
                    .get(&(r.stream, r.op.clone()))
                    .is_some_and(|old| old.same(r))
            })
            .collect();
        if new.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::new();
        // start on a fresh line, in case the previous run left a torn one
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > 0) {
            buf.push(b'\n');
        }
        for r in &new {
            serde_json::to_writer(&mut buf, r).map_err(io::Error::other)?;
            buf.push(b'\n');
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(&buf)?;
        f.sync_all()?;
        for r in new {
            self.insert(r);
        }
        self.save_pins()
    }
}

/// The attribution routes, for the transports that sync a scope.
pub trait AttributionTransport {
    /// One page of `stream`'s attribution from `from_seq` (inclusive).
    fn attribution_page(
        &mut self,
        stream: Stream,
        from_seq: u64,
        limit: u32,
    ) -> TResult<AttributionPage>;
}

/// Fetches every attribution after what `store` has, page by page, storing each page before
/// asking for the next. Returns how many entries were received.
pub fn fetch(
    store: &mut AttributionStore,
    transport: &mut dyn AttributionTransport,
    stream: Stream,
) -> TResult<usize> {
    fetch_from(store, transport, stream, store.resume_from(stream))
}

/// Fetches the whole stream again from the start. Entries the server changed since (an erased
/// account's id replaced by its tombstone) replace the stored ones, and the file is then
/// rewritten with only the current records ([`AttributionStore::compact`]), so a replaced
/// account id doesn't linger on disk. Call after a membership change; [`fetch`] is enough
/// otherwise.
pub fn refresh(
    store: &mut AttributionStore,
    transport: &mut dyn AttributionTransport,
    stream: Stream,
) -> TResult<usize> {
    let n = fetch_from(store, transport, stream, 1)?;
    store
        .compact()
        .map_err(|e| crate::transport::TransportError::Protocol(format!("attribution: {e}")))?;
    Ok(n)
}

fn fetch_from(
    store: &mut AttributionStore,
    transport: &mut dyn AttributionTransport,
    stream: Stream,
    from: u64,
) -> TResult<usize> {
    let mut from = from;
    let mut stored = 0;
    loop {
        let page = transport.attribution_page(stream, from, FETCH_PAGE)?;
        let records: Vec<AttributionRecord> = page
            .items
            .iter()
            .map(|e| AttributionRecord::from_entry(stream, e))
            .collect();
        stored += records.len();
        store
            .append(records)
            .and_then(|()| {
                if page.next_from_seq > from {
                    store.advance(stream, page.next_from_seq)
                } else {
                    Ok(())
                }
            })
            .map_err(|e| crate::transport::TransportError::Protocol(format!("attribution: {e}")))?;
        // A page may be empty while `has_more` is set: `limit` bounds the operations the server
        // scanned, and operations whose author it can't establish have no entry. Keep going
        // while `next_from_seq` advances; stop if it doesn't, so a misbehaving server can't make
        // this spin.
        if !page.has_more || page.next_from_seq <= from {
            return Ok(stored);
        }
        from = page.next_from_seq;
    }
}

// ---------------------------------------------------------- classification --

/// A member as the classifier needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    /// What to show (the member's verified email, or empty).
    pub display: String,
    /// Whether the membership is active (else they left or were removed).
    pub active: bool,
}

/// What [`classify`] knows about the scope.
#[derive(Debug, Clone, Default)]
pub struct AuthorContext {
    /// Members by account id (for an ordinary account: the account itself).
    pub members: HashMap<String, MemberInfo>,
    /// Ids of operations re-published from the account log when the notebook was shared
    /// ([`transplanted_ids`]); known on the sharing account's devices.
    pub transplanted: HashSet<Uuid>,
    /// When the space was created (Unix ms). The owner's operations whose own time is earlier
    /// were made before sharing; this is how other members' devices recognize transplanted
    /// history.
    pub shared_at_ms: Option<i64>,
    /// The space's owner. Only operations the server attributes to the owner (from the owner's
    /// verified device) can be "before sharing".
    pub owner: Option<String>,
}

impl AuthorContext {
    /// A context from a space's member list (`GET /v1/shared-notebooks/{id}/members`) and the
    /// space's creation time (`SharedNotebookRecord::created_at_ms`).
    pub fn for_space(
        members: &penroam_sync_protocol::MemberList,
        created_at_ms: Option<u64>,
        transplanted: HashSet<Uuid>,
    ) -> Self {
        Self {
            members: members
                .members
                .iter()
                .map(|m| {
                    (
                        m.account_id.to_string(),
                        MemberInfo {
                            display: m.display.clone(),
                            active: m.state == penroam_sync_protocol::MembershipState::Active,
                        },
                    )
                })
                .collect(),
            transplanted,
            shared_at_ms: created_at_ms.map(|t| t as i64),
            owner: members
                .members
                .iter()
                .find(|m| m.role == penroam_sync_protocol::Role::Owner)
                .map(|m| m.account_id.to_string()),
        }
    }

    /// A context for an ordinary account or vault: the account is the only member.
    pub fn for_account(account: &str, display: &str) -> Self {
        Self {
            members: [(
                account.to_string(),
                MemberInfo {
                    display: display.to_string(),
                    active: true,
                },
            )]
            .into(),
            ..Self::default()
        }
    }
}

fn is_tombstone(account: &str) -> bool {
    account
        .parse()
        .is_ok_and(|id: penroam_sync_protocol::AccountId| is_erased_account(&id))
}

/// The ids the notebook's account operations got when transplanted into `space_id`
/// ([`crate::shared::republished_id`]).
pub fn transplanted_ids(space_id: &str, account_ops: &[Uuid]) -> HashSet<Uuid> {
    account_ops
        .iter()
        .map(|op| crate::shared::republished_id(space_id, *op))
        .collect()
}

/// What kind of author an operation has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthorKind {
    /// A current member (or the account itself).
    Member,
    /// A member who left or was removed.
    Former,
    /// An erased account.
    Erased,
    /// Made before the notebook was shared.
    BeforeSharing,
    /// No attribution, or an account the scope doesn't know.
    Unknown,
    /// The server's device doesn't match the operation's signer.
    Unverified,
}

/// An operation's author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// The account the server named, when there is one.
    pub account: Option<String>,
    /// The display name, for members and former members.
    pub display: String,
    /// The kind.
    pub kind: AuthorKind,
    /// The [`AuthorKey`] for it.
    pub key: AuthorKey,
    /// When the server accepted it, Unix ms.
    pub accepted_at: Option<i64>,
}

/// Classifies `op`'s author from its attribution (see the module docs for the order of rules).
pub fn classify(
    op: &LocalOperation,
    attribution: Option<&AttributionRecord>,
    ctx: &AuthorContext,
) -> Author {
    let accepted_at = attribution
        .filter(|a| a.accepted_at_ms > 0)
        .map(|a| a.accepted_at_ms as i64);
    let make = |account: Option<String>, display: String, kind, key: String| Author {
        account,
        display,
        kind,
        key,
        accepted_at,
    };
    let Some(a) = attribution else {
        return make(None, String::new(), AuthorKind::Unknown, UNKNOWN.into());
    };
    if a.pin_mismatch || a.device != op.device_id.hyphenated().to_string() {
        return make(
            Some(a.account.clone()),
            String::new(),
            AuthorKind::Unverified,
            format!("{UNVERIFIED_PREFIX}{}", a.account),
        );
    }
    // Transplanted history is pushed by the owner's device. An operation's own time comes from
    // its author's clock, so it only counts towards "before sharing" for the owner's operations;
    // anyone else who backdates an operation is still shown as its author.
    let by_owner = ctx.owner.as_deref() == Some(a.account.as_str());
    if by_owner
        && (ctx.transplanted.contains(&op.id) || ctx.shared_at_ms.is_some_and(|t| op.at < t))
    {
        return make(
            None,
            String::new(),
            AuthorKind::BeforeSharing,
            BEFORE_SHARING.into(),
        );
    }
    let erased = a
        .account
        .parse()
        .is_ok_and(|id: penroam_sync_protocol::AccountId| is_erased_account(&id));
    if erased {
        return make(
            Some(a.account.clone()),
            String::new(),
            AuthorKind::Erased,
            a.account.clone(),
        );
    }
    match ctx.members.get(&a.account) {
        Some(m) if m.active => make(
            Some(a.account.clone()),
            m.display.clone(),
            AuthorKind::Member,
            a.account.clone(),
        ),
        Some(m) => make(
            Some(a.account.clone()),
            m.display.clone(),
            AuthorKind::Former,
            a.account.clone(),
        ),
        None => make(
            Some(a.account.clone()),
            String::new(),
            AuthorKind::Unknown,
            a.account.clone(),
        ),
    }
}

/// How an [`AuthorKey`] is shown, given the scope's members.
pub fn describe_key(key: &str, ctx: &AuthorContext) -> (Option<String>, String, AuthorKind) {
    if key == BEFORE_SHARING {
        return (None, String::new(), AuthorKind::BeforeSharing);
    }
    if key == UNKNOWN {
        return (None, String::new(), AuthorKind::Unknown);
    }
    if let Some(acct) = key.strip_prefix(UNVERIFIED_PREFIX) {
        return (Some(acct.into()), String::new(), AuthorKind::Unverified);
    }
    if key
        .parse()
        .is_ok_and(|id: penroam_sync_protocol::AccountId| is_erased_account(&id))
    {
        return (Some(key.into()), String::new(), AuthorKind::Erased);
    }
    match ctx.members.get(key) {
        Some(m) if m.active => (Some(key.into()), m.display.clone(), AuthorKind::Member),
        Some(m) => (Some(key.into()), m.display.clone(), AuthorKind::Former),
        None => (Some(key.into()), String::new(), AuthorKind::Unknown),
    }
}

/// The notebook's logged operations (as [`crate::SyncEngine::logged_ops`] returns them, in log
/// order) with their attribution, for [`penroam_core::activity::summarize`]. The author is an
/// [`AuthorKey`].
pub fn notebook_ops(
    entries: &[(OpMeta, LocalOperation)],
    store: &AttributionStore,
    ctx: &AuthorContext,
) -> Vec<(LocalOperation, OpAttribution)> {
    entries
        .iter()
        .map(|(_, op)| {
            let a = classify(op, store.get(Stream::Ops, op.id), ctx);
            (
                op.clone(),
                OpAttribution {
                    author: Some(a.key),
                    accepted_at: a.accepted_at,
                },
            )
        })
        .collect()
}

/// The element ids an operation on `page` touches.
fn touched(body: &DocumentOperation, page: Uuid) -> Vec<Uuid> {
    use DocumentOperation as D;
    match body {
        D::ElementsAdded {
            page_id, elements, ..
        }
        | D::ElementsReplaced {
            page_id, elements, ..
        } if *page_id == page => elements.iter().map(|e| e.id()).collect(),
        D::ElementsRemoved { page_id, ids, .. } if *page_id == page => ids.clone(),
        D::TextEdited {
            page_id,
            element_id,
            ..
        } if *page_id == page => vec![*element_id],
        _ => Vec::new(),
    }
}

/// For each element of `page` changed after the version made of the notebook's first `position`
/// logged operations (the `position` of `page_versions`), the authors that changed it, as
/// [`AuthorKey`]s. Operations without verified attribution add a `~` key, so the restore planner
/// never treats them as the current user's.
pub fn writers_since(
    entries: &[(OpMeta, LocalOperation)],
    store: &AttributionStore,
    ctx: &AuthorContext,
    page: Uuid,
    position: u64,
) -> WritersSinceBase {
    let mut out: WritersSinceBase = HashMap::new();
    for (_, op) in entries.iter().skip(position as usize) {
        let ids = touched(&op.body, page);
        if ids.is_empty() {
            continue;
        }
        let key = classify(op, store.get(Stream::Ops, op.id), ctx).key;
        for id in ids {
            out.entry(id).or_default().insert(key.clone());
        }
    }
    out
}

// --------------------------------------------------------------- transports --

#[cfg(feature = "testkit")]
mod in_process_impl {
    use super::*;
    use crate::transport::{InProcessTransport, TransportError};
    use penroam_sync_testkit::{Caller, RequestError};

    impl AttributionTransport for InProcessTransport {
        fn attribution_page(
            &mut self,
            stream: Stream,
            from_seq: u64,
            limit: u32,
        ) -> TResult<AttributionPage> {
            if self.offline {
                return Err(TransportError::Network("offline".into()));
            }
            let caller = Caller {
                account: &self.account,
                member: self.member.as_ref(),
            };
            let r = match stream {
                Stream::Ops => self
                    .server
                    .attribution(caller, &self.device, from_seq, limit),
                Stream::Comments => {
                    self.server
                        .comment_attribution(caller, &self.device, from_seq, limit)
                }
            };
            r.map_err(|e| match e {
                RequestError::Api(b) => TransportError::Api(b),
                RequestError::ResponseLost => TransportError::ResponseLost,
            })
        }
    }
}

#[cfg(feature = "http")]
pub use http_impl::HttpAttribution;

#[cfg(feature = "http")]
mod http_impl {
    use super::*;
    use crate::transport::TransportError;
    use penroam_sync_protocol::{routes, ErrorBody, ErrorCode};
    use std::time::Duration;

    /// The attribution routes over HTTP(S), with the base URL and device token of the scope's
    /// [`crate::transport::HttpTransport`]. Refresh the token with [`HttpAttribution::set_token`]
    /// when the transport's changes.
    pub struct HttpAttribution {
        base: String,
        token: String,
        agent: ureq::Agent,
    }

    impl std::fmt::Debug for HttpAttribution {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("HttpAttribution")
                .field("base", &self.base)
                .finish_non_exhaustive()
        }
    }

    impl HttpAttribution {
        /// A client for `base_url` with a device session `token`.
        pub fn new(base_url: &str, token: String) -> Self {
            Self {
                base: base_url.trim_end_matches('/').to_string(),
                token,
                agent: ureq::Agent::config_builder()
                    .timeout_global(Some(Duration::from_secs(60)))
                    .timeout_connect(Some(Duration::from_secs(10)))
                    .http_status_as_error(false)
                    .build()
                    .into(),
            }
        }

        /// Replaces the token.
        pub fn set_token(&mut self, token: String) {
            self.token = token;
        }
    }

    impl AttributionTransport for HttpAttribution {
        fn attribution_page(
            &mut self,
            stream: Stream,
            from_seq: u64,
            limit: u32,
        ) -> TResult<AttributionPage> {
            let route = match stream {
                Stream::Ops => routes::ATTRIBUTION,
                Stream::Comments => routes::COMMENT_ATTRIBUTION,
            };
            let url = format!("{}{route}?from_seq={from_seq}&limit={limit}", self.base);
            let mut r = self
                .agent
                .get(url)
                .header("Authorization", format!("Bearer {}", self.token))
                .call()
                .map_err(|e| TransportError::Network(e.to_string()))?;
            let status = r.status().as_u16();
            let body = r
                .body_mut()
                .with_config()
                .limit(8 << 20)
                .read_to_vec()
                .map_err(|_| TransportError::ResponseLost)?;
            if status >= 400 {
                return Err(match serde_json::from_slice::<ErrorBody>(&body) {
                    Ok(b) => TransportError::Api(b),
                    Err(_) if status >= 500 => TransportError::Api(ErrorBody::new(
                        ErrorCode::ServiceUnavailable,
                        format!("HTTP {status}"),
                        "",
                    )),
                    Err(_) => TransportError::Protocol(format!("HTTP {status}")),
                });
            }
            serde_json::from_slice(&body).map_err(|e| TransportError::Protocol(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penroam_sync_protocol::history::AttributionEntry;

    /// Serves scripted pages and records the `from_seq` of each request.
    struct Script {
        pages: Vec<AttributionPage>,
        asked: Vec<u64>,
    }

    impl AttributionTransport for Script {
        fn attribution_page(
            &mut self,
            _: Stream,
            from_seq: u64,
            _: u32,
        ) -> TResult<AttributionPage> {
            self.asked.push(from_seq);
            Ok(if self.pages.is_empty() {
                AttributionPage {
                    items: vec![],
                    next_from_seq: from_seq,
                    has_more: false,
                }
            } else {
                self.pages.remove(0)
            })
        }
    }

    fn entry(seq: u64) -> AttributionEntry {
        AttributionEntry {
            server_seq: seq,
            op_id: Uuid::from_u128(seq.into()).to_string().parse().unwrap(),
            account_id: "acct-a".parse().unwrap(),
            device_id: "dev-a".parse().unwrap(),
            accepted_at_ms: 1,
        }
    }

    fn page(items: Vec<AttributionEntry>, next: u64, more: bool) -> AttributionPage {
        AttributionPage {
            items,
            next_from_seq: next,
            has_more: more,
        }
    }

    #[test]
    fn empty_pages_with_more_keep_the_fetch_going() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AttributionStore::open(dir.path()).unwrap();
        // Ops 1–4 have no established author; 5 has; 6–9 none; 10 has.
        let mut t = Script {
            pages: vec![
                page(vec![], 5, true),
                page(vec![entry(5)], 7, true),
                page(vec![], 10, true),
                page(vec![entry(10)], 11, false),
            ],
            asked: vec![],
        };
        assert_eq!(fetch(&mut store, &mut t, Stream::Ops).unwrap(), 2);
        assert_eq!(t.asked, vec![1, 5, 7, 10]);
        assert_eq!(store.last_seq(Stream::Ops), 10);
        assert!(store.get(Stream::Ops, Uuid::from_u128(5)).is_some());
    }

    fn record(seq: u64, account: &str, device: &str) -> AttributionEntry {
        AttributionEntry {
            account_id: account.parse().unwrap(),
            device_id: device.parse().unwrap(),
            ..entry(seq)
        }
    }

    #[test]
    fn a_re_attributed_device_stays_unverified_through_refresh_compaction_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AttributionStore::open(dir.path()).unwrap();
        // The device is first seen with acct-ben.
        let mut t = Script {
            pages: vec![page(
                vec![
                    record(1, "acct-ben", "dev-b"),
                    record(2, "acct-ben", "dev-b"),
                ],
                3,
                false,
            )],
            asked: vec![],
        };
        fetch(&mut store, &mut t, Stream::Ops).unwrap();
        assert_eq!(store.pinned_account("dev-b"), Some("acct-ben"));
        // Later the server claims every op of that device for acct-ann; a refresh replaces the
        // records and compacts the file, so no acct-ben record remains.
        let mut t = Script {
            pages: vec![page(
                vec![
                    record(1, "acct-ann", "dev-b"),
                    record(2, "acct-ann", "dev-b"),
                ],
                3,
                false,
            )],
            asked: vec![],
        };
        refresh(&mut store, &mut t, Stream::Ops).unwrap();
        let flagged = |s: &AttributionStore| {
            [1u128, 2].iter().all(|op| {
                s.get(Stream::Ops, Uuid::from_u128(*op))
                    .unwrap()
                    .pin_mismatch
            })
        };
        assert!(flagged(&store), "still flagged after the compaction");
        assert_eq!(
            store.pinned_account("dev-b"),
            Some("acct-ben"),
            "the lie didn't become the pin"
        );
        let file = std::fs::read_to_string(dir.path().join(ATTRIBUTION_FILE)).unwrap();
        assert!(!file.contains("acct-ben"));
        // And after a restart.
        let again = AttributionStore::open(dir.path()).unwrap();
        assert!(flagged(&again));
        assert_eq!(again.pinned_account("dev-b"), Some("acct-ben"));
        assert_eq!(again.pin_violations().len(), 2);
        // Another refresh doesn't change that either.
        let mut again = again;
        let mut t = Script {
            pages: vec![page(
                vec![
                    record(1, "acct-ann", "dev-b"),
                    record(2, "acct-ann", "dev-b"),
                ],
                3,
                false,
            )],
            asked: vec![],
        };
        refresh(&mut again, &mut t, Stream::Ops).unwrap();
        assert!(flagged(&again));
    }

    #[test]
    fn a_pin_follows_its_account_to_a_tombstone_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AttributionStore::open(dir.path()).unwrap();
        let tomb = format!("erased-{}", "ab".repeat(16));
        let other = format!("erased-{}", "cd".repeat(16));
        let mut t = Script {
            pages: vec![page(vec![record(1, "acct-ben", "dev-b")], 2, false)],
            asked: vec![],
        };
        fetch(&mut store, &mut t, Stream::Ops).unwrap();
        let mut t = Script {
            pages: vec![page(vec![record(1, &tomb, "dev-b")], 2, false)],
            asked: vec![],
        };
        refresh(&mut store, &mut t, Stream::Ops).unwrap();
        assert!(store.pin_violations().is_empty());
        assert_eq!(store.pinned_account("dev-b"), Some(tomb.as_str()));
        let pins = std::fs::read_to_string(dir.path().join(STATE_FILE)).unwrap();
        assert!(
            !pins.contains("acct-ben"),
            "the erased id left the pins file too"
        );
        // From the tombstone, neither a real account nor another tombstone is accepted.
        for liar in ["acct-ann", other.as_str()] {
            let mut t = Script {
                pages: vec![page(vec![record(1, liar, "dev-b")], 2, false)],
                asked: vec![],
            };
            refresh(&mut store, &mut t, Stream::Ops).unwrap();
            assert!(
                store
                    .get(Stream::Ops, Uuid::from_u128(1))
                    .unwrap()
                    .pin_mismatch,
                "{liar}"
            );
        }
    }

    #[test]
    fn fetches_resume_from_the_servers_next_from_seq() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AttributionStore::open(dir.path()).unwrap();
        // Op 1 has an author; 2–9 (the end of the stream) don't.
        let mut t = Script {
            pages: vec![page(vec![entry(1)], 10, false)],
            asked: vec![],
        };
        fetch(&mut store, &mut t, Stream::Ops).unwrap();
        assert_eq!(store.last_seq(Stream::Ops), 1);
        assert_eq!(store.resume_from(Stream::Ops), 10);
        // The next sync, also after a restart, doesn't scan 2–9 again.
        let mut store = AttributionStore::open(dir.path()).unwrap();
        assert_eq!(store.resume_from(Stream::Ops), 10);
        let mut t = Script {
            pages: vec![page(vec![entry(10)], 11, false)],
            asked: vec![],
        };
        fetch(&mut store, &mut t, Stream::Ops).unwrap();
        assert_eq!(t.asked, vec![10]);
        // Streams resume separately.
        assert_eq!(store.resume_from(Stream::Comments), 1);
        // A refresh still starts from the beginning.
        let mut t = Script {
            pages: vec![page(vec![entry(1), entry(10)], 11, false)],
            asked: vec![],
        };
        refresh(&mut store, &mut t, Stream::Ops).unwrap();
        assert_eq!(t.asked, vec![1]);
        // A server's next_from_seq never moves the resume point backwards.
        let mut t = Script {
            pages: vec![page(vec![], 3, false)],
            asked: vec![],
        };
        fetch(&mut store, &mut t, Stream::Ops).unwrap();
        assert_eq!(store.resume_from(Stream::Ops), 11);
    }

    #[test]
    fn a_page_that_doesnt_advance_stops_the_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = AttributionStore::open(dir.path()).unwrap();
        let mut t = Script {
            pages: vec![
                page(vec![entry(1)], 2, true),
                // claims more but points back at itself
                page(vec![], 2, true),
                page(vec![entry(3)], 4, false),
            ],
            asked: vec![],
        };
        assert_eq!(fetch(&mut store, &mut t, Stream::Ops).unwrap(), 1);
        assert_eq!(t.asked, vec![1, 2], "no spin");
        // and one that points backwards
        let mut t = Script {
            pages: vec![page(vec![], 1, true)],
            asked: vec![],
        };
        assert_eq!(fetch(&mut store, &mut t, Stream::Ops).unwrap(), 0);
        assert_eq!(t.asked, vec![2]);
    }
}
