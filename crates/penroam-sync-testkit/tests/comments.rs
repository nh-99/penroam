//! Comment streams and notifications in the development test server: the role matrix (including
//! the commenter row on document routes), mention validation, notification rules (mention, reply,
//! dedup, self, mutes), revocation, cursors, live hints, persistence and the HTTP routes.
//!
//! Refusals go through [`deny`], which also asserts that server state is unchanged.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::comments::{
    CommentPushItem, CommentPushKind, MutedThread, NotificationKind, NotificationPrefs,
    UpdateNotificationRequest,
};
use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::live::{Hello, ServerFrame};
use penroam_sync_protocol::sharing::{
    ChangeRoleRequest, CreateInvitationRequest, CreateSpaceRequest, Role, UpdateSpaceMetaRequest,
};
use penroam_sync_protocol::{
    AccountId, AckRequest, AckResult, CheckpointRecord, DeviceId, DeviceRegistrationRequest,
    ErrorCode, Limits, ProtocolVersion, PullOpsRequest, PushOpsResponse, ThreadId,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{http, Caller, InMemoryServer, RequestError};

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn dev(s: &str) -> DeviceId {
    s.parse().unwrap()
}
fn thread(s: &str) -> ThreadId {
    s.parse().unwrap()
}

fn code<T: std::fmt::Debug>(r: Result<T, RequestError>) -> ErrorCode {
    match r {
        Err(RequestError::Api(b)) => b.code,
        other => panic!("expected an API error, got {other:?}"),
    }
}

#[track_caller]
fn deny<T: std::fmt::Debug>(
    s: &InMemoryServer,
    expected: ErrorCode,
    f: impl FnOnce() -> Result<T, RequestError>,
) {
    let before = s.state_fingerprint();
    assert_eq!(code(f()), expected);
    assert_eq!(s.state_fingerprint(), before, "a refusal changed state");
}

/// A pushed item that was rejected, with its code; asserts nothing else changed.
#[track_caller]
fn rejected(s: &InMemoryServer, expected: ErrorCode, f: impl FnOnce() -> PushOpsResponse) {
    let before = s.state_fingerprint();
    let r = f();
    assert!(
        matches!(&r.acks[0].result, AckResult::Rejected { error } if error.code == expected),
        "{r:?}"
    );
    assert_eq!(s.state_fingerprint(), before);
}

fn accepted(r: &PushOpsResponse) -> bool {
    r.acks
        .iter()
        .all(|a| matches!(a.result, AckResult::Accepted { .. }))
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
    seq: std::cell::Cell<u64>,
    accounts: std::collections::BTreeMap<&'static str, AccountId>,
}

fn a(n: &str) -> AccountId {
    acct(&format!("acct-{n}"))
}
fn d(n: &str) -> DeviceId {
    dev(&format!("dev-{n}"))
}

impl World {
    fn new() -> Self {
        Self::with(InMemoryServer::new())
    }

    /// Owner, editor, commenter, viewer, a removed member (an editor before) and an outsider;
    /// every member with a device `dev-<name>` in the space.
    fn with(server: InMemoryServer) -> Self {
        let s = Arc::new(server);
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
            assert_eq!(
                s.invitation_accept(&a(n), &inv.invitation_id).unwrap().role,
                role
            );
        }
        for n in ["owner", "editor", "commenter", "viewer", "revoked"] {
            s.register_device_keys(
                Caller::member(&space, &a(n)),
                &DeviceRegistrationRequest {
                    device_id: Some(d(n)),
                    signing_public_key: base64url::encode(&[1; 32]),
                    encryption_public_key: base64url::encode(&[2; 32]),
                    label: None,
                },
            )
            .unwrap();
        }
        s.shared_remove_member(&a("owner"), &space, &a("revoked"))
            .unwrap();
        World::from_parts(s, space, 0)
    }

    fn from_parts(s: Arc<InMemoryServer>, space: AccountId, seq: u64) -> Self {
        World {
            s,
            space,
            seq: std::cell::Cell::new(seq),
            accounts: NAMES.into_iter().map(|n| (n, a(n))).collect(),
        }
    }

    fn m(&self, n: &str) -> Caller<'_> {
        Caller::member(&self.space, &self.accounts[n])
    }

    /// A comment push body from `n`'s device in `t` mentioning `mentions`.
    fn comment(&self, n: &str, t: &str, mentions: &[&str]) -> (String, Vec<u8>) {
        self.item(n, t, mentions, CommentPushKind::Comment)
    }

    fn item(
        &self,
        n: &str,
        t: &str,
        mentions: &[&str],
        kind: CommentPushKind,
    ) -> (String, Vec<u8>) {
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        let device = format!("dev-{n}");
        let ct = fake_ciphertext(&format!("comment-{n}-{seq}"), 60);
        let op = format!("c-{n}-{seq}");
        let item = CommentPushItem {
            envelope: header_for(&op, &device, seq, &[], &ct),
            thread_id: thread(t),
            kind,
            mentions: mentions.iter().map(|m| a(m)).collect(),
        };
        (
            op,
            encode_json_batch(&[(item, ct)], &Limits::DEFAULT).unwrap(),
        )
    }

    fn post(&self, n: &str, t: &str, mentions: &[&str]) -> Result<PushOpsResponse, RequestError> {
        let (_, body) = self.comment(n, t, mentions);
        self.s.comment_push(self.m(n), &d(n), &body)
    }

    /// An update (edit, deletion, resolve…) on thread `t`.
    fn update(&self, n: &str, t: &str, mentions: &[&str]) -> Result<PushOpsResponse, RequestError> {
        let (_, body) = self.item(n, t, mentions, CommentPushKind::Update);
        self.s.comment_push(self.m(n), &d(n), &body)
    }

    fn doc_push(&self, n: &str) -> Result<PushOpsResponse, RequestError> {
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        let device = format!("dev-{n}");
        let ct = fake_ciphertext(&format!("doc-{n}-{seq}"), 40);
        let h = header_for(&format!("op-{n}-{seq}"), &device, seq, &[], &ct);
        let body = encode_json_batch(&[(h, ct)], &Limits::DEFAULT).unwrap();
        self.s.push(self.m(n), &d(n), &body)
    }

    fn notes(&self, n: &str) -> Vec<(NotificationKind, AccountId, ThreadId)> {
        self.s
            .notifications_list(&a(n), None, 100)
            .unwrap()
            .notifications
            .into_iter()
            .map(|r| (r.kind, r.actor, r.thread_id))
            .collect()
    }
}

