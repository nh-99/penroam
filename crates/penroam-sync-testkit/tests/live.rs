//! The live hub of the development test server, in process: who may connect, commit hints
//! (coalescing and fault knobs), awareness relay with verified identity and space scoping,
//! membership changes and revocation, rate limits, slow consumers, oversized frames, idle
//! timeouts, presence expiry and shutdown.

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::live::{
    Awareness, Bye, ByeCode, ClientFrame, Committed, Hello, ServerFrame, DEFAULT_HEARTBEAT_MS,
};
use penroam_sync_protocol::sharing::{
    ChangeRoleRequest, CreateInvitationRequest, CreateSpaceRequest, MembershipState, Role,
};
use penroam_sync_protocol::{
    AccountId, DeviceId, DeviceRegistrationRequest, EnvelopeHeader, ErrorCode, Limits,
    LiveSessionId, ProtocolVersion,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{Caller, InMemoryServer, LiveConfig, LiveRefused};

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn dev(s: &str) -> DeviceId {
    s.parse().unwrap()
}

const T0: u64 = 1_800_000_000_000;

struct World {
    s: InMemoryServer,
    space: AccountId,
    owner: AccountId,
    editor: AccountId,
    viewer: AccountId,
    revoked: AccountId,
    outsider: AccountId,
}

fn reg(id: &str) -> DeviceRegistrationRequest {
    DeviceRegistrationRequest {
        device_id: Some(dev(id)),
        signing_public_key: base64url::encode(&[7; 32]),
        encryption_public_key: base64url::encode(&[8; 32]),
        label: None,
    }
}

/// A space owned by `owner` with an editor, a viewer and a removed member, each with a device
/// (`dev-<name>`), and an outsider who owns a second space with device `dev-outsider`.
fn world() -> World {
    let s = InMemoryServer::new();
    s.set_clock_ms(Some(T0));
    let [owner, editor, viewer, revoked, outsider] =
        ["owner", "editor", "viewer", "revoked", "outsider"].map(|n| acct(&format!("acct-{n}")));
    for (a, n) in [
        (&owner, "owner"),
        (&editor, "editor"),
        (&viewer, "viewer"),
        (&revoked, "revoked"),
        (&outsider, "outsider"),
    ] {
        s.set_verified_email(a, Some(&format!("{n}@example.org")))
            .unwrap();
    }
    let create = |who: &AccountId, key: &str| {
        s.shared_create(
            who,
            &CreateSpaceRequest {
                idempotency_key: key.into(),
                sealed_meta: "AAAA".into(),
            },
        )
        .unwrap()
        .space_id
    };
    let space = create(&owner, "k");
    for (who, role) in [
        (&editor, Role::Editor),
        (&viewer, Role::Viewer),
        (&revoked, Role::Editor),
    ] {
        let email = format!("{}@example.org", &who.as_str()["acct-".len()..]);
        let inv = s
            .shared_invite(
                &owner,
                &space,
                &CreateInvitationRequest {
                    email,
                    role,
                    idempotency_key: who.as_str().into(),
                },
            )
            .unwrap();
        s.invitation_accept(who, &inv.invitation_id).unwrap();
    }
    for who in [&owner, &editor, &viewer, &revoked] {
        let d = format!("dev-{}", &who.as_str()["acct-".len()..]);
        s.register_device_keys(Caller::member(&space, who), &reg(&d))
            .unwrap();
    }
    s.shared_remove_member(&owner, &space, &revoked).unwrap();
    let other = create(&outsider, "other");
    s.register_device_keys(Caller::member(&other, &outsider), &reg("dev-outsider"))
        .unwrap();
    World {
        s,
        space,
        owner,
        editor,
        viewer,
        revoked,
        outsider,
    }
}

fn hello(d: &str) -> Hello {
    Hello {
        protocol: ProtocolVersion::CURRENT,
        device_id: dev(d),
        last_server_seq: 0,
        presence: true,
    }
}

impl World {
    fn connect(&self, who: &AccountId) -> LiveSessionId {
        let d = format!("dev-{}", &who.as_str()["acct-".len()..]);
        let w = self
            .s
            .live_connect(Caller::member(&self.space, who), &dev(&d), &hello(&d))
            .unwrap();
        w.session_id
    }

    fn push(&self, who: &AccountId, n: u64) -> bool {
        let d = format!("dev-{}", &who.as_str()["acct-".len()..]);
        let ct = fake_ciphertext(&format!("{d}-{n}"), 40);
        let h: EnvelopeHeader = header_for(&format!("op-{d}-{n}"), &d, n, &[], &ct);
        let body = encode_json_batch(&[(h, ct)], &Limits::DEFAULT).unwrap();
        self.s
            .push(Caller::member(&self.space, who), &dev(&d), &body)
            .is_ok()
    }
}

fn http_code(r: Result<penroam_sync_protocol::live::Welcome, LiveRefused>) -> ErrorCode {
    match r {
        Err(LiveRefused::Http(b)) => b.code,
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}

fn bye_code(r: Result<penroam_sync_protocol::live::Welcome, LiveRefused>) -> ByeCode {
    match r {
        Err(LiveRefused::Bye(b)) => b.code,
        other => panic!("expected a bye, got {other:?}"),
    }
}

fn awareness(payload: &str, ttl_ms: u32) -> ClientFrame {
    ClientFrame::Awareness(Awareness {
        session_id: None,
        member: None,
        device_id: None,
        payload: payload.into(),
        ttl_ms,
    })
}

fn last_bye(frames: &[ServerFrame]) -> Option<Bye> {
    match frames.last() {
        Some(ServerFrame::Bye(b)) => Some(*b),
        _ => None,
    }
}

// ------------------------------------------------------------------ connecting --

#[test]
fn who_may_connect() {
    let w = world();
    let s = &w.s;
    // Every active member, with their role.
    for (who, role) in [
        (&w.owner, Role::Owner),
        (&w.editor, Role::Editor),
        (&w.viewer, Role::Viewer),
    ] {
        let d = format!("dev-{}", &who.as_str()["acct-".len()..]);
        let welcome = s
            .live_connect(Caller::member(&w.space, who), &dev(&d), &hello(&d))
            .unwrap();
        assert_eq!(welcome.role, role);
        assert_eq!(welcome.heartbeat_ms, DEFAULT_HEARTBEAT_MS);
        welcome.validate().unwrap();
    }
    // A device session alone resolves the member.
    assert!(s
        .live_connect(&w.space, &dev("dev-editor"), &hello("dev-editor"))
        .is_ok());
    // Removed members, other accounts, unknown accounts, ordinary accounts.
    for caller in [Caller::from(&w.space), Caller::member(&w.space, &w.revoked)] {
        assert_eq!(
            http_code(s.live_connect(caller, &dev("dev-revoked"), &hello("dev-revoked"))),
            ErrorCode::AccessRevoked
        );
    }
    assert_eq!(
        http_code(s.live_connect(
            Caller::member(&w.space, &w.outsider),
            &dev("dev-editor"),
            &hello("dev-editor")
        )),
        ErrorCode::NotFound
    );
    assert_eq!(
        http_code(s.live_connect(
            &acct("acct-nobody"),
            &dev("dev-editor"),
            &hello("dev-editor")
        )),
        ErrorCode::Unauthenticated
    );
    s.register_device(&w.owner, &dev("dev-own"));
    assert_eq!(
        http_code(s.live_connect(&w.owner, &dev("dev-own"), &hello("dev-own"))),
        ErrorCode::Forbidden
    );
    // Someone else's device, or an unregistered one.
    assert_eq!(
        http_code(s.live_connect(
            Caller::member(&w.space, &w.viewer),
            &dev("dev-editor"),
            &hello("dev-editor")
        )),
        ErrorCode::Forbidden
    );
    assert_eq!(
        http_code(s.live_connect(
            Caller::member(&w.space, &w.viewer),
            &dev("dev-ghost"),
            &hello("dev-ghost")
        )),
        ErrorCode::Forbidden
    );
    // A hello for another device, or an older protocol.
    assert_eq!(
        bye_code(s.live_connect(&w.space, &dev("dev-viewer"), &hello("dev-owner"))),
        ByeCode::Unauthenticated
    );
    let mut old = hello("dev-viewer");
    old.protocol = ProtocolVersion::new(1, 7);
    assert_eq!(
        bye_code(s.live_connect(&w.space, &dev("dev-viewer"), &old)),
        ByeCode::UpgradeRequired
    );
    // Too many sessions per device.
    for _ in 1..s.live_config().sessions_per_device {
        s.live_connect(&w.space, &dev("dev-viewer"), &hello("dev-viewer"))
            .unwrap();
    }
    let r = s.live_connect(&w.space, &dev("dev-viewer"), &hello("dev-viewer"));
    assert_eq!(http_code(r), ErrorCode::RateLimited);
}

// ------------------------------------------------------------------------ hints --

#[test]
fn every_member_gets_coalesced_commit_hints() {
    let w = world();
    let s = &w.s;
    let ids: Vec<_> = [&w.owner, &w.editor, &w.viewer]
        .into_iter()
        .map(|m| w.connect(m))
        .collect();
    assert!(w.push(&w.editor, 1));
    assert!(w.push(&w.editor, 2));
    assert!(w.push(&w.owner, 1));
    for id in &ids {
        assert_eq!(
            s.live_recv(id),
            vec![ServerFrame::Committed(Committed { server_seq: 3 })]
        );
    }
    // A refused push (viewer) commits nothing and hints nothing.
    assert!(!w.push(&w.viewer, 1));
    for id in &ids {
        assert!(s.live_recv(id).is_empty());
    }
    // A push whose response was lost still committed, so it still hints.
    s.drop_next_responses(1);
    assert!(!w.push(&w.editor, 3));
    assert_eq!(
        s.live_recv(&ids[2]),
        vec![ServerFrame::Committed(Committed { server_seq: 4 })]
    );
    // Hints for one space never reach another space's sessions.
    let other = s.shared_list(&w.outsider).unwrap().spaces[0]
        .space_id
        .clone();
    let o = s
        .live_connect(&other, &dev("dev-outsider"), &hello("dev-outsider"))
        .unwrap()
        .session_id;
    assert!(w.push(&w.editor, 4));
    assert!(s.live_recv(&o).is_empty());
}

#[test]
fn hint_fault_knobs_drop_duplicate_and_hold() {
    let w = world();
    let s = &w.s;
    let id = w.connect(&w.viewer);
    s.live_drop_next_hints(1);
    assert!(w.push(&w.editor, 1));
    assert!(s.live_recv(&id).is_empty(), "dropped");
    // The log has it anyway; the next hint carries the latest seq.
    assert!(w.push(&w.editor, 2));
    assert_eq!(
        s.live_recv(&id),
        vec![ServerFrame::Committed(Committed { server_seq: 2 })]
    );
    s.live_duplicate_next_hints(1);
    assert!(w.push(&w.editor, 3));
    assert_eq!(
        s.live_recv(&id),
        vec![ServerFrame::Committed(Committed { server_seq: 3 }); 2]
    );
    s.live_hold_hints(true);
    assert!(w.push(&w.editor, 4));
    assert!(w.push(&w.editor, 5));
    assert!(s.live_recv(&id).is_empty(), "held");
    s.live_hold_hints(false);
    assert_eq!(
        s.live_recv(&id),
        vec![ServerFrame::Committed(Committed { server_seq: 5 })]
    );
}

// -------------------------------------------------------------------- awareness --

#[test]
fn awareness_is_relayed_with_the_verified_sender_within_the_space_only() {
    let w = world();
    let s = &w.s;
    let owner = w.connect(&w.owner);
    let editor = w.connect(&w.editor);
    let viewer = w.connect(&w.viewer);
    let other_space = s.shared_list(&w.outsider).unwrap().spaces[0]
        .space_id
        .clone();
    let outsider = s
        .live_connect(&other_space, &dev("dev-outsider"), &hello("dev-outsider"))
        .unwrap()
        .session_id;
    // The editor forges another session, member and device; the server overwrites all three.
    let forged = ClientFrame::Awareness(Awareness {
        session_id: Some(owner.clone()),
        member: Some(w.owner.clone()),
        device_id: Some(dev("dev-owner")),
        payload: "AAAA".into(),
        ttl_ms: 10_000,
    });
    s.live_send(&editor, &forged);
    for id in [&owner, &viewer] {
        let frames = s.live_recv(id);
        let [ServerFrame::Awareness(a)] = frames.as_slice() else {
            panic!("{frames:?}");
        };
        assert_eq!(a.session_id.as_ref(), Some(&editor));
        assert_eq!(a.member.as_ref(), Some(&w.editor));
        assert_eq!(a.device_id.as_ref(), Some(&dev("dev-editor")));
        assert_eq!(a.payload, "AAAA");
    }
    // Not echoed to the sender, never to another space.
    assert!(s.live_recv(&editor).is_empty());
    assert!(s.live_recv(&outsider).is_empty());
    // And the other space's awareness doesn't reach this one.
    s.live_send(&outsider, &awareness("BBBB", 5_000));
    for id in [&owner, &editor, &viewer] {
        assert!(s.live_recv(id).is_empty());
    }
    // Viewers share presence too.
    s.live_send(&viewer, &awareness("CCCC", 5_000));
    assert_eq!(s.live_recv(&owner).len(), 1);
    // TTLs are capped at two heartbeats.
    s.live_send(&viewer, &awareness("DDDD", 60_000));
    let frames = s.live_recv(&owner);
    let [ServerFrame::Awareness(a)] = frames.as_slice() else {
        panic!("{frames:?}");
    };
    assert_eq!(a.ttl_ms, 2 * DEFAULT_HEARTBEAT_MS);
}

#[test]
fn presence_off_sends_nothing_and_presence_expires() {
    let w = world();
    let s = &w.s;
    let owner = w.connect(&w.owner);
    let mut quiet = hello("dev-viewer");
    quiet.presence = false;
    let viewer = s
        .live_connect(&w.space, &dev("dev-viewer"), &quiet)
        .unwrap()
        .session_id;
    s.live_send(&viewer, &awareness("AAAA", 5_000));
    assert!(s.live_recv(&owner).is_empty());
    assert!(s.live_presence(&w.space).is_empty());
    // ... but it still gets hints.
    assert!(w.push(&w.editor, 1));
    assert_eq!(s.live_recv(&viewer).len(), 1);

    s.live_send(&owner, &awareness("AAAA", 5_000));
    let p = s.live_presence(&w.space);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].member, w.owner);
    assert_eq!(p[0].expires_at_ms, T0 + 5_000);
    s.advance_clock_ms(5_000);
    assert!(s.live_presence(&w.space).is_empty());
}

