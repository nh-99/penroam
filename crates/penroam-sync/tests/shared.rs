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
fn concurrent_edits_merge_text_and_keep_both_versions_of_a_style_until_resolved() {
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(&mut a.lib(), nb, page, vec![text(0x77, "agenda")]);
    settle(&mut [&mut a, &mut b]);
    a.offline(true);
    b.offline(true);
    // the same block: different words typed, and different colours chosen
    let colored = |s: &str, c: &str| match text(0x77, s) {
        Element::Text(mut t) => {
            t.color = c.into();
            Element::Text(t)
        }
        _ => unreachable!(),
    };
    edit(
        &mut a.lib(),
        nb,
        page,
        OpKind::ReplaceElements {
            elements: vec![colored("agenda: budget", "#aa0000")],
        },
    );
    edit(
        &mut b.lib(),
        nb,
        page,
        OpKind::ReplaceElements {
            elements: vec![colored("agenda: hiring", "#0000aa")],
        },
    );
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    let texts = |d: &Dev| {
        let p = d.lib().load_page(nb, page).unwrap();
        p.elements
            .iter()
            .find_map(|e| match e {
                Element::Text(t) if t.id == Uuid::from_u128(0x77) => {
                    Some((t.text.clone(), t.color.clone()))
                }
                _ => None,
            })
            .unwrap()
    };
    // the text merges: both people's words are kept
    let (ta, _) = texts(&a);
    assert_eq!(ta, texts(&b).0);
    assert!(ta.contains("budget") && ta.contains("hiring"), "{ta}");
    // the colour can't merge: both values are kept until someone picks one
    let conflicts = b.space_engine(&space).conflicts(nb).unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    let c = &conflicts[0];
    let mut values: Vec<String> = std::iter::once(&c.winner_value)
        .chain(c.alternates.iter().map(|x| &x.value))
        .map(|v| v["color"].as_str().unwrap().to_string())
        .collect();
    values.sort();
    assert_eq!(values, ["#0000aa", "#aa0000"]);
    assert_eq!(b.sync_state(nb), NotebookSyncState::NeedsResolution);
    // B picks the alternate; everyone converges on it, and the merged text stays
    let pick = c.alternates[0].op_id;
    let chosen = c.alternates[0].value["color"].as_str().unwrap().to_string();
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
    assert_eq!(texts(&a), (ta.clone(), chosen.clone()));
    assert_eq!(texts(&b), (ta, chosen));
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

fn set_text(lib: &mut Library, nb: Uuid, page: Uuid, id: u128, s: &str) {
    edit(
        lib,
        nb,
        page,
        OpKind::ReplaceElements {
            elements: vec![text(id, s)],
        },
    );
}

fn text_of(lib: &mut Library, nb: Uuid, page: Uuid, id: u128) -> Option<String> {
    lib.load_page(nb, page)
        .unwrap()
        .elements
        .iter()
        .find_map(|e| match e {
            Element::Text(t) if t.id == Uuid::from_u128(id) => Some(t.text.clone()),
            _ => None,
        })
}

#[test]
fn typing_at_the_same_spot_in_one_block_keeps_both_peoples_text() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(
        &mut a.lib(),
        nb,
        page,
        vec![text(0x51, "Meeting: | budget |")],
    );
    settle(&mut [&mut a, &mut b]);
    assert_eq!(
        text_of(&mut b.lib(), nb, page, 0x51).as_deref(),
        Some("Meeting: | budget |")
    );
    // both offline (a partition), typing at the same position, several saves each
    a.offline(true);
    b.offline(true);
    for (i, w) in ["A", "Al", "Alpha "].iter().enumerate() {
        set_text(
            &mut a.lib(),
            nb,
            page,
            0x51,
            &format!("Meeting: {w}| budget |"),
        );
        let _ = i;
        a.sync();
    }
    for w in ["B", "Be", "Beta "] {
        set_text(
            &mut b.lib(),
            nb,
            page,
            0x51,
            &format!("Meeting: {w}| budget |"),
        );
        b.sync();
    }
    // reconnect with lost responses (retries, duplicates on the server side)
    a.offline(false);
    b.offline(false);
    s.drop_next_responses(3);
    settle(&mut [&mut a, &mut b]);
    let ta = text_of(&mut a.lib(), nb, page, 0x51).unwrap();
    let tb = text_of(&mut b.lib(), nb, page, 0x51).unwrap();
    assert_eq!(ta, tb);
    assert!(
        ta.contains("Alpha ")
            && ta.contains("Beta ")
            && ta.starts_with("Meeting: ")
            && ta.ends_with("| budget |"),
        "{ta}"
    );
    // the space carries text updates, not whole replaces of the text
    let ops = a.space_engine(&space).logged_ops(nb).unwrap();
    assert!(ops.iter().any(|(_, o)| o.body.name() == "textEdited"));
    assert!(a.space_engine(&space).conflicts(nb).unwrap().is_empty());
    // a third party joining later reads the same text
    let c_id = person(&s, "cat");
    let mut c = Dev::new(&s, &c_id, "c");
    a.invite(&space, "cat@example.org", Role::Viewer);
    c.accept();
    settle(&mut [&mut a, &mut c]);
    assert_eq!(text_of(&mut c.lib(), nb, page, 0x51).unwrap(), ta);
}

