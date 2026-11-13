//! Comment streams end to end against the public test server: concurrent comments by several
//! members, the resolve-versus-reply race, what commenters and viewers may push, a comment
//! envelope hiding a document operation, the offline queue across crashes and lost responses,
//! drafts when commenting is no longer allowed, and notifications from mentions and replies.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{account, add, stroke};
use penroam_core::comment::{CommentAnchor, CommentOp, CommentOperation, ThreadStatus};
use penroam_core::PageDefaults;
use penroam_storage::Library;
use penroam_sync::comments::{
    comment_key, mark_notification, notification_prefs, notifications, seal_payload,
    set_notification_prefs, CommentIntent, CommentReport, CommentSync, CommentTransport, Delivery,
    SpaceCredentials, SpaceRoster,
};
use penroam_sync::shared::{SpaceAccess, SpaceSet};
use penroam_sync::transport::InProcessTransport;
use penroam_sync::vault::Scope;
use penroam_sync::{FileSecretStore, SecretStore, Setup, SyncEngine, SyncTransport};
use penroam_sync_protocol::comments::{
    CommentPushItem, MutedThread, NotificationKind, NotificationPrefs,
};
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::{
    AccountId, ChangeRoleRequest, CreateInvitationRequest, DeviceId, ErrorCode, Limits, Role,
    ThreadId,
};
use penroam_sync_testkit::InMemoryServer;
use uuid::Uuid;

type Engine = SyncEngine<InProcessTransport>;

// ---------------------------------------------------------------------------
// people and devices (as in tests/shared.rs)

struct Dev {
    lib: Mutex<Library>,
    account: Engine,
    spaces: SpaceSet<InProcessTransport>,
    root: PathBuf,
    server: Arc<InMemoryServer>,
    me: AccountId,
    _dir: tempfile::TempDir,
}

fn secrets_at(root: &std::path::Path) -> Box<dyn SecretStore> {
    Box::new(FileSecretStore::new(root.join("sync-secrets")))
}

fn person(s: &Arc<InMemoryServer>, name: &str) -> AccountId {
    let a = account(s, name);
    s.set_verified_email(&a, Some(&format!("{name}@example.org")))
        .unwrap();
    a
}

impl Dev {
    fn new(s: &Arc<InMemoryServer>, acct: &AccountId, label: &str) -> Dev {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let lib = Library::open(&root).unwrap();
        let account = Engine::connect(
            Setup {
                library_root: root.clone(),
                scope: Scope::Account,
                device_id: None,
                server_url: "in-process".into(),
                account_id: acct.as_str().into(),
                label: label.into(),
            },
            InProcessTransport::new(s.clone(), acct.clone(), DeviceId::new("pending").unwrap()),
            secrets_at(&root),
        )
        .unwrap();
        Dev {
            lib: Mutex::new(lib),
            account,
            spaces: SpaceSet::new(),
            root,
            server: s.clone(),
            me: acct.clone(),
            _dir: dir,
        }
    }

    fn with_access<R>(
        &mut self,
        f: impl FnOnce(
            &mut Engine,
            &mut SpaceSet<InProcessTransport>,
            &Mutex<Library>,
            &mut SpaceAccess<'_, InProcessTransport>,
        ) -> R,
    ) -> R {
        let server = self.server.clone();
        let me = self.me.clone();
        let root = self.root.clone();
        let mut connect = move |acc: &mut Engine,
                                id: &AccountId|
              -> penroam_sync::engine::Result<InProcessTransport> {
            acc.transport_mut().shared_session(id)?;
            Ok(InProcessTransport::for_space(
                server.clone(),
                id.clone(),
                me.clone(),
                DeviceId::new(acc.device_id()).unwrap(),
            ))
        };
        let secrets = move || Ok(secrets_at(&root));
        let mut upgrade = |_: &mut Engine, _: &mut Engine| Ok(());
        let mut access = SpaceAccess {
            connect: &mut connect,
            secrets: &secrets,
            upgrade: &mut upgrade,
        };
        f(&mut self.account, &mut self.spaces, &self.lib, &mut access)
    }

    fn sync(&mut self) {
        let _ = self.account.reconcile(&self.lib);
        self.with_access(|acc, spaces, lib, access| {
            let _ = spaces.refresh(acc, lib);
            spaces.reconcile(acc, lib, access)
        });
    }
}

