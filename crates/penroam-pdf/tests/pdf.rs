use lopdf::{Dictionary, Document, Object, Stream};
use penroam_core::*;
use penroam_pdf::*;
use uuid::Uuid;

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

/// Builds a PDF whose pages are (width, height, rotate). The first two pages
/// inherit MediaBox/Rotate from the page tree to exercise inheritance.
pub fn make_pdf(pages: &[(f64, f64, i64)], inherit_first: bool) -> Vec<u8> {
    let mut doc = Document::with_version("1.5");
    let root = doc.new_object_id();
    let font = doc.add_object(Dictionary::from_iter(vec![
        ("Type", name("Font")),
        ("Subtype", name("Type1")),
        ("BaseFont", name("Courier")),
    ]));
    let mut kids = vec![];
    for (i, (w, h, rot)) in pages.iter().enumerate() {
        let content = format!(
            "q 0.9 0.2 0.2 rg 10 10 50 50 re f Q BT /F1 18 Tf 72 {} Td (Source page {}) Tj ET",
            h - 72.0,
            i + 1
        );
        let c = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let mut d = Dictionary::from_iter(vec![
            ("Type", name("Page")),
            ("Parent", Object::Reference(root)),
            ("Contents", Object::Reference(c)),
            (
                "Resources",
                Object::Dictionary(Dictionary::from_iter(vec![(
                    "Font",
                    Object::Dictionary(Dictionary::from_iter(vec![(
                        "F1",
                        Object::Reference(font),
                    )])),
                )])),
            ),
        ]);
        if !(inherit_first && i == 0) {
            d.set(
                "MediaBox",
                Object::Array(vec![
                    0.into(),
                    0.into(),
                    Object::Real(*w as f32),
                    Object::Real(*h as f32),
                ]),
            );
            d.set("Rotate", Object::Integer(*rot));
        }
        if i == 1 {
            d.set(
                "CropBox",
                Object::Array(vec![
                    10.into(),
                    20.into(),
                    Object::Real((*w - 10.0) as f32),
                    Object::Real((*h - 20.0) as f32),
                ]),
            );
        }
        kids.push(Object::Reference(doc.add_object(d)));
    }
    let mut pd = Dictionary::from_iter(vec![
        ("Type", name("Pages")),
        ("Count", Object::Integer(kids.len() as i64)),
        ("Kids", Object::Array(kids)),
    ]);
    if inherit_first {
        let (w, h, rot) = pages[0];
        pd.set(
            "MediaBox",
            Object::Array(vec![
                0.into(),
                0.into(),
                Object::Real(w as f32),
                Object::Real(h as f32),
            ]),
        );
        pd.set("Rotate", Object::Integer(rot));
    }
    doc.objects.insert(root, Object::Dictionary(pd));
    let cat = doc.add_object(Dictionary::from_iter(vec![
        ("Type", name("Catalog")),
        ("Pages", Object::Reference(root)),
    ]));
    doc.trailer.set("Root", Object::Reference(cat));
    let mut buf = Vec::new();
    doc.save_to(&mut buf).unwrap();
    buf
}

fn stroke(points: &[(f64, f64)], color: &str, tool: InkTool) -> Element {
    Element::Ink(InkStroke {
        id: Uuid::new_v4(),
        z: 1,
        tool,
        color: color.into(),
        opacity: if tool == InkTool::Highlighter {
            0.35
        } else {
            1.0
        },
        width: if tool == InkTool::Highlighter {
            14.0
        } else {
            3.0
        },
        pressure: tool == InkTool::Pen,
        source: "fixture".into(),
        input: Default::default(),
        points: points
            .iter()
            .enumerate()
            .map(|(i, (x, y))| InkPoint {
                x: *x,
                y: *y,
                pressure: 0.3 + 0.05 * i as f64,
                tilt_x: 0.0,
                tilt_y: 0.0,
                time_us: i as i64 * 4000,
                twist: None,
                flags: None,
            })
            .collect(),
        extra: Default::default(),
    })
}

fn text(x: f64, y: f64, s: &str) -> Element {
    Element::Text(TextBlock {
        id: Uuid::new_v4(),
        z: 2,
        x,
        y,
        width: 200.0,
        height: 40.0,
        text: s.into(),
        font_size: 16.0,
        color: "#1d4ed8".into(),
        align: TextAlign::Left,
        extra: Default::default(),
    })
}

