//! Shared notebooks end to end against the public test server: sharing an existing notebook
//! (history and all), invitations, offline edits by several people, conflicts, viewers, access
//! loss, crashes while sharing, and several devices per person.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{account, add, edit, ink_count, stroke, text, view};
use penroam_core::operation::DocumentOperation;
use penroam_core::{Element, OpKind, PageDefaults};
use penroam_storage::Library;
use penroam_sync::crypto::RootSecret;
use penroam_sync::shared::{
    self, NotebookSyncState, ShareStep, SharedState, SpaceAccess, SpaceSet,
};
use penroam_sync::transport::InProcessTransport;
use penroam_sync::vault::{NotebookOwners, Scope};
use penroam_sync::{FileSecretStore, SecretStore, Setup, SyncEngine, SyncError, SyncTransport};
use penroam_sync_protocol::framing::{encode_batch, FrameItem};
use penroam_sync_protocol::{
    AccountId, AccountKeyDocument, ChangeRoleRequest, CreateInvitationRequest, DeviceId, ErrorCode,
    InvitationId, Limits, PullOpsRequest, Role,
};
use penroam_sync_testkit::{Caller, InMemoryServer, RequestError};
use uuid::Uuid;

type Engine = SyncEngine<InProcessTransport>;

/// One device: its own library, account engine and shared notebook engines.
struct Dev {
    lib: Mutex<Library>,
    account: Engine,
    spaces: SpaceSet<InProcessTransport>,
    root: PathBuf,
    server: Arc<InMemoryServer>,
    me: AccountId,
    /// Fault knobs copied into every new space transport.
    faults: (bool, bool),
    _dir: tempfile::TempDir,
}

fn secrets_at(root: &std::path::Path) -> Box<dyn SecretStore> {
    Box::new(FileSecretStore::new(root.join("sync-secrets")))
}

fn setup(root: &std::path::Path, acct: &AccountId, label: &str) -> Setup {
    Setup {
        library_root: root.to_path_buf(),
        scope: Scope::Account,
        device_id: None,
        server_url: "in-process".into(),
        account_id: acct.as_str().into(),
        label: label.into(),
    }
}

/// An account with a verified email; the first device creates its keys.
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
            setup(&root, acct, label),
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
            faults: (false, false),
            _dir: dir,
        }
    }

    fn lib(&self) -> std::sync::MutexGuard<'_, Library> {
        self.lib.lock().unwrap()
    }

    /// What the app passes the sharing functions.
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
        let offline = self.account.transport_mut().offline;
        let (corrupt_blobs, corrupt_checkpoints) = self.faults;
        let mut connect = move |acc: &mut Engine,
                                id: &AccountId|
              -> penroam_sync::engine::Result<InProcessTransport> {
            acc.transport_mut().shared_session(id)?;
            let mut t = InProcessTransport::for_space(
                server.clone(),
                id.clone(),
                me.clone(),
                DeviceId::new(acc.device_id()).unwrap(),
            );
            t.offline = offline;
            t.corrupt_blob_gets = corrupt_blobs;
            t.corrupt_checkpoints = corrupt_checkpoints;
            Ok(t)
        };
        let secrets = move || Ok(secrets_at(&root));
        // the in-process server makes no difference between account and device sessions
        let mut upgrade = |_: &mut Engine, _: &mut Engine| Ok(());
        let mut access = SpaceAccess {
            connect: &mut connect,
            secrets: &secrets,
            upgrade: &mut upgrade,
        };
        f(&mut self.account, &mut self.spaces, &self.lib, &mut access)
    }

    /// One pass of the app's worker: account, then the server's lists, then every space.
    fn sync(&mut self) -> Vec<penroam_sync::ReconcileReport> {
        let _ = self.account.reconcile(&self.lib);
        self.with_access(|acc, spaces, lib, access| {
            let _ = spaces.refresh(acc, lib);
            spaces.reconcile(acc, lib, access)
        })
    }

    fn share(&mut self, nb: Uuid) -> String {
        self.with_access(|acc, spaces, lib, access| spaces.share(acc, lib, access, nb))
            .unwrap()
    }

    fn invite(&mut self, space: &str, email: &str, role: Role) -> InvitationId {
        self.account
            .transport_mut()
            .shared_invite(
                &space.parse().unwrap(),
                &CreateInvitationRequest {
                    email: email.into(),
                    role,
                    idempotency_key: Uuid::new_v4().to_string(),
                },
            )
            .unwrap()
            .invitation_id
    }

    fn accept(&mut self) -> Uuid {
        let _ = self.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
        let inv = self.spaces.invitations[0].invitation_id.clone();
        let (_, nb, _) = self
            .with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv))
            .unwrap();
        nb
    }

    fn offline(&mut self, off: bool) {
        self.account.transport_mut().offline = off;
        for e in &mut self.spaces.engines {
            e.transport_mut().offline = off;
        }
    }

    fn owners(&self) -> NotebookOwners {
        NotebookOwners::load(&self.root).unwrap()
    }

    fn state(&self, nb: Uuid) -> Option<SharedState> {
        self.owners().shared_entry(&nb).map(|e| e.state)
    }

    fn sync_state(&self, nb: Uuid) -> NotebookSyncState {
        let lib = self.lib();
        self.spaces.sync_state(&lib, &self.owners(), nb)
    }

    /// This device's id in a space (from the keys it keeps for it).
    fn space_keys_id(&self, space: &str) -> String {
        device_keys(self, space).device_id
    }

    fn space_engine(&mut self, space: &str) -> &mut Engine {
        self.spaces.engine(space).expect("space engine")
    }

    fn notebook_view(&self, nb: Uuid) -> serde_json::Value {
        let mut lib = self.lib();
        view(&mut lib)
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == serde_json::json!(nb))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }
}

/// A notebook with a page and some strokes, synced to the account.
fn notebook_with_strokes(d: &mut Dev, title: &str, n: u64) -> (Uuid, Uuid) {
    let (nb, page) = d
        .lib()
        .create_notebook(title, PageDefaults::default(), None)
        .unwrap();
    for i in 0..n {
        add(
            &mut d.lib(),
            nb.id,
            page.id,
            vec![stroke(i, 1.0, "#000000")],
        );
    }
    d.sync();
    (nb.id, page.id)
}

fn settle(devs: &mut [&mut Dev]) {
    for _ in 0..4 {
        for d in devs.iter_mut() {
            d.sync();
        }
    }
}

/// A shares a notebook with B (editor unless said otherwise); B accepts and has it.
fn shared_pair(role: Role) -> (Arc<InMemoryServer>, Dev, Dev, Uuid, Uuid, String) {
    let s = Arc::new(InMemoryServer::new());
    let (a_id, b_id) = (person(&s, "ann"), person(&s, "ben"));
    let mut a = Dev::new(&s, &a_id, "ann's tablet");
    let mut b = Dev::new(&s, &b_id, "ben's laptop");
    let (nb, page) = notebook_with_strokes(&mut a, "Field notes", 3);
    let space = a.share(nb);
    a.sync();
    a.invite(&space, "Ben@Example.org", role);
    assert_eq!(b.accept(), nb);
    settle(&mut [&mut a, &mut b]);
    (s, a, b, nb, page, space)
}