/// A shares a notebook with B (`role`); both have it. Returns the server, both devices, the
/// notebook and the space.
fn shared_pair(role: Role) -> (Arc<InMemoryServer>, Dev, Dev, Uuid, String) {
    let s = Arc::new(InMemoryServer::new());
    let (a_id, b_id) = (person(&s, "ann"), person(&s, "ben"));
    let mut a = Dev::new(&s, &a_id, "ann's tablet");
    let mut b = Dev::new(&s, &b_id, "ben's laptop");
    let (nb, page) = a
        .lib
        .lock()
        .unwrap()
        .create_notebook("Field notes", PageDefaults::default(), None)
        .unwrap();
    add(
        &mut a.lib.lock().unwrap(),
        nb.id,
        page.id,
        vec![stroke(1, 1.0, "#000000")],
    );
    a.sync();
    let space = a
        .with_access(|acc, spaces, lib, access| spaces.share(acc, lib, access, nb.id))
        .unwrap();
    a.sync();
    a.account
        .transport_mut()
        .shared_invite(
            &space.parse().unwrap(),
            &CreateInvitationRequest {
                email: "ben@example.org".into(),
                role,
                idempotency_key: Uuid::new_v4().to_string(),
            },
        )
        .unwrap();
    let _ = b.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
    let inv = b.spaces.invitations[0].invitation_id.clone();
    b.with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv))
        .unwrap();
    for _ in 0..3 {
        a.sync();
        b.sync();
    }
    (s, a, b, nb.id, space)
}

// ---------------------------------------------------------------------------
// one member's comment stream

/// A member's comment stream, wired as the worker would wire it.
struct Talker {
    sync: CommentSync,
    t: InProcessTransport,
    dir: PathBuf,
    space: String,
    nb: Uuid,
    me: AccountId,
    root: PathBuf,
    server: Arc<InMemoryServer>,
    device: Uuid,
    /// Test knob: the roster this member believes in (`None`: the server's member list).
    pretend_role: Option<Role>,
    at: i64,
    /// Keys from the last successful load (a removed member can't read the key log any more).
    cached: Option<SpaceCredentials>,
    /// Test knob: lose this many responses after keys and roster were read.
    drop: u32,
}

impl Talker {
    fn new(d: &Dev, space: &str, nb: Uuid) -> Talker {
        let keys = space_device(d, space);
        let device = Uuid::parse_str(&keys).unwrap();
        let dir = d.root.join("comment-test").join(space);
        Talker {
            sync: CommentSync::open(&dir, device, &d.me).unwrap(),
            t: InProcessTransport::for_space(
                d.server.clone(),
                space.parse().unwrap(),
                d.me.clone(),
                DeviceId::new(keys).unwrap(),
            ),
            dir,
            space: space.into(),
            nb,
            me: d.me.clone(),
            root: d.root.clone(),
            server: d.server.clone(),
            device,
            pretend_role: None,
            at: 1_700_000_000_000,
            cached: None,
            drop: 0,
        }
    }

    /// Reopens from disk (what a restart after a crash does).
    fn restart(&mut self) {
        self.sync = CommentSync::open(&self.dir, self.device, &self.me).unwrap();
    }

    fn creds(&mut self) -> SpaceCredentials {
        let mut st = InProcessTransport::for_space(
            self.server.clone(),
            self.space.parse().unwrap(),
            self.me.clone(),
            self.t.device.clone(),
        );
        if let Ok(c) =
            SpaceCredentials::load(&mut st, &*secrets_at(&self.root), &self.space, self.nb)
        {
            self.cached = Some(c);
        }
        let c = self.cached.as_ref().expect("loaded once while a member");
        SpaceCredentials {
            device: penroam_sync::crypto::DeviceKeys::from_secret_string(
                &c.device.to_secret_string(),
            )
            .unwrap(),
            membership: c.membership.clone(),
            epoch_keys: c.epoch_keys.clone(),
            notebook: c.notebook,
        }
    }

    fn roster(&mut self) -> SpaceRoster {
        let mut st = InProcessTransport::new(
            self.server.clone(),
            self.me.clone(),
            DeviceId::new("x").unwrap(),
        );
        let list = st.shared_members(&self.space.parse().unwrap());
        let mut r = match list {
            Ok(l) => SpaceRoster::from_member_list(&l).unwrap(),
            // a removed member no longer sees the list: what the app last knew
            Err(_) => SpaceRoster {
                me: self.me.clone(),
                role: Role::Editor,
                owner: self.me.clone(),
                active: [self.me.clone()].into(),
                devices: Default::default(),
            },
        };
        if let Some(role) = self.pretend_role {
            r.role = role;
        }
        r
    }

    fn try_sync(&mut self) -> penroam_sync::comments::Result<CommentReport> {
        let creds = self.creds();
        let roster = self.roster();
        if self.drop > 0 {
            self.server
                .drop_next_responses(std::mem::take(&mut self.drop));
        }
        self.sync
            .sync(&mut self.t, &creds.keys(), &roster, &Limits::DEFAULT)
    }

    fn sync(&mut self) -> CommentReport {
        self.try_sync().unwrap()
    }

