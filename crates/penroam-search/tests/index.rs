//! Search index behaviour: segmentation, incremental re-recognition, ranking, fuzzy matching,
//! corrections, notebook changes, PDF text and index recovery. All recognition here uses the
//! deterministic `ScriptedRecognizer`; the Windows recognizer has its own test.

use penroam_core::*;
use penroam_search::index::Recognition;
use penroam_search::*;
use std::cell::Cell;
use uuid::Uuid;

/// A small zig-zag stroke filling `[x, x+w] × [y, y+h]`, written at `t` µs.
fn stroke(id: u128, x: f64, y: f64, w: f64, h: f64, t: i64) -> Element {
    let points = (0..8)
        .map(|i| InkPoint {
            x: x + w * i as f64 / 7.0,
            y: if i % 2 == 0 { y } else { y + h },
            pressure: 0.5,
            tilt_x: 0.0,
            tilt_y: 0.0,
            time_us: t + i * 1000,
            twist: None,
            flags: None,
        })
        .collect();
    Element::Ink(InkStroke {
        id: Uuid::from_u128(id),
        z: id as i64,
        tool: InkTool::Pen,
        color: "#000000".into(),
        opacity: 1.0,
        width: 1.5,
        pressure: false,
        source: "test".into(),
        input: Default::default(),
        points,
        extra: Default::default(),
    })
}

fn text_block(id: u128, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::from_u128(id),
        z: id as i64,
        x: 72.0,
        y: 600.0,
        width: 200.0,
        height: 20.0,
        text: s.into(),
        font_size: 12.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        extra: Default::default(),
    })
}

fn page(id: u128) -> Page {
    Page::new(
        Uuid::from_u128(id),
        Paper::Letter,
        Paper::Letter.size().unwrap(),
        Template::default(),
        0,
    )
}

fn notebook(id: u128, title: &str, pages: &[&Page]) -> Notebook {
    let mut nb = Notebook::new(
        Uuid::from_u128(id),
        title.into(),
        PageDefaults::default(),
        "#3b5b7a".into(),
        0,
    );
    nb.page_order = pages.iter().map(|p| p.id).collect();
    nb.rebuild_order_keys();
    nb
}

fn uid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// Two lines of writing: "quarterly budget review" at y=100 and "call alice" at y=160.
/// Each word is two strokes; "alice" has an i-dot.
fn handwritten_page() -> (Page, ScriptedRecognizer) {
    let mut p = page(100);
    let mut rec = ScriptedRecognizer::new("1");
    let mut t = 0;
    let mut next = |p: &mut Page, id: u128, x: f64, y: f64, w: f64, h: f64| {
        t += 100_000;
        p.elements.push(stroke(id, x, y, w, h, t));
    };
    // line 1
    next(&mut p, 1, 72.0, 100.0, 30.0, 18.0);
    next(&mut p, 2, 104.0, 100.0, 30.0, 18.0);
    next(&mut p, 3, 150.0, 100.0, 25.0, 18.0);
    next(&mut p, 4, 177.0, 100.0, 25.0, 18.0);
    next(&mut p, 5, 220.0, 100.0, 30.0, 18.0);
    next(&mut p, 6, 252.0, 100.0, 30.0, 18.0);
    // line 2
    next(&mut p, 7, 72.0, 160.0, 25.0, 18.0);
    next(&mut p, 8, 120.0, 160.0, 25.0, 18.0);
    next(&mut p, 9, 147.0, 160.0, 15.0, 18.0);
    next(&mut p, 10, 152.0, 150.0, 2.0, 2.0); // i-dot, above the line
                                              // a large box: a drawing, not writing
    next(&mut p, 11, 300.0, 300.0, 200.0, 200.0);
    rec.word("quarterly", &[uid(1), uid(2)]);
    rec.word("budget", &[uid(3), uid(4)]);
    rec.word("review", &[uid(5), uid(6)]);
    rec.word("call", &[uid(7)]);
    rec.word("alice", &[uid(8), uid(9), uid(10)]);
    p.revision = 1;
    (p, rec)
}

