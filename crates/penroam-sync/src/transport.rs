//! Sync transports: the protocol calls the engine needs, with typed errors.
//!
//! * `HttpTransport` (feature `http`): the HTTP mapping of protocol v1.
//! * `InProcessTransport` (feature `testkit`): calls the public test
//!   server directly, with the same fault semantics (tests and tools).
//!
//! Transports never log tokens or payloads.
//!
//! Errors are classified in [`TransportError`] so the engine can decide
//! between "retry later with backoff" (network, lost response, 429/5xx) and
//! "needs the user or an update" (auth, revocation, quota, protocol errors).
//! A lost response is *not* a failure of the operation: every mutating call is
//! idempotent, so the engine simply retries it.

#[cfg(any(feature = "http", feature = "testkit"))]
use penroam_sync_protocol::framing::decode_json_batch;
use penroam_sync_protocol::framing::FrameItem;
use penroam_sync_protocol::{
    AccountId, AccountKeyDocument, AckRequest, BlobId, BlobPutResponse, CheckpointRecord,
    CreateVaultRequest, DeviceId, DeviceListResponse, DeviceRegistrationRequest,
    DeviceRegistrationResponse, ErrorBody, ErrorCode, KeyLogAppendRequest, KeyLogAppendResponse,
    KeyLogPage, Limits, PullOpsRequest, PullOpsResponse, PulledOp, PushOpsResponse, ServerMeta,
    SessionTokens, UpdateVaultLockRequest, VaultListResponse, VaultRecord,
};
use penroam_sync_protocol::{
    ChangeRoleRequest, CreateInvitationRequest, CreateSpaceRequest, IncomingInvitationList,
    InvitationId, InvitationRecord, MemberList, MemberRecord, SharedNotebookList,
    SharedNotebookRecord, SpaceId, UpdateSpaceMetaRequest,
};

/// A transport failure, classified for the retry policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The server answered with an error body.
    #[error("{}: {}", .0.code, .0.message)]
    Api(ErrorBody),
    /// No answer (offline, DNS, refused, timeout). Retry with backoff.
    #[error("network unavailable: {0}")]
    Network(String),
    /// The request may have been processed but the response never arrived.
    /// Retrying is safe (every mutation is idempotent).
    #[error("the response was lost")]
    ResponseLost,
    /// The server answered something that does not follow the protocol.
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl TransportError {
    /// The server error code, if the server answered with an error body.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Api(b) => Some(b.code),
            _ => None,
        }
    }

    /// Whether an automatic retry (with backoff) may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Api(b) => b.code.is_retryable(),
            Self::Network(_) | Self::ResponseLost => true,
            Self::Protocol(_) => false,
        }
    }

    /// A synthetic error body for the retry policy.
    pub fn as_error_body(&self) -> ErrorBody {
        match self {
            Self::Api(b) => b.clone(),
            other => ErrorBody::new(ErrorCode::ServiceUnavailable, other.to_string(), "local"),
        }
    }
}

/// Result of a transport call.
pub type TResult<T> = Result<T, TransportError>;

/// One pulled page with ciphertexts.
#[derive(Debug, Clone)]
pub struct PulledPage {
    /// The control document (items, next cursor, has-more).
    pub response: PullOpsResponse,
    /// One ciphertext per item, same order as `response.items`.
    pub ciphertexts: Vec<Vec<u8>>,
}

/// The protocol calls used by the engine.
pub trait SyncTransport: Send {
    /// `GET /v1/meta`: versions, capabilities, limits (unauthenticated).
    fn meta(&mut self) -> TResult<ServerMeta>;
    /// `POST /v1/devices`: registers this device's public keys. Registration
    /// grants no trust; membership comes from the key log.
    fn register_device(
        &mut self,
        req: &DeviceRegistrationRequest,
    ) -> TResult<DeviceRegistrationResponse>;
    /// `GET /v1/devices`: what the server has registered. Untrusted: membership comes from the
    /// key log.
    fn list_devices(&mut self) -> TResult<DeviceListResponse>;
    /// `DELETE /v1/devices/{id}`: the server stops accepting that device's writes.
    fn revoke_device(&mut self, id: &DeviceId) -> TResult<()>;
    /// `POST /v1/ops/batch`. `frame` is a PRSB batch of envelope headers +
    /// ciphertexts; the response has one ack per envelope, in order.
    fn push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse>;
    /// `GET /v1/ops`: one page of envelopes after `req.cursor`, checked against
    /// `limits` while decoding.
    fn pull(&mut self, req: &PullOpsRequest, limits: &Limits) -> TResult<PulledPage>;
    /// `POST /v1/acks`: tells the server how far this device has applied
    /// (informs retention only).
    fn ack(&mut self, req: &AckRequest) -> TResult<()>;
    /// `GET /v1/keylog?from=`: key-log entries from index `from`.
    fn keylog_get(&mut self, from: u64) -> TResult<KeyLogPage>;
    /// `POST /v1/keylog`: appends at `req.index`; `conflict` if someone else
    /// appended first.
    fn keylog_append(&mut self, req: &KeyLogAppendRequest) -> TResult<KeyLogAppendResponse>;
    /// `PUT /v1/blobs/{id}`: uploads encrypted asset bytes (idempotent).
    fn blob_put(&mut self, id: &BlobId, sha256_hex: &str, bytes: &[u8])
        -> TResult<BlobPutResponse>;
    /// `GET /v1/blobs/{id}`: downloads encrypted asset bytes.
    fn blob_get(&mut self, id: &BlobId) -> TResult<Vec<u8>>;
    /// `POST /v1/checkpoints`: publish a signed checkpoint record (its blob
    /// must already be uploaded).
    fn checkpoint_put(&mut self, rec: &CheckpointRecord) -> TResult<()>;
    /// `GET /v1/checkpoints/latest`; `None` when the account has none yet.
    fn checkpoint_latest(&mut self) -> TResult<Option<CheckpointRecord>>;
    /// `GET /v1/checkpoints/previous` (`history.v1`): the checkpoint before the latest, for
    /// recovering when the latest is damaged; `None` when there is none or the server doesn't
    /// keep it.
    fn checkpoint_previous(&mut self) -> TResult<Option<CheckpointRecord>> {
        Ok(None)
    }
    /// `GET /v1/account/key`: the stored root secret; `None` when nothing is stored. Vault
    /// accounts always fail with `forbidden`.
    fn account_key_get(&mut self) -> TResult<Option<AccountKeyDocument>>;
    /// `PUT /v1/account/key`: stores the root secret once; `conflict` if one is already stored.
    fn account_key_put(&mut self, doc: &AccountKeyDocument) -> TResult<()>;
    /// `GET /v1/vaults`: the account's vaults. Their contents are not trusted until a lock opens.
    fn vault_list(&mut self) -> TResult<VaultListResponse>;
    /// `POST /v1/vaults`: creates a vault and its hidden account.
    fn vault_create(&mut self, req: &CreateVaultRequest) -> TResult<VaultRecord>;
    /// `PUT /v1/vaults/{id}`: replaces a vault's passphrase lock.
    fn vault_set_lock(&mut self, id: &AccountId, req: &UpdateVaultLockRequest) -> TResult<()>;
    /// `DELETE /v1/vaults/{id}`: erases a vault's cloud copy.
    fn vault_delete(&mut self, id: &AccountId) -> TResult<()>;
    /// `POST /v1/vaults/{id}/session`: an account session for the vault's own account.
    fn vault_session(&mut self, id: &AccountId) -> TResult<SessionTokens>;

