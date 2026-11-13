//! Attribution, the previous checkpoint and account erasure in the development test server: who
//! may read attribution, what it says on accounts and spaces for both streams, paging, erasure
//! tombstones, the two kept checkpoints, retained compaction, and the HTTP routes.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::comments::CommentPushItem;
use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::history::{is_erased_account, AttributionPage};
use penroam_sync_protocol::live::{ByeCode, Hello, ServerFrame};
use penroam_sync_protocol::sharing::{
    CreateInvitationRequest, CreateSpaceRequest, MembershipState, Role,
};
use penroam_sync_protocol::{
    AccountId, BlobId, CheckpointRecord, DeviceId, DeviceRegistrationRequest, ErrorCode, Limits,
    ProtocolVersion, PullOpsRequest,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{http, Caller, InMemoryServer, RequestError};

fn a(n: &str) -> AccountId {
    format!("acct-{n}").parse().unwrap()
}
fn d(n: &str) -> DeviceId {
    format!("dev-{n}").parse().unwrap()
}

fn code<T: std::fmt::Debug>(r: Result<T, RequestError>) -> ErrorCode {
    match r {
        Err(RequestError::Api(b)) => b.code,
        other => panic!("expected an API error, got {other:?}"),
    }
}

const T0: u64 = 1_800_000_000_000;
const NAMES: [&str; 6] = [
    "owner",
    "editor",
    "commenter",
    "viewer",
    "revoked",
    "outsider",
];

struct World {
    s: Arc<InMemoryServer>,
    space: AccountId,
    accounts: std::collections::BTreeMap<&'static str, AccountId>,
    seq: std::cell::Cell<u64>,
}

impl World {
    fn new() -> Self {
        let s = Arc::new(InMemoryServer::new());
        s.set_clock_ms(Some(T0));
        for n in NAMES {
            s.set_verified_email(&a(n), Some(&format!("{n}@example.org")))
                .unwrap();
        }
        let space = s
            .shared_create(
                &a("owner"),
                &CreateSpaceRequest {
                    idempotency_key: "k".into(),
                    sealed_meta: "AAAA".into(),
                },
            )
            .unwrap()
            .space_id;
        for (n, role) in [
            ("editor", Role::Editor),
            ("commenter", Role::Commenter),
            ("viewer", Role::Viewer),
            ("revoked", Role::Editor),
        ] {
            let inv = s
                .shared_invite(
                    &a("owner"),
                    &space,
                    &CreateInvitationRequest {
                        email: format!("{n}@example.org"),
                        role,
                        idempotency_key: n.into(),
                    },
                )
                .unwrap();
            s.invitation_accept(&a(n), &inv.invitation_id).unwrap();
        }
        let w = World {
            s,
            space,
            accounts: NAMES.into_iter().map(|n| (n, a(n))).collect(),
            seq: std::cell::Cell::new(0),
        };
        for n in ["owner", "editor", "commenter", "viewer", "revoked"] {
            w.s.register_device_keys(w.m(n), &reg(n)).unwrap();
        }
        w.s.shared_remove_member(&a("owner"), &w.space, &a("revoked"))
            .unwrap();
        w
    }

    fn m(&self, n: &str) -> Caller<'_> {
        Caller::member(&self.space, &self.accounts[n])
    }

    fn next(&self) -> u64 {
        let n = self.seq.get() + 1;
        self.seq.set(n);
        n
    }

    fn push(&self, n: &str) {
        let seq = self.next();
        let dev = format!("dev-{n}");
        let ct = fake_ciphertext(&format!("op-{n}-{seq}"), 40);
        let h = header_for(&format!("op-{n}-{seq}"), &dev, seq, &[], &ct);
        let body = encode_json_batch(&[(h, ct)], &Limits::DEFAULT).unwrap();
        self.s.push(self.m(n), &d(n), &body).unwrap();
    }

    fn comment(&self, n: &str) {
        let seq = self.next();
        let dev = format!("dev-{n}");
        let ct = fake_ciphertext(&format!("c-{n}-{seq}"), 40);
        let item = CommentPushItem {
            envelope: header_for(&format!("c-{n}-{seq}"), &dev, seq, &[], &ct),
            thread_id: "t-1".parse().unwrap(),
            kind: Default::default(),
            mentions: vec![],
        };
        let body = encode_json_batch(&[(item, ct)], &Limits::DEFAULT).unwrap();
        self.s.comment_push(self.m(n), &d(n), &body).unwrap();
    }

    /// A checkpoint by the owner covering the first `covers` ops.
    fn checkpoint(&self, covers: u32, label: &str) -> CheckpointRecord {
        let bytes = fake_ciphertext(label, 32);
        let sha = sha256_hex(&bytes);
        let blob: BlobId = format!("b-{label}").parse().unwrap();
        self.s
            .blob_put(self.m("owner"), &d("owner"), &blob, &sha, &bytes)
            .unwrap();
        let page = self
            .s
            .pull(
                self.m("owner"),
                &d("owner"),
                &PullOpsRequest {
                    cursor: None,
                    limit: covers,
                },
            )
            .unwrap();
        let rec = CheckpointRecord {
            covers_cursor: page.response.next_cursor,
            covers_server_seq: u64::from(covers),
            blob_id: blob,
            blob_sha256: sha,
            blob_length: 32,
            key_epoch: 1,
            created_by: d("owner"),
            signature: base64url::encode(&[0xA5; 64]),
        };
        self.s
            .checkpoint_put(self.m("owner"), &d("owner"), &rec)
            .unwrap();
        rec
    }
}

