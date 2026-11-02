//! Behaviour tests for the in-memory fake server.

use std::sync::Arc;

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::framing::{
    decode_json_batch, encode_batch, encode_json_batch, FrameItem,
};
use penroam_sync_protocol::{
    AccountId, AckRequest, AckResult, Cursor, DeviceId, EnvelopeHeader, ErrorCode, Limits,
    PullOpsRequest, PulledOp, PushOpsResponse,
};
use penroam_sync_testkit::fixtures::{fake_ciphertext, header_for};
use penroam_sync_testkit::{InMemoryServer, RequestError};

fn acct(s: &str) -> AccountId {
    s.parse().unwrap()
}
fn dev(s: &str) -> DeviceId {
    s.parse().unwrap()
}

fn setup() -> (InMemoryServer, AccountId, DeviceId) {
    let s = InMemoryServer::new();
    let (a, d) = (acct("acct-1"), dev("dev-a"));
    s.register_device(&a, &d);
    (s, a, d)
}

fn op(n: u64, device: &str) -> (EnvelopeHeader, Vec<u8>) {
    let ct = fake_ciphertext(&format!("{device}-{n}"), 50 + n as usize);
    (
        header_for(&format!("op-{device}-{n}"), device, n, &[], &ct),
        ct,
    )
}

fn batch(items: &[(EnvelopeHeader, Vec<u8>)]) -> Vec<u8> {
    encode_json_batch(items, &Limits::DEFAULT).unwrap()
}

fn push(
    s: &InMemoryServer,
    a: &AccountId,
    d: &DeviceId,
    items: &[(EnvelopeHeader, Vec<u8>)],
) -> PushOpsResponse {
    s.push(a, d, &batch(items)).unwrap()
}

fn results(r: &PushOpsResponse) -> Vec<AckResult> {
    r.acks.iter().map(|a| a.result.clone()).collect()
}

fn rejected_code(r: &AckResult) -> ErrorCode {
    match r {
        AckResult::Rejected { error } => error.code,
        other => panic!("expected rejection, got {other:?}"),
    }
}

fn pull_all(s: &InMemoryServer, a: &AccountId, d: &DeviceId, limit: u32) -> Vec<u64> {
    let mut cursor = None;
    let mut seqs = vec![];
    loop {
        let page = s.pull(a, d, &PullOpsRequest { cursor, limit }).unwrap();
        seqs.extend(page.response.items.iter().map(|i| i.server_seq));
        cursor = Some(page.response.next_cursor);
        if !page.response.has_more {
            return seqs;
        }
    }
}

#[test]
fn accepts_and_assigns_increasing_seqs() {
    let (s, a, d) = setup();
    let r = push(&s, &a, &d, &[op(1, "dev-a"), op(2, "dev-a")]);
    assert_eq!(
        results(&r),
        vec![
            AckResult::Accepted { server_seq: 1 },
            AckResult::Accepted { server_seq: 2 }
        ]
    );
    let r = push(&s, &a, &d, &[op(3, "dev-a")]);
    assert_eq!(results(&r), vec![AckResult::Accepted { server_seq: 3 }]);
}

#[test]
fn idempotent_duplicate_push() {
    let (s, a, d) = setup();
    let items = [op(1, "dev-a"), op(2, "dev-a")];
    push(&s, &a, &d, &items);
    let r = push(&s, &a, &d, &items);
    assert_eq!(
        results(&r),
        vec![
            AckResult::Duplicate { server_seq: 1 },
            AckResult::Duplicate { server_seq: 2 }
        ]
    );
    assert_eq!(s.op_count(&a), 2);
    assert_eq!(s.usage(&a).unwrap().op_count, 2);
}

#[test]
fn op_id_reuse_with_different_ciphertext_rejected() {
    let (s, a, d) = setup();
    let (h, ct) = op(1, "dev-a");
    push(&s, &a, &d, &[(h.clone(), ct)]);
    let other = fake_ciphertext("other", 10);
    let mut h2 = header_for(h.op_id.as_str(), "dev-a", 1, &[], &other);
    let r = push(&s, &a, &d, &[(h2.clone(), other.clone())]);
    assert_eq!(rejected_code(&r.acks[0].result), ErrorCode::OpIdConflict);
    // Same op_id, new device_seq: still a conflict.
    h2.device_seq = 9;
    let r = push(&s, &a, &d, &[(h2, other)]);
    assert_eq!(rejected_code(&r.acks[0].result), ErrorCode::OpIdConflict);
    // Different op_id reusing a device_seq: conflict.
    let ct3 = fake_ciphertext("x", 5);
    let h3 = header_for("op-new", "dev-a", 1, &[], &ct3);
    let r = push(&s, &a, &d, &[(h3, ct3)]);
    assert_eq!(rejected_code(&r.acks[0].result), ErrorCode::OpIdConflict);
    assert_eq!(s.op_count(&a), 1);
}