#[test]
fn a_shares_with_b_who_sees_everything_and_c_sees_nothing() {
    let (s, mut a, b, nb, _, space) = shared_pair(Role::Editor);
    assert_eq!(b.state(nb), Some(SharedState::Active));
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    assert_eq!(ink_count(&mut b.lib(), nb), 3);
    // the notebook kept its id, and A's account engine leaves it to the space now
    assert_eq!(a.owners().scope_of(&nb), Scope::Shared(space.clone()));
    assert_eq!(b.owners().scope_of(&nb), Scope::Shared(space.clone()));
    assert_eq!(a.sync_state(nb), NotebookSyncState::Synced);
    assert_eq!(b.sync_state(nb), NotebookSyncState::Synced);
    // B's own account holds nothing of it
    assert_eq!(s.op_count(&b.me), 0);

    // C guesses the id: nothing, and the same answer as for a space that doesn't exist
    let c = person(&s, "cat");
    let sid: AccountId = space.parse().unwrap();
    let probe = |id: &AccountId| match s.shared_key_get(&c, id) {
        Err(RequestError::Api(b)) => b.code,
        other => panic!("{other:?}"),
    };
    assert_eq!(probe(&sid), ErrorCode::NotFound);
    assert_eq!(
        probe(&"no-such-space".parse().unwrap()),
        ErrorCode::NotFound
    );
    let pulled = s.pull(
        Caller::member(&sid, &c),
        &DeviceId::new("c-device").unwrap(),
        &PullOpsRequest {
            cursor: None,
            limit: 10,
        },
    );
    assert!(matches!(pulled, Err(RequestError::Api(b)) if b.code == ErrorCode::NotFound));
    a.sync();
}

#[test]
fn independent_offline_strokes_merge_without_duplicates() {
    let (s, mut a, mut b, nb, page, _) = shared_pair(Role::Editor);
    a.offline(true);
    b.offline(true);
    for i in 10..15 {
        add(&mut a.lib(), nb, page, vec![stroke(i, 100.0, "#aa0000")]);
    }
    for i in 20..24 {
        add(&mut b.lib(), nb, page, vec![stroke(i, 200.0, "#0000aa")]);
    }
    a.sync();
    b.sync();
    assert_eq!(b.sync_state(nb), NotebookSyncState::WaitingToUpload);
    a.offline(false);
    b.offline(false);
    // responses get lost: every upload is retried, and the server answers duplicates
    s.drop_next_responses(3);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib(), nb), 3 + 5 + 4);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    // more retries change nothing
    s.drop_next_responses(2);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib(), nb), 12);
    assert_eq!(b.sync_state(nb), NotebookSyncState::Synced);
}

#[test]
fn concurrent_text_edits_keep_both_versions_until_resolved() {
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(&mut a.lib(), nb, page, vec![text(0x77, "agenda")]);
    settle(&mut [&mut a, &mut b]);
    a.offline(true);
    b.offline(true);
    edit(
        &mut a.lib(),
        nb,
        page,
        OpKind::ReplaceElements {
            elements: vec![text(0x77, "agenda: budget")],
        },
    );
    edit(
        &mut b.lib(),
        nb,
        page,
        OpKind::ReplaceElements {
            elements: vec![text(0x77, "agenda: hiring")],
        },
    );
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    let conflicts = b.space_engine(&space).conflicts(nb).unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    let c = &conflicts[0];
    let mut values: Vec<String> = std::iter::once(&c.winner_value)
        .chain(c.alternates.iter().map(|x| &x.value))
        .map(|v| v["text"].as_str().unwrap().to_string())
        .collect();
    values.sort();
    assert_eq!(values, ["agenda: budget", "agenda: hiring"]);
    assert_eq!(a.space_engine(&space).conflicts(nb).unwrap().len(), 1);
    assert_eq!(b.sync_state(nb), NotebookSyncState::NeedsResolution);
    // B picks the alternate (the version not shown); everyone converges on it
    let pick = c.alternates[0].op_id;
    let chosen = c.alternates[0].value["text"].as_str().unwrap().to_string();
    {
        let mut lib = b.lib.lock().unwrap();
        b.spaces
            .engine(&space)
            .unwrap()
            .resolve_conflict(&mut lib, nb, &c.target, pick)
            .unwrap();
    }
    settle(&mut [&mut a, &mut b]);
    assert!(a.space_engine(&space).conflicts(nb).unwrap().is_empty());
    let texts = |d: &Dev| {
        let p = d.lib().load_page(nb, page).unwrap();
        p.elements
            .iter()
            .find_map(|e| match e {
                Element::Text(t) if t.id == Uuid::from_u128(0x77) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(texts(&a), chosen);
    assert_eq!(texts(&b), chosen);
}

#[test]
fn a_viewer_uploads_nothing_and_forged_writes_are_refused() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Viewer);
    let sid: AccountId = space.parse().unwrap();
    let before = s.op_count(&sid);
    // a change that got past the app's guard (the library itself has no roles)
    add(&mut b.lib(), nb, page, vec![stroke(90, 300.0, "#00aa00")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(s.op_count(&sid), before, "a viewer's engine never uploads");
    assert_eq!(ink_count(&mut a.lib(), nb), 3);
    // the edit stays on B's device, recoverable, and is never shown as on its way
    assert_eq!(b.space_engine(&space).unsent_ops().unwrap().len(), 1);
    assert_eq!(b.sync_state(nb), NotebookSyncState::LocalSaved);
    // calling the engine's upload directly, or sending a hand-made envelope: the server refuses
    let mut report = penroam_sync::ReconcileReport::default();
    let err = b
        .space_engine(&space)
        .push_for_tests(&mut report)
        .unwrap_err();
    assert!(
        matches!(err, SyncError::Transport(ref t) if t.code() == Some(ErrorCode::Forbidden)),
        "{err}"
    );
    let sealed = b.space_engine(&space).seal_pending_for_tests().unwrap();
    let items: Vec<FrameItem> = sealed
        .into_iter()
        .map(|(h, ct)| FrameItem {
            header_json: serde_json::to_vec(&h).unwrap(),
            ciphertext: ct,
        })
        .collect();
    let frame = encode_batch(&items, &Limits::DEFAULT).unwrap();
    let dev = DeviceId::new(b.account.device_id()).unwrap();
    let pushed = s.push(Caller::member(&sid, &b.me), &dev, &frame);
    assert!(
        matches!(pushed, Err(RequestError::Api(ref e)) if e.code == ErrorCode::Forbidden),
        "{pushed:?}"
    );
    assert_eq!(s.op_count(&sid), before);
}

#[test]
fn removing_a_member_keeps_their_unsent_edits_and_uploads_nothing_more() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    b.offline(true);
    for i in 40..43 {
        add(&mut b.lib(), nb, page, vec![stroke(i, 400.0, "#123456")]);
    }
    b.sync();
    // A removes B while B is offline with queued edits
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    let before = s.op_count(&sid);
    b.offline(false);
    b.sync();
    b.sync();
    assert_eq!(b.state(nb), Some(SharedState::AccessRemoved));
    assert_eq!(b.sync_state(nb), NotebookSyncState::AccessRemoved);
    assert_eq!(s.op_count(&sid), before, "nothing uploaded after removal");
    assert!(
        b.spaces.engine(&space).is_none(),
        "the space's engine stopped"
    );
    // the notebook and the edits are still there, read-only
    assert_eq!(ink_count(&mut b.lib(), nb), 6);
    let access = shared::notebook_access(&b.root, nb).unwrap();
    assert_eq!(access.read_only_reason(), Some("accessRemoved"));
    // a private copy has everything, unsent edits included, under a new id
    let copy = shared::keep_private_copy(&mut b.lib(), nb).unwrap();
    assert_ne!(copy.id, nb);
    assert_eq!(ink_count(&mut b.lib(), copy.id), 6);
    assert_eq!(b.owners().scope_of(&copy.id), Scope::Account);
    // the copy goes to B's own account; the shared notebook never does
    settle(&mut [&mut b]);
    assert!(s.op_count(&b.me) > 0);
    // and B can then remove the shared one from this device
    {
        let root = b.root.clone();
        let mut lib = b.lib.lock().unwrap();
        shared::remove_local(&mut lib, secrets_at(&root).as_ref(), &space).unwrap();
    }
    assert!(b.lib().notebook(nb).is_err());
    assert!(!Scope::Shared(space.clone()).dir(&b.root).exists());
    b.sync();
    assert!(b.lib().notebook(copy.id).is_ok());
    assert_eq!(s.op_count(&sid), before);
}

#[test]
fn a_member_lowered_to_viewer_keeps_queued_edits_without_sending_them() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    b.offline(true);
    add(&mut b.lib(), nb, page, vec![stroke(60, 500.0, "#654321")]);
    b.sync();
    let members = a.account.transport_mut().shared_members(&sid).unwrap();
    let ben = members
        .members
        .iter()
        .find(|m| m.account_id == b.me)
        .unwrap();
    a.account
        .transport_mut()
        .shared_change_role(
            &sid,
            &b.me,
            &ChangeRoleRequest {
                role: Role::Viewer,
                version: ben.version,
            },
        )
        .unwrap();
    let before = s.op_count(&sid);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(b.owners().shared_entry(&nb).unwrap().role, Role::Viewer);
    assert_eq!(s.op_count(&sid), before);
    assert_eq!(b.space_engine(&space).unsent_ops().unwrap().len(), 1);
    assert_eq!(ink_count(&mut b.lib(), nb), 4);
    assert_eq!(ink_count(&mut a.lib(), nb), 3);
    let copy = shared::keep_private_copy(&mut b.lib(), nb).unwrap();
    assert_eq!(ink_count(&mut b.lib(), copy.id), 4);
}

#[test]
fn offline_owner_server_restart_and_late_reconnects_converge() {
    let dir = tempfile::tempdir().unwrap();
    let s = Arc::new(InMemoryServer::open_persistent(dir.path()).unwrap());
    let (a_id, b_id) = (person(&s, "ann"), person(&s, "ben"));
    let mut a = Dev::new(&s, &a_id, "a");
    let mut b = Dev::new(&s, &b_id, "b");
    let (nb, page) = notebook_with_strokes(&mut a, "Lab", 2);
    let space = a.share(nb);
    a.sync();
    a.invite(&space, "ben@example.org", Role::Editor);
    b.accept();
    settle(&mut [&mut a, &mut b]);
    // the owner goes offline and keeps writing; the editor writes online
    a.offline(true);
    add(&mut a.lib(), nb, page, vec![stroke(70, 10.0, "#000001")]);
    a.lib().rename_notebook(nb, "Lab (owner)").unwrap();
    a.sync();
    add(&mut b.lib(), nb, page, vec![stroke(80, 20.0, "#000002")]);
    b.sync();
    // the server restarts from disk
    drop(s);
    let s = Arc::new(InMemoryServer::open_persistent(dir.path()).unwrap());
    for d in [&mut a, &mut b] {
        d.server = s.clone();
        d.account.transport_mut().server = s.clone();
        for e in &mut d.spaces.engines {
            e.transport_mut().server = s.clone();
        }
    }
    // the editor comes back first and writes more, then the owner reconnects
    add(&mut b.lib(), nb, page, vec![stroke(81, 30.0, "#000003")]);
    b.sync();
    a.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib(), nb), 5);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    assert_eq!(b.lib().notebook(nb).unwrap().title, "Lab (owner)");
    // the rename reached the sealed title too
    let rec = s
        .shared_list(&b_id)
        .unwrap()
        .spaces
        .into_iter()
        .find(|r| r.space_id.as_str() == space)
        .unwrap();
    let root = shared::load_root(secrets_at(&b.root).as_ref(), &space)
        .unwrap()
        .unwrap();
    assert_eq!(
        shared::open_meta(&root, &rec.sealed_meta).unwrap().title,
        "Lab (owner)"
    );
}

