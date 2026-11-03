//! The single-writer sync engine.
//!
//! One reconcile has three phases so that the library lock is never held
//! during network I/O (pen input and saving never wait for the network):
//!
//! 1. [`SyncEngine::prepare`] (library locked): seed notebooks the first time,
//!    copy new local journal operations into the sync operation log, collect
//!    asset bytes that must be uploaded.
//! 2. [`SyncEngine::exchange`] (no lock): handshake, refresh and verify the key
//!    log, upload blobs, push encrypted envelopes (per-op acks), pull pages,
//!    verify signatures and AEAD *before* anything is applied, stage or deliver
//!    operations causally, persist the cursor only after that is durable.
//! 3. [`SyncEngine::apply`] (library locked): copy local operations made in the
//!    meantime, merge every notebook with new remote operations
//!    ([`Replica::apply_delivered`]) and install the result into the library.
//!
//! Local operations are re-sequenced into the sync device's contiguous
//! sequence and depend on the full version vector the library view had
//! absorbed (`installed`), so concurrency is decided correctly everywhere.
//!
//! ## Why re-sequence local operations
//!
//! The library journal numbers operations per *library* device and records
//! only the previous local operation as a dependency. Merging needs more: each
//! operation must say which remote operations its author had already seen, or
//! an edit made after receiving a remote change would look concurrent with it
//! (and, say, a rename would conflict with the rename it replaced). The engine
//! therefore copies each journal operation into the operation log as an
//! operation of the *sync device*, with `causal_dependencies` set to the
//! version vector that the library view reflected when the user made it. That
//! vector is exactly [`SyncState::installed`](crate::store::SyncState::installed),
//! because every local operation ingested in a reconcile was made after the
//! previous install and before the next one.
//!
//! ## Causal delivery
//!
//! Remote envelopes arrive in server order, which is not causal order. An
//! envelope is *delivered* (appended to the operation log) only when the
//! writer's previous sequence and every dependency are already in the log;
//! otherwise it is parked under `pending` and retried after each page. This
//! makes delivery causal across the whole account, which is what
//! [`Replica::apply_delivered`] requires: per notebook, device sequences may
//! have gaps (the device also wrote to other notebooks), so the replica cannot
//! check contiguity itself.
//!
//! ## Durability and at-least-once delivery
//!
//! * Local: an operation is only released from the library journal
//!   (`Library::mark_uploaded`) after it is durable in the operation log.
//! * Upload: per-operation acks; a retried envelope is byte-identical (derived
//!   nonces), so the server reports `duplicate` instead of a conflict.
//! * Download: the cursor is saved only after every envelope of the page is in
//!   the operation log or parked, both fsynced. A crash before that just
//!   redelivers the page, and redelivered operations are deduplicated by id.
//! * Install: notebooks with delivered remote operations are recorded as
//!   dirty in the state and installed (again) after a restart.
//!
//! Everything a remote envelope claims is verified before use: signer in the
//! verified key log, Ed25519 signature, ciphertext hash, AEAD, and the
//! decrypted operation matching its signed header. Failures are quarantined
//! and never applied. See `docs/protocol/sync-protocol-v1.md` §13–§15 and
//! `docs/security/threat-model.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use penroam_core::operation::{DocumentOperation, LocalOperation, OperationRef};
use penroam_core::replica::{Conflict, ConflictTarget, Replica};
use penroam_core::{Element, OpKind, OperationBatch, PageOperation};
use penroam_storage::journal::{decode_txn, encode_txn, RecordError, Txn, TxnTarget};
use penroam_storage::{Library, StorageError};
use penroam_sync_protocol::framing::{encode_batch, FrameItem};
use penroam_sync_protocol::version::{negotiate, ProtocolRange, ENVELOPE_VERSION};
use penroam_sync_protocol::{
    base64url, canonical_aad_bytes, canonical_checkpoint_bytes, canonical_signing_bytes,
    verify_ciphertext, AckRequest, AckResult, BlobId, CheckpointRecord, Cursor, DeviceId,
    DeviceRegistrationRequest, EnvelopeHeader, ErrorCode, KeyLogAppendRequest, Limits,
    NotebookRoutingId, OpId, OpRef, PullOpsRequest, RetryPolicy,
};
use serde::Serialize;
use uuid::Uuid;

use crate::crypto::{self, CryptoError, DeviceKeys, EpochKey, RootSecret};
use crate::keylog::{
    self, DeviceCert, EntryKind, KeyLogError, Membership, Pin, ACCOUNT_SIGNER, RECOVERY,
};
use crate::secrets::SecretStore;
use crate::store::{OpMeta, Origin, Parked, SyncState, SyncStore};
use crate::transport::{SyncTransport, TransportError};

const DEVICE_SECRET: &str = "sync-device-keys";
const PULL_PAGE: u32 = 200;
/// Publish a checkpoint after the operation log grew by this many entries.
const CHECKPOINT_EVERY: u64 = 500;
/// Associated data of checkpoint blobs.
const CHECKPOINT_AAD: &[u8] = b"penroam/checkpoint-blob/v1";
/// Payload size above which ink operations are split (well below the
/// default 1 MiB per-op limit after encryption overhead).
const SPLIT_ABOVE: usize = 512 * 1024;

/// Everything a sync call can fail with. `Display` texts are written for the
/// user (no secrets, no note content).
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The network or the server (see [`TransportError::is_retryable`]).
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The library refused or failed an operation.
    #[error("local storage: {0}")]
    Storage(#[from] StorageError),
    /// Reading or writing the sync folder or the secret store failed.
    #[error("sync files: {0}")]
    Io(#[from] std::io::Error),
    /// The key log failed verification (treated as an integrity alert).
    #[error(transparent)]
    KeyLog(#[from] KeyLogError),
    /// A cryptographic operation failed (for example a bad recovery code).
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// Account setup was attempted on an account whose key log is not empty.
    #[error("this account already has encryption keys; add this device by pairing or with the recovery code")]
    AccountHasKeys,
    /// Pairing or recovery was attempted on an account with no key log yet.
    #[error("this account has no encryption keys yet; set it up on your first device")]
    AccountHasNoKeys,
    /// This device has no epoch keys yet (pairing not approved).
    #[error("this device is waiting for approval from a trusted device")]
    NotEnrolled,
    /// This device is no longer an active member; nothing more is synced.
    #[error("this device was removed from the account; local notes are kept")]
    Revoked,
    /// The server or the data needs a newer Penroam; nothing is lost meanwhile.
    #[error("an update is required: {0}")]
    UpgradeRequired(String),
    /// Local sync data or server responses are inconsistent in a way that must
    /// not be papered over (for example a reused operation id).
    #[error("integrity problem: {0}")]
    Integrity(String),
    /// A request that cannot work as asked (bad code, wrong state).
    #[error("{0}")]
    Invalid(String),
}

impl SyncError {
    /// Whether retrying later (with backoff) may succeed without user action.
    pub fn is_retryable(&self) -> bool {
        matches!(self, SyncError::Transport(t) if t.is_retryable())
    }
}

/// Result of a sync call.
pub type Result<T> = std::result::Result<T, SyncError>;

/// What the status indicator shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Set up by pairing; waiting for a trusted device to approve it.
    AwaitingApproval,
    /// Nothing in progress; the last reconcile succeeded.
    Idle,
    /// A reconcile is running.
    Syncing,
    /// The server could not be reached; changes stay queued.
    Offline,
    /// A retryable server error; the next attempt is scheduled with backoff.
    Backoff,
    /// Paused by the user.
    Paused,
    /// The session token was rejected; sign in again.
    NeedsReauth,
    /// This device was removed from the account (local notes are kept).
    Revoked,
    /// Data or server need a newer Penroam.
    NeedsUpgrade,
    /// The server refuses new data until space is freed (changes stay queued).
    QuotaExceeded,
    /// The server no longer has the changes after this device's cursor; a
    /// snapshot rebase is needed (not implemented yet). Uploads continue.
    RebaseRequired,
    /// Something failed verification (signature, tag, key log, reused id).
    IntegrityAlert,
    /// Any other failure; see `last_error`.
    Error,
}

/// A snapshot of sync state for the UI ([`SyncEngine::status`]).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    /// What is happening now.
    pub phase: Phase,
    /// Local changes the server has not acknowledged yet, including journal
    /// operations not yet copied into the sync log.
    pub queued: u64,
    /// Remote operations received but not applied yet (waiting for
    /// dependencies, keys or an update).
    pub pending_remote: usize,
    /// Envelopes that failed verification and were set aside.
    pub quarantined: usize,
    /// Unresolved conflicts in notebooks merged since the engine started.
    pub conflicts: usize,
    /// True only when nothing is queued, everything received is applied and
    /// the last pull reached the end of the log.
    pub up_to_date: bool,
    /// Wall-clock time of the last successful reconcile.
    pub last_success_ms: Option<i64>,
    /// User-facing text of the last failure, if the last reconcile failed.
    pub last_error: Option<String>,
    /// This sync device's id.
    pub device_id: String,
    /// The account id.
    pub account_id: String,
    /// The server base URL.
    pub server_url: String,
    /// Where the device keys are kept ([`SecretStore::kind`]).
    pub secret_store: String,
    /// E2EE has not been independently reviewed.
    pub security_review_pending: bool,
}

/// A device as shown in the device list (from the verified key log).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    /// Sync device id.
    pub device_id: String,
    /// User-chosen label from its certificate.
    pub label: String,
    /// Key fingerprint (compare with the device's own screen).
    pub fingerprint: String,
    /// False once removed.
    pub active: bool,
    /// Whether this is the device running the engine.
    pub this_device: bool,
    /// Who added it: a device id, or `"account"` (recovery code / first device).
    pub added_by: String,
}

/// Where a new sync connection points.
#[derive(Debug, Clone)]
pub struct Setup {
    /// Root of the library to sync (the sync folder is created inside it).
    pub library_root: PathBuf,
    /// Server base URL (stored for display and reconnects).
    pub server_url: String,
    /// Account id on that server.
    pub account_id: String,
    /// Device label other devices will see (inside the encrypted key log).
    pub label: String,
}

/// What one reconcile did (diagnostics, UI refresh hints).
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileReport {
    /// Local operations copied into the sync log (including seeded ones).
    pub ingested: usize,
    /// Envelopes acknowledged by the server (accepted or duplicate).
    pub pushed: usize,
    /// Envelopes received.
    pub pulled: usize,
    /// Remote operations appended to the sync log.
    pub delivered: usize,
    /// Notebooks whose merged state was installed into the library.
    pub installed_notebooks: Vec<Uuid>,
    /// `(notebook, page)` pairs whose content was reinstalled (open editors
    /// should reload these).
    pub installed_pages: Vec<(Uuid, Uuid)>,
    /// Envelopes quarantined during this reconcile.
    pub quarantined: usize,
}

