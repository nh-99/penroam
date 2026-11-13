//! Concurrent text editing in shared notebooks: golden vectors for the pinned CRDT encoding, and
//! the merge model's handling of `textEdited` (convergence, legacy whole-content writes, delete
//! versus typing).

use penroam_core::operation::*;
use penroam_core::replica::*;
use penroam_core::textcrdt::{b64, edit_client, TextDoc};
use penroam_core::*;
use std::collections::BTreeMap;
use uuid::Uuid;

const NB: Uuid = Uuid::from_u128(0x10);
const PG: Uuid = Uuid::from_u128(0x20);
const TXT: Uuid = Uuid::from_u128(0x30);
const GENESIS_OP: Uuid = Uuid::from_u128(0x4000_0000_0000_0000_0000_0000_0000_0001);
const SAMPLE: &str =
    "# Lab 🧪\n- [x] $\\lambda$\n| a | b |\n|---|---|\n| 1 | =A2*2 |\n日本 שלום 👩🏽‍🔬";

fn vectors_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/text-crdt.json")
}

/// The fixture as this build produces it.
fn produce() -> serde_json::Value {
    let genesis = TextDoc::genesis_update(GENESIS_OP, SAMPLE);
    let a = TextDoc::genesis(GENESIS_OP, SAMPLE);
    let b = TextDoc::genesis(GENESIS_OP, SAMPLE);
    let ua = a
        .edit_update(
            edit_client(Uuid::from_u128(1)),
            &SAMPLE.replace("Lab", "Lab A"),
        )
        .unwrap();
    let ub = b
        .edit_update(
            edit_client(Uuid::from_u128(2)),
            &SAMPLE.replace("=A2*2", "=A2*3"),
        )
        .unwrap();
    a.apply(&ub).unwrap();
    a.apply(&ua).unwrap();
    serde_json::json!({
        "format": "penroam-text-crdt-vectors",
        "yrs": "0.28.1",
        "encoding": "yrs update v1, base64url",
        "genesis": { "op": GENESIS_OP, "text": SAMPLE, "update": b64::encode(&genesis) },
        "edits": [
            { "device": Uuid::from_u128(1), "update": b64::encode(&ua) },
            { "device": Uuid::from_u128(2), "update": b64::encode(&ub) },
        ],
        "expected": a.text(),
    })
}

/// Writes the fixture (run once with `--ignored` after a deliberate encoding change).
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
    // byte-identical genesis and edits: the encoding hasn't changed
    assert_eq!(v, produce(), "the pinned text CRDT encoding changed");
    // and every delivery order, with duplicates, gives the expected text
    let g = b64::decode(v["genesis"]["update"].as_str().unwrap()).unwrap();
    let e: Vec<Vec<u8>> = v["edits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| b64::decode(x["update"].as_str().unwrap()).unwrap())
        .collect();
    for order in [[0usize, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0]] {
        let d = TextDoc::genesis(Uuid::nil(), "");
        let all = [&g, &e[0], &e[1]];
        for i in order {
            d.apply(all[i]).unwrap();
            d.apply(all[i]).unwrap();
        }
        // a doc started empty under another client: only the genesis' items count
        assert!(d.text().ends_with(v["expected"].as_str().unwrap()));
    }
}

struct Dev {
    id: Uuid,
    seq: u64,
    seen: BTreeMap<Uuid, u64>,
    n: u128,
}

impl Dev {
    fn new(n: u128) -> Self {
        Self {
            id: Uuid::from_u128(n),
            seq: 0,
            seen: BTreeMap::new(),
            n: n << 64,
        }
    }
    fn op(&mut self, body: DocumentOperation) -> LocalOperation {
        self.seq += 1;
        self.n += 1;
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
            id: Uuid::from_u128(self.n),
            device_id: self.id,
            local_sequence: self.seq,
            schema_version: op_schema_for(&body),
            causal_dependencies: deps,
            at: self.seq as i64,
            body,
        }
    }
    fn saw(&mut self, ops: &[LocalOperation]) {
        for o in ops {
            let e = self.seen.entry(o.device_id).or_insert(0);
            *e = (*e).max(o.local_sequence);
        }
    }
    /// What the sync client writes for a local text change, against `r`'s state.
    fn type_text(&mut self, r: &Replica, text: &str) -> LocalOperation {
        let (base, update) = r
            .text_edit(PG, TXT, edit_client(self.id), text, &|_, _| true)
            .expect("a change");
        self.op(DocumentOperation::TextEdited {
            notebook_id: NB,
            page_id: PG,
            element_id: TXT,
            base,
            update,
        })
    }
}