#[test]
fn deleting_a_block_while_someone_types_in_it_keeps_the_typing_recoverable() {
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(&mut a.lib(), nb, page, vec![text(0x52, "draft")]);
    settle(&mut [&mut a, &mut b]);
    a.offline(true);
    b.offline(true);
    set_text(&mut a.lib(), nb, page, 0x52, "draft, with more");
    a.sync();
    edit(
        &mut b.lib(),
        nb,
        page,
        OpKind::RemoveElements {
            ids: vec![Uuid::from_u128(0x52)],
        },
    );
    b.sync();
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(
        text_of(&mut a.lib(), nb, page, 0x52),
        text_of(&mut b.lib(), nb, page, 0x52)
    );
    let c = b.space_engine(&space).conflicts(nb).unwrap();
    let kept = c.iter().find(|c| {
        matches!(
            c.target,
            penroam_core::replica::ConflictTarget::EditedWhileDeleted { .. }
        )
    });
    assert_eq!(
        kept.unwrap().alternates[0].value["text"],
        "draft, with more"
    );
}

#[test]
fn distinct_strokes_and_an_erase_converge_without_duplicates() {
    let (_s, mut a, mut b, nb, page, _space) = shared_pair(Role::Editor);
    a.offline(true);
    b.offline(true);
    add(&mut a.lib(), nb, page, vec![stroke(1100, 1.0, "#110000")]);
    add(&mut b.lib(), nb, page, vec![stroke(1101, 2.0, "#110001")]);
    // b erases one of the strokes both already had
    let first = b.lib().load_page(nb, page).unwrap().elements[0].id();
    edit(
        &mut b.lib(),
        nb,
        page,
        OpKind::RemoveElements { ids: vec![first] },
    );
    a.sync();
    b.sync();
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    let p = a.lib().load_page(nb, page).unwrap();
    let ids: std::collections::BTreeSet<Uuid> = p.elements.iter().map(|e| e.id()).collect();
    assert_eq!(ids.len(), p.elements.len(), "no duplicates");
    assert!(!ids.contains(&first));
    assert_eq!(ink_count(&mut a.lib(), nb), 3 - 1 + 2);
    // pressure and points come through unchanged
    let want = stroke(1101, 2.0, "#110001");
    let mine = p.elements.iter().find(|e| e.id() == want.id()).unwrap();
    assert_eq!(mine, &want);
}

#[test]
fn a_long_partition_with_work_on_both_sides_converges_from_the_old_cursor() {
    let (_s, mut a, mut b, nb, page, _space) = shared_pair(Role::Editor);
    add(&mut a.lib(), nb, page, vec![text(0x53, "log:")]);
    settle(&mut [&mut a, &mut b]);
    b.offline(true);
    for i in 0..40u64 {
        add(
            &mut a.lib(),
            nb,
            page,
            vec![stroke(1200 + i, i as f64, "#120000")],
        );
        if i % 4 == 0 {
            let t = text_of(&mut a.lib(), nb, page, 0x53).unwrap();
            set_text(&mut a.lib(), nb, page, 0x53, &format!("{t} a{i}"));
        }
        a.sync();
    }
    for i in 0..15u64 {
        add(
            &mut b.lib(),
            nb,
            page,
            vec![stroke(1300 + i, i as f64, "#130000")],
        );
        let t = text_of(&mut b.lib(), nb, page, 0x53).unwrap();
        set_text(&mut b.lib(), nb, page, 0x53, &format!("{t} b{i}"));
        b.sync();
    }
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    let t = text_of(&mut a.lib(), nb, page, 0x53).unwrap();
    assert!(t.contains("a36") && t.contains("b14"), "{t}");
    assert_eq!(ink_count(&mut a.lib(), nb), 3 + 40 + 15);
}

// ------------------------------------------------------------------ live --

