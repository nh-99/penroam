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
    base64url, canonical_aad_bytes, canonical_signing_bytes, verify_ciphertext, AckRequest,
    AckResult, BlobId, Cursor, DeviceId, DeviceRegistrationRequest, EnvelopeHeader, ErrorCode,
    KeyLogAppendRequest, Limits, NotebookRoutingId, OpId, OpRef, PullOpsRequest, RetryPolicy,
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
/// Payload size above which ink operations are split (well below the
/// default 1 MiB per-op limit after encryption overhead).
const SPLIT_ABOVE: usize = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("local storage: {0}")]
    Storage(#[from] StorageError),
    #[error("sync files: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    KeyLog(#[from] KeyLogError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("this account already has encryption keys; add this device by pairing or with the recovery code")]
    AccountHasKeys,
    #[error("this account has no encryption keys yet; set it up on your first device")]
    AccountHasNoKeys,
    #[error("this device is waiting for approval from a trusted device")]
    NotEnrolled,
    #[error("this device was removed from the account; local notes are kept")]
    Revoked,
    #[error("an update is required: {0}")]
    UpgradeRequired(String),
    #[error("integrity problem: {0}")]
    Integrity(String),
    #[error("{0}")]
    Invalid(String),
}

impl SyncError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, SyncError::Transport(t) if t.is_retryable())
    }
}

pub type Result<T> = std::result::Result<T, SyncError>;

/// What the status indicator shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    AwaitingApproval,
    Idle,
    Syncing,
    Offline,
    Backoff,
    Paused,
    NeedsReauth,
    Revoked,
    NeedsUpgrade,
    QuotaExceeded,
    RebaseRequired,
    IntegrityAlert,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub phase: Phase,
    /// Local operations not yet acknowledged by the server.
    pub queued: u64,
    /// Remote operations received but not applied yet (waiting for
    /// dependencies, keys or an update).
    pub pending_remote: usize,
    pub quarantined: usize,
    pub conflicts: usize,
    /// True only when nothing is queued, everything received is applied and
    /// the last pull reached the end of the log.
    pub up_to_date: bool,
    pub last_success_ms: Option<i64>,
    pub last_error: Option<String>,
    pub device_id: String,
    pub account_id: String,
    pub server_url: String,
    pub secret_store: String,
    /// E2EE has not been independently reviewed.
    pub security_review_pending: bool,
}

/// A device as shown in the device list (from the verified key log).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub device_id: String,
    pub label: String,
    pub fingerprint: String,
    pub active: bool,
    pub this_device: bool,
    pub added_by: String,
}

/// Where a new sync connection points.
#[derive(Debug, Clone)]
pub struct Setup {
    pub library_root: PathBuf,
    pub server_url: String,
    pub account_id: String,
    pub label: String,
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileReport {
    pub ingested: usize,
    pub pushed: usize,
    pub pulled: usize,
    pub delivered: usize,
    pub installed_notebooks: Vec<Uuid>,
    pub installed_pages: Vec<(Uuid, Uuid)>,
    pub quarantined: usize,
}

pub struct SyncEngine<T: SyncTransport> {
    pub store: SyncStore,
    transport: T,
    secrets: Box<dyn SecretStore>,
    keys: DeviceKeys,
    my_uuid: Uuid,
    membership: Option<Membership>,
    keylog_raw: Vec<Vec<u8>>,
    epoch_keys: BTreeMap<u32, EpochKey>,
    replicas: HashMap<Uuid, (Replica, usize)>,
    limits: Limits,
    outgoing_blobs: Vec<(Uuid, String, Vec<u8>)>,
    missing_blobs: BTreeSet<(Uuid, String)>,
    pull_done: bool,
    ack_due: Option<Cursor>,
    phase: Phase,
    last_error: Option<String>,
    conflicts: HashMap<Uuid, usize>,
    pub attempt: u32,
    /// Privacy-safe diagnostic events (no content, no keys, no tokens).
    pub events: Vec<String>,
}

fn now_ms() -> i64 {
    penroam_core::now_ms()
}

fn parse_uuid(s: &str) -> Option<Uuid> {
    Uuid::parse_str(s)
        .ok()
        .filter(|u| u.hyphenated().to_string() == s)
}

fn did(s: &str) -> Result<DeviceId> {
    DeviceId::new(s).map_err(|e| SyncError::Invalid(e.to_string()))
}

impl<T: SyncTransport> SyncEngine<T> {
    // ------------------------------------------------------------ setup --

    fn new_device(setup: &Setup, secrets: &dyn SecretStore) -> Result<DeviceKeys> {
        let keys = DeviceKeys::generate(Uuid::new_v4().hyphenated().to_string())?;
        secrets.set(DEVICE_SECRET, &keys.to_secret_string())?;
        let _ = setup;
        Ok(keys)
    }

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

    pub fn device_id(&self) -> &str {
        &self.keys.device_id
    }

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

    fn event(&mut self, e: impl Into<String>) {
        if self.events.len() > 500 {
            self.events.drain(..100);
        }
        self.events.push(format!("{} {}", now_ms(), e.into()));
    }

    // ---------------------------------------------------------- key log --

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