fn text_block(s: &str) -> Element {
    Element::Text(TextBlock {
        id: TXT,
        z: 1,
        x: 0.0,
        y: 0.0,
        width: 300.0,
        height: 40.0,
        text: s.into(),
        font_size: 16.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        font: Default::default(),
        row_pitch: None,
        extra: Default::default(),
    })
}

fn setup(a: &mut Dev) -> Vec<LocalOperation> {
    vec![
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
            template: Template {
                kind: TemplateKind::Blank,
                spacing: 24.0,
            },
            background: None,
        }),
        a.op(DocumentOperation::ElementsAdded {
            notebook_id: NB,
            page_id: PG,
            elements: vec![text_block("hello world")],
        }),
    ]
}

fn replica(ops: &[&LocalOperation]) -> Replica {
    let mut r = Replica::new(NB);
    for o in ops {
        assert_eq!(
            r.apply_delivered(o).unwrap(),
            Applied::New,
            "{}",
            o.body.name()
        );
    }
    r
}

fn shown(r: &Replica) -> String {
    match r
        .page(PG)
        .unwrap()
        .elements
        .into_iter()
        .find(|e| e.id() == TXT)
        .unwrap()
    {
        Element::Text(t) => t.text,
        _ => unreachable!(),
    }
}

#[test]
fn concurrent_typing_in_one_block_keeps_everyones_text_in_any_order() {
    let (mut a, mut b) = (Dev::new(1), Dev::new(2));
    let base = setup(&mut a);
    b.saw(&base);
    a.saw(&base);
    let start = replica(&base.iter().collect::<Vec<_>>());
    // both type at the same spot, twice each, without seeing each other
    let a1 = a.type_text(&start, "hello A world");
    let ra = replica(&base.iter().chain([&a1]).collect::<Vec<_>>());
    let a2 = a.type_text(&ra, "hello AA world");
    let b1 = b.type_text(&start, "hello B world");
    let rb = replica(&base.iter().chain([&b1]).collect::<Vec<_>>());
    let b2 = b.type_text(&rb, "hello BB world");
    assert_eq!(a1.schema_version, 6);
    let orders: Vec<Vec<&LocalOperation>> = vec![
        vec![&a1, &a2, &b1, &b2],
        vec![&b1, &b2, &a1, &a2],
        vec![&a1, &b1, &a2, &b2],
        vec![&b1, &a1, &b2, &a2],
    ];
    let mut results = Vec::new();
    for o in orders {
        let mut r = replica(&base.iter().collect::<Vec<_>>());
        for op in &o {
            r.apply_delivered(op).unwrap();
            // a duplicate is a no-op
            assert_eq!(r.apply_delivered(op).unwrap(), Applied::Duplicate);
        }
        results.push(shown(&r));
        assert!(r.conflicts().is_empty(), "{:?}", r.conflicts());
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]), "{results:?}");
    let t = &results[0];
    assert!(
        t.contains("AA") && t.contains("BB") && t.starts_with("hello ") && t.ends_with(" world"),
        "{t}"
    );
}

#[test]
fn an_older_clients_whole_text_write_keeps_the_typed_text_as_an_alternate() {
    let (mut a, mut b) = (Dev::new(1), Dev::new(2));
    let base = setup(&mut a);
    a.saw(&base);
    b.saw(&base);
    let start = replica(&base.iter().collect::<Vec<_>>());
    let typed = a.type_text(&start, "hello typed world");
    // b runs an older build: it replaces the whole text
    let old = b.op(DocumentOperation::ElementsReplaced {
        notebook_id: NB,
        page_id: PG,
        elements: vec![text_block("rewritten")],
        changed: vec![groups::CONTENT],
    });
    for o in [vec![&typed, &old], vec![&old, &typed]] {
        let mut r = replica(&base.iter().collect::<Vec<_>>());
        for op in o {
            r.apply_delivered(op).unwrap();
        }
        let c = r.conflicts();
        assert_eq!(c.len(), 1, "{c:?}");
        let mut values: Vec<String> = std::iter::once(&c[0].winner_value)
            .chain(c[0].alternates.iter().map(|x| &x.value))
            .map(|v| v["text"].as_str().unwrap().to_string())
            .collect();
        values.sort();
        values.dedup();
        assert!(
            values.contains(&"hello typed world".to_string()),
            "{values:?}"
        );
        assert!(values.contains(&"rewritten".to_string()), "{values:?}");
    }
}