// ------------------------------------------------------------------- membership --

#[test]
fn removal_closes_the_members_sessions_and_others_hear_it() {
    let w = world();
    let s = &w.s;
    let owner = w.connect(&w.owner);
    let editor = w.connect(&w.editor);
    let viewer = w.connect(&w.viewer);
    assert!(w.push(&w.owner, 1));
    s.shared_remove_member(&w.owner, &w.space, &w.editor)
        .unwrap();
    // The removed member: queued hints are withheld; the membership frame, then the bye.
    let frames = s.live_recv(&editor);
    assert_eq!(frames.len(), 2, "{frames:?}");
    assert!(matches!(&frames[0], ServerFrame::Membership(m)
        if m.account_id == w.editor && m.state == MembershipState::Revoked));
    assert_eq!(last_bye(&frames).unwrap().code, ByeCode::AccessRevoked);
    assert!(!s.live_is_open(&editor));
    assert!(s.live_recv(&editor).is_empty());
    // Everyone else hears it.
    for id in [&owner, &viewer] {
        let frames = s.live_recv(id);
        assert!(frames.iter().any(|f| matches!(f, ServerFrame::Membership(m)
            if m.account_id == w.editor && m.state == MembershipState::Revoked)));
    }
    // Their later awareness and hints don't reach the removed member, and they can't reconnect.
    assert!(w.push(&w.owner, 2));
    assert_eq!(
        http_code(s.live_connect(&w.space, &dev("dev-editor"), &hello("dev-editor"))),
        ErrorCode::AccessRevoked
    );
}

