//! Comment threads on shared notebooks: the binary encoding (round trips,
//! malformed and hostile input, separation from document payloads), body and
//! mention rules, the merge rules of the comment replica, an order-independence
//! test over random delivery orders, anchor resolution, and the golden vectors
//! in `tests/vectors/comments.json`.

use penroam_core::comment::*;
use penroam_core::model::*;
use penroam_core::operation::{DocumentOperation, LocalOperation, OperationRef};
use penroam_core::textcrdt::b64;
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// helpers

const PAGE: Uuid = Uuid::from_u128(0x5000);
const PAGE2: Uuid = Uuid::from_u128(0x5001);
const TRASHED: Uuid = Uuid::from_u128(0x5002);
const GONE: Uuid = Uuid::from_u128(0x5003);
const PDF_PAGE: Uuid = Uuid::from_u128(0x5004);
const STROKE: Uuid = Uuid::from_u128(0x6000);
const TEXT: Uuid = Uuid::from_u128(0x6001);
const IMAGE_PDF: Uuid = Uuid::from_u128(0x6002);

fn dev(n: u128) -> Uuid {
    Uuid::from_u128(0xD000 + n)
}

/// A device writing comment operations with its own sequence numbers.
struct Writer {
    device: Uuid,
    author: &'static str,
    seq: u64,
    next_id: u128,
}

impl Writer {
    fn new(n: u128, author: &'static str) -> Self {
        Writer {
            device: dev(n),
            author,
            seq: 0,
            next_id: n << 64,
        }
    }
    fn id(&mut self) -> Uuid {
        self.next_id += 1;
        Uuid::from_u128(self.next_id)
    }
    /// An operation depending on everything `seen` has applied.
    fn op(&mut self, seen: &CommentReplica, f: impl FnOnce(Uuid) -> CommentOp) -> CommentOperation {
        self.seq += 1;
        let id = self.id();
        CommentOperation {
            id,
            device_id: self.device,
            sequence: self.seq,
            schema_version: COMMENT_SCHEMA_VERSION,
            causal_dependencies: seen.frontier(),
            author: self.author.into(),
            moderation: false,
            at: 1_700_000_000_000
                + (self.seq as i64) * 1000
                + (self.device.as_u128() as i64 & 0xff),
            op: f(id),
        }
    }
}

fn rc(corners: [f64; 4]) -> Rect {
    Rect::from_corners(corners)
}

fn point(x: f64, y: f64) -> CommentAnchor {
    CommentAnchor::PagePoint {
        page_id: PAGE,
        x,
        y,
    }
}

fn create(body: &str) -> impl FnOnce(Uuid) -> CommentOp + '_ {
    move |id| CommentOp::ThreadCreated {
        thread_id: id,
        anchor: point(10.0, 20.0),
        body: body.into(),
    }
}

fn reply(thread: Uuid, body: &str) -> impl FnOnce(Uuid) -> CommentOp + '_ {
    move |id| CommentOp::Replied {
        thread_id: thread,
        comment_id: id,
        body: body.into(),
        reply_to: None,
    }
}

fn apply_all(r: &mut CommentReplica, ops: &[&CommentOperation]) {
    for op in ops {
        r.apply(op).unwrap();
    }
}

fn every_kind_of_op() -> Vec<CommentOperation> {
    let r = CommentReplica::new();
    let mut a = Writer::new(1, "acct_alice");
    let t = Uuid::from_u128(0x1_0000_0000_0000_0000_0001);
    let anchors = [
        point(-1250.5, -0.0),
        CommentAnchor::PageRect {
            page_id: PAGE,
            rect: rc([-10.0, -20.0, 30.0, 40.0]),
        },
        CommentAnchor::Element {
            page_id: PAGE,
            element_id: STROKE,
            fallback_rect: Some(rc([1.0, 2.0, 3.0, 4.0])),
        },
        CommentAnchor::Element {
            page_id: PAGE,
            element_id: STROKE,
            fallback_rect: None,
        },
        CommentAnchor::TextRange {
            page_id: PAGE,
            element_id: TEXT,
            start: vec![1, 2, 3],
            end: vec![4, 5, 6, 7],
            quote: "日本 👩🏽‍🔬".into(),
            quote_hash: quote_hash("日本 👩🏽‍🔬"),
            fallback_rect: Some(rc([0.0, 0.0, 10.0, 10.0])),
        },
        CommentAnchor::PdfRegion {
            page_id: PDF_PAGE,
            pdf_page_index: 70_000,
            normalized_rect: rc([0.0, 0.25, 0.5, 1.0]),
            rotation: 270,
            pdf_asset_sha256: None,
        },
        CommentAnchor::PdfRegion {
            page_id: PDF_PAGE,
            pdf_page_index: 0,
            normalized_rect: rc([0.0, 0.0, 1.0, 1.0]),
            rotation: 0,
            pdf_asset_sha256: Some([0xab; 32]),
        },
    ];
    let mut ops: Vec<CommentOperation> = anchors
        .iter()
        .map(|an| {
            a.op(&r, |id| CommentOp::ThreadCreated {
                thread_id: id,
                anchor: an.clone(),
                body: "hi @[acct_bob]\n\tthere".into(),
            })
        })
        .collect();
    ops.push(a.op(&r, |id| CommentOp::Replied {
        thread_id: t,
        comment_id: id,
        body: "r".into(),
        reply_to: Some(t),
    }));
    ops.push(a.op(&r, reply(t, "no reply_to")));
    ops.push(a.op(&r, |_| CommentOp::CommentEdited {
        comment_id: t,
        body: "e".into(),
    }));
    let mut del = a.op(&r, |_| CommentOp::CommentDeleted { comment_id: t });
    del.moderation = true;
    del.causal_dependencies = vec![
        OperationRef {
            device_id: dev(0),
            device_seq: u64::MAX,
        },
        OperationRef {
            device_id: dev(1),
            device_seq: 3,
        },
    ];
    del.at = -5;
    ops.push(del);
    ops.push(a.op(&r, |_| CommentOp::ThreadResolved { thread_id: t }));
    ops.push(a.op(&r, |_| CommentOp::ThreadReopened { thread_id: t }));
    ops.push(a.op(&r, |_| CommentOp::AnchorReattached {
        thread_id: t,
        anchor: point(1.0, 2.0),
    }));
    ops
}

// ---------------------------------------------------------------------------
// encoding

#[test]
fn every_operation_round_trips_through_binary_and_json() {
    for op in every_kind_of_op() {
        let bytes = op.encode();
        assert_eq!(&bytes[..4], b"PRCM");
        assert!(is_comment_payload(&bytes));
        let back = CommentOperation::decode(&bytes).unwrap();
        assert_eq!(back, op);
        assert_eq!(back.encode(), bytes, "one operation, one encoding");
        let json = serde_json::to_string(&op).unwrap();
        let from_json: CommentOperation = serde_json::from_str(&json).unwrap();
        assert_eq!(from_json, op, "{json}");
    }
    // -0.0 survives bit for bit
    let op = &every_kind_of_op()[0];
    let back = CommentOperation::decode(&op.encode()).unwrap();
    let CommentOp::ThreadCreated {
        anchor: CommentAnchor::PagePoint { y, .. },
        ..
    } = back.op
    else {
        panic!()
    };
    assert!(y.is_sign_negative());
}

#[test]
fn every_truncation_and_bit_flip_is_refused() {
    for op in every_kind_of_op() {
        let bytes = op.encode();
        for n in 0..bytes.len() {
            assert!(CommentOperation::decode(&bytes[..n]).is_err(), "prefix {n}");
        }
        for i in 6..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x40;
            assert!(
                matches!(
                    CommentOperation::decode(&b),
                    Err(CommentError::Malformed(_))
                ),
                "flip at {i}"
            );
        }
        let mut b = bytes.clone();
        b.push(0);
        assert!(CommentOperation::decode(&b).is_err());
    }
}

/// Re-signs a body with a fresh CRC so tests reach the checks after it.
fn with_crc(mut body: Vec<u8>) -> Vec<u8> {
    let c = crc32fast::hash(&body);
    body.extend_from_slice(&c.to_le_bytes());
    body
}