/// The sync engine for one library and one sync device. Single writer: run
/// one reconcile at a time (the app does so from one worker thread).
///
/// Create it with [`SyncEngine::create_account`] (first device),
/// [`SyncEngine::begin_pairing`] / [`SyncEngine::enroll_with_recovery`]
/// (additional devices) or [`SyncEngine::open`] (existing setup), then call
/// [`SyncEngine::reconcile`] repeatedly.
pub struct SyncEngine<T: SyncTransport> {
    /// Durable sync state and operation log.
    pub store: SyncStore,
    transport: T,
    secrets: Box<dyn SecretStore>,
    keys: DeviceKeys,
    /// `keys.device_id` as a UUID (operations carry device ids as UUIDs).
    my_uuid: Uuid,
    /// Verified key log state; `None` until first read.
    membership: Option<Membership>,
    /// Raw key-log entries fetched so far (re-verified as a whole each time).
    keylog_raw: Vec<Vec<u8>>,
    /// Every epoch key granted to this device.
    epoch_keys: BTreeMap<u32, EpochKey>,
    /// Per-notebook merge state and how many of the notebook's log entries
    /// it has applied (cache; rebuilt from the operation log when missing).
    replicas: HashMap<Uuid, (Replica, usize)>,
    /// Limits advertised by the server (defaults until the first handshake).
    limits: Limits,
    /// Assets to upload in this reconcile: `(notebook, sha256, bytes)`,
    /// collected under the library lock in `prepare`.
    outgoing_blobs: Vec<(Uuid, String, Vec<u8>)>,
    /// Assets the library references but does not hold yet. Rebuilt from the
    /// library in every `prepare` (so it survives restarts), filled by
    /// delivered `AssetReferenced` operations, downloaded into the blob cache
    /// during `exchange`, and cleared only once `apply` installs the bytes.
    missing_blobs: BTreeSet<(Uuid, String)>,
    /// Op ids currently parked in `pending/` (loaded lazily, kept in step
    /// with park/unpark) so admitting an envelope does not re-read the folder.
    pending_ids: Option<std::collections::HashSet<String>>,
    /// The last pull reached the end of the server log (`has_more == false`).
    pull_done: bool,
    /// Cursor to acknowledge at the next exchange.
    ack_due: Option<Cursor>,
    phase: Phase,
    last_error: Option<String>,
    /// Unresolved conflict counts per notebook, from the last merge.
    conflicts: HashMap<Uuid, usize>,
    /// Consecutive retryable failures (drives the backoff delay).
    pub attempt: u32,
    /// Privacy-safe diagnostic events (no content, no keys, no tokens).
    pub events: Vec<String>,
}

/// Wall clock in ms (display only; never used for ordering).
fn now_ms() -> i64 {
    penroam_core::now_ms()
}

/// Parses a UUID only in canonical hyphenated lowercase form, so the same id
/// cannot be written two ways (that would defeat comparisons against signed
/// headers).
fn parse_uuid(s: &str) -> Option<Uuid> {
    Uuid::parse_str(s)
        .ok()
        .filter(|u| u.hyphenated().to_string() == s)
}

/// A protocol [`DeviceId`] from a string.
fn did(s: &str) -> Result<DeviceId> {
    DeviceId::new(s).map_err(|e| SyncError::Invalid(e.to_string()))
}

impl<T: SyncTransport> SyncEngine<T> {
    // ------------------------------------------------------------ setup --

    /// Generates this device's keys and stores them in the secret store first,
    /// so a crash right after setup never leaves a registered device whose
    /// private keys are lost.
    fn new_device(setup: &Setup, secrets: &dyn SecretStore) -> Result<DeviceKeys> {
        let keys = DeviceKeys::generate(Uuid::new_v4().hyphenated().to_string())?;
        secrets.set(DEVICE_SECRET, &keys.to_secret_string())?;
        let _ = setup;
        Ok(keys)
    }

    /// Registers the public keys with the server under the client-chosen id.
    /// The server-visible label is deliberately generic; the real label only
    /// appears inside the key log.
    fn register(transport: &mut T, keys: &DeviceKeys, label: &str) -> Result<()> {
        transport.register_device(&DeviceRegistrationRequest {
            device_id: Some(did(&keys.device_id)?),
            signing_public_key: base64url::encode(&keys.sign_pk()),
            encryption_public_key: base64url::encode(&keys.dh_pk()),
            // the server-visible label is deliberately generic
            label: Some("Penroam device".into()),
        })?;
        let _ = label;
        Ok(())
    }

    /// Builds the engine from its parts (common tail of every constructor).
    fn assemble(
        store: SyncStore,
        transport: T,
        secrets: Box<dyn SecretStore>,
        keys: DeviceKeys,
    ) -> Self {
        let my_uuid = parse_uuid(&keys.device_id).unwrap_or_default();
        let phase = if store.state.enrolled {
            Phase::Idle
        } else {
            Phase::AwaitingApproval
        };
        Self {
            store,
            transport,
            secrets,
            keys,
            my_uuid,
            membership: None,
            keylog_raw: Vec::new(),
            epoch_keys: BTreeMap::new(),
            replicas: HashMap::new(),
            limits: Limits::DEFAULT,
            outgoing_blobs: Vec::new(),
            missing_blobs: BTreeSet::new(),
            pending_ids: None,
            pull_done: false,
            ack_due: None,
            phase,
            last_error: None,
            conflicts: HashMap::new(),
            attempt: 0,
            events: Vec::new(),
        }
    }

    /// First device of an account: creates the account keys. Returns the
    /// engine and the recovery code, which must be shown to the user once and
    /// is not stored anywhere.
    pub fn create_account(
        setup: Setup,
        mut transport: T,
        secrets: Box<dyn SecretStore>,
    ) -> Result<(Self, String)> {
        if SyncStore::exists(&setup.library_root) {
            return Err(SyncError::Invalid(
                "sync is already set up for this library".into(),
            ));
        }
        if transport.keylog_get(0)?.next_index != 0 {
            return Err(SyncError::AccountHasKeys);
        }
        let keys = Self::new_device(&setup, secrets.as_ref())?;
        Self::register(&mut transport, &keys, &setup.label)?;
        let root = RootSecret::generate()?;
        let rec_pk = x25519_dalek::PublicKey::from(&root.recovery_dh()).to_bytes();
        let k1 = EpochKey::generate()?;
        let entry = keylog::make_entry(
            0,
            "",
            EntryKind::Genesis {
                account_pk: base64url::encode(
                    &root.account_signing_key().verifying_key().to_bytes(),
                ),
                recovery_pk: base64url::encode(&rec_pk),
                device: DeviceCert::of(&keys, &setup.label),
                grants: vec![
                    keylog::grant(1, &k1, &keys.device_id, &keys.dh_pk())?,
                    keylog::grant(1, &k1, RECOVERY, &rec_pk)?,
                ],
            },
            ACCOUNT_SIGNER,
            &root.account_signing_key(),
        );
        Self::append_entry(&mut transport, 0, &entry)?;
        let mut state = SyncState::new(
            &setup.server_url,
            &setup.account_id,
            &keys.device_id,
            &setup.label,
        );
        state.enrolled = true;
        let store = SyncStore::create(&setup.library_root, state)?;
        let mut e = Self::assemble(store, transport, secrets, keys);
        e.refresh_keylog()?;
        e.event("account created; this device is the first member");
        Ok((e, root.recovery_code()))
    }

    /// Appends a signed key-log entry at `index` (fails with `conflict` if the
    /// log grew in the meantime; callers re-read and retry).
    fn append_entry(transport: &mut T, index: u64, entry: &[u8]) -> Result<()> {
        transport.keylog_append(&KeyLogAppendRequest {
            index,
            data: base64url::encode(entry),
        })?;
        Ok(())
    }

    /// Adds this device with the recovery code (no other device needed).
    pub fn enroll_with_recovery(
        setup: Setup,
        mut transport: T,
        secrets: Box<dyn SecretStore>,
        code: &str,
    ) -> Result<Self> {
        if SyncStore::exists(&setup.library_root) {
            return Err(SyncError::Invalid(
                "sync is already set up for this library".into(),
            ));
        }
        let root = RootSecret::parse_recovery_code(code)?;
        let acct = root.account_signing_key().verifying_key().to_bytes();
        let raw = Self::fetch_keylog(&mut transport, Vec::new())?;
        if raw.is_empty() {
            return Err(SyncError::AccountHasNoKeys);
        }
        let m = keylog::verify_log(&raw, Some(&acct), None)?;
        let all = m.epoch_keys_with_recovery(&root.recovery_dh());
        let keys = Self::new_device(&setup, secrets.as_ref())?;
        Self::register(&mut transport, &keys, &setup.label)?;
        let mut grants = Vec::new();
        for (epoch, k) in &all {
            grants.push(keylog::grant(*epoch, k, &keys.device_id, &keys.dh_pk())?);
        }
        let entry = keylog::make_entry(
            m.len,
            &m.head,
            EntryKind::AddDevice {
                device: DeviceCert::of(&keys, &setup.label),
                grants,
            },
            ACCOUNT_SIGNER,
            &root.account_signing_key(),
        );
        Self::append_entry(&mut transport, m.len, &entry)?;
        let mut state = SyncState::new(
            &setup.server_url,
            &setup.account_id,
            &keys.device_id,
            &setup.label,
        );
        state.enrolled = true;
        state.pin = Some(Pin {
            account_pk: base64url::encode(&acct),
            len: m.len,
            head: m.head.clone(),
        });
        let store = SyncStore::create(&setup.library_root, state)?;
        let mut e = Self::assemble(store, transport, secrets, keys);
        e.refresh_keylog()?;
        e.event("device added with the recovery code");
        Ok(e)
    }

    /// New device, pairing flow step 1: registers keys and returns the code
    /// to show on this screen. A trusted device approves it with
    /// [`SyncEngine::approve_device`]; then [`SyncEngine::poll_enrollment`].
    pub fn begin_pairing(
        setup: Setup,
        mut transport: T,
        secrets: Box<dyn SecretStore>,
    ) -> Result<(Self, String)> {
        if SyncStore::exists(&setup.library_root) {
            return Err(SyncError::Invalid(
                "sync is already set up for this library".into(),
            ));
        }
        if transport.keylog_get(0)?.next_index == 0 {
            return Err(SyncError::AccountHasNoKeys);
        }
        let keys = Self::new_device(&setup, secrets.as_ref())?;
        Self::register(&mut transport, &keys, &setup.label)?;
        let state = SyncState::new(
            &setup.server_url,
            &setup.account_id,
            &keys.device_id,
            &setup.label,
        );
        let store = SyncStore::create(&setup.library_root, state)?;
        let code = pairing_code(&keys.device_id, &keys.fingerprint());
        let mut e = Self::assemble(store, transport, secrets, keys);
        e.event("pairing started; waiting for approval");
        Ok((e, code))
    }

    /// Opens an existing sync setup.
    pub fn open(library_root: &Path, transport: T, secrets: Box<dyn SecretStore>) -> Result<Self> {
        let mut store = SyncStore::open(library_root)?;
        let raw = secrets.get(DEVICE_SECRET)?.ok_or_else(|| {
            SyncError::Invalid("this device's sync keys are missing from the secret store".into())
        })?;
        let keys = DeviceKeys::from_secret_string(&raw)?;
        if keys.device_id != store.state.device_id {
            return Err(SyncError::Invalid(
                "the stored device keys belong to another sync setup".into(),
            ));
        }
        // repair counters after a crash between an oplog append and a state save
        let my = parse_uuid(&keys.device_id).unwrap_or_default();
        let max_seq = store.oplog.frontier.get(&my).copied().unwrap_or(0);
        let max_lib = store
            .oplog
            .entries()
            .iter()
            .filter(|e| e.meta.origin == Origin::Local)
            .filter_map(|e| e.meta.lib)
            .max()
            .unwrap_or(0);
        if max_seq > store.state.next_seq || max_lib > store.state.ingested_lib_seq {
            store.state.next_seq = store.state.next_seq.max(max_seq);
            store.state.ingested_lib_seq = store.state.ingested_lib_seq.max(max_lib);
            store.save()?;
        }
        if let Some(d) = &store.oplog.damaged {
            return Err(SyncError::Integrity(format!("sync operation log: {d}")));
        }
        Ok(Self::assemble(store, transport, secrets, keys))
    }