#[test]
fn typing_while_someone_deletes_the_block_stays_recoverable() {
    let (mut a, mut b) = (Dev::new(1), Dev::new(2));
    let base = setup(&mut a);
    a.saw(&base);
    b.saw(&base);
    let start = replica(&base.iter().collect::<Vec<_>>());
    let typed = a.type_text(&start, "hello typed world");
    let del = b.op(DocumentOperation::ElementsRemoved {
        notebook_id: NB,
        page_id: PG,
        ids: vec![TXT],
    });
    for o in [vec![&typed, &del], vec![&del, &typed]] {
        let mut r = replica(&base.iter().collect::<Vec<_>>());
        for op in o {
            r.apply_delivered(op).unwrap();
        }
        assert!(r.page(PG).unwrap().elements.iter().all(|e| e.id() != TXT));
        let c: Vec<_> = r
            .conflicts()
            .into_iter()
            .filter(|c| matches!(c.target, ConflictTarget::EditedWhileDeleted { .. }))
            .collect();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].alternates[0].value["text"], "hello typed world");
    }
}

#[test]
fn an_edit_made_against_the_installed_text_keeps_text_that_arrived_since() {
    let (mut a, mut b) = (Dev::new(1), Dev::new(2));
    let base = setup(&mut a);
    a.saw(&base);
    b.saw(&base);
    let start = replica(&base.iter().collect::<Vec<_>>());
    let remote = b.type_text(&start, "hello remote world");
    // a's replica has b's edit, but a typed on the text it had installed ("hello world")
    let r = replica(&base.iter().chain([&remote]).collect::<Vec<_>>());
    let (base_id, update) = r
        .text_edit(PG, TXT, edit_client(a.id), "hello world!", &|d, _| {
            d != b.id
        })
        .unwrap();
    let mine = a.op(DocumentOperation::TextEdited {
        notebook_id: NB,
        page_id: PG,
        element_id: TXT,
        base: base_id,
        update,
    });
    let mut r = r;
    r.apply_delivered(&mine).unwrap();
    assert_eq!(shown(&r), "hello remote world!");
}

#[test]
fn a_text_update_for_a_missing_block_or_garbage_is_refused() {
    let mut a = Dev::new(1);
    let base = setup(&mut a);
    let mut r = replica(&base.iter().collect::<Vec<_>>());
    let bad = a.op(DocumentOperation::TextEdited {
        notebook_id: NB,
        page_id: PG,
        element_id: TXT,
        base: Uuid::from_u128(999),
        update: b64::encode(b"x"),
    });
    assert!(r.apply_delivered(&bad).is_err());
    assert_eq!(shown(&r), "hello world");
}

/// Two people rewrite the same block concurrently; returns the merged text (same in both orders).
fn concurrent_rewrites(start: &str, a_text: &str, b_text: &str) -> String {
    let (mut a, mut b) = (Dev::new(1), Dev::new(2));
    let mut base = setup(&mut a);
    base.pop();
    base.push(a.op(DocumentOperation::ElementsAdded {
        notebook_id: NB,
        page_id: PG,
        elements: vec![text_block(start)],
    }));
    a.saw(&base);
    b.saw(&base);
    let r0 = replica(&base.iter().collect::<Vec<_>>());
    assert_eq!(shown(&r0), start);
    let ua = a.type_text(&r0, a_text);
    let ub = b.type_text(&r0, b_text);
    let mut out = Vec::new();
    for o in [[&ua, &ub], [&ub, &ua]] {
        let mut r = replica(&base.iter().collect::<Vec<_>>());
        for op in o {
            r.apply_delivered(op).unwrap();
        }
        out.push(shown(&r));
    }
    assert_eq!(out[0], out[1]);
    out.remove(0)
}