#[test]
fn inspect_reads_boxes_rotation_and_inheritance() {
    let pdf = make_pdf(
        &[
            (612.0, 792.0, 90),
            (595.0, 842.0, 0),
            (300.0, 200.0, 180),
            (400.0, 500.0, 270),
            (200.0, 200.0, -90),
        ],
        true,
    );
    let info = inspect(&pdf).unwrap();
    assert_eq!(info.len(), 5);
    assert_eq!(info[0].media_box, [0.0, 0.0, 612.0, 792.0]);
    assert_eq!(info[0].rotation, 90, "inherited from the page tree");
    assert_eq!(info[1].crop_box, [10.0, 20.0, 585.0, 822.0]);
    assert_eq!(info[2].rotation, 180);
    assert_eq!(info[3].rotation, 270);
    assert_eq!(info[4].rotation, 270, "-90 normalises to 270");
    let bg = info[0].background(&"a".repeat(64));
    assert_eq!(
        (bg.display_size().width, bg.display_size().height),
        (792.0, 612.0)
    );
}

#[test]
fn rejects_garbage() {
    assert!(inspect(b"definitely not a pdf").is_err());
    assert!(inspect(b"%PDF-1.4\n%%EOF").is_err());
}

fn pdf_page(info: &PdfPageInfo, hash: &str, elements: Vec<Element>) -> Page {
    let bg = info.background(hash);
    let mut p = Page::new(
        Uuid::new_v4(),
        Paper::Pdf,
        bg.display_size(),
        Template {
            kind: TemplateKind::Blank,
            spacing: 24.0,
        },
        0,
    );
    p.background = Some(bg);
    p.elements = elements;
    p
}

#[test]
fn annotated_export_keeps_source_pages_and_adds_vector_annotations() {
    let pdf = make_pdf(
        &[
            (612.0, 792.0, 0),
            (595.0, 842.0, 0),
            (300.0, 200.0, 90),
            (400.0, 500.0, 180),
            (300.0, 400.0, 270),
        ],
        false,
    );
    let hash = penroam_storage_free_sha(&pdf);
    let info = inspect(&pdf).unwrap();
    let mut pages: Vec<Page> = info
        .iter()
        .map(|i| {
            pdf_page(
                i,
                &hash,
                vec![
                    stroke(
                        &[(20.0, 20.0), (60.0, 40.0), (100.0, 30.0), (140.0, 60.0)],
                        "#e11d48",
                        InkTool::Pen,
                    ),
                    text(30.0, 80.0, "Note on page (é) ✓"),
                ],
            )
        })
        .collect();
    let mut blank = Page::new(
        Uuid::new_v4(),
        Paper::Letter,
        Paper::Letter.size().unwrap(),
        Template::default(),
        0,
    );
    blank.elements = vec![
        stroke(
            &[(72.0, 72.0), (300.0, 72.0)],
            "#facc15",
            InkTool::Highlighter,
        ),
        stroke(&[(100.0, 100.0)], "#000000", InkTool::Pen),
    ];
    pages.insert(2, blank);

    let (bytes, report) = export_annotated(&pages, |h| (h == hash).then(|| pdf.clone())).unwrap();
    assert_eq!(report.pages, 6);
    assert_eq!(report.strokes, 7);
    assert_eq!(report.text_blocks, 5);
    assert_eq!(report.replaced_characters, 5, "✓ is not in WinAnsi; é is");
    assert!(!report.warnings.is_empty());

    let out = Document::load_mem(&bytes).unwrap();
    let ids: Vec<_> = out.get_pages().values().copied().collect();
    assert_eq!(ids.len(), 6);
    let order_src = [0usize, 1, usize::MAX, 2, 3, 4];
    for (k, id) in ids.iter().enumerate() {
        let d = out.get_dictionary(*id).unwrap();
        let content = String::from_utf8_lossy(&out.get_page_content(*id)).to_string();
        if order_src[k] == usize::MAX {
            assert!(
                content.contains("1 0 0 -1 0 792 cm"),
                "blank page uses the y-flip matrix"
            );
            assert!(content.contains("/PRgs350 gs"), "highlighter alpha");
            continue;
        }
        let src = &info[order_src[k]];
        // geometry preserved exactly
        let mb: Vec<f32> = d
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap())
            .collect();
        assert_eq!(
            mb.iter().map(|v| *v as f64).collect::<Vec<_>>(),
            src.media_box.to_vec()
        );
        let rot = d.get(b"Rotate").map(|o| o.as_i64().unwrap()).unwrap_or(0);
        assert_eq!(rot as u32, src.rotation);
        // original content kept, then ours, with the page's matrix
        assert!(
            content.contains(&format!("Source page {}", src.index + 1)),
            "original content stream kept"
        );
        let m = src.background(&hash).doc_to_pdf_matrix();
        let fmt = |v: f64| {
            let s = format!("{v:.3}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        let cm = format!(
            "{} {} {} {} {} {} cm",
            fmt(m[0]),
            fmt(m[1]),
            fmt(m[2]),
            fmt(m[3]),
            fmt(m[4]),
            fmt(m[5])
        );
        assert!(content.contains(&cm), "page {k}: expected {cm}");
        assert!(
            content.contains("(Note on page \\(\\351\\) ?) Tj"),
            "text with WinAnsi é and replaced ✓: {content}"
        );
        let res = out
            .dereference(d.get(b"Resources").unwrap())
            .unwrap()
            .1
            .as_dict()
            .unwrap();
        let fonts = out
            .dereference(res.get(b"Font").unwrap())
            .unwrap()
            .1
            .as_dict()
            .unwrap();
        assert!(
            fonts.has(b"F1") && fonts.has(b"PRHelv"),
            "source fonts kept, ours added"
        );
    }
}