#[test]
fn hash_and_length_mismatch_rejected() {
    let (s, a, d) = setup();
    let (mut h, ct) = op(1, "dev-a");
    h.ciphertext_sha256 = "0".repeat(64);
    let r = push(&s, &a, &d, &[(h, ct.clone())]);
    assert_eq!(
        rejected_code(&r.acks[0].result),
        ErrorCode::IntegrityMismatch
    );
    let (mut h, _) = op(2, "dev-a");
    h.ciphertext_length += 1;
    let r = push(&s, &a, &d, &[(h, ct)]);
    assert_eq!(
        rejected_code(&r.acks[0].result),
        ErrorCode::IntegrityMismatch
    );
    assert_eq!(s.op_count(&a), 0);
}

#[test]
fn invalid_header_and_foreign_device_rejected_per_op() {
    let (s, a, d) = setup();
    let (mut bad, ct) = op(1, "dev-a");
    bad.depends_on.push(penroam_sync_protocol::OpRef {
        device_id: dev("dev-a"),
        device_seq: 1,
    });
    let foreign = op(1, "dev-z");
    let good = op(2, "dev-a");
    let r = push(&s, &a, &d, &[(bad, ct), foreign, good]);
    assert_eq!(rejected_code(&r.acks[0].result), ErrorCode::InvalidEnvelope);
    assert_eq!(rejected_code(&r.acks[1].result), ErrorCode::Forbidden);
    assert_eq!(r.acks[2].result, AckResult::Accepted { server_seq: 1 });
}

#[test]
fn malformed_batch_rejects_whole_request() {
    let (s, a, d) = setup();
    let mut body = batch(&[op(1, "dev-a")]);
    body.push(0);
    let e = s.push(&a, &d, &body).unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::MalformedBatch));
    let body = encode_batch(
        &[FrameItem {
            header_json: b"{\"nope\":1}".to_vec(),
            ciphertext: vec![1],
        }],
        &Limits::DEFAULT,
    )
    .unwrap();
    assert_eq!(
        s.push(&a, &d, &body).unwrap_err().code(),
        Some(ErrorCode::MalformedBatch)
    );
    assert_eq!(s.op_count(&a), 0);
}

#[test]
fn auth_and_revocation() {
    let (s, a, d) = setup();
    let body = batch(&[op(1, "dev-a")]);
    assert_eq!(
        s.push(&acct("nobody"), &d, &body).unwrap_err().code(),
        Some(ErrorCode::Unauthenticated)
    );
    assert_eq!(
        s.push(&a, &dev("dev-x"), &body).unwrap_err().code(),
        Some(ErrorCode::Forbidden)
    );
    s.revoke_device(&a, &d);
    assert_eq!(
        s.push(&a, &d, &body).unwrap_err().code(),
        Some(ErrorCode::DeviceRevoked)
    );
}

#[test]
fn quota_enforced() {
    let (s, a, d) = setup();
    s.set_quota(&a, Some(60));
    let r = push(&s, &a, &d, &[op(1, "dev-a"), op(2, "dev-a")]);
    assert!(matches!(r.acks[0].result, AckResult::Accepted { .. }));
    assert_eq!(rejected_code(&r.acks[1].result), ErrorCode::QuotaExceeded);
    let u = s.usage(&a).unwrap();
    assert_eq!((u.op_count, u.op_bytes, u.quota_bytes), (1, 51, Some(60)));
}

#[test]
fn pagination_and_frames() {
    let (s, a, d) = setup();
    let items: Vec<_> = (1..=7).map(|n| op(n, "dev-a")).collect();
    push(&s, &a, &d, &items);
    let p1 = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: None,
                limit: 3,
            },
        )
        .unwrap();
    assert_eq!(
        p1.response
            .items
            .iter()
            .map(|i| i.server_seq)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(p1.response.has_more);
    let framed: Vec<(PulledOp, Vec<u8>)> = decode_json_batch(&p1.frame, &Limits::DEFAULT).unwrap();
    assert_eq!(framed.len(), 3);
    for ((p, ct), (_, orig)) in framed.iter().zip(&items) {
        assert_eq!(ct, orig);
        penroam_sync_protocol::verify_ciphertext(&p.header, ct).unwrap();
    }
    assert_eq!(pull_all(&s, &a, &d, 3), (1..=7).collect::<Vec<_>>());
    // Limit is clamped, zero is rejected.
    assert_eq!(pull_all(&s, &a, &d, u32::MAX), (1..=7).collect::<Vec<_>>());
    assert_eq!(
        s.pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: None,
                limit: 0
            }
        )
        .unwrap_err()
        .code(),
        Some(ErrorCode::ValidationFailed)
    );
    // Empty page at the end keeps the position.
    let last = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: None,
                limit: 7,
            },
        )
        .unwrap();
    let empty = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: Some(last.response.next_cursor.clone()),
                limit: 7,
            },
        )
        .unwrap();
    assert!(empty.response.items.is_empty() && !empty.response.has_more);
    assert_eq!(empty.response.next_cursor, last.response.next_cursor);
}