fn index_with(
    ix: &mut SearchIndex,
    nb: &Notebook,
    p: &Page,
    rec: &mut ScriptedRecognizer,
) -> PageOutcome {
    ix.sync_notebook(nb).unwrap();
    let i = nb.page_order.iter().position(|x| *x == p.id).unwrap() as u32;
    ix.index_page(
        nb.id,
        i,
        p,
        Some(Recognition {
            recognizer: rec,
            language: "en-US",
        }),
        &|| false,
    )
    .unwrap()
}

#[test]
fn strokes_group_into_lines_and_drawings_are_skipped() {
    let (p, _) = handwritten_page();
    let regions = penroam_search::segment::regions(&p);
    let ids: Vec<Vec<Uuid>> = regions.iter().map(|r| r.stroke_ids()).collect();
    assert_eq!(
        ids,
        vec![
            (1..=6).map(uid).collect::<Vec<_>>(),
            (7..=10).map(uid).collect::<Vec<_>>()
        ]
    );
    // keys are independent of position, fingerprints are not
    let mut moved = p.clone();
    for e in &mut moved.elements {
        if let Element::Ink(s) = e {
            for pt in &mut s.points {
                pt.x += 5.0;
            }
        }
    }
    let r2 = penroam_search::segment::regions(&moved);
    assert_eq!(regions[0].key, r2[0].key);
    assert_ne!(regions[0].fingerprint, r2[0].fingerprint);
}

#[test]
fn search_finds_handwriting_typed_text_and_titles_with_sources() {
    let (mut p, mut rec) = handwritten_page();
    p.elements
        .push(text_block(50, "Budget notes for the offsite"));
    let nb = notebook(1, "Planning", &[&p]);
    let mut ix = SearchIndex::open_in_memory().unwrap();
    let out = index_with(&mut ix, &nb, &p, &mut rec);
    assert_eq!((out.regions, out.recognized, out.pending), (2, 2, 0));

    let hits = ix.search("budget", 10).unwrap();
    assert_eq!(hits.len(), 2);
    // typed text outranks handwriting with the same match
    assert_eq!(hits[0].kind, EntryKind::Text);
    assert_eq!(hits[1].kind, EntryKind::Ink);
    let ink = &hits[1];
    assert_eq!((ink.page_id, ink.page_index), (Some(p.id), Some(0)));
    assert_eq!(ink.notebook_title, "Planning");
    assert_eq!(
        ink.match_bounds.len(),
        1,
        "only the word 'budget' is highlighted"
    );
    let b = ink.match_bounds[0];
    assert!(
        b.x < 150.0 && b.right() > 202.0 && b.y < 100.0 && b.bottom() > 118.0,
        "{b:?}"
    );
    let line = ink.bounds.unwrap();
    assert!(line.x < 72.0 && line.right() > 282.0);
    assert!(ink.snippet.iter().any(|s| s.hit && s.text == "budget"));

    // prefix, phrase and title matches
    assert_eq!(ix.search("quart", 10).unwrap().len(), 1);
    assert_eq!(ix.search("\"budget review\"", 10).unwrap().len(), 1);
    assert!(ix.search("\"review budget\"", 10).unwrap().is_empty());
    let t = ix.search("planning", 10).unwrap();
    assert_eq!((t.len(), t[0].kind), (1, EntryKind::Title));
    // the i-dot belongs to "alice"
    let a = ix.search("alice", 10).unwrap();
    assert_eq!(a.len(), 1);
    assert!(a[0].match_bounds[0].y < 152.0);
    // query syntax is inert
    for q in [
        "NOT",
        "budget OR",
        "\"",
        "a*b:c^",
        "(",
        "NEAR(x y)",
        "{col}: x",
    ] {
        ix.search(q, 10).unwrap();
    }
}

