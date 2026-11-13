//! Comment streams over HTTP against the development test server, with the sessions the app uses:
//! a device session on the space for the comment routes, the member's account session for
//! notifications.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{add, stroke};
use penroam_core::comment::{CommentAnchor, ThreadStatus};
use penroam_core::PageDefaults;
use penroam_storage::Library;
use penroam_sync::comments::{
    mark_notification, notification_prefs, notifications, set_notification_prefs, CommentIntent,
    CommentSync, CommentTransport, Delivery, HttpCommentTransport, SpaceCredentials, SpaceRoster,
};
use penroam_sync::shared::{SpaceAccess, SpaceSet};
use penroam_sync::transport::HttpTransport;
use penroam_sync::vault::Scope;
use penroam_sync::{FileSecretStore, SecretStore, Setup, SyncEngine, SyncTransport};
use penroam_sync_protocol::comments::{MutedThread, NotificationKind, NotificationPrefs};
use penroam_sync_protocol::{
    AccountId, CreateInvitationRequest, ErrorCode, Limits, Role, ThreadId,
};
use penroam_sync_testkit::{http, InMemoryServer};
use uuid::Uuid;

type Engine = SyncEngine<HttpTransport>;

struct Dev {
    lib: Mutex<Library>,
    account: Engine,
    spaces: SpaceSet<HttpTransport>,
    root: PathBuf,
    url: String,
    me: String,
    _dir: tempfile::TempDir,
}

fn secrets_at(root: &std::path::Path) -> Box<dyn SecretStore> {
    Box::new(FileSecretStore::new(root.join("sync-secrets")))
}

impl Dev {
    fn new(url: &str, name: &str) -> Dev {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let lib = Library::open(&root).unwrap();
        let email = format!("{name}@example.org");
        let (token, acct) =
            HttpTransport::test_session_as(url, name, None, None, Some(&email)).unwrap();
        let mut account = Engine::connect(
            Setup {
                library_root: root.clone(),
                scope: Scope::Account,
                device_id: None,
                server_url: url.into(),
                account_id: acct.clone(),
                label: name.into(),
            },
            HttpTransport::new(url, token),
            secrets_at(&root),
        )
        .unwrap();
        let (dt, _) = HttpTransport::test_session(url, &acct, Some(account.device_id())).unwrap();
        account.transport_mut().set_token(dt);
        Dev {
            lib: Mutex::new(lib),
            account,
            spaces: SpaceSet::new(),
            root,
            url: url.into(),
            me: acct,
            _dir: dir,
        }
    }

    fn with_access<R>(
        &mut self,
        f: impl FnOnce(
            &mut Engine,
            &mut SpaceSet<HttpTransport>,
            &Mutex<Library>,
            &mut SpaceAccess<'_, HttpTransport>,
        ) -> R,
    ) -> R {
        let url = self.url.clone();
        let me = self.me.clone();
        let root = self.root.clone();
        let mut connect =
            |acc: &mut Engine, id: &AccountId| -> penroam_sync::engine::Result<HttpTransport> {
                let t = acc.transport_mut().shared_session(id)?;
                Ok(HttpTransport::new(&url, t.access_token))
            };
        let secrets = move || Ok(secrets_at(&root));
        let url2 = self.url.clone();
        let mut upgrade =
            |_: &mut Engine, space: &mut Engine| -> penroam_sync::engine::Result<()> {
                let (t, _) = HttpTransport::test_session_as(
                    &url2,
                    &space.store.state.account_id,
                    Some(space.device_id()),
                    Some(&me),
                    None,
                )?;
                space.transport_mut().set_token(t);
                Ok(())
            };
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
            spaces.refresh(acc, lib).unwrap();
            spaces.reconcile(acc, lib, access)
        });
        assert!(self.spaces.errors.is_empty(), "{:?}", self.spaces.errors);
    }
}

/// A member's comment stream over HTTP, wired from the space engine's device session.
struct Talker {
    sync: CommentSync,
    /// Device session on the space (comment routes).
    space_t: HttpCommentTransport,
    /// The member's account session (notification routes).
    account_t: HttpCommentTransport,
    space: String,
    nb: Uuid,
    at: i64,
    _dir: tempfile::TempDir,
}

impl Talker {
    fn new(d: &mut Dev, space: &str, nb: Uuid) -> Talker {
        let engine = d.spaces.engine(space).unwrap();
        let device = Uuid::parse_str(engine.device_id()).unwrap();
        let space_token = engine.transport_mut().token().to_string();
        let account_token = d.account.transport_mut().token().to_string();
        let dir = tempfile::tempdir().unwrap();
        Talker {
            sync: CommentSync::open(dir.path(), device, &d.me.parse().unwrap()).unwrap(),
            space_t: HttpCommentTransport::new(&d.url, space_token),
            account_t: HttpCommentTransport::new(&d.url, account_token),
            space: space.into(),
            nb,
            at: 1_700_000_000_000,
            _dir: dir,
        }
    }

    fn context(&self, d: &mut Dev) -> (SpaceCredentials, SpaceRoster) {
        let root = d.root.clone();
        let engine = d.spaces.engine(&self.space).unwrap();
        let creds = SpaceCredentials::load(
            engine.transport_mut(),
            &*secrets_at(&root),
            &self.space,
            self.nb,
        )
        .unwrap();
        let list = d
            .account
            .transport_mut()
            .shared_members(&self.space.parse().unwrap())
            .unwrap();
        (creds, SpaceRoster::from_member_list(&list).unwrap())
    }