fn without_crc(op: &CommentOperation) -> Vec<u8> {
    let mut b = op.encode();
    b.truncate(b.len() - 4);
    b
}

#[test]
fn hostile_encodings_are_refused_without_large_allocations() {
    let op = &every_kind_of_op()[0];
    let base = without_crc(op);

    // a newer schema is reported as such, not as garbage
    let mut b = base.clone();
    b[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        CommentOperation::decode(&with_crc(b)),
        Err(CommentError::NewerSchema(2))
    );
    let mut b = base.clone();
    b[4..6].copy_from_slice(&0u16.to_le_bytes());
    assert!(CommentOperation::decode(&with_crc(b)).is_err());

    // a length prefix of 2^63 with nothing behind it: refused before allocating
    let at = 6 + 32;
    let mut b = base[..at].to_vec();
    b.extend_from_slice(&[1, 0, 0]); // seq 1, at 0, flags 0
    b.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]);
    assert!(matches!(
        CommentOperation::decode(&with_crc(b)),
        Err(CommentError::Malformed(_))
    ));

    // a non-minimal varint (sequence 1 as 0x81 0x00) is not the canonical encoding
    let mut b = base[..at].to_vec();
    b.extend_from_slice(&[0x81, 0x00]);
    b.extend_from_slice(&base[at + 1..]);
    assert_eq!(
        CommentOperation::decode(&with_crc(b)),
        Err(CommentError::Malformed("not the canonical encoding".into()))
    );

    // unknown operation and anchor tags
    let mut b = base.clone();
    // from the end: thread id, page-point anchor (tag, page, x, y), body length and body
    let tag = base.len() - (16 + 33 + 1 + "hi @[acct_bob]\n\tthere".len()) - 1;
    assert_eq!(b[tag], 1);
    b[tag] = 99;
    assert!(CommentOperation::decode(&with_crc(b)).is_err());

    // over the size limit
    assert!(CommentOperation::decode(&vec![b'P'; limits::MAX_ENCODED_BYTES + 1]).is_err());
    assert!(CommentOperation::decode(&[]).is_err());
    assert!(CommentOperation::decode(b"PRCM").is_err());
}

#[test]
fn document_payloads_and_comment_payloads_never_decode_as_each_other() {
    // a document operation, as JSON and as a journal-style record, is not a comment
    let doc = LocalOperation {
        id: Uuid::from_u128(1),
        device_id: dev(1),
        local_sequence: 1,
        schema_version: 1,
        causal_dependencies: vec![],
        at: 0,
        body: DocumentOperation::NotebookRenamed {
            notebook_id: Uuid::from_u128(2),
            title: "x".into(),
        },
    };
    let json = serde_json::to_vec(&doc).unwrap();
    assert!(!is_comment_payload(&json));
    assert!(CommentOperation::decode(&json).is_err());
    for magic in [b"PRJR", b"PRCP", b"PRPG", b"PRSB", b"PRSL", b"PRHK"] {
        let mut b = magic.to_vec();
        b.extend_from_slice(&without_crc(&every_kind_of_op()[0])[4..]);
        assert!(CommentOperation::decode(&with_crc(b)).is_err());
    }
    // and an encoded comment is neither JSON nor a document operation
    let c = every_kind_of_op()[0].encode();
    assert!(serde_json::from_slice::<LocalOperation>(&c).is_err());
    assert!(serde_json::from_slice::<DocumentOperation>(&c).is_err());
}

#[test]
fn invalid_operations_are_refused() {
    let r = CommentReplica::new();
    let mut a = Writer::new(1, "acct_alice");
    let good = a.op(&r, create("ok"));
    let check = |f: &dyn Fn(&mut CommentOperation)| {
        let mut op = good.clone();
        f(&mut op);
        assert!(op.validate().is_err(), "{op:?}");
        assert!(CommentOperation::decode(&op.encode()).is_err());
        assert!(CommentReplica::new().apply(&op).is_err());
    };
    check(&|o| o.author = "not an id!".into());
    check(&|o| o.author = String::new());
    check(&|o| o.author = "a".repeat(129));
    check(&|o| o.sequence = 0);
    check(&|o| o.moderation = true);
    check(&|o| {
        o.op = CommentOp::ThreadCreated {
            thread_id: Uuid::from_u128(9),
            anchor: point(0.0, 0.0),
            body: "id differs from the op id".into(),
        }
    });
    check(&|o| {
        o.op = CommentOp::Replied {
            thread_id: Uuid::from_u128(9),
            comment_id: o.id,
            body: "self".into(),
            reply_to: Some(o.id),
        }
    });
    check(&|o| {
        o.causal_dependencies = vec![
            OperationRef {
                device_id: dev(5),
                device_seq: 1,
            },
            OperationRef {
                device_id: dev(4),
                device_seq: 1,
            },
        ]
    });
    check(&|o| {
        o.causal_dependencies = vec![OperationRef {
            device_id: o.device_id,
            device_seq: 1,
        }]
    });
    check(&|o| {
        o.causal_dependencies = vec![OperationRef {
            device_id: dev(4),
            device_seq: 0,
        }]
    });
    let bad_anchors = [
        point(f64::NAN, 0.0),
        point(f64::INFINITY, 0.0),
        point(2.0e6, 0.0),
        CommentAnchor::PageRect {
            page_id: PAGE,
            rect: Rect {
                x: 5.0,
                y: 0.0,
                w: -4.0,
                h: 1.0,
            },
        },
        CommentAnchor::PdfRegion {
            page_id: PAGE,
            pdf_page_index: 0,
            normalized_rect: rc([0.0, 0.0, 1.5, 1.0]),
            rotation: 0,
            pdf_asset_sha256: None,
        },
        CommentAnchor::PdfRegion {
            page_id: PAGE,
            pdf_page_index: 0,
            normalized_rect: rc([0.0, 0.0, 1.0, 1.0]),
            rotation: 45,
            pdf_asset_sha256: None,
        },
        CommentAnchor::TextRange {
            page_id: PAGE,
            element_id: TEXT,
            start: vec![],
            end: vec![1],
            quote: "q".into(),
            quote_hash: [0; 32],
            fallback_rect: None,
        },
        CommentAnchor::TextRange {
            page_id: PAGE,
            element_id: TEXT,
            start: vec![1],
            end: vec![1; 257],
            quote: "q".into(),
            quote_hash: [0; 32],
            fallback_rect: None,
        },
        CommentAnchor::TextRange {
            page_id: PAGE,
            element_id: TEXT,
            start: vec![1],
            end: vec![1],
            quote: "x".repeat(1025),
            quote_hash: [0; 32],
            fallback_rect: None,
        },
    ];
    for an in bad_anchors {
        check(&|o| {
            o.op = CommentOp::ThreadCreated {
                thread_id: o.id,
                anchor: an.clone(),
                body: "b".into(),
            }
        });
    }
    // newer schema in the struct
    let mut op = good.clone();
    op.schema_version = 9;
    assert_eq!(op.validate(), Err(CommentError::NewerSchema(9)));
}

#[test]
fn attribution_must_match_the_verified_envelope() {
    let r = CommentReplica::new();
    let mut a = Writer::new(1, "acct_alice");
    let mut op = a.op(&r, |_| CommentOp::CommentDeleted {
        comment_id: Uuid::from_u128(1),
    });
    assert!(op.check_attribution(dev(1), "acct_alice", false).is_ok());
    assert!(op.check_attribution(dev(2), "acct_alice", false).is_err());
    assert!(op.check_attribution(dev(1), "acct_mallory", false).is_err());
    op.moderation = true;
    assert!(op.check_attribution(dev(1), "acct_alice", false).is_err());
    assert!(op.check_attribution(dev(1), "acct_alice", true).is_ok());
}

// ---------------------------------------------------------------------------
// bodies and mentions