#[test]
fn two_whole_rewrites_both_survive_intact() {
    // the case found by the cloud end-to-end test: no character of either rewrite is lost
    let t = concurrent_rewrites("first draft", "agenda: budget", "agenda: hiring");
    assert!(
        t.contains("agenda: budget") && t.contains("agenda: hiring"),
        "{t}"
    );
    assert!(!t.contains("first") && !t.contains("draft"), "{t}");
}

#[test]
fn a_rewrite_sharing_only_one_trailing_char_does_not_borrow_it() {
    let t = concurrent_rewrites("plan", "lean", "moon");
    assert!(t.contains("lean") && t.contains("moon"), "{t}");
    assert_eq!(t.len(), "lean".len() + "moon".len(), "{t}");
}

#[test]
fn a_rewrite_sharing_a_word_keeps_the_word_shared() {
    // both keep " notes" and change the word before it: the shared word appears once
    let t = concurrent_rewrites("meeting notes", "standup notes", "review notes");
    assert!(t.contains("standup") && t.contains("review"), "{t}");
    assert_eq!(t.matches("notes").count(), 1, "{t}");
    assert!(t.ends_with(" notes"), "{t}");
}

#[test]
fn emoji_and_cjk_rewrites_keep_every_cluster() {
    let t = concurrent_rewrites("会议记录 👩🏽‍🔬", "会议纪要 👩🏽‍🚀", "会议总结 🧑🏿‍🔬");
    for want in ["纪要", "总结", "👩🏽‍🚀", "🧑🏿‍🔬"] {
        assert!(t.contains(want), "{want} missing from {t}");
    }
    assert!(t.starts_with("会议"), "{t}");
    assert_eq!(t.matches("会议").count(), 1, "{t}");
    // no lone skin tone, joiner or half of a sequence left behind
    for c in ["\u{200d}🔬", "\u{200d}🚀"] {
        assert!(t.matches(c).count() <= 2, "{t}");
    }
}

#[test]
fn typing_into_shared_text_stays_minimal() {
    assert_eq!(
        penroam_core::textcrdt::diff("first draft", "first drafts"),
        (11, 0, "s".into())
    );
    assert_eq!(
        penroam_core::textcrdt::diff("ab cd", "ab xcd"),
        (3, 0, "x".into())
    );
    assert_eq!(
        penroam_core::textcrdt::diff("ab cd", "ab d"),
        (3, 1, String::new())
    );
    // a replace snaps to whole words, and never borrows a coincidental last letter
    assert_eq!(
        penroam_core::textcrdt::diff("first draft", "agenda: budget"),
        (0, 11, "agenda: budget".into())
    );
}

// ------------------------------------------------------- hostile updates --

use yrs::updates::decoder::Decode;
use yrs::{Doc, OffsetKind, Options, ReadTxn, Text, Transact, Update};

/// A Yrs doc holding `r`'s current text for the block, under `client`.
fn raw_fork(r: &Replica, client: u64) -> Doc {
    let d = Doc::with_options(Options {
        client_id: yrs::block::ClientID::new(client),
        offset_kind: OffsetKind::Utf16,
        ..Options::default()
    });
    let (base, _) = r.text_of(PG, TXT).unwrap();
    // the genesis only: enough for crafting edits on top of it
    let g = TextDoc::genesis_update(base, "hello world");
    d.transact_mut()
        .apply_update(Update::decode_v1(&g).unwrap())
        .unwrap();
    d
}

fn edit_op(dev: &mut Dev, update: Vec<u8>) -> LocalOperation {
    let base = Uuid::from_u128((1u128 << 64) + 3); // the element-add of `setup` (device 1, op 3)
    dev.op(DocumentOperation::TextEdited {
        notebook_id: NB,
        page_id: PG,
        element_id: TXT,
        base,
        update: b64::encode(&update),
    })
}

fn crafted(
    r: &Replica,
    client: u64,
    f: impl FnOnce(&Doc, &yrs::TextRef, &mut yrs::TransactionMut),
) -> Vec<u8> {
    let d = raw_fork(r, client);
    let before = d.transact().state_vector();
    let t = d.get_or_insert_text("text");
    {
        let mut tx = d.transact_mut();
        f(&d, &t, &mut tx);
    }
    let u = d.transact().encode_diff_v1(&before);
    u
}