    fn sync(&mut self, d: &mut Dev) {
        let (creds, roster) = self.context(d);
        let r = self
            .sync
            .sync(&mut self.space_t, &creds.keys(), &roster, &Limits::DEFAULT)
            .unwrap();
        assert_eq!(r.quarantined, 0, "{r:?}");
    }

    fn submit(&mut self, d: &mut Dev, intent: CommentIntent) -> Uuid {
        let (_, roster) = self.context(d);
        self.at += 1000;
        self.sync
            .submit(intent, roster.may_comment(), roster.is_owner(), self.at)
            .unwrap()
    }
}

#[test]
fn comments_and_notifications_work_over_http_with_device_sessions() {
    let server = Arc::new(InMemoryServer::new());
    let handle = http::spawn("127.0.0.1:0", server.clone()).unwrap();
    let url = handle.base_url.clone();
    let mut a = Dev::new(&url, "ann");
    let mut b = Dev::new(&url, "ben");
    let (nb, page) = a
        .lib
        .lock()
        .unwrap()
        .create_notebook("Over the wire", PageDefaults::default(), None)
        .unwrap();
    let (nb, page) = (nb.id, page.id);
    add(
        &mut a.lib.lock().unwrap(),
        nb,
        page,
        vec![stroke(1, 1.0, "#000000")],
    );
    a.sync();
    let space = a
        .with_access(|acc, spaces, lib, access| spaces.share(acc, lib, access, nb))
        .unwrap();
    a.sync();
    a.account
        .transport_mut()
        .shared_invite(
            &space.parse().unwrap(),
            &CreateInvitationRequest {
                email: "ben@example.org".into(),
                role: Role::Commenter,
                idempotency_key: Uuid::new_v4().to_string(),
            },
        )
        .unwrap();
    b.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib))
        .unwrap();
    let inv = b.spaces.invitations[0].invitation_id.clone();
    b.with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv))
        .unwrap();
    for _ in 0..3 {
        a.sync();
        b.sync();
    }

    let mut ta = Talker::new(&mut a, &space, nb);
    let mut tb = Talker::new(&mut b, &space, nb);
    // A starts a thread mentioning B; B (a commenter) replies
    let t = ta.submit(
        &mut a,
        CommentIntent::CreateThread {
            anchor: CommentAnchor::PagePoint {
                page_id: page,
                x: -40.0,
                y: 12.5,
            },
            body: format!("@[{}] is this right?", b.me),
        },
    );
    ta.sync(&mut a);
    assert_eq!(ta.sync.delivery(t), Some(Delivery::Sent));
    tb.sync(&mut b);
    let r = tb.submit(
        &mut b,
        CommentIntent::Reply {
            thread_id: t,
            body: "looks fine".into(),
            reply_to: Some(t),
        },
    );
    tb.sync(&mut b);
    // A has seen the reply before resolving (a concurrent resolve would leave it open)
    ta.sync(&mut a);
    ta.submit(
        &mut a,
        CommentIntent::SetResolved {
            thread_id: t,
            resolved: true,
        },
    );
    for _ in 0..2 {
        ta.sync(&mut a);
        tb.sync(&mut b);
    }
    let (va, vb) = (ta.sync.threads(), tb.sync.threads());
    assert_eq!(va, vb);
    assert_eq!(va[0].comments.len(), 2);
    assert_eq!(va[0].comments[1].author, b.me);
    assert_eq!(va[0].status, ThreadStatus::Resolved);
    assert_eq!(server.comment_count(&space.parse().unwrap()), 3);

    // notifications on the account sessions
    let (items, unread, _) = notifications(&mut tb.account_t, None).unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, NotificationKind::Mention);
    assert_eq!(items[0].comment_op_id.as_str(), t.to_string());
    assert_eq!(unread, 1);
    let (a_items, _, _) = notifications(&mut ta.account_t, None).unwrap();
    assert_eq!(a_items.len(), 1);
    assert_eq!(a_items[0].kind, NotificationKind::Reply);
    assert_eq!(a_items[0].comment_op_id.as_str(), r.to_string());
    assert!(
        mark_notification(&mut tb.account_t, &items[0].notification_id, true)
            .unwrap()
            .read
    );
    assert_eq!(notifications(&mut tb.account_t, None).unwrap().1, 0);
    let prefs = NotificationPrefs {
        muted_spaces: vec![space.parse().unwrap()],
        muted_threads: vec![MutedThread {
            space_id: space.parse().unwrap(),
            thread_id: ThreadId::new(t.to_string()).unwrap(),
        }],
    };
    assert_eq!(
        set_notification_prefs(&mut tb.account_t, &prefs).unwrap(),
        prefs
    );
    assert_eq!(notification_prefs(&mut tb.account_t).unwrap(), prefs);

    // the comment routes refuse the account session; the notification routes a space session
    let e = ta
        .account_t
        .comment_push(
            penroam_sync_protocol::framing::encode_json_batch::<
                penroam_sync_protocol::comments::CommentPushItem,
            >(&[], &Limits::DEFAULT)
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::Forbidden), "{e:?}");
    assert!(ta.space_t.notification_prefs_get().is_err());
    handle.shutdown();
}