    /// New device, pairing flow step 3: checks whether a trusted device has
    /// certified this device. Returns the short authentication string to
    /// compare with the approving screen once enrolled.
    pub fn poll_enrollment(&mut self) -> Result<Option<String>> {
        self.refresh_keylog()?;
        let m = self.membership.as_ref().expect("refreshed");
        let Some(me) = m.devices.get(&self.keys.device_id) else {
            return Ok(None);
        };
        if me.cert.sign_pk()? != self.keys.sign_pk() || me.cert.dh_pk()? != self.keys.dh_pk() {
            return Err(SyncError::Integrity(
                "the key log certifies different keys for this device id".into(),
            ));
        }
        if !me.active {
            return Err(SyncError::Revoked);
        }
        let sas = crypto::short_auth_string(&m.account_pk, &self.keys.fingerprint());
        if !self.store.state.enrolled {
            self.store.state.enrolled = true;
            self.store.save()?;
            self.phase = Phase::Idle;
            self.event("this device was approved");
        }
        Ok(Some(sas))
    }

    /// Trusted device: approves a device that shows `code`. The registered
    /// keys must match the code's fingerprint (so the server cannot swap
    /// keys). Returns the short authentication string both screens show.
    pub fn approve_device(&mut self, code: &str, label: &str) -> Result<String> {
        self.require_member()?;
        let (prefix, fp) = parse_pairing_code(code)
            .ok_or_else(|| SyncError::Invalid("that pairing code is not valid".into()))?;
        let listed = self.transport.list_devices()?;
        let mut found = None;
        for d in listed.devices.iter().filter(|d| !d.revoked) {
            if !d
                .device_id
                .as_str()
                .replace('-', "")
                .to_uppercase()
                .starts_with(&prefix)
            {
                continue;
            }
            let (Ok(s), Ok(x)) = (
                crypto::decode32(&d.signing_public_key),
                crypto::decode32(&d.encryption_public_key),
            ) else {
                continue;
            };
            if crypto::fingerprint(&s, &x) == fp {
                found = Some((d.device_id.as_str().to_string(), s, x));
            }
        }
        let (id, s, x) = found.ok_or_else(|| {
            SyncError::Invalid(
                "no waiting device matches that code (keys differ or it is not registered)".into(),
            )
        })?;
        for attempt in 0..3 {
            self.refresh_keylog()?;
            let m = self.membership.clone().expect("refreshed");
            if m.devices.contains_key(&id) {
                break;
            }
            let mut grants = Vec::new();
            for (epoch, k) in &self.epoch_keys {
                grants.push(keylog::grant(*epoch, k, &id, &x)?);
            }
            let cert = DeviceCert {
                device_id: id.clone(),
                sign_pk: base64url::encode(&s),
                dh_pk: base64url::encode(&x),
                label: label.to_string(),
            };
            let entry = keylog::make_entry(
                m.len,
                &m.head,
                EntryKind::AddDevice {
                    device: cert,
                    grants,
                },
                &self.keys.device_id.clone(),
                &self.keys.signing,
            );
            match Self::append_entry(&mut self.transport, m.len, &entry) {
                Ok(()) => break,
                Err(SyncError::Transport(TransportError::Api(b)))
                    if b.code == ErrorCode::Conflict && attempt < 2 =>
                {
                    continue
                }
                Err(e) => return Err(e),
            }
        }
        self.refresh_keylog()?;
        let account_pk = self.membership.as_ref().expect("refreshed").account_pk;
        self.event("approved a new device");
        Ok(crypto::short_auth_string(
            &account_pk,
            &crypto::fingerprint(&s, &x),
        ))
    }