#[test]
fn an_update_under_another_devices_client_id_is_refused() {
    let (mut a, mut b) = (Dev::new(1), Dev::new(2));
    let base = setup(&mut a);
    a.saw(&base);
    b.saw(&base);
    let mut r = replica(&base.iter().collect::<Vec<_>>());
    // b writes items in a's name, at a's next clocks
    let forged = crafted(&r, edit_client(a.id), |_, t, tx| t.insert(tx, 5, " forged"));
    let op = edit_op(&mut b, forged);
    assert!(r.apply_delivered(&op).is_err());
    assert_eq!(shown(&r), "hello world");
    // the same bytes under b's own id are fine
    let honest = crafted(&r, edit_client(b.id), |_, t, tx| t.insert(tx, 5, " honest"));
    let op = edit_op(&mut b, honest);
    r.apply_delivered(&op).unwrap();
    assert_eq!(shown(&r), "hello honest world");
}

#[test]
fn three_replicas_converge_with_two_devices_typing_at_once() {
    let (mut a, mut b) = (Dev::new(1), Dev::new(2));
    let base = setup(&mut a);
    a.saw(&base);
    b.saw(&base);
    let start = replica(&base.iter().collect::<Vec<_>>());
    let a1 = a.type_text(&start, "hello there world");
    let b1 = b.type_text(&start, "hello world, friend");
    let mut seen = Vec::new();
    for o in [[&a1, &b1], [&b1, &a1], [&a1, &b1]] {
        let mut r = replica(&base.iter().collect::<Vec<_>>());
        for op in o {
            r.apply_delivered(op).unwrap();
        }
        seen.push(shown(&r));
    }
    assert!(
        seen.iter().all(|t| t == "hello there world, friend"),
        "{seen:?}"
    );
}

#[test]
fn formatting_embeds_and_other_types_are_refused() {
    let mut a = Dev::new(1);
    let base = setup(&mut a);
    let mut b = Dev::new(2);
    b.saw(&base);
    let mut r = replica(&base.iter().collect::<Vec<_>>());
    let me = edit_client(b.id);
    let attrs = yrs::types::Attrs::from([("bold".into(), true.into())]);
    let formatted = crafted(&r, me, |_, t, tx| {
        t.insert_with_attributes(tx, 0, "B", attrs)
    });
    assert!(r.apply_delivered(&edit_op(&mut b, formatted)).is_err());
    let embed = crafted(&r, me, |_, t, tx| {
        t.insert_embed(tx, 0, yrs::Any::from(7.0));
    });
    let mut b2 = Dev::new(3);
    b2.saw(&base);
    let embed_under_b2 = {
        let _ = embed;
        crafted(&r, edit_client(b2.id), |_, t, tx| {
            t.insert_embed(tx, 0, yrs::Any::from(7.0));
        })
    };
    assert!(r
        .apply_delivered(&edit_op(&mut b2, embed_under_b2))
        .is_err());
    let mut b3 = Dev::new(4);
    b3.saw(&base);
    // creating a root type alone writes no blocks; a map with content does
    let other_type2 = {
        let d = raw_fork(&r, edit_client(b3.id));
        let before = d.transact().state_vector();
        let m = d.get_or_insert_map("evil");
        {
            let mut tx = d.transact_mut();
            yrs::Map::insert(&m, &mut tx, "k", "v");
        }
        let u = d.transact().encode_diff_v1(&before);
        u
    };
    assert!(r.apply_delivered(&edit_op(&mut b3, other_type2)).is_err());
    assert_eq!(shown(&r), "hello world");
}

#[test]
fn an_update_that_skips_ahead_is_refused_quickly() {
    let mut a = Dev::new(1);
    let base = setup(&mut a);
    let mut b = Dev::new(2);
    b.saw(&base);
    let mut r = replica(&base.iter().collect::<Vec<_>>());
    // b's doc: one first edit, then 100 000 single-character inserts; only the later part is sent
    let d = raw_fork(&r, edit_client(b.id));
    let t = d.get_or_insert_text("text");
    {
        let mut tx = d.transact_mut();
        t.insert(&mut tx, 0, "x");
    }
    let after_first = d.transact().state_vector();
    for i in 0..100_000u32 {
        let mut tx = d.transact_mut();
        t.insert(&mut tx, 1 + (i % 7), "y");
    }
    let tail = d.transact().encode_diff_v1(&after_first);
    let t0 = std::time::Instant::now();
    assert!(r.apply_delivered(&edit_op(&mut b, tail)).is_err());
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(shown(&r), "hello world");
}