#[test]
fn pagination_stable_while_appending() {
    let s = Arc::new(InMemoryServer::new());
    let a = acct("acct-1");
    for w in 0..4 {
        s.register_device(&a, &dev(&format!("dev-{w}")));
    }
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let s = Arc::clone(&s);
            let a = a.clone();
            std::thread::spawn(move || {
                let name = format!("dev-{w}");
                let d = dev(&name);
                for n in 1..=50 {
                    let r = s.push(&a, &d, &batch(&[op(n, &name)])).unwrap();
                    assert!(matches!(r.acks[0].result, AckResult::Accepted { .. }));
                }
            })
        })
        .collect();
    // Concurrently paginate until all 200 ops have been seen.
    let reader = dev("dev-0");
    let mut cursor: Option<Cursor> = None;
    let mut seen = Vec::new();
    while seen.len() < 200 {
        let page = s
            .pull(
                &a,
                &reader,
                &PullOpsRequest {
                    cursor: cursor.clone(),
                    limit: 7,
                },
            )
            .unwrap();
        seen.extend(page.response.items.iter().map(|i| i.server_seq));
        cursor = Some(page.response.next_cursor);
    }
    for w in writers {
        w.join().unwrap();
    }
    // No gaps, no duplicates, strictly increasing.
    assert_eq!(seen, (1..=200).collect::<Vec<_>>());
    // Per-device order is preserved in server_seq order.
    let all = s
        .pull(
            &a,
            &reader,
            &PullOpsRequest {
                cursor: None,
                limit: 1000,
            },
        )
        .unwrap();
    for w in 0..4 {
        let name = format!("dev-{w}");
        let seqs: Vec<u64> = all
            .response
            .items
            .iter()
            .filter(|i| i.header.device_id.as_str() == name)
            .map(|i| i.header.device_seq)
            .collect();
        assert_eq!(seqs, (1..=50).collect::<Vec<_>>());
    }
}

#[test]
fn tampered_and_foreign_cursors_rejected() {
    let (s, a, d) = setup();
    push(&s, &a, &d, &[op(1, "dev-a"), op(2, "dev-a")]);
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
    let c = page.response.next_cursor;
    let mut raw = base64url::decode(c.as_str()).unwrap();
    for i in 0..raw.len() {
        raw[i] ^= 0x01;
        let t = Cursor::new(base64url::encode(&raw)).unwrap();
        let e = s
            .pull(
                &a,
                &d,
                &PullOpsRequest {
                    cursor: Some(t),
                    limit: 1,
                },
            )
            .unwrap_err();
        assert_eq!(e.code(), Some(ErrorCode::InvalidCursor), "byte {i}");
        raw[i] ^= 0x01;
    }
    // A cursor from another server instance is rejected.
    let other = InMemoryServer::new();
    other.register_device(&a, &d);
    let e = other
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: Some(c.clone()),
                limit: 1,
            },
        )
        .unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::InvalidCursor));
    // Ack with a tampered cursor is rejected too.
    let bad = Cursor::new("AAAA").unwrap();
    assert_eq!(
        s.ack(
            &a,
            &d,
            &AckRequest {
                applied_cursor: bad
            }
        )
        .unwrap_err()
        .code(),
        Some(ErrorCode::InvalidCursor)
    );
}

