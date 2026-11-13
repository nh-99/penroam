//! HTTP/1.1 front end for [`InMemoryServer`]: the **development test
//! server**. It has a stub login, no TLS, no rate limiting and no hardening.
//! It is meant for loopback use in tests and local development only.
//!
//! Every response carries `Connection: close` and a `Penroam-Request-Id`.
//! Request bodies need `Content-Length` (chunked uploads are refused), and
//! the size limit is checked before the body is read.
//!
//! # Mapping
//!
//! | Route | Auth | Request | Response |
//! |---|---|---|---|
//! | `POST /_test/session` | none | `{"account": id, "device_id"?: id, "email"?: string, "member"?: id}` | `{"token", "account_id"}`. Creates the account; a `device_id` must be registered (else 403 `forbidden`). `email` records a verified sign-in email for the account, verified now (invitations match it). `member` makes an account session on a sharing space act as that member (a device session acts as the member who registered the device) |
//! | `GET /v1/meta` | none | — | `ServerMeta` |
//! | `POST /v1/devices` | account | `DeviceRegistrationRequest` | `DeviceRegistrationResponse` |
//! | `GET /v1/devices` | account | — | `DeviceListResponse` |
//! | `DELETE /v1/devices/{id}` | device | — | 204 |
//! | `POST /v1/ops/batch` | device | PRSB frame (`application/vnd.penroam.batch.v1`) | `PushOpsResponse` |
//! | `GET /v1/ops?cursor=&limit=` | device | — | PRSB frame of `PulledOp`; headers `Penroam-Next-Cursor`, `Penroam-Has-More: true\|false`. With `Accept: application/json` the JSON `PullOpsResponse` (no ciphertext) instead |
//! | `POST /v1/acks` | device | `AckRequest` | 204 |
//! | `GET /v1/usage` | account | — | `UsageResponse` |
//! | `GET /v1/keylog?from=N` | account | — | `KeyLogPage` |
//! | `POST /v1/keylog` | account | `KeyLogAppendRequest` | `KeyLogAppendResponse` |
//! | `PUT /v1/blobs/{id}` | device | raw bytes + `Penroam-Content-Sha256` | `BlobPutResponse` |
//! | `GET /v1/blobs/{id}` | device | — | raw bytes (`application/octet-stream`) |
//! | `GET`, `PUT /v1/account/key` | account | `AccountKeyDocument` | `AccountKeyDocument`, 204 |
//! | `GET`, `POST /v1/vaults` | account | `CreateVaultRequest` | `VaultListResponse`, `VaultRecord` |
//! | `PUT`, `DELETE /v1/vaults/{id}` | account | `UpdateVaultLockRequest` | 204 |
//! | `POST /v1/vaults/{id}/session` | account | — | `SessionTokens` for the vault's account |
//! | `GET`, `POST /v1/shared-notebooks` | account | `CreateSpaceRequest` | `SharedNotebookList`, `SharedNotebookRecord` |
//! | `PUT /v1/shared-notebooks/{id}/meta` | account | `UpdateSpaceMetaRequest` | 204 |
//! | `POST /v1/shared-notebooks/{id}/session` | account | — | `SessionTokens` for the space, acting as the caller |
//! | `GET`, `PUT /v1/shared-notebooks/{id}/key` | account | `AccountKeyDocument` | `AccountKeyDocument`, 204 |
//! | `GET /v1/shared-notebooks/{id}/members` | account | — | `MemberList` |
//! | `PATCH`, `DELETE /v1/shared-notebooks/{id}/members/{account_id}` | account | `ChangeRoleRequest` | `MemberRecord`, 204 |
//! | `POST /v1/shared-notebooks/{id}/invitations` | account | `CreateInvitationRequest` | `InvitationRecord` |
//! | `GET /v1/invitations` | account | — | `IncomingInvitationList` |
//! | `POST /v1/invitations/{inv}/accept`, `…/decline` | account | — | `SharedNotebookRecord`, 204 |
//! | `DELETE /v1/invitations/{inv}` | account | — | 204 |
//!
//! The sharing routes need a token of the caller's own account (a vault or space session gets
//! 403 `forbidden`). Sync routes used with a space session apply the member's role.
//!
//! "account" means any token from `/_test/session`, and "device" means a
//! token issued with a `device_id`. Every `/v1/*` route except `/v1/meta`
//! needs `Authorization: Bearer <token>`. A missing or unknown token gets
//! 401 `unauthenticated`. Tokens live in memory and must be re-issued after
//! a restart.
//!
//! Errors are an `ErrorBody` JSON with the status from
//! `ErrorCode::http_status`. When the body has `retry_after_ms`, a
//! `Retry-After` header (seconds, rounded up) is added. A dropped response
//! (`drop_next_responses`) closes the connection without sending anything.
//! The request was still processed and persisted.
//!
//! Test-control routes take no auth and accept loopback peers only:
//! `POST /_test/fail-next {"n", "code"}`, `/_test/drop-next {"n"}`,
//! `/_test/delay-next {"n", "ms"}`, `/_test/revoke {"account", "device_id"}`,
//! `/_test/expire-cursors {"account", "server_seq"}`,
//! `/_test/compact {"account"}` (expire cursors before the latest checkpoint),
//! `/_test/quota {"account", "quota_bytes": n|null}`,
//! `/_test/clock {"now_ms": n|null}` (pin or release the clock used for invitation expiry),
//! `/_test/advance-clock {"ms"}`, `/_test/reauth-window {"ms": n|null}` (accepting and declining
//! invitations need an email verified by a `/_test/session` with `email` within that long; listing
//! answers an empty list flagged `reauth_required`), `/_test/device-cap {"n"}` (live devices per member in a space).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use penroam_sync_protocol::routes::{
    HEADER_CONTENT_SHA256, HEADER_HAS_MORE, HEADER_NEXT_CURSOR, HEADER_REQUEST_ID, MEDIA_BATCH,
    MEDIA_JSON, MEDIA_OCTETS,
};
use penroam_sync_protocol::{
    AccountId, AccountKeyDocument, AckRequest, BlobId, ChangeRoleRequest, CheckpointRecord,
    CreateInvitationRequest, CreateSpaceRequest, CreateVaultRequest, Cursor, DeviceId,
    DeviceRegistrationRequest, ErrorBody, ErrorCode, InvitationId, KeyLogAppendRequest,
    PullOpsRequest, SessionTokens, UpdateSpaceMetaRequest, UpdateVaultLockRequest,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{Caller, InMemoryServer, RequestError};

const WORKERS: usize = 8;
const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_JSON_BYTES: u64 = 1 << 20;
const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// What a bearer token stands for: an account (or vault, or space), optionally a device, and for
/// an account session on a sharing space, the member acting in it.
#[derive(Clone)]
struct Principal {
    account: AccountId,
    device: Option<DeviceId>,
    member: Option<AccountId>,
}

struct Ctx {
    server: Arc<InMemoryServer>,
    tokens: Mutex<HashMap<String, Principal>>,
    counter: AtomicU64,
    stop: AtomicBool,
}

/// A running HTTP test server. Dropping it stops the server.
pub struct HttpHandle {
    /// `http://127.0.0.1:PORT`
    pub base_url: String,
    addr: SocketAddr,
    ctx: Arc<Ctx>,
    threads: Vec<JoinHandle<()>>,
}

impl HttpHandle {
    /// Stops accepting connections and joins the worker threads.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if self.ctx.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // wake every worker blocked in accept()
        for _ in 0..self.threads.len() {
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for HttpHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Binds `addr` (e.g. `127.0.0.1:0`) and serves `server` on worker threads.
pub fn spawn(addr: &str, server: Arc<InMemoryServer>) -> std::io::Result<HttpHandle> {
    let listener = TcpListener::bind(addr)?;
    let addr = listener.local_addr()?;
    let ctx = Arc::new(Ctx {
        server,
        tokens: Mutex::new(HashMap::new()),
        counter: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });
    let mut threads = Vec::new();
    for _ in 0..WORKERS {
        let l = listener.try_clone()?;
        let ctx = ctx.clone();
        threads.push(std::thread::spawn(move || loop {
            let conn = l.accept();
            if ctx.stop.load(Ordering::SeqCst) {
                return;
            }
            if let Ok((stream, peer)) = conn {
                handle(&ctx, stream, peer);
            }
        }));
    }
    Ok(HttpHandle {
        base_url: format!("http://{addr}"),
        addr,
        ctx,
        threads,
    })
}

// ------------------------------------------------------------- parsing --

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

enum Outcome {
    Respond(Response),
    /// Close the connection without a response.
    Drop,
}

struct Response {
    status: u16,
    content_type: &'static str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn json<T: Serialize>(status: u16, v: &T) -> Self {
        Self {
            status,
            content_type: MEDIA_JSON,
            headers: Vec::new(),
            body: serde_json::to_vec(v).expect("DTOs serialize"),
        }
    }
    fn empty() -> Self {
        Self {
            status: 204,
            content_type: MEDIA_JSON,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
    fn error(body: ErrorBody) -> Self {
        let mut r = Self::json(body.code.http_status(), &body);
        if let Some(ms) = body.retry_after_ms {
            r.headers
                .push(("Retry-After".into(), ms.div_ceil(1000).to_string()));
        }
        r.headers
            .push((HEADER_REQUEST_ID.into(), body.request_id.clone()));
        r
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        _ => "Status",
    }
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (k.to_string(), v.to_string()),
            None => (kv.to_string(), String::new()),
        })
        .collect()
}

/// Body size limit for a route, decided before the body is read.
fn body_limit(ctx: &Ctx, method: &str, path: &str) -> u64 {
    let limits = ctx.server.limits();
    if method == "POST" && path == "/v1/ops/batch" {
        limits.max_batch_bytes
    } else if method == "PUT" && path.starts_with("/v1/blobs/") {
        limits.max_blob_bytes
    } else {
        MAX_JSON_BYTES
    }
}

/// Serves one connection: read one request, route it, apply any injected
/// delay, then write the response (or close silently for a dropped one).
fn handle(ctx: &Ctx, stream: TcpStream, peer: SocketAddr) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let n = ctx.counter.fetch_add(1, Ordering::SeqCst) + 1;
    let request_id = format!("http-{n}");
    let (outcome, control) = match read_request(ctx, &stream, &request_id) {
        Ok(req) => {
            let control = req.path.starts_with("/_test/");
            (route(ctx, req, peer, &request_id), control)
        }
        Err(resp) => (Outcome::Respond(*resp), false),
    };
    // injected delays apply to protocol requests, never to test-control ones
    if !control {
        if let Some(ms) = ctx.server.take_delay() {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
    match outcome {
        Outcome::Drop => {
            let _ = stream.shutdown(Shutdown::Both);
        }
        Outcome::Respond(mut r) => {
            if !r.headers.iter().any(|(k, _)| k == HEADER_REQUEST_ID) {
                r.headers.push((HEADER_REQUEST_ID.into(), request_id));
            }
            let mut head = format!(
                "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                r.status,
                reason(r.status),
                r.body.len()
            );
            if !r.body.is_empty() {
                head.push_str(&format!("Content-Type: {}\r\n", r.content_type));
            }
            for (k, v) in &r.headers {
                head.push_str(&format!("{k}: {v}\r\n"));
            }
            head.push_str("\r\n");
            let mut w = &stream;
            let _ = w.write_all(head.as_bytes());
            let _ = w.write_all(&r.body);
            let _ = w.flush();
            let _ = stream.shutdown(Shutdown::Write);
            // drain whatever the client still sends so it sees a clean close
            let mut sink = [0u8; 1024];
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            while matches!((&stream).read(&mut sink), Ok(k) if k > 0) {}
        }
    }
}

fn http_err(code: ErrorCode, msg: &str, request_id: &str) -> Box<Response> {
    Box::new(Response::error(ErrorBody::new(code, msg, request_id)))
}

/// Parses the request line and headers, then reads a `Content-Length` body
/// after checking it against the route's size limit (so oversized bodies are
/// refused before they are read).
fn read_request(ctx: &Ctx, stream: &TcpStream, rid: &str) -> Result<Request, Box<Response>> {
    let mut reader = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let before = head.len();
        let k = reader
            .by_ref()
            .take((MAX_HEADER_BYTES + 1 - head.len().min(MAX_HEADER_BYTES)) as u64)
            .read_until(b'\n', &mut head)
            .map_err(|_| http_err(ErrorCode::MalformedRequest, "unreadable request", rid))?;
        if k == 0 {
            return Err(http_err(
                ErrorCode::MalformedRequest,
                "truncated request",
                rid,
            ));
        }
        if head.len() > MAX_HEADER_BYTES {
            return Err(http_err(
                ErrorCode::MalformedRequest,
                "headers too large",
                rid,
            ));
        }
        if &head[before..] == b"\r\n" || &head[before..] == b"\n" {
            break;
        }
    }
    let text = String::from_utf8(head)
        .map_err(|_| http_err(ErrorCode::MalformedRequest, "headers are not UTF-8", rid))?;
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(http_err(
            ErrorCode::MalformedRequest,
            "bad request line",
            rid,
        ));
    };
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target.to_string(), HashMap::new()),
    };
    if headers
        .get("transfer-encoding")
        .is_some_and(|t| !t.eq_ignore_ascii_case("identity"))
    {
        return Err(Box::new(Response {
            status: 411,
            ..*http_err(
                ErrorCode::MalformedRequest,
                "chunked bodies are not supported; send Content-Length",
                rid,
            )
        }));
    }
    let len: u64 = match headers.get("content-length") {
        Some(v) => v
            .parse()
            .map_err(|_| http_err(ErrorCode::MalformedRequest, "bad Content-Length", rid))?,
        None => 0,
    };
    if len > body_limit(ctx, method, &path) {
        return Err(http_err(
            ErrorCode::PayloadTooLarge,
            "request body too large",
            rid,
        ));
    }
    let mut body = vec![0u8; len as usize];
    reader
        .read_exact(&mut body)
        .map_err(|_| http_err(ErrorCode::MalformedRequest, "truncated body", rid))?;
    Ok(Request {
        method: method.to_string(),
        path,
        query,
        headers,
        body,
    })
}

