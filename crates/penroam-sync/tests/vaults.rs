//! Sign-in-only sync and passphrase-locked vaults, end to end against the public test server.

mod common;

use std::sync::Arc;

use common::{Client, *};
use penroam_core::PageDefaults;
use penroam_storage::Library;
use penroam_sync::crypto::RootSecret;
use penroam_sync::transport::InProcessTransport;
use penroam_sync::vault::{self, NotebookOwners, Scope, VaultKey};
use penroam_sync::{FileSecretStore, SecretStore, Setup, SyncEngine, SyncError, SyncTransport};
use penroam_sync_protocol::{
    AccountId, AccountKeyDocument, CreateVaultRequest, DeviceId, ErrorCode, UpdateVaultLockRequest,
    VaultRecord,
};
use penroam_sync_testkit::{InMemoryServer, RequestError};
use uuid::Uuid;

const PASSPHRASE: &str = "correct horse battery staple";
type VaultEngine = SyncEngine<InProcessTransport>;

fn server() -> Arc<InMemoryServer> {
    Arc::new(InMemoryServer::new())
}

fn transport(s: &Arc<InMemoryServer>, account: &AccountId) -> InProcessTransport {
    InProcessTransport::new(
        s.clone(),
        account.clone(),
        DeviceId::new("pending").unwrap(),
    )
}

fn vault_setup(c: &Client, id: &AccountId) -> Setup {
    Setup {
        library_root: c.root.clone(),
        scope: Scope::vault(id.as_str()).unwrap(),
        device_id: None,
        server_url: "in-process".into(),
        account_id: id.as_str().into(),
        label: "vault device".into(),
    }
}

fn vault_secrets(c: &Client) -> Box<FileSecretStore> {
    Box::new(FileSecretStore::new(c.root.join("sync-secrets")))
}

/// What the app does to make a vault: returns its engine, record and recovery code.
fn create_vault(s: &Arc<InMemoryServer>, c: &Client) -> (VaultEngine, VaultRecord, String) {
    let root = RootSecret::generate().unwrap();
    let record = transport(s, &c.account)
        .vault_create(&CreateVaultRequest {
            lock: vault::new_lock(&root, PASSPHRASE).unwrap(),
            name: vault::seal_name(&root, "Journal"),
        })
        .unwrap();
    let e = SyncEngine::open_vault(
        vault_setup(c, &record.vault_id),
        transport(s, &record.vault_id),
        vault_secrets(c),
        &root,
    )
    .unwrap();
    (e, record, root.recovery_code())
}

fn unlock(s: &Arc<InMemoryServer>, c: &Client, r: &VaultRecord, key: VaultKey<'_>) -> VaultEngine {
    let root = vault::root_for(&r.lock, &key).unwrap();
    SyncEngine::open_vault(
        vault_setup(c, &r.vault_id),
        transport(s, &r.vault_id),
        vault_secrets(c),
        &root,
    )
    .unwrap()
}

/// A notebook created inside a vault, assigned under the same (here exclusive) library access.
fn vault_notebook(c: &mut Client, vault_id: &AccountId, title: &str) -> Uuid {
    let (nb, _) = c
        .lib
        .create_notebook(title, PageDefaults::default(), None)
        .unwrap();
    let mut owners = NotebookOwners::load(&c.root).unwrap();
    owners.notebooks.insert(nb.id, vault_id.as_str().into());
    owners.save(&c.root).unwrap();
    nb.id
}

fn sync_vault(c: &mut Client, e: &mut VaultEngine) {
    e.reconcile_owned(&mut c.lib).unwrap();
}

