//! Operation algebra: local replay equivalence with editor page batches, and the
//! merge model's convergence, idempotency and conflict retention.

use penroam_core::operation::*;
use penroam_core::replica::*;
use penroam_core::*;
use std::collections::BTreeMap;
use uuid::Uuid;

const NB: Uuid = Uuid::from_u128(0x10);
const PG: Uuid = Uuid::from_u128(0x20);
const TXT: Uuid = Uuid::from_u128(0x30);

struct Device {
    id: Uuid,
    seq: u64,
    seen: BTreeMap<Uuid, u64>,
    next_op: u128,
}

impl Device {
    fn new(n: u128) -> Self {
        Self {
            id: Uuid::from_u128(n),
            seq: 0,
            seen: BTreeMap::new(),
            next_op: n << 64,
        }
    }
    fn op(&mut self, body: DocumentOperation) -> LocalOperation {
        self.seq += 1;
        self.next_op += 1;
        let mut deps: Vec<OperationRef> = self
            .seen
            .iter()
            .map(|(d, s)| OperationRef {
                device_id: *d,
                device_seq: *s,
            })
            .collect();
        if self.seq > 1 {
            deps.push(OperationRef {
                device_id: self.id,
                device_seq: self.seq - 1,
            });
        }
        LocalOperation {
            id: Uuid::from_u128(self.next_op),
            device_id: self.id,
            local_sequence: self.seq,
            schema_version: op_schema_for(&body),
            causal_dependencies: deps,
            at: 1_000 + self.seq as i64,
            body,
        }
    }
    /// Marks remote operations as seen (they were applied locally).
    fn saw(&mut self, ops: &[LocalOperation]) {
        for o in ops {
            if o.device_id != self.id {
                let e = self.seen.entry(o.device_id).or_insert(0);
                *e = (*e).max(o.local_sequence);
            }
        }
    }
}

fn text(id: Uuid, z: i64, s: &str) -> Element {
    Element::Text(TextBlock {
        id,
        z,
        x: 10.0,
        y: 20.0,
        width: 200.0,
        height: 30.0,
        text: s.into(),
        font_size: 14.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        font: Default::default(),
        row_pitch: None,
        extra: Default::default(),
    })
}

fn stroke(id: u128, z: i64, x: f64) -> Element {
    Element::Ink(InkStroke {
        id: Uuid::from_u128(id),
        z,
        tool: InkTool::Pen,
        color: "#000000".into(),
        opacity: 1.0,
        width: 2.0,
        pressure: true,
        source: "test".into(),
        input: Default::default(),
        points: vec![InkPoint {
            x,
            y: 1.0,
            pressure: 0.5,
            tilt_x: 0.0,
            tilt_y: 0.0,
            time_us: 0,
            twist: None,
            flags: None,
        }],
        extra: Default::default(),
    })
}

fn added(els: Vec<Element>) -> DocumentOperation {
    DocumentOperation::ElementsAdded {
        notebook_id: NB,
        page_id: PG,
        elements: els,
    }
}

fn replaced(e: Element, mask: u8) -> DocumentOperation {
    DocumentOperation::ElementsReplaced {
        notebook_id: NB,
        page_id: PG,
        elements: vec![e],
        changed: vec![mask],
    }
}

/// Notebook + page + one text block, made by `a` and seen by `b`.
fn setup(a: &mut Device, b: &mut Device) -> Vec<LocalOperation> {
    let ops = vec![
        a.op(DocumentOperation::NotebookCreated {
            notebook_id: NB,
            title: "Lab".into(),
            cover: "#3b5b7a".into(),
            defaults: PageDefaults::default(),
        }),
        a.op(DocumentOperation::PageCreated {
            notebook_id: NB,
            page_id: PG,
            order_key: "V".into(),
            paper: Paper::Letter,
            size: Size {
                width: 612.0,
                height: 792.0,
            },
            template: Template::default(),
            background: None,
        }),
        a.op(added(vec![text(TXT, 1, "hello")])),
    ];
    b.saw(&ops);
    ops
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    notebook: serde_json::Value,
    pages: Vec<serde_json::Value>,
    conflicts: serde_json::Value,
}

