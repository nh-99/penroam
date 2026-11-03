//! Device keys, key log, blobs, fault consistency and persistence of the
//! development test server.

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::envelope::sha256_hex;
use penroam_sync_protocol::framing::encode_json_batch;
use penroam_sync_protocol::{
    AccountId, AckResult, BlobId, DeviceId, DeviceRegistrationRequest, EnvelopeHeader, ErrorCode,
    KeyLogAppendRequest, Limits, PullOpsRequest,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{InMemoryServer, RequestError};

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn dev(s: &str) -> DeviceId {
    s.parse().unwrap()
}
fn blob(s: &str) -> BlobId {
    s.parse().unwrap()
}

fn reg(id: Option<&str>, sign: u8) -> DeviceRegistrationRequest {
    DeviceRegistrationRequest {
        device_id: id.map(dev),
        signing_public_key: base64url::encode(&[sign; 32]),
        encryption_public_key: base64url::encode(&[0x22; 32]),
        label: Some("test".into()),
    }
}

fn code<T: std::fmt::Debug>(r: Result<T, RequestError>) -> ErrorCode {
    match r {
        Err(RequestError::Api(b)) => b.code,
        other => panic!("expected an API error, got {other:?}"),
    }
}

fn op(n: u64, device: &str) -> (EnvelopeHeader, Vec<u8>) {
    let ct = fake_ciphertext(&format!("{device}-{n}"), 40 + n as usize);
    (
        header_for(&format!("op-{device}-{n}"), device, n, &[], &ct),
        ct,
    )
}

fn batch(items: &[(EnvelopeHeader, Vec<u8>)]) -> Vec<u8> {
    encode_json_batch(items, &Limits::DEFAULT).unwrap()
}

fn keylog_req(index: u64, byte: u8) -> KeyLogAppendRequest {
    KeyLogAppendRequest {
        index,
        data: base64url::encode(&[byte; 40]),
    }
}

#[test]
fn registration_is_idempotent_and_rejects_key_changes() {
    let s = InMemoryServer::new();
    let a = acct("acct-1");
    s.create_account(&a);
    let r = s.register_device_keys(&a, &reg(Some("dev-a"), 1)).unwrap();
    assert_eq!(r.device_id, dev("dev-a"));
    assert_eq!(
        s.register_device_keys(&a, &reg(Some("dev-a"), 1))
            .unwrap()
            .device_id,
        dev("dev-a")
    );
    assert_eq!(
        code(s.register_device_keys(&a, &reg(Some("dev-a"), 9))),
        ErrorCode::Conflict
    );
    let generated = s.register_device_keys(&a, &reg(None, 3)).unwrap().device_id;
    assert_ne!(generated, dev("dev-a"));
    let mut bad = reg(Some("dev-b"), 1);
    bad.signing_public_key = "not base64!".into();
    assert_eq!(
        code(s.register_device_keys(&a, &bad)),
        ErrorCode::ValidationFailed
    );

    let list = s.list_devices(&a).unwrap().devices;
    assert_eq!(list.len(), 2);
    let d = list.iter().find(|d| d.device_id == dev("dev-a")).unwrap();
    assert_eq!(d.signing_public_key, base64url::encode(&[1; 32]));
    assert!(!d.revoked);

    // a registered device can push
    let ops = [op(1, "dev-a")];
    let resp = s.push(&a, &dev("dev-a"), &batch(&ops)).unwrap();
    assert!(matches!(
        resp.acks[0].result,
        AckResult::Accepted { server_seq: 1 }
    ));
}

#[test]
fn revoke_device_as_needs_a_live_caller_and_blocks_writes() {
    let s = InMemoryServer::new();
    let a = acct("acct-1");
    s.create_account(&a);
    s.register_device_keys(&a, &reg(Some("dev-a"), 1)).unwrap();
    s.register_device_keys(&a, &reg(Some("dev-b"), 2)).unwrap();
    assert_eq!(
        code(s.revoke_device_as(&a, &dev("dev-x"), &dev("dev-b"))),
        ErrorCode::Forbidden
    );
    assert_eq!(
        code(s.revoke_device_as(&a, &dev("dev-a"), &dev("dev-zz"))),
        ErrorCode::NotFound
    );
    s.revoke_device_as(&a, &dev("dev-a"), &dev("dev-b"))
        .unwrap();
    assert_eq!(
        code(s.push(&a, &dev("dev-b"), &batch(&[op(1, "dev-b")]))),
        ErrorCode::DeviceRevoked
    );
    assert_eq!(
        code(s.revoke_device_as(&a, &dev("dev-b"), &dev("dev-a"))),
        ErrorCode::DeviceRevoked
    );
    assert_eq!(
        code(s.register_device_keys(&a, &reg(Some("dev-b"), 2))),
        ErrorCode::DeviceRevoked
    );
    assert!(s
        .list_devices(&a)
        .unwrap()
        .devices
        .iter()
        .any(|d| d.device_id == dev("dev-b") && d.revoked));
}

#[test]
fn keylog_appends_at_exact_index_only() {
    let s = InMemoryServer::new();
    let a = acct("acct-1");
    s.create_account(&a);
    assert_eq!(s.keylog_get(&a, 0).unwrap().next_index, 0);
    assert_eq!(s.keylog_append(&a, &keylog_req(0, 1)).unwrap().index, 0);
    assert_eq!(
        code(s.keylog_append(&a, &keylog_req(0, 2))),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(s.keylog_append(&a, &keylog_req(5, 2))),
        ErrorCode::Conflict
    );
    s.keylog_append(&a, &keylog_req(1, 2)).unwrap();
    let bad = KeyLogAppendRequest {
        index: 2,
        data: "!!".into(),
    };
    assert_eq!(code(s.keylog_append(&a, &bad)), ErrorCode::ValidationFailed);
    let page = s.keylog_get(&a, 0).unwrap();
    assert_eq!(page.next_index, 2);
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[1].index, 1);
    assert_eq!(page.entries[1].data, keylog_req(1, 2).data);
    let tail = s.keylog_get(&a, 1).unwrap();
    assert_eq!(tail.entries.len(), 1);
    assert!(s.keylog_get(&a, 99).unwrap().entries.is_empty());
    assert_eq!(
        code(s.keylog_get(&acct("nobody"), 0)),
        ErrorCode::Unauthenticated
    );
}