fn reg(n: &str) -> DeviceRegistrationRequest {
    DeviceRegistrationRequest {
        device_id: Some(d(n)),
        signing_public_key: base64url::encode(&[1; 32]),
        encryption_public_key: base64url::encode(&[2; 32]),
        label: None,
    }
}

fn authors(p: &AttributionPage) -> Vec<(u64, String)> {
    p.items
        .iter()
        .map(|e| (e.server_seq, e.account_id.to_string()))
        .collect()
}

#[test]
fn ordinary_accounts_attribute_ops_to_the_account() {
    let s = InMemoryServer::new();
    s.set_clock_ms(Some(T0));
    let acct = a("solo");
    s.register_device(&acct, &d("solo"));
    let ct = fake_ciphertext("x", 40);
    let h = header_for("op-1", "dev-solo", 1, &[], &ct);
    s.push(
        &acct,
        &d("solo"),
        &encode_json_batch(&[(h, ct)], &Limits::DEFAULT).unwrap(),
    )
    .unwrap();
    let p = s.attribution(&acct, &d("solo"), 1, 10).unwrap();
    assert_eq!(p.items.len(), 1);
    let e = &p.items[0];
    assert_eq!(
        (e.account_id.clone(), e.device_id.clone(), e.accepted_at_ms),
        (acct, d("solo"), T0)
    );
    assert_eq!(e.op_id.as_str(), "op-1");
    // Ordinary accounts have no comment stream.
    assert_eq!(
        code(s.comment_attribution(&a("solo"), &d("solo"), 1, 10)),
        ErrorCode::Forbidden
    );
}

#[test]
fn space_attribution_names_the_member_and_any_member_may_read_it() {
    let w = World::new();
    let s = &*w.s;
    w.push("owner");
    s.advance_clock_ms(1_000);
    w.push("editor");
    w.comment("commenter");
    w.comment("editor");
    for n in ["owner", "editor", "commenter", "viewer"] {
        let p = s.attribution(w.m(n), &d(n), 1, 10).unwrap();
        assert_eq!(
            authors(&p),
            vec![(1, "acct-owner".into()), (2, "acct-editor".into())],
            "{n}"
        );
        assert_eq!(p.items[1].accepted_at_ms, T0 + 1_000);
        assert_eq!(p.items[1].device_id, d("editor"));
        let c = s.comment_attribution(w.m(n), &d(n), 1, 10).unwrap();
        assert_eq!(
            authors(&c),
            vec![(1, "acct-commenter".into()), (2, "acct-editor".into())]
        );
    }
    // Removed members, other accounts and member-less sessions are refused.
    let before = s.state_fingerprint();
    assert_eq!(
        code(s.attribution(w.m("revoked"), &d("revoked"), 1, 10)),
        ErrorCode::AccessRevoked
    );
    assert_eq!(
        code(s.comment_attribution(w.m("revoked"), &d("revoked"), 1, 10)),
        ErrorCode::AccessRevoked
    );
    assert_eq!(
        code(s.attribution(w.m("outsider"), &d("viewer"), 1, 10)),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(s.comment_attribution(w.m("outsider"), &d("viewer"), 1, 10)),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(s.attribution(w.m("viewer"), &d("editor"), 1, 10)),
        ErrorCode::Forbidden,
        "someone else's device"
    );
    assert_eq!(s.state_fingerprint(), before);
}