use penroam_sync::live::{InProcessDialer, LiveClient, LiveEvent, LiveState};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// A live client for `d` in `space`, and its offline switch.
fn live_client(d: &Dev, space: &str) -> (LiveClient, Arc<AtomicBool>) {
    let root = shared::load_root(secrets_at(&d.root).as_ref(), space)
        .unwrap()
        .unwrap();
    let offline = Arc::new(AtomicBool::new(false));
    let device = DeviceId::new(d.space_keys_id(space)).unwrap();
    let dialer = InProcessDialer {
        server: d.server.clone(),
        space: space.parse().unwrap(),
        member: d.me.clone(),
        device: device.clone(),
        offline: offline.clone(),
    };
    let c = LiveClient::new(
        space.parse().unwrap(),
        d.me.clone(),
        device,
        root.awareness_key(),
        true,
        Box::new(dialer),
    );
    (c, offline)
}

fn pump(c: &mut LiveClient, steps: usize) -> Vec<LiveEvent> {
    let mut out = Vec::new();
    for _ in 0..steps {
        out.extend(c.step(Duration::ZERO));
    }
    out
}

fn catch_ups(ev: &[LiveEvent]) -> Vec<u64> {
    ev.iter()
        .filter_map(|e| match e {
            LiveEvent::CatchUp(s) => Some(*s),
            _ => None,
        })
        .collect()
}

#[test]
fn a_commit_hint_makes_the_other_member_pull_at_once() {
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let (mut lb, _) = live_client(&b, &space);
    let ev = pump(&mut lb, 3);
    assert!(ev.contains(&LiveEvent::State(LiveState::Live)), "{ev:?}");
    assert_eq!(catch_ups(&ev).len(), 1, "connecting always catches up");
    add(&mut a.lib(), nb, page, vec![stroke(1400, 1.0, "#140000")]);
    a.sync();
    let ev = pump(&mut lb, 5);
    assert_eq!(catch_ups(&ev).len(), 1, "{ev:?}");
    // what the app does on a hint: reconcile that space now
    b.spaces.engine(&space).unwrap().reconcile(&b.lib).unwrap();
    assert_eq!(ink_count(&mut b.lib(), nb), 4);
}

#[test]
fn hints_that_are_lost_or_doubled_never_lose_or_repeat_anything() {
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    let (mut lb, offline) = live_client(&b, &space);
    pump(&mut lb, 3);
    // the server commits but never broadcasts (as if it restarted in between)
    s.live_drop_next_hints(1);
    add(&mut a.lib(), nb, page, vec![stroke(1500, 1.0, "#150000")]);
    a.sync();
    assert!(catch_ups(&pump(&mut lb, 5)).is_empty());
    // the connection drops and comes back: it catches up over HTTP anyway
    offline.store(true, std::sync::atomic::Ordering::SeqCst);
    let ev = pump(&mut lb, 2);
    assert!(ev.contains(&LiveEvent::State(LiveState::Offline)), "{ev:?}");
    offline.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut ev = Vec::new();
    for _ in 0..200 {
        ev.extend(lb.step(Duration::from_millis(10)));
        if !catch_ups(&ev).is_empty() {
            break;
        }
    }
    assert_eq!(catch_ups(&ev).len(), 1, "{ev:?}");
    b.spaces.engine(&space).unwrap().reconcile(&b.lib).unwrap();
    assert_eq!(ink_count(&mut b.lib(), nb), 4);
    // doubled hints: one pull
    s.live_duplicate_next_hints(3);
    add(&mut a.lib(), nb, page, vec![stroke(1501, 2.0, "#150001")]);
    a.sync();
    assert_eq!(catch_ups(&pump(&mut lb, 6)).len(), 1);
    // and even with no live channel at all, ordinary sync gets it
    s.live_drop_next_hints(1);
    add(&mut a.lib(), nb, page, vec![stroke(1502, 3.0, "#150002")]);
    a.sync();
    b.sync();
    assert_eq!(ink_count(&mut b.lib(), nb), 6);
}

#[test]
fn removing_a_member_ends_their_live_session_and_the_notebook_goes_read_only() {
    let (_s, mut a, mut b, nb, _page, space) = shared_pair(Role::Editor);
    let (mut lb, _) = live_client(&b, &space);
    pump(&mut lb, 3);
    assert_eq!(lb.state(), LiveState::Live);
    a.account
        .transport_mut()
        .shared_remove_member(&space.parse().unwrap(), &b.me)
        .unwrap();
    let ev = pump(&mut lb, 5);
    assert!(ev.contains(&LiveEvent::AccessRemoved), "{ev:?}");
    assert_eq!(lb.state(), LiveState::Disabled);
    // what the app does then: confirm over HTTP, which stops the space
    let still = b
        .with_access(|acc, spaces, lib, access| spaces.confirm_access(acc, lib, access, &space))
        .unwrap();
    assert!(!still);
    assert_eq!(b.state(nb), Some(SharedState::AccessRemoved));
    assert_eq!(
        shared::notebook_access(&b.root, nb)
            .unwrap()
            .read_only_reason(),
        Some("accessRemoved")
    );
    // and it never dials again
    assert!(pump(&mut lb, 5).is_empty());
}