    /// Removes a device: it is deactivated in the key log, a new content
    /// epoch is started for everyone else, and the server revokes it.
    /// (It keeps whatever it already downloaded.)
    pub fn remove_device(&mut self, device_id: &str) -> Result<()> {
        self.require_member()?;
        // Pull first, so the cutoff recorded below covers everything the
        // device managed to upload before its removal (best effort: if this
        // fails, the key-log append below fails too or the cutoff is lower).
        let mut pulled = ReconcileReport::default();
        if let Err(e) = self.pull(&mut pulled) {
            self.event(format!("pull before removal failed: {}", short_error(&e)));
        }
        let last_seq =
            parse_uuid(device_id).map(|u| self.store.oplog.frontier.get(&u).copied().unwrap_or(0));
        for attempt in 0..3 {
            self.refresh_keylog()?;
            let m = self.membership.clone().expect("refreshed");
            if !m.is_active(device_id) {
                break;
            }
            let new_epoch = m.current_epoch + 1;
            let k = EpochKey::generate()?;
            let mut grants = vec![keylog::grant(new_epoch, &k, RECOVERY, &m.recovery_pk)?];
            for d in m.active_devices().filter(|d| d.cert.device_id != device_id) {
                grants.push(keylog::grant(
                    new_epoch,
                    &k,
                    &d.cert.device_id,
                    &d.cert.dh_pk()?,
                )?);
            }
            let entry = keylog::make_entry(
                m.len,
                &m.head,
                EntryKind::RemoveDevice {
                    device_id: device_id.to_string(),
                    new_epoch,
                    grants,
                    last_seq,
                },
                &self.keys.device_id.clone(),
                &self.keys.signing,
            );
            match Self::append_entry(&mut self.transport, m.len, &entry) {
                Ok(()) => break,
                Err(SyncError::Transport(TransportError::Api(b)))
                    if b.code == ErrorCode::Conflict && attempt < 2 =>
                {
                    continue
                }
                Err(e) => return Err(e),
            }
        }
        self.refresh_keylog()?;
        match self.transport.revoke_device(&did(device_id)?) {
            Ok(()) => {}
            Err(TransportError::Api(b)) if b.code == ErrorCode::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.event("removed a device and started a new key epoch");
        Ok(())
    }

    /// Every device in the verified key log (empty before the first refresh).
    pub fn devices(&self) -> Vec<DeviceView> {
        let Some(m) = &self.membership else {
            return Vec::new();
        };
        m.devices
            .values()
            .map(|d| DeviceView {
                device_id: d.cert.device_id.clone(),
                label: d.cert.label.clone(),
                fingerprint: d.cert.fingerprint().unwrap_or_default(),
                active: d.active,
                this_device: d.cert.device_id == self.keys.device_id,
                added_by: d.added_by.clone(),
            })
            .collect()
    }

    /// This sync device's id.
    pub fn device_id(&self) -> &str {
        &self.keys.device_id
    }

    /// Obtains a device session from a server with `auth.v1`
    /// ([`crate::auth::device_session`]): signs the server's challenge with this device's key,
    /// which never leaves the engine. `access_token` is an account (or older device) session of
    /// this engine's account. Does not install the new token; call
    /// `transport_mut().set_token(..)` with it.
    #[cfg(feature = "http")]
    pub fn device_session(
        &mut self,
        server_url: &str,
        access_token: &str,
    ) -> std::result::Result<penroam_sync_protocol::SessionTokens, crate::auth::AuthError> {
        let instance = self.transport.meta()?.server_instance_id;
        let account: penroam_sync_protocol::AccountId = self
            .store
            .state
            .account_id
            .parse()
            .map_err(|_| crate::auth::AuthError::Provider("invalid account id".into()))?;
        crate::auth::device_session(server_url, access_token, &account, &self.keys, &instance)
    }

    /// The transport (to refresh a token, or to simulate faults in tests).
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Forgets the sync setup on this device (keys and sync metadata). The
    /// library and its notes are untouched.
    pub fn disconnect(self, library_root: &Path) -> Result<()> {
        self.secrets.delete(DEVICE_SECRET)?;
        std::fs::remove_dir_all(SyncStore::dir_for(library_root))?;
        Ok(())
    }

    /// Fails unless this device is enrolled and still an active member.
    fn require_member(&mut self) -> Result<()> {
        if !self.store.state.enrolled {
            return Err(SyncError::NotEnrolled);
        }
        if self.membership.is_none() {
            self.refresh_keylog()?;
        }
        if !self
            .membership
            .as_ref()
            .is_some_and(|m| m.is_active(&self.keys.device_id))
        {
            self.phase = Phase::Revoked;
            return Err(SyncError::Revoked);
        }
        Ok(())
    }

    /// Records a diagnostic event (bounded; callers must not pass content or secrets).
    fn event(&mut self, e: impl Into<String>) {
        if self.events.len() > 500 {
            self.events.drain(..100);
        }
        self.events.push(format!("{} {}", now_ms(), e.into()));
    }

    // ---------------------------------------------------------- key log --

    /// Fetches key-log entries after those in `have`, page by page.
    fn fetch_keylog(transport: &mut T, mut have: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>> {
        loop {
            let page = transport.keylog_get(have.len() as u64)?;
            if page.entries.is_empty() {
                break;
            }
            for e in page.entries {
                if e.index != have.len() as u64 {
                    return Err(SyncError::Integrity("key log page out of order".into()));
                }
                have.push(
                    base64url::decode(&e.data)
                        .map_err(|_| SyncError::Integrity("key log entry encoding".into()))?,
                );
            }
            if have.len() as u64 >= page.next_index {
                break;
            }
        }
        Ok(have)
    }

    /// Fetches new key-log entries, verifies the whole log against this
    /// device's pin, updates membership and epoch keys, and moves the pin
    /// forward once this device appears in the log with its own keys. A log
    /// that fails verification puts the engine in [`Phase::IntegrityAlert`].
    fn refresh_keylog(&mut self) -> Result<()> {
        let raw = Self::fetch_keylog(&mut self.transport, std::mem::take(&mut self.keylog_raw))?;
        let pin = self.store.state.pin.clone();
        let m = match keylog::verify_log(&raw, None, pin.as_ref()) {
            Ok(m) => m,
            Err(e) => {
                self.keylog_raw = raw;
                self.phase = Phase::IntegrityAlert;
                return Err(e.into());
            }
        };
        self.keylog_raw = raw;
        self.epoch_keys = m.epoch_keys_for(&self.keys);
        let me_member = m.devices.get(&self.keys.device_id);
        if let Some(me) = me_member {
            if me.cert.sign_pk()? == self.keys.sign_pk() {
                let new_pin = Pin {
                    account_pk: base64url::encode(&m.account_pk),
                    len: m.len,
                    head: m.head.clone(),
                };
                if self.store.state.pin.as_ref() != Some(&new_pin) {
                    self.store.state.pin = Some(new_pin);
                    self.store.save()?;
                }
            }
        }
        self.membership = Some(m);
        Ok(())
    }

    /// The epoch new writes use and its key.
    fn current_epoch(&self) -> Result<(u32, &EpochKey)> {
        let m = self.membership.as_ref().ok_or(SyncError::NotEnrolled)?;
        let k = self.epoch_keys.get(&m.current_epoch).ok_or_else(|| {
            SyncError::Integrity("this device has no key for the current epoch".into())
        })?;
        Ok((m.current_epoch, k))
    }

    // ---------------------------------------------------------- prepare --

    /// Dependencies for a local operation with sync sequence `own_seq`: the
    /// installed version vector of every other device plus this device's
    /// previous operation (see the module docs).
    fn installed_deps(&self, own_seq: u64) -> Vec<OperationRef> {
        let mut deps: Vec<OperationRef> = self
            .store
            .state
            .installed
            .iter()
            .filter_map(|(d, s)| parse_uuid(d).map(|u| (u, *s)))
            .filter(|(u, _)| *u != self.my_uuid)
            .map(|(device_id, device_seq)| OperationRef {
                device_id,
                device_seq,
            })
            .collect();
        if own_seq > 1 {
            deps.push(OperationRef {
                device_id: self.my_uuid,
                device_seq: own_seq - 1,
            });
        }
        deps.sort();
        deps
    }

    /// Appends one local operation body to the operation log as the next
    /// sync sequence (splitting very large ink operations).
    fn append_local(
        &mut self,
        id: Uuid,
        at: i64,
        body: DocumentOperation,
        lib_seq: Option<u64>,
    ) -> Result<usize> {
        let mut n = 0;
        for (i, body) in split_body(body).into_iter().enumerate() {
            let seq = self.store.state.next_seq + 1;
            let op = LocalOperation {
                id: if i == 0 { id } else { derived_id(id, i) },
                device_id: self.my_uuid,
                local_sequence: seq,
                schema_version: penroam_core::operation::OP_SCHEMA_VERSION,
                causal_dependencies: self.installed_deps(seq),
                at,
                body,
            };
            let nb = op.body.notebook_id();
            let record = encode_txn(&Txn {
                target: TxnTarget::Notebook { notebook_id: nb },
                ops: vec![op.clone()],
            });
            // remember the epoch: a retry after a lost response must be
            // sealed with the same key even if the epoch changed meanwhile
            let epoch = self.membership.as_ref().map(|m| m.current_epoch);
            self.store.oplog.append_buffered(
                OpMeta {
                    epoch,
                    origin: Origin::Local,
                    nb,
                    op: op.id,
                    dev: self.my_uuid,
                    seq,
                    ct: None,
                    lib: lib_seq,
                },
                &record,
            )?;
            self.store.state.next_seq = seq;
            if let Some((rep, applied)) = self.replicas.get_mut(&nb) {
                // keep a cached replica current
                if rep.apply_delivered(&op).is_ok() {
                    *applied += 1;
                }
            }
            n += 1;
        }
        Ok(n)
    }

    /// Synthesizes creation operations for one notebook from its current
    /// snapshot: create, assets, pages (with order keys, templates and
    /// backgrounds), their elements, and trash state. Used when sync is first
    /// enabled and for notebooks that enter the library without journal
    /// operations (archive imports).
    fn seed_notebook(&mut self, lib: &mut Library, nb_id: Uuid) -> Result<usize> {
        let mut n = 0;
        let nb = lib.notebook(nb_id)?.clone();
        n += self.append_local(
            Uuid::new_v4(),
            nb.created_at,
            DocumentOperation::NotebookCreated {
                notebook_id: nb.id,
                title: nb.title.clone(),
                cover: nb.cover.clone(),
                defaults: nb.defaults.clone(),
            },
            None,
        )?;
        for a in &nb.assets {
            n += self.append_local(
                Uuid::new_v4(),
                nb.created_at,
                DocumentOperation::AssetReferenced {
                    notebook_id: nb.id,
                    asset: a.clone(),
                },
                None,
            )?;
        }
        for page_id in nb.all_page_ids().collect::<Vec<_>>() {
            let page = lib.load_page(nb.id, page_id)?;
            let order_key = nb
                .key_of(page_id)
                .map(str::to_string)
                .unwrap_or_else(|| penroam_core::order_key::n_between(None, None, 1).remove(0));
            n += self.append_local(
                Uuid::new_v4(),
                page.created_at,
                DocumentOperation::PageCreated {
                    notebook_id: nb.id,
                    page_id,
                    order_key,
                    paper: page.paper,
                    size: page.size,
                    template: page.template.clone(),
                    background: page.background.clone(),
                },
                None,
            )?;
            if !page.elements.is_empty() {
                n += self.append_local(
                    Uuid::new_v4(),
                    page.updated_at,
                    DocumentOperation::ElementsAdded {
                        notebook_id: nb.id,
                        page_id,
                        elements: page.elements.clone(),
                    },
                    None,
                )?;
            }
            if nb.trashed_pages.iter().any(|t| t.id == page_id) {
                n += self.append_local(
                    Uuid::new_v4(),
                    page.updated_at,
                    DocumentOperation::PageDeleted {
                        notebook_id: nb.id,
                        page_id,
                    },
                    None,
                )?;
            }
        }
        if nb.deleted_at.is_some() {
            n += self.append_local(
                Uuid::new_v4(),
                nb.deleted_at.unwrap_or(nb.updated_at),
                DocumentOperation::NotebookDeleted { notebook_id: nb.id },
                None,
            )?;
        }
        Ok(n)
    }

    /// Synthesizes creation operations for every existing notebook (the
    /// library may have checkpointed its history away).
    fn seed(&mut self, lib: &mut Library) -> Result<usize> {
        let mut n = 0;
        let at_seq = lib.last_seq();
        for nb_id in lib.notebook_ids() {
            n += self.seed_notebook(lib, nb_id)?;
        }
        self.store.state.seeded = true;
        self.store.state.ingested_lib_seq = at_seq;
        self.store.save()?;
        lib.set_sync_enabled(true)?;
        lib.mark_uploaded(at_seq)?;
        self.event(format!("seeded {n} operations from existing notebooks"));
        Ok(n)
    }

    /// Copies local journal operations made since the last ingestion.
    fn ingest(&mut self, lib: &mut Library) -> Result<usize> {
        let from = self.store.state.ingested_lib_seq;
        if lib.last_seq() <= from {
            return Ok(0);
        }
        let ops = lib.operations_after(from)?;
        let mut n = 0;
        let mut last = from;
        for op in ops {
            last = op.local_sequence;
            if matches!(op.body, DocumentOperation::NotebookPurged { .. }) {
                continue; // local-only
            }
            n += self.append_local(op.id, op.at, op.body, Some(op.local_sequence))?;
        }
        // order matters: the copies must be durable (save flushes the log)
        // before the library may release its journal entries
        self.store.state.ingested_lib_seq = last;
        self.store.save()?;
        lib.mark_uploaded(last)?;
        Ok(n)
    }

    /// Phase 1 (library locked).
    pub fn prepare(&mut self, lib: &mut Library) -> Result<usize> {
        if !self.store.state.enrolled {
            return Err(SyncError::NotEnrolled);
        }
        let mut n = 0;
        if !self.store.state.seeded {
            n += self.seed(lib)?;
        }
        n += self.ingest(lib)?;
        // Notebooks that entered the library without journal operations (an
        // archive import) are unknown to the operation log: seed them now.
        // Everything created or edited locally after enabling sync arrives
        // through the journal instead, and remote notebooks are in the log.
        let mut seeded_any = false;
        for nb_id in lib.notebook_ids() {
            if self.store.oplog.indexes_for(&nb_id).is_empty() {
                let k = self.seed_notebook(lib, nb_id)?;
                self.event(format!(
                    "seeded {k} operations for a notebook added without history"
                ));
                n += k;
                seeded_any = true;
            }
        }
        if seeded_any {
            self.store.save()?;
        }
        // Assets the library references but does not hold (a download that
        // failed earlier, possibly before a restart): fetch them next exchange.
        for nb_id in lib.notebook_ids() {
            for a in lib.notebook(nb_id)?.assets.clone() {
                if !lib.has_asset(nb_id, &a.sha256) {
                    self.missing_blobs.insert((nb_id, a.sha256));
                }
            }
        }
        // assets referenced by unacknowledged local operations
        self.outgoing_blobs.clear();
        for idx in 0..self.store.oplog.len() {
            let e = self.store.oplog.entry(idx).meta.clone();
            if e.origin != Origin::Local || self.store.state.is_acked(e.seq) {
                continue;
            }
            let rec = self.store.oplog.record(idx)?;
            let Ok(txn) = decode_txn(&rec) else { continue };
            for op in &txn.ops {
                if let DocumentOperation::AssetReferenced { notebook_id, asset } = &op.body {
                    if !self
                        .outgoing_blobs
                        .iter()
                        .any(|(n, s, _)| n == notebook_id && *s == asset.sha256)
                    {
                        let bytes = lib.read_asset(*notebook_id, &asset.sha256)?;
                        self.outgoing_blobs
                            .push((*notebook_id, asset.sha256.clone(), bytes));
                    }
                }
            }
        }
        Ok(n)
    }

    // --------------------------------------------------------- exchange --

    /// Seals operation-log entry `idx` into a signed envelope for `epoch`.
    ///
    /// Retries must produce identical bytes (the server deduplicates by id and
    /// rejects the same id with different content), which holds because the
    /// record is fixed, the nonce is derived from it, and Ed25519 is
    /// deterministic.
    fn seal_local(
        &self,
        idx: usize,
        epoch: u32,
        ek: &EpochKey,
    ) -> Result<(EnvelopeHeader, Vec<u8>)> {
        let record = self.store.oplog.record(idx)?;
        let txn = decode_txn(&record).map_err(|e| SyncError::Integrity(e.to_string()))?;
        let op = &txn.ops[0];
        let routing = ek.routing_id(op.body.notebook_id());
        let key = ek.notebook_key(&routing);
        let mut h = EnvelopeHeader {
            envelope_version: ENVELOPE_VERSION,
            op_id: OpId::new(op.id.hyphenated().to_string())
                .map_err(|e| SyncError::Invalid(e.to_string()))?,
            device_id: did(&self.keys.device_id)?,
            device_seq: op.local_sequence,
            notebook_id: NotebookRoutingId::new(routing)
                .map_err(|e| SyncError::Invalid(e.to_string()))?,
            depends_on: op
                .causal_dependencies
                .iter()
                .map(|d| {
                    Ok(OpRef {
                        device_id: did(&d.device_id.hyphenated().to_string())?,
                        device_seq: d.device_seq,
                    })
                })
                .collect::<Result<_>>()?,
            key_epoch: epoch,
            op_schema_version: op.schema_version,
            // placeholders: the AAD covers the length (known before encrypting,
            // fixed overhead) but not the hash or signature, which depend on
            // the ciphertext and are filled in below
            ciphertext_sha256: "0".repeat(64),
            ciphertext_length: crypto::sealed_len(record.len()) as u64,
            signature: String::new(),
        };
        let ct = crypto::seal(&key, op.id.as_bytes(), &canonical_aad_bytes(&h), &record);
        h.ciphertext_sha256 = crypto::sha256_hex(&ct);
        let sig = crypto::sign(&self.keys.signing, b"", &canonical_signing_bytes(&h));
        h.signature = base64url::encode(&sig);
        Ok((h, ct))
    }

    /// [`Self::current_epoch`] with an owned key (so `self` can be borrowed mutably).
    fn epoch_for_local(&self) -> Result<(u32, EpochKey)> {
        let (e, k) = self.current_epoch()?;
        Ok((e, k.clone()))
    }

    /// Uploads pending blobs, then every unacknowledged local operation in
    /// batches that respect the server limits. Acks are recorded per operation;
    /// the first rejection ends the push with that error after saving the acks
    /// that did succeed.
    fn push(&mut self, report: &mut ReconcileReport) -> Result<()> {
        // blobs first: an operation never references an asset the server lacks
        let (epoch, ek) = self.epoch_for_local()?;
        let blobs = std::mem::take(&mut self.outgoing_blobs);
        for (nb, sha, bytes) in &blobs {
            let routing = ek.routing_id(*nb);
            let id = ek.blob_id(&routing, sha);
            if self.store.state.uploaded_blobs.contains(&id) {
                continue;
            }
            let ct = crypto::seal(
                &ek.blob_key(&routing),
                sha.as_bytes(),
                routing.as_bytes(),
                bytes,
            );
            let bid = BlobId::new(id.clone()).map_err(|e| SyncError::Invalid(e.to_string()))?;
            match self.transport.blob_put(&bid, &crypto::sha256_hex(&ct), &ct) {
                Ok(_) => {}
                Err(e) => {
                    self.outgoing_blobs = blobs.clone();
                    return Err(e.into());
                }
            }
            self.store.state.uploaded_blobs.insert(id);
            self.store.save()?;
        }
        let pending: Vec<usize> = (0..self.store.oplog.len())
            .filter(|i| {
                let m = &self.store.oplog.entry(*i).meta;
                m.origin == Origin::Local && !self.store.state.is_acked(m.seq)
            })
            .collect();
        let mut batch: Vec<FrameItem> = Vec::new();
        let mut seqs: Vec<u64> = Vec::new();
        let mut bytes = 0usize;
        let flush = |me: &mut Self,
                     batch: &mut Vec<FrameItem>,
                     seqs: &mut Vec<u64>,
                     report: &mut ReconcileReport|
         -> Result<()> {
            if batch.is_empty() {
                return Ok(());
            }
            let frame =
                encode_batch(batch, &me.limits).map_err(|e| SyncError::Invalid(e.to_string()))?;
            let resp = me.transport.push(frame)?;
            // acks name exact operations, in request order
            if resp.acks.len() != seqs.len() {
                return Err(SyncError::Transport(TransportError::Protocol(
                    "ack count mismatch".into(),
                )));
            }
            let mut first_err = None;
            for (ack, seq) in resp.acks.iter().zip(seqs.iter()) {
                match &ack.result {
                    AckResult::Accepted { .. } | AckResult::Duplicate { .. } => {
                        me.store.state.ack(*seq);
                        report.pushed += 1;
                    }
                    AckResult::Rejected { error } => {
                        first_err.get_or_insert_with(|| error.clone());
                    }
                }
            }
            me.store.save()?;
            batch.clear();
            seqs.clear();
            if let Some(e) = first_err {
                return Err(SyncError::Transport(TransportError::Api(e)));
            }
            Ok(())
        };
        for idx in pending {
            // seal with the epoch recorded when the operation was queued
            // (header epoch and key always change together)
            let recorded = self.store.oplog.entry(idx).meta.epoch.unwrap_or(epoch);
            let (op_epoch, op_key) = match self.epoch_keys.get(&recorded) {
                Some(k) => (recorded, k.clone()),
                None => (epoch, ek.clone()),
            };
            let (h, ct) = self.seal_local(idx, op_epoch, &op_key)?;
            let header = serde_json::to_vec(&h).expect("header serializes");
            let item_len = header.len() + ct.len() + 8;
            if !batch.is_empty()
                && (batch.len() as u32 >= self.limits.max_batch_envelopes
                    || (bytes + item_len) as u64 > self.limits.max_batch_bytes - 64)
            {
                flush(self, &mut batch, &mut seqs, report)?;
                bytes = 0;
            }
            bytes += item_len;
            seqs.push(h.device_seq);
            batch.push(FrameItem {
                header_json: header,
                ciphertext: ct,
            });
        }
        flush(self, &mut batch, &mut seqs, report)
    }

    /// Test hook: seals every unacknowledged local operation without pushing
    /// it (to simulate what a removed device would upload).
    #[doc(hidden)]
    pub fn seal_pending_for_tests(&mut self) -> Result<Vec<(EnvelopeHeader, Vec<u8>)>> {
        let mut out = Vec::new();
        for idx in 0..self.store.oplog.len() {
            let m = self.store.oplog.entry(idx).meta.clone();
            if m.origin != Origin::Local || self.store.state.is_acked(m.seq) {
                continue;
            }
            let epoch = m.epoch.unwrap_or(1);
            let key = self
                .epoch_keys
                .get(&epoch)
                .cloned()
                .ok_or_else(|| SyncError::Integrity("no key for that epoch".into()))?;
            out.push(self.seal_local(idx, epoch, &key)?);
        }
        Ok(out)
    }

    /// Test hook: only the push step of an exchange.
    #[doc(hidden)]
    pub fn push_for_tests(&mut self, report: &mut ReconcileReport) -> Result<()> {
        if self.membership.is_none() {
            self.refresh_keylog()?;
        }
        self.push(report)
    }

    /// Sets an envelope aside permanently (failed verification); it is never applied.
    fn quarantine(
        &mut self,
        server_seq: u64,
        h: &EnvelopeHeader,
        ct: &[u8],
        reason: &str,
        report: &mut ReconcileReport,
    ) -> Result<()> {
        self.store.park(
            "quarantine",
            &Parked {
                server_seq,
                header: h.clone(),
                ciphertext: base64url::encode(ct),
                reason: reason.to_string(),
            },
        )?;
        report.quarantined += 1;
        self.event(format!(
            "quarantined op from server_seq {server_seq}: {reason}"
        ));
        Ok(())
    }

    /// Verifies a pulled envelope. `Ok(None)` = already known (dedup) or
    /// quarantined; `Ok(Some(reason))` = verified as far as currently possible,
    /// where `"deps"` means fully verified and only waiting for causal delivery,
    /// and `"upgrade"`, `"unknown-device"`, `"key"` mean it must be parked.
    ///
    /// Checks run from cheap to expensive, and nothing is decrypted here:
    /// decryption happens at delivery, after the signature was checked.
    fn admit(
        &mut self,
        server_seq: u64,
        h: &EnvelopeHeader,
        ct: &[u8],
        report: &mut ReconcileReport,
    ) -> Result<Option<&'static str>> {
        let Some(op_id) = parse_uuid(h.op_id.as_str()) else {
            self.quarantine(server_seq, h, ct, "operation id is not a UUID", report)?;
            return Ok(None);
        };
        let sha = crypto::sha256_hex(ct);
        // a known operation id is a redelivery (lost cursor, our own upload
        // echoed back) unless its content differs, which is an integrity alert
        if let Some(meta) = self.store.oplog.meta_of(&op_id) {
            let same = match meta.origin {
                Origin::Local => {
                    h.device_id.as_str() == self.keys.device_id && h.device_seq == meta.seq
                }
                Origin::Remote => meta.ct.as_deref() == Some(sha.as_str()),
            };
            if !same {
                self.phase = Phase::IntegrityAlert;
                self.quarantine(
                    server_seq,
                    h,
                    ct,
                    "operation id reused with different content",
                    report,
                )?;
            }
            return Ok(None);
        }
        if self.is_pending(h.op_id.as_str()) {
            return Ok(None);
        }
        if let Err(e) = h.validate(&self.limits) {
            self.quarantine(server_seq, h, ct, e.kind(), report)?;
            return Ok(None);
        }
        if verify_ciphertext(h, ct).is_err() {
            self.quarantine(
                server_seq,
                h,
                ct,
                "ciphertext does not match its declared hash",
                report,
            )?;
            return Ok(None);
        }
        if h.check_op_schema_supported().is_err() {
            return Ok(Some("upgrade"));
        }
        // the signer must be certified by the verified key log; keys the
        // server returns from /v1/devices are never used for verification.
        let m = self.membership.as_ref().ok_or(SyncError::NotEnrolled)?;
        let Some(member) = m.devices.get(h.device_id.as_str()) else {
            return Ok(Some("unknown-device"));
        };
        let pk = member.cert.sign_pk()?;
        let allowed = member.may_write(h.device_seq);
        let sig = base64url::decode(&h.signature).unwrap_or_default();
        if crypto::verify(&pk, b"", &canonical_signing_bytes(h), &sig).is_err() {
            self.phase = Phase::IntegrityAlert;
            self.quarantine(server_seq, h, ct, "bad signature", report)?;
            return Ok(None);
        }
        // a removed device's operations past the cutoff recorded at removal
        // were written after it was removed: only a misbehaving server would
        // deliver them, so they are refused regardless of the signature
        if !allowed {
            self.phase = Phase::IntegrityAlert;
            self.quarantine(
                server_seq,
                h,
                ct,
                "written by a device after it was removed",
                report,
            )?;
            return Ok(None);
        }
        if !self.epoch_keys.contains_key(&h.key_epoch) {
            return Ok(Some("key"));
        }
        Ok(Some("deps"))
    }

    /// Causal delivery check: the writer's previous sequence and every
    /// dependency are already in the operation log.
    fn deliverable(&self, h: &EnvelopeHeader) -> bool {
        let Some(dev) = parse_uuid(h.device_id.as_str()) else {
            return false;
        };
        // the log frontier is the delivered version vector (operations are
        // appended in causal order only)
        let f = &self.store.oplog.frontier;
        if f.get(&dev).copied().unwrap_or(0) + 1 != h.device_seq {
            return false;
        }
        h.depends_on.iter().all(|d| {
            parse_uuid(d.device_id.as_str())
                .is_some_and(|u| f.get(&u).copied().unwrap_or(0) >= d.device_seq)
        })
    }

    /// Decrypts, checks and appends a verified envelope whose dependencies
    /// are delivered. Returns false when it was quarantined instead.
    fn deliver(
        &mut self,
        server_seq: u64,
        h: &EnvelopeHeader,
        ct: &[u8],
        report: &mut ReconcileReport,
    ) -> Result<bool> {
        let ek = self
            .epoch_keys
            .get(&h.key_epoch)
            .cloned()
            .ok_or(SyncError::NotEnrolled)?;
        // the AAD binds the ciphertext to its header (ids, sequence, epoch,
        // length, dependencies): a valid ciphertext moved under another header
        // fails here
        let key = ek.notebook_key(h.notebook_id.as_str());
        let record = match crypto::open(&key, &canonical_aad_bytes(h), ct) {
            Ok(r) => r,
            Err(_) => {
                self.phase = Phase::IntegrityAlert;
                self.quarantine(server_seq, h, ct, "authentication tag mismatch", report)?;
                return Ok(false);
            }
        };
        let txn = match decode_txn(&record) {
            Ok(t) => t,
            Err(RecordError::NewerSchema(_)) => {
                self.park_kind("pending", server_seq, h, ct, "upgrade")?;
                self.phase = Phase::NeedsUpgrade;
                return Ok(false);
            }
            Err(e) => {
                self.quarantine(server_seq, h, ct, &format!("payload: {e}"), report)?;
                return Ok(false);
            }
        };
        // the server only sees the header, and other devices trust it for
        // routing and causal delivery: the decrypted operation must say exactly
        // the same thing (and be mergeable: a purge is local-only)
        let ok = txn.ops.len() == 1 && {
            let op = &txn.ops[0];
            let mut deps: Vec<(String, u64)> = op
                .causal_dependencies
                .iter()
                .map(|d| (d.device_id.hyphenated().to_string(), d.device_seq))
                .collect();
            deps.sort();
            let mut hdeps: Vec<(String, u64)> = h
                .depends_on
                .iter()
                .map(|d| (d.device_id.as_str().to_string(), d.device_seq))
                .collect();
            hdeps.sort();
            op.id.hyphenated().to_string() == h.op_id.as_str()
                && op.device_id.hyphenated().to_string() == h.device_id.as_str()
                && op.local_sequence == h.device_seq
                && op.schema_version == h.op_schema_version
                && deps == hdeps
                && ek.routing_id(op.body.notebook_id()) == h.notebook_id.as_str()
                && !matches!(op.body, DocumentOperation::NotebookPurged { .. })
        };
        if !ok {
            self.phase = Phase::IntegrityAlert;
            self.quarantine(
                server_seq,
                h,
                ct,
                "payload does not match its signed header",
                report,
            )?;
            return Ok(false);
        }
        let op = txn.ops.into_iter().next().unwrap();
        let nb = op.body.notebook_id();
        self.store.oplog.append_buffered(
            OpMeta {
                epoch: Some(h.key_epoch),
                origin: Origin::Remote,
                nb,
                op: op.id,
                dev: op.device_id,
                seq: op.local_sequence,
                ct: Some(crypto::sha256_hex(ct)),
                lib: None,
            },
            &record,
        )?;
        self.note_delivered(nb, &op.body);
        report.delivered += 1;
        Ok(true)
    }

    /// Parks an envelope under `kind` with a reason (see [`Parked`]).
    /// Marks what a newly delivered remote operation changes, for `apply`:
    /// its notebook, the page whose content it edits or creates, and any
    /// asset whose bytes must be downloaded.
    fn note_delivered(&mut self, nb: Uuid, body: &DocumentOperation) {
        self.store.state.dirty.insert(nb);
        if let Some(p) = body.content_page() {
            self.store.state.dirty_pages.insert(format!("{nb}/{p}"));
        }
        if let DocumentOperation::PageCreated { page_id, .. } = body {
            self.store
                .state
                .dirty_pages
                .insert(format!("{nb}/{page_id}"));
        }
        if let DocumentOperation::AssetReferenced { asset, .. } = body {
            self.missing_blobs.insert((nb, asset.sha256.clone()));
        }
    }

    fn park_kind(
        &mut self,
        kind: &str,
        server_seq: u64,
        h: &EnvelopeHeader,
        ct: &[u8],
        reason: &str,
    ) -> Result<()> {
        self.store.park(
            kind,
            &Parked {
                server_seq,
                header: h.clone(),
                ciphertext: base64url::encode(ct),
                reason: reason.to_string(),
            },
        )?;
        if kind == "pending" {
            self.pending_set().insert(h.op_id.as_str().to_string());
        }
        Ok(())
    }

    /// The in-memory index of parked op ids, loaded from disk on first use.
    fn pending_set(&mut self) -> &mut std::collections::HashSet<String> {
        if self.pending_ids.is_none() {
            let ids = self
                .store
                .parked("pending")
                .into_iter()
                .map(|p| p.header.op_id.as_str().to_string())
                .collect();
            self.pending_ids = Some(ids);
        }
        self.pending_ids.as_mut().expect("just loaded")
    }

    fn is_pending(&mut self, op: &str) -> bool {
        self.pending_set().contains(op)
    }

    /// Removes a parked envelope (after its oplog copy is durable).
    fn unpark_pending(&mut self, op: &str) -> Result<()> {
        self.store.unpark("pending", op)?;
        self.pending_set().remove(op);
        Ok(())
    }

    /// Tries to deliver parked envelopes until nothing more is deliverable.
    fn drain_pending(&mut self, report: &mut ReconcileReport) -> Result<()> {
        let mut delivered: Vec<String> = Vec::new();
        loop {
            let mut progressed = false;
            for p in self.store.parked("pending") {
                let ct = base64url::decode(&p.ciphertext).unwrap_or_default();
                let reason = match p.reason.as_str() {
                    "deps" => "deps",
                    _ => match self.admit_parked(&p, &ct, report)? {
                        Some(r) => r,
                        None => {
                            self.unpark_pending(p.header.op_id.as_str())?;
                            continue;
                        }
                    },
                };
                if reason != "deps" {
                    if reason != p.reason {
                        self.park_kind("pending", p.server_seq, &p.header, &ct, reason)?;
                    }
                    continue;
                }
                if p.reason != "deps" {
                    self.park_kind("pending", p.server_seq, &p.header, &ct, "deps")?;
                }
                if self.deliverable(&p.header) {
                    self.deliver(p.server_seq, &p.header, &ct, report)?;
                    delivered.push(p.header.op_id.as_str().to_string());
                    progressed = true;
                }
            }
            // the parked copy goes only after the oplog copy is durable
            self.store.oplog.flush()?;
            for op in delivered.drain(..) {
                self.unpark_pending(&op)?;
            }
            if !progressed {
                return Ok(());
            }
        }
    }

    /// Re-checks a parked envelope whose reason may have changed (new key-log
    /// entries can make an unknown signer known or grant a missing epoch key).
    fn admit_parked(
        &mut self,
        p: &Parked,
        ct: &[u8],
        report: &mut ReconcileReport,
    ) -> Result<Option<&'static str>> {
        // re-check reasons that may have changed (new key-log entries, epoch keys)
        let m = self.membership.as_ref().ok_or(SyncError::NotEnrolled)?;
        if p.header.check_op_schema_supported().is_err() {
            return Ok(Some("upgrade"));
        }
        let Some(member) = m.devices.get(p.header.device_id.as_str()) else {
            return Ok(Some("unknown-device"));
        };
        let pk = member.cert.sign_pk()?;
        let allowed = member.may_write(p.header.device_seq);
        let sig = base64url::decode(&p.header.signature).unwrap_or_default();
        if crypto::verify(&pk, b"", &canonical_signing_bytes(&p.header), &sig).is_err() {
            self.quarantine(p.server_seq, &p.header, ct, "bad signature", report)?;
            return Ok(None);
        }
        if !allowed {
            self.quarantine(
                p.server_seq,
                &p.header,
                ct,
                "written by a device after it was removed",
                report,
            )?;
            return Ok(None);
        }
        if !self.epoch_keys.contains_key(&p.header.key_epoch) {
            return Ok(Some("key"));
        }
        Ok(Some("deps"))
    }