#[test]
fn bodies_are_plain_bounded_text_with_mention_tokens() {
    assert_eq!(
        parse_mentions(
            "hi @[acct_bob] and @[acct-carol], @[acct_bob] again; @dave @[] @[x y] @[@[z]"
        ),
        vec!["acct_bob", "acct-carol", "z"]
    );
    assert_eq!(parse_mentions("email@[example].com"), vec!["example"]);
    assert!(parse_mentions(&format!("@[{}]", "a".repeat(129))).is_empty());

    assert_eq!(
        sanitize_body("a\r\nb\rc\u{0}\u{7}\u{1b}[31m\u{202e}evil\u{2066}\td\u{85}"),
        "a\nb\nc[31mevil\td"
    );
    assert_eq!(validate_body("ok @[acct_bob]").unwrap(), vec!["acct_bob"]);
    assert!(validate_body("").is_err());
    assert!(validate_body(" \n\t").is_err());
    assert!(validate_body("a\rb").is_err());
    assert!(validate_body("a\u{0}b").is_err());
    assert!(validate_body("a\u{202e}b").is_err());
    assert!(
        validate_body("<script>alert(1)</script>").is_ok(),
        "plain text, escaped on display"
    );
    assert!(validate_body(&"é".repeat(4096)).is_ok());
    assert!(validate_body(&"é".repeat(4097)).is_err(), "8 KiB of UTF-8");
    let twenty: String = (0..20).map(|i| format!("@[m{i}] ")).collect();
    assert!(validate_body(&twenty).is_ok());
    assert!(validate_body(&format!("{twenty}@[m20]")).is_err());
    assert!(
        validate_body(&format!("{twenty}@[m0]")).is_ok(),
        "repeats count once"
    );
}

// ---------------------------------------------------------------------------
// merge rules

#[test]
fn retries_are_idempotent_and_reuse_is_an_error() {
    let mut r = CommentReplica::new();
    let mut a = Writer::new(1, "acct_alice");
    let op = a.op(&r, create("one"));
    assert_eq!(r.apply(&op), Ok(CommentApplied::Applied));
    assert_eq!(r.apply(&op), Ok(CommentApplied::Duplicate));
    let mut changed = op.clone();
    changed.op = CommentOp::ThreadCreated {
        thread_id: op.id,
        anchor: point(10.0, 20.0),
        body: "different".into(),
    };
    assert_eq!(r.apply(&changed), Err(CommentMergeError::OpIdReuse(op.id)));
    let mut seq_reuse = a.op(&r, create("two"));
    seq_reuse.sequence = 1;
    seq_reuse.causal_dependencies = vec![];
    assert!(matches!(
        r.apply(&seq_reuse),
        Err(CommentMergeError::SequenceReuse { .. })
    ));
    assert_eq!(r.threads().len(), 1);
    assert_eq!(r.threads()[0].comments[0].body.as_deref(), Some("one"));
}

#[test]
fn delivery_must_be_causal() {
    let mut a = Writer::new(1, "acct_alice");
    let mut b = Writer::new(2, "acct_bob");
    let mut ra = CommentReplica::new();
    let t = a.op(&ra, create("t"));
    ra.apply(&t).unwrap();
    let a2 = a.op(&ra, create("t2"));
    let reply_b = b.op(&ra, reply(t.id, "r"));

    let mut fresh = CommentReplica::new();
    assert!(matches!(
        fresh.apply(&a2),
        Err(CommentMergeError::MissingDependency(_))
    ));
    assert!(matches!(
        fresh.apply(&reply_b),
        Err(CommentMergeError::MissingDependency(_))
    ));
    assert!(fresh.threads().is_empty());
    apply_all(&mut fresh, &[&t, &reply_b, &a2]);
    assert_eq!(fresh.threads().len(), 2);
}

#[test]
fn only_authors_edit_and_owners_may_only_tombstone() {
    let mut r = CommentReplica::new();
    let mut alice = Writer::new(1, "acct_alice");
    let mut bob = Writer::new(2, "acct_bob");
    let mut owner = Writer::new(3, "acct_owner");
    let t = alice.op(&r, create("alice's words @[acct_bob]"));
    r.apply(&t).unwrap();
    let b_reply = bob.op(&r, reply(t.id, "bob's reply"));
    r.apply(&b_reply).unwrap();

    // bob can't edit or delete alice's comment
    let edit = bob.op(&r, |_| CommentOp::CommentEdited {
        comment_id: t.id,
        body: "rewritten".into(),
    });
    assert_eq!(
        r.apply(&edit),
        Ok(CommentApplied::Rejected(Rejection::NotAuthor(t.id)))
    );
    let del = bob.op(&r, |_| CommentOp::CommentDeleted { comment_id: t.id });
    assert_eq!(
        r.apply(&del),
        Ok(CommentApplied::Rejected(Rejection::NotAuthor(t.id)))
    );
    // an owner can't rewrite it either
    let edit = owner.op(&r, |_| CommentOp::CommentEdited {
        comment_id: t.id,
        body: "owner rewrite".into(),
    });
    assert_eq!(
        r.apply(&edit),
        Ok(CommentApplied::Rejected(Rejection::NotAuthor(t.id)))
    );
    // alice edits her own
    let edit = alice.op(&r, |_| CommentOp::CommentEdited {
        comment_id: t.id,
        body: "alice, edited".into(),
    });
    assert_eq!(r.apply(&edit), Ok(CommentApplied::Applied));
    // the owner removes bob's reply with the moderation flag
    let mut moderate = owner.op(&r, |_| CommentOp::CommentDeleted {
        comment_id: b_reply.id,
    });
    moderate.moderation = true;
    assert_eq!(r.apply(&moderate), Ok(CommentApplied::Applied));
    // a reply to the removed comment still attaches
    let after = alice.op(&r, |id| CommentOp::Replied {
        thread_id: t.id,
        comment_id: id,
        body: "answering the removed one".into(),
        reply_to: Some(b_reply.id),
    });
    assert_eq!(r.apply(&after), Ok(CommentApplied::Applied));

    let th = &r.threads()[0];
    assert_eq!(th.comments.len(), 3);
    let first = &th.comments[0];
    assert_eq!(first.body.as_deref(), Some("alice, edited"));
    assert!(first.edited);
    assert_eq!(first.edited_at, Some(edit.at));
    assert!(first.mentions.is_empty());
    let removed = th
        .comments
        .iter()
        .find(|c| c.comment_id == b_reply.id)
        .unwrap();
    assert_eq!(removed.state, CommentState::RemovedByOwner);
    assert_eq!(removed.body, None);
    assert_eq!(removed.author, "acct_bob");
    assert_eq!(
        th.comments
            .iter()
            .find(|c| c.comment_id == after.id)
            .unwrap()
            .reply_to,
        Some(b_reply.id)
    );

    // the author's own delete reads as deleted, not removed
    let own = alice.op(&r, |_| CommentOp::CommentDeleted { comment_id: t.id });
    r.apply(&own).unwrap();
    assert_eq!(r.threads()[0].comments[0].state, CommentState::Deleted);
    assert_eq!(
        r.threads()[0].comments[0].comment_id,
        t.id,
        "the structure stays"
    );
}

#[test]
fn operations_on_unseen_or_foreign_targets_are_rejected() {
    let mut r = CommentReplica::new();
    let mut a = Writer::new(1, "acct_alice");
    let mut b = Writer::new(2, "acct_bob");
    let blank = CommentReplica::new();
    let t1 = a.op(&r, create("t1"));
    // bob hasn't seen t1 (empty frontier), but it is applied here first
    let blind = b.op(&blank, reply(t1.id, "blind"));
    r.apply(&t1).unwrap();
    assert_eq!(
        r.apply(&blind),
        Ok(CommentApplied::Rejected(Rejection::UnknownThread(t1.id)))
    );
    let t2 = a.op(&r, create("t2"));
    r.apply(&t2).unwrap();
    let cross = b.op(&r, |id| CommentOp::Replied {
        thread_id: t2.id,
        comment_id: id,
        body: "x".into(),
        reply_to: Some(t1.id),
    });
    assert_eq!(
        r.apply(&cross),
        Ok(CommentApplied::Rejected(Rejection::ReplyOutsideThread(
            t1.id
        )))
    );
    let ghost = Uuid::from_u128(0xdead);
    let e = b.op(&r, |_| CommentOp::CommentEdited {
        comment_id: ghost,
        body: "x".into(),
    });
    assert_eq!(
        r.apply(&e),
        Ok(CommentApplied::Rejected(Rejection::UnknownComment(ghost)))
    );
    let res = b.op(&r, |_| CommentOp::ThreadResolved { thread_id: ghost });
    assert_eq!(
        r.apply(&res),
        Ok(CommentApplied::Rejected(Rejection::UnknownThread(ghost)))
    );
    // rejected ops still advance the frontier, so bob's later ops apply
    let ok = b.op(&r, reply(t2.id, "fine"));
    assert_eq!(r.apply(&ok), Ok(CommentApplied::Applied));
    assert!(r.contains(blind.id));
    assert_eq!(r.thread(t2.id).unwrap().comments.len(), 2);
    assert_eq!(r.thread(t1.id).unwrap().comments.len(), 1);
}