#[test]
fn two_devices_per_person_keep_their_own_cursors() {
    let s = Arc::new(InMemoryServer::new());
    let (a_id, b_id) = (person(&s, "ann"), person(&s, "ben"));
    let mut a1 = Dev::new(&s, &a_id, "a1");
    let mut a2 = Dev::new(&s, &a_id, "a2");
    let mut b1 = Dev::new(&s, &b_id, "b1");
    let mut b2 = Dev::new(&s, &b_id, "b2");
    let (nb, page) = notebook_with_strokes(&mut a1, "Plans", 1);
    a2.sync();
    let space = a1.share(nb);
    a1.sync();
    a1.invite(&space, "ben@example.org", Role::Editor);
    b1.accept();
    settle(&mut [&mut a1, &mut a2, &mut b1, &mut b2]);
    for d in [&a1, &a2, &b1, &b2] {
        assert_eq!(
            d.state(nb),
            Some(SharedState::Active),
            "{}",
            d.account.device_id()
        );
    }
    // everyone writes; everyone ends up with everything
    for (i, d) in [&mut a1, &mut a2, &mut b1, &mut b2].into_iter().enumerate() {
        add(
            &mut d.lib(),
            nb,
            page,
            vec![stroke(100 + i as u64, 50.0, "#222222")],
        );
    }
    settle(&mut [&mut a1, &mut a2, &mut b1, &mut b2]);
    let want = a1.notebook_view(nb);
    for d in [&a2, &b1, &b2] {
        assert_eq!(d.notebook_view(nb), want);
    }
    assert_eq!(ink_count(&mut b2.lib(), nb), 5);
    // four devices in the space, each with its own position on the server
    let sid: AccountId = space.parse().unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for d in [&a1, &a2, &b1, &b2] {
        let dev = DeviceId::new(d.account.device_id()).unwrap();
        assert!(s.applied_position(&sid, &dev).is_some());
        seen.insert(d.account.device_id().to_string());
    }
    assert_eq!(seen.len(), 4);
    // nothing of the shared notebook in Ben's account, nothing of Ben's in Ann's
    assert_eq!(s.op_count(&b_id), 0);
}