#[test]
fn blobs_are_immutable_and_verified() {
    let s = InMemoryServer::new();
    let a = acct("acct-1");
    let d = dev("dev-a");
    s.register_device(&a, &d);
    let bytes = vec![7u8; 1000];
    let sha = sha256_hex(&bytes);
    let r = s.blob_put(&a, &d, &blob("b1"), &sha, &bytes).unwrap();
    assert!(!r.duplicate);
    assert!(
        s.blob_put(&a, &d, &blob("b1"), &sha, &bytes)
            .unwrap()
            .duplicate
    );
    let other = vec![8u8; 1000];
    assert_eq!(
        code(s.blob_put(&a, &d, &blob("b1"), &sha256_hex(&other), &other)),
        ErrorCode::Conflict
    );
    assert_eq!(
        code(s.blob_put(&a, &d, &blob("b2"), &sha, &other)),
        ErrorCode::IntegrityMismatch
    );
    assert_eq!(
        code(s.blob_put(&a, &d, &blob("b2"), "abc", &other)),
        ErrorCode::ValidationFailed
    );
    assert_eq!(s.blob_get(&a, &d, &blob("b1")).unwrap(), bytes);
    assert_eq!(code(s.blob_get(&a, &d, &blob("nope"))), ErrorCode::NotFound);
    let u = s.usage(&a).unwrap();
    assert_eq!((u.blob_count, u.blob_bytes), (1, 1000));

    s.set_quota(&a, Some(1500));
    let big = vec![9u8; 600];
    assert_eq!(
        code(s.blob_put(&a, &d, &blob("b3"), &sha256_hex(&big), &big)),
        ErrorCode::QuotaExceeded
    );
    s.revoke_device(&a, &d);
    assert_eq!(
        code(s.blob_put(&a, &d, &blob("b4"), &sha, &bytes)),
        ErrorCode::DeviceRevoked
    );
}

#[test]
fn oversized_blob_is_rejected() {
    let mut limits = Limits::DEFAULT;
    limits.max_blob_bytes = 100;
    let s = InMemoryServer::with_secret([1; 32], "t", limits);
    let (a, d) = (acct("acct-1"), dev("dev-a"));
    s.register_device(&a, &d);
    let bytes = vec![1u8; 101];
    assert_eq!(
        code(s.blob_put(&a, &d, &blob("b"), &sha256_hex(&bytes), &bytes)),
        ErrorCode::PayloadTooLarge
    );
}

