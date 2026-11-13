//! `GET /v1/live` over a real WebSocket against the HTTP test server: upgrade refusals (no or bad
//! token, account session, non-member, removed member, disallowed Origin, plain GET), hello and
//! welcome, commit hints after an HTTP push, verified awareness relay, revocation mid-session,
//! oversized and binary frames, and shutdown.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::live::{
    Awareness, ByeCode, ClientFrame, Hello, ServerFrame, DEFAULT_MAX_FRAME_BYTES,
};
use penroam_sync_protocol::sharing::{CreateInvitationRequest, CreateSpaceRequest, Role};
use penroam_sync_protocol::{
    routes, AccountId, DeviceId, DeviceRegistrationRequest, Limits, ProtocolVersion,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{http, Caller, InMemoryServer};
use tungstenite::client::IntoClientRequest;
use tungstenite::{Message, WebSocket};

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn dev(s: &str) -> DeviceId {
    s.parse().unwrap()
}

/// `POST` a JSON body to the test server; returns the status and the body.
fn post(base: &str, path: &str, body: serde_json::Value) -> (u16, Vec<u8>) {
    let addr = base.strip_prefix("http://").unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    let body = serde_json::to_vec(&body).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n",
        body.len()
    )
    .unwrap();
    s.write_all(&body).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let status = String::from_utf8_lossy(&raw[..split])
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, raw[split + 4..].to_vec())
}

fn token(base: &str, v: serde_json::Value) -> String {
    let (status, body) = post(base, "/_test/session", v);
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    v["token"].as_str().unwrap().to_string()
}

struct Env {
    _h: http::HttpHandle,
    s: Arc<InMemoryServer>,
    base: String,
    space: AccountId,
}

/// A space with owner `acct-owner`, editor `acct-editor` and viewer `acct-viewer`, devices
/// `dev-owner` / `dev-editor` / `dev-viewer`.
fn env() -> Env {
    let s = Arc::new(InMemoryServer::new());
    let h = http::spawn("127.0.0.1:0", s.clone()).unwrap();
    let base = h.base_url.clone();
    let [owner, editor, viewer] = ["owner", "editor", "viewer"].map(|n| acct(&format!("acct-{n}")));
    for (a, n) in [(&owner, "owner"), (&editor, "editor"), (&viewer, "viewer")] {
        s.set_verified_email(a, Some(&format!("{n}@example.org")))
            .unwrap();
    }
    let space = s
        .shared_create(
            &owner,
            &CreateSpaceRequest {
                idempotency_key: "k".into(),
                sealed_meta: "AAAA".into(),
            },
        )
        .unwrap()
        .space_id;
    for (who, n, role) in [
        (&editor, "editor", Role::Editor),
        (&viewer, "viewer", Role::Viewer),
    ] {
        let inv = s
            .shared_invite(
                &owner,
                &space,
                &CreateInvitationRequest {
                    email: format!("{n}@example.org"),
                    role,
                    idempotency_key: n.into(),
                },
            )
            .unwrap();
        s.invitation_accept(who, &inv.invitation_id).unwrap();
    }
    for (who, n) in [(&owner, "owner"), (&editor, "editor"), (&viewer, "viewer")] {
        s.register_device_keys(
            Caller::member(&space, who),
            &DeviceRegistrationRequest {
                device_id: Some(dev(&format!("dev-{n}"))),
                signing_public_key: base64url::encode(&[1; 32]),
                encryption_public_key: base64url::encode(&[2; 32]),
                label: None,
            },
        )
        .unwrap();
    }
    Env {
        _h: h,
        s,
        base,
        space,
    }
}

type Ws = WebSocket<TcpStream>;

impl Env {
    fn device_token(&self, d: &str) -> String {
        token(
            &self.base,
            serde_json::json!({"account": self.space, "device_id": d}),
        )
    }