    fn submit(&mut self, intent: CommentIntent) -> Uuid {
        self.at += 1000;
        let r = self.roster();
        self.sync
            .submit(intent, r.may_comment(), r.is_owner(), self.at)
            .unwrap()
    }

    fn thread(&mut self, body: &str) -> Uuid {
        self.submit(CommentIntent::CreateThread {
            anchor: CommentAnchor::PagePoint {
                page_id: Uuid::from_u128(7),
                x: -12.0,
                y: 30.5,
            },
            body: body.into(),
        })
    }

    fn reply(&mut self, thread: Uuid, body: &str) -> Uuid {
        self.submit(CommentIntent::Reply {
            thread_id: thread,
            body: body.into(),
            reply_to: None,
        })
    }

    fn view(&self) -> String {
        serde_json::to_string(&self.sync.threads()).unwrap()
    }
}

/// This device's id in a space (from the keys it keeps for it).
fn space_device(d: &Dev, space: &str) -> String {
    let raw = secrets_at(&d.root)
        .get(
            &Scope::shared(space)
                .unwrap()
                .secret_name(penroam_sync::engine::DEVICE_SECRET),
        )
        .unwrap()
        .unwrap();
    penroam_sync::crypto::DeviceKeys::from_secret_string(&raw)
        .unwrap()
        .device_id
}

fn settle(ts: &mut [&mut Talker]) {
    for _ in 0..3 {
        for t in ts.iter_mut() {
            t.sync();
        }
    }
}

fn change_role(a: &mut Dev, space: &str, who: &AccountId, role: Role) {
    let sid = space.parse().unwrap();
    let members = a.account.transport_mut().shared_members(&sid).unwrap();
    let m = members
        .members
        .iter()
        .find(|m| &m.account_id == who)
        .unwrap();
    a.account
        .transport_mut()
        .shared_change_role(
            &sid,
            who,
            &ChangeRoleRequest {
                role,
                version: m.version,
            },
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// tests

#[test]
fn two_members_comment_concurrently_and_see_the_same_threads() {
    let (s, a, b, nb, space) = shared_pair(Role::Editor);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    // both write before either syncs
    let t1 = ta.thread("Is this the right arrow?");
    let t2 = tb.thread("Typo on the left");
    assert_eq!(ta.sync.delivery(t1), Some(Delivery::Waiting));
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(ta.sync.delivery(t1), Some(Delivery::Sent));
    // concurrent replies in each other's threads, and in their own
    ta.reply(t2, "fixed it");
    tb.reply(t1, "no, the other one");
    tb.reply(t2, "thanks");
    ta.reply(t1, "ok");
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(ta.view(), tb.view());
    let threads = ta.sync.threads();
    assert_eq!(threads.len(), 2);
    assert_eq!(threads.iter().map(|t| t.comments.len()).sum::<usize>(), 6);
    assert_eq!(s.comment_count(&space.parse().unwrap()), 6);
    // authors come from the verified attribution
    let th = threads.iter().find(|t| t.thread_id == t1).unwrap();
    assert_eq!(th.created_by, a.me.as_str());
    assert!(ta.sync.quarantined().is_empty() && tb.sync.quarantined().is_empty());
    // more syncs change nothing
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(s.comment_count(&space.parse().unwrap()), 6);
}

#[test]
fn a_reply_racing_a_resolve_reopens_the_thread_for_everyone() {
    let (_s, a, b, nb, space) = shared_pair(Role::Editor);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = ta.thread("Ready to ship?");
    settle(&mut [&mut ta, &mut tb]);
    // A resolves while B, offline, replies
    tb.t.offline = true;
    ta.submit(CommentIntent::SetResolved {
        thread_id: t,
        resolved: true,
    });
    ta.sync();
    tb.reply(t, "one more thing");
    assert!(tb.try_sync().unwrap_err().to_string().contains("offline"));
    tb.t.offline = false;
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(ta.view(), tb.view());
    let th = ta.sync.replica().thread(t).unwrap();
    assert_eq!(th.status, ThreadStatus::Open);
    assert_eq!(th.comments.len(), 2);
    // resolving after seeing the reply closes it
    ta.submit(CommentIntent::SetResolved {
        thread_id: t,
        resolved: true,
    });
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(
        tb.sync.replica().thread(t).unwrap().status,
        ThreadStatus::Resolved
    );
}

#[test]
fn a_commenter_comments_but_a_forged_document_push_is_refused() {
    let (s, a, mut b, nb, space) = shared_pair(Role::Commenter);
    let sid: AccountId = space.parse().unwrap();
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = tb.thread("Can we move this up?");
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(s.comment_count(&sid), 1);
    assert_eq!(
        ta.sync.replica().thread(t).unwrap().created_by,
        b.me.as_str()
    );
    // a document envelope pushed on the space's operation log: refused for commenters
    let before = s.op_count(&sid);
    let creds = tb.creds();
    let op = fake_op(tb.device, 900, &b.me);
    let ek = &creds.epoch_keys[&creds.membership.current_epoch];
    let alias = ek.routing_id(nb);
    let (h, ct) = seal_payload(
        &op,
        b"anything",
        creds.membership.current_epoch,
        &ek.notebook_key(&alias),
        &alias,
        &creds.keys(),
    )
    .unwrap();
    let frame = encode_json_batch(&[(h, ct)], &Limits::DEFAULT).unwrap();
    let engine = b.spaces.engine(&space).unwrap();
    let err = engine.transport_mut().push(frame).unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::Forbidden));
    assert_eq!(s.op_count(&sid), before);
}