#[test]
fn a_reply_racing_a_resolve_reopens_the_thread_in_any_order() {
    let mut a = Writer::new(1, "acct_alice");
    let mut b = Writer::new(2, "acct_bob");
    let mut base = CommentReplica::new();
    let t = a.op(&base, create("question"));
    base.apply(&t).unwrap();
    let resolve = a.op(&base, |_| CommentOp::ThreadResolved { thread_id: t.id });
    let reply_offline = b.op(&base, reply(t.id, "wait, one more thing"));
    for order in [[&resolve, &reply_offline], [&reply_offline, &resolve]] {
        let mut r = base.clone();
        apply_all(&mut r, &order);
        let th = r.thread(t.id).unwrap();
        assert_eq!(th.status, ThreadStatus::Open);
        assert_eq!(
            th.status_change.as_ref().unwrap().kind,
            StatusChangeKind::Replied
        );
        assert_eq!(th.comments.len(), 2, "the reply is kept");
    }
    // a resolve made after seeing the reply closes it
    let mut r = base.clone();
    apply_all(&mut r, &[&resolve, &reply_offline]);
    let again = a.op(&r, |_| CommentOp::ThreadResolved { thread_id: t.id });
    r.apply(&again).unwrap();
    let th = r.thread(t.id).unwrap();
    assert_eq!(th.status, ThreadStatus::Resolved);
    assert_eq!(th.status_change.unwrap().by, "acct_alice");
    // a reply made after seeing the resolve reopens it
    let late = b.op(&r, reply(t.id, "reopening"));
    r.apply(&late).unwrap();
    assert_eq!(r.thread(t.id).unwrap().status, ThreadStatus::Open);
    // edits and deletes don't reopen
    let res = a.op(&r, |_| CommentOp::ThreadResolved { thread_id: t.id });
    r.apply(&res).unwrap();
    let e = b.op(&r, |_| CommentOp::CommentEdited {
        comment_id: late.id,
        body: "x".into(),
    });
    r.apply(&e).unwrap();
    assert_eq!(r.thread(t.id).unwrap().status, ThreadStatus::Resolved);
}

#[test]
fn concurrent_resolve_and_reopen_go_to_the_highest_operation_id() {
    let mut a = Writer::new(1, "acct_alice");
    let mut b = Writer::new(2, "acct_bob");
    let mut base = CommentReplica::new();
    let t = a.op(&base, create("t"));
    base.apply(&t).unwrap();
    let res = base.clone();
    let resolve = a.op(&res, |_| CommentOp::ThreadResolved { thread_id: t.id });
    let mut seen = base.clone();
    seen.apply(&resolve).unwrap();
    let mut c = Writer::new(3, "acct_carol");
    // bob reopens after seeing the resolve; carol resolves concurrently with bob
    let reopen = b.op(&seen, |_| CommentOp::ThreadReopened { thread_id: t.id });
    let resolve2 = c.op(&seen, |_| CommentOp::ThreadResolved { thread_id: t.id });
    let expect = if reopen.id > resolve2.id {
        ThreadStatus::Open
    } else {
        ThreadStatus::Resolved
    };
    for order in [[&reopen, &resolve2], [&resolve2, &reopen]] {
        let mut r = seen.clone();
        apply_all(&mut r, &order);
        assert_eq!(r.thread(t.id).unwrap().status, expect);
    }
}

#[test]
fn concurrent_edits_keep_alternates_and_a_delete_wins() {
    let mut a1 = Writer::new(1, "acct_alice");
    let mut a2 = Writer::new(2, "acct_alice"); // alice's second device
    let mut base = CommentReplica::new();
    let t = a1.op(&base, create("draft"));
    base.apply(&t).unwrap();
    let e1 = a1.op(&base, |_| CommentOp::CommentEdited {
        comment_id: t.id,
        body: "from laptop".into(),
    });
    let e2 = a2.op(&base, |_| CommentOp::CommentEdited {
        comment_id: t.id,
        body: "from tablet".into(),
    });
    let mut views = vec![];
    for order in [[&e1, &e2], [&e2, &e1]] {
        let mut r = base.clone();
        apply_all(&mut r, &order);
        views.push(r.threads());
    }
    assert_eq!(views[0], views[1]);
    let c = &views[0][0].comments[0];
    let (win, lose) = if e1.id > e2.id {
        ("from laptop", "from tablet")
    } else {
        ("from tablet", "from laptop")
    };
    assert_eq!(c.body.as_deref(), Some(win));
    assert_eq!(c.alternate_bodies, vec![lose.to_string()]);

    // a delete concurrent with an edit: deleted either way, no text kept
    let mut after_e1 = base.clone();
    after_e1.apply(&e1).unwrap();
    let del = a1.op(&after_e1, |_| CommentOp::CommentDeleted {
        comment_id: t.id,
    });
    let mut views = vec![];
    for order in [[&del, &e2], [&e2, &del]] {
        let mut r = after_e1.clone();
        apply_all(&mut r, &order);
        views.push(r.threads());
    }
    assert_eq!(views[0], views[1]);
    assert_eq!(views[0][0].comments[0].state, CommentState::Deleted);
    assert!(!format!("{:?}", views[0]).contains("tablet"));
}

#[test]
fn concurrent_reattachments_keep_the_other_anchor() {
    let mut a = Writer::new(1, "acct_alice");
    let mut b = Writer::new(2, "acct_bob");
    let mut base = CommentReplica::new();
    let t = a.op(&base, create("t"));
    base.apply(&t).unwrap();
    let ra = a.op(&base, |_| CommentOp::AnchorReattached {
        thread_id: t.id,
        anchor: point(1.0, 1.0),
    });
    let rb = b.op(&base, |_| CommentOp::AnchorReattached {
        thread_id: t.id,
        anchor: point(2.0, 2.0),
    });
    let mut r = base.clone();
    apply_all(&mut r, &[&rb, &ra]);
    let th = r.thread(t.id).unwrap();
    let (win, lose) = if ra.id > rb.id {
        (point(1.0, 1.0), point(2.0, 2.0))
    } else {
        (point(2.0, 2.0), point(1.0, 1.0))
    };
    assert_eq!(th.anchor, win);
    assert_eq!(th.anchor_alternates, vec![lose]);
}

// ---------------------------------------------------------------------------
// order independence

/// xorshift64*: a small deterministic generator, so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> Option<&'a T> {
        (!v.is_empty()).then(|| &v[self.below(v.len())])
    }
}