// ------------------------------------------------------------- routing --

fn parse_json<T: DeserializeOwned>(req: &Request, rid: &str) -> Result<T, Box<Response>> {
    serde_json::from_slice(&req.body).map_err(|e| {
        http_err(
            ErrorCode::MalformedRequest,
            &format!("invalid JSON: {e}"),
            rid,
        )
    })
}

/// Maps an [`InMemoryServer`] result to HTTP: errors become `ErrorBody`
/// responses, and a simulated lost response closes the connection.
fn api<T>(r: Result<T, RequestError>, ok: impl FnOnce(T) -> Response) -> Outcome {
    match r {
        Ok(v) => Outcome::Respond(ok(v)),
        Err(RequestError::Api(body)) => Outcome::Respond(Response::error(body)),
        Err(RequestError::ResponseLost) => Outcome::Drop,
    }
}

#[derive(Deserialize)]
struct SessionReq {
    account: AccountId,
    #[serde(default)]
    device_id: Option<DeviceId>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    member: Option<AccountId>,
}

#[derive(Serialize)]
struct SessionResp {
    token: String,
    account_id: AccountId,
}

#[derive(Deserialize)]
struct FailNext {
    n: u32,
    code: ErrorCode,
}
#[derive(Deserialize)]
struct DropNext {
    n: u32,
}
#[derive(Deserialize)]
struct DelayNext {
    n: u32,
    ms: u64,
}
#[derive(Deserialize)]
struct Revoke {
    account: AccountId,
    device_id: DeviceId,
}
#[derive(Deserialize)]
struct Expire {
    account: AccountId,
    server_seq: u64,
}
#[derive(Deserialize)]
struct Quota {
    account: AccountId,
    quota_bytes: Option<u64>,
}