#[test]
fn sharing_survives_a_crash_at_every_step() {
    for stop_at in 0..6 {
        let s = Arc::new(InMemoryServer::new());
        let a_id = person(&s, "ann");
        let mut a = Dev::new(&s, &a_id, "a");
        let (nb, page) = notebook_with_strokes(&mut a, "History", 3);
        a.lib().rename_notebook(nb, "History, renamed").unwrap();
        add(&mut a.lib(), nb, page, vec![text(0x99, "first")]);
        edit(
            &mut a.lib(),
            nb,
            page,
            OpKind::ReplaceElements {
                elements: vec![text(0x99, "second")],
            },
        );
        a.sync();
        let versions_before = a.account.page_versions(nb, page).unwrap().len();
        let mut step = 0;
        let r = a.with_access(|acc, _, lib, access| {
            shared::share(acc, lib, nb, access, &mut |st| {
                let here = match st {
                    ShareStep::SpaceCreated => 0,
                    ShareStep::KeyStored => 1,
                    ShareStep::EngineReady => 2,
                    ShareStep::Transplanted(1) => 3,
                    ShareStep::Transplanted(_) => return Ok(()),
                    ShareStep::Marked => 4,
                };
                step = here;
                if here == stop_at {
                    Err(SyncError::Invalid("simulated crash".into()))
                } else {
                    Ok(())
                }
            })
        });
        if stop_at < 5 {
            assert!(r.is_err(), "stop at {stop_at}");
        }
        drop(r);
        // the run really stopped where it was meant to (5 is the run without a crash)
        if stop_at < 5 {
            assert_eq!(step, stop_at, "the crash at step {stop_at} didn't happen");
        } else {
            assert_eq!(step, 4, "the run without a crash went through every step");
        }
        // more local work before the retry, which must also end up in the space
        add(&mut a.lib(), nb, page, vec![stroke(55, 9.0, "#999999")]);
        let space = a.share(nb);
        a.sync();
        assert!(!shared::share_in_progress(&a.root, nb));
        // exactly one space, holding exactly the local notebook, with its history
        let spaces = s.shared_list(&a_id).unwrap().spaces;
        assert_eq!(spaces.len(), 1, "stop at {stop_at}");
        let local = a.notebook_view(nb);
        let e = a.space_engine(&space);
        let (merged, _) = e.merged(nb).unwrap().unwrap();
        assert_eq!(merged.title, "History, renamed");
        assert!(
            e.page_versions(nb, page).unwrap().len() >= versions_before,
            "stop at {stop_at}"
        );
        // a second device of a fresh member sees the same notebook
        let b_id = person(&s, &format!("ben{stop_at}"));
        let mut b = Dev::new(&s, &b_id, "b");
        a.invite(&space, &format!("ben{stop_at}@example.org"), Role::Viewer);
        b.accept();
        settle(&mut [&mut a, &mut b]);
        assert_eq!(b.notebook_view(nb), local, "stop at {stop_at}");
    }
}

#[test]
fn the_share_marker_moves_the_notebook_on_another_device_without_losing_its_edits() {
    let s = Arc::new(InMemoryServer::new());
    let a_id = person(&s, "ann");
    let mut a1 = Dev::new(&s, &a_id, "a1");
    let mut a2 = Dev::new(&s, &a_id, "a2");
    let (nb, page) = notebook_with_strokes(&mut a1, "Shared later", 2);
    a2.sync();
    assert_eq!(ink_count(&mut a2.lib(), nb), 2);
    // a2 edits offline, before it knows the notebook was shared
    a2.offline(true);
    add(&mut a2.lib(), nb, page, vec![stroke(30, 77.0, "#0a0a0a")]);
    a2.sync();
    let space = a1.share(nb);
    a1.sync();
    // a2 edits again after its account engine moved the notebook, before it joined
    a2.offline(false);
    let _ = a2.account.reconcile(&a2.lib);
    assert_eq!(a2.state(nb), Some(SharedState::Joining));
    add(&mut a2.lib(), nb, page, vec![stroke(31, 78.0, "#0b0b0b")]);
    settle(&mut [&mut a1, &mut a2]);
    assert_eq!(a2.state(nb), Some(SharedState::Active));
    assert_eq!(a2.owners().scope_of(&nb), Scope::Shared(space.clone()));
    assert_eq!(ink_count(&mut a1.lib(), nb), 4);
    assert_eq!(a1.notebook_view(nb), a2.notebook_view(nb));
    // republishing is idempotent: nothing more after further passes
    let sid: AccountId = space.parse().unwrap();
    let n = s.op_count(&sid);
    settle(&mut [&mut a1, &mut a2]);
    assert_eq!(s.op_count(&sid), n);
}

#[test]
fn vault_notebooks_cannot_be_shared() {
    let s = Arc::new(InMemoryServer::new());
    let a_id = person(&s, "ann");
    let mut a = Dev::new(&s, &a_id, "a");
    let (nb, _) = a
        .lib()
        .create_notebook("Private", PageDefaults::default(), None)
        .unwrap();
    let mut owners = a.owners();
    owners.notebooks.insert(nb.id, "v1".into());
    owners.save(&a.root).unwrap();
    let r = a.with_access(|acc, spaces, lib, access| spaces.share(acc, lib, access, nb.id));
    assert!(
        matches!(r, Err(SyncError::Invalid(ref m)) if m.contains("vault")),
        "{r:?}"
    );
    assert!(s.shared_list(&a_id).unwrap().spaces.is_empty());
}

#[test]
fn only_the_marker_needs_the_new_operation_schema() {
    let (s, a, _b, nb, _, space) = shared_pair(Role::Editor);
    let a_id: AccountId = "ann".parse().unwrap();
    let dev = DeviceId::new(a.account.device_id()).unwrap();
    let page = s
        .pull(
            &a_id,
            &dev,
            &PullOpsRequest {
                cursor: None,
                limit: 500,
            },
        )
        .unwrap();
    let items: Vec<(penroam_sync_protocol::PulledOp, Vec<u8>)> =
        penroam_sync_protocol::framing::decode_json_batch(&page.frame, &Limits::DEFAULT).unwrap();
    let newest: Vec<u16> = items
        .iter()
        .map(|(p, _)| p.header.op_schema_version)
        .collect();
    // exactly one envelope (the marker) is schema 5: a client that knows schema 4 parks that
    // one and what depends on it, and pauses with "needs an update" (nothing is lost)
    assert_eq!(newest.iter().filter(|v| **v == 5).count(), 1, "{newest:?}");
    assert!(newest.iter().all(|v| *v <= 5));
    let _ = (nb, space);
    // and an older build refuses to apply it rather than misreading it
    let mut op = penroam_core::operation::LocalOperation {
        id: Uuid::new_v4(),
        device_id: Uuid::new_v4(),
        local_sequence: 1,
        schema_version: penroam_core::operation::OP_SCHEMA_VERSION + 1,
        causal_dependencies: vec![],
        at: 0,
        body: DocumentOperation::NotebookShared {
            notebook_id: Uuid::new_v4(),
            space_id: "s".into(),
        },
    };
    assert!(penroam_core::operation::check_schema(&op).is_err());
    op.schema_version = 5;
    assert!(penroam_core::operation::check_schema(&op).is_ok());
}