/// A random history: devices of three members (one an owner) writing,
/// editing, deleting, resolving and replying, syncing with each other now and
/// then. Returns every operation.
fn random_history(rng: &mut Rng, steps: usize) -> Vec<CommentOperation> {
    let authors = ["acct_alice", "acct_bob", "acct_owner", "acct_alice"];
    let mut devs: Vec<(Writer, CommentReplica, Vec<CommentOperation>)> = authors
        .iter()
        .enumerate()
        .map(|(i, a)| {
            (
                Writer::new(i as u128 + 1, a),
                CommentReplica::new(),
                Vec::new(),
            )
        })
        .collect();
    for _ in 0..steps {
        let d = rng.below(devs.len());
        if rng.below(4) == 0 {
            // sync: take everything another device has, in its order (causal)
            let o = rng.below(devs.len());
            if o != d {
                let theirs = devs[o].2.clone();
                let (_, r, log) = &mut devs[d];
                for op in theirs {
                    if !r.contains(op.id) {
                        r.apply(&op).unwrap();
                        log.push(op);
                    }
                }
            }
            continue;
        }
        let (w, r, log) = &mut devs[d];
        let threads = r.threads();
        let thread_ids: Vec<Uuid> = threads.iter().map(|t| t.thread_id).collect();
        let comment_ids: Vec<Uuid> = threads
            .iter()
            .flat_map(|t| t.comments.iter().map(|c| c.comment_id))
            .collect();
        let n = rng.next();
        let pick_t = rng.pick(&thread_ids).copied();
        let pick_c = rng.pick(&comment_ids).copied();
        let body = format!(
            "b{} @[acct_{}]",
            n % 1000,
            ["alice", "bob"][(n % 2) as usize]
        );
        let mut moderation = false;
        let make: Box<dyn FnOnce(Uuid) -> CommentOp> = match (rng.below(8), pick_t, pick_c) {
            (0, _, _) | (_, None, _) => Box::new(move |id| CommentOp::ThreadCreated {
                thread_id: id,
                anchor: CommentAnchor::PagePoint {
                    page_id: PAGE,
                    x: -((n % 500) as f64),
                    y: (n % 77) as f64,
                },
                body,
            }),
            (1 | 2, Some(t), c) => {
                // reply_to may be in another thread (then it is rejected everywhere)
                let rt = c.filter(|_| n.is_multiple_of(3));
                Box::new(move |id| CommentOp::Replied {
                    thread_id: t,
                    comment_id: id,
                    body,
                    reply_to: rt,
                })
            }
            (3, _, Some(c)) => Box::new(move |_| CommentOp::CommentEdited {
                comment_id: c,
                body,
            }),
            (4, _, Some(c)) => {
                moderation = w.author == "acct_owner";
                Box::new(move |_| CommentOp::CommentDeleted { comment_id: c })
            }
            (5, Some(t), _) => Box::new(move |_| CommentOp::ThreadResolved { thread_id: t }),
            (6, Some(t), _) => Box::new(move |_| CommentOp::ThreadReopened { thread_id: t }),
            (_, Some(t), _) => Box::new(move |_| CommentOp::AnchorReattached {
                thread_id: t,
                anchor: CommentAnchor::PageRect {
                    page_id: PAGE,
                    rect: rc([-5.0, -5.0, (n % 9) as f64, 9.0]),
                },
            }),
        };
        let mut op = w.op(r, make);
        op.moderation = moderation;
        r.apply(&op).unwrap();
        log.push(op);
    }
    let mut all: Vec<CommentOperation> = Vec::new();
    let mut ids = HashSet::new();
    for (_, _, log) in devs {
        for op in log {
            if ids.insert(op.id) {
                all.push(op);
            }
        }
    }
    all
}

/// A random causal delivery order of `ops`, with some retries mixed in.
fn shuffled_causal(rng: &mut Rng, ops: &[CommentOperation]) -> Vec<CommentOperation> {
    let mut have: BTreeMap<Uuid, u64> = BTreeMap::new();
    let mut left: Vec<&CommentOperation> = ops.iter().collect();
    let mut out = Vec::new();
    while !left.is_empty() {
        let ready: Vec<usize> = (0..left.len())
            .filter(|&i| {
                let o = left[i];
                have.get(&o.device_id).copied().unwrap_or(0) + 1 == o.sequence
                    && o.causal_dependencies.iter().all(|d| {
                        d.device_id == o.device_id
                            || have.get(&d.device_id).copied().unwrap_or(0) >= d.device_seq
                    })
            })
            .collect();
        let i = ready[rng.below(ready.len())];
        let o = left.remove(i);
        have.insert(o.device_id, o.sequence);
        out.push(o.clone());
        if rng.below(10) == 0 {
            let j = rng.below(out.len());
            out.push(out[j].clone());
        }
    }
    out
}

#[test]
fn any_causal_delivery_order_gives_the_same_threads() {
    let mut saw_resolved = false;
    let mut saw_reply_reopen = false;
    let mut saw_rejection = false;
    let mut saw_removed = false;
    for seed in 1..=60u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let ops = random_history(&mut rng, 120);
        let mut reference: Option<String> = None;
        for _ in 0..8 {
            let mut r = CommentReplica::new();
            for op in shuffled_causal(&mut rng, &ops) {
                match r.apply(&op).unwrap() {
                    CommentApplied::Rejected(_) => saw_rejection = true,
                    CommentApplied::Applied | CommentApplied::Duplicate => {}
                }
            }
            let threads = r.threads();
            for t in &threads {
                saw_resolved |= t.status == ThreadStatus::Resolved;
                saw_reply_reopen |= t
                    .status_change
                    .as_ref()
                    .is_some_and(|s| s.kind == StatusChangeKind::Replied);
                saw_removed |= t
                    .comments
                    .iter()
                    .any(|c| c.state == CommentState::RemovedByOwner);
            }
            let json = serde_json::to_string(&threads).unwrap();
            match &reference {
                None => reference = Some(json),
                Some(want) => assert_eq!(&json, want, "seed {seed} diverged"),
            }
        }
    }
    assert!(
        saw_resolved && saw_reply_reopen && saw_rejection && saw_removed,
        "the histories exercise every rule"
    );
}

// ---------------------------------------------------------------------------
// anchor resolution

fn pdf_ref(index: u32, rotation: u32) -> PdfBackgroundRef {
    PdfBackgroundRef {
        asset_hash: "ab".repeat(32),
        page_index: index,
        media_box: [0.0, 0.0, 600.0, 800.0],
        crop_box: [0.0, 0.0, 600.0, 800.0],
        rotation,
    }
}

fn stroke(id: Uuid, pts: &[(f64, f64)]) -> Element {
    let points: Vec<serde_json::Value> = pts
        .iter()
        .map(|(x, y)| serde_json::json!({ "x": x, "y": y, "pressure": 0.5 }))
        .collect();
    Element::Ink(
        serde_json::from_value(serde_json::json!({
            "id": id, "z": 0, "tool": "pen", "color": "#000000", "width": 2.0,
            "source": "test", "points": points
        }))
        .unwrap(),
    )
}

fn text_block(id: Uuid, text: &str) -> Element {
    Element::Text(
        serde_json::from_value(serde_json::json!({
            "id": id, "z": 1, "x": 100.0, "y": 200.0, "width": 300.0, "height": 40.0,
            "text": text, "fontSize": 16.0, "color": "#000000", "align": "left"
        }))
        .unwrap(),
    )
}

fn image_pdf(id: Uuid, index: u32, rotation: u32) -> Element {
    let mut v = serde_json::json!({
        "type": "image", "id": id, "z": 2, "x": -1000.0, "y": -2000.0, "width": 800.0, "height": 600.0,
        "assetHash": "ab".repeat(32)
    });
    v["pdfPage"] = serde_json::to_value(pdf_ref(index, rotation)).unwrap();
    serde_json::from_value(v).unwrap()
}

/// The fixed scene the resolution vectors run against.
fn scene() -> (Notebook, Vec<Page>) {
    let size = Size {
        width: 600.0,
        height: 800.0,
    };
    let mut nb = Notebook::new(
        Uuid::from_u128(0x4000),
        "N".into(),
        PageDefaults::default(),
        "#336699".into(),
        0,
    );
    nb.page_order = vec![PAGE, PAGE2, PDF_PAGE];
    nb.trashed_pages = vec![TrashedPage {
        id: TRASHED,
        deleted_at: 1,
        former_index: 1,
    }];
    let mut p = Page::new(PAGE, Paper::A4, size, Template::default(), 0);
    p.elements = vec![
        stroke(STROKE, &[(-50.0, -60.0), (-10.0, -20.0)]),
        text_block(TEXT, "say 日本 👩🏽‍🔬 ok"),
        image_pdf(IMAGE_PDF, 3, 90),
        image_pdf(Uuid::from_u128(0x6003), 4, 0),
        image_pdf(Uuid::from_u128(0x6004), 4, 0),
    ];
    let p2 = Page::new(PAGE2, Paper::A4, size, Template::default(), 0);
    let mut pdf = Page::new(
        PDF_PAGE,
        Paper::A4,
        Size {
            width: 800.0,
            height: 600.0,
        },
        Template::default(),
        0,
    );
    pdf.background = Some(pdf_ref(0, 90));
    (nb, vec![p, p2, pdf])
}

