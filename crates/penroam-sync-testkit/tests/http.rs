//! HTTP round trips against the development test server, using a tiny
//! hand-rolled HTTP/1.1 client (no client dependency).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::framing::{decode_json_batch, encode_json_batch};
use penroam_sync_protocol::{
    BlobPutResponse, DeviceRegistrationRequest, DeviceRegistrationResponse, EnvelopeHeader,
    ErrorBody, ErrorCode, KeyLogAppendRequest, KeyLogPage, Limits, PulledOp, PushOpsResponse,
    ServerMeta, UsageResponse,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{http, InMemoryServer};

struct Resp {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Resp {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn error(&self) -> ErrorBody {
        self.json()
    }
}

/// `None` when the server closed the connection without a response.
fn call(
    base: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Option<Resp> {
    let addr = base.strip_prefix("http://").unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(t) = token {
        req.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    if raw.is_empty() {
        return None;
    }
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header end");
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect::<HashMap<_, _>>();
    let body = raw[split + 4..].to_vec();
    assert_eq!(
        headers
            .get("content-length")
            .map(|l| l.parse::<usize>().unwrap()),
        Some(body.len())
    );
    Some(Resp {
        status,
        headers,
        body,
    })
}

fn json_call<T: serde::Serialize>(
    base: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    v: &T,
) -> Option<Resp> {
    call(
        base,
        method,
        path,
        token,
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(v).unwrap(),
    )
}

fn session(base: &str, account: &str, device: Option<&str>) -> String {
    let v = match device {
        Some(d) => serde_json::json!({"account": account, "device_id": d}),
        None => serde_json::json!({"account": account}),
    };
    let r = json_call(base, "POST", "/_test/session", None, &v).unwrap();
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let v: serde_json::Value = r.json();
    assert_eq!(v["account_id"], account);
    v["token"].as_str().unwrap().to_string()
}

fn op(n: u64) -> (EnvelopeHeader, Vec<u8>) {
    let ct = fake_ciphertext(&format!("http-{n}"), 30 + n as usize);
    (header_for(&format!("op-{n}"), "dev-a", n, &[], &ct), ct)
}

fn setup() -> (http::HttpHandle, Arc<InMemoryServer>, String) {
    let server = Arc::new(InMemoryServer::new());
    let h = http::spawn("127.0.0.1:0", server.clone()).unwrap();
    let base = h.base_url.clone();
    let acct = session(&base, "acct-1", None);
    let reg = DeviceRegistrationRequest {
        device_id: Some("dev-a".parse().unwrap()),
        signing_public_key: base64url::encode(&[1; 32]),
        encryption_public_key: base64url::encode(&[2; 32]),
        label: None,
    };
    let r = json_call(&base, "POST", "/v1/devices", Some(&acct), &reg).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(
        r.json::<DeviceRegistrationResponse>().device_id.as_str(),
        "dev-a"
    );
    let dev = session(&base, "acct-1", Some("dev-a"));
    (h, server, dev)
}

#[test]
fn auth_meta_and_unknown_routes() {
    let (h, _s, dev) = setup();
    let base = &h.base_url;
    let meta: ServerMeta = call(base, "GET", "/v1/meta", None, &[], b"")
        .unwrap()
        .json();
    assert!(meta.has_capability("keylog.v1"));
    let r = call(base, "GET", "/v1/usage", None, &[], b"").unwrap();
    assert_eq!(r.status, 401);
    assert_eq!(r.error().code, ErrorCode::Unauthenticated);
    assert!(r.headers.contains_key("penroam-request-id"));
    assert_eq!(
        call(base, "GET", "/v1/usage", Some("bogus"), &[], b"")
            .unwrap()
            .status,
        401
    );
    let u: UsageResponse = call(base, "GET", "/v1/usage", Some(&dev), &[], b"")
        .unwrap()
        .json();
    assert_eq!(u.op_count, 0);
    assert_eq!(
        call(base, "GET", "/v1/nope", Some(&dev), &[], b"")
            .unwrap()
            .status,
        404
    );
    // device session for an unregistered device
    let r = json_call(
        base,
        "POST",
        "/_test/session",
        None,
        &serde_json::json!({"account": "acct-1", "device_id": "dev-zz"}),
    )
    .unwrap();
    assert_eq!(r.status, 403);
    // account tokens cannot push
    let acct = session(base, "acct-1", None);
    let body = encode_json_batch(&[op(1)], &Limits::DEFAULT).unwrap();
    assert_eq!(
        call(base, "POST", "/v1/ops/batch", Some(&acct), &[], &body)
            .unwrap()
            .status,
        403
    );
}

#[test]
fn push_and_framed_pull() {
    let (h, _s, dev) = setup();
    let base = &h.base_url;
    let ops = [op(1), op(2), op(3)];
    let body = encode_json_batch(&ops, &Limits::DEFAULT).unwrap();
    let r = call(
        base,
        "POST",
        "/v1/ops/batch",
        Some(&dev),
        &[("Content-Type", "application/vnd.penroam.batch.v1")],
        &body,
    )
    .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.json::<PushOpsResponse>().acks.len(), 3);

    let r = call(base, "GET", "/v1/ops?limit=2", Some(&dev), &[], b"").unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(
        r.headers["content-type"],
        "application/vnd.penroam.batch.v1"
    );
    assert_eq!(r.headers["penroam-has-more"], "true");
    let items: Vec<(PulledOp, Vec<u8>)> = decode_json_batch(&r.body, &Limits::DEFAULT).unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].1, ops[1].1);
    let cursor = r.headers["penroam-next-cursor"].clone();
    let r = call(
        base,
        "GET",
        &format!("/v1/ops?cursor={cursor}&limit=10"),
        Some(&dev),
        &[],
        b"",
    )
    .unwrap();
    assert_eq!(r.headers["penroam-has-more"], "false");
    let items: Vec<(PulledOp, Vec<u8>)> = decode_json_batch(&r.body, &Limits::DEFAULT).unwrap();
    assert_eq!(items[0].0.server_seq, 3);
    // JSON control document on request
    let r = call(
        base,
        "GET",
        "/v1/ops?limit=1",
        Some(&dev),
        &[("Accept", "application/json")],
        b"",
    )
    .unwrap();
    let v: serde_json::Value = r.json();
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    // ack
    let r = json_call(
        base,
        "POST",
        "/v1/acks",
        Some(&dev),
        &serde_json::json!({"applied_cursor": cursor}),
    )
    .unwrap();
    assert_eq!(r.status, 204);
    // malformed cursor
    let r = call(base, "GET", "/v1/ops?cursor=a=b", Some(&dev), &[], b"").unwrap();
    assert_eq!(r.error().code, ErrorCode::InvalidCursor);
}

#[test]
fn keylog_and_blobs_over_http() {
    let (h, _s, dev) = setup();
    let base = &h.base_url;
    let req = KeyLogAppendRequest {
        index: 0,
        data: base64url::encode(b"entry-0"),
    };
    assert_eq!(
        json_call(base, "POST", "/v1/keylog", Some(&dev), &req)
            .unwrap()
            .status,
        200
    );
    let r = json_call(base, "POST", "/v1/keylog", Some(&dev), &req).unwrap();
    assert_eq!((r.status, r.error().code), (409, ErrorCode::Conflict));
    let page: KeyLogPage = call(base, "GET", "/v1/keylog?from=0", Some(&dev), &[], b"")
        .unwrap()
        .json();
    assert_eq!(page.next_index, 1);

    let bytes = vec![0xabu8; 300_000];
    let sha = sha256_hex(&bytes);
    let r = call(
        base,
        "PUT",
        "/v1/blobs/blob-1",
        Some(&dev),
        &[("Penroam-Content-Sha256", &sha)],
        &bytes,
    )
    .unwrap();
    assert_eq!(r.status, 200);
    assert!(!r.json::<BlobPutResponse>().duplicate);
    let r = call(base, "GET", "/v1/blobs/blob-1", Some(&dev), &[], b"").unwrap();
    assert_eq!(r.headers["content-type"], "application/octet-stream");
    assert_eq!(r.body, bytes);
    let r = call(base, "PUT", "/v1/blobs/blob-2", Some(&dev), &[], &bytes).unwrap();
    assert_eq!(r.error().code, ErrorCode::ValidationFailed);
    let r = call(
        base,
        "PUT",
        "/v1/blobs/blob-2",
        Some(&dev),
        &[("Penroam-Content-Sha256", &sha256_hex(b"x"))],
        &bytes,
    )
    .unwrap();
    assert_eq!(r.error().code, ErrorCode::IntegrityMismatch);
    assert_eq!(
        call(base, "GET", "/v1/blobs/missing", Some(&dev), &[], b"")
            .unwrap()
            .status,
        404
    );
}

#[test]
fn fault_injection_over_http() {
    let (h, _s, dev) = setup();
    let base = &h.base_url;
    let r = json_call(
        base,
        "POST",
        "/_test/fail-next",
        None,
        &serde_json::json!({"n": 1, "code": "service_unavailable"}),
    )
    .unwrap();
    assert_eq!(r.status, 204);
    let r = call(base, "GET", "/v1/usage", Some(&dev), &[], b"").unwrap();
    assert_eq!(r.status, 503);
    assert_eq!(r.headers["retry-after"], "1");
    assert_eq!(r.error().code, ErrorCode::ServiceUnavailable);
    assert_eq!(r.headers["penroam-request-id"], r.error().request_id);

    // response lost after commit: connection closed, op persisted
    json_call(
        base,
        "POST",
        "/_test/drop-next",
        None,
        &serde_json::json!({"n": 1}),
    )
    .unwrap();
    let body = encode_json_batch(&[op(1)], &Limits::DEFAULT).unwrap();
    assert!(call(base, "POST", "/v1/ops/batch", Some(&dev), &[], &body).is_none());
    let r = call(base, "POST", "/v1/ops/batch", Some(&dev), &[], &body).unwrap();
    let acks = r.json::<PushOpsResponse>().acks;
    assert!(matches!(
        acks[0].result,
        penroam_sync_protocol::AckResult::Duplicate { server_seq: 1 }
    ));

    // delay
    json_call(
        base,
        "POST",
        "/_test/delay-next",
        None,
        &serde_json::json!({"n": 1, "ms": 300}),
    )
    .unwrap();
    let t = std::time::Instant::now();
    call(base, "GET", "/v1/usage", Some(&dev), &[], b"").unwrap();
    assert!(t.elapsed() >= std::time::Duration::from_millis(300));

    // revoke through the control route
    json_call(
        base,
        "POST",
        "/_test/revoke",
        None,
        &serde_json::json!({"account": "acct-1", "device_id": "dev-a"}),
    )
    .unwrap();
    let r = call(
        base,
        "POST",
        "/v1/ops/batch",
        Some(&dev),
        &[],
        &encode_json_batch(&[op(2)], &Limits::DEFAULT).unwrap(),
    )
    .unwrap();
    assert_eq!((r.status, r.error().code), (403, ErrorCode::DeviceRevoked));

    // quota and cursor expiry controls are accepted
    assert_eq!(
        json_call(
            base,
            "POST",
            "/_test/quota",
            None,
            &serde_json::json!({"account": "acct-1", "quota_bytes": null})
        )
        .unwrap()
        .status,
        204
    );
    assert_eq!(
        json_call(
            base,
            "POST",
            "/_test/expire-cursors",
            None,
            &serde_json::json!({"account": "acct-1", "server_seq": 1})
        )
        .unwrap()
        .status,
        204
    );
}

#[test]
fn oversized_bodies_are_refused_before_reading() {
    let mut limits = Limits::DEFAULT;
    limits.max_blob_bytes = 1000;
    let server = Arc::new(InMemoryServer::with_secret([3; 32], "t", limits));
    server.register_device(&"acct-1".parse().unwrap(), &"dev-a".parse().unwrap());
    let h = http::spawn("127.0.0.1:0", server).unwrap();
    let dev = session(&h.base_url, "acct-1", Some("dev-a"));
    // announce a huge body but send none of it: the server must answer anyway
    let addr = h.base_url.strip_prefix("http://").unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "PUT /v1/blobs/b HTTP/1.1\r\nAuthorization: Bearer {dev}\r\nContent-Length: 999999999\r\n\r\n").unwrap();
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    assert!(raw.starts_with("HTTP/1.1 413"), "{raw}");
    let r = call(
        &h.base_url,
        "POST",
        "/v1/keylog",
        Some(&dev),
        &[("Transfer-Encoding", "chunked")],
        b"",
    )
    .unwrap();
    assert_eq!(r.status, 411);
    h.shutdown();
}