    /// Pulls every page after the saved cursor, admitting, delivering or
    /// parking each envelope, and saves the cursor after each page only once
    /// all of its envelopes are durable.
    fn pull(&mut self, report: &mut ReconcileReport) -> Result<()> {
        self.pull_done = false;
        let mut tried_checkpoint = false;
        loop {
            let cursor = match &self.store.state.cursor {
                Some(c) => {
                    Some(Cursor::new(c.clone()).map_err(|e| SyncError::Integrity(e.to_string()))?)
                }
                None => None,
            };
            let page = match self.transport.pull(
                &PullOpsRequest {
                    cursor,
                    limit: PULL_PAGE,
                },
                &self.limits,
            ) {
                Ok(p) => p,
                Err(TransportError::Api(b))
                    if matches!(
                        b.code,
                        ErrorCode::CursorExpired | ErrorCode::CheckpointRequired
                    ) =>
                {
                    // The server compacted the history this device still
                    // needs: recover from the latest checkpoint once, then
                    // continue from the position it covers.
                    if !tried_checkpoint {
                        tried_checkpoint = true;
                        if self.rebase_from_checkpoint(report)? {
                            continue;
                        }
                    }
                    self.phase = Phase::RebaseRequired;
                    return Err(SyncError::Transport(TransportError::Api(b)));
                }
                Err(e) => return Err(e.into()),
            };
            let mut unknown_device = false;
            for (item, ct) in page.response.items.iter().zip(page.ciphertexts.iter()) {
                report.pulled += 1;
                self.store.state.last_server_seq =
                    self.store.state.last_server_seq.max(item.server_seq);
                match self.admit(item.server_seq, &item.header, ct, report)? {
                    None => {}
                    Some("deps") if self.deliverable(&item.header) => {
                        self.deliver(item.server_seq, &item.header, ct, report)?;
                    }
                    Some(reason) => {
                        if reason == "unknown-device" || reason == "key" {
                            unknown_device = true;
                        }
                        if reason == "upgrade" {
                            self.phase = Phase::NeedsUpgrade;
                        }
                        self.park_kind("pending", item.server_seq, &item.header, ct, reason)?;
                    }
                }
            }
            if unknown_device {
                // a device may have been added after we last read the key log
                self.refresh_keylog()?;
            }
            self.drain_pending(report)?;
            // everything on this page is in the oplog or parked (fsynced)
            let next = page.response.next_cursor.as_str().to_string();
            if self.store.state.cursor.as_deref() != Some(next.as_str())
                || !page.response.items.is_empty()
            {
                self.store.state.cursor = Some(next);
                self.store.save()?;
                self.ack_due = Some(page.response.next_cursor.clone());
            }
            if !page.response.has_more {
                self.pull_done = true;
                return Ok(());
            }
        }
    }