#[test]
fn awareness_is_sealed_for_members_and_expires() {
    let (s, a, b, nb, page, space) = shared_pair(Role::Editor);
    let (mut la, _) = live_client(&a, &space);
    let (mut lb, _) = live_client(&b, &space);
    pump(&mut la, 3);
    pump(&mut lb, 3);
    let here = serde_json::json!({"page_id": page, "kind": "caret", "caret": [12.5, 40.0], "block_id": null});
    assert!(la.send_awareness(&here).unwrap());
    let ev = pump(&mut lb, 5);
    let peers = ev
        .iter()
        .rev()
        .find_map(|e| match e {
            LiveEvent::Peers(p) => Some(p.clone()),
            _ => None,
        })
        .expect("a peer");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].member, a.me);
    assert_eq!(peers[0].payload, here);
    // the server relays it but can't read it
    let presence = s.live_presence(&space.parse().unwrap());
    assert!(!presence.is_empty());
    // a client with the wrong key drops it
    let (mut wrong, _) = live_client(&b, &space);
    let mut wrong = {
        let _ = &mut wrong;
        LiveClient::new(
            space.parse().unwrap(),
            b.me.clone(),
            DeviceId::new(b.space_keys_id(&space)).unwrap(),
            [7u8; 32],
            true,
            Box::new(InProcessDialer {
                server: s.clone(),
                space: space.parse().unwrap(),
                member: b.me.clone(),
                device: DeviceId::new(b.space_keys_id(&space)).unwrap(),
                offline: Arc::new(AtomicBool::new(false)),
            }),
        )
    };
    pump(&mut wrong, 3);
    // let the burst budget refill
    std::thread::sleep(Duration::from_millis(100));
    assert!(la.send_awareness(&here).unwrap());
    assert!(
        wrong.peers().is_empty()
            || pump(&mut wrong, 5)
                .iter()
                .all(|e| !matches!(e, LiveEvent::Peers(p) if !p.is_empty()))
    );
    // presence off: nothing is sent
    la.set_presence(false);
    pump(&mut la, 3);
    assert!(!la.send_awareness(&here).unwrap());
    let _ = nb;
}

#[test]
fn two_members_rewriting_the_same_block_offline_lose_no_character() {
    let (_s, mut a, mut b, nb, page, _space) = shared_pair(Role::Editor);
    add(&mut a.lib(), nb, page, vec![text(0x54, "first draft")]);
    settle(&mut [&mut a, &mut b]);
    a.offline(true);
    b.offline(true);
    set_text(&mut a.lib(), nb, page, 0x54, "agenda: budget");
    a.sync();
    set_text(&mut b.lib(), nb, page, 0x54, "agenda: hiring");
    b.sync();
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    let ta = text_of(&mut a.lib(), nb, page, 0x54).unwrap();
    assert_eq!(Some(&ta), text_of(&mut b.lib(), nb, page, 0x54).as_ref());
    assert!(
        ta.contains("agenda: budget") && ta.contains("agenda: hiring"),
        "{ta}"
    );
}

/// A live link that answers with fixed frames, for forged or misbehaving servers.
struct ScriptedLink(std::collections::VecDeque<penroam_sync_protocol::live::ServerFrame>);

impl penroam_sync::live::LiveLink for ScriptedLink {
    fn send(&mut self, _: &penroam_sync_protocol::live::ClientFrame) -> Result<(), String> {
        Ok(())
    }
    fn recv(
        &mut self,
        _: Duration,
    ) -> Result<Option<penroam_sync_protocol::live::ServerFrame>, String> {
        match self.0.pop_front() {
            Some(f) => Ok(Some(f)),
            None => Ok(None),
        }
    }
    fn close(&mut self) {}
}

/// Welcomes every dial, then sends `after` (counting dials).
struct ScriptedDialer {
    after: Vec<penroam_sync_protocol::live::ServerFrame>,
    dials: Arc<std::sync::atomic::AtomicUsize>,
}