fn snapshot(r: &Replica) -> Snapshot {
    let nb = r.notebook().unwrap();
    let pages = nb
        .all_page_ids()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|p| serde_json::to_value(r.page(p).unwrap()).unwrap())
        .collect();
    Snapshot {
        notebook: serde_json::to_value(nb).unwrap(),
        pages,
        conflicts: serde_json::to_value(r.conflicts()).unwrap(),
    }
}

/// Every interleaving of two per-device sequences.
fn interleavings(a: &[LocalOperation], b: &[LocalOperation]) -> Vec<Vec<LocalOperation>> {
    if a.is_empty() {
        return vec![b.to_vec()];
    }
    if b.is_empty() {
        return vec![a.to_vec()];
    }
    let mut out = Vec::new();
    for mut rest in interleavings(&a[1..], b) {
        rest.insert(0, a[0].clone());
        out.push(rest);
    }
    for mut rest in interleavings(a, &b[1..]) {
        rest.insert(0, b[0].clone());
        out.push(rest);
    }
    out
}

/// Applies base + every interleaving of the concurrent sequences and checks
/// all replicas converge. Returns the common snapshot's replica.
fn converge(base: &[LocalOperation], a: &[LocalOperation], b: &[LocalOperation]) -> Replica {
    let orders = interleavings(a, b);
    assert!(orders.len() > 1);
    let mut first: Option<(Snapshot, Replica)> = None;
    for order in orders {
        let mut r = Replica::new(NB);
        for op in base.iter().chain(order.iter()) {
            assert_eq!(r.apply(op), Ok(Applied::New), "{}", op.body.name());
        }
        let s = snapshot(&r);
        match &first {
            None => first = Some((s, r)),
            Some((f, _)) => assert_eq!(&s, f, "replicas diverged"),
        }
    }
    first.unwrap().1
}

#[test]
fn independent_strokes_commute() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let ao = vec![
        a.op(added(vec![stroke(100, 2, 1.0)])),
        a.op(added(vec![stroke(101, 3, 2.0)])),
    ];
    let bo = vec![
        b.op(added(vec![stroke(200, 2, 5.0)])),
        b.op(added(vec![stroke(201, 3, 6.0)])),
    ];
    let r = converge(&base, &ao, &bo);
    let page = r.page(PG).unwrap();
    assert_eq!(page.elements.len(), 5);
    assert!(r.conflicts().is_empty());
    // order = (z, id): equal z from two devices is ordered by id everywhere
    let ids: Vec<u128> = page.elements.iter().map(|e| e.id().as_u128()).collect();
    assert_eq!(ids, vec![0x30, 100, 200, 101, 201]);
}

#[test]
fn retries_are_idempotent_and_id_reuse_is_rejected() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let mut r = Replica::new(NB);
    for op in &base {
        r.apply(op).unwrap();
    }
    let s = snapshot(&r);
    for op in &base {
        assert_eq!(r.apply(op), Ok(Applied::Duplicate));
    }
    assert_eq!(snapshot(&r), s);
    let mut forged = base[2].clone();
    forged.body = added(vec![text(TXT, 1, "other")]);
    assert_eq!(r.apply(&forged), Err(MergeError::OpIdReuse(forged.id)));
    let mut same_seq = base[2].clone();
    same_seq.id = Uuid::from_u128(0xdead);
    assert!(matches!(
        r.apply(&same_seq),
        Err(MergeError::SequenceReuse { .. })
    ));
    assert_eq!(snapshot(&r), s, "rejections change nothing");
}

#[test]
fn concurrent_text_edits_keep_a_reproducible_conflict() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let ao = vec![a.op(replaced(text(TXT, 1, "hello from A"), groups::CONTENT))];
    let bo = vec![b.op(replaced(text(TXT, 1, "hello from B"), groups::CONTENT))];
    let r = converge(&base, &ao, &bo);
    let c = r.conflicts();
    assert_eq!(c.len(), 1);
    let winner_is_b = bo[0].id > ao[0].id;
    let (win, lose) = if winner_is_b { ("B", "A") } else { ("A", "B") };
    let Element::Text(t) = &r.page(PG).unwrap().elements[0] else {
        panic!()
    };
    assert_eq!(t.text, format!("hello from {win}"));
    assert_eq!(
        c[0].alternates[0].value["text"],
        serde_json::json!(format!("hello from {lose}")),
        "the other paragraph is retained, not overwritten"
    );
    // a later edit that saw both resolves the conflict
    let mut a2 = a;
    a2.saw(&bo);
    let fix = a2.op(replaced(text(TXT, 1, "merged"), groups::CONTENT));
    let mut r = r;
    r.apply(&fix).unwrap();
    assert!(r.conflicts().is_empty());
}

