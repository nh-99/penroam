//! The Windows handwriting recognizer on real strokes. Runs only on Windows with the
//! `windows-ink` feature, and skips (with a message) when no recognizer is installed, as on
//! most CI images.
#![cfg(all(windows, feature = "windows-ink"))]

use penroam_core::*;
use penroam_search::index::Recognition;
use penroam_search::windows_ink::WindowsInkRecognizer;
use penroam_search::*;
use uuid::Uuid;

/// A polyline stroke through `pts` (page points), sampled every ~1 pt, 8 ms per segment.
fn polyline(id: u128, pts: &[(f64, f64)], t0: i64) -> Element {
    let mut points = Vec::new();
    let mut t = t0;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let n = ((b.0 - a.0).hypot(b.1 - a.1)).ceil().max(1.0) as usize;
        for i in 0..n {
            let f = i as f64 / n as f64;
            points.push(InkPoint {
                x: a.0 + (b.0 - a.0) * f,
                y: a.1 + (b.1 - a.1) * f,
                pressure: 0.6,
                tilt_x: 0.0,
                tilt_y: 0.0,
                time_us: t,
                twist: None,
                flags: None,
            });
            t += 8000 / n as i64;
        }
    }
    let last = *pts.last().unwrap();
    points.push(InkPoint {
        x: last.0,
        y: last.1,
        pressure: 0.6,
        tilt_x: 0.0,
        tilt_y: 0.0,
        time_us: t,
        twist: None,
        flags: None,
    });
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

#[test]
fn recognizes_block_letters_and_maps_strokes_back() {
    let Some(mut rec) = WindowsInkRecognizer::new() else {
        eprintln!("skipped: no Windows handwriting recognizer installed");
        return;
    };
    let lang = rec
        .info()
        .languages
        .iter()
        .find(|l| l.contains("English"))
        .cloned()
        .unwrap_or_else(|| rec.info().languages[0].clone());
    let mut p = Page::new(
        Uuid::from_u128(1),
        Paper::Letter,
        Paper::Letter.size().unwrap(),
        Template::default(),
        0,
    );
    let (x, y, h) = (72.0, 100.0, 24.0);
    let s = 1_000_000;
    // "LIT"
    p.elements
        .push(polyline(1, &[(x, y), (x, y + h), (x + 14.0, y + h)], 0));
    p.elements
        .push(polyline(2, &[(x + 24.0, y), (x + 24.0, y + h)], s));
    p.elements
        .push(polyline(3, &[(x + 34.0, y), (x + 52.0, y)], 2 * s));
    p.elements
        .push(polyline(4, &[(x + 43.0, y), (x + 43.0, y + h)], 3 * s));
    // "TILL" on the next line
    let y2 = y + 60.0;
    p.elements
        .push(polyline(5, &[(x, y2), (x + 18.0, y2)], 4 * s));
    p.elements
        .push(polyline(6, &[(x + 9.0, y2), (x + 9.0, y2 + h)], 5 * s));
    p.elements
        .push(polyline(7, &[(x + 28.0, y2), (x + 28.0, y2 + h)], 6 * s));
    p.elements.push(polyline(
        8,
        &[(x + 38.0, y2), (x + 38.0, y2 + h), (x + 52.0, y2 + h)],
        7 * s,
    ));
    p.elements.push(polyline(
        9,
        &[(x + 60.0, y2), (x + 60.0, y2 + h), (x + 74.0, y2 + h)],
        8 * s,
    ));
    p.revision = 1;

    let regions = penroam_search::segment::regions(&p);
    assert_eq!(regions.len(), 2);
    let t = std::time::Instant::now();
    let first = rec.recognize(&regions[0], &lang).unwrap();
    eprintln!("line 1 -> {first:?} in {:?}", t.elapsed());
    let text: String = first
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.eq_ignore_ascii_case("lit"), "read {text:?}");
    let ids: Vec<Uuid> = first.iter().flat_map(|c| c.stroke_ids.clone()).collect();
    assert_eq!(ids.len(), 4, "every stroke maps back to its Penroam id");
    let second = rec.recognize(&regions[1], &lang).unwrap();
    eprintln!("line 2 -> {second:?}");
    let text: String = second
        .iter()
        .map(|c| c.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.eq_ignore_ascii_case("till"), "read {text:?}");

    // end to end through the index
    let mut nb = Notebook::new(
        Uuid::from_u128(9),
        "Ink".into(),
        PageDefaults::default(),
        "#3b5b7a".into(),
        0,
    );
    nb.page_order.push(p.id);
    let mut ix = SearchIndex::open_in_memory().unwrap();
    ix.sync_notebook(&nb).unwrap();
    ix.index_page(
        nb.id,
        0,
        &p,
        Some(Recognition {
            recognizer: &mut rec,
            language: &lang,
        }),
        &|| false,
    )
    .unwrap();
    let hits = ix.search("till", 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].match_bounds.len(), 1);
}