    /// Downloads assets referenced by delivered operations, decrypts and checks
    /// their SHA-256, and writes them to the blob cache. Failures are retried
    /// in later reconciles (best effort; the notes still install without them).
    fn fetch_blobs(&mut self) {
        let Ok((_, _)) = self.current_epoch() else {
            return;
        };
        let wanted: Vec<(Uuid, String)> = self.missing_blobs.iter().cloned().collect();
        // Entries stay in `missing_blobs` until `apply` installs the bytes into
        // the library; this only fills the download cache.
        for (nb, sha) in wanted {
            if self.store.blob_cache(&sha).exists() {
                continue;
            }
            let mut got = false;
            for ek in self.epoch_keys.values().cloned().collect::<Vec<_>>() {
                let routing = ek.routing_id(nb);
                let Ok(bid) = BlobId::new(ek.blob_id(&routing, &sha)) else {
                    continue;
                };
                let Ok(ct) = self.transport.blob_get(&bid) else {
                    continue;
                };
                if let Ok(plain) = crypto::open(&ek.blob_key(&routing), routing.as_bytes(), &ct) {
                    if crypto::sha256_hex(&plain) == sha
                        && penroam_storage::fsutil::write_atomic(
                            &self.store.blob_cache(&sha),
                            &plain,
                        )
                        .is_ok()
                    {
                        got = true;
                        break;
                    }
                }
            }
            if !got {
                self.event("asset download failed; will retry");
            }
        }
    }

    /// Phase 2 (no library lock).
    pub fn exchange(&mut self, report: &mut ReconcileReport) -> Result<()> {
        if self.store.state.paused {
            self.phase = Phase::Paused;
            return Ok(());
        }
        self.phase = Phase::Syncing;
        let meta = self.transport.meta()?;
        if let Err(e) = negotiate(ProtocolRange::CLIENT, &meta) {
            self.phase = Phase::NeedsUpgrade;
            return Err(SyncError::UpgradeRequired(format!("{e:?}")));
        }
        self.limits = meta
            .limits
            .validate()
            .map(|_| meta.limits)
            .unwrap_or(Limits::DEFAULT);
        self.refresh_keylog()?;
        if !self
            .membership
            .as_ref()
            .is_some_and(|m| m.is_active(&self.keys.device_id))
        {
            self.phase = Phase::Revoked;
            return Err(SyncError::Revoked);
        }
        // acks only inform server retention: never let them block uploads
        if let Some(c) = self.ack_due.take() {
            match self.transport.ack(&AckRequest {
                applied_cursor: c.clone(),
            }) {
                Ok(()) => {}
                Err(e) if e.is_retryable() => self.ack_due = Some(c),
                Err(e) => self.event(format!(
                    "ack dropped: {}",
                    short_error(&SyncError::Transport(e))
                )),
            }
        }
        self.push(report)?;
        self.pull(report)?;
        self.fetch_blobs();
        // publish a checkpoint now and then so devices that fall far behind
        // can recover after the server compacts (best effort)
        let grown = self.store.oplog.len() as u64
            - self
                .store
                .state
                .checkpointed_len
                .min(self.store.oplog.len() as u64);
        if grown >= CHECKPOINT_EVERY
            && meta.has_capability(penroam_sync_protocol::dto::capabilities::CHECKPOINTS_V1)
        {
            if let Err(e) = self.create_checkpoint() {
                self.event(format!("checkpoint not published: {}", short_error(&e)));
            }
        }
        Ok(())
    }