// ------------------------------------------------------------------ role matrix --

#[test]
fn comment_stream_role_matrix() {
    let w = World::new();
    let s = &*w.s;
    // Owner, editor and commenter write.
    for n in ["owner", "editor", "commenter"] {
        assert!(accepted(&w.post(n, "t-1", &[]).unwrap()), "{n}");
    }
    assert_eq!(s.comment_count(&w.space), 3);
    // Viewer reads only; removed and other accounts get nothing.
    let (_, body) = w.comment("viewer", "t-1", &[]);
    deny(s, ErrorCode::Forbidden, || {
        s.comment_push(w.m("viewer"), &d("viewer"), &body)
    });
    let (_, body) = w.comment("revoked", "t-1", &[]);
    deny(s, ErrorCode::AccessRevoked, || {
        s.comment_push(w.m("revoked"), &d("revoked"), &body)
    });
    deny(s, ErrorCode::NotFound, || {
        s.comment_push(w.m("outsider"), &d("editor"), &body)
    });
    // A forged device in the envelope is rejected per item.
    let (_, body) = w.comment("owner", "t-1", &[]);
    rejected(s, ErrorCode::Forbidden, || {
        s.comment_push(w.m("editor"), &d("editor"), &body).unwrap()
    });
    let req = PullOpsRequest {
        cursor: None,
        limit: 10,
    };
    for n in ["owner", "editor", "commenter", "viewer"] {
        let page = s.comment_pull(w.m(n), &d(n), &req).unwrap();
        assert_eq!(page.response.items.len(), 3, "{n}");
        assert_eq!(page.response.items[2].member, a("commenter"));
        s.comment_ack(
            w.m(n),
            &d(n),
            &AckRequest {
                applied_cursor: page.response.next_cursor,
            },
        )
        .unwrap();
        assert_eq!(s.comment_applied_position(&w.space, &d(n)), Some(3));
    }
    deny(s, ErrorCode::AccessRevoked, || {
        s.comment_pull(w.m("revoked"), &d("revoked"), &req)
    });
    deny(s, ErrorCode::NotFound, || {
        s.comment_pull(w.m("outsider"), &d("viewer"), &req)
    });
    // Ordinary accounts have no comment stream.
    s.register_device(&a("outsider"), &dev("dev-own"));
    deny(s, ErrorCode::Forbidden, || {
        s.comment_pull(&a("outsider"), &dev("dev-own"), &req)
    });
}