/// A comment operation used only for its header fields.
fn fake_op(device: Uuid, seq: u64, author: &AccountId) -> CommentOperation {
    let id = Uuid::new_v4();
    CommentOperation {
        id,
        device_id: device,
        sequence: seq,
        schema_version: 1,
        causal_dependencies: vec![],
        author: author.as_str().into(),
        moderation: false,
        at: 0,
        op: CommentOp::ThreadReopened { thread_id: id },
    }
}

fn push_raw(tb: &mut Talker, payload: &[u8], doc_key: bool, seq: u64) {
    let creds = tb.creds();
    let op = fake_op(tb.device, seq, &tb.me);
    let epoch = creds.membership.current_epoch;
    let ek = &creds.epoch_keys[&epoch];
    let alias = ek.routing_id(tb.nb);
    let key = if doc_key {
        ek.notebook_key(&alias)
    } else {
        comment_key(ek, &alias)
    };
    let (h, ct) = seal_payload(&op, payload, epoch, &key, &alias, &creds.keys()).unwrap();
    let item = CommentPushItem {
        kind: penroam_sync_protocol::comments::CommentPushKind::Comment,
        envelope: h,
        thread_id: ThreadId::new(op.id.to_string()).unwrap(),
        mentions: vec![],
    };
    let frame = encode_json_batch(&[(item, ct)], &Limits::DEFAULT).unwrap();
    let r = tb.t.comment_push(frame).unwrap();
    assert!(
        matches!(
            r.acks[0].result,
            penroam_sync_protocol::AckResult::Accepted { .. }
        ),
        "{r:?}"
    );
}

#[test]
fn a_comment_envelope_hiding_a_document_operation_is_quarantined() {
    let (_s, a, b, nb, space) = shared_pair(Role::Commenter);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    // B's client misbehaves: a document operation sealed under the comment key ...
    let doc = serde_json::json!({
        "type": "elementsAdded", "notebookId": nb, "pageId": Uuid::from_u128(1), "elements": []
    });
    push_raw(&mut tb, &serde_json::to_vec(&doc).unwrap(), false, 900);
    // ... and a payload sealed under the document key
    let mut real = b"PRCM".to_vec();
    real.extend_from_slice(&[1, 0, 0, 0]);
    push_raw(&mut tb, &real, true, 901);
    let r = ta.sync();
    assert_eq!(r.quarantined, 2, "{r:?}");
    let reasons: Vec<&str> = ta
        .sync
        .quarantined()
        .iter()
        .map(|q| q.reason.as_str())
        .collect();
    assert!(reasons[0].contains("not a comment"), "{reasons:?}");
    assert!(reasons[1].contains("tag mismatch"), "{reasons:?}");
    assert!(ta.sync.threads().is_empty());
    // quarantine survives a restart and isn't counted again
    ta.restart();
    assert_eq!(ta.sync.quarantined().len(), 2);
    assert_eq!(ta.sync().quarantined, 0);
}

#[test]
fn queued_comments_survive_crashes_and_lost_responses_without_duplicates() {
    let (s, a, b, nb, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    tb.t.offline = true;
    let t = tb.thread("written on the train");
    let r1 = tb.reply(t, "and another");
    tb.reply(t, "and a third");
    assert!(tb.try_sync().is_err());
    assert_eq!(tb.sync.waiting_count(), 3);
    // crash: a half-written record at the end of the log
    tb.restart();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(tb.dir.join("comments.log"))
            .unwrap();
        f.write_all(&[200, 0, 0, 0, 1, 2]).unwrap();
    }
    tb.restart();
    assert_eq!(tb.sync.waiting_count(), 3);
    assert_eq!(tb.sync.delivery(r1), Some(Delivery::Waiting));
    // online, but the push's response is lost: retried after another restart
    tb.t.offline = false;
    tb.drop = 1;
    let e = tb.try_sync().unwrap_err();
    assert!(matches!(
        e,
        penroam_sync::comments::CommentSyncError::Transport(ref t) if t.is_retryable()
    ));
    assert_eq!(
        s.comment_count(&sid),
        3,
        "stored although the answer was lost"
    );
    tb.restart();
    tb.sync();
    assert_eq!(s.comment_count(&sid), 3);
    assert_eq!(tb.sync.waiting_count(), 0);
    assert_eq!(tb.sync.delivery(r1), Some(Delivery::Sent));
    ta.sync();
    assert_eq!(ta.sync.replica().thread(t).unwrap().comments.len(), 3);
    assert_eq!(ta.view(), tb.view());
}