#[test]
fn attribution_pages() {
    let w = World::new();
    let s = &*w.s;
    for _ in 0..5 {
        w.push("editor");
    }
    let p = s.attribution(w.m("viewer"), &d("viewer"), 0, 2).unwrap();
    assert_eq!(
        (
            p.items.len(),
            p.items[0].server_seq,
            p.next_from_seq,
            p.has_more
        ),
        (2, 1, 3, true)
    );
    let p = s.attribution(w.m("viewer"), &d("viewer"), 3, 2).unwrap();
    assert_eq!(
        (p.items[0].server_seq, p.next_from_seq, p.has_more),
        (3, 5, true)
    );
    let p = s.attribution(w.m("viewer"), &d("viewer"), 5, 100).unwrap();
    assert_eq!((p.items.len(), p.next_from_seq, p.has_more), (1, 6, false));
    let p = s.attribution(w.m("viewer"), &d("viewer"), 9, 100).unwrap();
    assert!(p.items.is_empty() && !p.has_more);
    assert_eq!(p.next_from_seq, 9);
    assert_eq!(
        code(s.attribution(w.m("viewer"), &d("viewer"), 1, 0)),
        ErrorCode::ValidationFailed
    );
}

#[test]
fn erasing_a_member_leaves_a_stable_tombstone() {
    let w = World::new();
    let s = &*w.s;
    w.push("editor");
    w.comment("editor");
    w.comment("commenter");
    let live = s
        .live_connect(
            w.m("editor"),
            &d("editor"),
            &Hello {
                protocol: ProtocolVersion::CURRENT,
                device_id: d("editor"),
                last_server_seq: 0,
                presence: true,
            },
        )
        .unwrap()
        .session_id;
    let tomb = s.erase_account(&a("editor")).unwrap();
    assert!(is_erased_account(&tomb));
    assert_eq!(tomb, s.erased_account_id(&a("editor")), "stable");
    assert_ne!(tomb, s.erased_account_id(&a("viewer")));
    let p = s.attribution(w.m("viewer"), &d("viewer"), 1, 10).unwrap();
    assert_eq!(p.items[0].account_id, tomb);
    let c = s
        .comment_attribution(w.m("viewer"), &d("viewer"), 1, 10)
        .unwrap();
    assert_eq!(c.items[0].account_id, tomb);
    assert_eq!(c.items[1].account_id, a("commenter"));
    // The member list shows the tombstone, removed; the device is revoked; the live session ends.
    let members = s.shared_members(&a("owner"), &w.space).unwrap().members;
    let m = members.iter().find(|m| m.account_id == tomb).unwrap();
    assert_eq!(m.state, MembershipState::Revoked);
    assert!(m.display.is_empty());
    assert!(!members.iter().any(|m| m.account_id == a("editor")));
    assert!(s.device_revoked(&w.space, &d("editor")));
    assert!(s
        .live_recv(&live)
        .iter()
        .any(|f| matches!(f, ServerFrame::Bye(b) if b.code == ByeCode::AccessRevoked)));
    // The account itself is gone.
    assert_eq!(
        code(s.shared_list(&a("editor"))),
        ErrorCode::Unauthenticated
    );
    assert_eq!(code(s.erase_account(&a("editor"))), ErrorCode::NotFound);
}

#[test]
fn erasing_the_owner_erases_their_spaces() {
    let w = World::new();
    let s = &*w.s;
    w.push("editor");
    s.erase_account(&a("owner")).unwrap();
    assert!(s.shared_list(&a("editor")).unwrap().spaces.is_empty());
    assert_eq!(
        code(s.attribution(w.m("editor"), &d("editor"), 1, 10)),
        ErrorCode::Unauthenticated
    );
}

