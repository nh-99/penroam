//! Non-destructive restore in a shared notebook, across two members' devices: the restore
//! planner (`penroam_storage::restore`) driven the way the app drives it (base version from the
//! space's operation log, writers from attribution, the batch applied through the library), then
//! synced. Covers a restore concurrent with someone else's addition, a re-add concurrent with a
//! delete, text edited by both members (with Markdown table and LaTeX source), and a stale preview.

mod common;

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::{account, add, edit, stroke, text};
use penroam_core::{Element, OpKind, Page, PageDefaults};
use penroam_storage::restore::{
    check_precondition, ChangedChoice, RestoreError, RestorePlan, RestoreSelection,
    RECOVERED_FROM_KEY,
};
use penroam_storage::Library;
use penroam_sync::attribution::{self, AttributionStore, AuthorContext};
use penroam_sync::shared::{SpaceAccess, SpaceSet};
use penroam_sync::transport::InProcessTransport;
use penroam_sync::vault::Scope;
use penroam_sync::{FileSecretStore, SecretStore, Setup, SyncEngine, SyncTransport};
use penroam_sync_protocol::{AccountId, CreateInvitationRequest, DeviceId, Role};
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

    fn page(&self, nb: Uuid, page: Uuid) -> Page {
        self.lib.lock().unwrap().load_page(nb, page).unwrap()
    }

    fn edit(&self, nb: Uuid, page: Uuid, kind: OpKind) {
        edit(&mut self.lib.lock().unwrap(), nb, page, kind);
    }

    /// How many of the notebook's operations this device's space log holds: a version
    /// position, as `page_versions` reports them.
    fn position(&mut self, space: &str, nb: Uuid) -> u64 {
        self.space(space).logged_ops(nb).unwrap().len() as u64
    }

    /// What the app's restore preview does: the page at `position` from the space log, the
    /// current page, and the writers since then from attribution. Offline it uses what this
    /// device already has (the stored attribution; no member list), as the app does.
    fn preview(&mut self, space: &str, nb: Uuid, page: Uuid, position: u64) -> RestorePlan {
        let id = space.parse().unwrap();
        let ctx = match self.account.transport_mut().shared_members(&id) {
            Ok(members) => {
                let created = self
                    .account
                    .transport_mut()
                    .shared_list()
                    .ok()
                    .and_then(|l| l.spaces.into_iter().find(|r| r.space_id == id))
                    .map(|r| r.created_at_ms);
                AuthorContext::for_space(&members, created, HashSet::new())
            }
            Err(_) => AuthorContext::default(),
        };
        let me = self.me.to_string();
        let e = self.space(space);
        let mut store = AttributionStore::open(&e.store.dir).unwrap();
        let _ = attribution::fetch(&mut store, e.transport_mut(), attribution::Stream::Ops);
        let base = e.page_at(nb, page, position).unwrap().expect("version");
        let entries = e.logged_ops(nb).unwrap();
        let writers = attribution::writers_since(&entries, &store, &ctx, page, position);
        let head = self.page(nb, page);
        RestorePlan::new(&base, &head, &writers, &me).unwrap()
    }

    /// What the app's commit does: recheck the head, then apply the batch.
    fn commit(
        &self,
        nb: Uuid,
        plan: &RestorePlan,
        sel: &RestoreSelection,
    ) -> Result<(), RestoreError> {
        let mut lib = self.lib.lock().unwrap();
        let head = lib.load_page(nb, plan.page_id).unwrap();
        check_precondition(plan, &head)?;
        if let Some(batch) = plan.to_batch(sel)? {
            lib.apply_page_operations(nb, plan.page_id, &batch).unwrap();
        }
        Ok(())
    }

    fn offline(&mut self, off: bool) {
        self.account.transport_mut().offline = off;
        for e in &mut self.spaces.engines {
            e.transport_mut().offline = off;
        }
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
    a: Dev,
    b: Dev,
    nb: Uuid,
    page: Uuid,
    space: String,
}

/// Ann shares a notebook with `initial` on its page; Ben joins as an editor.
fn world(initial: Vec<Element>) -> World {
    let s = Arc::new(InMemoryServer::new());
    let (a_id, b_id) = (person(&s, "ann"), person(&s, "ben"));
    let mut a = Dev::new(&s, &a_id);
    let mut b = Dev::new(&s, &b_id);
    let (nb, page) = a
        .lib
        .lock()
        .unwrap()
        .create_notebook("Restore", PageDefaults::default(), None)
        .unwrap();
    if !initial.is_empty() {
        add(&mut a.lib.lock().unwrap(), nb.id, page.id, initial);
    }
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
    World {
        a,
        b,
        nb: nb.id,
        page: page.id,
        space,
    }
}

