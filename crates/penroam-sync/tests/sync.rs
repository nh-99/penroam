//! Convergence and fault-injection scenarios with independent clients.

mod common;

use common::*;
use penroam_core::replica::ConflictTarget;
use penroam_core::*;
use penroam_sync::Phase;
use penroam_sync_protocol::ErrorCode;
use penroam_sync_testkit::InMemoryServer;
use std::sync::Arc;
use uuid::Uuid;

fn server() -> Arc<InMemoryServer> {
    Arc::new(InMemoryServer::new())
}

#[test]
fn signing_in_brings_existing_notebooks_to_the_new_device() {
    let s = server();
    let acct = account(&s, "acct-1");
    let (mut a, _code) = Client::first(&s, &acct, "laptop");
    let (nb, p) = a
        .lib
        .create_notebook("Physics", PageDefaults::default(), None)
        .unwrap();
    add(
        &mut a.lib,
        nb.id,
        p.id,
        (0..5).map(|i| stroke(i, 1.0, "#111111")).collect(),
    );
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "tablet");
    b.sync().unwrap();
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert_eq!(ink_count(&mut b.lib, nb.id), 5);
    assert!(b.status().up_to_date, "{:?}", b.status());
    assert_eq!(b.engine.devices().len(), 2);
    // the server never saw the title or the strokes
    assert!(!format!("{:?}", s.list_devices(&acct).unwrap()).contains("Physics"));
}

#[test]
fn a_first_sync_is_announced_while_it_runs() {
    let s = server();
    let acct = account(&s, "acct-first-sync");
    let (mut a, _code) = Client::first(&s, &acct, "laptop");
    let (nb, p) = a
        .lib
        .create_notebook("Physics", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(0, 1.0, "#111111")]);
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "tablet");
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hook = seen.clone();
    b.engine.set_phase_hook(Box::new(move |st| {
        hook.lock().unwrap().push((st.phase, st.last_success_ms))
    }));
    b.sync().unwrap();
    // the app shows "first sync" from this: syncing, and never synced before
    assert_eq!(seen.lock().unwrap().first(), Some(&(Phase::Syncing, None)));
    assert_eq!(b.status().phase, Phase::Idle);
    assert!(b.status().last_success_ms.is_some());
}

#[test]
fn offline_strokes_on_both_devices_converge_to_the_union() {
    let s = server();
    let acct = account(&s, "acct-2");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Shared", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    a.offline(true);
    b.offline(true);
    for i in 0..50 {
        add(&mut a.lib, nb.id, p.id, vec![stroke(i, 100.0, "#1d4ed8")]);
        add(&mut b.lib, nb.id, p.id, vec![stroke(i, 300.0, "#b91c1c")]);
    }
    // offline: saving is unaffected; sync reports offline and keeps everything queued
    assert!(a.sync().is_err());
    assert_eq!(a.status().phase, Phase::Offline);
    assert_eq!(a.status().queued, 50);
    assert!(!a.status().up_to_date);
    a.offline(false);
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib, nb.id), 100);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert!(a.status().up_to_date && b.status().up_to_date);
    assert_eq!(a.status().conflicts, 0);
}

#[test]
fn concurrent_text_edits_keep_both_versions_until_resolved() {
    let s = server();
    let acct = account(&s, "acct-3");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Notes", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![text(42, "original")]);
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    edit(
        &mut a.lib,
        nb.id,
        p.id,
        OpKind::ReplaceElements {
            elements: vec![text(42, "edited on A")],
        },
    );
    edit(
        &mut b.lib,
        nb.id,
        p.id,
        OpKind::ReplaceElements {
            elements: vec![text(42, "edited on B")],
        },
    );
    settle(&mut [&mut a, &mut b]);
    assert_eq!(
        view(&mut a.lib),
        view(&mut b.lib),
        "same visible winner everywhere"
    );
    let conflicts = a.engine.conflicts(nb.id).unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    let c = &conflicts[0];
    let all: Vec<String> =
        std::iter::once(a.lib.load_page(nb.id, p.id).unwrap().elements[0].clone())
            .map(|e| serde_json::to_string(&e).unwrap())
            .chain(c.alternates.iter().map(|x| x.value.to_string()))
            .collect();
    assert!(
        all.iter().any(|v| v.contains("edited on A"))
            && all.iter().any(|v| v.contains("edited on B"))
    );
    // resolve on B by picking the alternate; everyone converges without conflict
    let (target, pick) = {
        let c = &b.engine.conflicts(nb.id).unwrap()[0];
        (c.target.clone(), c.alternates[0].op_id)
    };
    assert!(matches!(target, ConflictTarget::Element { .. }));
    b.engine
        .resolve_conflict(&mut b.lib, nb.id, &target, pick)
        .unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert!(a.engine.conflicts(nb.id).unwrap().is_empty());
}

#[test]
fn page_deleted_on_one_device_while_edited_on_the_other_keeps_the_edits() {
    let s = server();
    let acct = account(&s, "acct-4");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Draft", PageDefaults::default(), None)
        .unwrap();
    a.lib.create_page(nb.id, None, None, None).unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    a.lib.delete_page(nb.id, p.id).unwrap();
    add(
        &mut b.lib,
        nb.id,
        p.id,
        (0..3).map(|i| stroke(i, 50.0, "#15803d")).collect(),
    );
    settle(&mut [&mut a, &mut b]);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    let n = a.lib.notebook(nb.id).unwrap().clone();
    assert!(
        n.trashed_pages.iter().any(|t| t.id == p.id),
        "tombstone, recoverable from the trash"
    );
    assert_eq!(
        a.lib.load_page(nb.id, p.id).unwrap().elements.len(),
        3,
        "B's strokes kept"
    );
}

#[test]
fn lost_push_response_is_retried_without_duplicates() {
    let s = server();
    let acct = account(&s, "acct-5");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Retry", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.engine.prepare(&mut a.lib).unwrap();
    let p0 = s.op_count(&acct);
    // the push is committed but its response never arrives
    s.drop_next_responses(1);
    let mut r = penroam_sync::ReconcileReport::default();
    assert!(a.engine.push_for_tests(&mut r).is_err());
    assert_eq!(s.op_count(&acct), p0 + 1, "the server committed it");
    assert_eq!(a.status().queued, 1, "not shown as uploaded");
    a.sync().unwrap();
    assert_eq!(s.op_count(&acct), p0 + 1, "the retry was a duplicate");
    let mut b = Client::joined(&s, &acct, "b");
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb.id), 1, "applied once");
    assert!(a.status().up_to_date);
}