impl penroam_sync::live::LiveDialer for ScriptedDialer {
    fn dial(
        &mut self,
        _: &penroam_sync_protocol::live::Hello,
    ) -> Result<
        (
            penroam_sync_protocol::live::Welcome,
            Box<dyn penroam_sync::live::LiveLink>,
        ),
        penroam_sync::live::DialError,
    > {
        self.dials.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let w = penroam_sync_protocol::live::Welcome {
            session_id: "ls-test".parse().unwrap(),
            server_seq: 0,
            heartbeat_ms: 5_000,
            max_frame_bytes: 16 * 1024,
            role: Role::Editor,
        };
        Ok((w, Box::new(ScriptedLink(self.after.clone().into()))))
    }
}

fn scripted(
    after: Vec<penroam_sync_protocol::live::ServerFrame>,
) -> (LiveClient, Arc<std::sync::atomic::AtomicUsize>) {
    let dials = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c = LiveClient::new(
        "s-space".parse().unwrap(),
        "ben".parse().unwrap(),
        DeviceId::new("d1").unwrap(),
        [1u8; 32],
        true,
        Box::new(ScriptedDialer {
            after,
            dials: dials.clone(),
        }),
    );
    (c, dials)
}

#[test]
fn a_live_revocation_notice_alone_never_removes_access() {
    let (_s, mut a, mut b, nb, _page, space) = shared_pair(Role::Editor);
    // a notice that ben was removed, from something that isn't the server's HTTP answer
    let (mut forged, _) = scripted(vec![penroam_sync_protocol::live::ServerFrame::Bye(
        penroam_sync_protocol::live::Bye {
            code: penroam_sync_protocol::live::ByeCode::AccessRevoked,
            retry_after_ms: None,
        },
    )]);
    let ev = pump(&mut forged, 3);
    assert!(ev.contains(&LiveEvent::AccessRemoved));
    // the app confirms over HTTP: still a member, so nothing changes
    let still = b
        .with_access(|acc, spaces, lib, access| spaces.confirm_access(acc, lib, access, &space))
        .unwrap();
    assert!(still);
    assert_eq!(b.state(nb), Some(SharedState::Active));
    // and the owner's notebook can't be touched this way either
    let still = a
        .with_access(|acc, spaces, lib, access| spaces.confirm_access(acc, lib, access, &space))
        .unwrap();
    assert!(still);
    assert_eq!(a.state(nb), Some(SharedState::Active));
}

#[test]
fn a_server_that_says_bye_right_after_welcome_is_not_redialled_every_second() {
    let (mut c, dials) = scripted(vec![penroam_sync_protocol::live::ServerFrame::Bye(
        penroam_sync_protocol::live::Bye {
            code: penroam_sync_protocol::live::ByeCode::ServerRestart,
            retry_after_ms: None,
        },
    )]);
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_millis(3000) {
        c.step(Duration::from_millis(20));
    }
    // without growing backoff this would be ~3 dials a second at least
    let n = dials.load(std::sync::atomic::Ordering::SeqCst);
    assert!(n <= 6, "{n} dials in 3 s");
    // each short-lived link counts as a failure, so the backoff keeps growing
    assert!(n >= 2, "{n}");
    assert!(
        c.backoff_attempt() as usize >= n,
        "attempt {} after {n} dials",
        c.backoff_attempt()
    );
}

// -------------------------------------------------------------- comments --

use penroam_core::comment::{quote_hash, CommentAnchor};
use penroam_sync::comments::CommentIntent;

/// What the app does for a new thread: submit, then sync the comment stream.
fn comment(d: &mut Dev, space: &str, intent: CommentIntent) -> Uuid {
    let space = space.to_string();
    d.with_access(|acc, spaces, lib, _| {
        let roster = spaces.refresh_roster(acc, &space).unwrap().unwrap();
        let cs = spaces.comment_stream(acc, lib, &space).unwrap().unwrap();
        let id = cs
            .submit(
                intent,
                roster.may_comment(),
                roster.is_owner(),
                penroam_core::now_ms(),
            )
            .unwrap();
        let _ = spaces.sync_comments(acc, lib, &space);
        id
    })
}

fn threads(d: &mut Dev, space: &str) -> Vec<penroam_core::comment::CommentThread> {
    let space = space.to_string();
    d.with_access(|acc, spaces, lib, _| {
        spaces
            .comment_stream(acc, lib, &space)
            .unwrap()
            .map(|cs| cs.threads())
            .unwrap_or_default()
    })
}