#[test]
fn edits_re_recognize_only_changed_lines() {
    let (mut p, mut rec) = handwritten_page();
    let mut nb = notebook(1, "N", &[&p]);
    let mut ix = SearchIndex::open_in_memory().unwrap();
    index_with(&mut ix, &nb, &p, &mut rec);
    assert_eq!(rec.calls, 2);
    assert!(!ix
        .page_is_stale(nb.id, p.id, 1, "scripted/1/en-US")
        .unwrap());

    // add a word to line 2
    p.elements
        .push(stroke(12, 180.0, 160.0, 25.0, 18.0, 9_000_000));
    rec.word("tomorrow", &[uid(12)]);
    p.revision = 2;
    assert!(ix
        .page_is_stale(nb.id, p.id, 2, "scripted/1/en-US")
        .unwrap());
    let out = index_with(&mut ix, &nb, &p, &mut rec);
    assert_eq!((out.recognized, out.reused), (1, 1));
    assert_eq!(ix.search("tomorrow", 10).unwrap().len(), 1);

    // delete line 1 entirely
    p.elements
        .retain(|e| !(1..=6).contains(&(e.id().as_u128())));
    p.revision = 3;
    let out = index_with(&mut ix, &nb, &p, &mut rec);
    assert_eq!((out.recognized, out.reused), (0, 1));
    assert!(ix.search("budget", 10).unwrap().is_empty());

    // a model version bump makes the page stale; old rows stay searchable until replaced
    let mut rec2 = ScriptedRecognizer::new("2");
    rec2.word("call", &[uid(7)]);
    rec2.word("alice", &[uid(8), uid(9), uid(10)]);
    assert!(ix
        .page_is_stale(nb.id, p.id, 3, "scripted/2/en-US")
        .unwrap());
    assert_eq!(ix.search("tomorrow", 10).unwrap().len(), 1);
    index_with(&mut ix, &nb, &p, &mut rec2);
    assert_eq!(rec2.calls, 1);
    assert!(ix.search("tomorrow", 10).unwrap().is_empty());

    // turning recognition off drops handwriting rows but keeps typed text
    p.elements.push(text_block(50, "typed"));
    ix.index_page(nb.id, 0, &p, None, &|| false).unwrap();
    assert!(ix.search("alice", 10).unwrap().is_empty());
    assert_eq!(ix.search("typed", 10).unwrap().len(), 1);
    assert_eq!(ix.page_state(nb.id, p.id).unwrap().unwrap().recognizer, "");

    // trashing the page removes its rows; restoring makes it unknown again
    nb.page_order.clear();
    assert!(ix.sync_notebook(&nb).unwrap().is_empty());
    assert!(ix.search("typed", 10).unwrap().is_empty());
    nb.page_order.push(p.id);
    assert_eq!(ix.sync_notebook(&nb).unwrap(), vec![p.id]);
}

#[test]
fn cancelled_pass_is_incomplete_and_resumes() {
    let (p, mut rec) = handwritten_page();
    let nb = notebook(1, "N", &[&p]);
    let mut ix = SearchIndex::open_in_memory().unwrap();
    ix.sync_notebook(&nb).unwrap();
    let calls = Cell::new(0);
    let out = ix
        .index_page(
            nb.id,
            0,
            &p,
            Some(Recognition {
                recognizer: &mut rec,
                language: "en-US",
            }),
            &|| {
                calls.set(calls.get() + 1);
                calls.get() > 1 // cancel before the second region
            },
        )
        .unwrap();
    assert_eq!((out.recognized, out.pending), (1, 1));
    assert!(!ix.page_state(nb.id, p.id).unwrap().unwrap().complete);
    assert_eq!(ix.search("budget", 10).unwrap().len(), 1);
    let out = index_with(&mut ix, &nb, &p, &mut rec);
    assert_eq!((out.recognized, out.reused, out.pending), (1, 1, 0));
    assert!(ix.page_state(nb.id, p.id).unwrap().unwrap().complete);

    // an unavailable language is a pending error, not a failure
    let mut ix2 = SearchIndex::open_in_memory().unwrap();
    ix2.sync_notebook(&nb).unwrap();
    let out = ix2
        .index_page(
            nb.id,
            0,
            &p,
            Some(Recognition {
                recognizer: &mut rec,
                language: "xx",
            }),
            &|| false,
        )
        .unwrap();
    assert_eq!(out.pending, 2);
    assert!(out.error.unwrap().contains("xx"));
}