#[test]
fn crash_after_pull_before_apply_and_lost_cursor_redelivery_are_safe() {
    let s = server();
    let acct = account(&s, "acct-6");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Crash", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    add(
        &mut a.lib,
        nb.id,
        p.id,
        (0..10).map(|i| stroke(i, 5.0, "#111111")).collect(),
    );
    a.sync().unwrap();
    // B pulls (exchange) and is killed before installing
    b.engine.prepare(&mut b.lib).unwrap();
    let mut r = penroam_sync::ReconcileReport::default();
    b.engine.exchange(&mut r).unwrap();
    assert_eq!(r.delivered, 1);
    let mut b = b.restart();
    b.sync().unwrap();
    assert_eq!(ink_count(&mut b.lib, nb.id), 10);
    // B "lost" its cursor (killed after apply, before saving it): redelivery
    b.engine.store.state.cursor = None;
    b.engine.store.save().unwrap();
    let mut b = b.restart();
    b.sync().unwrap();
    assert_eq!(
        ink_count(&mut b.lib, nb.id),
        10,
        "redelivered ops deduplicated"
    );
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    // a kill right after local journaling (before ingest) loses nothing either
    add(&mut b.lib, nb.id, p.id, vec![stroke(99, 9.0, "#b91c1c")]);
    let mut b = b.restart();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib, nb.id), 11);
}

