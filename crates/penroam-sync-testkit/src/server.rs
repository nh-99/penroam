//! In-memory fake sync server.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use penroam_sync_protocol::dto::capabilities;
use penroam_sync_protocol::framing::{decode_json_batch, encode_json_batch};
use penroam_sync_protocol::version::PROTOCOL_MAJOR;
use penroam_sync_protocol::{
    verify_ciphertext, AccountId, AckRequest, AckResult, Cursor, DeviceId, EnvelopeHeader,
    ErrorBody, ErrorCode, Limits, OpAck, OpId, ProtocolRange, PullOpsRequest, PullOpsResponse,
    PulledOp, PushOpsResponse, ServerMeta, UsageResponse,
};

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

struct StoredOp {
    header: EnvelopeHeader,
    ciphertext: Vec<u8>,
}

#[derive(Default)]
struct DeviceState {
    revoked: bool,
    applied_position: Option<u64>,
}

#[derive(Default)]
struct Account {
    devices: BTreeMap<DeviceId, DeviceState>,
    /// `log[i]` has `server_seq = i + 1`.
    log: Vec<StoredOp>,
    by_op: HashMap<OpId, u64>,
    by_device_seq: HashMap<(DeviceId, u64), OpId>,
    /// Cursors with a position below this are expired (simulated compaction).
    retention_floor: u64,
    quota_bytes: Option<u64>,
    op_bytes: u64,
}

#[derive(Default)]
struct Faults {
    fail_next: VecDeque<ErrorCode>,
    drop_next_responses: u32,
}

struct State {
    accounts: BTreeMap<AccountId, Account>,
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
pub struct InMemoryServer {
    state: Mutex<State>,
    secret: [u8; 32],
    instance_id: String,
    limits: Limits,
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
        Self::with_secret(secret, format!("testkit-{instance}"), Limits::DEFAULT)
    }

    /// Deterministic server (fixtures, reproducible tests).
    pub fn with_secret(secret: [u8; 32], instance_id: impl Into<String>, limits: Limits) -> Self {
        Self {
            state: Mutex::new(State {
                accounts: BTreeMap::new(),
                faults: Faults::default(),
                request_counter: 0,
            }),
            secret,
            instance_id: instance_id.into(),
            limits: limits.clamped(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---- administration (not protocol endpoints) ----

    /// Create an empty account (idempotent).
    pub fn create_account(&self, account: &AccountId) {
        self.lock().accounts.entry(account.clone()).or_default();
    }

    /// Register a device under an account (idempotent; un-revokes).
    pub fn register_device(&self, account: &AccountId, device: &DeviceId) {
        let mut s = self.lock();
        let a = s.accounts.entry(account.clone()).or_default();
        a.devices.entry(device.clone()).or_default().revoked = false;
    }

    /// Revoke a device.
    pub fn revoke_device(&self, account: &AccountId, device: &DeviceId) {
        if let Some(d) = self
            .lock()
            .accounts
            .get_mut(account)
            .and_then(|a| a.devices.get_mut(device))
        {
            d.revoked = true;
        }
    }

    /// Set an account byte quota over stored op ciphertext.
    pub fn set_quota(&self, account: &AccountId, quota_bytes: Option<u64>) {
        if let Some(a) = self.lock().accounts.get_mut(account) {
            a.quota_bytes = quota_bytes;
        }
    }

    /// Simulate compaction: cursors positioned before `server_seq` now fail
    /// with `cursor_expired`, and a pull without a cursor fails with
    /// `checkpoint_required`.
    pub fn expire_cursors_before(&self, account: &AccountId, server_seq: u64) {
        if let Some(a) = self.lock().accounts.get_mut(account) {
            a.retention_floor = a.retention_floor.max(server_seq);
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

    // ---- protocol endpoints ----

    /// `GET /v1/meta`.
    pub fn meta(&self) -> ServerMeta {
        ServerMeta {
            min_client_protocol: ProtocolRange::CLIENT.min,
            max_client_protocol: ProtocolRange::CLIENT.max,
            capabilities: vec![
                capabilities::OPS_BATCH_V1.into(),
                capabilities::OPS_PULL_V1.into(),
                capabilities::ACKS_V1.into(),
                capabilities::USAGE_V1.into(),
            ],
            limits: self.limits,
            server_instance_id: self.instance_id.clone(),
        }
    }

    /// Run `f` as one request: request id, injected failures, auth check,
    /// response dropping.
    fn request<T>(
        &self,
        account: &AccountId,
        device: Option<&DeviceId>,
        f: impl FnOnce(
            &mut Account,
            &mut dyn FnMut(ErrorCode, String) -> ErrorBody,
        ) -> Result<T, ErrorBody>,
    ) -> Result<T, RequestError> {
        let mut guard = self.lock();
        let s = &mut *guard;
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
        let Some(acct) = s.accounts.get_mut(account) else {
            return Err(RequestError::Api(err(
                ErrorCode::Unauthenticated,
                "unknown account".into(),
            )));
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
        if s.faults.drop_next_responses > 0 {
            s.faults.drop_next_responses -= 1;
            return Err(RequestError::ResponseLost);
        }
        result
    }

    /// `POST /v1/ops/batch` with a framed body of [`EnvelopeHeader`]s.
    pub fn push(
        &self,
        account: &AccountId,
        device: &DeviceId,
        body: &[u8],
    ) -> Result<PushOpsResponse, RequestError> {
        let limits = self.limits;
        self.request(account, Some(device), |acct, err| {
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
        })
    }

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
        if let Some(q) = acct.quota_bytes {
            if acct.op_bytes + len > q {
                return Err((ErrorCode::QuotaExceeded, "account quota exceeded".into()));
            }
        }
        // Single lock held: sequence assignment and insert are atomic.
        let seq = acct.log.len() as u64 + 1;
        acct.by_op.insert(header.op_id.clone(), seq);
        acct.by_device_seq.insert(key, header.op_id.clone());
        acct.op_bytes += len;
        acct.log.push(StoredOp { header, ciphertext });
        Ok(AckResult::Accepted { server_seq: seq })
    }

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
    pub fn pull(
        &self,
        account: &AccountId,
        device: &DeviceId,
        req: &PullOpsRequest,
    ) -> Result<PullPage, RequestError> {
        let limits = self.limits;
        self.request(account, Some(device), |acct, err| {
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
    pub fn ack(
        &self,
        account: &AccountId,
        device: &DeviceId,
        req: &AckRequest,
    ) -> Result<(), RequestError> {
        self.request(account, Some(device), |acct, err| {
            let pos = self
                .open_cursor(account, acct, &req.applied_cursor)
                .map_err(|(code, m)| err(code, m.into()))?;
            let d = acct.devices.get_mut(device).expect("checked by request()");
            d.applied_position = Some(d.applied_position.map_or(pos, |p| p.max(pos)));
            Ok(())
        })
    }

    /// `GET /v1/usage`.
    pub fn usage(&self, account: &AccountId) -> Result<UsageResponse, RequestError> {
        self.request(account, None, |acct, _| {
            Ok(UsageResponse {
                account_id: account.clone(),
                op_count: acct.log.len() as u64,
                op_bytes: acct.op_bytes,
                blob_count: 0,
                blob_bytes: 0,
                quota_bytes: acct.quota_bytes,
            })
        })
    }
}