    // Shared notebooks (`sharing.v1`). All need a session of the caller's own account. The
    // defaults refuse, for transports that don't implement them.

    /// `GET /v1/shared-notebooks`: spaces the caller owns or belongs to (revoked ones once).
    fn shared_list(&mut self) -> TResult<SharedNotebookList> {
        Err(no_sharing())
    }
    /// `POST /v1/shared-notebooks`: creates a space (idempotent by `idempotency_key`).
    fn shared_create(&mut self, req: &CreateSpaceRequest) -> TResult<SharedNotebookRecord> {
        let _ = req;
        Err(no_sharing())
    }
    /// `PUT /v1/shared-notebooks/{id}/meta`: replaces the sealed title.
    fn shared_set_meta(&mut self, id: &SpaceId, req: &UpdateSpaceMetaRequest) -> TResult<()> {
        let _ = (id, req);
        Err(no_sharing())
    }
    /// `POST /v1/shared-notebooks/{id}/session`: an account session on the space.
    fn shared_session(&mut self, id: &SpaceId) -> TResult<SessionTokens> {
        let _ = id;
        Err(no_sharing())
    }
    /// `GET /v1/shared-notebooks/{id}/key`: the space's root; `None` when it isn't stored yet.
    fn shared_key_get(&mut self, id: &SpaceId) -> TResult<Option<AccountKeyDocument>> {
        let _ = id;
        Err(no_sharing())
    }
    /// `PUT /v1/shared-notebooks/{id}/key`: stores the space's root once (owner); `conflict` if
    /// one is stored.
    fn shared_key_put(&mut self, id: &SpaceId, doc: &AccountKeyDocument) -> TResult<()> {
        let _ = (id, doc);
        Err(no_sharing())
    }
    /// `GET /v1/shared-notebooks/{id}/members`: members, and for the owner, invitations.
    fn shared_members(&mut self, id: &SpaceId) -> TResult<MemberList> {
        let _ = id;
        Err(no_sharing())
    }
    /// `POST /v1/shared-notebooks/{id}/invitations` (owner).
    fn shared_invite(
        &mut self,
        id: &SpaceId,
        req: &CreateInvitationRequest,
    ) -> TResult<InvitationRecord> {
        let _ = (id, req);
        Err(no_sharing())
    }
    /// `PATCH /v1/shared-notebooks/{id}/members/{account}` (owner).
    fn shared_change_role(
        &mut self,
        id: &SpaceId,
        account: &AccountId,
        req: &ChangeRoleRequest,
    ) -> TResult<MemberRecord> {
        let _ = (id, account, req);
        Err(no_sharing())
    }
    /// `DELETE /v1/shared-notebooks/{id}/members/{account}`: the owner removes a member, or a
    /// member removes themselves (leaving).
    fn shared_remove_member(&mut self, id: &SpaceId, account: &AccountId) -> TResult<()> {
        let _ = (id, account);
        Err(no_sharing())
    }
    /// `GET /v1/invitations`: invitations waiting for the caller's verified email.
    fn invitations_list(&mut self) -> TResult<IncomingInvitationList> {
        Err(no_sharing())
    }
    /// `POST /v1/invitations/{inv}/accept`.
    fn invitation_accept(&mut self, inv: &InvitationId) -> TResult<SharedNotebookRecord> {
        let _ = inv;
        Err(no_sharing())
    }
    /// `POST /v1/invitations/{inv}/decline`.
    fn invitation_decline(&mut self, inv: &InvitationId) -> TResult<()> {
        let _ = inv;
        Err(no_sharing())
    }
    /// `DELETE /v1/invitations/{inv}`: the owner withdraws an invitation.
    fn invitation_cancel(&mut self, inv: &InvitationId) -> TResult<()> {
        let _ = inv;
        Err(no_sharing())
    }
}

/// What the default sharing methods answer.
fn no_sharing() -> TransportError {
    TransportError::Protocol("this transport doesn't support shared notebooks".into())
}

/// Pairs a pull control document with its frame and rejects a frame whose
/// item headers differ from the document (a server bug or tampering).
#[cfg(any(feature = "http", feature = "testkit"))]
fn pulled_from_frame(
    response: PullOpsResponse,
    items: Vec<(PulledOp, Vec<u8>)>,
) -> TResult<PulledPage> {
    if items.len() != response.items.len() {
        return Err(TransportError::Protocol(
            "pull frame does not match its control document".into(),
        ));
    }
    let mut cts = Vec::with_capacity(items.len());
    for ((h, ct), expected) in items.into_iter().zip(&response.items) {
        if &h != expected {
            return Err(TransportError::Protocol(
                "pull frame item differs from the control document".into(),
            ));
        }
        cts.push(ct);
    }
    Ok(PulledPage {
        response,
        ciphertexts: cts,
    })
}