#[test]
fn large_asset_upload_interrupted_then_retried() {
    let s = server();
    let acct = account(&s, "acct-7");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, _) = a
        .lib
        .create_notebook("Scans", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    let big: Vec<u8> = (0..25 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    let asset = a
        .lib
        .add_asset(nb.id, &big, "application/pdf", Some("scan.pdf".into()))
        .unwrap();
    let ops_before = s.op_count(&acct);
    // the blob upload fails (server error); nothing referencing it is pushed
    s.fail_next_requests(3, ErrorCode::ServiceUnavailable);
    assert!(a.sync().is_err());
    assert!(a.sync().is_err());
    assert!(a.sync().is_err());
    assert_eq!(
        s.op_count(&acct),
        ops_before,
        "no op references a missing blob"
    );
    a.sync().unwrap();
    b.sync().unwrap();
    let got = b.lib.read_asset(nb.id, &asset.sha256).unwrap();
    assert_eq!(got.len(), big.len());
    assert_eq!(got, big);
}

#[test]
fn removed_device_cannot_write_and_keeps_local_notes() {
    let s = server();
    let acct = account(&s, "acct-8");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Team", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    let b_id = b.engine.device_id().to_string();
    a.engine.remove_device(&b_id).unwrap();
    add(&mut b.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    let err = b.sync().unwrap_err();
    assert_eq!(b.status().phase, Phase::Revoked, "{err}");
    assert_eq!(
        ink_count(&mut b.lib, nb.id),
        1,
        "pending local note preserved"
    );
    // A continues in a new epoch that B cannot read
    add(&mut a.lib, nb.id, p.id, vec![stroke(2, 2.0, "#111111")]);
    a.sync().unwrap();
    assert!(a
        .engine
        .devices()
        .iter()
        .any(|d| d.device_id == b_id && !d.active));
}

#[test]
fn recovery_code_restores_access_on_a_new_device() {
    let s = server();
    let acct = account(&s, "acct-9");
    let (mut a, code) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Recover me", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.sync().unwrap();
    let mut c = Client::recovered(&s, &acct, &code);
    c.sync().unwrap();
    assert_eq!(view(&mut a.lib), view(&mut c.lib));
    // a wrong code is refused without touching anything
    let bad = code.replace(&code[5..10], "00000");
    let dir = tempfile::tempdir().unwrap();
    let t = penroam_sync::transport::InProcessTransport::new(
        s.clone(),
        acct.clone(),
        penroam_sync_protocol::DeviceId::new("x").unwrap(),
    );
    let r = penroam_sync::SyncEngine::enroll_with_recovery(
        penroam_sync::Setup {
            library_root: dir.path().to_path_buf(),
            scope: penroam_sync::vault::Scope::Account,
            device_id: None,
            server_url: "x".into(),
            account_id: acct.as_str().into(),
            label: "x".into(),
        },
        t,
        Box::new(penroam_sync::FileSecretStore::new(dir.path().join("s"))),
        &bad,
    );
    assert!(r.is_err());
}

#[test]
fn server_registered_device_without_a_key_log_entry_gets_nothing() {
    let s = server();
    let acct = account(&s, "acct-10");
    let (mut a, _) = Client::first(&s, &acct, "a");
    a.lib
        .create_notebook("Private", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let evil =
        penroam_sync::crypto::DeviceKeys::generate(uuid::Uuid::new_v4().to_string()).unwrap();
    s.register_device_keys(
        &acct,
        &penroam_sync_protocol::DeviceRegistrationRequest {
            device_id: Some(penroam_sync_protocol::DeviceId::new(evil.device_id.clone()).unwrap()),
            signing_public_key: penroam_sync_protocol::base64url::encode(&evil.sign_pk()),
            encryption_public_key: penroam_sync_protocol::base64url::encode(&evil.dh_pk()),
            label: None,
        },
    )
    .unwrap();
    a.sync().unwrap();
    let devices = a.engine.devices();
    assert_eq!(devices.len(), 1);
    assert!(devices.iter().all(|d| d.device_id != evil.device_id));
}

#[test]
fn tampered_envelopes_are_quarantined_and_never_applied() {
    use penroam_sync_protocol::framing::{encode_json_batch, FrameItem};
    let s = server();
    let acct = account(&s, "acct-11");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Signed", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    let before = view(&mut b.lib);
    // A's real keys sign an envelope whose ciphertext was not produced with the content key
    let keys = penroam_sync::crypto::DeviceKeys::from_secret_string(
        &std::fs::read_to_string(a.root.join("sync-secrets/sync-device-keys.secret")).unwrap(),
    )
    .unwrap();
    let a_dev = penroam_sync_protocol::DeviceId::new(keys.device_id.clone()).unwrap();
    let next = a.engine.store.state.next_seq + 1;
    let ct = vec![7u8; 100];
    let mut h = penroam_sync_protocol::EnvelopeHeader {
        envelope_version: 1,
        op_id: penroam_sync_protocol::OpId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        device_id: a_dev.clone(),
        device_seq: next,
        notebook_id: penroam_sync_protocol::NotebookRoutingId::new("nb-fake").unwrap(),
        depends_on: vec![],
        key_epoch: 1,
        op_schema_version: 1,
        ciphertext_sha256: penroam_sync::crypto::sha256_hex(&ct),
        ciphertext_length: ct.len() as u64,
        signature: String::new(),
    };
    let sig = penroam_sync::crypto::sign(
        &keys.signing,
        b"",
        &penroam_sync_protocol::canonical_signing_bytes(&h),
    );
    h.signature = penroam_sync_protocol::base64url::encode(&sig);
    // and a second one with a forged signature
    let mut h2 = h.clone();
    h2.op_id = penroam_sync_protocol::OpId::new(uuid::Uuid::new_v4().to_string()).unwrap();
    h2.device_seq = next + 1;
    h2.signature = penroam_sync_protocol::base64url::encode(&[1u8; 64]);
    let _ = FrameItem {
        header_json: vec![],
        ciphertext: vec![],
    };
    let frame = encode_json_batch(
        &[(h.clone(), ct.clone()), (h2.clone(), ct.clone())],
        &s.limits(),
    )
    .unwrap();
    s.push(&acct, &a_dev, &frame).unwrap();
    b.sync().unwrap();
    assert_eq!(b.status().quarantined, 2);
    assert_eq!(b.status().phase, Phase::IntegrityAlert);
    assert_eq!(view(&mut b.lib), before, "nothing applied");
    let _ = (nb, p);
}

#[test]
fn newer_operation_schema_pauses_without_losing_anything() {
    use penroam_sync_protocol::framing::encode_json_batch;
    let s = server();
    let acct = account(&s, "acct-12");
    let (mut a, _) = Client::first(&s, &acct, "a");
    a.lib
        .create_notebook("Future", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    let keys = penroam_sync::crypto::DeviceKeys::from_secret_string(
        &std::fs::read_to_string(a.root.join("sync-secrets/sync-device-keys.secret")).unwrap(),
    )
    .unwrap();
    let a_dev = penroam_sync_protocol::DeviceId::new(keys.device_id.clone()).unwrap();
    let ct = vec![9u8; 64];
    let mut h = penroam_sync_protocol::EnvelopeHeader {
        envelope_version: 1,
        op_id: penroam_sync_protocol::OpId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
        device_id: a_dev.clone(),
        device_seq: a.engine.store.state.next_seq + 1,
        notebook_id: penroam_sync_protocol::NotebookRoutingId::new("nb-future").unwrap(),
        depends_on: vec![],
        key_epoch: 1,
        op_schema_version: penroam_core::operation::OP_SCHEMA_VERSION + 1,
        ciphertext_sha256: penroam_sync::crypto::sha256_hex(&ct),
        ciphertext_length: ct.len() as u64,
        signature: String::new(),
    };
    let sig = penroam_sync::crypto::sign(
        &keys.signing,
        b"",
        &penroam_sync_protocol::canonical_signing_bytes(&h),
    );
    h.signature = penroam_sync_protocol::base64url::encode(&sig);
    s.push(
        &acct,
        &a_dev,
        &encode_json_batch(&[(h, ct)], &s.limits()).unwrap(),
    )
    .unwrap();
    b.sync().unwrap();
    let st = b.status();
    assert_eq!(st.phase, Phase::NeedsUpgrade);
    assert_eq!(st.pending_remote, 1, "ciphertext kept for a future version");
    assert!(!st.up_to_date);
}

#[test]
fn long_offline_client_after_compaction_keeps_unsent_work() {
    let s = server();
    let acct = account(&s, "acct-13");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Old", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    b.engine.store.state.cursor = None; // as if B never pulled
    b.engine.store.save().unwrap();
    for i in 0..5 {
        add(&mut a.lib, nb.id, p.id, vec![stroke(i, 1.0, "#111111")]);
    }
    a.sync().unwrap();
    s.expire_cursors_before(&acct, s.op_count(&acct));
    add(&mut b.lib, nb.id, p.id, vec![stroke(77, 7.0, "#b91c1c")]);
    assert!(b.sync().is_err());
    assert_eq!(b.status().phase, Phase::RebaseRequired);
    assert_eq!(ink_count(&mut b.lib, nb.id), 1, "local work kept");
    assert_eq!(
        b.status().queued,
        0,
        "and already uploaded before the pull failed"
    );
    a.sync().unwrap();
    assert!(ink_count(&mut a.lib, nb.id) >= 6, "others still receive it");
}

#[test]
fn rename_conflict_and_notebook_created_on_both_sides() {
    let s = server();
    let acct = account(&s, "acct-14");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, _) = a
        .lib
        .create_notebook("Name", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    a.lib.rename_notebook(nb.id, "Name A").unwrap();
    b.lib.rename_notebook(nb.id, "Name B").unwrap();
    b.lib
        .create_notebook("Only on B", PageDefaults::default(), None)
        .unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert_eq!(a.lib.notebook_ids().len(), 2);
    let c = a.engine.conflicts(nb.id).unwrap();
    assert!(c.iter().any(|c| c.target == ConflictTarget::NotebookTitle));
    // a later rename on either side supersedes both
    a.lib.rename_notebook(nb.id, "Final").unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(b.lib.notebook(nb.id).unwrap().title, "Final");
    assert!(b.engine.conflicts(nb.id).unwrap().is_empty());
}

#[test]
fn notebook_imported_from_an_archive_after_setup_is_uploaded() {
    let s = server();
    let acct = account(&s, "acct-15");
    let (mut a, _) = Client::first(&s, &acct, "a");
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    // a notebook made elsewhere, exported, then imported into A (no journal ops)
    let other = tempfile::tempdir().unwrap();
    let mut lib = penroam_storage::Library::open(other.path()).unwrap();
    let (nb, p) = lib
        .create_notebook("From an archive", PageDefaults::default(), None)
        .unwrap();
    add(
        &mut lib,
        nb.id,
        p.id,
        (0..4).map(|i| stroke(i, 3.0, "#111111")).collect(),
    );
    let mut bytes = std::io::Cursor::new(Vec::new());
    lib.export_archive(nb.id, &mut bytes, "test").unwrap();
    bytes.set_position(0);
    a.lib.import_archive(bytes).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(b.lib.notebook_ids().len(), 1);
    let id = b.lib.notebook_ids()[0];
    assert_eq!(b.lib.notebook(id).unwrap().title, "From an archive");
    assert_eq!(ink_count(&mut b.lib, id), 4);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
}

#[test]
fn retry_after_an_epoch_change_is_still_a_duplicate() {
    let s = server();
    let acct = account(&s, "acct-16");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Epochs", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    let c = Client::joined(&s, &acct, "c");
    b.sync().unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.engine.prepare(&mut a.lib).unwrap();
    s.drop_next_responses(1);
    let mut r = penroam_sync::ReconcileReport::default();
    assert!(a.engine.push_for_tests(&mut r).is_err(), "response lost");
    // a removal starts epoch 2 before the retry
    a.engine.remove_device(c.engine.device_id()).unwrap();
    a.sync().unwrap();
    assert_ne!(a.status().phase, Phase::IntegrityAlert, "{:?}", a.status());
    assert_eq!(a.status().queued, 0);
    b.sync().unwrap();
    assert_eq!(ink_count(&mut b.lib, nb.id), 1);
    // new operations use the new epoch and still reach the remaining device
    add(&mut a.lib, nb.id, p.id, vec![stroke(2, 2.0, "#111111")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb.id), 2);
}

#[test]
fn asset_download_that_failed_is_retried_after_a_restart() {
    let s = server();
    let acct = account(&s, "acct-17");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, _) = a
        .lib
        .create_notebook("Scans", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    let asset = a
        .lib
        .add_asset(nb.id, b"%PDF-1.4 tiny", "application/pdf", None)
        .unwrap();
    a.sync().unwrap();
    b.engine.transport_mut().fail_blob_gets = 10;
    b.sync().unwrap();
    assert!(!b.lib.has_asset(nb.id, &asset.sha256), "download failed");
    let mut b = b.restart();
    b.sync().unwrap();
    assert_eq!(
        b.lib.read_asset(nb.id, &asset.sha256).unwrap(),
        b"%PDF-1.4 tiny"
    );
}

#[test]
fn same_archive_imported_on_two_devices_converges_without_conflicts() {
    let s = server();
    let acct = account(&s, "acct-18");
    let (mut a, _) = Client::first(&s, &acct, "a");
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut lib = penroam_storage::Library::open(other.path()).unwrap();
    let (nb, p) = lib
        .create_notebook("Shared archive", PageDefaults::default(), None)
        .unwrap();
    add(
        &mut lib,
        nb.id,
        p.id,
        (0..3).map(|i| stroke(i, 4.0, "#111111")).collect(),
    );
    let mut bytes = std::io::Cursor::new(Vec::new());
    lib.export_archive(nb.id, &mut bytes, "test").unwrap();
    for c in [&mut a, &mut b] {
        let mut copy = std::io::Cursor::new(bytes.get_ref().clone());
        copy.set_position(0);
        c.lib.import_archive(copy).unwrap();
    }
    settle(&mut [&mut a, &mut b]);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert_eq!(
        a.lib.notebook_ids().len(),
        1,
        "same id on both devices: one notebook"
    );
    assert_eq!(ink_count(&mut a.lib, nb.id), 3);
    assert!(a.engine.conflicts(nb.id).unwrap().is_empty());
    assert!(b.engine.conflicts(nb.id).unwrap().is_empty());
}

#[test]
fn removed_device_can_rejoin_with_its_old_copy() {
    let s = server();
    let acct = account(&s, "acct-19");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Rejoin", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    a.engine.remove_device(b.engine.device_id()).unwrap();
    // B keeps writing locally, then disconnects and connects again as a new device
    add(&mut b.lib, nb.id, p.id, vec![stroke(2, 2.0, "#b91c1c")]);
    let Client {
        lib,
        engine,
        root,
        server,
        account,
        _dir,
    } = b;
    engine.disconnect().unwrap();
    drop(lib);
    let mut b = common::Client::join_existing(&server, &account, "b-again", root, _dir);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert_eq!(
        ink_count(&mut a.lib, nb.id),
        2,
        "the offline stroke made it after re-joining"
    );
    assert!(a.engine.conflicts(nb.id).unwrap().is_empty());
    let bc = b.engine.conflicts(nb.id).unwrap();
    assert!(bc.is_empty(), "{bc:#?}");
}

#[test]
fn writes_from_a_removed_device_are_refused_even_if_the_server_delivers_them() {
    use penroam_sync_protocol::framing::encode_json_batch;
    let s = server();
    let acct = account(&s, "acct-20");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Team", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    let mut c = Client::joined(&s, &acct, "c");
    b.sync().unwrap();
    c.sync().unwrap();
    // C goes offline; B writes before its removal (must still reach C later)
    c.offline(true);
    add(&mut b.lib, nb.id, p.id, vec![stroke(1, 1.0, "#15803d")]);
    b.sync().unwrap();
    a.engine.remove_device(b.engine.device_id()).unwrap();
    // after removal B writes again; a misbehaving server stores and relays it
    add(&mut b.lib, nb.id, p.id, vec![stroke(2, 2.0, "#b91c1c")]);
    b.engine.prepare(&mut b.lib).unwrap();
    let sealed = b.engine.seal_pending_for_tests().unwrap();
    assert_eq!(sealed.len(), 1);
    let b_dev = penroam_sync_protocol::DeviceId::new(b.engine.device_id().to_string()).unwrap();
    // an honest server refuses it ...
    assert!(s
        .push(
            &acct,
            &b_dev,
            &encode_json_batch(&sealed, &s.limits()).unwrap()
        )
        .is_err());
    // ... a malicious one does not
    let r = s
        .push_ignoring_revocation(
            &acct,
            &b_dev,
            &encode_json_batch(&sealed, &s.limits()).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        r.acks[0].result,
        penroam_sync_protocol::AckResult::Accepted { .. }
    ));
    c.offline(false);
    c.sync().unwrap();
    a.sync().unwrap();
    for d in [&mut a, &mut c] {
        assert_eq!(
            ink_count(&mut d.lib, nb.id),
            1,
            "pre-removal stroke applied, post-removal refused"
        );
        assert_eq!(d.status().quarantined, 1);
        assert_eq!(d.status().phase, Phase::IntegrityAlert);
    }
    assert_eq!(view(&mut a.lib), view(&mut c.lib));
    assert_eq!(
        ink_count(&mut b.lib, nb.id),
        2,
        "B keeps its own note locally"
    );
}

#[test]
fn long_offline_device_recovers_from_a_checkpoint_after_compaction() {
    let s = server();
    let acct = account(&s, "acct-21");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Old", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    // B is away for a long time and keeps writing
    b.offline(true);
    add(&mut b.lib, nb.id, p.id, vec![stroke(77, 7.0, "#b91c1c")]);
    for i in 0..5 {
        add(&mut a.lib, nb.id, p.id, vec![stroke(i, 1.0, "#111111")]);
    }
    a.lib.rename_notebook(nb.id, "Old, renamed").unwrap();
    a.sync().unwrap();
    let rec = a.engine.create_checkpoint().unwrap().expect("published");
    assert!(rec.covers_server_seq > 0);
    assert!(
        s.compact_to_latest_checkpoint(&acct) > 0,
        "history before the checkpoint is gone"
    );
    b.offline(false);
    b.sync().unwrap();
    assert_ne!(b.status().phase, Phase::RebaseRequired);
    assert_eq!(b.lib.notebook(nb.id).unwrap().title, "Old, renamed");
    a.sync().unwrap();
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert_eq!(
        ink_count(&mut a.lib, nb.id),
        6,
        "B's unsent stroke survived and arrived"
    );
    assert!(b.status().up_to_date, "{:?}", b.status());
    // a later change still flows normally after the rebase
    add(&mut a.lib, nb.id, p.id, vec![stroke(9, 9.0, "#111111")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb.id), 7);
}

#[test]
fn checkpoint_from_a_removed_device_is_not_trusted() {
    let s = server();
    let acct = account(&s, "acct-22");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Team", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    let mut c = Client::joined(&s, &acct, "c");
    b.sync().unwrap();
    c.sync().unwrap();
    c.offline(true);
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.sync().unwrap();
    b.sync().unwrap();
    b.engine
        .create_checkpoint()
        .unwrap()
        .expect("B publishes a checkpoint");
    a.engine.remove_device(b.engine.device_id()).unwrap();
    s.compact_to_latest_checkpoint(&acct);
    c.offline(false);
    assert!(c.sync().is_err());
    assert_eq!(c.status().phase, Phase::IntegrityAlert, "{:?}", c.status());
    assert_eq!(
        ink_count(&mut c.lib, nb.id),
        0,
        "nothing imported from the untrusted checkpoint"
    );
}

/// One notebook on `a`, already on a second device `b`.
fn two_devices_with_a_notebook(
    s: &Arc<InMemoryServer>,
    name: &str,
) -> (Client, Client, Uuid, Uuid) {
    let acct = account(s, name);
    let (mut a, _) = Client::first(s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Field notes", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.sync().unwrap();
    let mut b = Client::joined(s, &acct, "b");
    b.sync().unwrap();
    (a, b, nb.id, p.id)
}

#[test]
fn reconnecting_brings_back_notebooks_permanently_deleted_here() {
    let s = server();
    let (mut a, mut b, nb, p) = two_devices_with_a_notebook(&s, "acct-purge-rejoin");
    a.engine.remove_device(b.engine.device_id()).unwrap();
    // removed, so this never reaches the account: only the local copy goes
    b.lib.delete_notebook(nb).unwrap();
    b.lib.purge_notebook(nb).unwrap();
    assert!(b.lib.is_purged(nb));
    let Client {
        lib,
        engine,
        root,
        server,
        account,
        _dir,
    } = b;
    engine.disconnect().unwrap();
    drop(lib);
    let mut b = common::Client::join_existing(&server, &account, "b-again", root, _dir);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(
        ink_count(&mut b.lib, nb),
        1,
        "the account's copy is installed"
    );
    assert!(b.engine.erased_notebooks(&b.lib).unwrap().is_empty());

    // the journal still holds the purge: a restart must not erase it again, or
    // later edits would be dropped with no error
    let mut b = b.restart();
    assert_eq!(ink_count(&mut b.lib, nb), 1, "still here after a restart");
    add(&mut a.lib, nb, p, vec![stroke(2, 2.0, "#b91c1c")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb), 2, "and it keeps receiving edits");
}

#[test]
fn a_permanent_delete_reaches_every_device_and_can_be_restored() {
    let s = server();
    let (mut a, mut b, nb, p) = two_devices_with_a_notebook(&s, "acct-purge-everywhere");
    let written = a.engine.store.state.next_seq;
    b.lib.delete_notebook(nb).unwrap();
    b.lib.purge_notebook(nb).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert!(a.lib.notebook(nb).is_err(), "erased on the other device");
    assert_eq!(
        a.engine.store.state.next_seq, written,
        "and not sent back out"
    );
    assert_eq!(a.status().phase, Phase::Idle);
    assert_eq!(b.status().phase, Phase::Idle);

    // it stays gone, and stays recoverable, past a restart
    let mut a = a.restart();
    settle(&mut [&mut a, &mut b]);
    assert!(a.lib.notebook(nb).is_err());
    for c in [&mut a, &mut b] {
        let offered = c.engine.erased_notebooks(&c.lib).unwrap();
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].title, "Field notes");
        assert!(offered[0].in_trash);
    }

    // restoring from a device that received the delete, not the one that made it
    a.engine.restore_erased(&mut a.lib, nb).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib, nb), 1);
    assert_eq!(ink_count(&mut b.lib, nb), 1, "back on every device");
    assert!(a.lib.notebook(nb).unwrap().deleted_at.is_none());
    assert!(a.engine.erased_notebooks(&a.lib).unwrap().is_empty());
    assert!(b.engine.erased_notebooks(&b.lib).unwrap().is_empty());

    let mut b = b.restart();
    add(&mut a.lib, nb, p, vec![stroke(3, 3.0, "#1d4ed8")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb), 2);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
}

