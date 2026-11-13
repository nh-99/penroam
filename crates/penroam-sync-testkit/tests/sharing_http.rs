//! Shared notebooks over HTTP against the development test server: every sharing route, token
//! kinds (none, invalid, own account, space session, vault session, device session), the
//! membership and role checks on space sync routes, and enumeration resistance.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::sharing::{
    IncomingInvitationList, InvitationRecord, MemberList, MemberRecord, Role, SharedNotebookList,
    SharedNotebookRecord, INVITATION_TTL_MS,
};
use penroam_sync_protocol::{
    routes, AckResult, DeviceRegistrationRequest, ErrorBody, ErrorCode, Limits, PushOpsResponse,
    SessionTokens,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{http, InMemoryServer};

struct Resp {
    status: u16,
    body: Vec<u8>,
}

impl Resp {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
    fn code(&self) -> ErrorCode {
        let b: ErrorBody = self.json();
        assert_eq!(b.code.http_status(), self.status);
        b.code
    }
}

struct Client {
    base: String,
}

impl Client {
    fn call(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        ctype: &str,
        body: &[u8],
    ) -> Resp {
        let addr = self.base.strip_prefix("http://").unwrap();
        let mut s = TcpStream::connect(addr).unwrap();
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\n",
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
        let head = String::from_utf8(raw[..split].to_vec()).unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        Resp {
            status,
            body: raw[split + 4..].to_vec(),
        }
    }

    fn json(&self, method: &str, path: &str, token: Option<&str>, v: serde_json::Value) -> Resp {
        let body = if v.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&v).unwrap()
        };
        self.call(method, path, token, "application/json", &body)
    }

    fn session(&self, v: serde_json::Value) -> String {
        let r = self.json("POST", "/_test/session", None, v);
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        r.json::<serde_json::Value>()["token"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn register(&self, token: &str, device: &str) {
        let reg = DeviceRegistrationRequest {
            device_id: Some(device.parse().unwrap()),
            signing_public_key: base64url::encode(&[1; 32]),
            encryption_public_key: base64url::encode(&[2; 32]),
            label: None,
        };
        let r = self.json(
            "POST",
            "/v1/devices",
            Some(token),
            serde_json::to_value(&reg).unwrap(),
        );
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    }

    fn push(&self, token: &str, device: &str, n: u64) -> Resp {
        let ct = fake_ciphertext(&format!("{device}-{n}"), 40);
        let h = header_for(&format!("op-{device}-{n}"), device, n, &[], &ct);
        let body = encode_json_batch(&[(h, ct)], &Limits::DEFAULT).unwrap();
        self.call(
            "POST",
            "/v1/ops/batch",
            Some(token),
            routes::MEDIA_BATCH,
            &body,
        )
    }
}

fn path(template: &str, id: &str) -> String {
    template.replace("{id}", id).replace("{inv}", id)
}

fn setup() -> (http::HttpHandle, Arc<InMemoryServer>, Client) {
    let server = Arc::new(InMemoryServer::new());
    let h = http::spawn("127.0.0.1:0", server.clone()).unwrap();
    let c = Client {
        base: h.base_url.clone(),
    };
    (h, server, c)
}

#[test]
fn sharing_routes_need_a_valid_token() {
    let (_h, _s, c) = setup();
    let id = "s-123";
    for (m, p) in [
        ("GET", routes::SHARED_NOTEBOOKS.to_string()),
        ("POST", routes::SHARED_NOTEBOOKS.to_string()),
        ("PUT", path(routes::SHARED_NOTEBOOK_META, id)),
        ("POST", path(routes::SHARED_NOTEBOOK_SESSION, id)),
        ("GET", path(routes::SHARED_NOTEBOOK_KEY, id)),
        ("PUT", path(routes::SHARED_NOTEBOOK_KEY, id)),
        ("GET", path(routes::SHARED_NOTEBOOK_MEMBERS, id)),
        ("POST", path(routes::SHARED_NOTEBOOK_INVITATIONS, id)),
        ("PATCH", format!("/v1/shared-notebooks/{id}/members/acct-x")),
        (
            "DELETE",
            format!("/v1/shared-notebooks/{id}/members/acct-x"),
        ),
        ("GET", routes::INVITATIONS.to_string()),
        ("POST", path(routes::INVITATION_ACCEPT, "inv-1")),
        ("POST", path(routes::INVITATION_DECLINE, "inv-1")),
        ("DELETE", path(routes::INVITATION, "inv-1")),
    ] {
        for token in [None, Some("not-a-real-token")] {
            let r = c.json(m, &p, token, serde_json::json!({}));
            assert_eq!(r.status, 401, "{m} {p}");
            assert_eq!(r.code(), ErrorCode::Unauthenticated);
        }
    }
}

#[test]
fn sharing_end_to_end_over_http() {
    let (_h, server, c) = setup();
    server.set_clock_ms(Some(1_800_000_000_000));
    let ada = c.session(serde_json::json!({"account": "acct-ada", "email": "Ada@Example.org"}));
    let bob = c.session(serde_json::json!({"account": "acct-bob", "email": "bob@example.org"}));
    let carol =
        c.session(serde_json::json!({"account": "acct-carol", "email": "carol@example.org"}));
    let eve = c.session(serde_json::json!({"account": "acct-eve", "email": "eve@example.org"}));
    // A malformed email in the test sign-in is refused.
    let r = c.json(
        "POST",
        "/_test/session",
        None,
        serde_json::json!({"account": "acct-x", "email": "nope"}),
    );
    assert_eq!(r.code(), ErrorCode::ValidationFailed);

    // Create (idempotent) and store the key once.
    let create = serde_json::json!({"idempotency_key": "k1", "sealed_meta": "AAAA"});
    let r = c.json("POST", routes::SHARED_NOTEBOOKS, Some(&ada), create.clone());
    let rec: SharedNotebookRecord = r.json();
    assert!(rec.owner);
    let again: SharedNotebookRecord = c
        .json("POST", routes::SHARED_NOTEBOOKS, Some(&ada), create)
        .json();
    assert_eq!(again.space_id, rec.space_id);
    let sp = rec.space_id.as_str().to_string();
    let key = serde_json::json!({"key": base64url::encode(&[5; 32])});
    let r = c.json(
        "PUT",
        &path(routes::SHARED_NOTEBOOK_KEY, &sp),
        Some(&ada),
        key.clone(),
    );
    assert_eq!(r.status, 204);
    let r = c.json(
        "PUT",
        &path(routes::SHARED_NOTEBOOK_KEY, &sp),
        Some(&ada),
        key,
    );
    assert_eq!(r.code(), ErrorCode::Conflict);

    // Invitations answer identically for an existing and a non-existent account.
    let invite = |email: &str, role: &str, k: &str| {
        c.json(
            "POST",
            &path(routes::SHARED_NOTEBOOK_INVITATIONS, &sp),
            Some(&ada),
            serde_json::json!({"email": email, "role": role, "idempotency_key": k}),
        )
    };
    let real = invite("bob@example.org", "editor", "i-bob");
    let ghost = invite("ghost@example.org", "editor", "i-ghost");
    assert_eq!((real.status, ghost.status), (200, 200));
    let keys = |r: &Resp| {
        r.json::<serde_json::Value>()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(keys(&real), keys(&ghost));
    let (real, ghost): (InvitationRecord, InvitationRecord) = (real.json(), ghost.json());
    assert_eq!(
        (real.state, real.role, real.expires_at_ms, real.version),
        (ghost.state, ghost.role, ghost.expires_at_ms, ghost.version)
    );
    assert_eq!(
        invite("x@example.org", "owner", "i-x").code(),
        ErrorCode::InvalidRole
    );
    let carol_inv: InvitationRecord = invite("carol@example.org", "viewer", "i-carol").json();
    // Only the owner invites.
    let r = c.json(
        "POST",
        &path(routes::SHARED_NOTEBOOK_INVITATIONS, &sp),
        Some(&eve),
        serde_json::json!({"email": "z@example.org", "role": "viewer", "idempotency_key": "z"}),
    );
    assert_eq!(r.code(), ErrorCode::NotFound);

    // Invitees see and accept their invitations.
    let incoming: IncomingInvitationList = c
        .json(
            "GET",
            routes::INVITATIONS,
            Some(&bob),
            serde_json::Value::Null,
        )
        .json();
    assert_eq!(incoming.invitations.len(), 1);
    assert_eq!(incoming.invitations[0].invited_by, "ada@example.org");
    let inv = incoming.invitations[0].invitation_id.as_str().to_string();
    let r = c.json(
        "POST",
        &path(routes::INVITATION_ACCEPT, &inv),
        Some(&eve),
        serde_json::json!({}),
    );
    assert_eq!(r.code(), ErrorCode::NotFound);
    let joined: SharedNotebookRecord = c
        .json(
            "POST",
            &path(routes::INVITATION_ACCEPT, &inv),
            Some(&bob),
            serde_json::json!({}),
        )
        .json();
    assert_eq!(joined.role, Role::Editor);
    let r = c.json(
        "POST",
        &path(routes::INVITATION_ACCEPT, carol_inv.invitation_id.as_str()),
        Some(&carol),
        serde_json::json!({}),
    );
    assert_eq!(r.status, 200);

    // Space sessions: members get one, others 404.
    let space_session = |token: &str| {
        c.json(
            "POST",
            &path(routes::SHARED_NOTEBOOK_SESSION, &sp),
            Some(token),
            serde_json::json!({}),
        )
    };
    assert_eq!(space_session(&eve).code(), ErrorCode::NotFound);
    let bob_space: SessionTokens = space_session(&bob).json();
    assert_eq!(bob_space.account_id.as_str(), sp);
    let carol_space: SessionTokens = space_session(&carol).json();
    let ada_space: SessionTokens = space_session(&ada).json();

    // A space or vault session cannot use the sharing routes or the account key.
    for t in [&bob_space.access_token, &ada_space.access_token] {
        let r = c.json(
            "GET",
            routes::SHARED_NOTEBOOKS,
            Some(t),
            serde_json::Value::Null,
        );
        assert_eq!(r.code(), ErrorCode::Forbidden);
        let r = c.json("GET", routes::ACCOUNT_KEY, Some(t), serde_json::Value::Null);
        assert_eq!(r.code(), ErrorCode::Forbidden);
    }
    let vault = c.json(
        "POST",
        routes::VAULTS,
        Some(&ada),
        serde_json::json!({"lock": {"kdf": "argon2id", "memory_kib": 65536, "iterations": 3,
            "parallelism": 1, "salt": "c2FsdA", "wrapped_root": "AAAA"}, "name": "AAAA"}),
    );
    let vault_id = vault.json::<serde_json::Value>()["vault_id"]
        .as_str()
        .unwrap()
        .to_string();
    let vault_token: SessionTokens = c
        .json(
            "POST",
            &format!("/v1/vaults/{vault_id}/session"),
            Some(&ada),
            serde_json::json!({}),
        )
        .json();
    let r = c.json(
        "GET",
        routes::INVITATIONS,
        Some(&vault_token.access_token),
        serde_json::Value::Null,
    );
    assert_eq!(r.code(), ErrorCode::Forbidden);

    // Devices registered through a space session belong to that member; device sessions act as
    // them.
    c.register(&ada_space.access_token, "dev-ada");
    c.register(&bob_space.access_token, "dev-bob");
    c.register(&carol_space.access_token, "dev-carol");
    let ada_dev = c.session(serde_json::json!({"account": sp, "device_id": "dev-ada"}));
    let bob_dev = c.session(serde_json::json!({"account": sp, "device_id": "dev-bob"}));
    let carol_dev = c.session(serde_json::json!({"account": sp, "device_id": "dev-carol"}));
    let r = c.push(&bob_dev, "dev-bob", 1);
    assert_eq!(r.status, 200);
    assert!(matches!(
        r.json::<PushOpsResponse>().acks[0].result,
        AckResult::Accepted { .. }
    ));
    assert_eq!(
        c.push(&carol_dev, "dev-carol", 1).code(),
        ErrorCode::Forbidden
    );
    // Forged device in the envelope.
    let r = c.push(&bob_dev, "dev-ada", 2);
    assert!(matches!(
        &r.json::<PushOpsResponse>().acks[0].result,
        AckResult::Rejected { error } if error.code == ErrorCode::Forbidden
    ));
    let r = c.push(&ada_dev, "dev-ada", 1);
    assert_eq!(r.status, 200);
    // Checkpoints in a space: owner only.
    let checkpoint = |by: &str| {
        serde_json::json!({"covers_cursor": "AAAA", "covers_server_seq": 2, "blob_id": "b-1",
            "blob_sha256": "0".repeat(64), "blob_length": 1, "key_epoch": 1,
            "created_by": by, "signature": "AAAA"})
    };
    for (t, d) in [(&bob_dev, "dev-bob"), (&carol_dev, "dev-carol")] {
        let before = server.state_fingerprint();
        let r = c.json("POST", routes::CHECKPOINTS, Some(t), checkpoint(d));
        assert_eq!(r.code(), ErrorCode::Forbidden, "{d}");
        assert_eq!(server.state_fingerprint(), before);
    }
    // the owner passes the role check (and then fails on the missing blob)
    let r = c.json(
        "POST",
        routes::CHECKPOINTS,
        Some(&ada_dev),
        checkpoint("dev-ada"),
    );
    assert_eq!(r.code(), ErrorCode::NotFound);
    let r = c.call(
        "GET",
        "/v1/ops?limit=10",
        Some(&carol_dev),
        "application/json",
        &[],
    );
    assert_eq!(r.status, 200);

    // Members list: owner sees invitations, others don't; non-members 404.
    let members = |t: &str| {
        c.json(
            "GET",
            &path(routes::SHARED_NOTEBOOK_MEMBERS, &sp),
            Some(t),
            serde_json::Value::Null,
        )
    };
    let owner_view: MemberList = members(&ada).json();
    assert_eq!(owner_view.members.len(), 3);
    assert!(!owner_view.invitations.is_empty());
    let bob_view: MemberList = members(&bob).json();
    assert!(bob_view.invitations.is_empty());
    assert_eq!(members(&eve).code(), ErrorCode::NotFound);

    let meta = |t: &str| {
        c.json(
            "PUT",
            &path(routes::SHARED_NOTEBOOK_META, &sp),
            Some(t),
            serde_json::json!({"sealed_meta": "BBBB"}),
        )
    };
    // Sealed meta is the owner's alone.
    for t in [&bob, &carol] {
        let before = server.state_fingerprint();
        assert_eq!(meta(t).code(), ErrorCode::Forbidden);
        assert_eq!(server.state_fingerprint(), before);
    }
    assert_eq!(meta(&ada).status, 204);

    // Role change with the member's version, then a stale retry.
    let carol_rec = owner_view
        .members
        .iter()
        .find(|m| m.account_id.as_str() == "acct-carol")
        .unwrap();
    let change = serde_json::json!({"role": "editor", "version": carol_rec.version});
    let p = format!("/v1/shared-notebooks/{sp}/members/acct-carol");
    let r = c.json("PATCH", &p, Some(&bob), change.clone());
    assert_eq!(r.code(), ErrorCode::Forbidden);
    let updated: MemberRecord = c.json("PATCH", &p, Some(&ada), change.clone()).json();
    assert_eq!(updated.role, Role::Editor);
    assert_eq!(
        c.json("PATCH", &p, Some(&ada), change).code(),
        ErrorCode::Conflict
    );
    assert_eq!(c.push(&carol_dev, "dev-carol", 1).status, 200);

    // Removal: every space request by the removed member is `access_revoked`.
    let p = format!("/v1/shared-notebooks/{sp}/members/acct-bob");
    assert_eq!(
        c.json("DELETE", &p, Some(&carol), serde_json::Value::Null)
            .code(),
        ErrorCode::Forbidden
    );
    assert_eq!(
        c.json("DELETE", &p, Some(&ada), serde_json::Value::Null)
            .status,
        204
    );
    assert_eq!(
        c.push(&bob_dev, "dev-bob", 3).code(),
        ErrorCode::AccessRevoked
    );
    for (m, route) in [
        ("GET", "/v1/keylog"),
        ("GET", "/v1/devices"),
        ("GET", "/v1/usage"),
    ] {
        let r = c.json(
            m,
            route,
            Some(&bob_space.access_token),
            serde_json::Value::Null,
        );
        assert_eq!(r.code(), ErrorCode::AccessRevoked, "{route}");
    }
    assert_eq!(space_session(&bob).code(), ErrorCode::AccessRevoked);
    let list: SharedNotebookList = c
        .json(
            "GET",
            routes::SHARED_NOTEBOOKS,
            Some(&bob),
            serde_json::Value::Null,
        )
        .json();
    assert_eq!(list.spaces.len(), 1);
    assert_eq!(
        list.spaces[0].state,
        penroam_sync_protocol::MembershipState::Revoked
    );
    // Carol leaves on her own.
    let p = format!("/v1/shared-notebooks/{sp}/members/acct-carol");
    assert_eq!(
        c.json("DELETE", &p, Some(&carol), serde_json::Value::Null)
            .status,
        204
    );

    // Decline, cancel, expiry on the test clock.
    let eve_inv: InvitationRecord = invite("eve@example.org", "viewer", "i-eve").json();
    let eve_id = eve_inv.invitation_id.as_str();
    let r = c.json(
        "POST",
        &path(routes::INVITATION_DECLINE, eve_id),
        Some(&eve),
        serde_json::json!({}),
    );
    assert_eq!(r.status, 204);
    let eve_inv: InvitationRecord = invite("eve@example.org", "viewer", "i-eve-2").json();
    let eve_id = eve_inv.invitation_id.as_str();
    assert_eq!(
        c.json(
            "DELETE",
            &path(routes::INVITATION, eve_id),
            Some(&eve),
            serde_json::Value::Null
        )
        .code(),
        ErrorCode::NotFound
    );
    assert_eq!(
        c.json(
            "DELETE",
            &path(routes::INVITATION, eve_id),
            Some(&ada),
            serde_json::Value::Null
        )
        .status,
        204
    );
    let eve_inv: InvitationRecord = invite("eve@example.org", "viewer", "i-eve-3").json();
    let r = c.json(
        "POST",
        "/_test/advance-clock",
        None,
        serde_json::json!({"ms": INVITATION_TTL_MS}),
    );
    assert_eq!(r.status, 204);
    let r = c.json(
        "POST",
        &path(routes::INVITATION_ACCEPT, eve_inv.invitation_id.as_str()),
        Some(&eve),
        serde_json::json!({}),
    );
    assert_eq!(r.status, 410);
    assert_eq!(r.code(), ErrorCode::InvitationExpired);
    let r = c.json(
        "POST",
        "/_test/clock",
        None,
        serde_json::json!({"now_ms": null}),
    );
    assert_eq!(r.status, 204);

    // Unknown sub-routes and malformed ids are plain 404s.
    for (m, p) in [
        ("GET", format!("/v1/shared-notebooks/{sp}/nope")),
        ("GET", "/v1/shared-notebooks/bad%20id/members".to_string()),
        ("POST", "/v1/invitations/bad.id/accept".to_string()),
        ("GET", format!("/v1/invitations/{}", eve_inv.invitation_id)),
    ] {
        let r = c.json(m, &p, Some(&ada), serde_json::json!({}));
        assert_eq!(r.code(), ErrorCode::NotFound, "{m} {p}");
    }
}

#[test]
fn reauth_window_and_invitation_probing_over_http() {
    let (_h, server, c) = setup();
    server.set_clock_ms(Some(1_800_000_000_000));
    let ada = c.session(serde_json::json!({"account": "acct-ada", "email": "ada@example.org"}));
    let bob = c.session(serde_json::json!({"account": "acct-bob", "email": "bob@example.org"}));
    let sp: SharedNotebookRecord = c
        .json(
            "POST",
            routes::SHARED_NOTEBOOKS,
            Some(&ada),
            serde_json::json!({"idempotency_key": "k", "sealed_meta": "AAAA"}),
        )
        .json();
    let inv: InvitationRecord = c
        .json(
            "POST",
            &path(routes::SHARED_NOTEBOOK_INVITATIONS, sp.space_id.as_str()),
            Some(&ada),
            serde_json::json!({"email": "bob@example.org", "role": "viewer", "idempotency_key": "i"}),
        )
        .json();
    let id = inv.invitation_id.as_str().to_string();

    // Bob may not cancel; his answer matches an unknown id exactly.
    let mut answers = Vec::new();
    for target in [id.as_str(), "inv-00000000000000000000000000000000"] {
        let r = c.json(
            "DELETE",
            &path(routes::INVITATION, target),
            Some(&bob),
            serde_json::Value::Null,
        );
        let b: ErrorBody = r.json();
        answers.push((r.status, b.code, b.message));
    }
    assert_eq!(answers[0], answers[1]);
    assert_eq!(answers[0].1, ErrorCode::NotFound);

    // Re-authentication window: a stale verification needs a new sign-in before accepting.
    let r = c.json(
        "POST",
        "/_test/reauth-window",
        None,
        serde_json::json!({"ms": 60_000}),
    );
    assert_eq!(r.status, 204);
    c.json(
        "POST",
        "/_test/advance-clock",
        None,
        serde_json::json!({"ms": 60_001}),
    );
    let accept = |t: &str| {
        c.json(
            "POST",
            &path(routes::INVITATION_ACCEPT, &id),
            Some(t),
            serde_json::json!({}),
        )
    };
    let r = accept(&bob);
    assert_eq!(r.status, 403);
    assert_eq!(r.code(), ErrorCode::ReauthRequired);
    let r = c.json(
        "GET",
        routes::INVITATIONS,
        Some(&bob),
        serde_json::Value::Null,
    );
    assert_eq!(r.status, 200);
    let stale: IncomingInvitationList = r.json();
    assert!(stale.invitations.is_empty() && stale.reauth_required);
    assert!(String::from_utf8_lossy(&r.body).contains("\"reauth_required\":true"));
    let r = c.json(
        "POST",
        &path(routes::INVITATION_DECLINE, &id),
        Some(&bob),
        serde_json::json!({}),
    );
    assert_eq!(r.code(), ErrorCode::ReauthRequired);
    let bob = c.session(serde_json::json!({"account": "acct-bob", "email": "bob@example.org"}));
    let r = c.json(
        "GET",
        routes::INVITATIONS,
        Some(&bob),
        serde_json::Value::Null,
    );
    let fresh: IncomingInvitationList = r.json();
    assert_eq!(fresh.invitations.len(), 1);
    assert!(!String::from_utf8_lossy(&r.body).contains("reauth_required"));
    assert_eq!(accept(&bob).status, 200);

    // Device cap per member in a space.
    let r = c.json(
        "POST",
        "/_test/device-cap",
        None,
        serde_json::json!({"n": 1}),
    );
    assert_eq!(r.status, 204);
    let space: SessionTokens = c
        .json(
            "POST",
            &path(routes::SHARED_NOTEBOOK_SESSION, sp.space_id.as_str()),
            Some(&bob),
            serde_json::json!({}),
        )
        .json();
    c.register(&space.access_token, "dev-bob-1");
    let reg = serde_json::json!({"device_id": "dev-bob-2",
        "signing_public_key": base64url::encode(&[1; 32]),
        "encryption_public_key": base64url::encode(&[2; 32])});
    let r = c.json("POST", "/v1/devices", Some(&space.access_token), reg);
    assert_eq!(r.code(), ErrorCode::Conflict);
}