#[test]
fn a_space_root_is_only_accepted_if_it_verifies_the_key_log() {
    let (s, mut a, _b, _nb, _, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    // someone (a hostile server) swaps the stored key: a new device can't enroll with it
    let c_id = person(&s, "cat");
    a.invite(&space, "cat@example.org", Role::Editor);
    let mut c = Dev::new(&s, &c_id, "c");
    let _ = c.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
    let inv = c.spaces.invitations[0].invitation_id.clone();
    let rec = c.account.transport_mut().invitation_accept(&inv).unwrap();
    let wrong = RootSecret::generate().unwrap();
    let doc = AccountKeyDocument {
        key: penroam_sync_protocol::base64url::encode(&wrong.to_bytes()),
    };
    let _ = doc;
    let setup = Setup {
        library_root: c.root.clone(),
        scope: Scope::shared(&space).unwrap(),
        device_id: Some(c.account.device_id().into()),
        server_url: "in-process".into(),
        account_id: space.clone(),
        label: "c".into(),
    };
    let t = InProcessTransport::for_space(
        s.clone(),
        sid,
        c_id,
        DeviceId::new(c.account.device_id()).unwrap(),
    );
    let r = Engine::enroll_with_root(setup, t, secrets_at(&c.root), &wrong);
    assert!(r.is_err());
    let _ = rec;
}

/// The space's key log as the server holds it, verified.
fn space_keylog(
    s: &InMemoryServer,
    space: &AccountId,
    member: &AccountId,
) -> penroam_sync::keylog::Membership {
    let mut raw = Vec::new();
    loop {
        let page = s
            .keylog_get(Caller::member(space, member), raw.len() as u64)
            .unwrap();
        if page.entries.is_empty() {
            break;
        }
        for e in page.entries {
            raw.push(penroam_sync_protocol::base64url::decode(&e.data).unwrap());
        }
        if raw.len() as u64 >= page.next_index {
            break;
        }
    }
    penroam_sync::keylog::verify_log(&raw, None, None).unwrap()
}

fn device_keys(d: &Dev, space: &str) -> penroam_sync::crypto::DeviceKeys {
    let raw = secrets_at(&d.root)
        .get(
            &Scope::shared(space)
                .unwrap()
                .secret_name(penroam_sync::engine::DEVICE_SECRET),
        )
        .unwrap()
        .unwrap();
    penroam_sync::crypto::DeviceKeys::from_secret_string(&raw).unwrap()
}

#[test]
fn removing_a_member_rotates_their_devices_out_of_the_key_log() {
    let (s, mut a1, mut b, nb, page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    // a second owner device, so two devices race to rotate
    let mut a2 = Dev::new(&s, &a1.me.clone(), "a2");
    settle(&mut [&mut a1, &mut a2, &mut b]);
    assert_eq!(a2.state(nb), Some(SharedState::Active));
    let before = space_keylog(&s, &sid, &a1.me);
    let b_dev = b.account.device_id().to_string();
    assert!(before.is_active(&b_dev));
    let b_keys = device_keys(&b, &space);
    assert!(before
        .epoch_keys_for(&b_keys)
        .contains_key(&before.current_epoch));

    a1.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    settle(&mut [&mut a1, &mut a2, &mut b]);
    let after = space_keylog(&s, &sid, &a1.me);
    assert!(!after.is_active(&b_dev));
    // exactly one new epoch, though both owner devices noticed
    assert_eq!(after.current_epoch, before.current_epoch + 1);
    assert_eq!(a1.space_engine(&space).remove_revoked_devices().unwrap(), 0);
    assert_eq!(a2.space_engine(&space).remove_revoked_devices().unwrap(), 0);
    // B's keys open nothing of the new epoch; the owners' do
    assert!(!after
        .epoch_keys_for(&b_keys)
        .contains_key(&after.current_epoch));
    assert!(after
        .epoch_keys_for(&device_keys(&a2, &space))
        .contains_key(&after.current_epoch));
    // what the owner writes now is sealed with that epoch
    add(&mut a1.lib(), nb, page, vec![stroke(120, 5.0, "#abcdef")]);
    a1.sync();
    let dev = DeviceId::new(a1.account.device_id()).unwrap();
    let pulled = s
        .pull(
            Caller::member(&sid, &a1.me),
            &dev,
            &PullOpsRequest {
                cursor: None,
                limit: 500,
            },
        )
        .unwrap();
    let items: Vec<(penroam_sync_protocol::PulledOp, Vec<u8>)> =
        penroam_sync_protocol::framing::decode_json_batch(&pulled.frame, &Limits::DEFAULT).unwrap();
    let last = &items.last().unwrap().0.header;
    assert_eq!(last.key_epoch, after.current_epoch);
    assert!(!after.epoch_keys_for(&b_keys).contains_key(&last.key_epoch));
    // and the other owner device still reads it
    settle(&mut [&mut a1, &mut a2]);
    assert_eq!(ink_count(&mut a2.lib(), nb), 4);
}

#[test]
fn sharing_one_notebook_from_two_devices_at_once_ends_in_one_space() {
    let s = Arc::new(InMemoryServer::new());
    let a_id = person(&s, "ann");
    let mut a1 = Dev::new(&s, &a_id, "a1");
    let mut a2 = Dev::new(&s, &a_id, "a2");
    let (nb, page) = notebook_with_strokes(&mut a1, "Twice", 1);
    a2.sync();
    // both share before either has seen the other's share
    let s1 = a1.share(nb);
    let s2 = a2.share(nb);
    assert_ne!(s1, s2);
    // and both keep writing, each into its own space
    add(&mut a1.lib(), nb, page, vec![stroke(201, 1.0, "#100000")]);
    add(&mut a2.lib(), nb, page, vec![stroke(202, 2.0, "#200000")]);
    settle(&mut [&mut a1, &mut a2]);
    let e1 = a1.owners().shared_entry(&nb).cloned().unwrap();
    let e2 = a2.owners().shared_entry(&nb).cloned().unwrap();
    assert_eq!(e1.space_id, e2.space_id, "one space on both devices");
    assert_eq!(e1.state, SharedState::Active);
    assert_eq!(e2.state, SharedState::Active);
    assert!(e1.replaces.is_none() && e2.replaces.is_none());
    let a1_lost = e1.space_id == s2;
    let loser = if a1_lost { s1 } else { s2 };
    let (lost, won) = if a1_lost {
        (&mut a1, &mut a2)
    } else {
        (&mut a2, &mut a1)
    };
    // the losing device keeps nothing of its own space, and nothing it wrote is lost
    assert!(!Scope::Shared(loser.clone()).dir(&lost.root).exists());
    assert!(lost
        .spaces
        .engines
        .iter()
        .all(|e| e.scope().id() != Some(loser.as_str())));
    assert_eq!(ink_count(&mut lost.lib(), nb), 3);
    assert_eq!(ink_count(&mut won.lib(), nb), 3);
    assert_eq!(lost.notebook_view(nb), won.notebook_view(nb));
    // later edits on the losing device reach the other one through the winner
    add(&mut lost.lib(), nb, page, vec![stroke(203, 3.0, "#300000")]);
    settle(&mut [lost, won]);
    assert_eq!(ink_count(&mut a1.lib(), nb), 4);
    assert_eq!(ink_count(&mut a2.lib(), nb), 4);
}

#[test]
fn a_removed_member_admitted_again_joins_with_a_new_device_and_keeps_the_old_copy_apart() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    let old_dev = b.space_engine(&space).device_id().to_string();
    assert_eq!(
        old_dev,
        b.account.device_id(),
        "first membership uses the main device id"
    );
    // B writes offline, then is removed
    b.offline(true);
    add(&mut b.lib(), nb, page, vec![stroke(300, 9.0, "#ff00ff")]);
    b.sync();
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(b.state(nb), Some(SharedState::AccessRemoved));
    // A invites B again; accepting waits until the old copy is dealt with
    a.invite(&space, "ben@example.org", Role::Editor);
    let _ = b.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
    let inv = b.spaces.invitations[0].invitation_id.clone();
    let r = b.with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv));
    assert!(
        matches!(r, Err(SyncError::Invalid(ref m)) if m.contains("earlier copy")),
        "{r:?}"
    );
    assert_eq!(b.state(nb), Some(SharedState::AccessRemoved));
    // B keeps a private copy (with the unsent stroke), removes the old one, accepts
    let copy = shared::keep_private_copy(&mut b.lib(), nb).unwrap();
    assert_eq!(ink_count(&mut b.lib(), copy.id), 4);
    {
        let root = b.root.clone();
        let mut lib = b.lib.lock().unwrap();
        shared::remove_local(&mut lib, secrets_at(&root).as_ref(), &space).unwrap();
    }
    let _ = b.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
    let inv = b.spaces.invitations[0].invitation_id.clone();
    b.with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv))
        .unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(b.state(nb), Some(SharedState::Active));
    let new_dev = b.space_engine(&space).device_id().to_string();
    assert_ne!(new_dev, old_dev, "a new device id in the space");
    // the stale outbox didn't come along: the space has A's three strokes only
    assert_eq!(ink_count(&mut b.lib(), nb), 3);
    assert_eq!(ink_count(&mut a.lib(), nb), 3);
    // and edits flow again both ways
    add(&mut b.lib(), nb, page, vec![stroke(301, 10.0, "#00ffff")]);
    add(&mut a.lib(), nb, page, vec![stroke(302, 11.0, "#ffff00")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib(), nb), 5);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    assert_eq!(ink_count(&mut b.lib(), copy.id), 4);
    let _ = s;
}