#[test]
fn a_permanent_delete_made_offline_is_queued_and_keeps_edits_from_elsewhere() {
    let s = server();
    let (mut a, mut b, nb, p) = two_devices_with_a_notebook(&s, "acct-purge-offline");
    b.offline(true);
    b.lib.delete_notebook(nb).unwrap();
    b.lib.purge_notebook(nb).unwrap();
    let _ = b.sync();
    add(&mut a.lib, nb, p, vec![stroke(2, 2.0, "#b91c1c")]);
    a.sync().unwrap();
    assert_eq!(
        ink_count(&mut a.lib, nb),
        2,
        "the delete hasn't arrived yet"
    );

    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert!(a.lib.notebook(nb).is_err(), "the queued delete went out");

    a.engine.restore_erased(&mut a.lib, nb).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib, nb), 2, "the edit survived the delete");
    assert_eq!(ink_count(&mut b.lib, nb), 2);
}

#[test]
fn restoring_right_after_a_delete_is_not_undone_by_the_queued_delete() {
    let s = server();
    let (mut a, mut b, nb, _) = two_devices_with_a_notebook(&s, "acct-purge-undo");
    b.lib.delete_notebook(nb).unwrap();
    b.lib.purge_notebook(nb).unwrap();
    // nothing synced in between: the purge is still only in b's journal
    b.engine.restore_erased(&mut b.lib, nb).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib, nb), 1);
    assert_eq!(ink_count(&mut b.lib, nb), 1);
}

