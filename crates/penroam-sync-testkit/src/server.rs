//! In-memory fake sync server (development test server, not production).
//!
//! One [`Mutex`] guards the whole state, so every request (including the sharing routes in
//! [`sharing`]) is atomic with respect to every other: sequence assignment, membership checks and
//! the write they guard happen under the same lock. That is what makes "revoke while the member is
//! pushing" and "accept while the owner cancels" race-free here; a real server needs the same
//! property from its database transactions.
//!
//! Accounts come in three kinds, all stored as [`Account`]: ordinary accounts, vaults
//! (`vault_of` set) and sharing spaces (`space` set). A request on a space is made by a *member*
//! ([`Caller::member`], or the member who registered the calling device); [`InMemoryServer`]
//! resolves that member's role before the route runs, answers `not_found` for non-members and
//! `access_revoked` for removed members, and the route then applies role rules. See
//! `docs/protocol/sync-protocol-v1.md` §10 and §12f.

mod comments;
mod history;
mod live;
mod sharing;

pub use comments::CommentPage;

pub use live::{LiveConfig, LivePresence, LiveRefused};

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use penroam_sync_protocol::account::{plan_status, read_only_reasons};
use penroam_sync_protocol::dto::capabilities;
use penroam_sync_protocol::dto::KEYLOG_PAGE_MAX;
use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::framing::{decode_json_batch, encode_json_batch};
use penroam_sync_protocol::hex;
use penroam_sync_protocol::version::PROTOCOL_MAJOR;
use penroam_sync_protocol::{
    verify_ciphertext, AccountId, AccountState, AckRequest, AckResult, BlobId, BlobPutResponse,
    CheckpointRecord, Cursor, DeviceId, DeviceInfo, DeviceListResponse, DeviceRegistrationRequest,
    DeviceRegistrationResponse, EnvelopeHeader, ErrorBody, ErrorCode, KeyLogAppendRequest,
    KeyLogAppendResponse, KeyLogEntry, KeyLogPage, Limits, OpAck, OpId, PlanState, ProtocolRange,
    PullOpsRequest, PullOpsResponse, PulledOp, PushOpsResponse, ServerMeta, StorageState,
    UpdateVaultLockRequest, UsageResponse, VaultListResponse, VaultRecord,
};
use penroam_sync_protocol::{AccountKeyDocument, CreateVaultRequest, Role};

use crate::cursor::{self, CursorError, CursorPayload, STREAM_OPS};

/// A request failure as a client would observe it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The server answered with an error body.
    #[error("{} ({}): {}", .0.code, .0.code.http_status(), .0.message)]
    Api(ErrorBody),
    /// The request was processed (and possibly persisted) but the response
    /// never reached the client — e.g. a timeout after commit.
    #[error("response lost")]
    ResponseLost,
}

impl RequestError {
    /// The error code, if the server answered.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Api(b) => Some(b.code),
            Self::ResponseLost => None,
        }
    }
}

/// A pulled page: the JSON control document plus the framed body whose
/// item headers are [`PulledOp`]s in the same order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPage {
    /// JSON control document.
    pub response: PullOpsResponse,
    /// `PRSB` frame carrying the ciphertexts.
    pub frame: Vec<u8>,
}