    /// Publishes a checkpoint: this device's whole operation log (every
    /// local and remote operation it holds), encrypted with the current
    /// epoch's checkpoint key, uploaded as a blob, and announced with a
    /// signed [`CheckpointRecord`] covering the cursor this device has
    /// durably staged. Returns `None` (nothing published) while envelopes
    /// are still parked, because the checkpoint would not cover them.
    ///
    /// Other devices trust a checkpoint only if a current member signed it;
    /// the operations inside are vouched for by that signer (see the threat
    /// model).
    pub fn create_checkpoint(&mut self) -> Result<Option<CheckpointRecord>> {
        self.require_member()?;
        if self.store.parked_count("pending") > 0 {
            return Ok(None);
        }
        let Some(cursor) = self.store.state.cursor.clone() else {
            return Ok(None);
        };
        let cursor = Cursor::new(cursor).map_err(|e| SyncError::Integrity(e.to_string()))?;
        let (epoch, ek) = self.epoch_for_local()?;
        // PRCP v1: count, then (u32 meta-json length, meta json, u32 record
        // length, record) per operation, in log order (which is causal order)
        let mut plain = b"PRCP".to_vec();
        plain.extend_from_slice(&1u16.to_le_bytes());
        plain.extend_from_slice(&(self.store.oplog.len() as u32).to_le_bytes());
        for idx in 0..self.store.oplog.len() {
            let mut meta = self.store.oplog.entry(idx).meta.clone();
            if meta.origin == Origin::Local {
                // the hash its envelope has (or will have): sealing is deterministic
                let e = meta.epoch.unwrap_or(epoch);
                let key = self
                    .epoch_keys
                    .get(&e)
                    .cloned()
                    .unwrap_or_else(|| ek.clone());
                let (_, ct) = self.seal_local(idx, e, &key)?;
                meta.ct = Some(crypto::sha256_hex(&ct));
            }
            meta.lib = None;
            let m = serde_json::to_vec(&meta).expect("meta serializes");
            let rec = self.store.oplog.record(idx)?;
            plain.extend_from_slice(&(m.len() as u32).to_le_bytes());
            plain.extend_from_slice(&m);
            plain.extend_from_slice(&(rec.len() as u32).to_le_bytes());
            plain.extend_from_slice(&rec);
        }
        let ct = crypto::seal(&ek.checkpoint_key(), b"checkpoint", CHECKPOINT_AAD, &plain);
        let sha = crypto::sha256_hex(&ct);
        let blob_id = BlobId::new(ek.blob_id("checkpoint", &crypto::sha256_hex(&plain)))
            .map_err(|e| SyncError::Invalid(e.to_string()))?;
        self.transport.blob_put(&blob_id, &sha, &ct)?;
        let mut rec = CheckpointRecord {
            covers_cursor: cursor,
            covers_server_seq: self.store.state.last_server_seq,
            blob_id,
            blob_sha256: sha,
            blob_length: ct.len() as u64,
            key_epoch: epoch,
            created_by: did(&self.keys.device_id)?,
            signature: String::new(),
        };
        rec.signature = base64url::encode(&crypto::sign(
            &self.keys.signing,
            b"",
            &canonical_checkpoint_bytes(&rec),
        ));
        self.transport.checkpoint_put(&rec)?;
        self.store.state.checkpointed_len = self.store.oplog.len() as u64;
        self.store.save()?;
        self.event(format!(
            "published a checkpoint of {} operations",
            self.store.oplog.len()
        ));
        Ok(Some(rec))
    }

    /// Recovers after the server compacted history this device had not
    /// pulled: verifies the latest checkpoint (signed by a current member,
    /// blob hash, AEAD), imports every operation it does not have yet in log
    /// order (each must come from a device allowed to write it and continue
    /// that device's sequence), then moves the cursor to the position the
    /// checkpoint covers. Local operations are untouched (and were pushed
    /// before pulling). Returns false when the account has no checkpoint.
    fn rebase_from_checkpoint(&mut self, report: &mut ReconcileReport) -> Result<bool> {
        let Some(rec) = self.transport.checkpoint_latest()? else {
            return Ok(false);
        };
        let m = self.membership.as_ref().ok_or(SyncError::NotEnrolled)?;
        let signer = m
            .devices
            .get(rec.created_by.as_str())
            .filter(|d| d.active)
            .ok_or_else(|| {
                SyncError::Integrity(
                    "checkpoint signed by a device that is not a current member".into(),
                )
            })?;
        let pk = signer.cert.sign_pk()?;
        let sig = base64url::decode(&rec.signature).unwrap_or_default();
        crypto::verify(&pk, b"", &canonical_checkpoint_bytes(&rec), &sig)
            .map_err(|_| SyncError::Integrity("checkpoint signature does not verify".into()))?;
        let ek = self
            .epoch_keys
            .get(&rec.key_epoch)
            .cloned()
            .ok_or_else(|| SyncError::Integrity("no key for the checkpoint's epoch".into()))?;
        let ct = self.transport.blob_get(&rec.blob_id)?;
        if ct.len() as u64 != rec.blob_length || crypto::sha256_hex(&ct) != rec.blob_sha256 {
            return Err(SyncError::Integrity(
                "checkpoint blob does not match its record".into(),
            ));
        }
        let plain = crypto::open(&ek.checkpoint_key(), CHECKPOINT_AAD, &ct)
            .map_err(|_| SyncError::Integrity("checkpoint does not decrypt".into()))?;
        let items = parse_checkpoint(&plain)
            .ok_or_else(|| SyncError::Integrity("checkpoint is malformed".into()))?;
        let mut imported = 0usize;
        for (meta, record) in items {
            if self.store.oplog.contains(&meta.op) {
                continue;
            }
            let txn = decode_txn(&record)
                .map_err(|e| SyncError::Integrity(format!("checkpoint operation: {e}")))?;
            let [op] = txn.ops.as_slice() else {
                return Err(SyncError::Integrity(
                    "checkpoint record with several operations".into(),
                ));
            };
            let dev = op.device_id.hyphenated().to_string();
            let allowed = self
                .membership
                .as_ref()
                .and_then(|m| m.devices.get(&dev))
                .is_some_and(|d| d.may_write(op.local_sequence));
            let have = self
                .store
                .oplog
                .frontier
                .get(&op.device_id)
                .copied()
                .unwrap_or(0);
            if op.id != meta.op
                || op.device_id != meta.dev
                || op.local_sequence != meta.seq
                || !allowed
                || have + 1 != op.local_sequence
            {
                return Err(SyncError::Integrity(
                    "checkpoint operation fails validation".into(),
                ));
            }
            let nb = op.body.notebook_id();
            let body = op.body.clone();
            self.store.oplog.append_buffered(
                OpMeta {
                    origin: Origin::Remote,
                    nb,
                    op: op.id,
                    dev: op.device_id,
                    seq: op.local_sequence,
                    ct: meta.ct.clone(),
                    lib: None,
                    epoch: meta.epoch,
                },
                &record,
            )?;
            self.note_delivered(nb, &body);
            imported += 1;
        }
        report.delivered += imported;
        self.store.state.cursor = Some(rec.covers_cursor.as_str().to_string());
        self.store.state.last_server_seq =
            self.store.state.last_server_seq.max(rec.covers_server_seq);
        self.store.save()?;
        self.drain_pending(report)?;
        self.event(format!(
            "recovered from a checkpoint: imported {imported} operations"
        ));
        Ok(true)
    }

    // ------------------------------------------------------------ apply --

    /// The merge state of a notebook, brought up to date with every operation
    /// in the log (building it from scratch the first time). Operations the merge
    /// model rejects are skipped and logged as events; they stay in the log.
    fn replica(&mut self, nb: Uuid) -> Result<&Replica> {
        let idxs: Vec<usize> = self.store.oplog.indexes_for(&nb).to_vec();
        let entry = self
            .replicas
            .entry(nb)
            .or_insert_with(|| (Replica::new(nb), 0));
        let start = entry.1;
        let mut new_events = Vec::new();
        for &i in &idxs[start.min(idxs.len())..] {
            let rec = self.store.oplog.record(i)?;
            let txn = decode_txn(&rec).map_err(|e| SyncError::Integrity(e.to_string()))?;
            for op in &txn.ops {
                if let Err(e) = self.replicas.get_mut(&nb).unwrap().0.apply_delivered(op) {
                    new_events.push(format!("operation in notebook skipped by merge: {e}"));
                }
            }
            self.replicas.get_mut(&nb).unwrap().1 += 1;
        }
        for e in new_events {
            self.event(e);
        }
        Ok(&self.replicas[&nb].0)
    }

    /// Phase 3 (library locked): installs merged notebooks.
    pub fn apply(&mut self, lib: &mut Library, report: &mut ReconcileReport) -> Result<()> {
        report.ingested += self.ingest(lib)?;
        // ingest first: local edits made during the exchange must be part of
        // the merged state we install, or the install would hide them
        let dirty: Vec<Uuid> = self.store.state.dirty.iter().copied().collect();
        for nb in dirty {
            let dirty_pages: BTreeSet<String> = self.store.state.dirty_pages.clone();
            let (merged, pages, conflicts) = {
                let rep = self.replica(nb)?;
                let Some(merged) = rep.notebook() else {
                    continue;
                };
                let prefix = format!("{nb}/");
                let local = lib
                    .notebook(nb)
                    .ok()
                    .map(|n| n.all_page_ids().collect::<BTreeSet<_>>())
                    .unwrap_or_default();
                // reinstall only pages that changed remotely or are new here;
                // pages edited only locally already show the right content
                let mut pages = Vec::new();
                for pid in merged.all_page_ids() {
                    let key = format!("{prefix}{pid}");
                    if !local.contains(&pid) || dirty_pages.contains(&key) {
                        if let Some(p) = rep.page(pid) {
                            pages.push(p);
                        }
                    }
                }
                (merged, pages, rep.conflicts().len())
            };
            for a in &merged.assets {
                if !lib.has_asset(nb, &a.sha256) {
                    let cache = self.store.blob_cache(&a.sha256);
                    if let Ok(bytes) = std::fs::read(&cache) {
                        lib.install_asset(nb, &bytes)?;
                        let _ = std::fs::remove_file(cache);
                    } else {
                        self.missing_blobs.insert((nb, a.sha256.clone()));
                    }
                }
            }
            let page_ids: Vec<Uuid> = pages.iter().map(|p| p.id).collect();
            lib.install_merged(merged, pages)?;
            for pid in &page_ids {
                self.store.state.dirty_pages.remove(&format!("{nb}/{pid}"));
                report.installed_pages.push((nb, *pid));
            }
            self.conflicts.insert(nb, conflicts);
            self.store.state.dirty.remove(&nb);
            report.installed_notebooks.push(nb);
        }
        // assets that arrived for notebooks installed earlier
        for (nb, sha) in self.missing_blobs.clone() {
            let cache = self.store.blob_cache(&sha);
            if lib.notebook(nb).is_ok() && !lib.has_asset(nb, &sha) {
                if let Ok(bytes) = std::fs::read(&cache) {
                    lib.install_asset(nb, &bytes)?;
                    let _ = std::fs::remove_file(cache);
                    self.missing_blobs.remove(&(nb, sha));
                }
            }
        }
        let installed: BTreeMap<String, u64> = self
            .store
            .oplog
            .frontier
            .iter()
            .map(|(d, s)| (d.hyphenated().to_string(), *s))
            .collect();
        if installed != self.store.state.installed || !report.installed_notebooks.is_empty() {
            self.store.state.installed = installed;
            self.store.save()?;
        }
        Ok(())
    }

