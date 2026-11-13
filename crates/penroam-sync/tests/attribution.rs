//! Attribution against the public test server: fetching and storing it for a shared notebook's
//! space, classifying authors on both members' devices (before sharing, members, a tampered
//! attribution, erasure), writers since a version and the activity input, durability of the
//! store, access loss, and the HTTP client.

mod common;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{account, add, stroke};
use penroam_core::PageDefaults;
use penroam_storage::Library;
use penroam_sync::attribution::{
    self, classify, AttributionRecord, AttributionStore, AuthorContext, AuthorKind, Stream,
    BEFORE_SHARING, UNKNOWN, UNVERIFIED_PREFIX,
};
use penroam_sync::shared::{SpaceAccess, SpaceSet};
use penroam_sync::transport::InProcessTransport;
use penroam_sync::vault::Scope;
use penroam_sync::{FileSecretStore, SecretStore, Setup, SyncEngine, SyncTransport};
use penroam_sync_protocol::{AccountId, CreateInvitationRequest, DeviceId, ErrorCode, Role};
use penroam_sync_testkit::InMemoryServer;
use uuid::Uuid;

type Engine = SyncEngine<InProcessTransport>;

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
    fn new(s: &Arc<InMemoryServer>, acct: &AccountId) -> Dev {
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
                label: "device".into(),
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

    fn space(&mut self, id: &str) -> &mut Engine {
        self.spaces.engine(id).expect("space engine")
    }

    /// Fetches the space's attribution into its store and returns it.
    fn attribution(&mut self, space: &str) -> AttributionStore {
        let e = self.space(space);
        let mut store = AttributionStore::open(&e.store.dir).unwrap();
        attribution::fetch(&mut store, e.transport_mut(), Stream::Ops).unwrap();
        store
    }

    /// The author context of a space as this device sees it.
    fn context(&mut self, space: &str, transplanted: HashSet<Uuid>) -> AuthorContext {
        let id = space.parse().unwrap();
        let t = self.account.transport_mut();
        let members = t.shared_members(&id).unwrap();
        let created = t
            .shared_list()
            .unwrap()
            .spaces
            .into_iter()
            .find(|r| r.space_id == id)
            .map(|r| r.created_at_ms);
        AuthorContext::for_space(&members, created, transplanted)
    }
}

fn settle(devs: &mut [&mut Dev]) {
    for _ in 0..4 {
        for d in devs.iter_mut() {
            d.sync();
        }
    }
}

struct World {
    s: Arc<InMemoryServer>,
    a: Dev,
    b: Dev,
    nb: Uuid,
    page: Uuid,
    space: String,
    /// Strokes added before sharing, and after it by ann and by ben.
    before: Vec<Uuid>,
    by_ann: Uuid,
    by_ben: Uuid,
    /// Ops the notebook had when ben joined.
    position: u64,
}

fn world() -> World {
    let s = Arc::new(InMemoryServer::new());
    let (a_id, b_id) = (person(&s, "ann"), person(&s, "ben"));
    let mut a = Dev::new(&s, &a_id);
    let mut b = Dev::new(&s, &b_id);
    let (nb, page) = a
        .lib
        .lock()
        .unwrap()
        .create_notebook("Field notes", PageDefaults::default(), None)
        .unwrap();
    let before: Vec<_> = (0..3).map(|i| stroke(i, 1.0, "#000000")).collect();
    for e in &before {
        add(&mut a.lib.lock().unwrap(), nb.id, page.id, vec![e.clone()]);
    }
    a.sync();
    // the space's creation time must be after the strokes' own times
    std::thread::sleep(std::time::Duration::from_millis(5));
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
                role: Role::Editor,
                idempotency_key: "inv".into(),
            },
        )
        .unwrap();
    let _ = b.with_access(|acc, spaces, lib, _| spaces.refresh(acc, lib));
    let inv = b.spaces.invitations[0].invitation_id.clone();
    b.with_access(|acc, spaces, lib, access| spaces.accept(acc, lib, access, &inv))
        .unwrap();
    settle(&mut [&mut a, &mut b]);
    let position = b.space(&space).logged_ops(nb.id).unwrap().len() as u64;
    let (ann, ben) = (stroke(10, 2.0, "#ff0000"), stroke(11, 3.0, "#0000ff"));
    add(
        &mut a.lib.lock().unwrap(),
        nb.id,
        page.id,
        vec![ann.clone()],
    );
    add(
        &mut b.lib.lock().unwrap(),
        nb.id,
        page.id,
        vec![ben.clone()],
    );
    settle(&mut [&mut a, &mut b]);
    World {
        s,
        a,
        b,
        nb: nb.id,
        page: page.id,
        space,
        before: before.iter().map(|e| e.id()).collect(),
        by_ann: ann.id(),
        by_ben: ben.id(),
        position,
    }
}