#[test]
fn deleting_a_notebook_before_its_asset_uploaded_does_not_stop_sync() {
    let s = server();
    let (mut a, mut b, nb, p) = two_devices_with_a_notebook(&s, "acct-purge-asset");
    b.offline(true);
    b.lib
        .add_asset(nb, b"%PDF-1.4 tiny", "application/pdf", None)
        .unwrap();
    let _ = b.sync();
    b.lib.delete_notebook(nb).unwrap();
    b.lib.purge_notebook(nb).unwrap();
    b.offline(false);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(b.status().phase, Phase::Idle);
    assert!(a.lib.notebook(nb).is_err());

    a.engine.restore_erased(&mut a.lib, nb).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(
        ink_count(&mut a.lib, nb),
        1,
        "the notes come back regardless"
    );
    add(&mut a.lib, nb, p, vec![stroke(2, 2.0, "#b91c1c")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb), 2);
}

#[test]
fn only_an_erased_notebook_can_be_restored() {
    let s = server();
    let (_a, mut b, nb, _) = two_devices_with_a_notebook(&s, "acct-purge-guard");
    assert!(
        b.engine.restore_erased(&mut b.lib, nb).is_err(),
        "not erased"
    );
    let unknown = Uuid::new_v4();
    assert!(b.engine.restore_erased(&mut b.lib, unknown).is_err());
}

