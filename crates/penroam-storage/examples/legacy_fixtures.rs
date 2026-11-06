//! Writes sample notebooks with the *current* storage code: `.penroam`
//! archives plus a live library left in a "crashed" state (unflushed
//! journal). Run against the pre-journal storage code to freeze legacy
//! fixtures that later builds must keep reading (`tests/fixtures/legacy/`).
//!
//! ```text
//! cargo run --release -p penroam-storage --example legacy_fixtures -- <out-dir>
//! ```

use penroam_core::migrate::stroke_doc_to_elements;
use penroam_core::*;
use penroam_storage::Library;
use std::path::PathBuf;
use uuid::Uuid;

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

fn text(z: i64, y: f64, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::new_v4(),
        z,
        x: 72.0,
        y,
        width: 320.0,
        height: 60.0,
        text: s.into(),
        font_size: 14.0,
        color: "#1d4ed8".into(),
        align: TextAlign::Left,
        row_pitch: None,
        extra: Default::default(),
    })
}

fn scene(n: usize) -> Vec<Element> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tests/fixtures/benchmark-scenes/scene-{n}.json"
    ));
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    stroke_doc_to_elements(&v, 1).unwrap()
}

fn export(lib: &mut Library, nb: Uuid, path: PathBuf) {
    let mut f = std::fs::File::create(&path).unwrap();
    lib.export_archive(nb, &mut f, "penroam-legacy-fixtures")
        .unwrap();
    println!("wrote {}", path.display());
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("output dir"));
    std::fs::create_dir_all(&out).unwrap();
    let tmp = std::env::temp_dir().join(format!("penroam-fixtures-{}", Uuid::new_v4()));
    let mut lib = Library::open(tmp.join("library")).unwrap();

    // tiny: one stroke
    let (nb, p) = lib
        .create_notebook("tiny", PageDefaults::default(), None)
        .unwrap();
    let one = scene(100).into_iter().take(1).collect();
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(0, OpKind::AddElements { elements: one }),
    )
    .unwrap();
    export(&mut lib, nb.id, out.join("tiny.penroam"));

    // 1k and 10k strokes
    for n in [1000usize, 10000] {
        let (nb, p) = lib
            .create_notebook(&format!("strokes-{n}"), PageDefaults::default(), None)
            .unwrap();
        lib.apply_page_operations(
            nb.id,
            p.id,
            &batch(0, OpKind::AddElements { elements: scene(n) }),
        )
        .unwrap();
        export(&mut lib, nb.id, out.join(format!("strokes-{n}.penroam")));
    }

    // mixed text + ink on several pages, one A4 grid page, a trashed page
    let (nb, p1) = lib
        .create_notebook("mixed text and ink", PageDefaults::default(), None)
        .unwrap();
    let mut els: Vec<Element> = scene(100).into_iter().take(20).collect();
    els.push(text(100, 400.0, "First block\nwith two lines"));
    els.push(text(101, 500.0, "Unicode: Größe ✓ — 東京"));
    els.push(text(102, 600.0, ""));
    lib.apply_page_operations(
        nb.id,
        p1.id,
        &batch(0, OpKind::AddElements { elements: els }),
    )
    .unwrap();
    let (_, p2) = lib
        .create_page(
            nb.id,
            None,
            Some(Template {
                kind: TemplateKind::Grid,
                spacing: 18.0,
            }),
            Some(Paper::A4),
        )
        .unwrap();
    lib.apply_page_operations(
        nb.id,
        p2.id,
        &batch(
            0,
            OpKind::AddElements {
                elements: vec![text(1, 72.0, "page two")],
            },
        ),
    )
    .unwrap();
    let (_, p3) = lib.create_page(nb.id, Some(0), None, None).unwrap();
    lib.delete_page(nb.id, p3.id).unwrap();
    export(&mut lib, nb.id, out.join("mixed-text.penroam"));

    // embedded PDF with annotations on every page
    let pdf = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/pdf/mixed-sizes-rotations.pdf"),
    )
    .unwrap();
    let (nb, _) = lib
        .create_notebook("pdf", PageDefaults::default(), None)
        .unwrap();
    let asset = lib
        .add_asset(
            nb.id,
            &pdf,
            "application/pdf",
            Some("mixed-sizes-rotations.pdf".into()),
        )
        .unwrap();
    let infos = penroam_pdf::inspect(&pdf).unwrap();
    let specs = infos
        .iter()
        .map(|i| {
            let bg = i.background(&asset.sha256);
            (
                Paper::Pdf,
                bg.display_size(),
                Template {
                    kind: TemplateKind::Blank,
                    spacing: 24.0,
                },
                Some(bg),
            )
        })
        .collect();
    let (_, pages) = lib.create_pages(nb.id, None, specs).unwrap();
    let strokes = scene(100);
    for (i, page) in pages.iter().enumerate() {
        lib.apply_page_operations(
            nb.id,
            page.id,
            &batch(
                0,
                OpKind::AddElements {
                    elements: vec![
                        strokes[i].clone(),
                        text(500, 100.0, &format!("pdf page {i}")),
                    ],
                },
            ),
        )
        .unwrap();
    }
    export(&mut lib, nb.id, out.join("pdf-annotated.penroam"));

    // a library left after a crash: snapshot + unflushed journal batches
    lib.flush_all().unwrap();
    drop(lib);
    let crashed = out.join("crashed-library");
    let mut lib = Library::open(&crashed).unwrap();
    let (nb, p) = lib
        .create_notebook("crashed", PageDefaults::default(), None)
        .unwrap();
    let mut rev = 0;
    for (i, s) in scene(100).into_iter().take(12).enumerate() {
        rev = lib
            .apply_page_operations(
                nb.id,
                p.id,
                &batch(rev, OpKind::AddElements { elements: vec![s] }),
            )
            .unwrap()
            .revision;
        if i == 5 {
            lib.flush_all().unwrap(); // half folded into the snapshot, half only in the journal
        }
    }
    lib.apply_page_operations(
        nb.id,
        p.id,
        &batch(
            rev,
            OpKind::AddElements {
                elements: vec![text(1000, 300.0, "typed after the last checkpoint")],
            },
        ),
    )
    .unwrap();
    lib.crash_for_tests();
    println!("wrote {}", crashed.display());
    let _ = std::fs::remove_dir_all(&tmp);
}