/// The author key of the op that added element `el`.
fn key_of(
    ops: &[(
        penroam_core::operation::LocalOperation,
        penroam_core::activity::OpAttribution,
    )],
    el: Uuid,
) -> String {
    ops.iter()
        .find(|(op, _)| match &op.body {
            penroam_core::operation::DocumentOperation::ElementsAdded { elements, .. } => {
                elements.iter().any(|e| e.id() == el)
            }
            _ => false,
        })
        .and_then(|(_, a)| a.author.clone())
        .unwrap_or_else(|| panic!("no op adds {el}"))
}

#[test]
fn both_members_see_the_same_authors() {
    let mut w = world();
    let (ann, ben) = (w.a.me.to_string(), w.b.me.to_string());
    // Ann's devices know the transplanted ids; Ben's go by the space's creation time.
    let account_ops: Vec<Uuid> =
        w.a.account
            .logged_ops(w.nb)
            .unwrap()
            .into_iter()
            .map(|(_, op)| op.id)
            .collect();
    let ann_ctx = {
        let t = attribution::transplanted_ids(&w.space, &account_ops);
        let mut c = w.a.context(&w.space, t);
        c.shared_at_ms = None;
        c
    };
    let ben_ctx = w.b.context(&w.space, HashSet::new());
    for (dev, ctx) in [(&mut w.a, &ann_ctx), (&mut w.b, &ben_ctx)] {
        let store = dev.attribution(&w.space);
        assert!(!store.is_empty());
        let entries = dev.space(&w.space).logged_ops(w.nb).unwrap();
        let ops = attribution::notebook_ops(&entries, &store, ctx);
        for el in &w.before {
            assert_eq!(key_of(&ops, *el), BEFORE_SHARING);
        }
        assert_eq!(key_of(&ops, w.by_ann), ann);
        assert_eq!(key_of(&ops, w.by_ben), ben);
        // Accepted times are there for attributed ops.
        assert!(ops.iter().any(|(_, a)| a.accepted_at.is_some()));
        // Writers since Ben joined: exactly the two later strokes, each by its author.
        let writers = attribution::writers_since(&entries, &store, ctx, w.page, w.position);
        assert_eq!(writers.get(&w.by_ann), Some(&[ann.clone()].into()));
        assert_eq!(writers.get(&w.by_ben), Some(&[ben.clone()].into()));
        assert!(w.before.iter().all(|el| !writers.contains_key(el)));
        // Member display names come from the member list.
        let (_, display, kind) = attribution::describe_key(&ben, ctx);
        assert_eq!(
            (display.as_str(), kind),
            ("ben@example.org", AuthorKind::Member)
        );
    }
}