#[test]
fn a_rename_reaches_every_device_and_keeps_keys_and_membership() {
    let s = server();
    let (mut a, mut b, nb, p) = two_devices_with_a_notebook(&s, "acct-rename");
    a.sync().unwrap(); // b joined by itself: a learns of it from the key log
    let id = b.engine.device_id().to_string();
    let before = a
        .engine
        .devices()
        .into_iter()
        .find(|d| d.device_id == id)
        .unwrap();

    a.engine.rename_device(&id, "  panther  ").unwrap();
    settle(&mut [&mut a, &mut b]);
    for c in [&a, &b] {
        let d = c
            .engine
            .devices()
            .into_iter()
            .find(|d| d.device_id == id)
            .unwrap();
        assert_eq!(d.label, "panther");
        assert_eq!(d.fingerprint, before.fingerprint, "keys are untouched");
        assert!(d.active);
    }
    add(&mut b.lib, nb, p, vec![stroke(2, 2.0, "#b91c1c")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(
        ink_count(&mut a.lib, nb),
        2,
        "the renamed device still syncs"
    );

    let mut b = b.restart();
    b.sync().unwrap();
    let d = b
        .engine
        .devices()
        .into_iter()
        .find(|d| d.device_id == id)
        .unwrap();
    assert_eq!(d.label, "panther", "replayed from the key log");
}

#[test]
fn a_rename_needs_a_usable_label_and_an_active_device() {
    let s = server();
    let (mut a, b, _, _) = two_devices_with_a_notebook(&s, "acct-rename-bad");
    let id = b.engine.device_id().to_string();
    assert!(a.engine.rename_device(&id, "   ").is_err());
    assert!(a.engine.rename_device(&id, &"x".repeat(129)).is_err());
    a.engine.remove_device(&id).unwrap();
    assert!(a.engine.rename_device(&id, "gone").is_err());
    // none of those may leave a bad entry behind
    a.sync().unwrap();
    assert_eq!(a.status().phase, Phase::Idle);
}

#[test]
fn a_notebook_is_offered_back_after_its_purge_leaves_the_journal() {
    let s = server();
    let (mut a, mut b, nb, p) = two_devices_with_a_notebook(&s, "acct-purge-checkpoint");
    b.lib.delete_notebook(nb).unwrap();
    b.lib.purge_notebook(nb).unwrap();
    // an older version kept these to itself: the account never heard of them
    let seq = b.lib.last_seq();
    b.engine.store.state.ingested_lib_seq = seq;
    b.engine.store.save().unwrap();
    b.lib.mark_uploaded(seq).unwrap();
    settle(&mut [&mut a, &mut b]);

    // every install checkpoints, and the journal lets go of what two have
    // covered; the purge goes with it, so nothing here records it any more
    for title in ["one", "two", "three", "four"] {
        a.lib
            .create_notebook(title, PageDefaults::default(), None)
            .unwrap();
        settle(&mut [&mut a, &mut b]);
    }
    // an older version wrote assets before it dropped the install
    let leftover = b.root.join("notebooks").join(nb.to_string()).join("assets");
    std::fs::create_dir_all(leftover).unwrap();
    let mut b = b.restart();
    assert!(
        !b.lib.is_purged(nb),
        "the journal no longer holds the purge"
    );
    assert!(b.lib.notebook(nb).is_err());

    let offered = b.engine.erased_notebooks(&b.lib).unwrap();
    assert_eq!(offered.len(), 1, "still in the account, so still offered");
    assert_eq!(offered[0].notebook_id, nb);
    assert_eq!(offered[0].page_count, 1);
    assert!(!offered[0].in_trash);

    b.engine.restore_erased(&mut b.lib, nb).unwrap();
    b.sync().unwrap();
    assert_eq!(ink_count(&mut b.lib, nb), 1);
    assert!(b.engine.erased_notebooks(&b.lib).unwrap().is_empty());

    let mut b = b.restart();
    add(&mut a.lib, nb, p, vec![stroke(2, 2.0, "#b91c1c")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb), 2, "and it keeps receiving edits");
}

#[test]
fn rolling_back_a_page_reaches_every_device_and_can_be_undone() {
    let s = server();
    let (mut a, mut b, nb, p) = two_devices_with_a_notebook(&s, "acct-rollback");
    for i in 2..6 {
        add(
            &mut a.lib,
            nb,
            p,
            vec![stroke(i, i as f64 * 10.0, "#111111")],
        );
    }
    let keep = a.lib.load_page(nb, p).unwrap().revision;
    settle(&mut [&mut a, &mut b]);
    // a change from the other device, seen here as an install from sync
    add(&mut b.lib, nb, p, vec![stroke(90, 400.0, "#b91c1c")]);
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut a.lib, nb), 6);
    let newest = a.lib.page_history(nb, p).unwrap().last().unwrap().revision;

    a.lib.restore_page_version(nb, p, keep).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb), 5, "the rollback synced");
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert_eq!(a.status().conflicts, 0);

    // restoring what was removed brings it back everywhere
    a.lib.restore_page_version(nb, p, newest).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(ink_count(&mut b.lib, nb), 6);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));

    // history keeps working on the other device too, after all of that
    let on_b = b.lib.page_history(nb, p).unwrap();
    let first = on_b[0].revision;
    b.lib.restore_page_version(nb, p, first).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
}