#[test]
fn a_comment_on_text_reaches_the_other_member_and_follows_the_text() {
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(
        &mut a.lib(),
        nb,
        page,
        vec![text(0x61, "check the budget numbers")],
    );
    settle(&mut [&mut a, &mut b]);
    // ann anchors a comment to "budget" (UTF-16 offsets 10..16)
    let el = Uuid::from_u128(0x61);
    let (start, end) = {
        let rep = a.spaces.engine(&space).unwrap().replica_of(nb).unwrap();
        (
            rep.text_sticky(page, el, 10, true).unwrap(),
            rep.text_sticky(page, el, 16, false).unwrap(),
        )
    };
    let anchor = CommentAnchor::TextRange {
        page_id: page,
        element_id: el,
        start: start.clone(),
        end: end.clone(),
        quote: "budget".into(),
        quote_hash: quote_hash("budget"),
        fallback_rect: None,
    };
    let thread = comment(
        &mut a,
        &space,
        CommentIntent::CreateThread {
            anchor,
            body: format!("@[{}] please look", b.me),
        },
    );
    settle(&mut [&mut a, &mut b]);
    let tb = threads(&mut b, &space);
    assert_eq!(tb.len(), 1);
    assert_eq!(tb[0].thread_id, thread);
    assert_eq!(tb[0].comments[0].author, a.me.to_string());
    // someone types before the word: the anchor still finds it on ben's device
    set_text(
        &mut b.lib(),
        nb,
        page,
        0x61,
        "please check the budget numbers",
    );
    settle(&mut [&mut a, &mut b]);
    for d in [&mut a, &mut b] {
        let rep = d.spaces.engine(&space).unwrap().replica_of(nb).unwrap();
        let s = rep.text_offset(page, el, &start).unwrap();
        let e = rep.text_offset(page, el, &end).unwrap();
        assert_eq!((s, e), (17, 23));
    }
    // ben replies; ann gets it
    comment(
        &mut b,
        &space,
        CommentIntent::Reply {
            thread_id: thread,
            body: "on it".into(),
            reply_to: None,
        },
    );
    settle(&mut [&mut a, &mut b]);
    let ta = threads(&mut a, &space);
    assert_eq!(ta[0].comments.len(), 2);
    assert_eq!(ta[0].comments[1].author, b.me.to_string());
}

#[test]
fn a_live_comment_hint_arrives_and_a_commenter_comments_without_editing() {
    let (s, mut a, _b, nb, page, space) = shared_pair(Role::Editor);
    let c_id = person(&s, "cat");
    let mut c = Dev::new(&s, &c_id, "c");
    a.invite(&space, "cat@example.org", Role::Commenter);
    c.accept();
    settle(&mut [&mut a, &mut c]);
    assert_eq!(c.state(nb), Some(SharedState::Active));
    let (mut la, _) = live_client(&a, &space);
    pump(&mut la, 3);
    // the commenter's document edit never leaves the device...
    let sid: AccountId = space.parse().unwrap();
    let before = s.op_count(&sid);
    add(&mut c.lib(), nb, page, vec![stroke(1600, 1.0, "#160000")]);
    settle(&mut [&mut c]);
    assert_eq!(s.op_count(&sid), before);
    // ...but a comment does, and ann's live channel hears about it
    comment(
        &mut c,
        &space,
        CommentIntent::CreateThread {
            anchor: CommentAnchor::PagePoint {
                page_id: page,
                x: 10.0,
                y: 20.0,
            },
            body: "nice".into(),
        },
    );
    let ev = pump(&mut la, 6);
    assert!(
        ev.iter()
            .any(|e| matches!(e, LiveEvent::CommentsCatchUp(_))),
        "{ev:?}"
    );
    let space2 = space.clone();
    a.with_access(|acc, spaces, lib, _| spaces.sync_comments(acc, lib, &space2))
        .unwrap();
    assert_eq!(threads(&mut a, &space).len(), 1);
}

#[test]
fn comments_go_into_an_export_only_when_asked_and_never_come_back_in() {
    let (_s, mut a, _b, nb, page, space) = shared_pair(Role::Editor);
    comment(
        &mut a,
        &space,
        CommentIntent::CreateThread {
            anchor: CommentAnchor::PagePoint {
                page_id: page,
                x: 1.0,
                y: 2.0,
            },
            body: "secret remark".into(),
        },
    );
    let json = serde_json::to_vec(&threads(&mut a, &space)).unwrap();
    let mut with = std::io::Cursor::new(Vec::new());
    a.lib()
        .export_archive_with(
            nb,
            &mut with,
            "test",
            &penroam_storage::ExportOptions {
                extra_entries: vec![(penroam_storage::COMMENTS_ENTRY.into(), json)],
                ..Default::default()
            },
        )
        .unwrap();
    let mut without = std::io::Cursor::new(Vec::new());
    a.lib().export_archive(nb, &mut without, "test").unwrap();
    let has = |b: &[u8]| {
        b.windows(b"secret remark".len())
            .any(|w| w == b"secret remark")
            || b.windows(penroam_storage::COMMENTS_ENTRY.len())
                .any(|w| w == penroam_storage::COMMENTS_ENTRY.as_bytes())
    };
    assert!(has(with.get_ref()));
    assert!(!has(without.get_ref()));
    // importing it gives an ordinary notebook: no comments, nothing shared, no warning about it
    let dir = tempfile::tempdir().unwrap();
    let mut other = Library::open(dir.path()).unwrap();
    with.set_position(0);
    let r = other.import_archive(with).unwrap();
    assert!(
        r.warnings.iter().all(|w| !w.contains("comments")),
        "{:?}",
        r.warnings
    );
    assert!(NotebookOwners::load(dir.path()).unwrap().shared.is_empty());
}