    /// Opens `/v1/live` with optional token and extra headers; the HTTP status on refusal.
    fn open(&self, token: Option<&str>, extra: &[(&'static str, &str)]) -> Result<Ws, u16> {
        let addr = self.base.strip_prefix("http://").unwrap();
        let mut req = format!("ws://{addr}{}", routes::LIVE)
            .into_client_request()
            .unwrap();
        if let Some(t) = token {
            req.headers_mut()
                .insert("Authorization", format!("Bearer {t}").parse().unwrap());
        }
        for (k, v) in extra {
            req.headers_mut().insert(*k, v.parse().unwrap());
        }
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        match tungstenite::client::client(req, stream) {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r))) => {
                Err(r.status().as_u16())
            }
            Err(e) => panic!("handshake: {e}"),
        }
    }

    /// Opens, says hello for `d` and returns the socket after the welcome.
    fn live(&self, d: &str) -> (Ws, penroam_sync_protocol::live::Welcome) {
        let mut ws = self.open(Some(&self.device_token(d)), &[]).unwrap();
        send(&mut ws, &hello(d));
        match recv(&mut ws) {
            ServerFrame::Welcome(w) => (ws, w),
            other => panic!("expected welcome, got {other:?}"),
        }
    }

    fn push(&self, d: &str, n: u64) {
        let ct = fake_ciphertext(&format!("{d}-{n}"), 40);
        let h = header_for(&format!("op-{d}-{n}"), d, n, &[], &ct);
        let body = encode_json_batch(&[(h, ct)], &Limits::DEFAULT).unwrap();
        self.s.push(&self.space, &dev(d), &body).unwrap();
    }
}

fn hello(d: &str) -> ClientFrame {
    ClientFrame::Hello(Hello {
        protocol: ProtocolVersion::CURRENT,
        device_id: dev(d),
        last_server_seq: 0,
        presence: true,
    })
}

fn send(ws: &mut Ws, f: &ClientFrame) {
    ws.send(Message::text(f.to_text())).unwrap();
}

/// The next text frame (skipping WebSocket-level ping/pong).
fn recv(ws: &mut Ws) -> ServerFrame {
    loop {
        match ws.read().expect("a frame") {
            Message::Text(t) => {
                return ServerFrame::parse(t.as_str(), DEFAULT_MAX_FRAME_BYTES).unwrap()
            }
            Message::Close(_) => panic!("closed without a frame"),
            _ => {}
        }
    }
}