#[test]
fn concurrent_move_and_edit_of_one_block_both_survive() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let mut moved = text(TXT, 1, "hello");
    if let Element::Text(t) = &mut moved {
        t.x = 300.0;
    }
    let ao = vec![a.op(replaced(moved, groups::PLACEMENT))];
    let bo = vec![b.op(replaced(text(TXT, 1, "hello, world"), groups::CONTENT))];
    let r = converge(&base, &ao, &bo);
    let Element::Text(t) = &r.page(PG).unwrap().elements[0] else {
        panic!()
    };
    assert_eq!((t.x, t.text.as_str()), (300.0, "hello, world"));
    assert!(r.conflicts().is_empty());
}

#[test]
fn deleted_element_restore_and_delete_vs_edit() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    // A deletes, then undoes (re-adds the same id = explicit restore)
    let del = a.op(DocumentOperation::ElementsRemoved {
        notebook_id: NB,
        page_id: PG,
        ids: vec![TXT],
    });
    let mut r = Replica::new(NB);
    for op in base.iter().chain([&del]) {
        r.apply(op).unwrap();
    }
    assert!(r.page(PG).unwrap().elements.is_empty());
    let undo = a.op(added(vec![text(TXT, 1, "hello")]));
    r.apply(&undo).unwrap();
    assert_eq!(r.page(PG).unwrap().elements.len(), 1);
    assert!(r.conflicts().is_empty());

    // concurrent: A deletes, B edits without having seen the delete
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let ao = vec![a.op(DocumentOperation::ElementsRemoved {
        notebook_id: NB,
        page_id: PG,
        ids: vec![TXT],
    })];
    let bo = vec![b.op(replaced(text(TXT, 1, "B's late edit"), groups::CONTENT))];
    let r = converge(&base, &ao, &bo);
    assert!(r.page(PG).unwrap().elements.is_empty(), "delete wins");
    let c = r.conflicts();
    assert_eq!(c.len(), 1, "{c:?}");
    assert!(matches!(
        c[0].target,
        ConflictTarget::EditedWhileDeleted { .. }
    ));
    assert_eq!(c[0].alternates[0].value["text"], "B's late edit");

    // concurrent: A deletes, B restores (undo of an older delete) → add wins
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let mut base = setup(&mut a, &mut b);
    let del0 = a.op(DocumentOperation::ElementsRemoved {
        notebook_id: NB,
        page_id: PG,
        ids: vec![TXT],
    });
    b.saw(std::slice::from_ref(&del0));
    base.push(del0);
    let ao = vec![a.op(added(vec![text(TXT, 1, "hello")]))]; // A undoes
    let bo = vec![b.op(added(vec![text(TXT, 1, "hello")]))]; // B undoes too
    let r = converge(&base, &ao, &bo);
    assert_eq!(r.page(PG).unwrap().elements.len(), 1);
}

#[test]
fn concurrent_renames_keep_both_variants() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let ao = vec![a.op(DocumentOperation::NotebookRenamed {
        notebook_id: NB,
        title: "Lab A".into(),
    })];
    let bo = vec![b.op(DocumentOperation::NotebookRenamed {
        notebook_id: NB,
        title: "Lab B".into(),
    })];
    let r = converge(&base, &ao, &bo);
    let c = r.conflicts();
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].target, ConflictTarget::NotebookTitle);
    let shown = r.notebook().unwrap().title;
    let alt = c[0].alternates[0].value.as_str().unwrap().to_string();
    let mut both = vec![shown, alt];
    both.sort();
    assert_eq!(both, vec!["Lab A", "Lab B"]);
}