/// Sticky indexes are opaque here: the vectors use one-byte stand-ins
/// resolved by this table (`[n]` → offset n; `[255]` → gone).
fn table_offset(_: Uuid, _: Uuid, b: &[u8]) -> Option<usize> {
    (b != [255]).then(|| b[0] as usize)
}

fn text_anchor(start: u8, end: u8, quoted: &str) -> CommentAnchor {
    CommentAnchor::TextRange {
        page_id: PAGE,
        element_id: TEXT,
        start: vec![start],
        end: vec![end],
        quote: quoted.into(),
        quote_hash: quote_hash(quoted),
        fallback_rect: Some(rc([100.0, 200.0, 150.0, 220.0])),
    }
}

fn resolution_cases() -> Vec<(&'static str, CommentAnchor)> {
    let pdf = |page_id, i, r: Rect, rotation| CommentAnchor::PdfRegion {
        page_id,
        pdf_page_index: i,
        normalized_rect: r,
        rotation,
        pdf_asset_sha256: None,
    };
    vec![
        (
            "negative point on an infinite canvas",
            point(-1500.25, -3.5),
        ),
        (
            "rect",
            CommentAnchor::PageRect {
                page_id: PAGE,
                rect: rc([-1.0, -2.0, 3.0, 4.0]),
            },
        ),
        (
            "element follows the stroke",
            CommentAnchor::Element {
                page_id: PAGE,
                element_id: STROKE,
                fallback_rect: Some(rc([0.0, 0.0, 1.0, 1.0])),
            },
        ),
        (
            "deleted element",
            CommentAnchor::Element {
                page_id: PAGE,
                element_id: Uuid::from_u128(0x6fff),
                fallback_rect: Some(rc([0.0, 0.0, 1.0, 1.0])),
            },
        ),
        (
            "element anchor never moves to another page",
            CommentAnchor::Element {
                page_id: PAGE2,
                element_id: STROKE,
                fallback_rect: None,
            },
        ),
        (
            "trashed page",
            CommentAnchor::PagePoint {
                page_id: TRASHED,
                x: 1.0,
                y: 1.0,
            },
        ),
        (
            "deleted page",
            CommentAnchor::PageRect {
                page_id: GONE,
                rect: rc([0.0, 0.0, 1.0, 1.0]),
            },
        ),
        // "say 日本 👩🏽‍🔬 ok": 日本 is UTF-16 [4, 6), the emoji sequence [7, 14)
        ("text range still matching", text_anchor(4, 6, "日本")),
        (
            "text range over an emoji sequence",
            text_anchor(7, 14, "👩🏽‍🔬"),
        ),
        ("text range whose text changed", text_anchor(4, 6, "中国")),
        (
            "text range inside a surrogate pair",
            text_anchor(8, 14, "👩🏽‍🔬"),
        ),
        (
            "text range whose sticky index is gone",
            text_anchor(255, 6, "日本"),
        ),
        (
            "text range on a deleted block",
            CommentAnchor::TextRange {
                page_id: PAGE,
                element_id: Uuid::from_u128(0x6fff),
                start: vec![0],
                end: vec![1],
                quote: "s".into(),
                quote_hash: quote_hash("s"),
                fallback_rect: None,
            },
        ),
        (
            "text range on a stroke",
            CommentAnchor::TextRange {
                page_id: PAGE,
                element_id: STROKE,
                start: vec![0],
                end: vec![1],
                quote: "s".into(),
                quote_hash: quote_hash("s"),
                fallback_rect: None,
            },
        ),
        (
            "pdf background measured unrotated",
            pdf(PDF_PAGE, 0, rc([0.0, 0.0, 0.5, 0.25]), 0),
        ),
        (
            "pdf background measured as displayed",
            pdf(PDF_PAGE, 0, rc([0.5, 0.0, 1.0, 0.5]), 90),
        ),
        (
            "pdf page on a canvas, rotated",
            pdf(PAGE, 3, rc([0.0, 0.0, 0.5, 0.25]), 0),
        ),
        (
            "pdf page shown twice is not guessed",
            pdf(PAGE, 4, rc([0.0, 0.0, 1.0, 1.0]), 0),
        ),
        (
            "pdf page not shown",
            pdf(PAGE, 9, rc([0.0, 0.0, 1.0, 1.0]), 0),
        ),
        (
            "pdf background of the same document",
            CommentAnchor::PdfRegion {
                page_id: PDF_PAGE,
                pdf_page_index: 0,
                normalized_rect: rc([0.0, 0.0, 0.5, 0.25]),
                rotation: 0,
                pdf_asset_sha256: Some([0xab; 32]),
            },
        ),
        (
            "pdf background replaced by another document",
            CommentAnchor::PdfRegion {
                page_id: PDF_PAGE,
                pdf_page_index: 0,
                normalized_rect: rc([0.0, 0.0, 0.5, 0.25]),
                rotation: 0,
                pdf_asset_sha256: Some([0xcd; 32]),
            },
        ),
        ("live page whose content wasn't supplied", point(1.0, 1.0)),
    ]
}

fn resolve_case(i: usize, anchor: &CommentAnchor) -> Resolved {
    let (nb, pages) = scene();
    // the last case leaves the page out on purpose
    let pages = if i + 1 == resolution_cases().len() {
        &pages[1..]
    } else {
        &pages[..]
    };
    resolve(anchor, &nb, pages, &table_offset)
}

#[test]
fn anchors_resolve_as_the_rules_say() {
    let cases = resolution_cases();
    let got: HashMap<&str, Resolved> = cases
        .iter()
        .enumerate()
        .map(|(i, (n, a))| (*n, resolve_case(i, a)))
        .collect();
    let placed = |n: &str| match &got[n] {
        Resolved::Placed {
            page_id,
            rect,
            text_range,
        } => (*page_id, *rect, *text_range),
        o => panic!("{n}: {o:?}"),
    };
    let orphan = |n: &str| match &got[n] {
        Resolved::Orphaned {
            reason,
            last_known_page,
            quote,
        } => (*reason, *last_known_page, quote.clone()),
        o => panic!("{n}: {o:?}"),
    };
    assert_eq!(
        placed("negative point on an infinite canvas").1.corners(),
        [-1500.25, -3.5, -1500.25, -3.5]
    );
    assert_eq!(
        placed("element follows the stroke").1.corners(),
        [-51.0, -61.0, -9.0, -19.0]
    );
    assert_eq!(orphan("deleted element").0, OrphanReason::ElementDeleted);
    assert_eq!(
        orphan("element anchor never moves to another page"),
        (OrphanReason::ElementDeleted, PAGE2, None)
    );
    assert_eq!(orphan("trashed page").0, OrphanReason::PageTrashed);
    assert_eq!(
        orphan("deleted page"),
        (OrphanReason::PageDeleted, GONE, None)
    );
    assert_eq!(placed("text range still matching").2, Some([4, 6]));
    assert_eq!(placed("text range over an emoji sequence").2, Some([7, 14]));
    assert_eq!(
        orphan("text range whose text changed"),
        (OrphanReason::QuoteMismatch, PAGE, Some("中国".into()))
    );
    assert_eq!(
        orphan("text range inside a surrogate pair").0,
        OrphanReason::TextRangeLost
    );
    assert_eq!(
        orphan("text range whose sticky index is gone").0,
        OrphanReason::TextRangeLost
    );
    assert_eq!(
        orphan("text range on a deleted block").0,
        OrphanReason::ElementDeleted
    );
    assert_eq!(
        orphan("text range on a stroke").0,
        OrphanReason::TextRangeLost
    );
    assert_eq!(
        orphan("pdf page shown twice is not guessed").0,
        OrphanReason::PdfPageMissing
    );
    assert_eq!(orphan("pdf page not shown").0, OrphanReason::PdfPageMissing);
    assert_eq!(
        placed("pdf background of the same document").1,
        placed("pdf background measured unrotated").1
    );
    assert_eq!(
        orphan("pdf background replaced by another document"),
        (OrphanReason::PdfPageMissing, PDF_PAGE, None)
    );
    assert_eq!(
        orphan("live page whose content wasn't supplied").0,
        OrphanReason::PageUnavailable
    );
    // the unrotated top-left eighth of a page shown at 90°: the top-right strip
    assert_eq!(
        placed("pdf background measured unrotated").1.corners(),
        [600.0, 0.0, 800.0, 300.0]
    );
    // measured as displayed at 90° and still displayed at 90°: unchanged
    assert_eq!(
        placed("pdf background measured as displayed").1.corners(),
        [400.0, 0.0, 800.0, 300.0]
    );
    // a PDF page placed on a canvas at negative coordinates
    assert_eq!(
        placed("pdf page on a canvas, rotated").1.corners(),
        [-400.0, -2000.0, -200.0, -1700.0]
    );
}