#[test]
fn commenters_cannot_touch_the_document() {
    let w = World::new();
    let s = &*w.s;
    // The document stream.
    let before = s.state_fingerprint();
    assert_eq!(code(w.doc_push("commenter")), ErrorCode::Forbidden);
    assert_eq!(s.state_fingerprint(), before);
    assert_eq!(s.op_count(&w.space), 0);
    // Blobs and checkpoints.
    let bytes = fake_ciphertext("blob", 32);
    let sha = sha256_hex(&bytes);
    deny(s, ErrorCode::Forbidden, || {
        s.blob_put(
            w.m("commenter"),
            &d("commenter"),
            &"b-1".parse().unwrap(),
            &sha,
            &bytes,
        )
    });
    let page = s
        .pull(
            w.m("commenter"),
            &d("commenter"),
            &PullOpsRequest {
                cursor: None,
                limit: 1,
            },
        )
        .unwrap();
    deny(s, ErrorCode::Forbidden, || {
        s.checkpoint_put(
            w.m("commenter"),
            &d("commenter"),
            &CheckpointRecord {
                covers_cursor: page.response.next_cursor.clone(),
                covers_server_seq: 0,
                blob_id: "b-1".parse().unwrap(),
                blob_sha256: sha.clone(),
                blob_length: 32,
                key_epoch: 1,
                created_by: d("commenter"),
                signature: base64url::encode(&[0xA5; 64]),
            },
        )
    });
    // Meta and members.
    deny(s, ErrorCode::Forbidden, || {
        s.shared_set_meta(
            &a("commenter"),
            &w.space,
            &UpdateSpaceMetaRequest {
                sealed_meta: "BBBB".into(),
            },
        )
    });
    deny(s, ErrorCode::Forbidden, || {
        s.shared_invite(
            &a("commenter"),
            &w.space,
            &CreateInvitationRequest {
                email: "x@example.org".into(),
                role: Role::Viewer,
                idempotency_key: "x".into(),
            },
        )
    });
    deny(s, ErrorCode::Forbidden, || {
        s.shared_change_role(
            &a("commenter"),
            &w.space,
            &a("viewer"),
            &ChangeRoleRequest {
                role: Role::Commenter,
                version: 2,
            },
        )
    });
    deny(s, ErrorCode::Forbidden, || {
        s.shared_remove_member(&a("commenter"), &w.space, &a("viewer"))
    });
    // But reads, the member list (for mention suggestions) and leaving work, for viewers too.
    for n in ["commenter", "viewer"] {
        let list = s.shared_members(&a(n), &w.space).unwrap();
        assert!(list
            .members
            .iter()
            .any(|m| m.is_you && m.account_id == a(n)));
        assert!(list.invitations.is_empty(), "invitations are the owner's");
        assert!(list
            .members
            .iter()
            .any(|m| m.display == "editor@example.org"));
    }
    s.shared_remove_member(&a("commenter"), &w.space, &a("commenter"))
        .unwrap();
}

#[test]
fn the_owner_can_grant_and_revoke_commenter() {
    let w = World::new();
    let s = &*w.s;
    let version = s
        .shared_members(&a("owner"), &w.space)
        .unwrap()
        .members
        .into_iter()
        .find(|m| m.account_id == a("viewer"))
        .unwrap()
        .version;
    let r = s
        .shared_change_role(
            &a("owner"),
            &w.space,
            &a("viewer"),
            &ChangeRoleRequest {
                role: Role::Commenter,
                version,
            },
        )
        .unwrap();
    assert_eq!(r.role, Role::Commenter);
    assert!(accepted(&w.post("viewer", "t-1", &[]).unwrap()));
    deny(s, ErrorCode::InvalidRole, || {
        s.shared_change_role(
            &a("owner"),
            &w.space,
            &a("viewer"),
            &ChangeRoleRequest {
                role: Role::Unknown,
                version: version + 1,
            },
        )
    });
}

