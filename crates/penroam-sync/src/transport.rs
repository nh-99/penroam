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
    AckRequest, BlobId, BlobPutResponse, CheckpointRecord, DeviceId, DeviceListResponse,
    DeviceRegistrationRequest, DeviceRegistrationResponse, ErrorBody, ErrorCode,
    KeyLogAppendRequest, KeyLogAppendResponse, KeyLogPage, Limits, PullOpsRequest, PullOpsResponse,
    PulledOp, PushOpsResponse, ServerMeta,
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
    /// `GET /v1/devices`: what the server has registered. Untrusted: used only
    /// to find a pairing device's keys, which are then checked against its code.
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
    use penroam_sync_testkit::{InMemoryServer, RequestError};
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
            }
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
                .register_device_keys(&self.account, req)
                .map_err(map)?;
            self.device = r.device_id.clone();
            Ok(r)
        }
        fn list_devices(&mut self) -> TResult<DeviceListResponse> {
            self.up()?;
            self.server.list_devices(&self.account).map_err(map)
        }
        fn revoke_device(&mut self, id: &DeviceId) -> TResult<()> {
            self.up()?;
            self.server
                .revoke_device_as(&self.account, &self.device, id)
                .map_err(map)
        }
        fn push(&mut self, frame: Vec<u8>) -> TResult<PushOpsResponse> {
            self.up()?;
            self.server
                .push(&self.account, &self.device, &frame)
                .map_err(map)
        }
        fn pull(&mut self, req: &PullOpsRequest, limits: &Limits) -> TResult<PulledPage> {
            self.up()?;
            let page = self
                .server
                .pull(&self.account, &self.device, req)
                .map_err(map)?;
            let items: Vec<(PulledOp, Vec<u8>)> = decode_json_batch(&page.frame, limits)
                .map_err(|e| TransportError::Protocol(e.to_string()))?;
            pulled_from_frame(page.response, items)
        }
        fn ack(&mut self, req: &AckRequest) -> TResult<()> {
            self.up()?;
            self.server
                .ack(&self.account, &self.device, req)
                .map(|_| ())
                .map_err(map)
        }
        fn keylog_get(&mut self, from: u64) -> TResult<KeyLogPage> {
            self.up()?;
            self.server.keylog_get(&self.account, from).map_err(map)
        }
        fn keylog_append(&mut self, req: &KeyLogAppendRequest) -> TResult<KeyLogAppendResponse> {
            self.up()?;
            self.server.keylog_append(&self.account, req).map_err(map)
        }
        fn blob_put(
            &mut self,
            id: &BlobId,
            sha256_hex: &str,
            bytes: &[u8],
        ) -> TResult<BlobPutResponse> {
            self.up()?;
            self.server
                .blob_put(&self.account, &self.device, id, sha256_hex, bytes)
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
            self.server
                .blob_get(&self.account, &self.device, id)
                .map_err(map)
        }
        fn checkpoint_put(&mut self, rec: &CheckpointRecord) -> TResult<()> {
            self.up()?;
            self.server
                .checkpoint_put(&self.account, &self.device, rec)
                .map_err(map)
        }
        fn checkpoint_latest(&mut self) -> TResult<Option<CheckpointRecord>> {
            self.up()?;
            match self.server.checkpoint_latest(&self.account, &self.device) {
                Ok(r) => Ok(Some(r)),
                Err(RequestError::Api(b)) if b.code == ErrorCode::NotFound => Ok(None),
                Err(e) => Err(map(e)),
            }
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

        /// Development test server only: obtain a session token
        /// (`POST /_test/session`). Real servers use their own sign-in.
        pub fn test_session(
            base_url: &str,
            account: &str,
            device_id: Option<&str>,
        ) -> TResult<(String, String)> {
            let base = base_url.trim_end_matches('/');
            let mut body = serde_json::json!({ "account": account });
            if let Some(d) = device_id {
                body["device_id"] = serde_json::Value::String(d.to_string());
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
    }
}

/// Keep the unused-import lint quiet when no transport feature is enabled.
#[allow(dead_code)]
fn _frame_item_type_check(_: FrameItem) {}