// ------------------------------------------------------------ in-process --

#[cfg(feature = "testkit")]
pub use in_process::InProcessTransport;

#[cfg(feature = "testkit")]
mod in_process {
    use super::*;
    use penroam_sync_protocol::AccountId;
    use penroam_sync_testkit::{Caller, InMemoryServer, RequestError};
    use std::sync::Arc;

    /// Calls an [`InMemoryServer`] directly as `(account, device)`.
    pub struct InProcessTransport {
        /// The shared test server.
        pub server: Arc<InMemoryServer>,
        /// Account this client acts as.
        pub account: AccountId,
        /// Device this client acts as (updated by `register_device`).
        pub device: DeviceId,
        /// When set, every call fails with `Network` (airplane mode).
        pub offline: bool,
        /// Test knob: the next this-many blob downloads fail (network error).
        pub fail_blob_gets: u32,
        /// Test knob: blob downloads return altered bytes (a server serving corrupt data).
        pub corrupt_blob_gets: bool,
        /// Test knob: the latest checkpoint is returned with an altered signature.
        pub corrupt_checkpoints: bool,
        /// Test knob: `GET /v1/invitations` fails (server error).
        pub fail_invitation_lists: bool,
        /// When `account` is a shared notebook space: the member this client acts as (an account
        /// session on the space). Sharing routes then act as this member's own account.
        pub member: Option<AccountId>,
    }

    impl InProcessTransport {
        /// A client of `server` acting as `(account, device)`, online.
        pub fn new(server: Arc<InMemoryServer>, account: AccountId, device: DeviceId) -> Self {
            Self {
                server,
                account,
                device,
                offline: false,
                fail_blob_gets: 0,
                corrupt_blob_gets: false,
                corrupt_checkpoints: false,
                fail_invitation_lists: false,
                member: None,
            }
        }

        /// A client of shared notebook space `space`, acting as `member` (what a space session
        /// gives).
        pub fn for_space(
            server: Arc<InMemoryServer>,
            space: AccountId,
            member: AccountId,
            device: DeviceId,
        ) -> Self {
            let mut t = Self::new(server, space, device);
            t.member = Some(member);
            t
        }

        /// Who a sync request is made by.
        fn caller(&self) -> Caller<'_> {
            Caller {
                account: &self.account,
                member: self.member.as_ref(),
            }
        }

        /// The account sharing requests act as: the member's own account inside a space.
        fn own(&self) -> &AccountId {
            self.member.as_ref().unwrap_or(&self.account)
        }