#[test]
fn edits_that_land_after_access_removal_stay_local_and_go_into_the_private_copy() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    b.sync();
    assert_eq!(b.state(nb), Some(SharedState::AccessRemoved));
    let (in_space, in_account) = (s.op_count(&sid), s.op_count(&b.me));
    // a batch the editor had in flight reaches the library (the app lets page edits through)
    add(&mut b.lib(), nb, page, vec![stroke(400, 1.0, "#010101")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(s.op_count(&sid), in_space, "nothing goes to the space");
    assert_eq!(
        s.op_count(&b.me),
        in_account,
        "nor to the member's own account"
    );
    assert_eq!(ink_count(&mut b.lib(), nb), 4);
    let copy = shared::keep_private_copy(&mut b.lib(), nb).unwrap();
    assert_eq!(ink_count(&mut b.lib(), copy.id), 4);
    settle(&mut [&mut b]);
    assert_eq!(s.op_count(&sid), in_space);
}

#[test]
fn only_the_owners_devices_publish_checkpoints_in_a_space() {
    let (s, mut a, mut b, _nb, _page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    assert!(b
        .space_engine(&space)
        .create_checkpoint()
        .unwrap()
        .is_none());
    let rec = a.space_engine(&space).create_checkpoint().unwrap().unwrap();
    let held = s
        .checkpoint_latest(
            Caller::member(&sid, &a.me),
            &DeviceId::new(a.account.device_id()).unwrap(),
        )
        .unwrap();
    assert_eq!(held, rec);
    assert_eq!(held.created_by.as_str(), a.account.device_id());
    b.sync();
}

/// A shared notebook with history, a checkpoint by the owner, more edits after it, and the
/// server compacted up to the checkpoint.
fn compacted_space() -> (Arc<InMemoryServer>, Dev, Uuid, Uuid, String) {
    let s = Arc::new(InMemoryServer::new());
    let a_id = person(&s, "ann");
    let mut a = Dev::new(&s, &a_id, "a");
    let (nb, page) = notebook_with_strokes(&mut a, "Long", 4);
    a.lib()
        .add_asset(nb, b"attached bytes", "application/octet-stream", None)
        .unwrap();
    let space = a.share(nb);
    a.sync();
    a.space_engine(&space).create_checkpoint().unwrap().unwrap();
    add(&mut a.lib(), nb, page, vec![stroke(500, 5.0, "#050505")]);
    a.sync();
    let sid: AccountId = space.parse().unwrap();
    assert!(s.compact_to_latest_checkpoint(&sid) > 0);
    (s, a, nb, page, space)
}

#[test]
fn joining_a_compacted_space_starts_from_the_checkpoint_and_its_tail() {
    let (s, mut a, nb, page, space) = compacted_space();
    // a newcomer has no cursor: checkpoint, then the operations after it
    let c_id = person(&s, "cat");
    let mut c = Dev::new(&s, &c_id, "c");
    a.invite(&space, "cat@example.org", Role::Editor);
    c.accept();
    settle(&mut [&mut a, &mut c]);
    assert_eq!(c.state(nb), Some(SharedState::Active));
    assert_eq!(c.notebook_view(nb), a.notebook_view(nb));
    assert_eq!(ink_count(&mut c.lib(), nb), 5);
    // a member whose cursor fell behind the compaction recovers the same way
    let d_id = person(&s, "dan");
    let mut d = Dev::new(&s, &d_id, "d");
    a.invite(&space, "dan@example.org", Role::Viewer);
    d.accept();
    settle(&mut [&mut a, &mut d]);
    d.offline(true);
    add(&mut a.lib(), nb, page, vec![stroke(501, 6.0, "#060606")]);
    a.sync();
    a.space_engine(&space).create_checkpoint().unwrap().unwrap();
    add(&mut a.lib(), nb, page, vec![stroke(502, 7.0, "#070707")]);
    a.sync();
    s.compact_to_latest_checkpoint(&space.parse().unwrap());
    d.offline(false);
    settle(&mut [&mut a, &mut d]);
    assert_eq!(ink_count(&mut d.lib(), nb), 7);
    assert_eq!(d.notebook_view(nb), a.notebook_view(nb));
}

#[test]
fn a_corrupt_blob_keeps_a_join_joining_until_good_bytes_arrive() {
    let (s, mut a, nb, _page, space) = compacted_space();
    let c_id = person(&s, "cat");
    let mut c = Dev::new(&s, &c_id, "c");
    c.faults = (true, false);
    a.invite(&space, "cat@example.org", Role::Editor);
    c.accept();
    for _ in 0..3 {
        c.sync();
    }
    assert_eq!(
        c.state(nb),
        Some(SharedState::Joining),
        "no success on corrupt bytes"
    );
    assert!(c.lib().notebook(nb).is_err(), "nothing shown while joining");
    // the server serves the right bytes again
    for e in &mut c.spaces.engines {
        e.transport_mut().corrupt_blob_gets = false;
    }
    c.faults = (false, false);
    settle(&mut [&mut a, &mut c]);
    assert_eq!(c.state(nb), Some(SharedState::Active));
    let sha = c.lib().notebook(nb).unwrap().assets[0].sha256.clone();
    assert_eq!(c.lib().read_asset(nb, &sha).unwrap(), b"attached bytes");
}

#[test]
fn a_corrupt_checkpoint_is_refused_and_the_join_waits() {
    let (s, mut a, nb, _page, space) = compacted_space();
    let c_id = person(&s, "cat");
    let mut c = Dev::new(&s, &c_id, "c");
    c.faults = (false, true);
    a.invite(&space, "cat@example.org", Role::Editor);
    c.accept();
    let _ = c.account.reconcile(&c.lib);
    c.with_access(|acc, spaces, lib, access| {
        let _ = spaces.refresh(acc, lib);
        spaces.reconcile(acc, lib, access)
    });
    assert_eq!(c.state(nb), Some(SharedState::Joining));
    assert!(c.lib().notebook(nb).is_err());
    let err = c.spaces.errors.get(&space).cloned().unwrap_or_default();
    assert!(err.contains("checkpoint"), "{err}");
    for e in &mut c.spaces.engines {
        e.transport_mut().corrupt_checkpoints = false;
    }
    c.faults = (false, false);
    settle(&mut [&mut a, &mut c]);
    assert_eq!(c.state(nb), Some(SharedState::Active));
    assert_eq!(c.notebook_view(nb), a.notebook_view(nb));
}

#[test]
fn an_older_build_parks_the_share_and_loses_nothing_until_it_is_updated() {
    let s = Arc::new(InMemoryServer::new());
    let a_id = person(&s, "ann");
    let mut a1 = Dev::new(&s, &a_id, "a1");
    let mut a2 = Dev::new(&s, &a_id, "a2");
    let (nb, page) = notebook_with_strokes(&mut a1, "Mixed versions", 1);
    a2.sync();
    // a2 runs a build that knows operation schemas up to 4
    a2.account.max_op_schema_for_tests = Some(4);
    a2.offline(true);
    add(&mut a2.lib(), nb, page, vec![stroke(600, 1.0, "#600000")]);
    a2.sync();
    let space = a1.share(nb);
    a1.sync();
    a2.offline(false);
    a2.sync();
    let st = a2.account.status(a2.lib().last_seq());
    assert_eq!(st.phase, penroam_sync::Phase::NeedsUpgrade);
    assert!(st.pending_remote > 0, "the marker is parked, not dropped");
    assert_eq!(st.queued, 0, "its own edit was still uploaded");
    assert_eq!(
        a2.state(nb),
        None,
        "the notebook isn't moved by a build that can't"
    );
    // it keeps working locally, and keeps uploading to the account
    add(&mut a2.lib(), nb, page, vec![stroke(601, 2.0, "#600001")]);
    a2.sync();
    assert_eq!(a2.account.status(a2.lib().last_seq()).queued, 0);
    assert_eq!(ink_count(&mut a2.lib(), nb), 3);
    // updated: the marker applies, the device joins and its edits reach the space
    a2.account.max_op_schema_for_tests = None;
    settle(&mut [&mut a1, &mut a2]);
    assert_eq!(a2.state(nb), Some(SharedState::Active));
    assert_eq!(a2.owners().scope_of(&nb), Scope::Shared(space));
    assert_eq!(ink_count(&mut a1.lib(), nb), 3);
    assert_eq!(a1.notebook_view(nb), a2.notebook_view(nb));
}

#[test]
fn a_reimported_shared_notebook_gets_a_new_id() {
    let (_s, mut a, mut b, nb, _page, space) = shared_pair(Role::Editor);
    let mut archive = std::io::Cursor::new(Vec::new());
    b.lib().export_archive(nb, &mut archive, "test").unwrap();
    let sid: AccountId = space.parse().unwrap();
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    b.sync();
    {
        let root = b.root.clone();
        let mut lib = b.lib.lock().unwrap();
        shared::remove_local(&mut lib, secrets_at(&root).as_ref(), &space).unwrap();
    }
    assert!(b.owners().retired.contains(&nb));
    let mut lib = b.lib.lock().unwrap();
    lib.set_reserved_ids(NotebookOwners::load(&b.root).unwrap().reserved_import_ids());
    archive.set_position(0);
    let r = lib.import_archive(archive).unwrap();
    assert_ne!(r.notebook.id, nb, "a reserved id is never reused");
    assert!(r.renamed_copy);
}

#[test]
fn a_rewritten_sealed_title_can_never_pull_another_notebook_into_a_space() {
    let (s, mut a1, mut b, nb, _page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    // the owner's personal notebook, on both of the owner's devices
    let (p, _) = notebook_with_strokes(&mut a1, "Private", 2);
    let mut a2 = Dev::new(&s, &a1.me.clone(), "a2");
    // a member (or a server that let it through) rewrites the title to name P
    let root = shared::load_root(secrets_at(&b.root).as_ref(), &space)
        .unwrap()
        .unwrap();
    let evil = shared::seal_meta(
        &root,
        &shared::SpaceMeta {
            notebook_id: p,
            title: "gotcha".into(),
        },
    );
    s.shared_set_meta(
        &a1.me,
        &sid,
        &penroam_sync_protocol::UpdateSpaceMetaRequest { sealed_meta: evil },
    )
    .unwrap();
    settle(&mut [&mut a1, &mut a2, &mut b]);
    // a2 joined the shared notebook the marker named, and P stayed personal everywhere
    assert_eq!(a2.state(nb), Some(SharedState::Active));
    for d in [&a1, &a2] {
        assert!(d.owners().shared_entry(&p).is_none());
        assert_eq!(d.owners().scope_of(&p), Scope::Account);
    }
    assert!(a1.space_engine(&space).logged_ops(p).unwrap().is_empty());
    assert!(a2.space_engine(&space).logged_ops(p).unwrap().is_empty());
    assert_eq!(ink_count(&mut a2.lib(), p), 2);
    assert_eq!(a2.notebook_view(nb), a1.notebook_view(nb));

    // a nil notebook id: an invitee refuses to join rather than getting stuck
    let nil = shared::seal_meta(
        &root,
        &shared::SpaceMeta {
            notebook_id: Uuid::nil(),
            title: "nothing".into(),
        },
    );
    s.shared_set_meta(
        &a1.me,
        &sid,
        &penroam_sync_protocol::UpdateSpaceMetaRequest { sealed_meta: nil },
    )
    .unwrap();
    let c_id = person(&s, "cat");
    let mut c = Dev::new(&s, &c_id, "c");
    a1.invite(&space, "cat@example.org", Role::Editor);
    let _ = c.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
    let inv = c.spaces.invitations[0].invitation_id.clone();
    let r = c.with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv));
    assert!(r.is_err());
    assert!(c.owners().shared.is_empty());
}

#[test]
fn a_recovery_only_removal_locks_nobody_out() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    let v_id = person(&s, "vic");
    let mut v = Dev::new(&s, &v_id, "v");
    a.invite(&space, "vic@example.org", Role::Viewer);
    v.accept();
    settle(&mut [&mut a, &mut b, &mut v]);
    // the viewer removes its own device and grants the new epoch only to the recovery key
    let m = space_keylog(&s, &sid, &a.me);
    let vk = device_keys(&v, &space);
    let k = penroam_sync::crypto::EpochKey::generate().unwrap();
    let entry = penroam_sync::keylog::make_entry(
        m.len,
        &m.head,
        penroam_sync::keylog::EntryKind::RemoveDevice {
            device_id: vk.device_id.clone(),
            new_epoch: m.current_epoch + 1,
            grants: vec![penroam_sync::keylog::grant(
                m.current_epoch + 1,
                &k,
                penroam_sync::keylog::RECOVERY,
                &m.recovery_pk,
            )
            .unwrap()],
            last_seq: None,
        },
        &vk.device_id,
        &vk.signing,
    );
    // stored or refused by the server (servers are adding a check), the others go on writing:
    // in a space the recovery grant gives everyone the new epoch's key
    let _ = s.keylog_append(
        Caller::member(&sid, &v_id),
        &penroam_sync_protocol::KeyLogAppendRequest {
            index: m.len,
            data: penroam_sync_protocol::base64url::encode(&entry),
        },
    );
    both_still_write(&mut a, &mut b, nb, page, 700, 3);
}