#[test]
fn cross_account_isolation() {
    let s = InMemoryServer::new();
    let (a1, a2, d1, d2) = (acct("acct-1"), acct("acct-2"), dev("dev-a"), dev("dev-b"));
    s.register_device(&a1, &d1);
    s.register_device(&a2, &d2);
    // Same op id in two accounts: both accepted independently.
    let (h, ct) = op(1, "dev-a");
    push(&s, &a1, &d1, &[(h.clone(), ct.clone())]);
    let mut h2 = h.clone();
    h2.device_id = d2.clone();
    let r = push(&s, &a2, &d2, &[(h2, ct)]);
    assert_eq!(r.acks[0].result, AckResult::Accepted { server_seq: 1 });
    // Device of account 1 is not a device of account 2.
    assert_eq!(
        s.pull(
            &a2,
            &d1,
            &PullOpsRequest {
                cursor: None,
                limit: 10
            }
        )
        .unwrap_err()
        .code(),
        Some(ErrorCode::Forbidden)
    );
    // Account 1 cursor is not usable for account 2.
    let c1 = s
        .pull(
            &a1,
            &d1,
            &PullOpsRequest {
                cursor: None,
                limit: 10,
            },
        )
        .unwrap()
        .response
        .next_cursor;
    let e = s
        .pull(
            &a2,
            &d2,
            &PullOpsRequest {
                cursor: Some(c1),
                limit: 10,
            },
        )
        .unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::InvalidCursor));
    let p2 = s
        .pull(
            &a2,
            &d2,
            &PullOpsRequest {
                cursor: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(p2.response.items.len(), 1);
    assert_eq!(p2.response.items[0].header.device_id, d2);
}

#[test]
fn expired_cursor_and_checkpoint_required() {
    let (s, a, d) = setup();
    push(
        &s,
        &a,
        &d,
        &(1..=5).map(|n| op(n, "dev-a")).collect::<Vec<_>>(),
    );
    let c2 = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: None,
                limit: 2,
            },
        )
        .unwrap()
        .response
        .next_cursor;
    let c4 = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: Some(c2.clone()),
                limit: 2,
            },
        )
        .unwrap()
        .response
        .next_cursor;
    s.expire_cursors_before(&a, 3);
    let e = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: Some(c2),
                limit: 2,
            },
        )
        .unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::CursorExpired));
    assert_eq!(e.code().unwrap().http_status(), 410);
    assert!(s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: Some(c4),
                limit: 2
            }
        )
        .is_ok());
    let e = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: None,
                limit: 2,
            },
        )
        .unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::CheckpointRequired));
}

#[test]
fn acks_are_per_device_and_monotonic() {
    let (s, a, d) = setup();
    let d2 = dev("dev-b");
    s.register_device(&a, &d2);
    push(
        &s,
        &a,
        &d,
        &(1..=4).map(|n| op(n, "dev-a")).collect::<Vec<_>>(),
    );
    let c2 = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: None,
                limit: 2,
            },
        )
        .unwrap()
        .response
        .next_cursor;
    let c4 = s
        .pull(
            &a,
            &d,
            &PullOpsRequest {
                cursor: None,
                limit: 4,
            },
        )
        .unwrap()
        .response
        .next_cursor;
    assert_eq!(s.applied_position(&a, &d), None);
    s.ack(&a, &d, &AckRequest { applied_cursor: c4 }).unwrap();
    s.ack(
        &a,
        &d,
        &AckRequest {
            applied_cursor: c2.clone(),
        },
    )
    .unwrap();
    assert_eq!(s.applied_position(&a, &d), Some(4));
    s.ack(&a, &d2, &AckRequest { applied_cursor: c2 }).unwrap();
    assert_eq!(s.applied_position(&a, &d2), Some(2));
}

#[test]
fn fault_injection_fail_next() {
    let (s, a, d) = setup();
    s.fail_next_requests(2, ErrorCode::ServiceUnavailable);
    let body = batch(&[op(1, "dev-a")]);
    for _ in 0..2 {
        let RequestError::Api(e) = s.push(&a, &d, &body).unwrap_err() else {
            panic!("expected API error")
        };
        assert_eq!(e.code, ErrorCode::ServiceUnavailable);
        assert!(e.code.is_retryable());
        assert!(e.retry_after_ms.is_some());
    }
    assert_eq!(s.op_count(&a), 0, "failed requests are not processed");
    let r = s.push(&a, &d, &body).unwrap();
    assert_eq!(r.acks[0].result, AckResult::Accepted { server_seq: 1 });
}

#[test]
fn fault_injection_response_lost_then_retry_is_duplicate() {
    let (s, a, d) = setup();
    let body = batch(&[op(1, "dev-a"), op(2, "dev-a")]);
    s.drop_next_responses(1);
    assert_eq!(s.push(&a, &d, &body), Err(RequestError::ResponseLost));
    assert_eq!(s.op_count(&a), 2, "persisted despite lost response");
    let r = s.push(&a, &d, &body).unwrap();
    assert_eq!(
        results(&r),
        vec![
            AckResult::Duplicate { server_seq: 1 },
            AckResult::Duplicate { server_seq: 2 }
        ]
    );
}

#[test]
fn meta_is_compatible() {
    let s = InMemoryServer::new();
    let m = s.meta();
    m.limits.validate().unwrap();
    assert_eq!(
        penroam_sync_protocol::negotiate(penroam_sync_protocol::ProtocolRange::CLIENT, &m),
        Ok(penroam_sync_protocol::ProtocolVersion::new(1, 0))
    );
    assert!(m.has_capability("ops.batch.v1"));
}