#[test]
fn fuzzy_matches_recognition_errors_and_corrections_win() {
    let (p, _) = handwritten_page();
    let mut rec = ScriptedRecognizer::new("1");
    rec.word("quarterlv", &[uid(1), uid(2)]); // recognizer misread
    rec.word("budqet", &[uid(3), uid(4)]);
    rec.word("callreview", &[uid(5), uid(6)]); // two words run together
    let nb = notebook(1, "N", &[&p]);
    let mut ix = SearchIndex::open_in_memory().unwrap();
    index_with(&mut ix, &nb, &p, &mut rec);

    let joined = ix.search("review", 10).unwrap();
    assert_eq!(joined.len(), 1);
    assert!(joined[0].fuzzy);
    assert_eq!(joined[0].match_bounds.len(), 1);

    let hits = ix.search("quarterly", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].fuzzy);
    assert_eq!(hits[0].match_bounds.len(), 1);
    // too short for fuzzy matching
    assert!(ix.search("cat", 10).unwrap().is_empty());

    let key = hits[0].region_key.clone().unwrap();
    ix.set_correction(nb.id, p.id, &key, Some("quarterly budget review"))
        .unwrap();
    let hits = ix.search("budget", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!hits[0].fuzzy);
    assert!(hits[0].snippet.iter().any(|s| s.hit && s.text == "budget"));

    // moving the strokes keeps the correction; erasing them drops it
    let mut moved = p.clone();
    for e in &mut moved.elements {
        if let Element::Ink(s) = e {
            for pt in &mut s.points {
                pt.y += 3.0;
            }
        }
    }
    moved.revision = 2;
    index_with(&mut ix, &nb, &moved, &mut rec);
    assert_eq!(ix.search("budget", 10).unwrap().len(), 1);
    let mut erased = moved.clone();
    erased.elements.retain(|e| e.id() != uid(1));
    erased.revision = 3;
    index_with(&mut ix, &nb, &erased, &mut rec);
    assert_eq!(ix.prune_corrections().unwrap(), 1);
    assert_eq!(ix.stats().unwrap().corrections, 0);
}

#[test]
fn pdf_text_titles_and_trashed_notebooks() {
    let mut p = page(200);
    p.background = Some(PdfBackgroundRef {
        asset_hash: "ab".repeat(32),
        page_index: 1,
        media_box: [0.0, 0.0, 612.0, 792.0],
        crop_box: [0.0, 0.0, 612.0, 792.0],
        rotation: 0,
    });
    let mut nb = notebook(2, "Contract", &[&p]);
    let mut ix = SearchIndex::open_in_memory().unwrap();
    assert!(!ix.has_pdf(&"ab".repeat(32)).unwrap());
    ix.store_pdf(
        &"ab".repeat(32),
        &["cover page".into(), "indemnification clause".into()],
    )
    .unwrap();
    assert!(ix.has_pdf(&"ab".repeat(32)).unwrap());
    ix.sync_notebook(&nb).unwrap();
    ix.index_page(nb.id, 0, &p, None, &|| false).unwrap();
    let hits = ix.search("indemnif", 10).unwrap();
    assert_eq!((hits.len(), hits[0].kind), (1, EntryKind::Pdf));
    assert!(ix.search("cover", 10).unwrap().is_empty(), "other PDF page");

    nb.title = "Lease".into();
    ix.sync_notebook(&nb).unwrap();
    assert!(ix.search("contract", 10).unwrap().is_empty());
    assert_eq!(ix.search("lease", 10).unwrap().len(), 1);
    nb.deleted_at = Some(1);
    ix.sync_notebook(&nb).unwrap();
    assert!(ix.search("lease", 10).unwrap().is_empty());
    assert!(ix.search("indemnification", 10).unwrap().is_empty());
    assert_eq!(ix.stats().unwrap(), IndexStats::default());
}