/// A viewer in A's space shared with editor B. Returns the viewer's device and the space id.
fn space_with_viewer() -> (Arc<InMemoryServer>, Dev, Dev, Dev, Uuid, Uuid, String) {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let v_id = person(&s, "vic");
    let mut v = Dev::new(&s, &v_id, "v");
    a.invite(&space, "vic@example.org", Role::Viewer);
    v.accept();
    settle(&mut [&mut a, &mut b, &mut v]);
    (s, a, b, v, nb, page, space)
}

/// The viewer removes its own device with the given grants for the new epoch.
fn viewer_removal(
    s: &InMemoryServer,
    v: &Dev,
    space: &str,
    grants: impl FnOnce(&penroam_sync::keylog::Membership, u32) -> Vec<penroam_sync::keylog::KeyGrant>,
) {
    let sid: AccountId = space.parse().unwrap();
    let m = space_keylog(s, &sid, &v.me);
    let vk = device_keys(v, space);
    let epoch = m.current_epoch + 1;
    let entry = penroam_sync::keylog::make_entry(
        m.len,
        &m.head,
        penroam_sync::keylog::EntryKind::RemoveDevice {
            device_id: vk.device_id.clone(),
            new_epoch: epoch,
            grants: grants(&m, epoch),
            last_seq: None,
        },
        &vk.device_id,
        &vk.signing,
    );
    s.keylog_append(
        Caller::member(&sid, &v.me),
        &penroam_sync_protocol::KeyLogAppendRequest {
            index: m.len,
            data: penroam_sync_protocol::base64url::encode(&entry),
        },
    )
    .expect("the server stores it: every recipient has a grant");
}