        fn up(&self) -> TResult<()> {
            if self.offline {
                Err(TransportError::Network("offline".into()))
            } else {
                Ok(())
            }
        }
    }

    fn map(e: RequestError) -> TransportError {
        match e {
            RequestError::Api(b) => TransportError::Api(b),
            RequestError::ResponseLost => TransportError::ResponseLost,
        }
    }

    impl SyncTransport for InProcessTransport {
        fn meta(&mut self) -> TResult<ServerMeta> {
            self.up()?;
            Ok(self.server.meta())
        }
        fn register_device(
            &mut self,
            req: &DeviceRegistrationRequest,
        ) -> TResult<DeviceRegistrationResponse> {
            self.up()?;
            let r = self
                .server
                .register_device_keys(self.caller(), req)
                .map_err(map)?;
            self.device = r.device_id.clone();
            Ok(r)
        }
        fn list_devices(&mut self) -> TResult<DeviceListResponse> {
            self.up()?;
            self.server.list_devices(self.caller()).map_err(map)
        }
        fn revoke_device(&mut self, id: &DeviceId) -> TResult<()> {
            self.up()?;
            self.server
                .revoke_device_as(self.caller(), &self.device, id)
                .map_err(map)
        }
        fn push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse> {
            self.up()?;
            self.server
                .push(self.caller(), &self.device, &frame)
                .map_err(map)
        }
        fn pull(&mut self, req: &PullOpsRequest, limits: &Limits) -> TResult<PulledPage> {
            self.up()?;
            let page = self
                .server
                .pull(self.caller(), &self.device, req)
                .map_err(map)?;
            let items: Vec<(PulledOp, Vec<u8>)> = decode_json_batch(&page.frame, limits)
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            pulled_from_frame(page.response, items)
        }
        fn ack(&mut self, req: &AckRequest) -> TResult<()> {
            self.up()?;
            self.server
                .ack(self.caller(), &self.device, req)
                .map(|_| ())
                .map_err(map)
        }
        fn keylog_get(&mut self, from: u64) -> TResult<KeyLogPage> {
            self.up()?;
            self.server.keylog_get(self.caller(), from).map_err(map)
        }
        fn keylog_append(&mut self, req: &KeyLogAppendRequest) -> TResult<KeyLogAppendResponse> {
            self.up()?;
            self.server.keylog_append(self.caller(), req).map_err(map)
        }
        fn blob_put(
            &mut self,
            id: &BlobId,
            sha256_hex: &str,
            bytes: &[u8],
        ) -> TResult<BlobPutResponse> {
            self.up()?;
            self.server
                .blob_put(self.caller(), &self.device, id, sha256_hex, bytes)
                .map_err(map)
        }
        fn blob_get(&mut self, id: &BlobId) -> TResult<Vec<u8>> {
            self.up()?;
            if self.fail_blob_gets > 0 {
                self.fail_blob_gets -= 1;
                return Err(TransportError::Network(
                    "injected blob download failure".into(),
                ));
            }
            let mut b = self
                .server
                .blob_get(self.caller(), &self.device, id)
                .map_err(map)?;
            if self.corrupt_blob_gets {
                if let Some(x) = b.last_mut() {
                    *x ^= 0x55;
                }
            }
            Ok(b)
        }
        fn checkpoint_put(&mut self, rec: &CheckpointRecord) -> TResult<()> {
            self.up()?;
            self.server
                .checkpoint_put(self.caller(), &self.device, rec)
                .map_err(map)
        }
        fn checkpoint_latest(&mut self) -> TResult<Option<CheckpointRecord>> {
            self.up()?;
            match self.server.checkpoint_latest(self.caller(), &self.device) {
                Ok(mut r) => {
                    if self.corrupt_checkpoints {
                        r.signature = penroam_sync_protocol::base64url::encode(&[7u8; 64]);
                    }
                    Ok(Some(r))
                }
                Err(RequestError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(map(e)),
            }
        }
        fn checkpoint_previous(&mut self) -> TResult<Option<CheckpointRecord>> {
            self.up()?;
            match self.server.checkpoint_previous(self.caller(), &self.device) {
                Ok(r) => Ok(Some(r)),
                Err(RequestError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(map(e)),
            }
        }
        fn account_key_get(&mut self) -> TResult<Option<AccountKeyDocument>> {
            self.up()?;
            match self.server.account_key_get(&self.account) {
                Ok(d) => Ok(Some(d)),
                Err(RequestError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(map(e)),
            }
        }
        fn account_key_put(&mut self, doc: &AccountKeyDocument) -> TResult<()> {
            self.up()?;
            self.server.account_key_put(&self.account, doc).map_err(map)
        }
        fn vault_list(&mut self) -> TResult<VaultListResponse> {
            self.up()?;
            self.server.vault_list(&self.account).map_err(map)
        }
        fn vault_create(&mut self, req: &CreateVaultRequest) -> TResult<VaultRecord> {
            self.up()?;
            self.server.vault_create(&self.account, req).map_err(map)
        }
        fn vault_set_lock(&mut self, id: &AccountId, req: &UpdateVaultLockRequest) -> TResult<()> {
            self.up()?;
            self.server
                .vault_set_lock(&self.account, id, req)
                .map_err(map)
        }
        fn vault_delete(&mut self, id: &AccountId) -> TResult<()> {
            self.up()?;
            self.server.vault_delete(&self.account, id).map_err(map)
        }
        fn vault_session(&mut self, id: &AccountId) -> TResult<SessionTokens> {
            self.up()?;
            if !self.server.vault_owned_by(&self.account, id) {
                return Err(TransportError::Protocol("unknown vault".into()));
            }
            // the in-process server has no tokens: a caller switches `account` to the vault
            Ok(SessionTokens {
                account_id: id.clone(),
                device_id: None,
                access_token: String::new(),
                expires_in_s: 3600,
                refresh_token: String::new(),
            })
        }

        fn shared_list(&mut self) -> TResult<SharedNotebookList> {
            self.up()?;
            self.server.shared_list(self.own()).map_err(map)
        }
        fn shared_create(&mut self, req: &CreateSpaceRequest) -> TResult<SharedNotebookRecord> {
            self.up()?;
            self.server.shared_create(self.own(), req).map_err(map)
        }
        fn shared_set_meta(&mut self, id: &SpaceId, req: &UpdateSpaceMetaRequest) -> TResult<()> {
            self.up()?;
            self.server
                .shared_set_meta(self.own(), id, req)
                .map_err(map)
        }
        fn shared_session(&mut self, id: &SpaceId) -> TResult<SessionTokens> {
            self.up()?;
            self.server
                .shared_session_check(self.own(), id)
                .map_err(map)?;
            // the in-process server has no tokens: a caller uses `for_space`
            Ok(SessionTokens {
                account_id: id.clone(),
                device_id: None,
                access_token: String::new(),
                expires_in_s: 3600,
                refresh_token: String::new(),
            })
        }
        fn shared_key_get(&mut self, id: &SpaceId) -> TResult<Option<AccountKeyDocument>> {
            self.up()?;
            match self.server.shared_key_get(self.own(), id) {
                Ok(d) => Ok(Some(d)),
                Err(RequestError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(map(e)),
            }
        }
        fn shared_key_put(&mut self, id: &SpaceId, doc: &AccountKeyDocument) -> TResult<()> {
            self.up()?;
            self.server.shared_key_put(self.own(), id, doc).map_err(map)
        }
        fn shared_members(&mut self, id: &SpaceId) -> TResult<MemberList> {
            self.up()?;
            self.server.shared_members(self.own(), id).map_err(map)
        }
        fn shared_invite(
            &mut self,
            id: &SpaceId,
            req: &CreateInvitationRequest,
        ) -> TResult<InvitationRecord> {
            self.up()?;
            self.server.shared_invite(self.own(), id, req).map_err(map)
        }
        fn shared_change_role(
            &mut self,
            id: &SpaceId,
            account: &AccountId,
            req: &ChangeRoleRequest,
        ) -> TResult<MemberRecord> {
            self.up()?;
            self.server
                .shared_change_role(self.own(), id, account, req)
                .map_err(map)
        }
        fn shared_remove_member(&mut self, id: &SpaceId, account: &AccountId) -> TResult<()> {
            self.up()?;
            self.server
                .shared_remove_member(self.own(), id, account)
                .map_err(map)
        }
        fn invitations_list(&mut self) -> TResult<IncomingInvitationList> {
            self.up()?;
            if self.fail_invitation_lists {
                return Err(TransportError::Api(ErrorBody::new(
                    ErrorCode::InternalError,
                    "injected failure",
                    "",
                )));
            }
            self.server.invitations_incoming(self.own()).map_err(map)
        }
        fn invitation_accept(&mut self, inv: &InvitationId) -> TResult<SharedNotebookRecord> {
            self.up()?;
            self.server.invitation_accept(self.own(), inv).map_err(map)
        }
        fn invitation_decline(&mut self, inv: &InvitationId) -> TResult<()> {
            self.up()?;
            self.server.invitation_decline(self.own(), inv).map_err(map)
        }
        fn invitation_cancel(&mut self, inv: &InvitationId) -> TResult<()> {
            self.up()?;
            self.server.invitation_cancel(self.own(), inv).map_err(map)
        }
    }
}

// ------------------------------------------------------------------ HTTP --

#[cfg(feature = "http")]
pub use http::HttpTransport;

#[cfg(feature = "http")]
mod http {
    use super::*;
    use penroam_sync_protocol::routes;
    use std::time::Duration;

    /// Protocol v1 over HTTP(S). The bearer token is held only in memory.
    pub struct HttpTransport {
        base: String,
        token: String,
        agent: ureq::Agent,
    }

    impl std::fmt::Debug for HttpTransport {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("HttpTransport")
                .field("base", &self.base)
                .finish_non_exhaustive()
        }
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .timeout_connect(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into()
    }

    const JSON_LIMIT: u64 = 1 << 20;

    /// Classifies a ureq failure. A connection that dies after the request was
    /// sent (reset, EOF, garbled response) may have been processed by the
    /// server, so it is a lost response (safe to retry), not "offline".
    fn net(e: ureq::Error) -> TransportError {
        match e {
            ureq::Error::Io(ref io)
                if matches!(
                    io.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                ) =>
            {
                TransportError::ResponseLost
            }
            ureq::Error::Protocol(_) => TransportError::ResponseLost,
            other => TransportError::Network(other.to_string()),
        }
    }

    impl HttpTransport {
        /// A client for `base_url` (a trailing slash is ignored) using a bearer token.
        pub fn new(base_url: &str, token: String) -> Self {
            Self {
                base: base_url.trim_end_matches('/').to_string(),
                token,
                agent: agent(),
            }
        }

        /// Replaces the bearer token (after re-authentication).
        pub fn set_token(&mut self, token: String) {
            self.token = token;
        }

        /// The bearer token in use (for opening the live channel with the same session). Keep it
        /// in memory only.
        pub fn token(&self) -> &str {
            &self.token
        }

        /// The server's base URL, without a trailing slash.
        pub fn base_url(&self) -> &str {
            &self.base
        }

        /// Development test server only: obtain a session token
        /// (`POST /_test/session`). Real servers use their own sign-in.
        pub fn test_session(
            base_url: &str,
            account: &str,
            device_id: Option<&str>,
        ) -> TResult<(String, String)> {
            Self::test_session_as(base_url, account, device_id, None, None)
        }

        /// [`HttpTransport::test_session`] with the development test server's other options:
        /// `member` makes a session on a shared notebook space act as that member (with
        /// `device_id`, a device session there), and `email` records a verified sign-in email
        /// for `account`, which invitations match.
        pub fn test_session_as(
            base_url: &str,
            account: &str,
            device_id: Option<&str>,
            member: Option<&str>,
            email: Option<&str>,
        ) -> TResult<(String, String)> {
            let base = base_url.trim_end_matches('/');
            let mut body = serde_json::json!({ "account": account });
            if let Some(d) = device_id {
                body["device_id"] = serde_json::Value::String(d.to_string());
            }
            if let Some(m) = member {
                body["member"] = serde_json::Value::String(m.to_string());
            }
            if let Some(e) = email {
                body["email"] = serde_json::Value::String(e.to_string());
            }
            let mut r = agent()
                .post(format!("{base}/_test/session"))
                .send_json(&body)
                .map_err(net)?;
            let status = r.status().as_u16();
            let v: serde_json::Value = r
                .body_mut()
                .with_config()
                .limit(JSON_LIMIT)
                .read_json()
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            if status != 200 {
                return Err(match serde_json::from_value::<ErrorBody>(v) {
                    Ok(b) => TransportError::Api(b),
                    Err(_) => TransportError::Protocol(format!("session: HTTP {status}")),
                });
            }
            let token = v["token"].as_str().unwrap_or_default().to_string();
            let acct = v["account_id"].as_str().unwrap_or_default().to_string();
            if token.is_empty() {
                return Err(TransportError::Protocol("session without token".into()));
            }
            Ok((token, acct))
        }

        fn url(&self, path: &str) -> String {
            format!("{}{}", self.base, path)
        }

        fn auth(&self) -> String {
            format!("Bearer {}", self.token)
        }

        fn finish(
            mut r: ureq::http::Response<ureq::Body>,
            limit: u64,
        ) -> TResult<(u16, ureq::http::HeaderMap, Vec<u8>)> {
            let status = r.status().as_u16();
            let headers = r.headers().clone();
            let body = r
                .body_mut()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(net)?;
            if status >= 400 {
                return Err(match serde_json::from_slice::<ErrorBody>(&body) {
                    Ok(b) => TransportError::Api(b),
                    Err(_) if status >= 500 => TransportError::Api(ErrorBody::new(
                        ErrorCode::ServiceUnavailable,
                        format!("HTTP {status}"),
                        "",
                    )),
                    Err(_) => {
                        TransportError::Protocol(format!("HTTP {status} without an error body"))
                    }
                });
            }
            Ok((status, headers, body))
        }

        fn json<T: serde::de::DeserializeOwned>(body: &[u8]) -> TResult<T> {
            serde_json::from_slice(body).map_err(|e| TransportError::Protocol(e.to_string()))
        }

        fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> TResult<T> {
            let r = self
                .agent
                .get(self.url(path))
                .header("Authorization", self.auth())
                .call()
                .map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }

        fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
            &self,
            path: &str,
            body: &B,
        ) -> TResult<Option<T>> {
            let r = self
                .agent
                .post(self.url(path))
                .header("Authorization", self.auth())
                .send_json(body)
                .map_err(net)?;
            let (status, _, b) = Self::finish(r, JSON_LIMIT)?;
            if status == 204 || b.is_empty() {
                return Ok(None);
            }
            Self::json(&b).map(Some)
        }
    }

    /// Account routes (capability `account.v1`). They are not part of [`SyncTransport`]: the
    /// sync engine never needs them, only the account settings of an app do. Every response is
    /// validated before it is returned; an invalid one is a [`TransportError::Protocol`].
    impl HttpTransport {
        /// `GET /v1/account`: plan, storage allowance and available actions.
        pub fn account_state(&self) -> TResult<penroam_sync_protocol::AccountState> {
            let s: penroam_sync_protocol::AccountState = self.get_json(routes::ACCOUNT)?;
            s.validate()
                .map_err(|e| TransportError::Protocol(format!("account state: {e}")))?;
            Ok(s)
        }

        /// `POST /v1/account/billing-link`. The returned link is checked to be `https://`
        /// ([`penroam_sync_protocol::BillingLink::validate`]); open it in the system browser.
        pub fn billing_link(
            &self,
            req: &penroam_sync_protocol::BillingLinkRequest,
        ) -> TResult<penroam_sync_protocol::BillingLink> {
            let l: penroam_sync_protocol::BillingLink = self
                .post_json(routes::ACCOUNT_BILLING_LINK, req)?
                .ok_or_else(|| TransportError::Protocol("empty billing link response".into()))?;
            l.validate()
                .map_err(|e| TransportError::Protocol(format!("billing link: {e}")))?;
            Ok(l)
        }

        /// `POST /v1/account/store-purchase`: tell the server about a store purchase, and get the
        /// resulting account state. Finish the store transaction only after this succeeds or
        /// the error is `conflict`; a retryable error means try again later.
        pub fn store_purchase(
            &self,
            req: &penroam_sync_protocol::StorePurchaseRequest,
        ) -> TResult<penroam_sync_protocol::AccountState> {
            let s: penroam_sync_protocol::AccountState = self
                .post_json(routes::ACCOUNT_STORE_PURCHASE, req)?
                .ok_or_else(|| TransportError::Protocol("empty store purchase response".into()))?;
            s.validate()
                .map_err(|e| TransportError::Protocol(format!("account state: {e}")))?;
            Ok(s)
        }

        /// `POST /v1/account/deletion`. Needs an account session from a recent sign-in;
        /// otherwise the error code is `reauth_required`.
        pub fn request_deletion(&self) -> TResult<penroam_sync_protocol::DeletionState> {
            self.post_json(routes::ACCOUNT_DELETION, &serde_json::json!({}))?
                .ok_or_else(|| TransportError::Protocol("empty deletion response".into()))
        }

        /// `DELETE /v1/account/deletion`: withdraw a pending deletion request.
        pub fn cancel_deletion(&self) -> TResult<()> {
            let r = self
                .agent
                .delete(self.url(routes::ACCOUNT_DELETION))
                .header("Authorization", self.auth())
                .call()
                .map_err(net)?;
            Self::finish(r, JSON_LIMIT).map(|_| ())
        }
    }

    impl SyncTransport for HttpTransport {
        fn meta(&mut self) -> TResult<ServerMeta> {
            let r = self.agent.get(self.url(routes::META)).call().map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }
        fn register_device(
            &mut self,
            req: &DeviceRegistrationRequest,
        ) -> TResult<DeviceRegistrationResponse> {
            self.post_json(routes::DEVICES, req)?
                .ok_or_else(|| TransportError::Protocol("empty registration response".into()))
        }
        fn list_devices(&mut self) -> TResult<DeviceListResponse> {
            self.get_json(routes::DEVICES)
        }
        fn revoke_device(&mut self, id: &DeviceId) -> TResult<()> {
            let r = self
                .agent
                .delete(self.url(&routes::DEVICE.replace("{id}", id.as_str())))
                .header("Authorization", self.auth())
                .call()
                .map_err(net)?;
            Self::finish(r, JSON_LIMIT).map(|_| ())
        }
        fn push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse> {
            let r = self
                .agent
                .post(self.url(routes::OPS_BATCH))
                .header("Authorization", self.auth())
                .header("Content-Type", routes::MEDIA_BATCH)
                .send(&frame[..])
                .map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }
        fn pull(&mut self, req: &PullOpsRequest, limits: &Limits) -> TResult<PulledPage> {
            let r = self
                .agent
                .get(format!("{}?{}", self.url(routes::OPS), req.to_query()))
                .header("Authorization", self.auth())
                .header("Accept", routes::MEDIA_BATCH)
                .call()
                .map_err(net)?;
            let (_, headers, body) = Self::finish(r, limits.max_batch_bytes + 1024)?;
            let h = |n: &str| {
                headers
                    .get(n)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            };
            let next = h(routes::HEADER_NEXT_CURSOR)
                .ok_or_else(|| TransportError::Protocol("pull without next cursor".into()))?;
            let next_cursor = penroam_sync_protocol::Cursor::new(next)
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            let has_more = h(routes::HEADER_HAS_MORE).as_deref() == Some("true");
            let items: Vec<(PulledOp, Vec<u8>)> = decode_json_batch(&body, limits)
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            let response = PullOpsResponse {
                items: items.iter().map(|(p, _)| p.clone()).collect(),
                next_cursor,
                has_more,
            };
            pulled_from_frame(response, items)
        }
        fn ack(&mut self, req: &AckRequest) -> TResult<()> {
            self.post_json::<_, serde_json::Value>(routes::ACKS, req)
                .map(|_| ())
        }
        fn keylog_get(&mut self, from: u64) -> TResult<KeyLogPage> {
            self.get_json(&format!("{}?from={from}", routes::KEYLOG))
        }
        fn keylog_append(&mut self, req: &KeyLogAppendRequest) -> TResult<KeyLogAppendResponse> {
            self.post_json(routes::KEYLOG, req)?
                .ok_or_else(|| TransportError::Protocol("empty keylog response".into()))
        }
        fn blob_put(
            &mut self,
            id: &BlobId,
            sha256_hex: &str,
            bytes: &[u8],
        ) -> TResult<BlobPutResponse> {
            let r = self
                .agent
                .put(self.url(&routes::BLOB.replace("{id}", id.as_str())))
                .header("Authorization", self.auth())
                .header("Content-Type", routes::MEDIA_OCTETS)
                .header(routes::HEADER_CONTENT_SHA256, sha256_hex)
                .send(bytes)
                .map_err(net)?;
            Self::json(&Self::finish(r, JSON_LIMIT)?.2)
        }
        fn blob_get(&mut self, id: &BlobId) -> TResult<Vec<u8>> {
            let r = self
                .agent
                .get(self.url(&routes::BLOB.replace("{id}", id.as_str())))
                .header("Authorization", self.auth())
                .call()
                .map_err(net)?;
            Ok(Self::finish(r, 1 << 30)?.2)
        }
        fn checkpoint_put(&mut self, rec: &CheckpointRecord) -> TResult<()> {
            self.post_json::<_, serde_json::Value>(routes::CHECKPOINTS, rec)
                .map(|_| ())
        }
        fn checkpoint_latest(&mut self) -> TResult<Option<CheckpointRecord>> {
            match self.get_json(routes::CHECKPOINT_LATEST) {
                Ok(r) => Ok(Some(r)),
                Err(TransportError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
        fn checkpoint_previous(&mut self) -> TResult<Option<CheckpointRecord>> {
            match self.get_json(routes::CHECKPOINT_PREVIOUS) {
                Ok(r) => Ok(Some(r)),
                // a server without `history.v1` has no such route
                Err(TransportError::Api(b))
                    if matches!(b.code, ErrorCode::NotFound | ErrorCode::Forbidden) =>
                {
                    Ok(None)
                }
                Err(e) => Err(e),
            }
        }
        fn account_key_get(&mut self) -> TResult<Option<AccountKeyDocument>> {
            match self.get_json(routes::ACCOUNT_KEY) {
                Ok(d) => Ok(Some(d)),
                Err(TransportError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
        fn account_key_put(&mut self, doc: &AccountKeyDocument) -> TResult<()> {
            let r = self
                .agent
                .put(self.url(routes::ACCOUNT_KEY))
                .header("Authorization", self.auth())
                .send_json(doc)
                .map_err(net)?;
            Self::finish(r, JSON_LIMIT).map(|_| ())
        }
        fn vault_list(&mut self) -> TResult<VaultListResponse> {
            self.get_json(routes::VAULTS)
        }
        fn vault_create(&mut self, req: &CreateVaultRequest) -> TResult<VaultRecord> {
            self.post_json(routes::VAULTS, req)?
                .ok_or_else(|| TransportError::Protocol("empty vault response".into()))
        }
        fn vault_set_lock(&mut self, id: &AccountId, req: &UpdateVaultLockRequest) -> TResult<()> {
            let r = self
                .agent
                .put(self.url(&vault_path(id)))
                .header("Authorization", self.auth())
                .send_json(req)
                .map_err(net)?;
            Self::finish(r, JSON_LIMIT).map(|_| ())
        }
        fn vault_delete(&mut self, id: &AccountId) -> TResult<()> {
            let r = self
                .agent
                .delete(self.url(&vault_path(id)))
                .header("Authorization", self.auth())
                .call()
                .map_err(net)?;
            Self::finish(r, JSON_LIMIT).map(|_| ())
        }
        fn vault_session(&mut self, id: &AccountId) -> TResult<SessionTokens> {
            self.post_json(
                &format!("{}/session", vault_path(id)),
                &serde_json::json!({}),
            )?
            .ok_or_else(|| TransportError::Protocol("empty session response".into()))
        }

        fn shared_list(&mut self) -> TResult<SharedNotebookList> {
            self.sharing_impl_list()
        }
        fn shared_create(&mut self, req: &CreateSpaceRequest) -> TResult<SharedNotebookRecord> {
            let r = self
                .post_json(routes::SHARED_NOTEBOOKS, req)?
                .ok_or_else(|| TransportError::Protocol("empty shared notebook".into()))?;
            checked_record(r)
        }
        fn shared_set_meta(&mut self, id: &SpaceId, req: &UpdateSpaceMetaRequest) -> TResult<()> {
            self.send_json(
                "PUT",
                &space_path(routes::SHARED_NOTEBOOK_META, id),
                Some(req),
            )
            .map(|_| ())
        }
        fn shared_session(&mut self, id: &SpaceId) -> TResult<SessionTokens> {
            self.post_json(
                &space_path(routes::SHARED_NOTEBOOK_SESSION, id),
                &serde_json::json!({}),
            )?
            .ok_or_else(|| TransportError::Protocol("empty session response".into()))
        }
        fn shared_key_get(&mut self, id: &SpaceId) -> TResult<Option<AccountKeyDocument>> {
            match self.get_json(&space_path(routes::SHARED_NOTEBOOK_KEY, id)) {
                Ok(d) => Ok(Some(d)),
                Err(TransportError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }
        fn shared_key_put(&mut self, id: &SpaceId, doc: &AccountKeyDocument) -> TResult<()> {
            self.send_json(
                "PUT",
                &space_path(routes::SHARED_NOTEBOOK_KEY, id),
                Some(doc),
            )
            .map(|_| ())
        }
        fn shared_members(&mut self, id: &SpaceId) -> TResult<MemberList> {
            let l: MemberList = self.get_json(&space_path(routes::SHARED_NOTEBOOK_MEMBERS, id))?;
            for m in &l.members {
                m.validate()
                    .map_err(|e| TransportError::Protocol(format!("member: {e}")))?;
            }
            Ok(l)
        }
        fn shared_invite(
            &mut self,
            id: &SpaceId,
            req: &CreateInvitationRequest,
        ) -> TResult<InvitationRecord> {
            self.post_json(&space_path(routes::SHARED_NOTEBOOK_INVITATIONS, id), req)?
                .ok_or_else(|| TransportError::Protocol("empty invitation".into()))
        }
        fn shared_change_role(
            &mut self,
            id: &SpaceId,
            account: &AccountId,
            req: &ChangeRoleRequest,
        ) -> TResult<MemberRecord> {
            let path = space_path(routes::SHARED_NOTEBOOK_MEMBER, id)
                .replace("{account_id}", account.as_str());
            Self::json(&self.send_json("PATCH", &path, Some(req))?)
        }
        fn shared_remove_member(&mut self, id: &SpaceId, account: &AccountId) -> TResult<()> {
            let path = space_path(routes::SHARED_NOTEBOOK_MEMBER, id)
                .replace("{account_id}", account.as_str());
            self.send_json::<()>("DELETE", &path, None).map(|_| ())
        }
        fn invitations_list(&mut self) -> TResult<IncomingInvitationList> {
            let l: IncomingInvitationList = self.get_json(routes::INVITATIONS)?;
            for i in &l.invitations {
                i.validate()
                    .map_err(|e| TransportError::Protocol(format!("invitation: {e}")))?;
            }
            Ok(l)
        }
        fn invitation_accept(&mut self, inv: &InvitationId) -> TResult<SharedNotebookRecord> {
            let r = self
                .post_json(
                    &inv_path(routes::INVITATION_ACCEPT, inv),
                    &serde_json::json!({}),
                )?
                .ok_or_else(|| TransportError::Protocol("empty shared notebook".into()))?;
            checked_record(r)
        }
        fn invitation_decline(&mut self, inv: &InvitationId) -> TResult<()> {
            self.post_json::<_, serde_json::Value>(
                &inv_path(routes::INVITATION_DECLINE, inv),
                &serde_json::json!({}),
            )
            .map(|_| ())
        }
        fn invitation_cancel(&mut self, inv: &InvitationId) -> TResult<()> {
            self.send_json::<()>("DELETE", &inv_path(routes::INVITATION, inv), None)
                .map(|_| ())
        }
    }

    /// The comment and notification routes, over the same server and session as this transport
    /// (the space's device session for comments, the account's for notifications).
    impl crate::comments::CommentTransport for HttpTransport {
        fn comment_push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse> {
            self.comments().comment_push(frame)
        }
        fn comment_pull(
            &mut self,
            req: &PullOpsRequest,
            limits: &Limits,
        ) -> TResult<crate::comments::PulledComments> {
            self.comments().comment_pull(req, limits)
        }
        fn comment_ack(&mut self, req: &AckRequest) -> TResult<()> {
            self.comments().comment_ack(req)
        }
        fn notifications_list(
            &mut self,
            cursor: Option<&penroam_sync_protocol::Cursor>,
            limit: u32,
        ) -> TResult<penroam_sync_protocol::comments::NotificationPage> {
            self.comments().notifications_list(cursor, limit)
        }
        fn notification_update(
            &mut self,
            id: &penroam_sync_protocol::NotificationId,
            read: bool,
        ) -> TResult<penroam_sync_protocol::comments::NotificationRecord> {
            self.comments().notification_update(id, read)
        }
        fn notification_prefs_get(
            &mut self,
        ) -> TResult<penroam_sync_protocol::comments::NotificationPrefs> {
            self.comments().notification_prefs_get()
        }
        fn notification_prefs_put(
            &mut self,
            prefs: &penroam_sync_protocol::comments::NotificationPrefs,
        ) -> TResult<()> {
            self.comments().notification_prefs_put(prefs)
        }
    }

    impl HttpTransport {
        fn comments(&self) -> crate::comments::HttpCommentTransport {
            crate::comments::HttpCommentTransport::new(&self.base, self.token.clone())
        }
    }

    // attribution uses this transport's base URL and session, like comments
    impl crate::attribution::AttributionTransport for HttpTransport {
        fn attribution_page(
            &mut self,
            stream: crate::attribution::Stream,
            from_seq: u64,
            limit: u32,
        ) -> TResult<penroam_sync_protocol::history::AttributionPage> {
            crate::attribution::HttpAttribution::new(&self.base, self.token.clone())
                .attribution_page(stream, from_seq, limit)
        }
    }

    fn vault_path(id: &AccountId) -> String {
        routes::VAULT.replace("{id}", id.as_str())
    }

    fn space_path(route: &str, id: &SpaceId) -> String {
        route.replace("{id}", id.as_str())
    }

    fn inv_path(route: &str, inv: &InvitationId) -> String {
        route.replace("{inv}", inv.as_str())
    }

    /// Records from the server are checked before the app relies on them.
    fn checked_record(r: SharedNotebookRecord) -> TResult<SharedNotebookRecord> {
        r.validate()
            .map_err(|e| TransportError::Protocol(format!("shared notebook: {e}")))?;
        Ok(r)
    }

    impl HttpTransport {
        fn send_json<B: serde::Serialize>(
            &self,
            method: &str,
            path: &str,
            body: Option<&B>,
        ) -> TResult<Vec<u8>> {
            let url = self.url(path);
            let auth = self.auth();
            let r = match (method, body) {
                ("PUT", Some(b)) => self
                    .agent
                    .put(url)
                    .header("Authorization", auth)
                    .send_json(b),
                ("PATCH", Some(b)) => self
                    .agent
                    .patch(url)
                    .header("Authorization", auth)
                    .send_json(b),
                ("DELETE", _) => self.agent.delete(url).header("Authorization", auth).call(),
                _ => return Err(TransportError::Protocol(format!("unsupported {method}"))),
            }
            .map_err(net)?;
            Ok(Self::finish(r, JSON_LIMIT)?.2)
        }

        fn sharing_impl_list(&self) -> TResult<SharedNotebookList> {
            let l: SharedNotebookList = self.get_json(routes::SHARED_NOTEBOOKS)?;
            for r in &l.spaces {
                r.validate()
                    .map_err(|e| TransportError::Protocol(format!("shared notebook: {e}")))?;
            }
            Ok(l)
        }
    }
}

/// Keep the unused-import lint quiet when no transport feature is enabled.
#[allow(dead_code)]
fn _frame_item_type_check(_: FrameItem) {}