/// The PDF mapping agrees with the export matrix the page model already uses:
/// a point given as a fraction of the *unrotated* crop box lands, after
/// [`PdfBackgroundRef::doc_to_pdf_matrix`], on the same PDF user-space point.
#[test]
fn pdf_regions_agree_with_the_export_transform() {
    for rotation in [0u32, 90, 180, 270] {
        let bg = PdfBackgroundRef {
            crop_box: [10.0, 20.0, 610.0, 820.0],
            ..pdf_ref(0, rotation)
        };
        let size = bg.display_size();
        let mut nb = Notebook::new(
            Uuid::from_u128(1),
            "N".into(),
            PageDefaults::default(),
            "#336699".into(),
            0,
        );
        nb.page_order = vec![PDF_PAGE];
        let mut page = Page::new(PDF_PAGE, Paper::A4, size, Template::default(), 0);
        page.background = Some(bg.clone());
        for (u, v) in [(0.0, 0.0), (0.25, 0.75), (1.0, 0.5)] {
            let anchor = CommentAnchor::PdfRegion {
                page_id: PDF_PAGE,
                pdf_page_index: 0,
                normalized_rect: rc([u, v, u, v]),
                rotation: 0,
                pdf_asset_sha256: None,
            };
            let Resolved::Placed { rect, .. } =
                resolve(&anchor, &nb, std::slice::from_ref(&page), &table_offset)
            else {
                panic!()
            };
            let m = bg.doc_to_pdf_matrix();
            let (x, y) = (rect.x, rect.y);
            let pdf = (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
            // the unrotated crop box: x from the left, v from the top (PDF y is up)
            let want = (10.0 + u * 600.0, 820.0 - v * 800.0);
            assert!(
                (pdf.0 - want.0).abs() < 1e-9 && (pdf.1 - want.1).abs() < 1e-9,
                "rotation {rotation} ({u},{v}): {pdf:?} vs {want:?}"
            );
        }
    }
}

/// Text anchors with real sticky indexes from the text CRDT: two people edit
/// around and inside the quoted range concurrently; the anchor follows the
/// text, or is orphaned with its quote, and never lands on other text.
#[test]
fn text_anchors_follow_concurrent_edits_or_orphan_with_their_quote() {
    use yrs::updates::decoder::Decode;
    use yrs::updates::encoder::Encode;
    use yrs::{
        Assoc, Doc, GetString, IndexedSequence, OffsetKind, Options, ReadTxn, StateVector,
        StickyIndex, Text, Transact, Update,
    };

    let doc = |client| {
        Doc::with_options(Options {
            client_id: yrs::block::ClientID::new(client),
            offset_kind: OffsetKind::Utf16,
            ..Default::default()
        })
    };
    let sync = |from: &Doc, to: &Doc| {
        let u = from
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        to.transact_mut()
            .apply_update(Update::decode_v1(&u).unwrap())
            .unwrap();
    };
    let a = doc(1);
    let b = doc(2);
    let ta = a.get_or_insert_text("text");
    let tb = b.get_or_insert_text("text");
    ta.insert(&mut a.transact_mut(), 0, "the quick brown fox");
    sync(&a, &b);
    // anchor "quick" ([4, 9)): start sticks to the following char, end to the preceding one
    let (start, end) = {
        let tx = a.transact();
        (
            ta.sticky_index(&tx, 4, Assoc::After).unwrap().encode_v1(),
            ta.sticky_index(&tx, 9, Assoc::Before).unwrap().encode_v1(),
        )
    };
    let anchor = CommentAnchor::TextRange {
        page_id: PAGE,
        element_id: TEXT,
        start: start.clone(),
        end: end.clone(),
        quote: "quick".into(),
        quote_hash: quote_hash("quick"),
        fallback_rect: None,
    };
    let place = |d: &Doc, t: &yrs::TextRef| {
        let text = t.get_string(&d.transact());
        let (mut nb, _) = scene();
        nb.page_order = vec![PAGE];
        let mut page = Page::new(
            PAGE,
            Paper::A4,
            Size {
                width: 600.0,
                height: 800.0,
            },
            Template::default(),
            0,
        );
        page.elements = vec![text_block(TEXT, &text)];
        let offset = |_: Uuid, _: Uuid, b: &[u8]| -> Option<usize> {
            let s = StickyIndex::decode_v1(b).ok()?;
            Some(s.get_offset(&d.transact())?.index as usize)
        };
        resolve(&anchor, &nb, &[page], &offset)
    };

    // concurrent edits next to the range on both sides
    ta.insert(&mut a.transact_mut(), 0, "see ");
    tb.insert(&mut b.transact_mut(), 9, " very");
    sync(&a, &b);
    sync(&b, &a);
    assert_eq!(ta.get_string(&a.transact()), "see the quick very brown fox");
    for (d, t) in [(&a, &ta), (&b, &tb)] {
        assert!(
            matches!(
                place(d, t),
                Resolved::Placed {
                    text_range: Some([8, 13]),
                    ..
                }
            ),
            "{:?}",
            place(d, t)
        );
    }

    // someone edits inside the quoted range: orphaned with the quote, not moved
    {
        let mut tx = b.transact_mut();
        tb.remove_range(&mut tx, 9, 2); // "quick" becomes "qck"
    }
    sync(&b, &a);
    for (d, t) in [(&a, &ta), (&b, &tb)] {
        match place(d, t) {
            Resolved::Orphaned { reason, quote, .. } => {
                assert_eq!(reason, OrphanReason::QuoteMismatch);
                assert_eq!(quote.as_deref(), Some("quick"));
            }
            other => panic!("{other:?}"),
        }
    }

    // the whole range deleted and the same word typed elsewhere: never re-attached
    {
        let mut tx = a.transact_mut();
        let s = ta.get_string(&tx);
        let at = s.find("qck").unwrap() as u32; // ASCII: bytes are UTF-16 units
        ta.remove_range(&mut tx, at, 3);
        ta.push(&mut tx, " quick");
    }
    assert!(ta.get_string(&a.transact()).ends_with(" quick"));
    let r = place(&a, &ta);
    assert!(matches!(r, Resolved::Orphaned { .. }), "{r:?}");
}

// ---------------------------------------------------------------------------
// golden vectors

fn vectors_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/comments.json")
}