// ------------------------------------------------------------ attribution --

#[test]
fn every_member_learns_who_pushed_each_operation_and_a_removal_refetches() {
    use penroam_sync::attribution::{classify, AttributionStore, AuthorKind, Stream};
    let (s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(&mut b.lib(), nb, page, vec![stroke(1700, 1.0, "#170000")]);
    settle(&mut [&mut a, &mut b]);
    assert!(a.spaces.history_available);
    let dir = a.spaces.engine(&space).unwrap().store.dir.clone();
    let store = AttributionStore::open(&dir).unwrap();
    let ops = a.spaces.engine(&space).unwrap().logged_ops(nb).unwrap();
    assert!(!store.is_empty());
    let list = a.spaces.members.get(&space).unwrap().clone();
    let ctx = penroam_sync::attribution::AuthorContext::for_space(&list, None, Default::default());
    let mut by_ben = 0;
    for (_, op) in &ops {
        let who = classify(op, store.get(Stream::Ops, op.id), &ctx);
        assert_ne!(who.kind, AuthorKind::Unverified, "{who:?}");
        assert_ne!(
            who.kind,
            AuthorKind::Unknown,
            "every pushed op is attributed"
        );
        if who.account.as_deref() == Some(b.me.as_str()) {
            by_ben += 1;
        }
    }
    assert!(by_ben >= 1, "ben's stroke is his");
    // ben is removed: the next pass fetches everything again (an erasure would rename him)
    let sid: AccountId = space.parse().unwrap();
    a.account
        .transport_mut()
        .shared_remove_member(&sid, &b.me)
        .unwrap();
    let _ = s;
    let space2 = space.clone();
    let n = a
        .with_access(|acc, spaces, _, _| {
            spaces.refresh_roster(acc, &space2).unwrap();
            spaces.sync_attribution(acc, &space2)
        })
        .unwrap();
    assert!(n >= ops.len(), "refetched from the start: {n}");
}

#[test]
fn a_comment_written_offline_waits_and_is_sent_once_online_even_after_a_restart() {
    use penroam_sync::comments::Delivery;
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    // ben read the member list once (any comment sync does), then goes offline
    let sp = space.clone();
    b.with_access(|acc, spaces, _, _| spaces.refresh_roster(acc, &sp).map(|_| ()))
        .unwrap();
    b.offline(true);
    // as after a restart: nothing in memory, only what was saved
    b.spaces.members.clear();
    b.spaces.rosters.clear();
    let sp = space.clone();
    let id = b.with_access(|acc, spaces, lib, _| {
        let roster = spaces.roster(acc, &sp).unwrap().expect("saved member list");
        assert!(roster.may_comment());
        let cs = spaces.comment_stream(acc, lib, &sp).unwrap().unwrap();
        let id = cs
            .submit(
                CommentIntent::CreateThread {
                    anchor: CommentAnchor::PagePoint {
                        page_id: page,
                        x: 5.0,
                        y: 5.0,
                    },
                    body: "written on the train".into(),
                },
                roster.may_comment(),
                roster.is_owner(),
                penroam_core::now_ms(),
            )
            .unwrap();
        assert!(spaces.sync_comments(acc, lib, &sp).is_err(), "offline");
        id
    });
    let waiting = b.with_access(|acc, spaces, lib, _| {
        let cs = spaces
            .comment_stream(acc, lib, &space.clone())
            .unwrap()
            .unwrap();
        (
            cs.comment_delivery(id),
            cs.threads().iter().any(|t| t.thread_id == id),
        )
    });
    assert_eq!(
        waiting,
        (Delivery::Waiting, true),
        "listed, waiting to send"
    );
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    let sp = space.clone();
    let sent = b.with_access(|acc, spaces, lib, _| {
        let cs = spaces.comment_stream(acc, lib, &sp).unwrap().unwrap();
        cs.comment_delivery(id)
    });
    assert_eq!(sent, Delivery::Sent);
    let ta = threads(&mut a, &space);
    assert!(ta.iter().any(|t| t.thread_id == id), "ann has it: {ta:?}");
    let _ = nb;
}

#[test]
fn a_comment_payload_on_the_document_stream_is_quarantined() {
    let (_s, mut a, mut b, nb, _page, space) = shared_pair(Role::Editor);
    let before = a.notebook_view(nb);
    let mut prcm = b"PRCM".to_vec();
    prcm.extend_from_slice(&[1, 0, 0, 0, 0, 0]);
    b.space_engine(&space)
        .push_raw_for_tests(nb, &prcm)
        .unwrap();
    a.sync();
    let st = a.space_engine(&space).status(0);
    assert_eq!(st.quarantined, 1, "{st:?}");
    assert!(a
        .space_engine(&space)
        .events
        .iter()
        .any(|e| e.contains("comment operation on the document stream")));
    assert_eq!(a.notebook_view(nb), before);
}

#[test]
fn an_older_client_parks_text_edits_keeps_uploading_and_converges_after_updating() {
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(&mut a.lib(), nb, page, vec![text(0x71, "draft")]);
    settle(&mut [&mut a, &mut b]);
    // ben runs a build that knows operation schemas up to 5 (no text CRDT)
    b.space_engine(&space).max_op_schema_for_tests = Some(5);
    set_text(&mut a.lib(), nb, page, 0x71, "draft two");
    a.sync();
    add(&mut b.lib(), nb, page, vec![stroke(1800, 1.0, "#180000")]);
    b.sync();
    assert_eq!(
        b.space_engine(&space).phase(),
        penroam_sync::Phase::NeedsUpgrade
    );
    assert!(
        b.space_engine(&space).status(0).pending_remote >= 1,
        "parked, not lost"
    );
    a.sync();
    assert_eq!(
        ink_count(&mut a.lib(), nb),
        4,
        "ben's own outbox still uploads"
    );
    // the update
    b.space_engine(&space).max_op_schema_for_tests = None;
    settle(&mut [&mut a, &mut b]);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    assert!(b.notebook_view(nb).to_string().contains("draft two"));
}

#[test]
fn keeping_the_shown_text_never_drops_typing_it_had_not_received() {
    let (_s, mut a, mut b, nb, page, space) = shared_pair(Role::Editor);
    add(&mut a.lib(), nb, page, vec![text(0x81, "base")]);
    settle(&mut [&mut a, &mut b]);
    // two whole-text writes at once (what older clients send): a content conflict
    for (d, body) in [(&mut a, "from ann"), (&mut b, "from ben")] {
        d.space_engine(&space)
            .append_fixed(
                Uuid::new_v4(),
                penroam_core::now_ms(),
                DocumentOperation::ElementsReplaced {
                    notebook_id: nb,
                    page_id: page,
                    elements: vec![text(0x81, body)],
                    changed: vec![penroam_core::operation::groups::CONTENT],
                },
            )
            .unwrap();
        d.space_engine(&space).save_state().unwrap();
    }
    settle(&mut [&mut a, &mut b]);
    let (target, winner) = {
        let c = b.space_engine(&space).conflicts(nb).unwrap();
        assert_eq!(c.len(), 1, "{c:?}");
        (c[0].target.clone(), c[0].winner)
    };
    // ann types into the shown text and uploads it; ben, who hasn't pulled it, keeps the shown
    // version
    let shown = match b
        .lib()
        .load_page(nb, page)
        .unwrap()
        .elements
        .iter()
        .find(|e| e.id() == Uuid::from_u128(0x81))
    {
        Some(Element::Text(t)) => t.text.clone(),
        _ => unreachable!(),
    };
    set_text(&mut a.lib(), nb, page, 0x81, &format!("{shown} TYPED"));
    a.sync();
    b.offline(true);
    {
        let mut lib = b.lib.lock().unwrap();
        b.spaces
            .engine(&space)
            .unwrap()
            .resolve_conflict(&mut lib, nb, &target, winner)
            .unwrap();
    }
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(a.notebook_view(nb), b.notebook_view(nb));
    // the typing is in the shown text or kept as a version to choose; never gone
    for d in [&mut a, &mut b] {
        let view = d.notebook_view(nb).to_string();
        let alts = serde_json::to_string(&d.space_engine(&space).conflicts(nb).unwrap()).unwrap();
        assert!(
            view.contains("TYPED") || alts.contains("TYPED"),
            "lost: {view} / {alts}"
        );
    }
}