#[test]
fn a_removed_member_keeps_queued_comments_as_drafts_and_pushes_nothing() {
    let (s, mut a, b, nb, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = ta.thread("Agenda");
    settle(&mut [&mut ta, &mut tb]);
    tb.t.offline = true;
    let queued = tb.reply(t, "I'll bring the slides");
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    tb.t.offline = false;
    let before = s.comment_count(&sid);
    let r = tb.sync();
    assert!(r.permission_lost);
    assert_eq!(tb.sync.delivery(queued), Some(Delivery::Draft));
    assert_eq!(s.comment_count(&sid), before);
    // what's written afterwards is a draft from the start, and stays on this device
    tb.pretend_role = Some(Role::Viewer);
    let later = tb.reply(t, "hello?");
    assert_eq!(tb.sync.delivery(later), Some(Delivery::Draft));
    tb.sync();
    assert_eq!(s.comment_count(&sid), before);
    assert_eq!(
        tb.sync.unsent().len(),
        1,
        "the later draft took no sequence"
    );
    ta.sync();
    assert_eq!(ta.sync.replica().thread(t).unwrap().comments.len(), 1);
}

#[test]
fn a_draft_keeps_no_hole_in_the_stream_when_commenting_is_allowed_again() {
    let (s, mut a, b, nb, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = ta.thread("Plan");
    settle(&mut [&mut ta, &mut tb]);
    // B queues a reply, then becomes a viewer before it uploads
    tb.t.offline = true;
    let draft = tb.reply(t, "queued while still an editor");
    change_role(&mut a, &space, &b.me, Role::Viewer);
    tb.t.offline = false;
    let r = tb.sync();
    assert!(r.permission_lost && r.drafted == 1);
    assert_eq!(tb.sync.delivery(draft), Some(Delivery::Draft));
    assert_eq!(s.comment_count(&sid), 1);
    // a viewer's push is refused by the server too, whatever the client believes
    tb.pretend_role = Some(Role::Editor);
    let forced = tb.reply(t, "forced");
    let r = tb.sync();
    assert!(r.permission_lost);
    assert_eq!(tb.sync.delivery(forced), Some(Delivery::Draft));
    assert_eq!(s.comment_count(&sid), 1);
    tb.pretend_role = None;
    // editor again: the drafts stay drafts, fillers take their places, new comments merge
    change_role(&mut a, &space, &b.me, Role::Editor);
    let fresh = tb.reply(t, "back again");
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(tb.sync.delivery(fresh), Some(Delivery::Sent));
    assert_eq!(tb.sync.delivery(draft), Some(Delivery::Draft));
    let bodies: Vec<String> = ta
        .sync
        .replica()
        .thread(t)
        .unwrap()
        .comments
        .iter()
        .filter_map(|c| c.body.clone())
        .collect();
    assert_eq!(bodies, vec!["Plan", "back again"]);
    assert_eq!(ta.view(), tb.view());
    assert_eq!(tb.sync.unsent().len(), 2);
    assert!(ta.sync.quarantined().is_empty());
    // the user drops a draft
    assert!(tb.sync.discard(draft).unwrap());
    assert_eq!(tb.sync.unsent().len(), 1);
}

#[test]
fn a_viewer_writes_drafts_only() {
    let (s, a, b, nb, space) = shared_pair(Role::Viewer);
    let sid: AccountId = space.parse().unwrap();
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = ta.thread("For review");
    settle(&mut [&mut ta, &mut tb]);
    // a viewer reads comments
    assert_eq!(tb.view(), ta.view());
    let d = tb.reply(t, "can't post this");
    assert_eq!(tb.sync.delivery(d), Some(Delivery::Draft));
    tb.sync();
    assert_eq!(s.comment_count(&sid), 1);
    // and a viewer pushing directly is refused
    let frame = encode_json_batch::<CommentPushItem>(&[], &Limits::DEFAULT).unwrap();
    assert_eq!(
        tb.t.comment_push(frame).unwrap_err().code(),
        Some(ErrorCode::Forbidden)
    );
}

#[test]
fn mentions_and_replies_notify_once_each() {
    let (_s, a, b, nb, space) = shared_pair(Role::Editor);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    // A mentions B twice in one comment, and an outsider who isn't notified
    let body = format!("@[{}] look, @[{}]! and @[nobody]", b.me, b.me);
    let t = ta.thread(&body);
    ta.drop = 1;
    assert!(ta.try_sync().is_err());
    settle(&mut [&mut ta, &mut tb]);
    let (items, unread, cursor) = notifications(&mut tb.t, None).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, NotificationKind::Mention);
    assert_eq!(items[0].comment_op_id.as_str(), t.to_string());
    assert_eq!(items[0].actor, a.me);
    assert_eq!(unread, 1);
    assert!(tb.sync.verify_notification(&items[0]));
    // B replies: A, who wrote in the thread, gets a reply notification
    let r = tb.reply(t, "seen it");
    settle(&mut [&mut ta, &mut tb]);
    let (a_items, _, _) = notifications(&mut ta.t, None).unwrap();
    assert_eq!(a_items.len(), 1);
    assert_eq!(a_items[0].kind, NotificationKind::Reply);
    assert_eq!(a_items[0].comment_op_id.as_str(), r.to_string());
    assert!(ta.sync.verify_notification(&a_items[0]));
    // an edit that adds a mention notifies under the edit's id; the client maps it back
    let edit = ta.submit(CommentIntent::Edit {
        comment_id: t,
        body: format!("ok @[{}]", b.me),
    });
    settle(&mut [&mut ta, &mut tb]);
    let (b_items, _, _) = notifications(&mut tb.t, Some(&cursor)).unwrap();
    assert_eq!(b_items.len(), 1, "{b_items:?}");
    assert_eq!(b_items[0].comment_op_id.as_str(), edit.to_string());
    assert_eq!(b_items[0].thread_id.as_str(), t.to_string());
    assert_eq!(tb.sync.thread_of_op(edit), Some(t));
    assert_eq!(tb.sync.comment_of_op(edit), Some(t));
    // a resolve notifies no one
    ta.submit(CommentIntent::SetResolved {
        thread_id: t,
        resolved: true,
    });
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(notifications(&mut tb.t, None).unwrap().0.len(), 2);
    assert_eq!(notifications(&mut ta.t, None).unwrap().0.len(), 1);
    // marking read
    let read = mark_notification(&mut tb.t, &items[0].notification_id, true).unwrap();
    assert!(read.read);
    assert_eq!(notifications(&mut tb.t, None).unwrap().1, 1);
    mark_notification(&mut tb.t, &b_items[0].notification_id, true).unwrap();
    assert_eq!(notifications(&mut tb.t, None).unwrap().1, 0);
    // muting the thread stops new notifications
    let prefs = NotificationPrefs {
        muted_spaces: vec![],
        muted_threads: vec![MutedThread {
            space_id: space.parse().unwrap(),
            thread_id: ThreadId::new(t.to_string()).unwrap(),
        }],
    };
    assert_eq!(set_notification_prefs(&mut ta.t, &prefs).unwrap(), prefs);
    assert_eq!(notification_prefs(&mut ta.t).unwrap(), prefs);
    tb.reply(t, "again");
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(notifications(&mut ta.t, None).unwrap().0.len(), 1);
    // an edit naming A in the muted thread notifies no one either (it names the real thread)
    tb.submit(CommentIntent::Edit {
        comment_id: r,
        body: format!("seen it, @[{}]", a.me),
    });
    settle(&mut [&mut ta, &mut tb]);
    assert_eq!(notifications(&mut ta.t, None).unwrap().0.len(), 1);
}

#[test]
fn only_authors_edit_and_the_owner_moderates() {
    let (_s, a, b, nb, space) = shared_pair(Role::Commenter);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = tb.thread("spam spam");
    settle(&mut [&mut ta, &mut tb]);
    let mine = ta.reply(t, "owner's words");
    settle(&mut [&mut ta, &mut tb]);
    // B can't edit or delete the owner's comment: refused locally, nothing queued
    let r = tb.sync.submit(
        CommentIntent::Edit {
            comment_id: mine,
            body: "rewritten".into(),
        },
        true,
        false,
        1,
    );
    assert!(r.is_err());
    assert!(tb
        .sync
        .submit(CommentIntent::Delete { comment_id: mine }, true, false, 1)
        .is_err());
    // the owner removes B's comment
    ta.submit(CommentIntent::Delete { comment_id: t });
    settle(&mut [&mut ta, &mut tb]);
    let th = tb.sync.replica().thread(t).unwrap();
    assert_eq!(
        th.comments[0].state,
        penroam_core::comment::CommentState::RemovedByOwner
    );
    assert_eq!(ta.view(), tb.view());
}

/// Pushes a correctly sealed and signed comment operation with a side-car the client chooses.
fn push_with_sidecar(
    tb: &mut Talker,
    op: &CommentOperation,
    thread: Uuid,
    mentions: &[&AccountId],
) {
    let creds = tb.creds();
    let (h, ct) =
        penroam_sync::comments::seal(op, creds.membership.current_epoch, &creds.keys()).unwrap();
    let item = CommentPushItem {
        kind: penroam_sync_protocol::comments::CommentPushKind::Comment,
        envelope: h,
        thread_id: ThreadId::new(thread.to_string()).unwrap(),
        mentions: mentions.iter().map(|m| (*m).clone()).collect(),
    };
    let frame = encode_json_batch(&[(item, ct)], &Limits::DEFAULT).unwrap();
    let r = tb.t.comment_push(frame).unwrap();
    assert!(
        matches!(
            r.acks[0].result,
            penroam_sync_protocol::AckResult::Accepted { .. }
        ),
        "{r:?}"
    );
}

#[test]
fn notifications_the_comments_dont_back_are_not_shown() {
    use penroam_sync::comments::NotificationCheck;
    let (_s, a, b, nb, space) = shared_pair(Role::Editor);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let mine = ta.thread("A's own thread");
    settle(&mut [&mut ta, &mut tb]);
    // B's client lies in the unsigned side-car: a mention the body doesn't make ...
    let deps = tb.sync.replica().frontier();
    let ghost_id = Uuid::new_v4();
    let ghost = CommentOperation {
        id: ghost_id,
        device_id: tb.device,
        sequence: 1,
        schema_version: 1,
        causal_dependencies: deps,
        author: b.me.as_str().into(),
        moderation: false,
        at: 5,
        op: CommentOp::ThreadCreated {
            thread_id: ghost_id,
            anchor: CommentAnchor::PagePoint {
                page_id: Uuid::from_u128(7),
                x: 0.0,
                y: 0.0,
            },
            body: "nothing about anyone".into(),
        },
    };
    push_with_sidecar(&mut tb, &ghost, ghost.id, &[&a.me]);
    // ... and a reply in its own thread that claims to be in A's thread
    let id = Uuid::new_v4();
    let stray = CommentOperation {
        id,
        sequence: 2,
        at: 6,
        op: CommentOp::Replied {
            thread_id: ghost.id,
            comment_id: id,
            body: "hi".into(),
            reply_to: None,
        },
        ..ghost.clone()
    };
    push_with_sidecar(&mut tb, &stray, mine, &[]);
    let (items, _, _) = notifications(&mut ta.t, None).unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    // not pulled yet: deferred
    for n in &items {
        assert_eq!(ta.sync.check_notification(n), NotificationCheck::Pending);
        assert!(!ta.sync.verify_notification(n));
    }
    assert!(ta.sync.forged_notifications().is_empty());
    ta.sync();
    let checks: Vec<NotificationCheck> = items
        .iter()
        .map(|n| ta.sync.check_notification(n))
        .collect();
    assert!(
        matches!(checks[0], NotificationCheck::Forged(w) if w.contains("doesn't mention")),
        "{checks:?}"
    );
    assert!(
        matches!(checks[1], NotificationCheck::Forged(w) if w.contains("another thread")),
        "{checks:?}"
    );
    for n in &items {
        assert!(!ta.sync.verify_notification(n));
    }
    assert_eq!(ta.sync.forged_notifications().len(), 2);
    // the comments themselves are fine and merged
    assert_eq!(ta.sync.threads().len(), 2);
}

/// Invites `who` (a new person with their own device) and lets them join `space`.
fn join_new(s: &Arc<InMemoryServer>, a: &mut Dev, space: &str, name: &str, role: Role) -> Dev {
    let id = person(s, name);
    let mut c = Dev::new(s, &id, &format!("{name}'s phone"));
    a.account
        .transport_mut()
        .shared_invite(
            &space.parse().unwrap(),
            &CreateInvitationRequest {
                email: format!("{name}@example.org"),
                role,
                idempotency_key: Uuid::new_v4().to_string(),
            },
        )
        .unwrap();
    let _ = c.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
    let inv = c.spaces.invitations[0].invitation_id.clone();
    c.with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv))
        .unwrap();
    for _ in 0..3 {
        a.sync();
        c.sync();
    }
    c
}