/// Owner and editor write, read each other, and keep doing so after their space engines restart.
fn both_still_write(a: &mut Dev, b: &mut Dev, nb: Uuid, page: Uuid, base: u64, start: usize) {
    add(&mut a.lib(), nb, page, vec![stroke(base, 1.0, "#800000")]);
    add(
        &mut b.lib(),
        nb,
        page,
        vec![stroke(base + 1, 2.0, "#800001")],
    );
    settle(&mut [&mut *a, &mut *b]);
    assert_eq!(ink_count(&mut a.lib(), nb), start + 2);
    assert_eq!(ink_count(&mut b.lib(), nb), start + 2);
    // restart: the engines are opened again from disk
    a.spaces.engines.clear();
    b.spaces.engines.clear();
    add(
        &mut a.lib(),
        nb,
        page,
        vec![stroke(base + 2, 3.0, "#800002")],
    );
    add(
        &mut b.lib(),
        nb,
        page,
        vec![stroke(base + 3, 4.0, "#800003")],
    );
    settle(&mut [&mut *a, &mut *b]);
    assert_eq!(ink_count(&mut a.lib(), nb), start + 4);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
}

#[test]
fn junk_grants_in_a_removal_void_the_epoch_and_everyone_goes_on_writing() {
    let (s, mut a, mut b, v, nb, page, space) = space_with_viewer();
    viewer_removal(&s, &v, &space, |m, epoch| {
        let mut to: Vec<String> = m
            .active_devices()
            .map(|d| d.cert.device_id.clone())
            .filter(|d| *d != v.space_keys_id(&space))
            .collect();
        to.push(penroam_sync::keylog::RECOVERY.into());
        to.into_iter()
            .map(|to| penroam_sync::keylog::KeyGrant {
                to,
                epoch,
                wrapped: penroam_sync_protocol::base64url::encode(&[9u8; 80]),
            })
            .collect()
    });
    both_still_write(&mut a, &mut b, nb, page, 800, 3);
    // the removal still took effect, and a later honest removal starts a fresh epoch
    let sid: AccountId = space.parse().unwrap();
    let m = space_keylog(&s, &sid, &a.me);
    assert!(!m.is_active(&v.space_keys_id(&space)));
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    settle(&mut [&mut a, &mut b]);
    let m2 = space_keylog(&s, &sid, &a.me);
    assert_eq!(m2.current_epoch, m.current_epoch + 1);
    let root = shared::load_root(secrets_at(&a.root).as_ref(), &space)
        .unwrap()
        .unwrap();
    assert!(m2
        .epoch_keys_by_recovery(&root.recovery_dh())
        .contains_key(&m2.current_epoch));
}

#[test]
fn split_device_grants_still_give_every_member_the_same_key() {
    let (s, mut a, mut b, v, nb, page, space) = space_with_viewer();
    viewer_removal(&s, &v, &space, |m, epoch| {
        let k = penroam_sync::crypto::EpochKey::generate().unwrap();
        let mut g = vec![penroam_sync::keylog::grant(
            epoch,
            &k,
            penroam_sync::keylog::RECOVERY,
            &m.recovery_pk,
        )
        .unwrap()];
        for d in m.active_devices() {
            if d.cert.device_id == v.space_keys_id(&space) {
                continue;
            }
            // a different valid key for each device
            let own = penroam_sync::crypto::EpochKey::generate().unwrap();
            g.push(
                penroam_sync::keylog::grant(
                    epoch,
                    &own,
                    &d.cert.device_id,
                    &d.cert.dh_pk().unwrap(),
                )
                .unwrap(),
            );
        }
        g
    });
    both_still_write(&mut a, &mut b, nb, page, 900, 3);
}

#[test]
fn a_stale_sign_in_hides_invitations_but_shared_notebooks_keep_syncing() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    // ben's verified email is older than the server's window
    s.set_reauth_window_ms(Some(7 * 24 * 60 * 60 * 1000));
    s.set_email_verified_at_ms(&b.me, 1);
    // a second notebook, to which ben has an invitation waiting
    let (other, _) = notebook_with_strokes(&mut a, "Second", 1);
    let other_space = a.share(other);
    a.invite(&other_space, "ben@example.org", Role::Viewer);
    let _ = space;
    add(&mut a.lib(), nb, page, vec![stroke(990, 1.0, "#990000")]);
    settle(&mut [&mut a, &mut b]);
    assert!(b.spaces.invitations_need_sign_in);
    assert!(b.spaces.invitations.is_empty());
    assert!(b.spaces.errors.is_empty(), "{:?}", b.spaces.errors);
    assert!(b.spaces.available);
    assert_eq!(
        ink_count(&mut b.lib(), nb),
        4,
        "the shared notebook still syncs"
    );
    // a fresh sign-in brings them back
    s.set_email_verified_at_ms(&b.me, penroam_core::now_ms() as u64);
    b.sync();
    assert!(!b.spaces.invitations_need_sign_in);
    assert_eq!(b.spaces.invitations.len(), 1);
}

#[test]
fn a_failing_invitation_list_never_holds_back_membership_changes() {
    let (_s, mut a, mut b, nb, _page, space) = shared_pair(Role::Editor);
    let sid: AccountId = space.parse().unwrap();
    b.account.transport_mut().fail_invitation_lists = true;
    // a role change reaches a device only through the refreshed list
    let members = a.account.transport_mut().shared_members(&sid).unwrap();
    let ben = members
        .members
        .iter()
        .find(|m| m.account_id == b.me)
        .unwrap();
    a.account
        .transport_mut()
        .shared_change_role(
            &sid,
            &b.me,
            &ChangeRoleRequest {
                role: Role::Viewer,
                version: ben.version,
            },
        )
        .unwrap();
    b.sync();
    assert_eq!(b.owners().shared_entry(&nb).unwrap().role, Role::Viewer);
    // and a removal too
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    b.sync();
    assert_eq!(b.state(nb), Some(SharedState::AccessRemoved));
}