#[test]
fn render_with_pdftoppm_if_available() {
    // Visual smoke test: rasterise the export and check that annotation pixels
    // land where expected. Skipped when poppler is not installed.
    if std::process::Command::new("pdftoppm")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("pdftoppm not installed; skipping");
        return;
    }
    let pdf = make_pdf(&[(300.0, 200.0, 90)], false);
    let hash = penroam_storage_free_sha(&pdf);
    let info = inspect(&pdf).unwrap();
    // displayed page is 200 x 300; draw a thick stroke near the display top-left
    let page = pdf_page(
        &info[0],
        &hash,
        vec![stroke(
            &[(10.0, 10.0), (40.0, 10.0), (70.0, 10.0)],
            "#0000ff",
            InkTool::Pen,
        )],
    );
    let (bytes, _) = export_annotated(&[page], |_| Some(pdf.clone())).unwrap();
    let dir = std::env::temp_dir().join(format!("penroam-pdf-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.pdf"), &bytes).unwrap();
    let st = std::process::Command::new("pdftoppm")
        .args(["-r", "72"])
        .arg(dir.join("a.pdf"))
        .arg(dir.join("out"))
        .status()
        .unwrap();
    assert!(st.success());
    let ppm = std::fs::read(dir.join("out-1.ppm")).unwrap();
    // parse binary PPM (P6)
    let header_end = ppm
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(2)
        .unwrap()
        .0
        + 1;
    let header = String::from_utf8_lossy(&ppm[..header_end]).to_string();
    let dims: Vec<usize> = header
        .split_whitespace()
        .skip(1)
        .take(2)
        .map(|v| v.parse().unwrap())
        .collect();
    assert_eq!(dims, vec![200, 300], "rendered in display orientation");
    let px = |x: usize, y: usize| {
        let i = header_end + (y * dims[0] + x) * 3;
        (ppm[i], ppm[i + 1], ppm[i + 2])
    };
    let (r, g, b) = px(40, 10);
    assert!(
        b > 200 && r < 80 && g < 80,
        "blue ink at display (40,10), got {r},{g},{b}"
    );
    let (r, g, b) = px(40, 60);
    assert!(
        r > 200 && g > 200 && b > 200,
        "background elsewhere, got {r},{g},{b}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

fn penroam_storage_free_sha(b: &[u8]) -> String {
    // tests don't depend on penroam-storage; any 64-hex id works as a key
    let mut h: u64 = 1469598103934665603;
    for x in b {
        h = (h ^ *x as u64).wrapping_mul(1099511628211);
    }
    format!("{h:016x}").repeat(4)
}