#[test]
fn an_erased_members_comments_still_verify_and_show_as_erased() {
    let (s, mut a, b, nb, space) = shared_pair(Role::Editor);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = tb.thread("Started by Ben");
    settle(&mut [&mut ta, &mut tb]);
    ta.reply(t, "Ann answers");
    settle(&mut [&mut ta, &mut tb]);
    tb.reply(t, "Ben again");
    settle(&mut [&mut ta, &mut tb]);
    // Ben's account is erased: the server now names a tombstone for his comments
    let tomb = s.erase_account(&b.me).unwrap();
    // a new member's device pulls everything afterwards
    let c = join_new(&s, &mut a, &space, "cat", Role::Commenter);
    let mut tc = Talker::new(&c, &space, nb);
    let r = tc.sync();
    assert_eq!(r.quarantined, 0, "{:?}", tc.sync.quarantined());
    let th = tc
        .sync
        .threads()
        .into_iter()
        .find(|x| x.thread_id == t)
        .unwrap();
    assert_eq!(th.comments.len(), 3);
    assert_eq!(th.created_by, tomb.as_str());
    let authors: Vec<&str> = th.comments.iter().map(|c| c.author.as_str()).collect();
    assert_eq!(authors, vec![tomb.as_str(), a.me.as_str(), tomb.as_str()]);
    assert!(!format!("{:?}", tc.sync.threads()).contains(b.me.as_str()));
    // Ann's device, which pulled before the erasure, still merges with the newcomer
    tc.reply(t, "Cat joins in");
    ta.sync();
    tc.sync();
    ta.sync();
    assert_eq!(ta.sync.replica().thread(t).unwrap().comments.len(), 4);
    assert!(ta.sync.quarantined().is_empty());
    assert_eq!(tc.sync.replica().thread(t).unwrap().comments.len(), 4);
}