#[test]
fn a_new_device_needs_only_a_sign_in() {
    let s = server();
    let acct = account(&s, "acct-a");
    let mut a = Client::joined(&s, &acct, "a");
    a.lib
        .create_notebook("Shared", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    assert_eq!(b.lib.notebook_ids().len(), 1);
    assert_eq!(a.engine.devices().len(), 1, "a has not refreshed yet");
    a.sync().unwrap();
    assert_eq!(a.engine.devices().len(), 2);
}

#[test]
fn an_older_account_needs_its_recovery_code_once() {
    let s = server();
    let acct = account(&s, "acct-b");
    let dir = tempfile::tempdir().unwrap();
    Library::open(dir.path()).unwrap();
    let setup = |root: &std::path::Path, label: &str| Setup {
        library_root: root.to_path_buf(),
        scope: Scope::Account,
        device_id: None,
        server_url: "x".into(),
        account_id: acct.as_str().into(),
        label: label.into(),
    };
    let (_, code) = SyncEngine::create_account(
        setup(dir.path(), "first"),
        transport(&s, &acct),
        Box::new(FileSecretStore::new(dir.path().join("s"))),
    )
    .unwrap();
    let other = tempfile::tempdir().unwrap();
    Library::open(other.path()).unwrap();
    let refused = SyncEngine::connect(
        setup(other.path(), "second"),
        transport(&s, &acct),
        Box::new(FileSecretStore::new(other.path().join("s"))),
    );
    assert!(matches!(refused, Err(SyncError::NeedsRecoveryCode)));
    Client::recovered(&s, &acct, &code);
    Client::joined(&s, &acct, "third");
}

#[test]
fn a_wrong_recovery_code_stores_nothing() {
    let s = server();
    let acct = account(&s, "acct-c");
    let dir = tempfile::tempdir().unwrap();
    Library::open(dir.path()).unwrap();
    let setup = |root: &std::path::Path| Setup {
        library_root: root.to_path_buf(),
        scope: Scope::Account,
        device_id: None,
        server_url: "x".into(),
        account_id: acct.as_str().into(),
        label: "x".into(),
    };
    SyncEngine::create_account(
        setup(dir.path()),
        transport(&s, &acct),
        Box::new(FileSecretStore::new(dir.path().join("s"))),
    )
    .unwrap();
    let other = tempfile::tempdir().unwrap();
    let wrong = RootSecret::generate().unwrap().recovery_code();
    let r = SyncEngine::enroll_with_recovery(
        setup(other.path()),
        transport(&s, &acct),
        Box::new(FileSecretStore::new(other.path().join("s"))),
        &wrong,
    );
    assert!(r.is_err());
    assert!(matches!(
        s.account_key_get(&acct),
        Err(RequestError::Api(b)) if b.code == ErrorCode::NotFound
    ));
}

#[test]
fn a_vault_syncs_between_unlocked_devices_and_stays_out_of_the_main_account() {
    let s = server();
    let acct = account(&s, "acct-d");
    let mut a = Client::joined(&s, &acct, "a");
    let (mut va, record, _) = create_vault(&s, &a);
    let secret = vault_notebook(&mut a, &record.vault_id, "Secret");
    let (open, _) = a
        .lib
        .create_notebook("Open", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    sync_vault(&mut a, &mut va);
    assert!(a.engine.store.oplog.indexes_for(&secret).is_empty());
    assert!(va.store.oplog.indexes_for(&open.id).is_empty());
    assert!(!va.store.oplog.indexes_for(&secret).is_empty());

    // a device that signed in sees the ordinary notebook only, and the vault as a locked record
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    assert_eq!(b.lib.notebook_ids(), vec![open.id]);
    let listed = transport(&s, &acct).vault_list().unwrap().vaults;
    assert_eq!(listed.len(), 1);

    // writes keep flowing to it while the vault is locked there
    let mut vb = unlock(&s, &b, &listed[0], VaultKey::Passphrase(PASSPHRASE));
    sync_vault(&mut b, &mut vb);
    assert_eq!(b.lib.notebook(secret).unwrap().title, "Secret");
    assert_eq!(
        NotebookOwners::load(&b.root).unwrap().owner(&secret),
        Some(record.vault_id.as_str()),
        "claimed before install, or the main engine would upload it"
    );

    // installing a vault notebook doesn't make the main engine upload it
    b.sync().unwrap();
    assert!(b.engine.store.oplog.indexes_for(&secret).is_empty());
}

#[test]
fn a_wrong_passphrase_opens_nothing_and_a_recovery_code_still_works() {
    let s = server();
    let acct = account(&s, "acct-e");
    let mut a = Client::joined(&s, &acct, "a");
    let (mut va, record, code) = create_vault(&s, &a);
    let nb = vault_notebook(&mut a, &record.vault_id, "Secret");
    sync_vault(&mut a, &mut va);

    let wrong = VaultKey::Passphrase("not the right passphrase");
    assert!(vault::root_for(&record.lock, &wrong).is_err());

    let mut b = Client::joined(&s, &acct, "b");
    let mut vb = unlock(&s, &b, &record, VaultKey::RecoveryCode(&code));
    sync_vault(&mut b, &mut vb);
    assert!(b.lib.notebook(nb).is_ok());

    // a recovered user sets a new passphrase: the root is re-wrapped and the old one stops working
    let root = RootSecret::parse_recovery_code(&code).unwrap();
    let lock = vault::new_lock(&root, "a brand new passphrase").unwrap();
    transport(&s, &acct)
        .vault_set_lock(&record.vault_id, &UpdateVaultLockRequest { lock })
        .unwrap();
    let fresh = transport(&s, &acct)
        .vault_list()
        .unwrap()
        .vaults
        .remove(0)
        .lock;
    assert!(vault::root_for(&fresh, &VaultKey::Passphrase(PASSPHRASE)).is_err());
    assert!(vault::root_for(&fresh, &VaultKey::Passphrase("a brand new passphrase")).is_ok());
}

#[test]
fn a_swapped_lock_is_caught_by_the_vault_key_log() {
    let s = server();
    let acct = account(&s, "acct-f");
    let a = Client::joined(&s, &acct, "a");
    let (_, one, _) = create_vault(&s, &a);
    let c = Client::joined(&s, &account(&s, "acct-f2"), "c");
    // the same passphrase on another vault: the server hands over that vault's lock instead
    let other = create_vault(&s, &c).1;
    let b = Client::joined(&s, &acct, "b");
    let root = vault::root_for(&other.lock, &VaultKey::Passphrase(PASSPHRASE)).unwrap();
    let r = SyncEngine::open_vault(
        vault_setup(&b, &one.vault_id),
        transport(&s, &one.vault_id),
        vault_secrets(&b),
        &root,
    );
    assert!(r.is_err());
}

#[test]
fn a_vault_can_never_hold_a_stored_key() {
    let s = server();
    let acct = account(&s, "acct-g");
    let a = Client::joined(&s, &acct, "a");
    let (_, record, code) = create_vault(&s, &a);
    let doc = AccountKeyDocument {
        key: "secret".into(),
    };
    assert!(matches!(
        s.account_key_put(&record.vault_id, &doc),
        Err(RequestError::Api(b)) if b.code == ErrorCode::Forbidden
    ));
    assert!(matches!(
        s.account_key_get(&record.vault_id),
        Err(RequestError::Api(b)) if b.code == ErrorCode::Forbidden
    ));
    // and the account path refuses a vault scope outright
    let r = SyncEngine::enroll_with_recovery(
        vault_setup(&a, &record.vault_id),
        transport(&s, &record.vault_id),
        vault_secrets(&a),
        &code,
    );
    assert!(matches!(r, Err(SyncError::Invalid(_))));
}

#[test]
fn a_notebook_whose_vault_is_not_open_here_stays_local() {
    let s = server();
    let acct = account(&s, "acct-h");
    let mut a = Client::joined(&s, &acct, "a");
    let nb = vault_notebook(
        &mut a,
        &AccountId::new("v-not-open-here").unwrap(),
        "Left behind",
    );
    a.sync().unwrap();
    assert!(a.engine.store.oplog.indexes_for(&nb).is_empty());
    assert_eq!(s.op_count(&acct), 0);
}

#[test]
fn the_journal_is_released_only_after_every_engine_copied_it() {
    let s = server();
    let acct = account(&s, "acct-i");
    let mut a = Client::joined(&s, &acct, "a");
    let (mut va, record, _) = create_vault(&s, &a);
    a.sync().unwrap();
    sync_vault(&mut a, &mut va);
    let nb = vault_notebook(&mut a, &record.vault_id, "Secret");
    // the main engine runs first and passes this operation by; the vault engine must still see it
    a.sync().unwrap();
    assert!(va.store.oplog.indexes_for(&nb).is_empty());
    let a2 = a.restart();
    a = a2;
    sync_vault(&mut a, &mut va);
    assert!(!va.store.oplog.indexes_for(&nb).is_empty());
}

/// Every byte under `dir`, for checking that a word is gone from disk.
fn contains_bytes(dir: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if std::fs::read(&p)
                .map(|b| b.windows(needle.len()).any(|w| w == needle))
                .unwrap_or(false)
            {
                hits.push(p);
            }
        }
    }
    hits
}