fn ids(p: &Page) -> Vec<Uuid> {
    p.elements.iter().map(Element::id).collect()
}

/// Both devices show the same page, with no element id twice.
fn converged(w: &World) -> Page {
    let (pa, pb) = (w.a.page(w.nb, w.page), w.b.page(w.nb, w.page));
    let sorted = |p: &Page| {
        let mut e = p.elements.clone();
        e.sort_by_key(Element::id);
        e
    };
    assert_eq!(sorted(&pa), sorted(&pb), "devices diverged");
    let unique: BTreeSet<Uuid> = ids(&pa).into_iter().collect();
    assert_eq!(unique.len(), pa.elements.len(), "duplicate element ids");
    pa
}

fn text_of(p: &Page, id: Uuid) -> String {
    p.elements
        .iter()
        .find_map(|e| match e {
            Element::Text(t) if t.id == id => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text block {id}"))
}

fn with_text(e: &Element, s: &str) -> Element {
    let mut e = e.clone();
    if let Element::Text(t) = &mut e {
        t.text = s.into();
    }
    e
}

#[test]
fn a_restore_and_a_concurrent_addition_both_survive() {
    let (s1, s2) = (stroke(1, 1.0, "#000000"), stroke(2, 2.0, "#000000"));
    let mut w = world(vec![s1.clone(), s2.clone()]);
    let base = w.a.position(&w.space, w.nb);
    // Ann erases a stroke, everyone sees it.
    w.a.edit(w.nb, w.page, OpKind::RemoveElements { ids: vec![s1.id()] });
    settle(&mut [&mut w.a, &mut w.b]);
    // Ann restores it while Ben, offline, draws a new stroke.
    w.b.offline(true);
    let ben_new = stroke(3, 3.0, "#0000ff");
    add(
        &mut w.b.lib.lock().unwrap(),
        w.nb,
        w.page,
        vec![ben_new.clone()],
    );
    let plan = w.a.preview(&w.space, w.nb, w.page, base);
    assert_eq!(plan.re_add.len(), 1);
    assert_eq!(plan.re_add[0].element.id(), s1.id());
    assert_eq!(plan.re_add[0].writers, vec![w.a.me.to_string()]);
    w.a.commit(w.nb, &plan, &plan.default_selection()).unwrap();
    w.b.offline(false);
    settle(&mut [&mut w.a, &mut w.b]);
    let page = converged(&w);
    let got: BTreeSet<Uuid> = ids(&page).into_iter().collect();
    assert_eq!(got, [s1.id(), s2.id(), ben_new.id()].into_iter().collect());
}

#[test]
fn a_re_add_concurrent_with_a_delete_converges_the_same_way_everywhere() {
    let s1 = stroke(1, 1.0, "#000000");
    let mut w = world(vec![s1.clone()]);
    let base = w.a.position(&w.space, w.nb);
    // Both offline: Ann deletes the stroke and restores it from before; Ben deletes it too,
    // without having seen either of Ann's operations.
    w.a.offline(true);
    w.b.offline(true);
    w.a.edit(w.nb, w.page, OpKind::RemoveElements { ids: vec![s1.id()] });
    let plan = w.a.preview(&w.space, w.nb, w.page, base);
    assert_eq!(plan.re_add.len(), 1);
    w.a.commit(w.nb, &plan, &plan.default_selection()).unwrap();
    assert_eq!(ids(&w.a.page(w.nb, w.page)), vec![s1.id()]);
    w.b.edit(w.nb, w.page, OpKind::RemoveElements { ids: vec![s1.id()] });
    w.a.offline(false);
    w.b.offline(false);
    settle(&mut [&mut w.a, &mut w.b]);
    let page = converged(&w);
    // The replica's rule: an explicit re-add (an `elementsAdded` of the same id) that is
    // concurrent with a delete wins, so the restored stroke is kept, on both devices.
    assert_eq!(
        ids(&page),
        vec![s1.id()],
        "the explicit re-add beats the delete"
    );
    assert_eq!(page.elements[0], s1);
}

#[test]
fn restoring_text_edited_by_both_keeps_every_character() {
    let table = "| a | b |\n|---|---|\n| $x^2$ | 1 |";
    let mine0 = format!("Notes\n{table}\n");
    let shared0 = "Formula: $$\\int_0^1 x\\,dx = \\frac{1}{2}$$\nEnd".to_string();
    let (mine, shared) = (text(0x7001, &mine0), text(0x7002, &shared0));
    let mut w = world(vec![mine.clone(), shared.clone()]);
    let base = w.a.position(&w.space, w.nb);
    // Ann rewrites her own block; both edit the shared block at different places.
    let mine1 = format!("Notes (draft)\n{table}\n| y | 2 |\n");
    w.a.edit(
        w.nb,
        w.page,
        OpKind::ReplaceElements {
            elements: vec![with_text(&mine, &mine1)],
        },
    );
    w.a.offline(true);
    w.b.offline(true);
    let ann_shared = format!("Ann: {shared0}");
    let ben_shared = format!("{shared0} (Ben: see \\S2)");
    w.a.edit(
        w.nb,
        w.page,
        OpKind::ReplaceElements {
            elements: vec![with_text(&shared, &ann_shared)],
        },
    );
    w.b.edit(
        w.nb,
        w.page,
        OpKind::ReplaceElements {
            elements: vec![with_text(&shared, &ben_shared)],
        },
    );
    w.a.offline(false);
    w.b.offline(false);
    settle(&mut [&mut w.a, &mut w.b]);
    let merged = text_of(&converged(&w), shared.id());
    assert!(
        merged.starts_with("Ann: ") && merged.ends_with("(Ben: see \\S2)"),
        "{merged}"
    );

    // Ann restores the page from before: her own block is reverted by default, the shared one
    // isn't; she adds it as a recovered copy.
    let plan = w.a.preview(&w.space, w.nb, w.page, base);
    let c = |id: Uuid| plan.changed.iter().find(|c| c.base.id() == id).unwrap();
    assert!(c(mine.id()).default_selected);
    assert!(!c(shared.id()).default_selected && c(shared.id()).recovered_copy);
    let mut sel = plan.default_selection();
    sel.changed
        .insert(shared.id(), ChangedChoice::RecoveredCopy);
    w.a.commit(w.nb, &plan, &sel).unwrap();
    settle(&mut [&mut w.a, &mut w.b]);
    let page = converged(&w);
    assert_eq!(
        text_of(&page, mine.id()),
        mine0,
        "my block is back, byte for byte"
    );
    assert_eq!(
        text_of(&page, shared.id()),
        merged,
        "nobody's merged text was touched"
    );
    let copy = page
        .elements
        .iter()
        .find_map(|e| match e {
            Element::Text(t) if t.extra.contains_key(RECOVERED_FROM_KEY) => Some(t.clone()),
            _ => None,
        })
        .expect("the recovered copy synced");
    assert_eq!(copy.text, shared0, "the old text, with its LaTeX intact");
    assert_ne!(copy.id, shared.id());
}

#[test]
fn a_change_between_preview_and_commit_needs_a_new_preview() {
    let s1 = stroke(1, 1.0, "#000000");
    let mut w = world(vec![s1.clone()]);
    let base = w.a.position(&w.space, w.nb);
    w.a.edit(w.nb, w.page, OpKind::RemoveElements { ids: vec![s1.id()] });
    settle(&mut [&mut w.a, &mut w.b]);
    let plan = w.a.preview(&w.space, w.nb, w.page, base);
    // Ben's stroke arrives between Ann's preview and her commit.
    let ben_new = stroke(2, 2.0, "#0000ff");
    add(
        &mut w.b.lib.lock().unwrap(),
        w.nb,
        w.page,
        vec![ben_new.clone()],
    );
    settle(&mut [&mut w.a, &mut w.b]);
    assert!(matches!(
        w.a.commit(w.nb, &plan, &plan.default_selection()),
        Err(RestoreError::ChangedSincePreview { .. })
    ));
    assert_eq!(
        ids(&w.a.page(w.nb, w.page)),
        vec![ben_new.id()],
        "nothing applied"
    );
    // A new preview sees Ben's stroke as an addition to keep, and commits.
    let again = w.a.preview(&w.space, w.nb, w.page, base);
    assert!(again.kept.iter().any(|k| k.element_id == ben_new.id()));
    w.a.commit(w.nb, &again, &again.default_selection())
        .unwrap();
    settle(&mut [&mut w.a, &mut w.b]);
    let got: BTreeSet<Uuid> = ids(&converged(&w)).into_iter().collect();
    assert_eq!(got, [s1.id(), ben_new.id()].into_iter().collect());
}