    /// Runs one full reconcile against a shared library, holding the lock
    /// only for the local phases.
    pub fn reconcile(&mut self, lib: &Mutex<Library>) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        // the lock guard is a temporary, so the library is unlocked during the
        // exchange (network) and editing continues meanwhile
        let result = (|| {
            report.ingested += self.prepare(&mut lib.lock().unwrap_or_else(|p| p.into_inner()))?;
            let ex = self.exchange(&mut report);
            // install whatever was delivered even if the exchange failed later
            self.apply(
                &mut lib.lock().unwrap_or_else(|p| p.into_inner()),
                &mut report,
            )?;
            ex
        })();
        self.finish(&result);
        result.map(|_| report)
    }

    /// Same as [`SyncEngine::reconcile`] for a library owned by the caller.
    pub fn reconcile_owned(&mut self, lib: &mut Library) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let result = (|| {
            report.ingested += self.prepare(lib)?;
            let ex = self.exchange(&mut report);
            self.apply(lib, &mut report)?;
            ex
        })();
        self.finish(&result);
        result.map(|_| report)
    }

    /// Updates the phase, error text and backoff counter after a reconcile.
    fn finish(&mut self, result: &Result<()>) {
        match result {
            Ok(()) => {
                self.attempt = 0;
                self.last_error = None;
                if self.store.state.paused {
                    self.phase = Phase::Paused;
                } else if !matches!(self.phase, Phase::NeedsUpgrade | Phase::IntegrityAlert) {
                    self.phase = Phase::Idle;
                }
                // kept in memory; persisted with the next real state change
                // (an idle reconcile writes nothing to disk)
                self.store.state.last_success_ms = Some(now_ms());
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                self.phase = match e {
                    SyncError::Transport(TransportError::Network(_)) => Phase::Offline,
                    SyncError::Transport(TransportError::Api(b)) => match b.code {
                        ErrorCode::Unauthenticated => Phase::NeedsReauth,
                        ErrorCode::DeviceRevoked | ErrorCode::Forbidden => Phase::Revoked,
                        ErrorCode::QuotaExceeded => Phase::QuotaExceeded,
                        ErrorCode::UpgradeRequired => Phase::NeedsUpgrade,
                        ErrorCode::CursorExpired | ErrorCode::CheckpointRequired => {
                            Phase::RebaseRequired
                        }
                        ErrorCode::OpIdConflict | ErrorCode::IntegrityMismatch => {
                            Phase::IntegrityAlert
                        }
                        c if c.is_retryable() => Phase::Backoff,
                        _ => Phase::Error,
                    },
                    SyncError::Transport(TransportError::ResponseLost) => Phase::Backoff,
                    SyncError::Revoked => Phase::Revoked,
                    SyncError::NotEnrolled => Phase::AwaitingApproval,
                    SyncError::UpgradeRequired(_) => Phase::NeedsUpgrade,
                    SyncError::Integrity(_) | SyncError::KeyLog(_) => Phase::IntegrityAlert,
                    _ => Phase::Error,
                };
                if e.is_retryable() {
                    self.attempt = self.attempt.saturating_add(1);
                }
                self.event(format!("reconcile failed: {}", short_error(e)));
            }
        }
    }

    /// Delay before the next automatic attempt after a retryable failure
    /// (exponential backoff with full jitter), or `None` if retrying will
    /// not help without user action.
    pub fn retry_delay_ms(&self, err: &SyncError) -> Option<u64> {
        let SyncError::Transport(t) = err else {
            return None;
        };
        let r: u64 = u64::from_le_bytes(crypto::random_bytes::<8>().unwrap_or([7; 8]));
        let policy = RetryPolicy {
            max_attempts: u32::MAX,
            ..RetryPolicy::default()
        };
        policy.delay_for(&t.as_error_body(), self.attempt.saturating_sub(1), r)
    }

    // ----------------------------------------------------------- status --

    /// Pauses or resumes syncing (persisted). Local editing is unaffected.
    pub fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.store.state.paused = paused;
        self.store.save()?;
        self.phase = if paused { Phase::Paused } else { Phase::Idle };
        Ok(())
    }

    /// Current status. `lib_last_seq` is [`Library::last_seq`], so journal
    /// operations not yet copied into the sync log count as queued.
    pub fn status(&self, lib_last_seq: u64) -> SyncStatus {
        let unacked = self
            .store
            .oplog
            .entries()
            .iter()
            .filter(|e| e.meta.origin == Origin::Local && !self.store.state.is_acked(e.meta.seq))
            .count() as u64;
        let not_ingested = lib_last_seq.saturating_sub(self.store.state.ingested_lib_seq);
        let queued = unacked + not_ingested;
        let pending_remote = self.store.parked_count("pending");
        let conflicts = self.conflicts.values().sum();
        SyncStatus {
            phase: self.phase,
            queued,
            pending_remote,
            quarantined: self.store.parked_count("quarantine"),
            conflicts,
            up_to_date: self.phase == Phase::Idle
                && queued == 0
                && pending_remote == 0
                && self.pull_done
                && self.store.state.dirty.is_empty(),
            last_success_ms: self.store.state.last_success_ms,
            last_error: self.last_error.clone(),
            device_id: self.keys.device_id.clone(),
            account_id: self.store.state.account_id.clone(),
            server_url: self.store.state.server_url.clone(),
            secret_store: self.secrets.kind().to_string(),
            security_review_pending: true,
        }
    }

    // -------------------------------------------------------- conflicts --

    /// Unresolved conflicts of a notebook (both values kept).
    pub fn conflicts(&mut self, nb: Uuid) -> Result<Vec<Conflict>> {
        let c = self.replica(nb)?.conflicts();
        self.conflicts.insert(nb, c.len());
        Ok(c)
    }

    /// Resolves a conflict by choosing one of the retained values. The
    /// choice is an ordinary local edit, so it syncs and supersedes every
    /// alternative on all devices.
    pub fn resolve_conflict(
        &mut self,
        lib: &mut Library,
        nb: Uuid,
        target: &ConflictTarget,
        choose: Uuid,
    ) -> Result<()> {
        let conflicts = self.conflicts(nb)?;
        let c = conflicts
            .iter()
            .find(|c| &c.target == target)
            .ok_or_else(|| SyncError::Invalid("that conflict no longer exists".into()))?;
        let value = if c.winner == choose {
            None
        } else {
            Some(
                c.alternates
                    .iter()
                    .find(|a| a.op_id == choose)
                    .ok_or_else(|| {
                        SyncError::Invalid("that version is not part of the conflict".into())
                    })?
                    .value
                    .clone(),
            )
        };
        match target {
            ConflictTarget::NotebookTitle => {
                let title = match value {
                    Some(v) => v.as_str().unwrap_or_default().to_string(),
                    None => lib.notebook(nb)?.title.clone(),
                };
                lib.rename_notebook(nb, &title)?;
            }
            ConflictTarget::NotebookCover => {
                let cover = match value {
                    Some(v) => v.as_str().unwrap_or_default().to_string(),
                    None => lib.notebook(nb)?.cover.clone(),
                };
                lib.set_notebook_cover(nb, &cover)?;
            }
            ConflictTarget::Element {
                page_id,
                element_id,
                ..
            }
            | ConflictTarget::EditedWhileDeleted {
                page_id,
                element_id,
            } => {
                let page = lib.load_page(nb, *page_id)?;
                let current = page
                    .elements
                    .iter()
                    .find(|e| e.id() == *element_id)
                    .cloned();
                let chosen: Option<Element> = match value {
                    Some(v) => Some(
                        serde_json::from_value(v).map_err(|e| SyncError::Invalid(e.to_string()))?,
                    ),
                    None => current.clone(),
                };
                let Some(chosen) = chosen else { return Ok(()) };
                let kind = if current.is_some() {
                    OpKind::ReplaceElements {
                        elements: vec![chosen],
                    }
                } else {
                    OpKind::AddElements {
                        elements: vec![chosen],
                    }
                };
                lib.apply_page_operations(
                    nb,
                    *page_id,
                    &OperationBatch {
                        batch_id: Uuid::new_v4(),
                        base_revision: page.revision,
                        ops: vec![PageOperation {
                            op_id: Uuid::new_v4(),
                            kind,
                        }],
                    },
                )?;
            }
            _ => {
                return Err(SyncError::Invalid(
                    "this kind of conflict is resolved by editing the page or notebook directly"
                        .into(),
                ))
            }
        }
        Ok(())
    }
}

/// A compact error label for diagnostic events (no messages from the server).
fn short_error(e: &SyncError) -> String {
    match e {
        SyncError::Transport(TransportError::Api(b)) => b.code.as_str().to_string(),
        SyncError::Transport(TransportError::Network(_)) => "network".into(),
        SyncError::Transport(TransportError::ResponseLost) => "response lost".into(),
        other => other.to_string(),
    }
}

/// `XXXXXXXX-XXXX-XXXX-XXXX-XXXX`: the first 8 hex digits of the device id
/// followed by the 64-bit key fingerprint.
pub fn pairing_code(device_id: &str, fingerprint: &str) -> String {
    let id: String = device_id
        .replace('-', "")
        .to_uppercase()
        .chars()
        .take(8)
        .collect();
    format!("{id}-{fingerprint}")
}

/// Parses a pairing code typed by a person (case, dashes and spaces ignored)
/// into the device-id prefix and the formatted fingerprint.
pub fn parse_pairing_code(code: &str) -> Option<(String, String)> {
    let c: String = code
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_uppercase();
    if c.len() != 24 {
        return None;
    }
    let fp = format!("{}-{}-{}-{}", &c[8..12], &c[12..16], &c[16..20], &c[20..24]);
    Some((c[..8].to_string(), fp))
}

/// Deterministic id for chunk `i` of a split operation, so re-ingesting the
/// same journal operation always yields the same chunk ids.
fn derived_id(id: Uuid, i: usize) -> Uuid {
    let h = crypto::sha256(&[id.as_bytes().as_slice(), &(i as u64).to_le_bytes()].concat());
    let mut b: [u8; 16] = h[..16].try_into().unwrap();
    b[6] = (b[6] & 0x0f) | 0x80; // custom (v8) UUID
    b[8] = (b[8] & 0x3f) | 0x80;
    Uuid::from_bytes(b)
}

/// Splits element-carrying bodies whose encoding would exceed the per-op
/// payload budget. Other bodies are returned unchanged.
fn split_body(body: DocumentOperation) -> Vec<DocumentOperation> {
    let size = |b: &DocumentOperation| {
        encode_txn(&Txn {
            target: TxnTarget::Notebook {
                notebook_id: b.notebook_id(),
            },
            ops: vec![LocalOperation {
                id: Uuid::nil(),
                device_id: Uuid::nil(),
                local_sequence: 1,
                schema_version: 1,
                causal_dependencies: vec![],
                at: 0,
                body: b.clone(),
            }],
        })
        .len()
    };
    if body.elements().len() < 2 || size(&body) <= SPLIT_ABOVE {
        return vec![body];
    }
    let halves = match body {
        DocumentOperation::ElementsAdded {
            notebook_id,
            page_id,
            elements,
        } => {
            let (a, b) = elements.split_at(elements.len() / 2);
            vec![
                DocumentOperation::ElementsAdded {
                    notebook_id,
                    page_id,
                    elements: a.to_vec(),
                },
                DocumentOperation::ElementsAdded {
                    notebook_id,
                    page_id,
                    elements: b.to_vec(),
                },
            ]
        }
        DocumentOperation::ElementsReplaced {
            notebook_id,
            page_id,
            elements,
            changed,
        } => {
            let mid = elements.len() / 2;
            vec![
                DocumentOperation::ElementsReplaced {
                    notebook_id,
                    page_id,
                    elements: elements[..mid].to_vec(),
                    changed: changed[..mid].to_vec(),
                },
                DocumentOperation::ElementsReplaced {
                    notebook_id,
                    page_id,
                    elements: elements[mid..].to_vec(),
                    changed: changed[mid..].to_vec(),
                },
            ]
        }
        other => return vec![other],
    };
    halves.into_iter().flat_map(split_body).collect()
}

/// Parses a decrypted checkpoint (`PRCP` v1) into (metadata, record) pairs.
fn parse_checkpoint(b: &[u8]) -> Option<Vec<(OpMeta, Vec<u8>)>> {
    let rd_u32 = |b: &[u8], i: usize| -> Option<u32> {
        Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
    };
    if b.get(..4)? != b"PRCP" || u16::from_le_bytes(b.get(4..6)?.try_into().ok()?) != 1 {
        return None;
    }
    let n = rd_u32(b, 6)? as usize;
    let mut i = 10;
    let mut out = Vec::with_capacity(n.min(1 << 16));
    for _ in 0..n {
        let ml = rd_u32(b, i)? as usize;
        let meta: OpMeta = serde_json::from_slice(b.get(i + 4..i + 4 + ml)?).ok()?;
        i += 4 + ml;
        let rl = rd_u32(b, i)? as usize;
        let rec = b.get(i + 4..i + 4 + rl)?.to_vec();
        i += 4 + rl;
        out.push((meta, rec));
    }
    (i == b.len()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_code_round_trip() {
        let code = pairing_code(
            "3f2a9c1e-0000-4000-8000-000000000000",
            "ABCD-EF01-2345-6789",
        );
        assert_eq!(code, "3F2A9C1E-ABCD-EF01-2345-6789");
        let (p, fp) = parse_pairing_code(&code.to_lowercase()).unwrap();
        assert_eq!(p, "3F2A9C1E");
        assert_eq!(fp, "ABCD-EF01-2345-6789");
        assert!(parse_pairing_code("nope").is_none());
    }
}