#[test]
fn an_erased_attribution_cannot_claim_a_current_members_comment() {
    use penroam_sync::comments::seal;
    let (s, a, b, nb, space) = shared_pair(Role::Editor);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    settle(&mut [&mut ta, &mut tb]);
    // Ben's client signs a comment that claims Ann wrote it (refused while Ben exists) ...
    let creds = tb.creds();
    let id = Uuid::new_v4();
    let op = CommentOperation {
        id,
        device_id: tb.device,
        sequence: 1,
        schema_version: 1,
        causal_dependencies: tb.sync.replica().frontier(),
        author: a.me.as_str().into(),
        moderation: false,
        at: 1,
        op: CommentOp::ThreadCreated {
            thread_id: id,
            anchor: CommentAnchor::PagePoint {
                page_id: Uuid::from_u128(7),
                x: 0.0,
                y: 0.0,
            },
            body: "words put in Ann's mouth".into(),
        },
    };
    let (h, ct) = seal(&op, creds.membership.current_epoch, &creds.keys()).unwrap();
    let item = CommentPushItem {
        kind: penroam_sync_protocol::comments::CommentPushKind::Comment,
        envelope: h,
        thread_id: ThreadId::new(id.to_string()).unwrap(),
        mentions: vec![],
    };
    tb.t.comment_push(encode_json_batch(&[(item, ct)], &Limits::DEFAULT).unwrap())
        .unwrap();
    // ... and still refused after Ben is erased, for a device that pulls only now
    s.erase_account(&b.me).unwrap();
    ta.restart();
    let fresh = tempfile::tempdir().unwrap();
    ta.sync = CommentSync::open(fresh.path(), ta.device, &a.me).unwrap();
    let r = ta.sync();
    assert_eq!(r.quarantined, 1, "{r:?}");
    assert!(ta.sync.quarantined()[0].reason.contains("current member"));
    assert!(ta.sync.threads().is_empty());
}