mod b64 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&penroam_sync_protocol::base64url::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        penroam_sync_protocol::base64url::decode(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize)]
struct StoredOp {
    header: EnvelopeHeader,
    #[serde(with = "b64")]
    ciphertext: Vec<u8>,
    /// Attribution (`history.v1`): the account whose session pushed it (the member, in a space).
    /// `None` for ops stored before attribution existed (then the account itself is reported).
    #[serde(default)]
    account: Option<AccountId>,
    /// When it was accepted (test clock, Unix ms); 0 when not recorded.
    #[serde(default)]
    accepted_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeviceKeys {
    signing_public_key: String,
    encryption_public_key: String,
    label: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct DeviceState {
    revoked: bool,
    applied_position: Option<u64>,
    #[serde(default)]
    keys: Option<DeviceKeys>,
    /// In a sharing space: the member account that registered this device. Device-session
    /// requests act as that member, and non-owners may only touch their own devices.
    #[serde(default)]
    registered_by: Option<AccountId>,
}

/// Per-request facts computed under the lock before a route runs (never persisted).
#[derive(Default)]
struct RequestCtx {
    /// The caller's role when the account is a sharing space; `None` for ordinary accounts and
    /// vaults, where the caller may do everything.
    role: Option<Role>,
    /// The member making a space request.
    member: Option<AccountId>,
    /// `(quota, bytes stored by the rest of the quota group)`. Spaces count towards their
    /// owner's quota, so pushes and blob uploads check the whole group. `None` outside a request
    /// (then the account's own quota applies).
    quota: Option<(Option<u64>, u64)>,
    /// The account attributed with writes in this request (the member in a space, else the
    /// account), and the time; set by `request()`.
    author: Option<AccountId>,
    now_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlobMeta {
    sha256: String,
    len: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Account {
    devices: BTreeMap<DeviceId, DeviceState>,
    /// `log[i]` has `server_seq = i + 1`.
    log: Vec<StoredOp>,
    #[serde(skip)]
    by_op: HashMap<OpId, u64>,
    #[serde(skip)]
    by_device_seq: HashMap<(DeviceId, u64), OpId>,
    /// Cursors with a position below this are expired (simulated compaction).
    retention_floor: u64,
    quota_bytes: Option<u64>,
    op_bytes: u64,
    /// Opaque key-log entries (base64url), index = position.
    #[serde(default)]
    keylog: Vec<String>,
    #[serde(default)]
    blobs: BTreeMap<BlobId, BlobMeta>,
    #[serde(skip)]
    blob_data: HashMap<BlobId, Vec<u8>>,
    #[serde(default)]
    blob_bytes: u64,
    /// Latest checkpoint record (content is an opaque blob).
    #[serde(default)]
    checkpoint: Option<CheckpointRecord>,
    /// The checkpoint before it (`history.v1`): servers keep two, so a client can fall back when
    /// the latest is corrupt.
    #[serde(default)]
    previous_checkpoint: Option<CheckpointRecord>,
    /// Stored root secret (`account-key.v1`). Vault accounts never have one.
    #[serde(default)]
    root_key: Option<String>,
    /// The account's vaults (`vaults.v1`).
    #[serde(default)]
    vaults: Vec<VaultRecord>,
    /// Set on a vault's own account: the account that owns the vault.
    #[serde(default)]
    vault_of: Option<AccountId>,
    /// Set on a sharing space's own account (`sharing.v1`): owner, members, sealed title.
    #[serde(default)]
    space: Option<sharing::Space>,
    /// The sharing spaces this account owns, oldest first.
    #[serde(default)]
    owned_spaces: Vec<AccountId>,
    /// `POST /v1/shared-notebooks` idempotency keys of this account → the space created.
    #[serde(default)]
    space_creates: BTreeMap<String, AccountId>,
    /// The comment stream (`comments.v1`); empty outside sharing spaces.
    #[serde(default)]
    comments: comments::CommentStream,
    #[serde(skip)]
    ctx: RequestCtx,
}

impl Account {
    /// Recomputes the dedup indexes from the log (after loading from disk).
    fn rebuild_indexes(&mut self) {
        self.by_op.clear();
        self.by_device_seq.clear();
        for (i, op) in self.log.iter().enumerate() {
            let seq = i as u64 + 1;
            self.by_op.insert(op.header.op_id.clone(), seq);
            self.by_device_seq.insert(
                (op.header.device_id.clone(), op.header.device_seq),
                op.header.op_id.clone(),
            );
        }
        self.comments.rebuild_indexes();
    }

    /// Bytes this account stores: operations, blobs and comments.
    fn stored_bytes(&self) -> u64 {
        self.op_bytes + self.blob_bytes + self.comments.bytes
    }

    /// The caller's role, or `None` outside a sharing space.
    fn role(&self) -> Option<Role> {
        self.ctx.role
    }

    /// Whether the caller may change this account's content (always, outside spaces).
    fn may_write(&self) -> bool {
        self.ctx.role.is_none_or(Role::may_write)
    }

    /// Whether storing `len` more bytes would exceed the quota (the owner's, for a space).
    fn over_quota_with(&self, len: u64) -> bool {
        let (quota, elsewhere) = self.ctx.quota.unwrap_or((self.quota_bytes, 0));
        quota.is_some_and(|q| elsewhere + self.stored_bytes() + len > q)
    }
}

/// Who makes a request: the account whose data it touches, and for a sharing space, the member
/// acting in it.
///
/// Every sync method of [`InMemoryServer`] takes `impl Into<Caller>`, so passing a plain
/// `&AccountId` keeps working for ordinary accounts and vaults. For a space, either pass
/// [`Caller::member`] (an account session on the space) or a device the member registered there
/// (a device session; the member is looked up from the device).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caller<'a> {
    /// The account the request is about (an ordinary account, a vault or a space).
    pub account: &'a AccountId,
    /// The member acting in a space, when known from the session.
    pub member: Option<&'a AccountId>,
}

impl<'a> Caller<'a> {
    /// A member's account session on a sharing space.
    pub fn member(space: &'a AccountId, member: &'a AccountId) -> Self {
        Self {
            account: space,
            member: Some(member),
        }
    }
}

impl<'a> From<&'a AccountId> for Caller<'a> {
    fn from(account: &'a AccountId) -> Self {
        Self {
            account,
            member: None,
        }
    }
}

#[derive(Default)]
struct Faults {
    fail_next: VecDeque<ErrorCode>,
    drop_next_responses: u32,
    delay_next: VecDeque<u64>,
}

/// On-disk form of the server state (`state.json`). Faults are not saved.
#[derive(Serialize, Deserialize)]
struct Persisted {
    format: String,
    version: u32,
    secret: String,
    instance_id: String,
    accounts: BTreeMap<AccountId, Account>,
    #[serde(default)]
    invitations: Vec<sharing::Invitation>,
    #[serde(default)]
    emails: BTreeMap<AccountId, String>,
    #[serde(default)]
    email_verified_at: BTreeMap<AccountId, u64>,
    #[serde(default)]
    notifications: Vec<comments::Notification>,
    #[serde(default)]
    notification_prefs: BTreeMap<AccountId, penroam_sync_protocol::comments::NotificationPrefs>,
}

const STATE_FORMAT: &str = "penroam-sync-testserver";

struct State {
    accounts: BTreeMap<AccountId, Account>,
    /// Shared-notebook invitations, oldest first.
    invitations: Vec<sharing::Invitation>,
    /// Verified (normalized) email per account, set by the test sign-in.
    emails: BTreeMap<AccountId, String>,
    /// When each verified email was last confirmed by a sign-in (test clock, Unix ms).
    email_verified_at: BTreeMap<AccountId, u64>,
    /// Listing and answering invitations needs an email verified within this long (`None`: no
    /// check).
    reauth_window_ms: Option<u64>,
    /// Most live devices one member may register in one space.
    member_device_cap: usize,
    /// Notifications of every account, oldest first (`comments.v1`).
    notifications: Vec<comments::Notification>,
    /// Notification mutes per account.
    notification_prefs: BTreeMap<AccountId, penroam_sync_protocol::comments::NotificationPrefs>,
    /// Live channel sessions and knobs (never persisted).
    live: live::LiveState,
    /// Test clock override in Unix milliseconds (never persisted).
    clock_ms: Option<u64>,
    faults: Faults,
    request_counter: u64,
}

/// Protocol-compatible in-memory **fake** server (thread-safe).
///
/// Behaviour mirrors the v1 contract: framed batch push with idempotency on
/// `(account, op_id)`, length/SHA-256 verification, strictly increasing
/// per-account `server_seq` assigned under one lock per accepted op, stable
/// cursor pagination, per-device applied-cursor acks, usage counters and
/// fault injection. Authentication is simulated: callers pass the account
/// and device they are "logged in" as.
///
/// **Development test server.** It lacks production authentication,
/// billing, rate limiting and hardening; never expose it to untrusted
/// networks.
pub struct InMemoryServer {
    state: Mutex<State>,
    secret: [u8; 32],
    instance_id: String,
    limits: Limits,
    /// Advertise the key-log and blob capabilities. Off for the deterministic
    /// fixture server so the pinned `meta.json` keeps its capability list.
    extended: bool,
    /// Directory of `state.json` and blob files when persistent.
    dir: Option<PathBuf>,
}

impl Default for InMemoryServer {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryServer {
    /// New server with a random cursor secret and default limits.
    pub fn new() -> Self {
        let mut secret = [0u8; 32];
        secret[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        secret[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        let instance = uuid::Uuid::new_v4().simple().to_string();
        let mut s = Self::with_secret(secret, format!("testkit-{instance}"), Limits::DEFAULT);
        s.extended = true;
        s
    }

    /// Opens (or creates) a persistent server in `dir`. Every successful
    /// mutation is written to `dir/state.json` (blobs to `dir/blobs/`)
    /// atomically before the call returns, so a killed process never loses
    /// an acknowledged op, key-log entry or blob. The cursor secret is
    /// stored too, so cursors stay valid across restarts.
    pub fn open_persistent(dir: &Path) -> std::io::Result<InMemoryServer> {
        std::fs::create_dir_all(dir.join("blobs"))?;
        let path = dir.join("state.json");
        let mut server = InMemoryServer::new();
        server.dir = Some(dir.to_path_buf());
        if path.exists() {
            let bytes = std::fs::read(&path)?;
            let p: Persisted = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if p.format != STATE_FORMAT || p.version != 1 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unsupported test server state",
                ));
            }
            let secret = hex::decode(&p.secret)
                .filter(|s| s.len() == 32)
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "bad secret")
                })?;
            server.secret.copy_from_slice(&secret);
            server.instance_id = p.instance_id;
            let mut accounts = p.accounts;
            for (id, a) in accounts.iter_mut() {
                a.rebuild_indexes();
                for (blob, meta) in &a.blobs {
                    let data = std::fs::read(Self::blob_path(dir, id, blob))?;
                    if sha256_hex(&data) != meta.sha256 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("blob {blob} does not match its digest"),
                        ));
                    }
                    a.blob_data.insert(blob.clone(), data);
                }
            }
            let mut st = server.lock();
            st.accounts = accounts;
            st.invitations = p.invitations;
            st.emails = p.emails;
            st.email_verified_at = p.email_verified_at;
            st.notifications = p.notifications;
            st.notification_prefs = p.notification_prefs;
            drop(st);
        } else {
            server.save(&server.lock())?;
        }
        Ok(server)
    }

    fn blob_path(dir: &Path, account: &AccountId, blob: &BlobId) -> PathBuf {
        dir.join("blobs").join(format!("{account}--{blob}"))
    }

    fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            if let Ok(d) = std::fs::File::open(parent) {
                let _ = d.sync_all();
            }
        }
        Ok(())
    }

    /// Writes `state.json` when persistent (caller holds the lock).
    fn save(&self, state: &State) -> std::io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        Self::write_atomic(&dir.join("state.json"), &self.snapshot(state)?)
    }

    /// SHA-256 (hex) of everything the server would persist: accounts, logs, devices, key logs,
    /// blob metadata, checkpoints, spaces, invitations and verified emails. Tests compare it
    /// before and after a refused request to show the refusal had no side effects.
    pub fn state_fingerprint(&self) -> String {
        let s = self.lock();
        sha256_hex(&self.snapshot(&s).expect("state serializes"))
    }

    /// The `state.json` bytes for `state`.
    fn snapshot(&self, state: &State) -> std::io::Result<Vec<u8>> {
        #[derive(Serialize)]
        struct View<'a> {
            format: &'a str,
            version: u32,
            secret: String,
            instance_id: &'a str,
            accounts: &'a BTreeMap<AccountId, Account>,
            invitations: &'a [sharing::Invitation],
            emails: &'a BTreeMap<AccountId, String>,
            email_verified_at: &'a BTreeMap<AccountId, u64>,
            notifications: &'a [comments::Notification],
            notification_prefs:
                &'a BTreeMap<AccountId, penroam_sync_protocol::comments::NotificationPrefs>,
        }
        serde_json::to_vec(&View {
            format: STATE_FORMAT,
            version: 1,
            secret: hex::encode(&self.secret),
            instance_id: &self.instance_id,
            accounts: &state.accounts,
            invitations: &state.invitations,
            emails: &state.emails,
            email_verified_at: &state.email_verified_at,
            notifications: &state.notifications,
            notification_prefs: &state.notification_prefs,
        })
        .map_err(std::io::Error::other)
    }

    /// Persist after an administrative change (panics only if the disk fails,
    /// which a test server reports loudly).
    fn save_admin(&self, state: &State) {
        if let Err(e) = self.save(state) {
            panic!("test server could not persist its state: {e}");
        }
    }

    /// Deterministic server (fixtures, reproducible tests).
    pub fn with_secret(secret: [u8; 32], instance_id: impl Into<String>, limits: Limits) -> Self {
        Self {
            state: Mutex::new(State {
                accounts: BTreeMap::new(),
                invitations: Vec::new(),
                emails: BTreeMap::new(),
                email_verified_at: BTreeMap::new(),
                reauth_window_ms: None,
                member_device_cap: sharing::DEFAULT_MEMBER_DEVICE_CAP,
                live: live::LiveState::default(),
                notifications: Vec::new(),
                notification_prefs: BTreeMap::new(),
                clock_ms: None,
                faults: Faults::default(),
                request_counter: 0,
            }),
            secret,
            instance_id: instance_id.into(),
            limits: limits.clamped(),
            extended: false,
            dir: None,
        }
    }

    /// Locks the state, recovering from poisoning (a panicking test thread
    /// must not wedge every later request).
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---- administration (not protocol endpoints) ----

    /// Create an empty account (idempotent).
    pub fn create_account(&self, account: &AccountId) {
        let mut s = self.lock();
        if !s.accounts.contains_key(account) {
            s.accounts.insert(account.clone(), Account::default());
            self.save_admin(&s);
        }
    }

    /// Whether `device` is registered under `account` (revoked or not).
    pub fn device_registered(&self, account: &AccountId, device: &DeviceId) -> bool {
        self.lock()
            .accounts
            .get(account)
            .is_some_and(|a| a.devices.contains_key(device))
    }

    /// Register a device under an account (idempotent; un-revokes).
    pub fn register_device(&self, account: &AccountId, device: &DeviceId) {
        let mut s = self.lock();
        let a = s.accounts.entry(account.clone()).or_default();
        a.devices.entry(device.clone()).or_default().revoked = false;
        self.save_admin(&s);
    }

    /// Revoke a device.
    pub fn revoke_device(&self, account: &AccountId, device: &DeviceId) {
        let mut s = self.lock();
        if let Some(d) = s
            .accounts
            .get_mut(account)
            .and_then(|a| a.devices.get_mut(device))
        {
            d.revoked = true;
            self.save_admin(&s);
            drop(s);
            self.live_device_revoked(account, device);
        }
    }

    /// Set an account byte quota over stored op ciphertext.
    pub fn set_quota(&self, account: &AccountId, quota_bytes: Option<u64>) {
        let mut s = self.lock();
        if let Some(a) = s.accounts.get_mut(account) {
            a.quota_bytes = quota_bytes;
            self.save_admin(&s);
        }
    }

    /// Simulates compaction up to the latest checkpoint: cursors before the
    /// position it covers expire (`cursor_expired`), so lagging devices must
    /// recover from the checkpoint. Returns the new floor (0 = no checkpoint).
    pub fn compact_to_latest_checkpoint(&self, account: &AccountId) -> u64 {
        let floor = {
            let s = self.lock();
            s.accounts
                .get(account)
                .and_then(|a| a.checkpoint.as_ref().map(|c| c.covers_server_seq))
                .unwrap_or(0)
        };
        if floor > 0 {
            self.expire_cursors_before(account, floor);
        }
        floor
    }

    /// Simulates compaction the way `history.v1` servers retain history: only up to the **older**
    /// of the two kept checkpoints (the latest if there is only one), so the previous checkpoint
    /// plus its tail stays usable. Returns the new floor (0 = no checkpoint).
    pub fn compact_to_retained_checkpoints(&self, account: &AccountId) -> u64 {
        let floor = {
            let s = self.lock();
            s.accounts
                .get(account)
                .and_then(|a| {
                    a.previous_checkpoint
                        .as_ref()
                        .or(a.checkpoint.as_ref())
                        .map(|c| c.covers_server_seq)
                })
                .unwrap_or(0)
        };
        if floor > 0 {
            self.expire_cursors_before(account, floor);
        }
        floor
    }

    /// Simulate compaction: cursors positioned before `server_seq` now fail
    /// with `cursor_expired`, and a pull without a cursor fails with
    /// `checkpoint_required`.
    pub fn expire_cursors_before(&self, account: &AccountId, server_seq: u64) {
        let mut s = self.lock();
        if let Some(a) = s.accounts.get_mut(account) {
            a.retention_floor = a.retention_floor.max(server_seq);
            self.save_admin(&s);
        }
    }

    /// The `server_seq` a device has acknowledged as durably applied.
    pub fn applied_position(&self, account: &AccountId, device: &DeviceId) -> Option<u64> {
        self.lock()
            .accounts
            .get(account)
            .and_then(|a| a.devices.get(device))
            .and_then(|d| d.applied_position)
    }

    /// Number of ops stored for an account.
    pub fn op_count(&self, account: &AccountId) -> u64 {
        self.lock()
            .accounts
            .get(account)
            .map_or(0, |a| a.log.len() as u64)
    }

    // ---- fault injection ----

    /// The next `n` requests (any endpoint except [`Self::meta`]) fail with
    /// `code` **without** being processed.
    pub fn fail_next_requests(&self, n: u32, code: ErrorCode) {
        let mut s = self.lock();
        for _ in 0..n {
            s.faults.fail_next.push_back(code);
        }
    }

    /// The next `n` requests are processed (and persisted) normally but the
    /// caller receives [`RequestError::ResponseLost`].
    pub fn drop_next_responses(&self, n: u32) {
        self.lock().faults.drop_next_responses += n;
    }

    /// The HTTP layer sleeps `ms` before answering each of the next `n`
    /// requests (simulated slow network/server). The in-process API ignores it.
    pub fn delay_next_responses(&self, n: u32, ms: u64) {
        let mut s = self.lock();
        for _ in 0..n {
            s.faults.delay_next.push_back(ms);
        }
    }

    /// Next pending delay for the HTTP layer, if any.
    pub(crate) fn take_delay(&self) -> Option<u64> {
        self.lock().faults.delay_next.pop_front()
    }

    /// Effective limits.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    // ---- protocol endpoints ----

    /// `GET /v1/meta`.
    pub fn meta(&self) -> ServerMeta {
        ServerMeta {
            min_client_protocol: ProtocolRange::CLIENT.min,
            max_client_protocol: ProtocolRange::CLIENT.max,
            capabilities: {
                let mut c: Vec<String> = vec![
                    capabilities::OPS_BATCH_V1.into(),
                    capabilities::OPS_PULL_V1.into(),
                    capabilities::ACKS_V1.into(),
                    capabilities::USAGE_V1.into(),
                ];
                if self.extended {
                    c.push(capabilities::KEYLOG_V1.into());
                    c.push(capabilities::BLOBS_V1.into());
                    c.push(capabilities::CHECKPOINTS_V1.into());
                    c.push(capabilities::ACCOUNT_V1.into());
                    c.push(capabilities::ACCOUNT_KEY_V1.into());
                    c.push(capabilities::VAULTS_V1.into());
                    c.push(capabilities::SHARING_V1.into());
                    c.push(capabilities::LIVE_V1.into());
                    c.push(capabilities::COMMENTS_V1.into());
                    c.push(capabilities::HISTORY_V1.into());
                }
                c
            },
            limits: self.limits,
            server_instance_id: self.instance_id.clone(),
        }
    }

    /// Run `f` as one request: request id, injected failures, auth check,
    /// response dropping.
    fn request<T>(
        &self,
        caller: Caller<'_>,
        device: Option<&DeviceId>,
        mutates: bool,
        f: impl FnOnce(
            &mut Account,
            &mut dyn FnMut(ErrorCode, String) -> ErrorBody,
        ) -> Result<T, ErrorBody>,
    ) -> Result<T, RequestError> {
        let mut guard = self.lock();
        let s: &mut State = &mut guard;
        s.request_counter += 1;
        let request_id = format!("req-{}", s.request_counter);
        let mut err = |code: ErrorCode, message: String| {
            let mut b = ErrorBody::new(code, message, request_id.clone());
            if code == ErrorCode::RateLimited || code == ErrorCode::ServiceUnavailable {
                b.retry_after_ms = Some(1000);
            }
            b
        };
        if let Some(code) = s.faults.fail_next.pop_front() {
            return Err(RequestError::Api(err(code, "injected failure".into())));
        }
        let account = caller.account;
        let Some(acct) = s.accounts.get(account) else {
            return Err(RequestError::Api(err(
                ErrorCode::Unauthenticated,
                "unknown account".into(),
            )));
        };
        // Membership is checked before the device: a removed member gets `access_revoked`
        // everywhere, even though their devices in the space are revoked too.
        let (role, member) = match &acct.space {
            Some(space) => {
                let (role, member) = space
                    .resolve(acct, caller.member, device)
                    .map_err(|(code, m)| RequestError::Api(err(code, m.into())))?;
                (Some(role), Some(member))
            }
            None if caller.member.is_some() => {
                return Err(RequestError::Api(err(
                    ErrorCode::Forbidden,
                    "not a shared notebook".into(),
                )))
            }
            None => (None, None),
        };
        let quota = Self::quota_group(&s.accounts, account);
        let now_ms = sharing::now_ms(s);
        let acct = s.accounts.get_mut(account).expect("looked up above");
        acct.ctx = RequestCtx {
            role,
            author: Some(member.clone().unwrap_or_else(|| account.clone())),
            member,
            quota: Some(quota),
            now_ms,
        };
        if let Some(device) = device {
            match acct.devices.get(device) {
                None => {
                    return Err(RequestError::Api(err(
                        ErrorCode::Forbidden,
                        "device not registered".into(),
                    )))
                }
                Some(d) if d.revoked => {
                    return Err(RequestError::Api(err(
                        ErrorCode::DeviceRevoked,
                        "device revoked".into(),
                    )))
                }
                Some(_) => {}
            }
        }
        let result = f(acct, &mut err).map_err(RequestError::Api);
        acct.ctx = RequestCtx::default();
        if mutates && result.is_ok() {
            if let Err(e) = self.save(s) {
                return Err(RequestError::Api(err(
                    ErrorCode::InternalError,
                    format!("could not persist: {e}"),
                )));
            }
        }
        if s.faults.drop_next_responses > 0 {
            s.faults.drop_next_responses -= 1;
            return Err(RequestError::ResponseLost);
        }
        result
    }

    /// `POST /v1/ops/batch` with a framed body of [`EnvelopeHeader`]s.
    pub fn push<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        body: &[u8],
    ) -> Result<PushOpsResponse, RequestError> {
        let caller = caller.into();
        let limits = self.limits;
        let result = self.request(caller, Some(device), true, |acct, err| {
            if !acct.may_write() {
                return Err(err(
                    ErrorCode::Forbidden,
                    "viewers cannot change a shared notebook".into(),
                ));
            }
            let items: Vec<(EnvelopeHeader, Vec<u8>)> =
                decode_json_batch(body, &limits).map_err(|e| err(e.error_code(), e.to_string()))?;
            let acks = items
                .into_iter()
                .map(|(header, ciphertext)| {
                    let op_id = header.op_id.clone();
                    let result = match Self::accept(acct, device, header, ciphertext, &limits) {
                        Ok(r) => r,
                        Err((code, message)) => AckResult::Rejected {
                            error: err(code, message),
                        },
                    };
                    OpAck { op_id, result }
                })
                .collect();
            Ok(PushOpsResponse { acks })
        });
        // After the commit (even if the response is lost): tell live sessions to pull.
        self.live_notify_commit(caller.account);
        result
    }

    /// **Test only.** Stores envelopes the way a misbehaving server would:
    /// without checking that the pushing device is still allowed to write
    /// (revocation is ignored; all other validation still applies). Used to
    /// show that clients reject writes from removed devices on their own. Not
    /// persisted and not reachable over HTTP.
    #[doc(hidden)]
    pub fn push_ignoring_revocation(
        &self,
        account: &AccountId,
        device: &DeviceId,
        body: &[u8],
    ) -> Result<PushOpsResponse, RequestError> {
        let limits = self.limits;
        let api = |code: ErrorCode, m: String| RequestError::Api(ErrorBody::new(code, m, "test"));
        let items: Vec<(EnvelopeHeader, Vec<u8>)> =
            decode_json_batch(body, &limits).map_err(|e| api(e.error_code(), e.to_string()))?;
        let mut st = self.lock();
        let acct = st
            .accounts
            .get_mut(account)
            .ok_or_else(|| api(ErrorCode::NotFound, "no such account".into()))?;
        let acks = items
            .into_iter()
            .map(|(header, ciphertext)| {
                let op_id = header.op_id.clone();
                let result = match Self::accept(acct, device, header, ciphertext, &limits) {
                    Ok(r) => r,
                    Err((code, message)) => AckResult::Rejected {
                        error: ErrorBody::new(code, message, "test"),
                    },
                };
                OpAck { op_id, result }
            })
            .collect();
        Ok(PushOpsResponse { acks })
    }

    /// Validates and stores one pushed envelope under the account lock, in
    /// this order: header validation, the header names the authenticated
    /// device, ciphertext length/hash, dedup by op id (identical → duplicate,
    /// different → `op_id_conflict`), reused (device, sequence), quota; then it
    /// assigns the next gap-free `server_seq`.
    fn accept(
        acct: &mut Account,
        device: &DeviceId,
        header: EnvelopeHeader,
        ciphertext: Vec<u8>,
        limits: &Limits,
    ) -> Result<AckResult, (ErrorCode, String)> {
        header
            .validate(limits)
            .map_err(|e| (e.error_code(), e.to_string()))?;
        if &header.device_id != device {
            return Err((
                ErrorCode::Forbidden,
                "envelope device_id is not the authenticated device".into(),
            ));
        }
        verify_ciphertext(&header, &ciphertext).map_err(|e| (e.error_code(), e.to_string()))?;
        if let Some(&seq) = acct.by_op.get(&header.op_id) {
            let existing = &acct.log[(seq - 1) as usize];
            // Identical content (the signed fields) => idempotent retry.
            return if existing.header.ciphertext_sha256 == header.ciphertext_sha256
                && existing.header.ciphertext_length == header.ciphertext_length
                && existing.header == header
            {
                Ok(AckResult::Duplicate { server_seq: seq })
            } else {
                Err((
                    ErrorCode::OpIdConflict,
                    "op_id already used with different content".into(),
                ))
            };
        }
        let key = (header.device_id.clone(), header.device_seq);
        if acct.by_device_seq.contains_key(&key) {
            return Err((
                ErrorCode::OpIdConflict,
                "device_seq already used by a different op_id".into(),
            ));
        }
        let len = ciphertext.len() as u64;
        if acct.over_quota_with(len) {
            return Err((ErrorCode::QuotaExceeded, "account quota exceeded".into()));
        }
        // Single lock held: sequence assignment and insert are atomic.
        let seq = acct.log.len() as u64 + 1;
        acct.by_op.insert(header.op_id.clone(), seq);
        acct.by_device_seq.insert(key, header.op_id.clone());
        acct.op_bytes += len;
        acct.log.push(StoredOp {
            header,
            ciphertext,
            account: acct.ctx.author.clone(),
            accepted_at_ms: acct.ctx.now_ms,
        });
        Ok(AckResult::Accepted { server_seq: seq })
    }

    /// A cursor for "after `position`" in this account's op stream.
    fn mint(&self, account: &AccountId, position: u64) -> Cursor {
        cursor::mint(
            &self.secret,
            &CursorPayload {
                account: account.to_string(),
                stream: STREAM_OPS,
                protocol_major: PROTOCOL_MAJOR,
                position,
            },
        )
    }

    /// Verifies a client cursor (MAC, account/stream/version binding, bounds,
    /// retention floor) and returns its position.
    fn open_cursor(
        &self,
        account: &AccountId,
        acct: &Account,
        c: &Cursor,
    ) -> Result<u64, (ErrorCode, &'static str)> {
        let p = cursor::open(&self.secret, c)
            .map_err(|CursorError::Invalid| (ErrorCode::InvalidCursor, "invalid cursor"))?;
        if p.account != account.as_str()
            || p.stream != STREAM_OPS
            || p.protocol_major != PROTOCOL_MAJOR
        {
            return Err((
                ErrorCode::InvalidCursor,
                "cursor is bound to another account/stream/version",
            ));
        }
        if p.position > acct.log.len() as u64 {
            return Err((ErrorCode::InvalidCursor, "cursor beyond end of log"));
        }
        if p.position < acct.retention_floor {
            return Err((
                ErrorCode::CursorExpired,
                "cursor expired; fetch a checkpoint",
            ));
        }
        Ok(p.position)
    }

    /// `GET /v1/ops`.
    pub fn pull<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        req: &PullOpsRequest,
    ) -> Result<PullPage, RequestError> {
        let caller = caller.into();
        let account = caller.account;
        let limits = self.limits;
        self.request(caller, Some(device), false, |acct, err| {
            if req.limit == 0 {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    "limit must be >= 1".into(),
                ));
            }
            let start = match &req.cursor {
                Some(c) => self
                    .open_cursor(account, acct, c)
                    .map_err(|(code, m)| err(code, m.into()))?,
                None if acct.retention_floor > 0 => {
                    return Err(err(
                        ErrorCode::CheckpointRequired,
                        "log compacted; fetch a checkpoint".into(),
                    ))
                }
                None => 0,
            };
            let limit = req.limit.min(limits.max_pull_page_size) as usize;
            let mut items = Vec::new();
            let mut frame_items = Vec::new();
            let mut bytes = penroam_sync_protocol::framing::PREAMBLE_LEN as u64;
            for (i, op) in acct.log.iter().enumerate().skip(start as usize).take(limit) {
                let pulled = PulledOp {
                    server_seq: i as u64 + 1,
                    header: op.header.clone(),
                };
                let hlen = serde_json::to_vec(&pulled).map(|v| v.len()).unwrap_or(0) as u64;
                let item = 8 + hlen + op.ciphertext.len() as u64;
                if !items.is_empty() && bytes + item > limits.max_batch_bytes {
                    break;
                }
                bytes += item;
                frame_items.push((pulled.clone(), op.ciphertext.clone()));
                items.push(pulled);
            }
            let end = start + items.len() as u64;
            let frame = encode_json_batch(&frame_items, &limits)
                .map_err(|e| err(ErrorCode::InternalError, e.to_string()))?;
            Ok(PullPage {
                response: PullOpsResponse {
                    items,
                    next_cursor: self.mint(account, end),
                    has_more: end < acct.log.len() as u64,
                },
                frame,
            })
        })
    }

    /// `POST /v1/acks`. Applied positions only move forward.
    pub fn ack<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        req: &AckRequest,
    ) -> Result<(), RequestError> {
        let caller = caller.into();
        let account = caller.account;
        self.request(caller, Some(device), true, |acct, err| {
            let pos = self
                .open_cursor(account, acct, &req.applied_cursor)
                .map_err(|(code, m)| err(code, m.into()))?;
            let d = acct.devices.get_mut(device).expect("checked by request()");
            d.applied_position = Some(d.applied_position.map_or(pos, |p| p.max(pos)));
            Ok(())
        })
    }

    /// `GET /v1/usage`.
    pub fn usage<'c>(&self, caller: impl Into<Caller<'c>>) -> Result<UsageResponse, RequestError> {
        let caller = caller.into();
        let account = caller.account;
        self.request(caller, None, false, |acct, _| {
            Ok(UsageResponse {
                account_id: account.clone(),
                op_count: acct.log.len() as u64,
                op_bytes: acct.op_bytes,
                blob_count: acct.blobs.len() as u64,
                blob_bytes: acct.blob_bytes,
                quota_bytes: acct.ctx.quota.map_or(acct.quota_bytes, |(q, _)| q),
            })
        })
    }

    /// `GET /v1/account` (capability `account.v1`). The test server sells nothing: every
    /// account is on a free plan whose allowance is the account's quota, no actions are offered,
    /// and the account is read-only (`over_quota`) once its stored bytes reach the quota.
    pub fn account_state<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
    ) -> Result<AccountState, RequestError> {
        let caller = caller.into();
        let account = caller.account;
        self.request(caller, None, false, |acct, _| {
            // Spaces count towards their owner's allowance, so report the whole group.
            let (quota, elsewhere) = acct.ctx.quota.unwrap_or((acct.quota_bytes, 0));
            let used = elsewhere + acct.stored_bytes();
            let over = quota.is_some_and(|q| used >= q);
            Ok(AccountState {
                account_id: account.clone(),
                plan: PlanState {
                    id: "free".into(),
                    name: "Test server".into(),
                    status: plan_status::FREE.into(),
                    renews_at_unix_s: None,
                    ends_at_unix_s: None,
                    managed_in: None,
                },
                storage: StorageState {
                    used_bytes: used,
                    quota_bytes: quota,
                    free_quota_bytes: quota,
                },
                writable: !over,
                read_only_reason: over.then(|| read_only_reasons::OVER_QUOTA.into()),
                offers: Vec::new(),
                actions: Vec::new(),
                deletion: None,
                store_account_token: None,
            })
        })
    }

    // ---- devices, key log, blobs ----

    /// `POST /v1/devices`: register a device with its public keys. The id is
    /// `req.device_id` or a generated one. Re-registering the same id with
    /// the same keys is idempotent; different keys are a `conflict`; a
    /// revoked id stays revoked (`device_revoked`). In a sharing space a member may have at
    /// most [`Self::set_member_device_cap`] live devices; one more is `conflict`.
    pub fn register_device_keys<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        req: &DeviceRegistrationRequest,
    ) -> Result<DeviceRegistrationResponse, RequestError> {
        let caller = caller.into();
        let cap = self.lock().member_device_cap;
        self.request(caller, None, true, |acct, err| {
            req.validate()
                .map_err(|field| err(ErrorCode::ValidationFailed, format!("invalid {field}")))?;
            let id = match &req.device_id {
                Some(id) => id.clone(),
                None => DeviceId::new(format!("dev-{}", uuid::Uuid::new_v4().simple()))
                    .expect("generated ids are valid"),
            };
            let keys = DeviceKeys {
                signing_public_key: req.signing_public_key.clone(),
                encryption_public_key: req.encryption_public_key.clone(),
                label: req.label.clone(),
            };
            let member = acct.ctx.member.clone();
            // In a space, a device id belongs to the member who registered it first.
            if let (Some(m), Some(d)) = (&member, acct.devices.get(&id)) {
                if d.registered_by.as_ref() != Some(m) {
                    return Err(err(
                        ErrorCode::Forbidden,
                        "device registered by another member".into(),
                    ));
                }
            }
            match acct.devices.get_mut(&id) {
                Some(d) if d.revoked => {
                    return Err(err(ErrorCode::DeviceRevoked, "device revoked".into()))
                }
                Some(d) => match &d.keys {
                    Some(k)
                        if k.signing_public_key != keys.signing_public_key
                            || k.encryption_public_key != keys.encryption_public_key =>
                    {
                        return Err(err(
                            ErrorCode::Conflict,
                            "device id already registered with different keys".into(),
                        ))
                    }
                    _ => d.keys = Some(keys),
                },
                None => {
                    if let Some(m) = &member {
                        let live = acct
                            .devices
                            .values()
                            .filter(|d| !d.revoked && d.registered_by.as_ref() == Some(m))
                            .count();
                        if live >= cap {
                            return Err(err(
                                ErrorCode::Conflict,
                                "too many devices in this shared notebook; remove one first".into(),
                            ));
                        }
                    }
                    acct.devices.insert(
                        id.clone(),
                        DeviceState {
                            keys: Some(keys),
                            registered_by: member,
                            ..Default::default()
                        },
                    );
                }
            }
            Ok(DeviceRegistrationResponse { device_id: id })
        })
    }

    /// `GET /v1/devices`.
    pub fn list_devices<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
    ) -> Result<DeviceListResponse, RequestError> {
        let caller = caller.into();
        self.request(caller, None, false, |acct, _| {
            Ok(DeviceListResponse {
                devices: acct
                    .devices
                    .iter()
                    .map(|(id, d)| DeviceInfo {
                        device_id: id.clone(),
                        signing_public_key: d
                            .keys
                            .as_ref()
                            .map(|k| k.signing_public_key.clone())
                            .unwrap_or_default(),
                        encryption_public_key: d
                            .keys
                            .as_ref()
                            .map(|k| k.encryption_public_key.clone())
                            .unwrap_or_default(),
                        label: d.keys.as_ref().and_then(|k| k.label.clone()),
                        revoked: d.revoked,
                    })
                    .collect(),
            })
        })
    }

    /// `DELETE /v1/devices/{id}` by an authenticated, non-revoked device.
    pub fn revoke_device_as<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        caller_device: &DeviceId,
        target: &DeviceId,
    ) -> Result<(), RequestError> {
        let caller = caller.into();
        let result = self.request(caller, Some(caller_device), true, |acct, err| {
            let member = acct.ctx.member.clone();
            let owner = acct.role().is_none_or(|r| r == Role::Owner);
            let d = acct
                .devices
                .get_mut(target)
                .ok_or_else(|| err(ErrorCode::NotFound, "unknown device".into()))?;
            // In a space, only the owner may revoke another member's device.
            if !owner && d.registered_by != member {
                return Err(err(
                    ErrorCode::Forbidden,
                    "only the owner may remove another member's device".into(),
                ));
            }
            d.revoked = true;
            Ok(())
        });
        if result.is_ok() {
            self.live_device_revoked(caller.account, target);
        }
        result
    }

    /// `GET /v1/keylog?from=N` (at most [`KEYLOG_PAGE_MAX`] entries).
    pub fn keylog_get<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        from: u64,
    ) -> Result<KeyLogPage, RequestError> {
        let caller = caller.into();
        self.request(caller, None, false, |acct, _| {
            let entries = acct
                .keylog
                .iter()
                .enumerate()
                .skip(from.min(acct.keylog.len() as u64) as usize)
                .take(KEYLOG_PAGE_MAX)
                .map(|(i, data)| KeyLogEntry {
                    index: i as u64,
                    data: data.clone(),
                })
                .collect();
            Ok(KeyLogPage {
                entries,
                next_index: acct.keylog.len() as u64,
            })
        })
    }

    /// `POST /v1/checkpoints`: stores a checkpoint record after checking that
    /// it names the authenticated device, that its blob exists with the
    /// declared hash and length, that its cursor is valid for this account
    /// and matches `covers_server_seq`, and that it does not move backwards.
    /// The signature is for clients; the server cannot verify it. In a sharing space only the
    /// owner may publish one (`forbidden` for editors and viewers).
    pub fn checkpoint_put<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        rec: &CheckpointRecord,
    ) -> Result<(), RequestError> {
        let caller = caller.into();
        let account = caller.account;
        self.request(caller, Some(device), true, |acct, err| {
            // In a space only the owner publishes checkpoints: a checkpoint replaces history for
            // lagging devices, and an editor could otherwise publish one that leaves out other
            // members' operations.
            if acct.role().is_some_and(|r| r != Role::Owner) {
                return Err(err(
                    ErrorCode::Forbidden,
                    "only the owner may publish a shared notebook's checkpoint".into(),
                ));
            }
            if &rec.created_by != device {
                return Err(err(
                    ErrorCode::Forbidden,
                    "checkpoint must be created by the authenticated device".into(),
                ));
            }
            match acct.blobs.get(&rec.blob_id) {
                Some(b) if b.sha256 == rec.blob_sha256 && b.len == rec.blob_length => {}
                Some(_) => {
                    return Err(err(
                        ErrorCode::IntegrityMismatch,
                        "checkpoint blob hash or length differs".into(),
                    ))
                }
                None => {
                    return Err(err(
                        ErrorCode::NotFound,
                        "upload the checkpoint blob first".into(),
                    ))
                }
            }
            let pos = self
                .open_cursor(account, acct, &rec.covers_cursor)
                .map_err(|(code, m)| err(code, m.into()))?;
            if pos != rec.covers_server_seq {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    "covers_server_seq does not match covers_cursor".into(),
                ));
            }
            if acct
                .checkpoint
                .as_ref()
                .is_some_and(|c| c.covers_server_seq > rec.covers_server_seq)
            {
                return Err(err(ErrorCode::Conflict, "a newer checkpoint exists".into()));
            }
            // Keep the two newest: a checkpoint covering more pushes the latest down; one
            // covering the same position replaces the latest only.
            if let Some(old) = acct.checkpoint.take() {
                if old.covers_server_seq < rec.covers_server_seq {
                    acct.previous_checkpoint = Some(old);
                }
            }
            acct.checkpoint = Some(rec.clone());
            Ok(())
        })
    }

    /// `GET /v1/checkpoints/latest`: the latest record, or `not_found`.
    pub fn checkpoint_latest<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
    ) -> Result<CheckpointRecord, RequestError> {
        let caller = caller.into();
        self.request(caller, Some(device), false, |acct, err| {
            acct.checkpoint
                .clone()
                .ok_or_else(|| err(ErrorCode::NotFound, "no checkpoint yet".into()))
        })
    }

    /// `POST /v1/keylog`: append at exactly the current length.
    pub fn keylog_append<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        req: &KeyLogAppendRequest,
    ) -> Result<KeyLogAppendResponse, RequestError> {
        let caller = caller.into();
        self.request(caller, None, true, |acct, err| {
            req.validate()
                .map_err(|_| err(ErrorCode::ValidationFailed, "invalid key-log entry".into()))?;
            if acct.space.is_some() {
                sharing::check_space_keylog_entry(acct, &req.data)
                    .map_err(|(code, m)| err(code, m.into()))?;
            }
            if req.index != acct.keylog.len() as u64 {
                return Err(err(
                    ErrorCode::Conflict,
                    format!(
                        "key log has {} entries; re-read it before appending",
                        acct.keylog.len()
                    ),
                ));
            }
            acct.keylog.push(req.data.clone());
            Ok(KeyLogAppendResponse { index: req.index })
        })
    }

    /// `GET /v1/account/key`. A vault account is refused: its root secret must never be here.
    pub fn account_key_get(&self, account: &AccountId) -> Result<AccountKeyDocument, RequestError> {
        self.request(account.into(), None, false, |acct, err| {
            if acct.vault_of.is_some() {
                return Err(err(
                    ErrorCode::Forbidden,
                    "vaults have no stored key".into(),
                ));
            }
            if acct.space.is_some() {
                return Err(err(
                    ErrorCode::Forbidden,
                    "a shared notebook's key is at /v1/shared-notebooks/{id}/key".into(),
                ));
            }
            acct.root_key
                .clone()
                .map(|key| AccountKeyDocument { key })
                .ok_or_else(|| err(ErrorCode::NotFound, "no stored key".into()))
        })
    }

    /// `PUT /v1/account/key`: stores the key once. A second store answers `conflict`, so two
    /// first devices settle on whichever got here first.
    pub fn account_key_put(
        &self,
        account: &AccountId,
        doc: &AccountKeyDocument,
    ) -> Result<(), RequestError> {
        self.request(account.into(), None, true, |acct, err| {
            if acct.vault_of.is_some() {
                return Err(err(
                    ErrorCode::Forbidden,
                    "vaults have no stored key".into(),
                ));
            }
            if acct.space.is_some() {
                return Err(err(
                    ErrorCode::Forbidden,
                    "a shared notebook's key is at /v1/shared-notebooks/{id}/key".into(),
                ));
            }
            if acct.root_key.is_some() {
                return Err(err(ErrorCode::Conflict, "a key is already stored".into()));
            }
            acct.root_key = Some(doc.key.clone());
            Ok(())
        })
    }

    /// `GET /v1/vaults`.
    pub fn vault_list(&self, account: &AccountId) -> Result<VaultListResponse, RequestError> {
        self.request(account.into(), None, false, |acct, err| {
            if acct.vault_of.is_some() || acct.space.is_some() {
                return Err(err(ErrorCode::Forbidden, "not from inside a vault".into()));
            }
            Ok(VaultListResponse {
                vaults: acct.vaults.clone(),
            })
        })
    }

    /// `POST /v1/vaults`: creates a vault and its hidden account.
    pub fn vault_create(
        &self,
        account: &AccountId,
        req: &CreateVaultRequest,
    ) -> Result<VaultRecord, RequestError> {
        let api = |code, m: &str| RequestError::Api(ErrorBody::new(code, m, "test"));
        let mut s = self.lock();
        let owner = s
            .accounts
            .get(account)
            .ok_or_else(|| api(ErrorCode::NotFound, "unknown account"))?;
        if owner.vault_of.is_some() || owner.space.is_some() {
            return Err(api(ErrorCode::Forbidden, "not from inside a vault"));
        }
        let quota_bytes = owner.quota_bytes;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let vault_id: AccountId = format!("v-{id}")
            .parse()
            .expect("hex digits with a prefix are a valid id");
        let record = VaultRecord {
            vault_id: vault_id.clone(),
            lock: req.lock.clone(),
            name: req.name.clone(),
            created_at_unix_s: 0,
        };
        s.accounts.insert(
            vault_id,
            Account {
                vault_of: Some(account.clone()),
                quota_bytes,
                ..Account::default()
            },
        );
        s.accounts
            .get_mut(account)
            .expect("checked above")
            .vaults
            .push(record.clone());
        self.save_admin(&s);
        Ok(record)
    }

    /// `PUT /v1/vaults/{id}`: replaces a vault's lock.
    pub fn vault_set_lock(
        &self,
        account: &AccountId,
        vault: &AccountId,
        req: &UpdateVaultLockRequest,
    ) -> Result<(), RequestError> {
        self.request(account.into(), None, true, |acct, err| {
            let v = acct
                .vaults
                .iter_mut()
                .find(|v| &v.vault_id == vault)
                .ok_or_else(|| err(ErrorCode::NotFound, "unknown vault".into()))?;
            v.lock = req.lock.clone();
            Ok(())
        })
    }

    /// `DELETE /v1/vaults/{id}`: erases a vault's cloud copy.
    pub fn vault_delete(&self, account: &AccountId, vault: &AccountId) -> Result<(), RequestError> {
        let mut s = self.lock();
        let owner = s.accounts.get_mut(account).ok_or_else(|| {
            RequestError::Api(ErrorBody::new(
                ErrorCode::NotFound,
                "unknown account",
                "test",
            ))
        })?;
        let before = owner.vaults.len();
        owner.vaults.retain(|v| &v.vault_id != vault);
        if owner.vaults.len() == before {
            return Err(RequestError::Api(ErrorBody::new(
                ErrorCode::NotFound,
                "unknown vault",
                "test",
            )));
        }
        s.accounts.remove(vault);
        self.save_admin(&s);
        Ok(())
    }

    /// Checks that `vault` belongs to `account`, for `POST /v1/vaults/{id}/session`.
    pub fn vault_owned_by(&self, account: &AccountId, vault: &AccountId) -> bool {
        self.lock()
            .accounts
            .get(account)
            .is_some_and(|a| a.vaults.iter().any(|v| &v.vault_id == vault))
    }

    /// `PUT /v1/blobs/{id}`: store an immutable encrypted blob.
    pub fn blob_put<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        id: &BlobId,
        sha256_hex_value: &str,
        bytes: &[u8],
    ) -> Result<BlobPutResponse, RequestError> {
        let caller = caller.into();
        let account = caller.account;
        let limits = self.limits;
        let dir = self.dir.clone();
        self.request(caller, Some(device), true, |acct, err| {
            if !acct.may_write() {
                return Err(err(
                    ErrorCode::Forbidden,
                    "viewers cannot change a shared notebook".into(),
                ));
            }
            if bytes.len() as u64 > limits.max_blob_bytes {
                return Err(err(ErrorCode::PayloadTooLarge, "blob too large".into()));
            }
            if bytes.is_empty() || !hex::is_sha256_hex(sha256_hex_value) {
                return Err(err(
                    ErrorCode::ValidationFailed,
                    "invalid blob digest".into(),
                ));
            }
            let actual = sha256_hex(bytes);
            if actual != sha256_hex_value {
                return Err(err(
                    ErrorCode::IntegrityMismatch,
                    "blob bytes do not match the declared SHA-256".into(),
                ));
            }
            if let Some(m) = acct.blobs.get(id) {
                return if m.sha256 == actual {
                    Ok(BlobPutResponse {
                        blob_id: id.clone(),
                        duplicate: true,
                    })
                } else {
                    Err(err(
                        ErrorCode::Conflict,
                        "blob id already stored with different bytes".into(),
                    ))
                };
            }
            let len = bytes.len() as u64;
            if acct.over_quota_with(len) {
                return Err(err(
                    ErrorCode::QuotaExceeded,
                    "account quota exceeded".into(),
                ));
            }
            if let Some(dir) = &dir {
                Self::write_atomic(&Self::blob_path(dir, account, id), bytes).map_err(|e| {
                    err(
                        ErrorCode::InternalError,
                        format!("could not store blob: {e}"),
                    )
                })?;
            }
            acct.blobs.insert(
                id.clone(),
                BlobMeta {
                    sha256: actual,
                    len,
                },
            );
            acct.blob_data.insert(id.clone(), bytes.to_vec());
            acct.blob_bytes += len;
            Ok(BlobPutResponse {
                blob_id: id.clone(),
                duplicate: false,
            })
        })
    }

    /// `GET /v1/blobs/{id}`.
    pub fn blob_get<'c>(
        &self,
        caller: impl Into<Caller<'c>>,
        device: &DeviceId,
        id: &BlobId,
    ) -> Result<Vec<u8>, RequestError> {
        let caller = caller.into();
        self.request(caller, Some(device), false, |acct, err| {
            acct.blob_data
                .get(id)
                .cloned()
                .ok_or_else(|| err(ErrorCode::NotFound, "unknown blob".into()))
        })
    }
}