#[test]
fn role_changes_and_device_removal_are_live() {
    let w = world();
    let s = &w.s;
    let owner = w.connect(&w.owner);
    let viewer = w.connect(&w.viewer);
    let version = s
        .shared_members(&w.owner, &w.space)
        .unwrap()
        .members
        .into_iter()
        .find(|m| m.account_id == w.viewer)
        .unwrap()
        .version;
    s.shared_change_role(
        &w.owner,
        &w.space,
        &w.viewer,
        &ChangeRoleRequest {
            role: Role::Editor,
            version,
        },
    )
    .unwrap();
    for id in [&owner, &viewer] {
        assert!(s
            .live_recv(id)
            .iter()
            .any(|f| matches!(f, ServerFrame::Membership(m)
            if m.account_id == w.viewer && m.role == Role::Editor
                && m.state == MembershipState::Active)));
    }
    assert!(s.live_is_open(&viewer));
    // The owner removes the viewer's device: that device's sessions end.
    s.revoke_device_as(&w.space, &dev("dev-owner"), &dev("dev-viewer"))
        .unwrap();
    assert_eq!(
        last_bye(&s.live_recv(&viewer)).unwrap().code,
        ByeCode::AccessRevoked
    );
    assert!(s.live_is_open(&owner));
}

// ---------------------------------------------------------------------- limits --

