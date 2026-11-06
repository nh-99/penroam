//! Shared helpers: independent clients (own library folder, own keys, own
//! secret store) talking to one test server.
#![allow(dead_code)]

use penroam_core::*;
use penroam_storage::Library;
use penroam_sync::transport::InProcessTransport;
use penroam_sync::{FileSecretStore, Setup, SyncEngine};
use penroam_sync_protocol::{AccountId, DeviceId};
use penroam_sync_testkit::InMemoryServer;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

pub type Engine = SyncEngine<InProcessTransport>;

pub struct Client {
    pub lib: Library,
    pub engine: Engine,
    pub root: PathBuf,
    pub server: Arc<InMemoryServer>,
    pub account: AccountId,
    pub _dir: Option<tempfile::TempDir>,
}

pub fn account(server: &InMemoryServer, name: &str) -> AccountId {
    let a = AccountId::new(name).unwrap();
    server.create_account(&a);
    a
}

fn transport(
    server: &Arc<InMemoryServer>,
    account: &AccountId,
    device: &str,
) -> InProcessTransport {
    InProcessTransport::new(
        server.clone(),
        account.clone(),
        DeviceId::new(device).unwrap(),
    )
}

fn setup(root: &Path, account: &AccountId, label: &str) -> Setup {
    Setup {
        library_root: root.to_path_buf(),
        server_url: "in-process".into(),
        account_id: account.as_str().into(),
        label: label.into(),
    }
}

fn secrets(root: &Path) -> Box<FileSecretStore> {
    Box::new(FileSecretStore::new(root.join("sync-secrets")))
}

impl Client {
    /// First device: creates the account keys. Returns the recovery code.
    pub fn first(
        server: &Arc<InMemoryServer>,
        account: &AccountId,
        label: &str,
    ) -> (Client, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let lib = Library::open(&root).unwrap();
        let (engine, code) = Engine::create_account(
            setup(&root, account, label),
            transport(server, account, "pending"),
            secrets(&root),
        )
        .unwrap();
        (
            Client {
                lib,
                engine,
                root,
                server: server.clone(),
                account: account.clone(),
                _dir: Some(dir),
            },
            code,
        )
    }

    /// New device approved by `trusted` through the pairing flow.
    pub fn paired(
        server: &Arc<InMemoryServer>,
        account: &AccountId,
        label: &str,
        trusted: &mut Client,
    ) -> Client {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let lib = Library::open(&root).unwrap();
        let (mut engine, code) = Engine::begin_pairing(
            setup(&root, account, label),
            transport(server, account, "pending"),
            secrets(&root),
        )
        .unwrap();
        assert_eq!(engine.poll_enrollment().unwrap(), None, "not approved yet");
        let sas_trusted = trusted.engine.approve_device(&code, label).unwrap();
        let sas_new = engine.poll_enrollment().unwrap().expect("approved");
        assert_eq!(sas_trusted, sas_new, "both screens show the same code");
        Client {
            lib,
            engine,
            root,
            server: server.clone(),
            account: account.clone(),
            _dir: Some(dir),
        }
    }

    /// Pairs an existing library folder (for example after a disconnect).
    pub fn pair_existing(
        server: &Arc<InMemoryServer>,
        account: &AccountId,
        label: &str,
        root: PathBuf,
        dir: Option<tempfile::TempDir>,
        trusted: &mut Client,
    ) -> Client {
        let lib = Library::open(&root).unwrap();
        let (mut engine, code) = Engine::begin_pairing(
            setup(&root, account, label),
            transport(server, account, "pending"),
            secrets(&root),
        )
        .unwrap();
        trusted.engine.approve_device(&code, label).unwrap();
        engine.poll_enrollment().unwrap().expect("approved");
        Client {
            lib,
            engine,
            root,
            server: server.clone(),
            account: account.clone(),
            _dir: dir,
        }
    }

    /// New device enrolled with the recovery code.
    pub fn recovered(server: &Arc<InMemoryServer>, account: &AccountId, code: &str) -> Client {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let lib = Library::open(&root).unwrap();
        let engine = Engine::enroll_with_recovery(
            setup(&root, account, "recovered"),
            transport(server, account, "pending"),
            secrets(&root),
            code,
        )
        .unwrap();
        Client {
            lib,
            engine,
            root,
            server: server.clone(),
            account: account.clone(),
            _dir: Some(dir),
        }
    }

    pub fn sync(&mut self) -> penroam_sync::engine::Result<penroam_sync::ReconcileReport> {
        self.engine.reconcile_owned(&mut self.lib)
    }

