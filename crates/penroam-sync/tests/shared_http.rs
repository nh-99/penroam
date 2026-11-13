//! Shared notebooks over HTTP against the development test server, with real sessions: an
//! account session on a space is traded for a device session before syncing, as the app does.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{add, ink_count, stroke};
use penroam_core::PageDefaults;
use penroam_storage::Library;
use penroam_sync::shared::{SharedState, SpaceAccess, SpaceSet};
use penroam_sync::transport::HttpTransport;
use penroam_sync::vault::{NotebookOwners, Scope};
use penroam_sync::{FileSecretStore, SecretStore, Setup, SyncEngine, SyncTransport};
use penroam_sync_protocol::{AccountId, CreateInvitationRequest, Role};
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

    fn state(&self, nb: Uuid) -> Option<SharedState> {
        NotebookOwners::load(&self.root)
            .unwrap()
            .shared_entry(&nb)
            .map(|e| e.state)
    }
}

#[test]
fn a_shared_notebook_syncs_over_http_with_device_sessions() {
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
                role: Role::Editor,
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
    assert_eq!(b.state(nb), Some(SharedState::Active));
    assert_eq!(ink_count(&mut b.lib.lock().unwrap(), nb), 1);

    // both ways
    add(
        &mut b.lib.lock().unwrap(),
        nb,
        page,
        vec![stroke(2, 2.0, "#000000")],
    );
    add(
        &mut a.lib.lock().unwrap(),
        nb,
        page,
        vec![stroke(3, 3.0, "#000000")],
    );
    for _ in 0..3 {
        a.sync();
        b.sync();
    }
    assert_eq!(ink_count(&mut a.lib.lock().unwrap(), nb), 3);
    assert_eq!(ink_count(&mut b.lib.lock().unwrap(), nb), 3);

    // removed: access removed on B, nothing lost there
    a.account
        .transport_mut()
        .shared_remove_member(&space.parse().unwrap(), &b.me.parse().unwrap())
        .unwrap();
    add(
        &mut b.lib.lock().unwrap(),
        nb,
        page,
        vec![stroke(4, 4.0, "#000000")],
    );
    b.sync();
    assert_eq!(b.state(nb), Some(SharedState::AccessRemoved));
    assert_eq!(ink_count(&mut b.lib.lock().unwrap(), nb), 4);
    a.sync();
    assert_eq!(ink_count(&mut a.lib.lock().unwrap(), nb), 3);
    handle.shutdown();
}

#[test]
fn the_live_channel_over_a_websocket_hints_commits_and_ends_on_removal() {
    use penroam_sync::live::{LiveClient, LiveEvent, LiveState, WsDialer};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    let server = Arc::new(InMemoryServer::new());
    let handle = http::spawn("127.0.0.1:0", server.clone()).unwrap();
    let url = handle.base_url.clone();
    let mut a = Dev::new(&url, "ann");
    let mut b = Dev::new(&url, "ben");
    let (nb, page) = a
        .lib
        .lock()
        .unwrap()
        .create_notebook("Live", PageDefaults::default(), None)
        .unwrap();
    let (nb, page) = (nb.id, page.id);
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
                role: Role::Editor,
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
    // ben's live channel, with his device session on the space
    let device = b.spaces.engine(&space).unwrap().device_id().to_string();
    let (token, _) =
        HttpTransport::test_session_as(&url, &space, Some(&device), Some(&b.me), None).unwrap();
    let root = penroam_sync::shared::load_root(secrets_at(&b.root).as_ref(), &space)
        .unwrap()
        .unwrap();
    let mut live = LiveClient::new(
        space.parse().unwrap(),
        b.me.parse().unwrap(),
        device.parse().unwrap(),
        root.awareness_key(),
        true,
        Box::new(WsDialer {
            base_url: url.clone(),
            token: Arc::new(StdMutex::new(token)),
        }),
    );
    let until = |live: &mut LiveClient, pred: &dyn Fn(&LiveEvent) -> bool| -> Vec<LiveEvent> {
        let mut seen = Vec::new();
        for _ in 0..200 {
            let ev = live.step(Duration::from_millis(20));
            let hit = ev.iter().any(pred);
            seen.extend(ev);
            if hit {
                break;
            }
        }
        seen
    };
    let ev = until(&mut live, &|e| matches!(e, LiveEvent::CatchUp(_)));
    assert_eq!(live.state(), LiveState::Live, "{ev:?}");
    // a commit by ann reaches ben as a hint, and he pulls
    add(
        &mut a.lib.lock().unwrap(),
        nb,
        page,
        vec![stroke(1, 1.0, "#000000")],
    );
    a.sync();
    let ev = until(&mut live, &|e| matches!(e, LiveEvent::CatchUp(_)));
    assert!(
        ev.iter().any(|e| matches!(e, LiveEvent::CatchUp(_))),
        "{ev:?}"
    );
    b.sync();
    assert_eq!(ink_count(&mut b.lib.lock().unwrap(), nb), 1);
    // ann removes ben: his socket is closed and the client stops
    a.account
        .transport_mut()
        .shared_remove_member(&space.parse().unwrap(), &b.me.parse().unwrap())
        .unwrap();
    let ev = until(&mut live, &|e| matches!(e, LiveEvent::AccessRemoved));
    assert!(ev.contains(&LiveEvent::AccessRemoved), "{ev:?}");
    assert_eq!(live.state(), LiveState::Disabled);
    handle.shutdown();
}