/// A fixed history covering every operation: alice starts a thread anchored
/// to a stroke, bob replies offline while alice resolves, the owner removes a
/// reply, alice edits, bob reattaches.
fn golden_ops() -> Vec<CommentOperation> {
    let mut alice = Writer::new(1, "acct_alice");
    let mut bob = Writer::new(2, "acct_bob");
    let mut owner = Writer::new(3, "acct_owner");
    let mut r = CommentReplica::new();
    let mut out = Vec::new();
    let mut push = |r: &mut CommentReplica, op: CommentOperation| {
        r.apply(&op).unwrap();
        out.push(op);
    };
    let t = alice.op(&r, |id| CommentOp::ThreadCreated {
        thread_id: id,
        anchor: CommentAnchor::Element {
            page_id: PAGE,
            element_id: STROKE,
            fallback_rect: Some(rc([-51.0, -61.0, -9.0, -19.0])),
        },
        body: "Is this arrow right, @[acct_bob]?".into(),
    });
    let tid = t.id;
    push(&mut r, t);
    let t2 = bob.op(&r, |id| CommentOp::ThreadCreated {
        thread_id: id,
        anchor: text_anchor(4, 6, "日本"),
        body: "Typo here?".into(),
    });
    let t2id = t2.id;
    push(&mut r, t2);
    let snapshot = r.clone();
    let resolve_op = alice.op(&snapshot, |_| CommentOp::ThreadResolved { thread_id: tid });
    let offline_reply = bob.op(&snapshot, |id| CommentOp::Replied {
        thread_id: tid,
        comment_id: id,
        body: "Not quite: it should point left.".into(),
        reply_to: Some(tid),
    });
    let reply_id = offline_reply.id;
    push(&mut r, resolve_op);
    push(&mut r, offline_reply);
    let spam = bob.op(&r, reply(t2id, "buy now"));
    let spam_id = spam.id;
    push(&mut r, spam);
    let mut removal = owner.op(&r, |_| CommentOp::CommentDeleted {
        comment_id: spam_id,
    });
    removal.moderation = true;
    push(&mut r, removal);
    let op = alice.op(&r, |_| CommentOp::CommentEdited {
        comment_id: tid,
        body: "Is this arrow right, @[acct_bob]? (the red one)".into(),
    });
    push(&mut r, op);
    let rejected = alice.op(&r, |_| CommentOp::CommentEdited {
        comment_id: reply_id,
        body: "not mine".into(),
    });
    push(&mut r, rejected);
    let op = bob.op(&r, |_| CommentOp::AnchorReattached {
        thread_id: t2id,
        anchor: point(-12.5, 40.0),
    });
    push(&mut r, op);
    let op = owner.op(&r, |_| CommentOp::ThreadResolved { thread_id: t2id });
    push(&mut r, op);
    let op = bob.op(&r, |_| CommentOp::ThreadReopened { thread_id: t2id });
    push(&mut r, op);
    out
}

fn produce() -> serde_json::Value {
    let ops = golden_ops();
    let mut r = CommentReplica::new();
    let outcomes: Vec<String> = ops
        .iter()
        .map(|o| format!("{:?}", r.apply(o).unwrap()))
        .collect();
    let cases = resolution_cases();
    serde_json::json!({
        "format": "penroam-comment-vectors",
        "encoding": "comment operation schema 1, base64url",
        "operations": ops.iter().zip(&outcomes).map(|(o, out)| serde_json::json!({
            "operation": o,
            "encoded": b64::encode(&o.encode()),
            "outcome": out,
        })).collect::<Vec<_>>(),
        "threads": r.threads(),
        "resolutions": cases.iter().enumerate().map(|(i, (name, a))| serde_json::json!({
            "case": name,
            "anchor": a,
            "expected": resolve_case(i, a),
        })).collect::<Vec<_>>(),
    })
}

/// Writes the fixture (run with `--ignored` after a deliberate change).
#[test]
#[ignore]
fn write_golden_vectors() {
    std::fs::write(
        vectors_path(),
        serde_json::to_string_pretty(&produce()).unwrap() + "\n",
    )
    .unwrap();
}

#[test]
fn golden_vectors_match_this_build() {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(vectors_path()).unwrap()).unwrap();
    assert_eq!(
        v,
        produce(),
        "comment encoding, merge or resolution changed"
    );

    // decode the committed bytes independently of the code that wrote them
    let ops: Vec<CommentOperation> = v["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let bytes = b64::decode(o["encoded"].as_str().unwrap()).unwrap();
            let op = CommentOperation::decode(&bytes).unwrap();
            let from_json: CommentOperation =
                serde_json::from_value(o["operation"].clone()).unwrap();
            assert_eq!(op, from_json);
            op
        })
        .collect();
    let want: Vec<CommentThread> = serde_json::from_value(v["threads"].clone()).unwrap();
    let mut rng = Rng(7);
    for _ in 0..20 {
        let mut r = CommentReplica::new();
        for op in shuffled_causal(&mut rng, &ops) {
            r.apply(&op).unwrap();
        }
        assert_eq!(r.threads(), want);
    }
    for (i, case) in v["resolutions"].as_array().unwrap().iter().enumerate() {
        let a: CommentAnchor = serde_json::from_value(case["anchor"].clone()).unwrap();
        let want: Resolved = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(resolve_case(i, &a), want, "{}", case["case"]);
    }
}

/// The anchor JSON is the shape the app sends: tagged by `kind`, camelCase
/// fields, rects as `{x, y, w, h}`, sticky indexes base64url, hash hex.
#[test]
fn anchor_json_matches_the_app_shape() {
    let id = |n| Uuid::from_u128(n).to_string();
    let r = serde_json::json!({ "x": -1.5, "y": 2.0, "w": 3.0, "h": 4.0 });
    let n = serde_json::json!({ "x": 0.25, "y": 0.5, "w": 0.5, "h": 0.25 });
    let cases = [
        serde_json::json!({ "kind": "point", "pageId": id(1), "x": -10.0, "y": 5.0 }),
        serde_json::json!({ "kind": "rect", "pageId": id(1), "rect": r }),
        serde_json::json!({ "kind": "element", "pageId": id(1), "elementId": id(2), "fallbackRect": r }),
        serde_json::json!({ "kind": "element", "pageId": id(1), "elementId": id(2) }),
        serde_json::json!({ "kind": "pdf", "pageId": id(1), "pdfPageIndex": 3, "normalizedRect": n, "rotation": 90 }),
        serde_json::json!({
            "kind": "pdf", "pageId": id(1), "pdfPageIndex": 3, "normalizedRect": n, "rotation": 0,
            "pdfAssetSha256": "ab".repeat(32)
        }),
        serde_json::json!({
            "kind": "text", "pageId": id(1), "elementId": id(2),
            "start": b64::encode(&[1, 2, 3]), "end": b64::encode(&[250, 251]),
            "quote": "日本", "quoteHash": quote_hash("日本").iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "fallbackRect": r
        }),
    ];
    for json in cases {
        let a: CommentAnchor = serde_json::from_value(json.clone()).unwrap();
        a.validate().unwrap();
        assert_eq!(serde_json::to_value(&a).unwrap(), json);
    }
    // the old corner-array form and unknown kinds are refused
    assert!(serde_json::from_value::<CommentAnchor>(
        serde_json::json!({ "kind": "rect", "pageId": id(1), "rect": [0.0, 0.0, 1.0, 1.0] })
    )
    .is_err());
    assert!(serde_json::from_value::<CommentAnchor>(
        serde_json::json!({ "kind": "lasso", "pageId": id(1) })
    )
    .is_err());
}

#[test]
fn alternates_are_capped_at_the_highest_operation_ids_in_any_order() {
    let mut a = Writer::new(1, "acct_alice");
    let mut base = CommentReplica::new();
    let t = a.op(&base, create("draft"));
    base.apply(&t).unwrap();
    // the author edits on 30 devices at once: 30 concurrent heads, one per device
    let edits: Vec<CommentOperation> = (2..32u128)
        .map(|n| {
            let mut w = Writer::new(n, "acct_alice");
            w.op(&base, |_| CommentOp::CommentEdited {
                comment_id: t.id,
                body: format!("from device {n}"),
            })
        })
        .collect();
    let mut by_id: Vec<&CommentOperation> = edits.iter().collect();
    by_id.sort_by_key(|o| o.id);
    let body = |o: &CommentOperation| o.op.body().unwrap().to_string();
    let want_shown = body(by_id[by_id.len() - 1]);
    let want_alternates: Vec<String> = by_id
        [by_id.len() - 1 - limits::MAX_ALTERNATES..by_id.len() - 1]
        .iter()
        .map(|o| body(o))
        .collect();
    let mut rng = Rng(99);
    for _ in 0..10 {
        let mut order = edits.clone();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i + 1));
        }
        let mut r = base.clone();
        for o in &order {
            r.apply(o).unwrap();
        }
        let c = &r.thread(t.id).unwrap().comments[0];
        assert_eq!(c.body.as_deref(), Some(want_shown.as_str()));
        assert_eq!(c.alternate_bodies, want_alternates);
    }
}