fn route(ctx: &Ctx, req: Request, peer: SocketAddr, rid: &str) -> Outcome {
    match route_inner(ctx, &req, peer, rid) {
        Ok(o) => o,
        Err(r) => Outcome::Respond(*r),
    }
}

/// Dispatches by method and path; checks auth (account vs device tokens) and
/// restricts `/_test/*` to loopback peers.
fn route_inner(
    ctx: &Ctx,
    req: &Request,
    peer: SocketAddr,
    rid: &str,
) -> Result<Outcome, Box<Response>> {
    let s = &ctx.server;
    let m = req.method.as_str();
    let p = req.path.as_str();
    if let Some(ctl) = p.strip_prefix("/_test/") {
        if !peer.ip().is_loopback() {
            return Err(http_err(
                ErrorCode::Forbidden,
                "test routes are loopback-only",
                rid,
            ));
        }
        if m != "POST" {
            return Err(http_err(ErrorCode::NotFound, "unknown route", rid));
        }
        match ctl {
            "session" => {
                let r: SessionReq = parse_json(req, rid)?;
                s.create_account(&r.account);
                if let Some(d) = &r.device_id {
                    if !s.device_registered(&r.account, d) {
                        return Err(http_err(ErrorCode::Forbidden, "device not registered", rid));
                    }
                }
                if let Some(email) = &r.email {
                    s.set_verified_email(&r.account, Some(email))
                        .map_err(|e| http_err(e.error_code(), &e.to_string(), rid))?;
                }
                let token = issue(
                    ctx,
                    Principal {
                        account: r.account.clone(),
                        device: r.device_id,
                        member: r.member,
                    },
                );
                return Ok(Outcome::Respond(Response::json(
                    200,
                    &SessionResp {
                        token,
                        account_id: r.account,
                    },
                )));
            }
            "fail-next" => {
                let r: FailNext = parse_json(req, rid)?;
                s.fail_next_requests(r.n, r.code);
            }
            "drop-next" => {
                let r: DropNext = parse_json(req, rid)?;
                s.drop_next_responses(r.n);
            }
            "delay-next" => {
                let r: DelayNext = parse_json(req, rid)?;
                s.delay_next_responses(r.n, r.ms);
            }
            "revoke" => {
                let r: Revoke = parse_json(req, rid)?;
                s.revoke_device(&r.account, &r.device_id);
            }
            "expire-cursors" => {
                let r: Expire = parse_json(req, rid)?;
                s.expire_cursors_before(&r.account, r.server_seq);
            }
            "compact" => {
                #[derive(serde::Deserialize)]
                struct Compact {
                    account: AccountId,
                }
                let r: Compact = parse_json(req, rid)?;
                s.compact_to_latest_checkpoint(&r.account);
            }
            "quota" => {
                let r: Quota = parse_json(req, rid)?;
                s.set_quota(&r.account, r.quota_bytes);
            }
            "clock" => {
                #[derive(serde::Deserialize)]
                struct Clock {
                    now_ms: Option<u64>,
                }
                let r: Clock = parse_json(req, rid)?;
                s.set_clock_ms(r.now_ms);
            }
            "reauth-window" => {
                #[derive(serde::Deserialize)]
                struct Window {
                    ms: Option<u64>,
                }
                let r: Window = parse_json(req, rid)?;
                s.set_reauth_window_ms(r.ms);
            }
            "device-cap" => {
                #[derive(serde::Deserialize)]
                struct Cap {
                    n: usize,
                }
                let r: Cap = parse_json(req, rid)?;
                s.set_member_device_cap(r.n);
            }
            "advance-clock" => {
                #[derive(serde::Deserialize)]
                struct Advance {
                    ms: u64,
                }
                let r: Advance = parse_json(req, rid)?;
                s.advance_clock_ms(r.ms);
            }
            _ => return Err(http_err(ErrorCode::NotFound, "unknown route", rid)),
        }
        return Ok(Outcome::Respond(Response::empty()));
    }

    if m == "GET" && p == "/v1/meta" {
        return Ok(Outcome::Respond(Response::json(200, &s.meta())));
    }
    if !p.starts_with("/v1/") {
        return Err(http_err(ErrorCode::NotFound, "unknown route", rid));
    }
    let token = req
        .headers
        .get("authorization")
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::trim);
    let auth = token.and_then(|t| {
        ctx.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(t)
            .cloned()
    });
    let Some(Principal {
        account,
        device,
        member,
    }) = auth
    else {
        return Err(http_err(
            ErrorCode::Unauthenticated,
            "missing or unknown bearer token",
            rid,
        ));
    };
    let caller = Caller {
        account: &account,
        member: member.as_ref(),
    };
    let need_device = || {
        device
            .clone()
            .ok_or_else(|| http_err(ErrorCode::Forbidden, "this route needs a device token", rid))
    };

    let out = match (m, p) {
        ("POST", "/v1/devices") => {
            let r: DeviceRegistrationRequest = parse_json(req, rid)?;
            api(s.register_device_keys(caller, &r), |v| {
                Response::json(200, &v)
            })
        }
        ("GET", "/v1/devices") => api(s.list_devices(caller), |v| Response::json(200, &v)),
        ("DELETE", _) if p.starts_with("/v1/devices/") => {
            let caller_device = need_device()?;
            let target: DeviceId = p["/v1/devices/".len()..]
                .parse()
                .map_err(|_| http_err(ErrorCode::MalformedRequest, "bad device id", rid))?;
            api(s.revoke_device_as(caller, &caller_device, &target), |_| {
                Response::empty()
            })
        }
        ("POST", "/v1/ops/batch") => {
            let d = need_device()?;
            api(s.push(caller, &d, &req.body), |v| Response::json(200, &v))
        }
        ("GET", "/v1/ops") => {
            let d = need_device()?;
            let cursor = match req.query.get("cursor").filter(|c| !c.is_empty()) {
                Some(c) => Some(
                    Cursor::new(c.clone())
                        .map_err(|_| http_err(ErrorCode::InvalidCursor, "malformed cursor", rid))?,
                ),
                None => None,
            };
            let limit = match req.query.get("limit") {
                Some(l) => l
                    .parse()
                    .map_err(|_| http_err(ErrorCode::MalformedRequest, "bad limit", rid))?,
                None => 100,
            };
            let json = req
                .headers
                .get("accept")
                .is_some_and(|a| a.contains(MEDIA_JSON) && !a.contains(MEDIA_BATCH));
            api(
                s.pull(caller, &d, &PullOpsRequest { cursor, limit }),
                |page| {
                    if json {
                        Response::json(200, &page.response)
                    } else {
                        Response {
                            status: 200,
                            content_type: MEDIA_BATCH,
                            headers: vec![
                                (
                                    HEADER_NEXT_CURSOR.into(),
                                    page.response.next_cursor.as_str().to_string(),
                                ),
                                (HEADER_HAS_MORE.into(), page.response.has_more.to_string()),
                            ],
                            body: page.frame,
                        }
                    }
                },
            )
        }
        ("POST", "/v1/acks") => {
            let d = need_device()?;
            let r: AckRequest = parse_json(req, rid)?;
            api(s.ack(caller, &d, &r), |_| Response::empty())
        }
        ("GET", "/v1/usage") => api(s.usage(caller), |v| Response::json(200, &v)),
        ("GET", "/v1/account") => api(s.account_state(caller), |v| Response::json(200, &v)),
        // The test server offers no account actions (`actions` is always empty).
        ("POST", "/v1/account/billing-link")
        | ("POST", "/v1/account/deletion")
        | ("DELETE", "/v1/account/deletion") => {
            return Err(http_err(
                ErrorCode::Forbidden,
                "the test server offers no billing or account deletion",
                rid,
            ));
        }
        ("GET", "/v1/keylog") => {
            let from = match req.query.get("from") {
                Some(f) => f
                    .parse()
                    .map_err(|_| http_err(ErrorCode::MalformedRequest, "bad from", rid))?,
                None => 0,
            };
            api(s.keylog_get(caller, from), |v| Response::json(200, &v))
        }
        ("POST", "/v1/checkpoints") => {
            let d = need_device()?;
            let r: CheckpointRecord = parse_json(req, rid)?;
            api(s.checkpoint_put(caller, &d, &r), |_| Response::empty())
        }
        ("GET", "/v1/checkpoints/latest") => {
            let d = need_device()?;
            api(s.checkpoint_latest(caller, &d), |v| Response::json(200, &v))
        }
        ("POST", "/v1/keylog") => {
            let r: KeyLogAppendRequest = parse_json(req, rid)?;
            api(s.keylog_append(caller, &r), |v| Response::json(200, &v))
        }
        ("PUT", _) | ("GET", _) if p.starts_with("/v1/blobs/") => {
            let d = need_device()?;
            let id: BlobId = p["/v1/blobs/".len()..]
                .parse()
                .map_err(|_| http_err(ErrorCode::MalformedRequest, "bad blob id", rid))?;
            if m == "PUT" {
                let sha = req
                    .headers
                    .get(&HEADER_CONTENT_SHA256.to_ascii_lowercase())
                    .cloned()
                    .ok_or_else(|| {
                        http_err(
                            ErrorCode::ValidationFailed,
                            "Penroam-Content-Sha256 header required",
                            rid,
                        )
                    })?;
                api(s.blob_put(caller, &d, &id, &sha, &req.body), |v| {
                    Response::json(200, &v)
                })
            } else {
                api(s.blob_get(caller, &d, &id), |bytes| Response {
                    status: 200,
                    content_type: MEDIA_OCTETS,
                    headers: Vec::new(),
                    body: bytes,
                })
            }
        }
        ("GET", "/v1/account/key") => api(s.account_key_get(&account), |v| Response::json(200, &v)),
        ("PUT", "/v1/account/key") => {
            let r: AccountKeyDocument = parse_json(req, rid)?;
            api(s.account_key_put(&account, &r), |_| Response::empty())
        }
        ("GET", "/v1/vaults") => api(s.vault_list(&account), |v| Response::json(200, &v)),
        ("POST", "/v1/vaults") => {
            let r: CreateVaultRequest = parse_json(req, rid)?;
            api(s.vault_create(&account, &r), |v| Response::json(200, &v))
        }
        (_, _) if p.starts_with("/v1/vaults/") => {
            let rest = &p["/v1/vaults/".len()..];
            let (id, session) = match rest.strip_suffix("/session") {
                Some(id) => (id, true),
                None => (rest, false),
            };
            let vault: AccountId = id
                .parse()
                .map_err(|_| http_err(ErrorCode::MalformedRequest, "bad vault id", rid))?;
            match (m, session) {
                ("POST", true) => {
                    if !s.vault_owned_by(&account, &vault) {
                        return Err(http_err(ErrorCode::NotFound, "unknown vault", rid));
                    }
                    let token = issue(
                        ctx,
                        Principal {
                            account: vault.clone(),
                            device: None,
                            member: None,
                        },
                    );
                    Outcome::Respond(Response::json(
                        200,
                        &SessionTokens {
                            account_id: vault,
                            device_id: None,
                            access_token: token,
                            expires_in_s: 3600,
                            refresh_token: new_token(),
                        },
                    ))
                }
                ("PUT", false) => {
                    let r: UpdateVaultLockRequest = parse_json(req, rid)?;
                    api(s.vault_set_lock(&account, &vault, &r), |_| {
                        Response::empty()
                    })
                }
                ("DELETE", false) => api(s.vault_delete(&account, &vault), |_| Response::empty()),
                _ => return Err(http_err(ErrorCode::NotFound, "unknown route", rid)),
            }
        }
        ("GET", "/v1/shared-notebooks") => {
            api(s.shared_list(&account), |v| Response::json(200, &v))
        }
        ("POST", "/v1/shared-notebooks") => {
            let r: CreateSpaceRequest = parse_json(req, rid)?;
            api(s.shared_create(&account, &r), |v| Response::json(200, &v))
        }
        (_, _) if p.starts_with("/v1/shared-notebooks/") => {
            let rest = &p["/v1/shared-notebooks/".len()..];
            let (id, sub) = rest.split_once('/').unwrap_or((rest, ""));
            let space: AccountId = id
                .parse()
                .map_err(|_| http_err(ErrorCode::NotFound, "unknown shared notebook", rid))?;
            route_space(ctx, req, rid, &account, &space, sub)?
        }
        ("GET", "/v1/invitations") => api(s.invitations_incoming(&account), |v| {
            Response::json(200, &v)
        }),
        (_, _) if p.starts_with("/v1/invitations/") => {
            let rest = &p["/v1/invitations/".len()..];
            let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
            let inv: InvitationId = id
                .parse()
                .map_err(|_| http_err(ErrorCode::NotFound, "unknown invitation", rid))?;
            match (m, action) {
                ("POST", "accept") => api(s.invitation_accept(&account, &inv), |v| {
                    Response::json(200, &v)
                }),
                ("POST", "decline") => {
                    api(s.invitation_decline(&account, &inv), |_| Response::empty())
                }
                ("DELETE", "") => api(s.invitation_cancel(&account, &inv), |_| Response::empty()),
                _ => return Err(http_err(ErrorCode::NotFound, "unknown route", rid)),
            }
        }
        _ => return Err(http_err(ErrorCode::NotFound, "unknown route", rid)),
    };
    Ok(out)
}