#[test]
fn random_and_mangled_bytes_never_panic_or_change_the_text() {
    let mut a = Dev::new(1);
    let base = setup(&mut a);
    let mut b = Dev::new(2);
    b.saw(&base);
    let mut r = replica(&base.iter().collect::<Vec<_>>());
    let good = crafted(&r, edit_client(b.id), |_, t, tx| t.insert(tx, 5, "!"));
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for i in 0..3000 {
        let bytes: Vec<u8> = if i % 2 == 0 {
            let len = (next() % 200) as usize;
            (0..len).map(|_| next() as u8).collect()
        } else {
            // a valid update with some bytes flipped
            let mut v = good.clone();
            for _ in 0..(1 + next() % 4) {
                let at = (next() as usize) % v.len();
                v[at] ^= (next() as u8) | 1;
            }
            v
        };
        let mut d = Dev::new(1000 + i as u128);
        d.saw(&base);
        let before = shown(&r);
        // a mangled update from someone else's name is never taken
        assert!(r.apply_delivered(&edit_op(&mut d, bytes)).is_err() || shown(&r) == before);
        assert_eq!(shown(&r), before, "update #{i} changed the text");
        let doc = TextDoc::genesis(Uuid::nil(), "");
        let _ = doc.apply(&b64::decode(&b64::encode(b"\x00\x01")).unwrap());
    }
    // nothing got in, and the replica still works
    let t = shown(&r);
    assert!(!t.is_empty());
    let fine = b.type_text(&r, &format!("{t}?"));
    r.apply_delivered(&fine).unwrap();
    let after = shown(&r);
    assert!(after.ends_with('?'), "{t:?} -> {after:?}");
}

#[test]
fn a_panic_inside_yrs_mid_sequence_leaves_the_text_as_if_that_edit_never_came() {
    let (mut a, mut b, mut c) = (Dev::new(1), Dev::new(2), Dev::new(3));
    let base = setup(&mut a);
    for d in [&mut a, &mut b, &mut c] {
        d.saw(&base);
    }
    let start = replica(&base.iter().collect::<Vec<_>>());
    // three people type at once, each at a different place
    let ea = a.type_text(&start, "A hello world");
    let eb = b.type_text(&start, "hello B world");
    let ec = c.type_text(&start, "hello world C");
    let expected = shown(&replica(&base.iter().chain([&ea, &ec]).collect::<Vec<_>>()));
    // applying B's edit panics inside Yrs after changing the document
    let DocumentOperation::TextEdited { update, .. } = &eb.body else {
        unreachable!()
    };
    TextDoc::panic_after_applying_for_tests(Some(b64::decode(update).unwrap()));
    let mut r = replica(&base.iter().collect::<Vec<_>>());
    for op in [&ea, &eb, &ec] {
        r.apply_delivered(op).unwrap();
    }
    TextDoc::panic_after_applying_for_tests(None);
    assert_eq!(shown(&r), expected);
    assert!(shown(&r).contains('A') && shown(&r).contains('C') && !shown(&r).contains('B'));
}

#[test]
fn edit_updates_must_be_applied_one_before_the_next() {
    let client = edit_client(Uuid::from_u128(9));
    let base = TextDoc::genesis(Uuid::from_u128(1), "ab");
    // applied in between: both edits land
    let d = base.clone();
    let u1 = d.edit_update(client, "aXb").unwrap();
    d.apply(&u1).unwrap();
    let u2 = d.edit_update(client, "aXbY").unwrap();
    d.apply(&u2).unwrap();
    assert_eq!(d.text(), "aXbY");
    // made from the same unapplied state: the second reuses the first's clocks and is lost
    let d = base.clone();
    let v1 = d.edit_update(client, "aXb").unwrap();
    let v2 = d.edit_update(client, "abY").unwrap();
    d.apply(&v1).unwrap();
    let _ = d.apply(&v2);
    assert_eq!(
        d.text(),
        "aXb",
        "the documented requirement: apply before the next edit"
    );
}