#[test]
fn damaged_foreign_or_old_index_files_are_recreated() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("search").join("index.db");
    let lib = uid(77);
    let (mut ix, fresh) = SearchIndex::open(&path, lib).unwrap();
    assert!(fresh);
    let (p, mut rec) = handwritten_page();
    let nb = notebook(1, "Kept", &[&p]);
    index_with(&mut ix, &nb, &p, &mut rec);
    drop(ix);

    // reopened as is
    let (ix, fresh) = SearchIndex::open(&path, lib).unwrap();
    assert!(!fresh);
    assert_eq!(ix.search("budget", 10).unwrap().len(), 1);
    // a reader sees the writer's rows
    let r = SearchIndex::open_reader(&path).unwrap();
    assert_eq!(r.search("alice", 10).unwrap().len(), 1);
    drop((ix, r));

    // another library's index is never reused
    let (ix, fresh) = SearchIndex::open(&path, uid(78)).unwrap();
    assert!(fresh);
    assert!(ix.search("budget", 10).unwrap().is_empty());
    drop(ix);

    // garbage bytes
    SearchIndex::delete_files(&path).unwrap();
    std::fs::write(
        &path,
        b"this is not a database at all, not even close......",
    )
    .unwrap();
    let (ix, fresh) = SearchIndex::open(&path, lib).unwrap();
    assert!(fresh);
    assert_eq!(ix.stats().unwrap(), IndexStats::default());
    drop(ix);
    SearchIndex::delete_files(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn chain_uses_the_platform_first_and_the_model_as_fallback() {
    use penroam_search::recognize::FailingRecognizer;
    let (p, _) = handwritten_page();
    let regions = penroam_search::segment::regions(&p);
    let info = |id: &str, lang: &str| RecognizerInfo {
        id: id.into(),
        model_version: "1".into(),
        name: id.into(),
        languages: vec![lang.into()],
        license: String::new(),
    };
    let mut model = ScriptedRecognizer::new("m1");
    model.word("budget", &[uid(3), uid(4)]);

    // a failing platform recognizer: every region falls back to the model
    let mut chain = ChainRecognizer::new(
        Some(Box::new(FailingRecognizer(info("platform", "en-US")))),
        Some(Box::new(model)),
    )
    .unwrap();
    assert_eq!(chain.info().id, "platform+scripted");
    assert_eq!(chain.info().model_version, "1+m1");
    let out = chain.recognize(&regions[0], "en-US").unwrap();
    assert_eq!(out[0].text, "budget");

    // a working platform recognizer is used as is
    let mut platform = ScriptedRecognizer::new("p1");
    platform.word("platform-says", &[uid(1)]);
    let mut chain = ChainRecognizer::new(
        Some(Box::new(platform)),
        Some(Box::new(ScriptedRecognizer::new("m1"))),
    )
    .unwrap();
    assert_eq!(
        chain.recognize(&regions[0], "en-US").unwrap()[0].text,
        "platform-says"
    );

    // no platform recognizer: the model alone; without a model the platform error stands
    let mut only_model =
        ChainRecognizer::new(None, Some(Box::new(ScriptedRecognizer::new("m")))).unwrap();
    assert!(
        only_model.recognize(&regions[1], "fr-FR").is_ok(),
        "the model runs in its own language"
    );
    let mut only_failing =
        ChainRecognizer::new(Some(Box::new(FailingRecognizer(info("p", "en-US")))), None).unwrap();
    assert!(matches!(
        only_failing.recognize(&regions[0], "en-US"),
        Err(RecognizeError::Engine(_))
    ));
    assert!(ChainRecognizer::new(None, None).is_none());
}