// --------------------------------------------------------------------- mentions --

#[test]
fn mentions_must_be_active_members_and_bounded() {
    let w = World::new();
    let s = &*w.s;
    for bad in [vec!["outsider"], vec!["revoked"], vec!["editor", "nobody"]] {
        let (_, body) = w.comment("commenter", "t-1", &bad);
        rejected(s, ErrorCode::ValidationFailed, || {
            s.comment_push(w.m("commenter"), &d("commenter"), &body)
                .unwrap()
        });
    }
    // More than 20, or repeated.
    let many: Vec<String> = (0..21).map(|i| format!("m{i}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let (_, body) = w.comment("commenter", "t-1", &many);
    rejected(s, ErrorCode::ValidationFailed, || {
        s.comment_push(w.m("commenter"), &d("commenter"), &body)
            .unwrap()
    });
    let (_, body) = w.comment("commenter", "t-1", &["editor", "editor"]);
    rejected(s, ErrorCode::ValidationFailed, || {
        s.comment_push(w.m("commenter"), &d("commenter"), &body)
            .unwrap()
    });
    // Viewers can be mentioned (they read comments).
    assert!(accepted(&w.post("commenter", "t-1", &["viewer"]).unwrap()));
}

// ---------------------------------------------------------------- notifications --

#[test]
fn notifications_for_mentions_and_replies() {
    let w = World::new();
    // The commenter starts a thread mentioning the editor (and themselves: skipped).
    assert!(accepted(
        &w.post("commenter", "t-1", &["editor", "commenter"])
            .unwrap()
    ));
    assert_eq!(
        w.notes("editor"),
        vec![(NotificationKind::Mention, a("commenter"), thread("t-1"))]
    );
    assert!(w.notes("commenter").is_empty());
    // The owner replies: the commenter (a participant) gets a reply; the editor isn't a
    // participant yet (being mentioned doesn't make you one).
    assert!(accepted(&w.post("owner", "t-1", &[]).unwrap()));
    assert_eq!(
        w.notes("commenter"),
        vec![(NotificationKind::Reply, a("owner"), thread("t-1"))]
    );
    assert_eq!(w.notes("editor").len(), 1);
    // The editor replies and mentions the owner: the owner gets a mention, not also a reply.
    assert!(accepted(&w.post("editor", "t-1", &["owner"]).unwrap()));
    assert_eq!(
        w.notes("owner"),
        vec![(NotificationKind::Mention, a("editor"), thread("t-1"))]
    );
    assert_eq!(w.notes("commenter").len(), 2);
    // Another thread doesn't involve t-1's participants.
    assert!(accepted(&w.post("editor", "t-2", &[]).unwrap()));
    assert_eq!(w.notes("commenter").len(), 2);
}

#[test]
fn updates_notify_only_whom_they_mention() {
    let w = World::new();
    let s = &*w.s;
    // A thread with two participants.
    assert!(accepted(&w.post("commenter", "t-1", &[]).unwrap()));
    assert!(accepted(&w.post("owner", "t-1", &[]).unwrap()));
    assert_eq!(w.notes("commenter").len(), 1, "the owner's reply");
    // Resolving and editing are updates: no "reply" for anyone.
    assert!(accepted(&w.update("owner", "t-1", &[]).unwrap()));
    assert!(accepted(&w.update("commenter", "t-1", &[]).unwrap()));
    assert_eq!(w.notes("commenter").len(), 1);
    assert!(w.notes("owner").is_empty());
    // An update doesn't make you a participant: the editor edits (say, as a moderator would),
    // then a new reply doesn't notify them.
    assert!(accepted(&w.update("editor", "t-1", &[]).unwrap()));
    assert!(accepted(&w.post("commenter", "t-1", &[]).unwrap()));
    assert!(w.notes("editor").is_empty());
    assert_eq!(w.notes("owner").len(), 1, "the owner is a participant");
    // A mention added by an edit notifies the mentioned member, under the real thread.
    assert!(accepted(
        &w.update("commenter", "t-1", &["viewer"]).unwrap()
    ));
    assert_eq!(
        w.notes("viewer"),
        vec![(NotificationKind::Mention, a("commenter"), thread("t-1"))]
    );
    // ...unless they muted that thread.
    s.notification_prefs_put(
        &a("editor"),
        &NotificationPrefs {
            muted_spaces: vec![],
            muted_threads: vec![MutedThread {
                space_id: w.space.clone(),
                thread_id: thread("t-1"),
            }],
        },
    )
    .unwrap();
    assert!(accepted(
        &w.update("commenter", "t-1", &["editor"]).unwrap()
    ));
    assert!(w.notes("editor").is_empty());
    // Updates are attributed and hinted like any comment.
    assert_eq!(s.comment_count(&w.space), 8);
}

#[test]
fn notifications_are_deduplicated_on_retries() {
    let w = World::new();
    let s = &*w.s;
    let (_, body) = w.comment("commenter", "t-1", &["editor"]);
    // The first response is lost; the retry is a duplicate and notifies nobody again.
    s.drop_next_responses(1);
    assert_eq!(
        s.comment_push(w.m("commenter"), &d("commenter"), &body),
        Err(RequestError::ResponseLost)
    );
    assert_eq!(w.notes("editor").len(), 1, "a lost response still notifies");
    let r = s
        .comment_push(w.m("commenter"), &d("commenter"), &body)
        .unwrap();
    assert!(matches!(r.acks[0].result, AckResult::Duplicate { .. }));
    assert_eq!(w.notes("editor").len(), 1);
}

#[test]
fn mutes_read_state_paging_and_isolation() {
    let w = World::new();
    let s = &*w.s;
    s.notification_prefs_put(
        &a("editor"),
        &NotificationPrefs {
            muted_spaces: vec![],
            muted_threads: vec![MutedThread {
                space_id: w.space.clone(),
                thread_id: thread("t-muted"),
            }],
        },
    )
    .unwrap();
    assert!(accepted(
        &w.post("commenter", "t-muted", &["editor"]).unwrap()
    ));
    assert!(w.notes("editor").is_empty());
    for i in 0..3 {
        assert!(accepted(
            &w.post("commenter", &format!("t-{i}"), &["editor"]).unwrap()
        ));
    }
    // Paging, unread count, marking read.
    let p1 = s.notifications_list(&a("editor"), None, 2).unwrap();
    assert_eq!(
        (p1.notifications.len(), p1.has_more, p1.unread),
        (2, true, 3)
    );
    let p2 = s
        .notifications_list(&a("editor"), Some(&p1.next_cursor), 2)
        .unwrap();
    assert_eq!((p2.notifications.len(), p2.has_more), (1, false));
    let id = p1.notifications[0].notification_id.clone();
    let r = s
        .notification_update(&a("editor"), &id, &UpdateNotificationRequest { read: true })
        .unwrap();
    assert!(r.read);
    assert_eq!(
        s.notifications_list(&a("editor"), None, 10).unwrap().unread,
        2
    );
    // Someone else's notification, a forged cursor, bad prefs.
    deny(s, ErrorCode::NotFound, || {
        s.notification_update(&a("owner"), &id, &UpdateNotificationRequest { read: true })
    });
    deny(s, ErrorCode::InvalidCursor, || {
        s.notifications_list(&a("owner"), Some(&p1.next_cursor), 10)
    });
    let comment_cursor = s
        .comment_pull(
            w.m("editor"),
            &d("editor"),
            &PullOpsRequest {
                cursor: None,
                limit: 1,
            },
        )
        .unwrap()
        .response
        .next_cursor;
    deny(s, ErrorCode::InvalidCursor, || {
        s.pull(
            w.m("editor"),
            &d("editor"),
            &PullOpsRequest {
                cursor: Some(comment_cursor.clone()),
                limit: 1,
            },
        )
    });
    deny(s, ErrorCode::ValidationFailed, || {
        s.notification_prefs_put(
            &a("editor"),
            &NotificationPrefs {
                muted_spaces: vec![w.space.clone(), w.space.clone()],
                muted_threads: vec![],
            },
        )
    });
    // Muting the whole space stops everything new.
    s.notification_prefs_put(
        &a("editor"),
        &NotificationPrefs {
            muted_spaces: vec![w.space.clone()],
            muted_threads: vec![],
        },
    )
    .unwrap();
    assert_eq!(
        s.notification_prefs_get(&a("editor")).unwrap().muted_spaces,
        vec![w.space.clone()]
    );
    assert!(accepted(&w.post("commenter", "t-9", &["editor"]).unwrap()));
    assert_eq!(w.notes("editor").len(), 3);
    // Space sessions can't use the notification routes.
    deny(s, ErrorCode::Forbidden, || {
        s.notifications_list(&w.space, None, 10)
    });
}

#[test]
fn revocation_stops_comments_and_notifications() {
    let w = World::new();
    let s = &*w.s;
    assert!(accepted(&w.post("commenter", "t-1", &["editor"]).unwrap()));
    assert!(accepted(&w.post("editor", "t-1", &[]).unwrap()));
    assert_eq!(w.notes("editor").len(), 1);
    s.shared_remove_member(&a("owner"), &w.space, &a("editor"))
        .unwrap();
    // Their notifications from the space disappear, their reads and writes are refused, they
    // can't be mentioned, and replies in their old thread don't notify them.
    assert!(w.notes("editor").is_empty());
    assert_eq!(
        s.notifications_list(&a("editor"), None, 10).unwrap().unread,
        0
    );
    assert_eq!(code(w.post("editor", "t-1", &[])), ErrorCode::AccessRevoked);
    let (_, body) = w.comment("commenter", "t-1", &["editor"]);
    rejected(s, ErrorCode::ValidationFailed, || {
        s.comment_push(w.m("commenter"), &d("commenter"), &body)
            .unwrap()
    });
    assert!(accepted(&w.post("commenter", "t-1", &[]).unwrap()));
    assert!(w.notes("editor").is_empty());
}

// --------------------------------------------------------------------- live hints --

#[test]
fn live_sessions_get_comment_and_notification_hints() {
    let w = World::new();
    let s = &*w.s;
    let hello = |n: &str, minor: u16| Hello {
        protocol: ProtocolVersion::new(1, minor),
        device_id: d(n),
        last_server_seq: 0,
        presence: true,
    };
    let editor = s
        .live_connect(w.m("editor"), &d("editor"), &hello("editor", 9))
        .unwrap()
        .session_id;
    // A 1.8 session of the commenter sees `viewer`, and no comment frames.
    let old = s
        .live_connect(w.m("commenter"), &d("commenter"), &hello("commenter", 8))
        .unwrap();
    assert_eq!(old.role, Role::Viewer);
    let new = s
        .live_connect(w.m("commenter"), &d("commenter"), &hello("commenter", 9))
        .unwrap();
    assert_eq!(new.role, Role::Commenter);
    assert!(accepted(&w.post("commenter", "t-1", &["editor"]).unwrap()));
    assert!(accepted(&w.post("owner", "t-1", &[]).unwrap()));
    let frames = s.live_recv(&editor);
    assert!(frames
        .iter()
        .any(|f| matches!(f, ServerFrame::CommentCommitted(c) if c.server_seq == 2)));
    assert_eq!(
        frames
            .iter()
            .filter(|f| matches!(f, ServerFrame::CommentCommitted(_)))
            .count(),
        1,
        "coalesced: {frames:?}"
    );
    assert!(frames
        .iter()
        .any(|f| matches!(f, ServerFrame::Notification(n) if n.space_id == w.space)));
    assert!(s.live_recv(&old.session_id).is_empty());
    // the new commenter session gets the reply notification
    assert!(s
        .live_recv(&new.session_id)
        .iter()
        .any(|f| matches!(f, ServerFrame::Notification(_))));
}

// ------------------------------------------------------------------- persistence --

#[test]
fn comments_notifications_and_prefs_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let space = {
        let w = World::with(InMemoryServer::open_persistent(dir.path()).unwrap());
        assert!(accepted(&w.post("commenter", "t-1", &["editor"]).unwrap()));
        w.s.notification_prefs_put(
            &a("owner"),
            &NotificationPrefs {
                muted_spaces: vec![],
                muted_threads: vec![MutedThread {
                    space_id: w.space.clone(),
                    thread_id: thread("t-1"),
                }],
            },
        )
        .unwrap();
        w.space.clone()
    };
    let s = InMemoryServer::open_persistent(dir.path()).unwrap();
    assert_eq!(s.comment_count(&space), 1);
    assert_eq!(
        s.notifications_list(&a("editor"), None, 10)
            .unwrap()
            .notifications
            .len(),
        1
    );
    assert_eq!(
        s.notification_prefs_get(&a("owner"))
            .unwrap()
            .muted_threads
            .len(),
        1
    );
    // participants survived: the editor's reply notifies the commenter
    let w = World::from_parts(Arc::new(s), space, 100);
    assert!(accepted(&w.post("editor", "t-1", &[]).unwrap()));
    assert_eq!(w.notes("commenter").len(), 1);
}

// ------------------------------------------------------------------------- HTTP --

fn http_call(
    base: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &[u8],
) -> (u16, Vec<u8>) {
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
fn comment_and_notification_routes_over_http() {
    let w = World::new();
    let h = http::spawn("127.0.0.1:0", w.s.clone()).unwrap();
    let base = h.base_url.clone();
    let session = |v: serde_json::Value| {
        let (status, body) = http_call(
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
    for (m, p) in [
        ("POST", "/v1/comments/batch"),
        ("GET", "/v1/comments"),
        ("POST", "/v1/comments/acks"),
        ("GET", "/v1/notifications"),
        ("PATCH", "/v1/notifications/n-1"),
        ("GET", "/v1/notification-prefs"),
        ("PUT", "/v1/notification-prefs"),
    ] {
        assert_eq!(http_call(&base, m, p, None, b"{}").0, 401, "{m} {p}");
    }
    let commenter = session(serde_json::json!({"account": w.space, "device_id": "dev-commenter"}));
    let viewer = session(serde_json::json!({"account": w.space, "device_id": "dev-viewer"}));
    let editor_own = session(serde_json::json!({"account": "acct-editor"}));
    let (_, body) = w.comment("commenter", "t-1", &["editor"]);
    let (status, resp) = http_call(&base, "POST", "/v1/comments/batch", Some(&commenter), &body);
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&resp));
    let (_, body) = w.comment("viewer", "t-1", &[]);
    assert_eq!(
        http_call(&base, "POST", "/v1/comments/batch", Some(&viewer), &body).0,
        403
    );
    // A commenter's document push over HTTP.
    let ct = fake_ciphertext("doc", 40);
    let h2 = header_for("op-x", "dev-commenter", 50, &[], &ct);
    let doc = encode_json_batch(&[(h2, ct)], &Limits::DEFAULT).unwrap();
    assert_eq!(
        http_call(&base, "POST", "/v1/ops/batch", Some(&commenter), &doc).0,
        403
    );
    let (status, page) = http_call(&base, "GET", "/v1/comments?limit=10", Some(&viewer), b"");
    assert_eq!(status, 200);
    assert!(!page.is_empty());
    let (status, body) = http_call(&base, "GET", "/v1/notifications", Some(&editor_own), b"");
    assert_eq!(status, 200);
    let page: penroam_sync_protocol::comments::NotificationPage =
        serde_json::from_slice(&body).unwrap();
    assert_eq!(page.unread, 1);
    let id = page.notifications[0].notification_id.as_str().to_string();
    let (status, _) = http_call(
        &base,
        "PATCH",
        &format!("/v1/notifications/{id}"),
        Some(&editor_own),
        br#"{"read":true}"#,
    );
    assert_eq!(status, 200);
    let (status, _) = http_call(
        &base,
        "PUT",
        "/v1/notification-prefs",
        Some(&editor_own),
        br#"{"muted_spaces":[],"muted_threads":[]}"#,
    );
    assert_eq!(status, 204);
    // A space token on the notification routes.
    assert_eq!(
        http_call(&base, "GET", "/v1/notifications", Some(&viewer), b"").0,
        403
    );
}