/// `/v1/shared-notebooks/{id}/{sub}` as the caller's own account.
fn route_space(
    ctx: &Ctx,
    req: &Request,
    rid: &str,
    account: &AccountId,
    space: &AccountId,
    sub: &str,
) -> Result<Outcome, Box<Response>> {
    let s = &ctx.server;
    let m = req.method.as_str();
    let member_of = |rest: &str| -> Result<AccountId, Box<Response>> {
        rest.parse()
            .map_err(|_| http_err(ErrorCode::NotFound, "not a member", rid))
    };
    Ok(match (m, sub) {
        ("PUT", "meta") => {
            let r: UpdateSpaceMetaRequest = parse_json(req, rid)?;
            api(s.shared_set_meta(account, space, &r), |_| Response::empty())
        }
        ("POST", "session") => match s.shared_session_check(account, space) {
            Ok(_) => {
                let token = issue(
                    ctx,
                    Principal {
                        account: space.clone(),
                        device: None,
                        member: Some(account.clone()),
                    },
                );
                Outcome::Respond(Response::json(
                    200,
                    &SessionTokens {
                        account_id: space.clone(),
                        device_id: None,
                        access_token: token,
                        expires_in_s: 3600,
                        refresh_token: new_token(),
                    },
                ))
            }
            Err(e) => api::<()>(Err(e), |_| Response::empty()),
        },
        ("GET", "key") => api(s.shared_key_get(account, space), |v| {
            Response::json(200, &v)
        }),
        ("PUT", "key") => {
            let r: AccountKeyDocument = parse_json(req, rid)?;
            api(s.shared_key_put(account, space, &r), |_| Response::empty())
        }
        ("GET", "members") => api(s.shared_members(account, space), |v| {
            Response::json(200, &v)
        }),
        ("POST", "invitations") => {
            let r: CreateInvitationRequest = parse_json(req, rid)?;
            api(s.shared_invite(account, space, &r), |v| {
                Response::json(200, &v)
            })
        }
        ("PATCH", _) if sub.starts_with("members/") => {
            let target = member_of(&sub["members/".len()..])?;
            let r: ChangeRoleRequest = parse_json(req, rid)?;
            api(s.shared_change_role(account, space, &target, &r), |v| {
                Response::json(200, &v)
            })
        }
        ("DELETE", _) if sub.starts_with("members/") => {
            let target = member_of(&sub["members/".len()..])?;
            api(s.shared_remove_member(account, space, &target), |_| {
                Response::empty()
            })
        }
        _ => return Err(http_err(ErrorCode::NotFound, "unknown route", rid)),
    })
}

/// Stores a new bearer token for `p`.
fn issue(ctx: &Ctx, p: Principal) -> String {
    let token = new_token();
    ctx.tokens
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(token.clone(), p);
    token
}

fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