#[test]
fn the_server_keeps_two_checkpoints_and_compacts_only_to_the_older() {
    let w = World::new();
    let s = &*w.s;
    for _ in 0..6 {
        w.push("editor");
    }
    assert_eq!(
        code(s.checkpoint_previous(w.m("viewer"), &d("viewer"))),
        ErrorCode::NotFound
    );
    let c2 = w.checkpoint(2, "c2");
    assert_eq!(
        code(s.checkpoint_previous(w.m("viewer"), &d("viewer"))),
        ErrorCode::NotFound
    );
    let c4 = w.checkpoint(4, "c4");
    assert_eq!(
        s.checkpoint_previous(w.m("viewer"), &d("viewer")).unwrap(),
        c2
    );
    assert_eq!(
        s.checkpoint_latest(w.m("viewer"), &d("viewer")).unwrap(),
        c4
    );
    // Re-publishing at the same position replaces the latest only.
    let c4b = w.checkpoint(4, "c4b");
    assert_eq!(
        s.checkpoint_previous(w.m("viewer"), &d("viewer")).unwrap(),
        c2
    );
    assert_eq!(
        s.checkpoint_latest(w.m("viewer"), &d("viewer")).unwrap(),
        c4b
    );
    let c6 = w.checkpoint(6, "c6");
    assert_eq!(
        s.checkpoint_previous(w.m("viewer"), &d("viewer")).unwrap(),
        c4b
    );
    assert_eq!(
        s.checkpoint_latest(w.m("viewer"), &d("viewer")).unwrap(),
        c6
    );
    // Retained compaction stops at the previous checkpoint: its cursor and tail still work.
    assert_eq!(s.compact_to_retained_checkpoints(&w.space), 4);
    let tail = s
        .pull(
            w.m("viewer"),
            &d("viewer"),
            &PullOpsRequest {
                cursor: Some(c4b.covers_cursor.clone()),
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(tail.response.items.len(), 2);
    assert_eq!(
        code(s.pull(
            w.m("viewer"),
            &d("viewer"),
            &PullOpsRequest {
                cursor: Some(c2.covers_cursor),
                limit: 10,
            },
        )),
        ErrorCode::CursorExpired
    );
    // Membership rules apply.
    assert_eq!(
        code(s.checkpoint_previous(w.m("revoked"), &d("revoked"))),
        ErrorCode::AccessRevoked
    );
    assert_eq!(
        code(s.checkpoint_previous(w.m("outsider"), &d("viewer"))),
        ErrorCode::NotFound
    );
}

fn call(base: &str, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> (u16, Vec<u8>) {
    let addr = base.strip_prefix("http://").unwrap();
    let mut s = TcpStream::connect(addr).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(t) = token {
        req.push_str(&format!("Authorization: Bearer {t}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
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

#[test]
fn history_routes_over_http() {
    let w = World::new();
    w.push("editor");
    w.comment("commenter");
    let h = http::spawn("127.0.0.1:0", w.s.clone()).unwrap();
    let base = h.base_url.clone();
    for p in [
        "/v1/attribution",
        "/v1/comments/attribution",
        "/v1/checkpoints/previous",
    ] {
        assert_eq!(call(&base, "GET", p, None, b"").0, 401, "{p}");
    }
    let session = |v: serde_json::Value| {
        let (status, body) = call(
            &base,
            "POST",
            "/_test/session",
            None,
            &serde_json::to_vec(&v).unwrap(),
        );
        assert_eq!(status, 200);
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let viewer = session(serde_json::json!({"account": w.space, "device_id": "dev-viewer"}));
    let (status, body) = call(
        &base,
        "GET",
        "/v1/attribution?from_seq=1&limit=5",
        Some(&viewer),
        b"",
    );
    assert_eq!(status, 200);
    let p: AttributionPage = serde_json::from_slice(&body).unwrap();
    assert_eq!(p.items[0].account_id, a("editor"));
    let (status, body) = call(&base, "GET", "/v1/comments/attribution", Some(&viewer), b"");
    assert_eq!(status, 200);
    let p: AttributionPage = serde_json::from_slice(&body).unwrap();
    assert_eq!(p.items[0].account_id, a("commenter"));
    assert_eq!(
        call(&base, "GET", "/v1/checkpoints/previous", Some(&viewer), b"").0,
        404
    );
    assert_eq!(
        call(
            &base,
            "GET",
            "/v1/attribution?from_seq=x",
            Some(&viewer),
            b""
        )
        .0,
        400
    );
    // An account session (no device) is refused.
    let account = session(serde_json::json!({"account": w.space, "member": "acct-viewer"}));
    assert_eq!(
        call(&base, "GET", "/v1/attribution", Some(&account), b"").0,
        403
    );
    // Erasure over the control route.
    let (status, _) = call(
        &base,
        "POST",
        "/_test/erase-account",
        None,
        br#"{"account":"acct-editor"}"#,
    );
    assert_eq!(status, 204);
    let (_, body) = call(&base, "GET", "/v1/attribution", Some(&viewer), b"");
    let p: AttributionPage = serde_json::from_slice(&body).unwrap();
    assert!(is_erased_account(&p.items[0].account_id));
}