#[test]
fn awareness_is_rate_limited_per_session() {
    let w = world();
    let s = &w.s;
    s.set_live_config(LiveConfig {
        session_burst: 5,
        session_rate_per_s: 5,
        rate_strikes: 3,
        ..LiveConfig::default()
    });
    let owner = w.connect(&w.owner);
    let editor = w.connect(&w.editor);
    for _ in 0..7 {
        s.live_send(&editor, &awareness("AAAA", 5_000));
    }
    // The burst got through; the excess was dropped, and the session is still open.
    assert_eq!(s.live_recv(&owner).len(), 5);
    assert!(s.live_is_open(&editor));
    // The bucket refills on the clock.
    s.advance_clock_ms(400);
    s.live_send(&editor, &awareness("AAAA", 5_000));
    s.live_send(&editor, &awareness("AAAA", 5_000));
    assert_eq!(s.live_recv(&owner).len(), 2);
    // Sustained excess ends the session.
    for _ in 0..3 {
        s.live_send(&editor, &awareness("AAAA", 5_000));
    }
    let bye = last_bye(&s.live_recv(&editor)).unwrap();
    assert_eq!(bye.code, ByeCode::RateLimited);
    assert!(bye.retry_after_ms.is_some());
}

#[test]
fn slow_consumers_are_disconnected_and_hints_always_fit() {
    let w = world();
    let s = &w.s;
    s.set_live_config(LiveConfig {
        queue_frames: 4,
        ..LiveConfig::default()
    });
    let owner = w.connect(&w.owner);
    let editor = w.connect(&w.editor);
    let viewer = w.connect(&w.viewer);
    // The viewer never reads. Hints coalesce, so they take one slot however many commits.
    for n in 1..=10 {
        assert!(w.push(&w.owner, n));
    }
    for _ in 0..3 {
        s.live_send(&editor, &awareness("AAAA", 5_000));
    }
    assert!(s.live_is_open(&viewer));
    // The owner keeps up.
    assert_eq!(s.live_recv(&owner).len(), 4);
    // Now the queue is full: awareness to it is dropped, and too many drops end the session.
    for _ in 0..4 {
        s.live_send(&editor, &awareness("AAAA", 5_000));
    }
    let frames = s.live_recv(&viewer);
    assert_eq!(
        frames,
        vec![ServerFrame::Bye(Bye {
            code: ByeCode::SlowConsumer,
            retry_after_ms: None
        })]
    );
    // The owner reads and stays.
    assert_eq!(s.live_recv(&owner).len(), 4);
    assert!(s.live_is_open(&owner));

    // A full queue without a queued hint can't take the next hint: slow consumer.
    let viewer = w.connect(&w.viewer);
    for _ in 0..4 {
        s.live_send(&editor, &awareness("AAAA", 5_000));
    }
    assert!(w.push(&w.owner, 11));
    assert_eq!(
        last_bye(&s.live_recv(&viewer)).unwrap().code,
        ByeCode::SlowConsumer
    );
}