#[test]
fn concurrent_changes_inside_one_grapheme_cluster_keep_both_and_converge() {
    let (mut a, mut b, mut c) = (Dev::new(1), Dev::new(2), Dev::new(3));
    let mut base = setup(&mut a);
    let r0 = replica(&base.iter().collect::<Vec<_>>());
    base.push(a.type_text(&r0, "hello 👍 world"));
    for d in [&mut a, &mut b, &mut c] {
        d.saw(&base);
    }
    let start = replica(&base.iter().collect::<Vec<_>>());
    // each adds a different skin tone to the same emoji: an insert inside its cluster, which the
    // diff widens to the whole cluster
    let eb = b.type_text(&start, "hello 👍🏽 world");
    let ec = c.type_text(&start, "hello 👍🏿 world");
    let one = shown(&replica(&base.iter().chain([&eb, &ec]).collect::<Vec<_>>()));
    let two = shown(&replica(&base.iter().chain([&ec, &eb]).collect::<Vec<_>>()));
    assert_eq!(one, two, "converges");
    // the price of never splitting a cluster: the base emoji is duplicated, nothing is lost
    assert!(one.contains("👍🏽") && one.contains("👍🏿"), "{one}");
    assert!(
        one.starts_with("hello ") && one.ends_with(" world"),
        "{one}"
    );
}

/// A small deterministic generator (xorshift64*), so failures replay from the seed.
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
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// One simulated device: its writer, replica, what it has applied and what is in flight to it.
struct Peer {
    dev: Dev,
    rep: Replica,
    applied: BTreeMap<Uuid, u64>,
    inbox: Vec<LocalOperation>,
    cut_off: bool,
}

impl Peer {
    fn ready(&self, op: &LocalOperation) -> bool {
        op.causal_dependencies
            .iter()
            .all(|d| self.applied.get(&d.device_id).copied().unwrap_or(0) >= d.device_seq)
    }
    fn take(&mut self, op: &LocalOperation) {
        self.rep.apply_delivered(op).unwrap();
        let e = self.applied.entry(op.device_id).or_insert(0);
        *e = (*e).max(op.local_sequence);
        self.dev.saw(std::slice::from_ref(op));
    }
    /// Applies whatever in the inbox has its dependencies, in the shuffled order, until nothing
    /// more can be applied. Duplicates are applied again (they must be no-ops).
    fn deliver(&mut self, rng: &mut Rng) {
        for i in (1..self.inbox.len()).rev() {
            self.inbox.swap(i, rng.below(i + 1));
        }
        while let Some(i) = self.inbox.iter().position(|o| self.ready(o)) {
            let op = self.inbox.remove(i);
            self.take(&op);
        }
    }
}

const TOKENS: &[&str] = &["日本", "שלום", "😀", "Ünï", "中文字", "🎉", "مرحبا", "ok"];