fn expect_bye(ws: &mut Ws, code: ByeCode) {
    loop {
        match recv(ws) {
            ServerFrame::Bye(b) => {
                assert_eq!(b.code, code);
                break;
            }
            ServerFrame::Membership(_) | ServerFrame::Committed(_) => {}
            other => panic!("expected bye {code:?}, got {other:?}"),
        }
    }
    // then the server closes
    loop {
        match ws.read() {
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

#[test]
fn upgrades_are_refused_without_a_member_device_session() {
    let e = env();
    assert_eq!(e.open(None, &[]).err(), Some(401));
    assert_eq!(e.open(Some("bogus"), &[]).err(), Some(401));
    // An account session (no device), on the space or on the member's own account.
    let space_account = token(
        &e.base,
        serde_json::json!({"account": e.space, "member": "acct-editor"}),
    );
    assert_eq!(e.open(Some(&space_account), &[]).err(), Some(403));
    e.s.register_device(&acct("acct-editor"), &dev("dev-own"));
    let own = token(
        &e.base,
        serde_json::json!({"account": "acct-editor", "device_id": "dev-own"}),
    );
    assert_eq!(e.open(Some(&own), &[]).err(), Some(403));
    // A browser Origin that isn't allow-listed.
    let viewer = e.device_token("dev-viewer");
    assert_eq!(
        e.open(Some(&viewer), &[("Origin", "https://evil.example")])
            .err(),
        Some(403)
    );
    e.s.set_live_allowed_origins(vec!["https://app.example".into()]);
    assert!(e
        .open(Some(&viewer), &[("Origin", "https://app.example")])
        .is_ok());
    // A removed member.
    e.s.shared_remove_member(&acct("acct-owner"), &e.space, &acct("acct-viewer"))
        .unwrap();
    assert_eq!(e.open(Some(&viewer), &[]).err(), Some(403));
    // A plain GET without the upgrade headers.
    let addr = e.base.strip_prefix("http://").unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "GET /v1/live HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {viewer}\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    let _ = s.read_to_string(&mut raw);
    assert!(raw.starts_with("HTTP/1.1 400"), "{raw}");
}

#[test]
fn hints_and_verified_awareness_over_websocket() {
    let e = env();
    let (mut owner, w) = e.live("dev-owner");
    assert_eq!(w.role, Role::Owner);
    let (mut viewer, vw) = e.live("dev-viewer");
    assert_eq!(vw.role, Role::Viewer);
    // A commit over the normal path becomes a hint on every socket.
    e.push("dev-editor", 1);
    for ws in [&mut owner, &mut viewer] {
        assert!(matches!(recv(ws), ServerFrame::Committed(c) if c.server_seq == 1));
    }
    // Forged identity in awareness is replaced by the verified one.
    send(
        &mut viewer,
        &ClientFrame::Awareness(Awareness {
            session_id: Some(w.session_id.clone()),
            member: Some(acct("acct-owner")),
            device_id: Some(dev("dev-owner")),
            payload: "AAAA".into(),
            ttl_ms: 5_000,
        }),
    );
    match recv(&mut owner) {
        ServerFrame::Awareness(a) => {
            assert_eq!(a.session_id.as_ref(), Some(&vw.session_id));
            assert_eq!(a.member.as_ref(), Some(&acct("acct-viewer")));
            assert_eq!(a.device_id.as_ref(), Some(&dev("dev-viewer")));
        }
        other => panic!("{other:?}"),
    }
    // Heartbeat.
    send(&mut owner, &ClientFrame::Ping);
    assert_eq!(recv(&mut owner), ServerFrame::Pong);
    // Revocation mid-session: the viewer's socket gets the bye and closes.
    e.s.shared_remove_member(&acct("acct-owner"), &e.space, &acct("acct-viewer"))
        .unwrap();
    expect_bye(&mut viewer, ByeCode::AccessRevoked);
    // The owner hears about it.
    assert!(matches!(recv(&mut owner), ServerFrame::Membership(m)
        if m.account_id.as_str() == "acct-viewer"));
}

#[test]
fn bad_hellos_and_frames_get_a_bye() {
    let e = env();
    // An old protocol in the hello.
    let mut ws = e.open(Some(&e.device_token("dev-editor")), &[]).unwrap();
    send(
        &mut ws,
        &ClientFrame::Hello(Hello {
            protocol: ProtocolVersion::new(1, 7),
            device_id: dev("dev-editor"),
            last_server_seq: 0,
            presence: true,
        }),
    );
    expect_bye(&mut ws, ByeCode::UpgradeRequired);
    // A hello for another device.
    let mut ws = e.open(Some(&e.device_token("dev-editor")), &[]).unwrap();
    send(&mut ws, &hello("dev-owner"));
    expect_bye(&mut ws, ByeCode::Unauthenticated);
    // Something other than hello first.
    let mut ws = e.open(Some(&e.device_token("dev-editor")), &[]).unwrap();
    send(&mut ws, &ClientFrame::Ping);
    expect_bye(&mut ws, ByeCode::InvalidFrame);
    // An oversized frame.
    let (mut ws, _) = e.live("dev-editor");
    ws.send(Message::text(
        "x".repeat(DEFAULT_MAX_FRAME_BYTES as usize + 1),
    ))
    .unwrap();
    expect_bye(&mut ws, ByeCode::InvalidFrame);
    // A binary frame.
    let (mut ws, _) = e.live("dev-editor");
    ws.send(Message::binary(vec![1, 2, 3])).unwrap();
    expect_bye(&mut ws, ByeCode::InvalidFrame);
}

#[test]
fn shutdown_says_server_restart() {
    let e = env();
    let (mut ws, _) = e.live("dev-viewer");
    e.s.live_shutdown(2_000);
    expect_bye(&mut ws, ByeCode::ServerRestart);
}
