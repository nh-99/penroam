//! A recovered copy made by the non-destructive restore planner keeps its provenance
//! (`extra["recoveredFrom"]` on the text block) through every encoding a page goes through: the
//! operation journal and a reopened library (`.prp` page files), the binary page codec, `.penroam`
//! archives, and the sync payload (a one-operation journal record) merged by a replica, also after
//! someone else moves the copy.

use std::io::Cursor;

use penroam_core::model::*;
use penroam_core::operation::{
    bodies_from_batch, groups, op_schema_for, DocumentOperation, LocalOperation, OperationRef,
};
use penroam_core::ops::{OpKind, OperationBatch, PageOperation};
use penroam_core::replica::Replica;
use penroam_storage::journal::{decode_txn, encode_txn, Txn, TxnTarget};
use penroam_storage::restore::{
    ChangedChoice, RestorePlan, RestoreSelection, WritersSinceBase, RECOVERED_FROM_KEY,
};
use penroam_storage::Library;
use tempfile::TempDir;
use uuid::Uuid;

fn text(body: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::new_v4(),
        z: 1,
        x: 10.0,
        y: 20.0,
        width: 200.0,
        height: 30.0,
        text: body.into(),
        font_size: 14.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        font: Default::default(),
        row_pitch: None,
        extra: Default::default(),
    })
}

fn rewritten(e: &Element, body: &str) -> Element {
    let mut e = e.clone();
    if let Element::Text(t) = &mut e {
        t.text = body.into();
    }
    e
}

fn batch(rev: u64, kind: OpKind) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: rev,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind,
        }],
    }
}

/// The recovered copy on `page`, with its `recoveredFrom` value.
fn copy_of(page: &Page, original: Uuid) -> serde_json::Value {
    let copies: Vec<&TextBlock> = page
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Text(t) if t.id != original => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(copies.len(), 1, "exactly one recovered copy");
    assert_eq!(copies[0].text, "original");
    copies[0]
        .extra
        .get(RECOVERED_FROM_KEY)
        .cloned()
        .unwrap_or_else(|| panic!("provenance lost: {:?}", copies[0].extra))
}

#[test]
fn recovered_copy_provenance_survives_every_encoding() {
    let d = TempDir::new().unwrap();
    let root = d.path().join("lib");
    let mut lib = Library::open(&root).unwrap();
    let (nb, page) = lib
        .create_notebook("Provenance", PageDefaults::default(), None)
        .unwrap();
    let block = text("original");
    lib.apply_page_operations(
        nb.id,
        page.id,
        &batch(
            0,
            OpKind::AddElements {
                elements: vec![block.clone()],
            },
        ),
    )
    .unwrap();
    let base = lib.load_page(nb.id, page.id).unwrap();
    lib.apply_page_operations(
        nb.id,
        page.id,
        &batch(
            base.revision,
            OpKind::ReplaceElements {
                elements: vec![rewritten(&block, "bob rewrote it")],
            },
        ),
    )
    .unwrap();
    let head = lib.load_page(nb.id, page.id).unwrap();
    let writers: WritersSinceBase = [(block.id(), ["acct-bob".to_string()].into())]
        .into_iter()
        .collect();
    let plan = RestorePlan::new(&base, &head, &writers, "acct-me").unwrap();
    let mut sel = RestoreSelection::default();
    sel.changed.insert(block.id(), ChangedChoice::RecoveredCopy);
    let restore = plan.to_batch(&sel).unwrap().unwrap();
    lib.apply_page_operations(nb.id, page.id, &restore).unwrap();
    let expected = serde_json::json!({"elementId": block.id(), "revision": base.revision});

    // In memory, and after reopening (journal replay + page file).
    assert_eq!(
        copy_of(&lib.load_page(nb.id, page.id).unwrap(), block.id()),
        expected
    );
    drop(lib);
    let mut lib = Library::open(&root).unwrap();
    let after = lib.load_page(nb.id, page.id).unwrap();
    assert_eq!(copy_of(&after, block.id()), expected);

    // The binary page codec.
    let decoded =
        penroam_core::binary::decode_page(&penroam_core::binary::encode_page(&after)).unwrap();
    assert_eq!(copy_of(&decoded, block.id()), expected);

    // A .penroam archive, imported elsewhere.
    let mut out = Cursor::new(Vec::new());
    lib.export_archive(nb.id, &mut out, "test").unwrap();
    let d2 = TempDir::new().unwrap();
    let mut other = Library::open(d2.path().join("lib")).unwrap();
    let report = other.import_archive(Cursor::new(out.into_inner())).unwrap();
    let imported = other
        .load_page(report.notebook.id, report.notebook.page_order[0])
        .unwrap();
    assert_eq!(copy_of(&imported, block.id()), expected);

    // The sync payload: each operation is one journal record; a replica merges them, and a later
    // move of the copy by another device keeps the provenance (it's in the content group).
    let dev = Uuid::from_u128(7);
    let seq = std::cell::Cell::new(0u64);
    let op = |body: DocumentOperation, deps: Vec<OperationRef>| {
        seq.set(seq.get() + 1);
        LocalOperation {
            id: Uuid::new_v4(),
            device_id: dev,
            local_sequence: seq.get(),
            schema_version: op_schema_for(&body),
            causal_dependencies: deps,
            at: 1_000 + seq.get() as i64,
            body,
        }
    };
    let prev = |n: u64| {
        vec![OperationRef {
            device_id: dev,
            device_seq: n,
        }]
    };
    let mut ops = vec![
        op(
            DocumentOperation::NotebookCreated {
                notebook_id: nb.id,
                title: "Provenance".into(),
                cover: "#336699".into(),
                defaults: PageDefaults::default(),
            },
            vec![],
        ),
        op(
            DocumentOperation::PageCreated {
                notebook_id: nb.id,
                page_id: page.id,
                order_key: "a".into(),
                paper: Paper::Letter,
                size: page.size,
                template: page.template.clone(),
                background: None,
            },
            prev(1),
        ),
        op(
            DocumentOperation::ElementsAdded {
                notebook_id: nb.id,
                page_id: page.id,
                elements: head.elements.clone(),
            },
            prev(2),
        ),
    ];
    for (_, body) in bodies_from_batch(nb.id, &head, &restore) {
        ops.push(op(body, prev(seq.get())));
    }
    let mut replica = Replica::new(nb.id);
    for o in &ops {
        let record = encode_txn(&Txn {
            target: TxnTarget::Notebook { notebook_id: nb.id },
            ops: vec![o.clone()],
        });
        let back = decode_txn(&record).unwrap();
        assert_eq!(&back.ops[0], o, "the payload round-trips exactly");
        replica.apply(&back.ops[0]).unwrap();
    }
    let merged = replica.page(page.id).unwrap();
    assert_eq!(copy_of(&merged, block.id()), expected);
    let mut moved = merged
        .elements
        .iter()
        .find(|e| e.id() != block.id())
        .unwrap()
        .clone();
    moved.translate(40.0, 0.0);
    let other_dev = Uuid::from_u128(8);
    let n = seq.get();
    let mv = LocalOperation {
        id: Uuid::new_v4(),
        device_id: other_dev,
        local_sequence: 1,
        schema_version: 1,
        causal_dependencies: prev(n),
        at: 9_000,
        body: DocumentOperation::ElementsReplaced {
            notebook_id: nb.id,
            page_id: page.id,
            elements: vec![moved],
            changed: vec![groups::PLACEMENT],
        },
    };
    replica.apply(&mv).unwrap();
    assert_eq!(
        copy_of(&replica.page(page.id).unwrap(), block.id()),
        expected
    );
}