fn random_delivery(seed: u64, peers: usize, rounds: usize) {
    let mut rng = Rng(seed | 1);
    let mut first = Dev::new(1);
    let base = setup(&mut first);
    let mut net: Vec<Peer> = (0..peers)
        .map(|i| {
            let mut p = Peer {
                dev: if i == 0 {
                    Dev::new(1)
                } else {
                    Dev::new(i as u128 + 1)
                },
                rep: Replica::new(NB),
                applied: BTreeMap::new(),
                inbox: Vec::new(),
                cut_off: false,
            };
            for op in &base {
                p.take(op);
            }
            p
        })
        .collect();
    net[0].dev.seq = first.seq;
    net[0].dev.n = first.n;
    let mut written: Vec<String> = Vec::new();
    for round in 0..rounds {
        // partitions come and go
        for p in &mut net {
            if rng.chance(15) {
                p.cut_off = !p.cut_off;
            }
        }
        // someone types a token at a random character boundary of what they see
        let w = rng.below(peers);
        let text = shown(&net[w].rep);
        let chars: Vec<char> = text.chars().collect();
        let at = rng.below(chars.len() + 1);
        let token = format!("{}{round}", TOKENS[rng.below(TOKENS.len())]);
        let new: String = chars[..at].iter().collect::<String>()
            + &token
            + &chars[at..].iter().collect::<String>();
        let op = {
            let p = &mut net[w];
            let op = p.dev.type_text(&p.rep, &new);
            p.take(&op);
            op
        };
        written.push(token);
        for (i, p) in net.iter_mut().enumerate() {
            if i != w {
                p.inbox.push(op.clone());
                if rng.chance(20) {
                    p.inbox.push(op.clone()); // duplicated in transit
                }
            }
        }
        for p in &mut net {
            if !p.cut_off {
                p.deliver(&mut rng);
            }
        }
    }
    // everyone heals and receives everything
    for p in &mut net {
        p.cut_off = false;
        p.deliver(&mut rng);
        assert!(p.inbox.is_empty(), "seed {seed}: undeliverable operations");
    }
    let texts: Vec<String> = net.iter().map(|p| shown(&p.rep)).collect();
    for t in &texts {
        assert_eq!(t, &texts[0], "seed {seed}: replicas differ");
    }
    // only insertions happened (later ones may land inside earlier tokens): the result holds
    // exactly the starting characters plus every inserted one, none lost and none doubled
    let count = |s: &str| {
        let mut m: BTreeMap<char, usize> = BTreeMap::new();
        for c in s.chars() {
            *m.entry(c).or_insert(0) += 1;
        }
        m
    };
    let mut want = count("hello world");
    for token in &written {
        for (c, n) in count(token) {
            *want.entry(c).or_insert(0) += n;
        }
    }
    assert_eq!(count(&texts[0]), want, "seed {seed}: {}", texts[0]);
}

#[test]
fn random_reorder_duplication_and_partitions_converge_without_losing_text() {
    for seed in [1u64, 7, 42, 1234, 99_991, 0xdead_beef, 31_337, 271_828] {
        random_delivery(seed, 4, 60);
    }
}

#[test]
fn concurrent_typing_in_non_latin_scripts_converges_in_any_order() {
    let (mut a, mut b, mut c) = (Dev::new(1), Dev::new(2), Dev::new(3));
    let mut base = setup(&mut a);
    let r0 = replica(&base.iter().collect::<Vec<_>>());
    base.push(a.type_text(&r0, "日本語 שלום 😀"));
    for d in [&mut a, &mut b, &mut c] {
        d.saw(&base);
    }
    let start = replica(&base.iter().collect::<Vec<_>>());
    let eb = b.type_text(&start, "日本語です שלום 😀");
    let ec = c.type_text(&start, "日本語 שלום עולם 😀🎉");
    let ea = a.type_text(&start, "中文 日本語 שלום 😀");
    let orders = [[&ea, &eb, &ec], [&ec, &eb, &ea], [&eb, &ea, &ec]];
    let texts: Vec<String> = orders
        .iter()
        .map(|o| {
            shown(&replica(
                &base.iter().chain(o.iter().copied()).collect::<Vec<_>>(),
            ))
        })
        .collect();
    assert!(texts.iter().all(|t| t == &texts[0]), "{texts:?}");
    for piece in ["です", "עולם", "🎉", "中文"] {
        assert!(texts[0].contains(piece), "{piece} in {}", texts[0]);
    }
}

#[test]
fn updates_declaring_huge_counts_are_refused_before_decoding() {
    let doc = TextDoc::genesis(Uuid::from_u128(1), "hello");
    let me = edit_client(Uuid::from_u128(2));
    let var = |mut v: u64| {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    };
    let bombs: Vec<Vec<u8>> = vec![
        // 200 million clients
        var(200_000_000),
        // one client with 200 million blocks
        [var(1), var(200_000_000), var(me), var(0)].concat(),
        // no blocks, a delete set naming 200 million clients
        [var(0), var(200_000_000)].concat(),
        // a string longer than the update
        [
            var(1),
            var(1),
            var(me),
            var(0),
            vec![4u8],
            var(1),
            var(4),
            b"text".to_vec(),
            var(1 << 30),
        ]
        .concat(),
    ];
    for b in &bombs {
        let t = std::time::Instant::now();
        assert!(doc.check_edit(b, me).is_err());
        assert!(
            t.elapsed() < std::time::Duration::from_millis(50),
            "{:?}",
            t.elapsed()
        );
    }
    assert_eq!(doc.text(), "hello");
}
