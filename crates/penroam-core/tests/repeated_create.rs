//! A notebook or page created twice with the same id (a re-joining device
//! seeding its copy, or one archive imported on two devices) must merge to the
//! same state in any order, without spurious conflicts for identical values.

use penroam_core::operation::{DocumentOperation, LocalOperation, OperationRef};
use penroam_core::replica::Replica;
use penroam_core::*;
use uuid::Uuid;

fn op(id: u128, dev: u128, seq: u64, body: DocumentOperation) -> LocalOperation {
    LocalOperation {
        id: Uuid::from_u128(id),
        device_id: Uuid::from_u128(dev),
        local_sequence: seq,
        schema_version: 1,
        causal_dependencies: if seq > 1 {
            vec![OperationRef {
                device_id: Uuid::from_u128(dev),
                device_seq: seq - 1,
            }]
        } else {
            vec![]
        },
        at: 1000 + id as i64,
        body,
    }
}

fn creates(nb: Uuid, page: Uuid, dev: u128, base: u128, title: &str) -> Vec<LocalOperation> {
    vec![
        op(
            base,
            dev,
            1,
            DocumentOperation::NotebookCreated {
                notebook_id: nb,
                title: title.into(),
                cover: "#3b5b7a".into(),
                defaults: PageDefaults::default(),
            },
        ),
        op(
            base + 1,
            dev,
            2,
            DocumentOperation::PageCreated {
                notebook_id: nb,
                page_id: page,
                order_key: penroam_core::order_key::n_between(None, None, 1).remove(0),
                paper: Paper::Letter,
                size: Paper::Letter.size().unwrap(),
                template: Template::default(),
                background: None,
            },
        ),
    ]
}

#[test]
fn repeated_creates_merge_in_any_order() {
    let nb = Uuid::from_u128(7);
    let page = Uuid::from_u128(8);
    let a = creates(nb, page, 1, 100, "Notes");
    let b = creates(nb, page, 2, 200, "Notes");
    let mut x = Replica::new(nb);
    let mut y = Replica::new(nb);
    for o in a.iter().chain(&b) {
        x.apply_delivered(o).unwrap();
    }
    for o in b.iter().chain(&a) {
        y.apply_delivered(o).unwrap();
    }
    assert_eq!(x.notebook(), y.notebook());
    assert_eq!(x.page(page), y.page(page));
    assert!(
        x.conflicts().is_empty(),
        "identical values are not a conflict"
    );
    assert_eq!(
        x.notebook().unwrap().created_at,
        1200,
        "immutable fields from the highest create id"
    );

    // different values in the two creates are a real, symmetric conflict
    let c = creates(nb, page, 3, 300, "Other title");
    let mut z = Replica::new(nb);
    let mut w = Replica::new(nb);
    for o in a.iter().chain(&c) {
        z.apply_delivered(o).unwrap();
    }
    for o in c.iter().chain(&a) {
        w.apply_delivered(o).unwrap();
    }
    assert_eq!(z.notebook(), w.notebook());
    assert_eq!(z.conflicts(), w.conflicts());
    assert_eq!(z.conflicts().len(), 1);
}

fn text(id: u128, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::from_u128(id),
        z: 1,
        x: 10.0,
        y: 10.0,
        width: 200.0,
        height: 20.0,
        text: s.into(),
        font_size: 16.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        row_pitch: None,
        extra: Extra::new(),
    })
}

/// A re-seeded copy of an element whose text was edited after it was created
/// matches every field group of the merged element: no conflict in any group.
#[test]
fn reseeded_element_with_later_text_edit_is_not_a_conflict() {
    let nb = Uuid::from_u128(7);
    let page = Uuid::from_u128(8);
    let mut ops = creates(nb, page, 1, 100, "Notes");
    ops.push(op(
        102,
        1,
        3,
        DocumentOperation::ElementsAdded {
            notebook_id: nb,
            page_id: page,
            elements: vec![text(50, "first")],
        },
    ));
    ops.push(op(
        103,
        1,
        4,
        DocumentOperation::ElementsReplaced {
            notebook_id: nb,
            page_id: page,
            elements: vec![text(50, "edited")],
            changed: vec![penroam_core::operation::groups::CONTENT],
        },
    ));
    // another device seeds its copy (already showing the edit), concurrently
    let mut seed = creates(nb, page, 2, 200, "Notes");
    seed.push(op(
        202,
        2,
        3,
        DocumentOperation::ElementsAdded {
            notebook_id: nb,
            page_id: page,
            elements: vec![text(50, "edited")],
        },
    ));
    let mut r = Replica::new(nb);
    for o in ops.iter().chain(&seed) {
        r.apply_delivered(o).unwrap();
    }
    assert!(r.conflicts().is_empty(), "{:#?}", r.conflicts());
}