#[test]
fn bad_frames_end_the_session() {
    let w = world();
    let s = &w.s;
    s.set_live_config(LiveConfig {
        max_frame_bytes: 4096,
        ..LiveConfig::default()
    });
    let owner = w.connect(&w.owner);
    let cases = [
        format!(
            r#"{{"type":"awareness","payload":"{}","ttl_ms":5000}}"#,
            "A".repeat(5000)
        ),
        r#"{"type":"awareness","payload":"AAAA","ttl_ms":1}"#.to_string(),
        r#"{"type":"awareness","payload":"not base64","ttl_ms":5000}"#.to_string(),
        r#"{"type":"committed","server_seq":9}"#.to_string(),
        "not json".to_string(),
        hello("dev-editor").clone_frame(),
    ];
    for text in cases {
        let editor = w.connect(&w.editor);
        s.live_send_text(&editor, &text);
        assert_eq!(
            last_bye(&s.live_recv(&editor)).unwrap().code,
            ByeCode::InvalidFrame,
            "{text}"
        );
    }
    // Nothing reached the other member.
    assert!(s.live_recv(&owner).is_empty());
}

trait CloneFrame {
    fn clone_frame(&self) -> String;
}
impl CloneFrame for Hello {
    fn clone_frame(&self) -> String {
        ClientFrame::Hello(self.clone()).to_text()
    }
}

#[test]
fn heartbeats_idle_timeout_and_shutdown() {
    let w = world();
    let s = &w.s;
    let owner = w.connect(&w.owner);
    let editor = w.connect(&w.editor);
    s.live_send(&owner, &ClientFrame::Ping);
    assert_eq!(s.live_recv(&owner), vec![ServerFrame::Pong]);
    // The owner keeps pinging, the editor goes silent.
    let beat = u64::from(DEFAULT_HEARTBEAT_MS);
    for _ in 0..3 {
        s.advance_clock_ms(beat);
        s.live_send(&owner, &ClientFrame::Ping);
        s.live_tick();
    }
    assert_eq!(
        last_bye(&s.live_recv(&editor)).unwrap().code,
        ByeCode::IdleTimeout
    );
    assert!(s.live_is_open(&owner));
    s.live_shutdown(3_000);
    let frames = s.live_recv(&owner);
    assert_eq!(
        last_bye(&frames),
        Some(Bye {
            code: ByeCode::ServerRestart,
            retry_after_ms: Some(3_000)
        })
    );
}

#[test]
fn meta_advertises_live() {
    assert!(InMemoryServer::new()
        .meta()
        .has_capability(penroam_sync_protocol::dto::capabilities::LIVE_V1));
}
