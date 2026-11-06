//! Storage benchmark (baseline and regression check).
//!
//! Uses only the long-standing public `Library` API, so the same file can be
//! run against the pre-journal storage code and the current tree:
//!
//! ```text
//! cargo run --release -p penroam-storage --example library_bench -- <strokes> [scene-dir]
//! ```
//!
//! One workload per process (so peak RSS is per size). Prints one JSON line.
//! Timings are wall-clock on the local file system with a warm page cache;
//! durable appends include fsync.

use penroam_core::migrate::stroke_doc_to_elements;
use penroam_core::*;
use penroam_storage::Library;
use std::path::{Path, PathBuf};
use std::time::Instant;
use uuid::Uuid;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn peak_rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

fn dir_bytes(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let m = e.metadata().unwrap();
            total += if m.is_dir() {
                dir_bytes(&e.path())
            } else {
                m.len()
            };
        }
    }
    total
}

fn batch(base: u64, kind: OpKind) -> OperationBatch {
    OperationBatch {
        batch_id: Uuid::new_v4(),
        base_revision: base,
        ops: vec![PageOperation {
            op_id: Uuid::new_v4(),
            kind,
        }],
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let scenes = args.get(2).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/benchmark-scenes")
    });
    let scene: serde_json::Value = serde_json::from_slice(
        &std::fs::read(scenes.join(format!("scene-{n}.json"))).expect("scene file"),
    )
    .unwrap();
    let elements = stroke_doc_to_elements(&scene, 1).unwrap();
    let points: usize = elements
        .iter()
        .map(|e| match e {
            Element::Ink(s) => s.points.len(),
            _ => 0,
        })
        .sum();
    let template_stroke = elements[0].clone();

    let tmp = std::env::temp_dir().join(format!("penroam-bench-{}", Uuid::new_v4()));
    let root = tmp.join("library");
    let mut lib = Library::open(&root).unwrap();
    let (nb, page) = lib
        .create_notebook("bench", PageDefaults::default(), None)
        .unwrap();

    // 1. bulk persist of the whole scene as one batch (paste / import path)
    let t = Instant::now();
    let r = lib
        .apply_page_operations(nb.id, page.id, &batch(0, OpKind::AddElements { elements }))
        .unwrap();
    let bulk_ms = ms(t);
    let mut rev = r.revision;

    // 2. incremental durable saves: 200 single-stroke batches on the big page
    let mut append = Vec::new();
    let mut z = n as i64 + 10;
    for _ in 0..200 {
        let mut s = template_stroke.clone();
        if let Element::Ink(ink) = &mut s {
            ink.id = Uuid::new_v4();
            ink.z = z;
        }
        z += 1;
        let t = Instant::now();
        rev = lib
            .apply_page_operations(
                nb.id,
                page.id,
                &batch(rev, OpKind::AddElements { elements: vec![s] }),
            )
            .unwrap()
            .revision;
        append.push(ms(t));
    }

    // 3. random text block edits (typing): one block, 100 replacements
    let tid = Uuid::new_v4();
    let mut tb = TextBlock {
        id: tid,
        z,
        x: 72.0,
        y: 72.0,
        width: 300.0,
        height: 40.0,
        text: String::new(),
        font_size: 16.0,
        color: "#111111".into(),
        align: TextAlign::Left,
        row_pitch: None,
        extra: Default::default(),
    };
    rev = lib
        .apply_page_operations(
            nb.id,
            page.id,
            &batch(
                rev,
                OpKind::AddElements {
                    elements: vec![Element::Text(tb.clone())],
                },
            ),
        )
        .unwrap()
        .revision;
    let mut text_edit = Vec::new();
    for i in 0..100 {
        tb.text.push(char::from(b'a' + (i % 26) as u8));
        let t = Instant::now();
        rev = lib
            .apply_page_operations(
                nb.id,
                page.id,
                &batch(
                    rev,
                    OpKind::ReplaceElements {
                        elements: vec![Element::Text(tb.clone())],
                    },
                ),
            )
            .unwrap()
            .revision;
        text_edit.push(ms(t));
    }

    // 4. checkpoint (fold journal into the binary snapshot)
    let t = Instant::now();
    lib.flush_all().unwrap();
    let flush_ms = ms(t);
    drop(lib);
    let bytes_after_flush = dir_bytes(&root);

    // 5. cold load (clean shutdown): open library + load the big page
    let t = Instant::now();
    let mut lib = Library::open(&root).unwrap();
    let open_ms = ms(t);
    let t = Instant::now();
    let count = lib.with_page(nb.id, page.id, |p| p.elements.len()).unwrap();
    let load_ms = ms(t);
    assert_eq!(count, n + 201);

    // 6. crash recovery: 50 unflushed batches, crash, reopen + load (journal replay)
    for _ in 0..50 {
        let mut s = template_stroke.clone();
        if let Element::Ink(ink) = &mut s {
            ink.id = Uuid::new_v4();
            ink.z = z;
        }
        z += 1;
        rev = lib
            .apply_page_operations(
                nb.id,
                page.id,
                &batch(rev, OpKind::AddElements { elements: vec![s] }),
            )
            .unwrap()
            .revision;
    }
    lib.crash_for_tests();
    let t = Instant::now();
    let mut lib = Library::open(&root).unwrap();
    let count = lib.with_page(nb.id, page.id, |p| p.elements.len()).unwrap();
    let recover_ms = ms(t);
    assert_eq!(count, n + 251);
    drop(lib);

    println!(
        "{}",
        serde_json::json!({
            "strokes": n,
            "points": points,
            "bulkPersistMs": bulk_ms,
            "appendStrokeMsMedian": pct(&mut append.clone(), 0.5),
            "appendStrokeMsP95": pct(&mut append, 0.95),
            "textEditMsMedian": pct(&mut text_edit.clone(), 0.5),
            "textEditMsP95": pct(&mut text_edit, 0.95),
            "checkpointMs": flush_ms,
            "libraryBytesAfterCheckpoint": bytes_after_flush,
            "openLibraryMs": open_ms,
            "coldLoadPageMs": load_ms,
            "crashRecoverOpenAndLoadMs": recover_ms,
            "peakRssKiB": peak_rss_kib(),
        })
    );
    let _ = std::fs::remove_dir_all(&tmp);
}