#[test]
fn a_locked_vault_leaves_no_plaintext_and_picks_up_where_it_left_off() {
    use penroam_sync::locker;
    let s = server();
    let acct = account(&s, "acct-l");
    let mut a = Client::joined(&s, &acct, "a");
    let (mut va, record, code) = create_vault(&s, &a);
    let vid = record.vault_id.as_str().to_string();
    let root = RootSecret::parse_recovery_code(&code).unwrap();
    locker::store_local_key(vault_secrets(&a).as_ref(), &vid, &root).unwrap();
    let nb = vault_notebook(&mut a, &record.vault_id, "Zanzibar diary");
    let page = a.lib.notebook(nb).unwrap().page_order[0];
    add(&mut a.lib, nb, page, vec![text(70, "zanzibarite one")]);
    sync_vault(&mut a, &mut va);
    let (open, open_page) = a
        .lib
        .create_notebook("Open", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();

    let mut b = Client::joined(&s, &acct, "b");
    let mut vb = unlock(&s, &b, &record, VaultKey::Passphrase(PASSPHRASE));
    sync_vault(&mut b, &mut vb);

    // an edit that only reached the journal: locking must carry it along
    add(&mut a.lib, nb, page, vec![text(71, "zanzibarite two")]);
    let vault_device = va.device_id().to_string();
    locker::lock_vault(&mut a.lib, va, vec![], None)
        .map_err(|(e, _)| e)
        .unwrap();
    assert!(locker::is_locked(&a.root, &vid));
    assert!(a.lib.notebook(nb).is_err());
    let names = locker::vault_secret_names(
        &vid,
        &Scope::vault(&vid)
            .unwrap()
            .secret_name(penroam_sync::engine::DEVICE_SECRET),
    )
    .unwrap();
    for n in &names {
        assert_eq!(
            vault_secrets(&a).get(n).unwrap(),
            None,
            "{n} left in the secret store"
        );
    }
    for word in [&b"zanzibarite"[..], b"Zanzibar diary"] {
        assert_eq!(
            contains_bytes(&a.root, word),
            Vec::<std::path::PathBuf>::new()
        );
    }

    // life goes on: ordinary notes sync and the journal is released past the lock
    for i in 0..3 {
        add(
            &mut a.lib,
            open.id,
            open_page.id,
            vec![text(80 + i, "plain")],
        );
        a.sync().unwrap();
    }
    // and the vault changes elsewhere
    add(&mut b.lib, nb, page, vec![text(72, "zanzibarite three")]);
    sync_vault(&mut b, &mut vb);
    a = a.restart();
    assert!(a.lib.notebook(nb).is_err(), "still locked after a restart");

    let store = vault_secrets(&a);
    let wrong = locker::key_from_root(&RootSecret::generate().unwrap());
    assert!(locker::unlock_vault(&mut a.lib, &vid, &wrong, store.as_ref(), |_| None).is_err());
    assert!(locker::is_locked(&a.root, &vid));

    let key = locker::key_from_root(&root);
    let back = locker::unlock_vault(&mut a.lib, &vid, &key, store.as_ref(), |_| None).unwrap();
    assert_eq!(back, vec![nb]);
    let mut va = SyncEngine::open_scope(
        &a.root,
        Scope::vault(&vid).unwrap(),
        InProcessTransport::new(
            s.clone(),
            record.vault_id.clone(),
            DeviceId::new(vault_device).unwrap(),
        ),
        vault_secrets(&a),
    )
    .unwrap();
    va.resume_after_lock(a.lib.last_seq()).unwrap();
    sync_vault(&mut a, &mut va);
    sync_vault(&mut b, &mut vb);
    let texts = |lib: &mut Library| {
        let p = lib.load_page(nb, page).unwrap();
        let mut v: Vec<String> = p
            .elements
            .iter()
            .filter_map(|e| match e {
                penroam_core::Element::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        v.sort();
        v
    };
    let want = vec!["zanzibarite one", "zanzibarite three", "zanzibarite two"];
    assert_eq!(texts(&mut a.lib), want);
    assert_eq!(texts(&mut b.lib), want);
    // the main account never saw any of it
    assert!(a.engine.store.oplog.indexes_for(&nb).is_empty());
}

#[test]
fn start_up_finishes_a_lock_that_stopped_before_deleting() {
    use penroam_sync::locker;
    let s = server();
    let acct = account(&s, "acct-m");
    let mut a = Client::joined(&s, &acct, "a");
    let (mut va, record, code) = create_vault(&s, &a);
    let vid = record.vault_id.as_str().to_string();
    let nb = vault_notebook(&mut a, &record.vault_id, "Zanzibar diary");
    sync_vault(&mut a, &mut va);
    let device_secret = va.device_secret_name();
    drop(va);
    a.lib.flush_all().unwrap();
    // the pack is committed, then the app stops before deleting anything
    let key = locker::key_from_root(&RootSecret::parse_recovery_code(&code).unwrap());
    let contents = locker::Contents {
        folders: locker::vault_folders(&a.root, &vid).unwrap(),
        secrets: vec![],
    };
    locker::lock(&a.root, &vid, &key, &contents).unwrap();
    let Client {
        lib, root, _dir, ..
    } = a;
    lib.crash_for_tests();
    let store = FileSecretStore::new(root.join("sync-secrets"));
    assert!(store.get(&device_secret).unwrap().is_some());
    locker::recover(&root, &store, |_| vec![]).unwrap();
    assert!(store.get(&device_secret).unwrap().is_none());
    assert!(!root.join("notebooks").join(nb.to_string()).exists());
    assert!(!Scope::vault(&vid).unwrap().dir(&root).exists());
    assert_eq!(
        contains_bytes(&root.join("notebooks"), b"Zanzibar diary"),
        Vec::<std::path::PathBuf>::new()
    );
    let lib = Library::open(&root).unwrap();
    assert!(lib.notebook(nb).is_err());
    drop(_dir);
}

/// What "restore cloud sync" does on a device: finds the vault re-created by another device (its
/// name opens with the same root) or creates it again with the same root and lock, moves the
/// local assignment over to the new id, and starts a fresh engine that seeds the notebooks.
fn restore(
    s: &Arc<InMemoryServer>,
    c: &mut Client,
    old: &str,
    root: &RootSecret,
    lock: &penroam_sync_protocol::VaultLock,
) -> VaultEngine {
    let existing = transport(s, &c.account)
        .vault_list()
        .unwrap()
        .vaults
        .into_iter()
        .find(|r| vault::open_name(root, &r.name).is_ok());
    let record = match existing {
        Some(r) => r,
        None => transport(s, &c.account)
            .vault_create(&CreateVaultRequest {
                lock: lock.clone(),
                name: vault::seal_name(root, "Journal"),
            })
            .unwrap(),
    };
    let mut owners = NotebookOwners::load(&c.root).unwrap();
    for v in owners.notebooks.values_mut() {
        if v == old {
            *v = record.vault_id.as_str().to_string();
        }
    }
    owners.save(&c.root).unwrap();
    SyncEngine::open_vault(
        vault_setup(c, &record.vault_id),
        transport(s, &record.vault_id),
        vault_secrets(c),
        root,
    )
    .unwrap()
}

#[test]
fn a_vault_that_stopped_syncing_can_be_restored_from_every_device_without_duplicates() {
    let s = server();
    let acct = account(&s, "acct-r");
    let mut a = Client::joined(&s, &acct, "a");
    let (mut va, record, code) = create_vault(&s, &a);
    let root = RootSecret::parse_recovery_code(&code).unwrap();
    let old = record.vault_id.as_str().to_string();
    let nb = vault_notebook(&mut a, &record.vault_id, "Secret");
    let page = a.lib.notebook(nb).unwrap().page_order[0];
    add(&mut a.lib, nb, page, vec![text(90, "one"), text(91, "two")]);
    sync_vault(&mut a, &mut va);
    let mut b = Client::joined(&s, &acct, "b");
    let mut vb = unlock(&s, &b, &record, VaultKey::Passphrase(PASSPHRASE));
    sync_vault(&mut b, &mut vb);

    // stop syncing: the cloud copy goes, both devices keep the notebook
    transport(&s, &acct).vault_delete(&record.vault_id).unwrap();
    va.disconnect().unwrap();
    vb.disconnect().unwrap();
    // offline edits on both sides meanwhile
    add(&mut a.lib, nb, page, vec![text(92, "from a")]);
    add(&mut b.lib, nb, page, vec![text(93, "from b")]);

    let mut va = restore(&s, &mut a, &old, &root, &record.lock);
    sync_vault(&mut a, &mut va);
    let mut vb = restore(&s, &mut b, &old, &root, &record.lock);
    sync_vault(&mut b, &mut vb);
    sync_vault(&mut a, &mut va);
    assert_eq!(
        transport(&s, &acct).vault_list().unwrap().vaults.len(),
        1,
        "one vault, not two"
    );
    let texts = |lib: &mut Library| {
        let mut v: Vec<String> = lib
            .load_page(nb, page)
            .unwrap()
            .elements
            .iter()
            .filter_map(|e| match e {
                penroam_core::Element::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        v.sort();
        v
    };
    let want = vec!["from a", "from b", "one", "two"];
    assert_eq!(texts(&mut a.lib), want);
    assert_eq!(texts(&mut b.lib), want);
    assert_eq!(a.lib.notebook(nb).unwrap().page_order.len(), 1);
    assert_eq!(b.lib.notebook(nb).unwrap().page_order.len(), 1);
    // and it keeps syncing
    add(&mut b.lib, nb, page, vec![text(94, "later")]);
    sync_vault(&mut b, &mut vb);
    sync_vault(&mut a, &mut va);
    assert!(texts(&mut a.lib).contains(&"later".to_string()));
    assert!(a.engine.store.oplog.indexes_for(&nb).is_empty());
}

#[test]
fn a_vault_that_isnt_synced_can_still_be_locked_and_unlocked() {
    use penroam_sync::locker;
    let s = server();
    let acct = account(&s, "acct-n");
    let mut a = Client::joined(&s, &acct, "a");
    let (mut va, record, code) = create_vault(&s, &a);
    let vid = record.vault_id.as_str().to_string();
    let root = RootSecret::parse_recovery_code(&code).unwrap();
    let store = vault_secrets(&a);
    locker::store_local_key(store.as_ref(), &vid, &root).unwrap();
    let nb = vault_notebook(&mut a, &record.vault_id, "Zanzibar diary");
    sync_vault(&mut a, &mut va);
    transport(&s, &acct).vault_delete(&record.vault_id).unwrap();
    va.disconnect().unwrap();

    locker::lock_local(&mut a.lib, &vid, store.as_ref(), vec![], None).unwrap();
    assert!(a.lib.notebook(nb).is_err());
    assert_eq!(
        contains_bytes(&a.root, b"Zanzibar diary"),
        Vec::<std::path::PathBuf>::new()
    );
    assert!(locker::local_key(store.as_ref(), &vid).unwrap().is_none());
    let back = locker::unlock_vault(
        &mut a.lib,
        &vid,
        &locker::key_from_root(&root),
        store.as_ref(),
        |_| None,
    )
    .unwrap();
    assert_eq!(back, vec![nb]);
    assert!(
        !Scope::vault(&vid).unwrap().dir(&a.root).exists(),
        "no sync folder comes back"
    );
}