#[test]
fn a_member_who_backdates_an_operation_is_still_its_author() {
    let mut w = world();
    let ctx = w.a.context(&w.space, HashSet::new());
    assert_eq!(ctx.owner.as_deref(), Some(w.a.me.as_str()));
    let store = w.a.attribution(&w.space);
    let entries = w.a.space(&w.space).logged_ops(w.nb).unwrap();
    let is_add_of = |op: &penroam_core::operation::LocalOperation, el: Uuid| {
        matches!(&op.body, penroam_core::operation::DocumentOperation::ElementsAdded { elements, .. }
            if elements.iter().any(|e| e.id() == el))
    };
    // Ben's real operation, with its clock set to long before the space existed.
    let (_, ben_op) = entries
        .iter()
        .find(|(_, op)| is_add_of(op, w.by_ben))
        .unwrap();
    let mut backdated = ben_op.clone();
    backdated.at = 1;
    let rec = store.get(Stream::Ops, ben_op.id);
    let a = classify(&backdated, rec, &ctx);
    assert_eq!(
        (a.kind, a.key.as_str()),
        (AuthorKind::Member, w.b.me.as_str())
    );
    // Even if its id were claimed as transplanted.
    let mut forged_ctx = ctx.clone();
    forged_ctx.transplanted.insert(ben_op.id);
    assert_eq!(
        classify(&backdated, rec, &forged_ctx).kind,
        AuthorKind::Member
    );
    // And it shows as edited offline in the activity list.
    let acts = penroam_core::activity::summarize(
        &[(
            backdated.clone(),
            penroam_core::activity::OpAttribution {
                author: Some(a.key.clone()),
                accepted_at: a.accepted_at,
            },
        )],
        Default::default(),
    );
    assert!(acts[0].edited_offline);
    // The owner's own history from before sharing still is "before sharing".
    let (_, old) = entries
        .iter()
        .find(|(_, op)| is_add_of(op, w.before[0]))
        .unwrap();
    assert_eq!(
        classify(old, store.get(Stream::Ops, old.id), &ctx).kind,
        AuthorKind::BeforeSharing
    );
    // An owner op without attribution yet is unknown, not "before sharing".
    assert_eq!(classify(old, None, &ctx).kind, AuthorKind::Unknown);
}

#[test]
fn a_mismatched_device_is_unverified_and_unknown_is_unknown() {
    let mut w = world();
    let ctx = w.b.context(&w.space, HashSet::new());
    let store = w.b.attribution(&w.space);
    let entries = w.b.space(&w.space).logged_ops(w.nb).unwrap();
    let (_, ben_op) = entries
        .iter()
        .find(|(_, op)| {
            op.at >= ctx.shared_at_ms.unwrap() && store.get(Stream::Ops, op.id).is_some()
        })
        .unwrap();
    let mut forged = store.get(Stream::Ops, ben_op.id).unwrap().clone();
    forged.device = Uuid::new_v4().hyphenated().to_string();
    let a = classify(ben_op, Some(&forged), &ctx);
    assert_eq!(a.kind, AuthorKind::Unverified);
    assert!(a.key.starts_with(UNVERIFIED_PREFIX));
    let none = classify(ben_op, None, &ctx);
    assert_eq!(
        (none.kind, none.key.as_str()),
        (AuthorKind::Unknown, UNKNOWN)
    );
    // A forged attribution never counts as the user's own write.
    let mut c = ctx.clone();
    c.shared_at_ms = None;
    assert_ne!(classify(ben_op, Some(&forged), &c).key, w.b.me.to_string());
}