#[test]
fn notifications_from_an_erased_member_are_still_genuine() {
    use penroam_sync::comments::NotificationCheck;
    let (s, a, b, nb, space) = shared_pair(Role::Editor);
    let mut ta = Talker::new(&a, &space, nb);
    let mut tb = Talker::new(&b, &space, nb);
    let t = ta.thread("Ann's question");
    settle(&mut [&mut ta, &mut tb]);
    tb.reply(t, &format!("@[{}] here's my answer", a.me));
    settle(&mut [&mut ta, &mut tb]);
    let tomb = s.erase_account(&b.me).unwrap();
    // the server now names Ben's tombstone as the actor (one mention; no reply for the mentioned)
    let (items, _, _) = notifications(&mut ta.t, None).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items.iter().all(|n| n.actor == tomb));
    // Ann's device pulled the reply before the erasure: genuine, not forged
    for n in &items {
        assert_eq!(ta.sync.check_notification(n), NotificationCheck::Genuine);
    }
    // a reinstalled device pulls it afterwards and learns the tombstone: genuine too
    let fresh = tempfile::tempdir().unwrap();
    ta.sync = CommentSync::open(fresh.path(), ta.device, &a.me).unwrap();
    ta.sync();
    for n in &items {
        assert!(
            ta.sync.verify_notification(n),
            "{:?}",
            ta.sync.check_notification(n)
        );
    }
    // but a different tombstone for that author is still refused
    let mut wrong = items[0].clone();
    wrong.actor = s.erased_account_id(&"someone-else".parse().unwrap());
    assert!(matches!(
        ta.sync.check_notification(&wrong),
        NotificationCheck::Forged(_)
    ));
    assert_eq!(ta.sync.forged_notifications().len(), 0);
}