#[test]
fn encrypted_cloud_plus_recovery_code_rebuilds_everything_after_losing_all_devices() {
    let s = server();
    let acct = account(&s, "acct-disaster");
    let (mut a, code) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Everything", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    let q = a.lib.create_page(nb.id, None, None, None).unwrap().1;
    add(&mut a.lib, nb.id, q.id, vec![text(7, "typed")]);
    a.sync().unwrap();
    let want = view(&mut a.lib);
    drop(a);

    let mut c = Client::recovered(&s, &acct, &code);
    c.sync().unwrap();
    assert_eq!(view(&mut c.lib), want);

    // the rebuilt library backs up and restores like any other
    let mut out = std::io::Cursor::new(Vec::new());
    c.lib.export_backup(&mut out, "test").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut fresh = penroam_storage::Library::open(dir.path().join("lib")).unwrap();
    let r = fresh
        .restore_backup(
            std::io::Cursor::new(out.into_inner()),
            &penroam_storage::RestoreOptions::default(),
        )
        .unwrap();
    assert!(r.failed.is_empty());
    assert_eq!(view(&mut fresh), want);
}

#[test]
fn a_replacement_device_can_see_and_restore_versions_it_never_recorded() {
    let s = server();
    let acct = account(&s, "acct-history");
    let (mut a, code) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Long lived", PageDefaults::default(), None)
        .unwrap();
    // a notebook's first sync uploads what it holds as one snapshot; edits after
    // that are separate versions
    a.sync().unwrap();
    for i in 1..=4 {
        // a version is an edit with its own timestamp
        std::thread::sleep(std::time::Duration::from_millis(2));
        add(
            &mut a.lib,
            nb.id,
            p.id,
            vec![stroke(i, i as f64 * 10.0, "#111111")],
        );
    }
    a.sync().unwrap();
    drop(a);

    let mut c = Client::recovered(&s, &acct, &code);
    c.sync().unwrap();
    assert!(
        c.lib.page_history(nb.id, p.id).unwrap().is_empty(),
        "this device has recorded nothing of its own"
    );
    let versions = c.engine.page_versions(nb.id, p.id).unwrap();
    assert_eq!(versions.len(), 5, "first sync, then four strokes");
    assert_eq!(versions.iter().map(|v| v.added).sum::<u32>(), 4);

    // version with two of the four strokes
    let target = c
        .engine
        .page_at(nb.id, p.id, versions[2].position)
        .unwrap()
        .unwrap();
    assert_eq!(target.elements.len(), 2);
    c.lib.restore_page_content(nb.id, p.id, &target).unwrap();
    c.sync().unwrap();
    assert_eq!(ink_count(&mut c.lib, nb.id), 2);

    // the rollback is in the log too, so going forward again works the same way
    let after = c.engine.page_versions(nb.id, p.id).unwrap();
    assert_eq!(after.len(), 6);
    let newest = c
        .engine
        .page_at(nb.id, p.id, versions[4].position)
        .unwrap()
        .unwrap();
    c.lib.restore_page_content(nb.id, p.id, &newest).unwrap();
    assert_eq!(ink_count(&mut c.lib, nb.id), 4);
}

#[test]
fn a_whole_notebook_can_be_rebuilt_from_the_log_at_a_past_time() {
    let s = server();
    let acct = account(&s, "acct-nb-history");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Before", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.sync().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let then = penroam_core::now_ms();
    std::thread::sleep(std::time::Duration::from_millis(5));
    a.lib.rename_notebook(nb.id, "After").unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(2, 2.0, "#111111")]);
    let extra = a.lib.create_page(nb.id, None, None, None).unwrap().1;
    a.sync().unwrap();

    let (old, pages) = a.engine.notebook_at(nb.id, then).unwrap().unwrap();
    assert_eq!(old.title, "Before");
    assert_eq!(old.page_order, vec![p.id]);
    assert_eq!(pages[0].elements.len(), 1);
    assert!(a.engine.notebook_at(nb.id, 1).unwrap().is_none());

    let targets = pages.into_iter().map(|p| (p.id, p)).collect();
    let r = a
        .lib
        .restore_notebook_content(nb.id, &old.title, &old.page_order, &targets)
        .unwrap();
    assert_eq!((r.pages_reverted, r.pages_trashed), (1, 1));
    let now = a.lib.notebook(nb.id).unwrap();
    assert_eq!(now.title, "Before");
    assert!(now.trashed_pages.iter().any(|t| t.id == extra.id));
}