#[test]
fn a_device_moved_to_another_account_is_unverified() {
    let mut w = world();
    let ctx = w.b.context(&w.space, HashSet::new());
    let real = w.b.attribution(&w.space);
    let entries = w.b.space(&w.space).logged_ops(w.nb).unwrap();
    let (_, ben_op) = entries
        .iter()
        .find(|(_, op)| {
            matches!(&op.body, penroam_core::operation::DocumentOperation::ElementsAdded { elements, .. }
                if elements.iter().any(|e| e.id() == w.by_ben))
        })
        .unwrap();
    let genuine = real.get(Stream::Ops, ben_op.id).unwrap().clone();
    assert_eq!(
        classify(ben_op, Some(&genuine), &ctx).kind,
        AuthorKind::Member
    );
    // A server that later claims Ben's device (and op) for Ann.
    let dir = tempfile::tempdir().unwrap();
    let mut store = AttributionStore::open(dir.path()).unwrap();
    store.append(vec![genuine.clone()]).unwrap();
    assert_eq!(store.pinned_account(&genuine.device), Some(w.b.me.as_str()));
    let mut moved = genuine.clone();
    moved.account = w.a.me.to_string();
    store.append(vec![moved.clone()]).unwrap();
    let v = store.pin_violations();
    assert_eq!(v.len(), 1);
    assert_eq!(
        (v[0].pinned.as_str(), v[0].claimed.as_str()),
        (w.b.me.as_str(), w.a.me.as_str())
    );
    assert!(v[0].event().starts_with("security:"));
    let rec = store.get(Stream::Ops, ben_op.id).unwrap();
    assert!(rec.pin_mismatch);
    let a = classify(ben_op, Some(rec), &ctx);
    assert_eq!(a.kind, AuthorKind::Unverified);
    assert!(a.key.starts_with(UNVERIFIED_PREFIX));
    // The pin is rebuilt from the file on open, so the mismatch survives a restart.
    let again = AttributionStore::open(dir.path()).unwrap();
    assert_eq!(again.pin_violations().len(), 1);
    assert!(again.get(Stream::Ops, ben_op.id).unwrap().pin_mismatch);
    // A move to an erased tombstone is how erasure looks, and is accepted.
    let mut erased = genuine;
    erased.account = format!("erased-{}", "ab".repeat(16));
    let dir2 = tempfile::tempdir().unwrap();
    let mut store = AttributionStore::open(dir2.path()).unwrap();
    store.append(vec![moved.clone()]).unwrap();
    store.append(vec![erased]).unwrap();
    assert!(store.pin_violations().is_empty());
    assert_eq!(
        classify(ben_op, store.get(Stream::Ops, ben_op.id), &ctx).kind,
        AuthorKind::Erased
    );
}

#[test]
fn erasure_turns_authors_into_tombstones_after_a_refresh() {
    let mut w = world();
    let mut store = w.a.attribution(&w.space);
    w.s.erase_account(&w.b.me).unwrap();
    let ctx = w.a.context(&w.space, HashSet::new());
    let e = w.a.space(&w.space);
    attribution::refresh(&mut store, e.transport_mut(), Stream::Ops).unwrap();
    let entries = e.logged_ops(w.nb).unwrap();
    let ops = attribution::notebook_ops(&entries, &store, &ctx);
    let key = key_of(&ops, w.by_ben);
    assert!(key.starts_with("erased-"), "{key}");
    assert_eq!(attribution::describe_key(&key, &ctx).2, AuthorKind::Erased);
    // The file was compacted: Ben's real account id is nowhere in it any more.
    let file = std::fs::read_to_string(e.store.dir.join(attribution::ATTRIBUTION_FILE)).unwrap();
    assert!(
        !file.contains(w.b.me.as_str()),
        "pre-erasure id left on disk"
    );
    assert!(file.contains(&key));
    assert!(file
        .lines()
        .all(|l| serde_json::from_str::<AttributionRecord>(l).is_ok()));
    // The refreshed store survives a reopen.
    let again = AttributionStore::open(&e.store.dir).unwrap();
    let ben_op = ops
        .iter()
        .find(|(_, a)| a.author.as_deref() == Some(key.as_str()))
        .unwrap()
        .0
        .id;
    assert_eq!(again.get(Stream::Ops, ben_op).unwrap().account, key);
}