    /// Simulates a process kill (nothing flushed) and restarts library and engine.
    pub fn restart(self) -> Client {
        let Client {
            lib,
            engine,
            root,
            server,
            account,
            _dir,
        } = self;
        let device = engine.device_id().to_string();
        lib.crash_for_tests();
        drop(engine);
        let lib = Library::open(&root).unwrap();
        let engine =
            Engine::open(&root, transport(&server, &account, &device), secrets(&root)).unwrap();
        Client {
            lib,
            engine,
            root,
            server,
            account,
            _dir,
        }
    }

    pub fn offline(&mut self, off: bool) {
        self.engine.transport_mut().offline = off;
    }

    pub fn status(&self) -> penroam_sync::SyncStatus {
        self.engine.status(self.lib.last_seq())
    }
}

pub fn stroke(i: u64, x: f64, color: &str) -> Element {
    Element::Ink(InkStroke {
        id: Uuid::from_u128(0x5000_0000 + i as u128 + ((x as u128) << 40)),
        z: i as i64 + 1,
        tool: InkTool::Pen,
        color: color.into(),
        opacity: 1.0,
        width: 2.0,
        pressure: true,
        source: "test".into(),
        input: Extra::new(),
        points: (0..8)
            .map(|k| InkPoint {
                x: x + k as f64,
                y: 10.0 + i as f64,
                pressure: 0.5,
                tilt_x: 0.0,
                tilt_y: 0.0,
                time_us: k * 1000,
                twist: None,
                flags: None,
            })
            .collect(),
        extra: Extra::new(),
    })
}

pub fn text(id: u128, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::from_u128(id),
        z: 1000,
        x: 10.0,
        y: 10.0,
        width: 200.0,
        height: 40.0,
        text: s.into(),
        font_size: 16.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        row_pitch: None,
        extra: Extra::new(),
    })
}

pub fn edit(lib: &mut Library, nb: Uuid, page: Uuid, kind: OpKind) {
    let rev = lib.load_page(nb, page).unwrap().revision;
    lib.apply_page_operations(
        nb,
        page,
        &OperationBatch {
            batch_id: Uuid::new_v4(),
            base_revision: rev,
            ops: vec![PageOperation {
                op_id: Uuid::new_v4(),
                kind,
            }],
        },
    )
    .unwrap();
}

pub fn add(lib: &mut Library, nb: Uuid, page: Uuid, elements: Vec<Element>) {
    edit(lib, nb, page, OpKind::AddElements { elements });
}

/// Order-independent, device-independent view of a whole library.
pub fn view(lib: &mut Library) -> serde_json::Value {
    let mut nbs = Vec::new();
    for id in lib.notebook_ids() {
        let nb = lib.notebook(id).unwrap().clone();
        let mut trashed: Vec<String> = nb.trashed_pages.iter().map(|t| t.id.to_string()).collect();
        trashed.sort();
        let mut assets: Vec<String> = nb.assets.iter().map(|a| a.sha256.clone()).collect();
        assets.sort();
        let mut pages = Vec::new();
        // live pages in order; the trash as a set (its display order is local)
        let mut ids: Vec<Uuid> = nb.page_order.clone();
        let mut t: Vec<Uuid> = nb.trashed_pages.iter().map(|t| t.id).collect();
        t.sort();
        ids.extend(t);
        for pid in ids {
            let p = lib.load_page(id, pid).unwrap();
            let mut els = p.elements.clone();
            els.sort_by_key(|e| (e.z(), e.id()));
            pages.push(serde_json::json!({
                "id": pid, "template": p.template, "background": p.background,
                "elements": els,
            }));
        }
        nbs.push(serde_json::json!({
            "id": id, "title": nb.title, "cover": nb.cover, "deleted": nb.deleted_at.is_some(),
            "order": nb.page_order, "trashed": trashed, "assets": assets, "pages": pages,
        }));
    }
    serde_json::Value::Array(nbs)
}

pub fn ink_count(lib: &mut Library, nb: Uuid) -> usize {
    let n = lib.notebook(nb).unwrap().clone();
    n.all_page_ids()
        .collect::<Vec<_>>()
        .into_iter()
        .map(|p| {
            lib.load_page(nb, p)
                .unwrap()
                .elements
                .iter()
                .filter(|e| matches!(e, Element::Ink(_)))
                .count()
        })
        .sum()
}

/// Syncs every client until no one receives anything new (bounded).
pub fn settle(clients: &mut [&mut Client]) {
    for _ in 0..4 {
        for c in clients.iter_mut() {
            c.sync().unwrap();
        }
    }
}