#[test]
fn page_inserts_and_moves_converge_by_key_then_id() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let p = |n: u128| Uuid::from_u128(0x2000 + n);
    let create = |d: &mut Device, n: u128, key: &str| {
        d.op(DocumentOperation::PageCreated {
            notebook_id: NB,
            page_id: p(n),
            order_key: key.into(),
            paper: Paper::A4,
            size: Size {
                width: 595.0,
                height: 842.0,
            },
            template: Template::default(),
            background: None,
        })
    };
    // both insert "after the first page" with the same key; A also moves PG last
    let ao = vec![
        create(&mut a, 1, "k"),
        a.op(DocumentOperation::PageReordered {
            notebook_id: NB,
            page_id: PG,
            order_key: "z".into(),
        }),
    ];
    let bo = vec![create(&mut b, 2, "k")];
    let r = converge(&base, &ao, &bo);
    let nb = r.notebook().unwrap();
    assert_eq!(nb.page_order, vec![p(1), p(2), PG]);
    assert!(nb.order_is_consistent());
}

#[test]
fn unknown_schema_and_missing_dependencies_are_not_applied() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let mut r = Replica::new(NB);
    for op in &base {
        r.apply(op).unwrap();
    }
    let s = snapshot(&r);
    let mut future = a.op(DocumentOperation::NotebookRenamed {
        notebook_id: NB,
        title: "x".into(),
    });
    future.schema_version = OP_SCHEMA_VERSION + 1;
    assert_eq!(
        r.apply(&future),
        Err(MergeError::Op(OpError::UnsupportedSchema {
            found: OP_SCHEMA_VERSION + 1
        }))
    );
    // b's second op before its first
    let b1 = b.op(added(vec![stroke(7, 9, 1.0)]));
    let b2 = b.op(added(vec![stroke(8, 9, 1.0)]));
    assert!(matches!(
        r.apply(&b2),
        Err(MergeError::MissingDependency(_))
    ));
    // an op depending on a device we have never heard of
    let mut c = Device::new(3);
    c.seen.insert(Uuid::from_u128(77), 4);
    let c1 = c.op(added(vec![stroke(9, 9, 1.0)]));
    assert!(matches!(
        r.apply(&c1),
        Err(MergeError::MissingDependency(_))
    ));
    assert_eq!(snapshot(&r), s);
    r.apply(&b1).unwrap();
    r.apply(&b2).unwrap();
}

#[test]
fn a_purge_merges_and_a_restore_undoes_it() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let mut r = Replica::new(NB);
    for op in &base {
        r.apply(op).unwrap();
    }
    let purge = a.op(DocumentOperation::NotebookPurged { notebook_id: NB });
    assert_eq!(purge.schema_version, 2, "older clients must park it");
    r.apply(&purge).unwrap();
    assert!(r.is_purged());
    assert!(r.notebook().unwrap().deleted_at.is_some());

    let restore = a.op(DocumentOperation::NotebookRestored { notebook_id: NB });
    assert_eq!(restore.schema_version, 1);
    r.apply(&restore).unwrap();
    assert!(!r.is_purged());
    assert!(r.notebook().unwrap().deleted_at.is_none());
}

#[test]
fn a_move_to_the_trash_is_not_a_purge() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let mut r = Replica::new(NB);
    for op in &base {
        r.apply(op).unwrap();
    }
    let trash = a.op(DocumentOperation::NotebookDeleted { notebook_id: NB });
    r.apply(&trash).unwrap();
    assert!(!r.is_purged());
}

#[test]
fn a_restore_concurrent_with_a_purge_keeps_the_notebook() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let ao = vec![a.op(DocumentOperation::NotebookPurged { notebook_id: NB })];
    let bo = vec![b.op(DocumentOperation::NotebookRestored { notebook_id: NB })];
    let r = converge(&base, &ao, &bo);
    assert!(!r.is_purged());
    assert!(r.notebook().unwrap().deleted_at.is_none());
}

#[test]
fn unknown_operation_type_does_not_deserialize() {
    let v = serde_json::json!({"type": "inkStrokeTeleported", "notebookId": NB});
    assert!(serde_json::from_value::<DocumentOperation>(v).is_err());
}