    fn current_epoch(&self) -> Result<(u32, &EpochKey)> {
        let m = self.membership.as_ref().ok_or(SyncError::NotEnrolled)?;
        let k = self.epoch_keys.get(&m.current_epoch).ok_or_else(|| {
            SyncError::Integrity("this device has no key for the current epoch".into())
        })?;
        Ok((m.current_epoch, k))
    }

    // ---------------------------------------------------------- prepare --

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
            self.store.oplog.append_buffered(
                OpMeta {
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

    /// Synthesizes creation operations for every existing notebook (the
    /// library may have checkpointed its history away).
    fn seed(&mut self, lib: &mut Library) -> Result<usize> {
        let mut n = 0;
        let at_seq = lib.last_seq();
        for nb_id in lib.notebook_ids() {
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

    fn epoch_for_local(&self) -> Result<(u32, EpochKey)> {
        let (e, k) = self.current_epoch()?;
        Ok((e, k.clone()))
    }

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
            let (h, ct) = self.seal_local(idx, epoch, &ek)?;
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

    /// Test hook: only the push step of an exchange.
    #[doc(hidden)]
    pub fn push_for_tests(&mut self, report: &mut ReconcileReport) -> Result<()> {
        if self.membership.is_none() {
            self.refresh_keylog()?;
        }
        self.push(report)
    }

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

    /// Verifies a pulled envelope. `Ok(None)` = already known (dedup).
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
        if self
            .store
            .parked("pending")
            .iter()
            .any(|p| p.header.op_id == h.op_id)
        {
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
        let m = self.membership.as_ref().ok_or(SyncError::NotEnrolled)?;
        let Some(member) = m.devices.get(h.device_id.as_str()) else {
            return Ok(Some("unknown-device"));
        };
        let pk = member.cert.sign_pk()?;
        let sig = base64url::decode(&h.signature).unwrap_or_default();
        if crypto::verify(&pk, b"", &canonical_signing_bytes(h), &sig).is_err() {
            self.phase = Phase::IntegrityAlert;
            self.quarantine(server_seq, h, ct, "bad signature", report)?;
            return Ok(None);
        }
        if !self.epoch_keys.contains_key(&h.key_epoch) {
            return Ok(Some("key"));
        }
        Ok(Some("deps"))
    }

    fn deliverable(&self, h: &EnvelopeHeader) -> bool {
        let Some(dev) = parse_uuid(h.device_id.as_str()) else {
            return false;
        };
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
        self.store.state.dirty.insert(nb);
        if let Some(p) = op.body.content_page() {
            self.store.state.page_revs.insert(format!("{nb}/{p}"), 0);
        }
        if let DocumentOperation::PageCreated { page_id, .. } = &op.body {
            self.store
                .state
                .page_revs
                .insert(format!("{nb}/{page_id}"), 0);
        }
        if let DocumentOperation::AssetReferenced { asset, .. } = &op.body {
            self.missing_blobs.insert((nb, asset.sha256.clone()));
        }
        report.delivered += 1;
        Ok(true)
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
                            self.store.unpark("pending", p.header.op_id.as_str())?;
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
                self.store.unpark("pending", &op)?;
            }
            if !progressed {
                return Ok(());
            }
        }
    }

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
        let sig = base64url::decode(&p.header.signature).unwrap_or_default();
        if crypto::verify(&pk, b"", &canonical_signing_bytes(&p.header), &sig).is_err() {
            self.quarantine(p.server_seq, &p.header, ct, "bad signature", report)?;
            return Ok(None);
        }
        if !self.epoch_keys.contains_key(&p.header.key_epoch) {
            return Ok(Some("key"));
        }
        Ok(Some("deps"))
    }

    fn pull(&mut self, report: &mut ReconcileReport) -> Result<()> {
        self.pull_done = false;
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
                    self.phase = Phase::RebaseRequired;
                    return Err(SyncError::Transport(TransportError::Api(b)));
                }
                Err(e) => return Err(e.into()),
            };
            let mut unknown_device = false;
            for (item, ct) in page.response.items.iter().zip(page.ciphertexts.iter()) {
                report.pulled += 1;
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

    fn fetch_blobs(&mut self) {
        let Ok((_, _)) = self.current_epoch() else {
            return;
        };
        let wanted: Vec<(Uuid, String)> = self.missing_blobs.iter().cloned().collect();
        for (nb, sha) in wanted {
            if self.store.blob_cache(&sha).exists() {
                self.missing_blobs.remove(&(nb, sha));
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
            if got {
                self.missing_blobs.remove(&(nb, sha));
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
        Ok(())
    }

    // ------------------------------------------------------------ apply --

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
        let dirty: Vec<Uuid> = self.store.state.dirty.iter().copied().collect();
        for nb in dirty {
            let dirty_pages: BTreeSet<String> =
                self.store.state.page_revs.keys().cloned().collect();
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
                self.store.state.page_revs.remove(&format!("{nb}/{pid}"));
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

    pub fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.store.state.paused = paused;
        self.store.save()?;
        self.phase = if paused { Phase::Paused } else { Phase::Idle };
        Ok(())
    }

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