#[test]
fn keeping_both_versions_of_a_conflict_adds_a_copy_and_ends_the_conflict() {
    let s = server();
    let acct = account(&s, "acct-keep-both");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Notes", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![text(42, "original")]);
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    for (c, body) in [(&mut a, "edited on A"), (&mut b, "edited on B")] {
        edit(
            &mut c.lib,
            nb.id,
            p.id,
            OpKind::ReplaceElements {
                elements: vec![text(42, body)],
            },
        );
    }
    settle(&mut [&mut a, &mut b]);
    let (target, alt) = {
        let c = &b.engine.conflicts(nb.id).unwrap()[0];
        (c.target.clone(), c.alternates[0].op_id)
    };
    // a page or notebook conflict has nothing to copy
    assert!(b
        .engine
        .keep_both(&mut b.lib, nb.id, &ConflictTarget::NotebookTitle, alt)
        .is_err());
    let copy = b.engine.keep_both(&mut b.lib, nb.id, &target, alt).unwrap();
    settle(&mut [&mut a, &mut b]);
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert!(a.engine.conflicts(nb.id).unwrap().is_empty());
    assert!(b.engine.conflicts(nb.id).unwrap().is_empty());
    let texts: Vec<String> = a
        .lib
        .load_page(nb.id, p.id)
        .unwrap()
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.iter().any(|t| t == "edited on A"));
    assert!(texts.iter().any(|t| t == "edited on B"));
    assert!(a
        .lib
        .load_page(nb.id, p.id)
        .unwrap()
        .elements
        .iter()
        .any(|e| e.id() == copy));
}

/// A notebook on `a`, a second device `b` that goes away, and two checkpoints published by `a`
/// while it is gone, with history compacted the way servers keep it (up to the older one).
fn away_device_with_two_checkpoints(
    s: &Arc<InMemoryServer>,
    name: &str,
) -> (Client, Client, Uuid, Uuid) {
    let acct = account(s, name);
    let (mut a, _) = Client::first(s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Old", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(s, &acct, "b");
    b.sync().unwrap();
    b.offline(true);
    add(&mut b.lib, nb.id, p.id, vec![stroke(77, 7.0, "#b91c1c")]);
    for round in 0..2 {
        for i in 0..3 {
            add(
                &mut a.lib,
                nb.id,
                p.id,
                vec![stroke(round * 10 + i, 1.0, "#111111")],
            );
        }
        a.sync().unwrap();
        a.engine.create_checkpoint().unwrap().expect("published");
    }
    assert!(s.compact_to_retained_checkpoints(&acct) > 0);
    b.offline(false);
    (a, b, nb.id, p.id)
}

#[test]
fn a_damaged_latest_checkpoint_falls_back_to_the_previous_one() {
    let s = server();
    let (mut a, mut b, nb, _) = away_device_with_two_checkpoints(&s, "acct-prev-cp");
    let phases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = phases.clone();
    b.engine
        .set_phase_hook(Box::new(move |st| seen.lock().unwrap().push(st.phase)));
    b.engine.transport_mut().corrupt_checkpoints = true;
    b.sync().unwrap();
    assert!(
        phases.lock().unwrap().contains(&Phase::NeedsCatchUp),
        "the catch-up is shown while it runs"
    );
    assert_eq!(b.status().phase, Phase::Idle, "{:?}", b.status());
    a.sync().unwrap();
    assert_eq!(view(&mut a.lib), view(&mut b.lib));
    assert_eq!(
        ink_count(&mut a.lib, nb),
        7,
        "B's unsent stroke arrived too"
    );
    assert!(b
        .engine
        .events
        .iter()
        .any(|e| e.contains("trying the previous one")));
}

#[test]
fn an_older_checkpoint_than_one_already_applied_is_refused() {
    let s = server();
    let (_a, mut b, nb, _) = away_device_with_two_checkpoints(&s, "acct-rollback-cp");
    // as if B had recovered from a newer checkpoint before: the server now offers older ones
    b.engine.store.state.applied_checkpoint_seq = 10_000;
    b.engine.store.save().unwrap();
    assert!(b.sync().is_err());
    assert_eq!(b.status().phase, Phase::IntegrityAlert, "{:?}", b.status());
    assert_eq!(
        ink_count(&mut b.lib, nb),
        1,
        "nothing imported; own stroke kept"
    );
}

#[test]
fn a_log_that_moves_backwards_is_refused() {
    let s = server();
    let acct = account(&s, "acct-rollback-log");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Log", PageDefaults::default(), None)
        .unwrap();
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    assert!(b.engine.store.state.cursor_seq > 0);
    // B saw further than the server now claims
    b.engine.store.state.cursor_seq += 1_000;
    b.engine.store.save().unwrap();
    add(&mut a.lib, nb.id, p.id, vec![stroke(1, 1.0, "#111111")]);
    a.sync().unwrap();
    assert!(b.sync().is_err());
    assert_eq!(b.status().phase, Phase::IntegrityAlert);
    assert_eq!(ink_count(&mut b.lib, nb.id), 0);
}

#[test]
fn keeping_the_shown_version_of_a_conflict_ends_it_everywhere() {
    let s = server();
    let acct = account(&s, "acct-keep-shown");
    let (mut a, _) = Client::first(&s, &acct, "a");
    let (nb, p) = a
        .lib
        .create_notebook("Notes", PageDefaults::default(), None)
        .unwrap();
    add(&mut a.lib, nb.id, p.id, vec![text(42, "original")]);
    a.sync().unwrap();
    let mut b = Client::joined(&s, &acct, "b");
    b.sync().unwrap();
    for (c, body) in [(&mut a, "edited on A"), (&mut b, "edited on B")] {
        edit(
            &mut c.lib,
            nb.id,
            p.id,
            OpKind::ReplaceElements {
                elements: vec![text(42, body)],
            },
        );
    }
    settle(&mut [&mut a, &mut b]);
    let shown = view(&mut a.lib);
    let (target, winner) = {
        let c = &b.engine.conflicts(nb.id).unwrap()[0];
        (c.target.clone(), c.winner)
    };
    b.engine
        .resolve_conflict(&mut b.lib, nb.id, &target, winner)
        .unwrap();
    settle(&mut [&mut a, &mut b]);
    assert!(a.engine.conflicts(nb.id).unwrap().is_empty());
    assert!(b.engine.conflicts(nb.id).unwrap().is_empty());
    assert_eq!(view(&mut a.lib), shown, "the shown version stayed");
    assert_eq!(view(&mut b.lib), shown);
}