// ------------------------------------------------------- local replay --

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random editor batches applied with `apply_batch` and, translated to
/// operations, with `apply_to_page` give identical pages (incl. equal-z
/// ordering), so journal replay reproduces exactly what the editor saw.
#[test]
fn local_replay_matches_page_batches() {
    let mut rng = Rng(7);
    let mut via_batch = Page::new(
        PG,
        Paper::Letter,
        Size {
            width: 612.0,
            height: 792.0,
        },
        Template::default(),
        0,
    );
    let mut via_ops = via_batch.clone();
    let mut next_id = 1u128;
    for _ in 0..400 {
        let mut ops = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let ids: Vec<Uuid> = via_batch.elements.iter().map(Element::id).collect();
            let kind = match rng.below(4) {
                0 | 1 => {
                    let n = 1 + rng.below(3);
                    OpKind::AddElements {
                        elements: (0..n)
                            .map(|_| {
                                next_id += 1;
                                let z = rng.below(5) as i64;
                                if rng.below(2) == 0 {
                                    stroke(next_id, z, rng.below(100) as f64)
                                } else {
                                    text(Uuid::from_u128(next_id), z, "t")
                                }
                            })
                            .collect(),
                    }
                }
                2 if !ids.is_empty() => OpKind::RemoveElements {
                    ids: vec![ids[rng.below(ids.len() as u64) as usize]],
                },
                3 if !ids.is_empty() => {
                    let id = ids[rng.below(ids.len() as u64) as usize];
                    let mut e = via_batch
                        .elements
                        .iter()
                        .find(|e| e.id() == id)
                        .unwrap()
                        .clone();
                    match &mut e {
                        Element::Ink(s) => s.z = rng.below(6) as i64,
                        Element::Text(t) => {
                            t.z = rng.below(6) as i64;
                            t.text.push('x');
                        }
                        Element::Image(_) => {}
                    }
                    OpKind::ReplaceElements { elements: vec![e] }
                }
                _ => OpKind::SetTemplate {
                    template: Template {
                        kind: TemplateKind::Grid,
                        spacing: 10.0 + rng.below(20) as f64,
                    },
                },
            };
            ops.push(PageOperation {
                op_id: Uuid::new_v4(),
                kind,
            });
        }
        let batch = OperationBatch {
            batch_id: Uuid::new_v4(),
            base_revision: via_batch.revision,
            ops,
        };
        let bodies = bodies_from_batch(NB, &via_ops, &batch);
        let refs: Vec<&DocumentOperation> = bodies.iter().map(|(_, b)| b).collect();
        let r1 = apply_batch(&mut via_batch, &batch, 5);
        let r2 = apply_to_page(&mut via_ops, &refs);
        assert_eq!(r1.is_ok(), r2.is_ok(), "{r1:?} {r2:?}");
        if r1.is_ok() {
            via_ops.revision += 1;
            via_ops.updated_at = 5;
        }
        assert_eq!(via_batch, via_ops);
    }
}

#[test]
fn changed_groups_detects_each_group() {
    let a = text(TXT, 1, "x");
    let mut b = a.clone();
    assert_eq!(changed_groups(&a, &b), 0);
    if let Element::Text(t) = &mut b {
        t.x += 1.0;
        t.color = "#ff0000".into();
    }
    assert_eq!(changed_groups(&a, &b), groups::PLACEMENT | groups::STYLE);
    assert_eq!(changed_groups(&a, &stroke(1, 1, 1.0)), groups::ALL);
}