#[test]
fn the_store_is_durable_append_only_and_fetches_from_where_it_stopped() {
    let mut w = world();
    let mut store = w.b.attribution(&w.space);
    let n = store.len();
    let last = store.last_seq(Stream::Ops);
    assert_eq!(last as usize, n);
    let dir = w.b.space(&w.space).store.dir.clone();
    // A torn line at the end is ignored, and the next append starts on a fresh line.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(attribution::ATTRIBUTION_FILE))
            .unwrap();
        f.write_all(br#"{"stream":"ops","seq":99"#).unwrap();
    }
    let reopened = AttributionStore::open(&dir).unwrap();
    assert_eq!((reopened.len(), reopened.last_seq(Stream::Ops)), (n, last));
    // Nothing new: nothing fetched twice.
    let e = w.b.space(&w.space);
    assert_eq!(
        attribution::fetch(&mut store, e.transport_mut(), Stream::Ops).unwrap(),
        0
    );
    // A copy of the store as it is now, which the worker won't touch.
    let copy = tempfile::tempdir().unwrap();
    std::fs::copy(
        dir.join(attribution::ATTRIBUTION_FILE),
        copy.path().join(attribution::ATTRIBUTION_FILE),
    )
    .unwrap();
    add(
        &mut w.b.lib.lock().unwrap(),
        w.nb,
        w.page,
        vec![stroke(20, 5.0, "#00ff00")],
    );
    // The worker fetches attribution after each space sync.
    settle(&mut [&mut w.a, &mut w.b]);
    let mut store = AttributionStore::open(&dir).unwrap();
    assert_eq!(store.last_seq(Stream::Ops), last + 1);
    let e = w.b.space(&w.space);
    assert_eq!(
        attribution::fetch(&mut store, e.transport_mut(), Stream::Ops).unwrap(),
        0,
        "already stored by the worker"
    );
    // The copy fetches exactly the one new entry, from where it stopped.
    let mut old = AttributionStore::open(copy.path()).unwrap();
    assert_eq!(
        attribution::fetch(&mut old, e.transport_mut(), Stream::Ops).unwrap(),
        1
    );
    assert_eq!(old.last_seq(Stream::Ops), last + 1);
    let lines = std::fs::read_to_string(dir.join(attribution::ATTRIBUTION_FILE)).unwrap();
    assert!(lines.lines().filter(|l| l.contains("\"seq\":99")).count() == 1);
    // Duplicates are skipped on append.
    let rec: AttributionRecord = serde_json::from_str(
        lines
            .lines()
            .find(|l| l.starts_with('{') && l.ends_with('}'))
            .unwrap(),
    )
    .unwrap();
    let before = std::fs::metadata(dir.join(attribution::ATTRIBUTION_FILE))
        .unwrap()
        .len();
    store.append(vec![rec]).unwrap();
    assert_eq!(
        std::fs::metadata(dir.join(attribution::ATTRIBUTION_FILE))
            .unwrap()
            .len(),
        before
    );
}

#[test]
fn a_removed_member_gets_access_revoked() {
    let mut w = world();
    w.s.shared_remove_member(&w.a.me, &w.space.parse().unwrap(), &w.b.me)
        .unwrap();
    let e = w.b.space(&w.space);
    let mut store = AttributionStore::open(&e.store.dir).unwrap();
    let err = attribution::fetch(&mut store, e.transport_mut(), Stream::Ops).unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::AccessRevoked));
}

#[test]
fn comment_attribution_has_its_own_stream() {
    let mut w = world();
    let e = w.b.space(&w.space);
    let mut store = AttributionStore::open(&e.store.dir).unwrap();
    // no comments yet
    assert_eq!(
        attribution::fetch(&mut store, e.transport_mut(), Stream::Comments).unwrap(),
        0
    );
    assert_eq!(store.last_seq(Stream::Comments), 0);
}

#[cfg(feature = "http")]
#[test]
fn the_http_client_reads_the_same_attribution() {
    let mut w = world();
    let h = penroam_sync_testkit::http::spawn("127.0.0.1:0", w.s.clone()).unwrap();
    let dev = w.b.space(&w.space).device_id().to_string();
    let (token, _) =
        penroam_sync::transport::HttpTransport::test_session(&h.base_url, &w.space, Some(&dev))
            .unwrap();
    let mut http = attribution::HttpAttribution::new(&h.base_url, token);
    let dir = tempfile::tempdir().unwrap();
    let mut store = AttributionStore::open(dir.path()).unwrap();
    let n = attribution::fetch(&mut store, &mut http, Stream::Ops).unwrap();
    let in_process = w.b.attribution(&w.space);
    assert_eq!(n, in_process.len());
    assert_eq!(
        store.last_seq(Stream::Ops),
        in_process.last_seq(Stream::Ops)
    );
}