#[test]
fn faults_apply_to_every_request_kind() {
    let s = InMemoryServer::new();
    let (a, d) = (acct("acct-1"), dev("dev-a"));
    s.register_device(&a, &d);
    s.fail_next_requests(1, ErrorCode::ServiceUnavailable);
    match s.keylog_get(&a, 0) {
        Err(RequestError::Api(b)) => {
            assert_eq!(b.code, ErrorCode::ServiceUnavailable);
            assert!(b.retry_after_ms.is_some());
        }
        other => panic!("{other:?}"),
    }
    // a dropped response is still processed
    s.drop_next_responses(1);
    assert_eq!(
        s.keylog_append(&a, &keylog_req(0, 1)),
        Err(RequestError::ResponseLost)
    );
    assert_eq!(s.keylog_get(&a, 0).unwrap().next_index, 1);
    let bytes = vec![3u8; 10];
    s.drop_next_responses(1);
    assert_eq!(
        s.blob_put(&a, &d, &blob("b"), &sha256_hex(&bytes), &bytes),
        Err(RequestError::ResponseLost)
    );
    assert!(
        s.blob_put(&a, &d, &blob("b"), &sha256_hex(&bytes), &bytes)
            .unwrap()
            .duplicate
    );
    s.fail_next_requests(1, ErrorCode::InternalError);
    assert_eq!(code(s.list_devices(&a)), ErrorCode::InternalError);
}

#[test]
fn meta_advertises_keylog_and_blobs_except_for_fixtures() {
    let m = InMemoryServer::new().meta();
    assert!(m.has_capability("keylog.v1") && m.has_capability("blobs.v1"));
    let f = InMemoryServer::with_secret([0; 32], "f", Limits::DEFAULT).meta();
    assert!(!f.has_capability("keylog.v1"));
}

#[test]
fn persistent_state_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (a, d) = (acct("acct-1"), dev("dev-a"));
    let bytes = vec![5u8; 4096];
    let cursor = {
        let s = InMemoryServer::open_persistent(dir.path()).unwrap();
        s.create_account(&a);
        s.register_device_keys(&a, &reg(Some("dev-a"), 1)).unwrap();
        let ops = [op(1, "dev-a"), op(2, "dev-a"), op(3, "dev-a")];
        s.push(&a, &d, &batch(&ops)).unwrap();
        s.keylog_append(&a, &keylog_req(0, 4)).unwrap();
        s.blob_put(&a, &d, &blob("b1"), &sha256_hex(&bytes), &bytes)
            .unwrap();
        s.set_quota(&a, Some(1 << 30));
        let page = s
            .pull(
                &a,
                &d,
                &PullOpsRequest {
                    cursor: None,
                    limit: 1,
                },
            )
            .unwrap();
        s.ack(
            &a,
            &d,
            &penroam_sync_protocol::AckRequest {
                applied_cursor: page.response.next_cursor.clone(),
            },
        )
        .unwrap();
        page.response.next_cursor
        // dropped without any shutdown: like a killed process
    };
    let s = InMemoryServer::open_persistent(dir.path()).unwrap();
    let page = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: Some(cursor),
                limit: 10,
            },
        )
        .unwrap();
    let seqs: Vec<u64> = page.response.items.iter().map(|i| i.server_seq).collect();
    assert_eq!(seqs, vec![2, 3]);
    assert_eq!(s.applied_position(&a, &d), Some(1));
    // retry after restart is still deduplicated
    let resp = s.push(&a, &d, &batch(&[op(2, "dev-a")])).unwrap();
    assert!(matches!(
        resp.acks[0].result,
        AckResult::Duplicate { server_seq: 2 }
    ));
    assert_eq!(
        s.keylog_get(&a, 0).unwrap().entries[0].data,
        keylog_req(0, 4).data
    );
    assert_eq!(s.blob_get(&a, &d, &blob("b1")).unwrap(), bytes);
    let u = s.usage(&a).unwrap();
    assert_eq!(
        (u.op_count, u.blob_count, u.quota_bytes),
        (3, 1, Some(1 << 30))
    );
    assert_eq!(
        s.list_devices(&a).unwrap().devices[0].signing_public_key,
        base64url::encode(&[1; 32])
    );
    // faults are not persisted
    s.fail_next_requests(1, ErrorCode::InternalError);
    drop(s);
    let s = InMemoryServer::open_persistent(dir.path()).unwrap();
    s.usage(&a).unwrap();
}

#[test]
fn corrupt_persistent_blob_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let (a, d) = (acct("acct-1"), dev("dev-a"));
    {
        let s = InMemoryServer::open_persistent(dir.path()).unwrap();
        s.register_device(&a, &d);
        let bytes = vec![1u8; 64];
        s.blob_put(&a, &d, &blob("b1"), &sha256_hex(&bytes), &bytes)
            .unwrap();
    }
    let path = std::fs::read_dir(dir.path().join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(path, b"tampered").unwrap();
    assert!(InMemoryServer::open_persistent(dir.path()).is_err());
}