/// Re-applying any suffix of a notebook journal (crash between journal and
/// notebook.json write) converges to the same notebook.
#[test]
fn notebook_ops_are_idempotent_under_suffix_replay() {
    let mut a = Device::new(1);
    let create = a.op(DocumentOperation::NotebookCreated {
        notebook_id: NB,
        title: "T".into(),
        cover: "#3B5B7A".into(),
        defaults: PageDefaults::default(),
    });
    let pages: Vec<Uuid> = (0..4).map(|i| Uuid::from_u128(0x500 + i)).collect();
    let mut ops = Vec::new();
    for (i, p) in pages.iter().enumerate() {
        ops.push(a.op(DocumentOperation::PageCreated {
            notebook_id: NB,
            page_id: *p,
            order_key: order_key::spread(4)[i].clone(),
            paper: Paper::Letter,
            size: Size {
                width: 612.0,
                height: 792.0,
            },
            template: Template::default(),
            background: None,
        }));
    }
    ops.push(a.op(DocumentOperation::PageReordered {
        notebook_id: NB,
        page_id: pages[0],
        order_key: "zz".into(),
    }));
    ops.push(a.op(DocumentOperation::PageDeleted {
        notebook_id: NB,
        page_id: pages[1],
    }));
    ops.push(a.op(DocumentOperation::NotebookRenamed {
        notebook_id: NB,
        title: "Renamed".into(),
    }));
    ops.push(a.op(DocumentOperation::PageRestored {
        notebook_id: NB,
        page_id: pages[1],
    }));
    ops.push(a.op(DocumentOperation::PageDeleted {
        notebook_id: NB,
        page_id: pages[2],
    }));
    ops.push(a.op(DocumentOperation::NotebookDeleted { notebook_id: NB }));

    let run = |from: usize, upto: usize| {
        let mut nb = notebook_from_create(&create).unwrap();
        for op in &ops[..upto] {
            apply_to_notebook(&mut nb, op).unwrap();
        }
        for op in &ops[from..] {
            apply_to_notebook(&mut nb, op).unwrap();
        }
        nb
    };
    let clean = run(ops.len(), ops.len());
    assert_eq!(clean.title, "Renamed");
    assert_eq!(clean.cover, "#3b5b7a");
    assert_eq!(clean.page_order, vec![pages[1], pages[3], pages[0]]);
    assert!(clean.deleted_at.is_some());
    for upto in 0..=ops.len() {
        for from in 0..=upto {
            let mut got = run(from, upto);
            let mut want = clean.clone();
            // former_index is advisory; everything else must match
            for t in got
                .trashed_pages
                .iter_mut()
                .chain(want.trashed_pages.iter_mut())
            {
                t.former_index = 0;
            }
            assert_eq!(got, want, "replay of [{from}..] over state at {upto}");
        }
    }
}

#[test]
fn infinite_canvas_switch_merges_without_a_conflict() {
    let (mut a, mut b) = (Device::new(1), Device::new(2));
    let base = setup(&mut a, &mut b);
    let on = |d: &mut Device, infinite| {
        d.op(DocumentOperation::NotebookCanvasSet {
            notebook_id: NB,
            infinite,
        })
    };
    let ao = vec![on(&mut a, true)];
    assert_eq!(ao[0].schema_version, 4);
    let bo = vec![on(&mut b, false)];
    let r = converge(&base, &ao, &bo);
    assert!(r.conflicts().is_empty());
    // the highest operation id wins: device 2's
    assert!(!r.notebook().unwrap().infinite_canvas);

    // a later switch, after seeing both, decides it
    a.saw(&bo);
    let later = on(&mut a, true);
    let mut r = r;
    assert_eq!(r.apply(&later), Ok(Applied::New));
    assert!(r.notebook().unwrap().infinite_canvas);

    // the local path agrees and is idempotent
    let mut nb = Notebook::new(
        NB,
        "Lab".into(),
        PageDefaults::default(),
        "#3b5b7a".into(),
        0,
    );
    assert_eq!(
        apply_to_notebook(&mut nb, &later),
        Ok(NotebookEffect::Changed)
    );
    assert!(nb.infinite_canvas);
    assert_eq!(
        apply_to_notebook(&mut nb, &later),
        Ok(NotebookEffect::Unchanged)
    );
}

#[test]
fn pdf_page_elements_must_name_their_own_asset() {
    let pdf = PdfBackgroundRef {
        asset_hash: "a".repeat(64),
        page_index: 2,
        media_box: [0.0, 0.0, 612.0, 792.0],
        crop_box: [0.0, 0.0, 612.0, 792.0],
        rotation: 0,
    };
    let mut img = ImageElement {
        id: Uuid::from_u128(0x40),
        z: -1,
        x: -20.0,
        y: 900.0,
        width: 612.0,
        height: 792.0,
        asset_hash: "a".repeat(64),
        pdf_page: Some(pdf),
        extra: Default::default(),
    };
    assert!(validate::validate_element(&Element::Image(img.clone())).is_ok());
    // survives a JSON round trip as `pdfPage`
    let json = serde_json::to_value(Element::Image(img.clone())).unwrap();
    assert_eq!(json["pdfPage"]["pageIndex"], 2);
    img.asset_hash = "b".repeat(64);
    assert!(validate::validate_element(&Element::Image(img)).is_err());
}
