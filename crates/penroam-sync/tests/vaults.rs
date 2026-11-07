//! Sign-in-only sync and passphrase-locked vaults, end to end against the public test server.

mod common;

use std::sync::Arc;

use common::{Client, *};
use penroam_core::PageDefaults;
use penroam_storage::Library;
use penroam_sync::crypto::RootSecret;
use penroam_sync::transport::InProcessTransport;
use penroam_sync::vault::{self, NotebookOwners, Scope, VaultKey};
use penroam_sync::{FileSecretStore, Setup, SyncEngine, SyncError, SyncTransport};
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
