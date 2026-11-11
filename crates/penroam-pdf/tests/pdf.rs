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
        font: Default::default(),
        row_pitch: None,
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

    let (bytes, report) =
        export_annotated(&pages, &[], |h| (h == hash).then(|| pdf.clone())).unwrap();
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
fn keep_pages_leaves_only_the_chosen_pages() {
    let source = make_pdf(
        &[(612.0, 792.0, 0), (400.0, 500.0, 0), (300.0, 300.0, 0)],
        true,
    );
    let infos = inspect(&source).unwrap();
    let hash = penroam_storage_free_sha(&source);
    let pages: Vec<Page> = infos
        .iter()
        .map(|info| pdf_page(info, &hash, vec![text(50.0, 50.0, "kept note")]))
        .collect();
    let (pdf, _) = export_annotated(&pages, &[], |_| Some(source.clone())).unwrap();

    // out of order and repeated indexes, and one past the end, still give document order
    let cut = keep_pages(&pdf, &[2, 0, 2, 9]).unwrap();
    let doc = Document::load_mem(&cut).unwrap();
    assert_eq!(doc.get_pages().len(), 2);
    assert!(doc.extract_text(&[1]).unwrap().contains("Source page 1"));
    assert!(doc.extract_text(&[2]).unwrap().contains("Source page 3"));
    assert_eq!(inspect(&cut).unwrap()[1].media_box, infos[2].media_box);

    // what was cut is gone from the file, not just unlinked
    let streams = |d: &Document| d.objects.values().filter(|o| o.as_stream().is_ok()).count();
    assert!(streams(&doc) < streams(&Document::load_mem(&pdf).unwrap()));

    assert!(matches!(keep_pages(&pdf, &[]), Err(PdfError::NoPages)));
    assert!(matches!(keep_pages(&pdf, &[3]), Err(PdfError::NoPages)));
    assert!(matches!(keep_pages(b"junk", &[0]), Err(PdfError::Parse(_))));
}

/// A blank page holding one text block.
fn text_page(text: &str, row_pitch: Option<f64>) -> Page {
    let mut element = self::text(40.0, 48.0, text);
    if let Element::Text(t) = &mut element {
        t.width = 400.0;
        t.row_pitch = row_pitch;
    }
    let mut page = Page::new(
        Uuid::new_v4(),
        Paper::Letter,
        Size {
            width: 612.0,
            height: 792.0,
        },
        Template::default(),
        0,
    );
    page.elements.push(element);
    page
}

fn content_of(pages: &[Page], math: &[RenderedMath]) -> (String, ExportReport) {
    let (bytes, report) = export_annotated(pages, math, |_| None).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let id = *doc.get_pages().values().next().unwrap();
    let content = doc.get_page_content(id);
    (String::from_utf8_lossy(&content).into_owned(), report)
}

#[test]
fn markdown_uses_the_matching_standard_fonts() {
    let page = text_page("# Title\n\n**b** *i* ***bi*** `c` plain", None);
    let (bytes, _) = export_annotated(&[page], &[], |_| None).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let id = *doc.get_pages().values().next().unwrap();
    let content = String::from_utf8_lossy(&doc.get_page_content(id)).into_owned();
    for font in ["PRHelv ", "PRHelvB ", "PRHelvI ", "PRHelvBI ", "PRCour "] {
        assert!(content.contains(&format!("/{font}")), "{font} in {content}");
    }
    assert!(content.contains("(Title) Tj"));
    assert!(!content.contains('#'), "the heading marker is not text");
    let fonts = doc.get_page_fonts(id).unwrap();
    assert_eq!(fonts.len(), 5);
    let base = |n: &[u8]| {
        fonts[n]
            .get(b"BaseFont")
            .unwrap()
            .as_name()
            .unwrap()
            .to_vec()
    };
    assert_eq!(base(b"PRHelvBI"), b"Helvetica-BoldOblique");
    assert_eq!(base(b"PRCour"), b"Courier");
}

#[test]
fn handwritten_text_embeds_excalifont_and_draws_its_bold_and_italic() {
    let mut page = text_page("- **b** *i* `c`\n- [x] [link](x)", None);
    if let Element::Text(t) = &mut page.elements[0] {
        t.font = TextFont::Handwritten;
    }
    let (bytes, report) = export_annotated(&[page], &[], |_| None).unwrap();
    assert_eq!(report.replaced_characters, 0);
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let id = *doc.get_pages().values().next().unwrap();
    let content = String::from_utf8_lossy(&doc.get_page_content(id)).into_owned();
    // the bullet is WinAnsi 0x95 in the handwritten font
    assert!(content.contains("/PRHand 16 Tf 1 0 0 -1"), "{content}");
    assert!(content.contains("(\\225) Tj"), "{content}");
    assert!(
        content.contains("2 Tr /PRHand"),
        "bold is stroked: {content}"
    );
    assert!(
        content.contains("1 0 0.2 -1"),
        "italic is sheared: {content}"
    );
    assert!(content.contains("/PRCour"), "code stays monospace");
    let fonts = doc.get_page_fonts(id).unwrap();
    let hand = fonts[b"PRHand".as_slice()];
    assert_eq!(
        hand.get(b"Subtype").unwrap().as_name().unwrap(),
        b"TrueType"
    );
    let descriptor = doc
        .get_dictionary(hand.get(b"FontDescriptor").unwrap().as_reference().unwrap())
        .unwrap();
    let file = descriptor
        .get(b"FontFile2")
        .unwrap()
        .as_reference()
        .unwrap();
    let ttf = doc.get_object(file).unwrap().as_stream().unwrap();
    assert_eq!(&ttf.decompressed_content().unwrap()[..4], &[0, 1, 0, 0]);
}

#[test]
fn formatted_only_exports_leave_the_handwritten_font_out() {
    let (bytes, _) = export_annotated(&[text_page("plain", None)], &[], |_| None).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let id = *doc.get_pages().values().next().unwrap();
    assert!(!doc
        .get_page_fonts(id)
        .unwrap()
        .contains_key(b"PRHand".as_slice()));
}

#[test]
fn row_locked_text_lands_on_the_rows() {
    let (content, _) = content_of(&[text_page("one\ntwo", Some(24.0))], &[]);
    assert!(content.contains("1 0 0 -1 40 72 Tm (one) Tj"), "{content}");
    assert!(content.contains("1 0 0 -1 40 96 Tm (two) Tj"), "{content}");
}

#[test]
fn formulas_are_filled_outlines_from_the_table() {
    let math = [RenderedMath {
        tex: "x".into(),
        display: false,
        width: 1.0,
        height: 0.5,
        depth: 0.1,
        path: "M 0 0 L 1 0 C 1 -0.25 0.5 -0.5 0 -0.5 Z".into(),
    }];
    let (content, _) = content_of(&[text_page("a $x$ b", None)], &math);
    // 16 pt: "a " is 13.344 wide and the first baseline is 15.52 down
    assert!(content.contains("53.344 63.52 m"), "{content}");
    assert!(content.contains("69.344 63.52 l"), "{content}");
    assert!(
        content.contains("69.344 59.52 61.344 55.52 53.344 55.52 c"),
        "{content}"
    );
    assert!(content.contains("h\nf"), "{content}");
    assert!(!content.contains('$'), "{content}");
}

#[test]
fn a_formula_outside_the_table_is_written_as_source() {
    let (content, report) = content_of(&[text_page("$x$", None)], &[]);
    assert!(content.contains("/PRCour"));
    assert!(content.contains("($x$) Tj"), "{content}");
    assert_eq!(report.replaced_characters, 0);
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
    let (bytes, _) = export_annotated(&[page], &[], |_| Some(pdf.clone())).unwrap();
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

#[test]
fn page_texts_follow_page_order_and_reject_garbage() {
    let pdf = make_pdf(
        &[(612.0, 792.0, 0), (595.0, 842.0, 90), (300.0, 300.0, 0)],
        true,
    );
    let texts = page_texts(&pdf).unwrap();
    assert_eq!(texts.len(), inspect(&pdf).unwrap().len());
    for (i, t) in texts.iter().enumerate() {
        assert!(
            t.contains(&format!("Source page {}", i + 1)),
            "page {i}: {t:?}"
        );
    }
    assert!(page_texts(b"not a pdf").is_err());
}

// ---------------------------------------------------------------- images --

const PIXEL_RED: [u8; 3] = [200, 30, 40];

fn image(hash: &str, x: f64, y: f64, width: f64, height: f64) -> Element {
    Element::Image(ImageElement {
        id: Uuid::new_v4(),
        z: 1,
        x,
        y,
        width,
        height,
        asset_hash: hash.into(),
        pdf_page: None,
        extra: Default::default(),
    })
}

fn plain_page(elements: Vec<Element>) -> Page {
    let mut page = Page::new(
        Uuid::new_v4(),
        Paper::Letter,
        Size {
            width: 612.0,
            height: 792.0,
        },
        Template::default(),
        0,
    );
    page.elements = elements;
    page
}

fn png_of(
    width: u32,
    height: u32,
    colour: png::ColorType,
    depth: png::BitDepth,
    pixels: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(colour);
    encoder.set_depth(depth);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(pixels).unwrap();
    writer.finish().unwrap();
    out
}

fn red_png(width: u32, height: u32) -> Vec<u8> {
    let pixels = PIXEL_RED.repeat((width * height) as usize);
    png_of(
        width,
        height,
        png::ColorType::Rgb,
        png::BitDepth::Eight,
        &pixels,
    )
}

/// A PNG that declares a size but holds no image data.
fn png_header_only(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    drop(encoder.write_header().unwrap());
    out
}

/// One JPEG marker segment.
fn segment(marker: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![0xff, marker];
    out.extend(((body.len() + 2) as u16).to_be_bytes());
    out.extend(body);
    out
}

/// The headers of a JPEG with a stub scan: enough for the exporter, which never decodes one.
fn jpeg_with(sof: u8, precision: u8, components: u8, size: (u16, u16), extra: &[u8]) -> Vec<u8> {
    let mut frame = vec![precision];
    frame.extend(size.1.to_be_bytes());
    frame.extend(size.0.to_be_bytes());
    frame.push(components);
    for c in 0..components {
        frame.extend([c + 1, 0x11, 0]);
    }
    let mut out = vec![0xff, 0xd8];
    out.extend(extra);
    out.extend(segment(sof, &frame));
    out.extend(segment(0xda, &[1, 1, 0, 0, 63, 0]));
    out.extend([0x12, 0x34, 0xff, 0xd9]);
    out
}

fn jpeg(components: u8, extra: &[u8]) -> Vec<u8> {
    jpeg_with(0xc0, 8, components, (640, 480), extra)
}

/// An APP1 segment holding an EXIF block with the given orientation.
fn exif(orientation: u16, little: bool) -> Vec<u8> {
    let u16b = |v: u16| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let u32b = |v: u32| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let mut body = b"Exif\0\0".to_vec();
    body.extend(if little { b"II*\0" } else { b"MM\0*" });
    body.extend(u32b(8));
    body.extend(u16b(1)); // one directory entry
    body.extend(u16b(0x0112)); // orientation
    body.extend(u16b(3)); // SHORT
    body.extend(u32b(1));
    body.extend(u16b(orientation));
    body.extend([0, 0]);
    body.extend(u32b(0));
    segment(0xe1, &body)
}

fn hash_of(n: u8) -> String {
    format!("{n:0>64x}")
}

fn export_with(pages: &[Page], assets: &[(&str, Vec<u8>)]) -> (Vec<u8>, ExportReport) {
    export_annotated(pages, &[], |h| {
        assets
            .iter()
            .find(|(hash, _)| *hash == h)
            .map(|(_, bytes)| bytes.clone())
    })
    .unwrap()
}

fn first_content(doc: &Document) -> String {
    let id = *doc.get_pages().values().next().unwrap();
    String::from_utf8_lossy(&doc.get_page_content(id)).into_owned()
}

/// The stream a page's `/XObject` resource points to.
fn xobject<'a>(doc: &'a Document, page: u32, resource: &str) -> (lopdf::ObjectId, &'a Stream) {
    let page = doc.get_dictionary(doc.get_pages()[&page]).unwrap();
    let id = page
        .get_deref(b"Resources", doc)
        .and_then(Object::as_dict)
        .and_then(|r| r.get_deref(b"XObject", doc))
        .and_then(Object::as_dict)
        .and_then(|x| x.get(resource.as_bytes()))
        .and_then(Object::as_reference)
        .unwrap();
    (id, doc.get_object(id).unwrap().as_stream().unwrap())
}

fn image_count(doc: &Document) -> usize {
    doc.objects
        .values()
        .filter_map(|o| o.as_stream().ok())
        .filter(|s| s.dict.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Image"))
        .count()
}

fn name_of<'a>(stream: &'a Stream, key: &[u8]) -> &'a [u8] {
    stream.dict.get(key).unwrap().as_name().unwrap()
}

#[test]
fn image_lands_in_its_box_between_ink_and_text() {
    let hash = hash_of(1);
    // written out of paint order on purpose
    let page = plain_page(vec![
        text(40.0, 300.0, "Caption"),
        image(&hash, 30.0, 40.0, 100.0, 50.0),
        stroke(&[(10.0, 10.0), (20.0, 20.0)], "#000000", InkTool::Pen),
    ]);
    let (bytes, report) = export_with(&[page], &[(&hash, red_png(3, 2))]);
    assert_eq!(
        (report.images, report.strokes, report.text_blocks),
        (1, 1, 1)
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let doc = Document::load_mem(&bytes).unwrap();
    let content = first_content(&doc);
    assert!(content.contains("1 0 0 -1 0 792 cm"), "{content}");
    // the unit square's top edge (v = 1) goes to y = 40, its bottom edge to y = 90
    let draw = content
        .find("q 100 0 0 -50 30 90 cm /PRim0 Do Q")
        .expect(&content);
    assert!(content.find(" S\n").unwrap() < draw, "ink under the image");
    assert!(draw < content.find("BT").unwrap(), "text over the image");

    let (_, stream) = xobject(&doc, 1, "PRim0");
    assert_eq!(name_of(stream, b"Filter"), b"FlateDecode");
    assert_eq!(name_of(stream, b"ColorSpace"), b"DeviceRGB");
    assert_eq!(stream.dict.get(b"Width").unwrap().as_i64().unwrap(), 3);
    assert_eq!(stream.dict.get(b"Height").unwrap().as_i64().unwrap(), 2);
    assert_eq!(stream.decompressed_content().unwrap(), PIXEL_RED.repeat(6));
    assert!(!stream.dict.has(b"SMask"));
}

#[test]
fn image_on_a_rotated_pdf_page_is_placed_in_display_coordinates() {
    let pdf = make_pdf(&[(300.0, 200.0, 90)], false);
    let hash = penroam_storage_free_sha(&pdf);
    let info = inspect(&pdf).unwrap();
    let picture = hash_of(2);
    let page = pdf_page(
        &info[0],
        &hash,
        vec![image(&picture, 20.0, 30.0, 60.0, 40.0)],
    );
    let m = page.background.as_ref().unwrap().doc_to_pdf_matrix();
    let (bytes, report) = export_with(&[page], &[(&hash, pdf.clone()), (&picture, red_png(2, 2))]);
    assert_eq!(report.images, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let doc = Document::load_mem(&bytes).unwrap();
    let content = first_content(&doc);
    assert!(content.contains("Source page 1"), "source content kept");
    let outer = m.map(|v| v.to_string()).join(" ");
    let cm = content.find(&format!("{outer} cm")).expect(&content);
    let draw = content
        .find("q 60 0 0 -40 20 70 cm /PRim0 Do Q")
        .expect(&content);
    assert!(cm < draw, "drawn inside the page's display transform");
    let _ = xobject(&doc, 1, "PRim0");

    // The real test of the matrices: does a renderer put it there, on the rotated page?
    if let Some((dims, px)) = rasterise(&bytes) {
        assert_eq!(dims, [200, 300]);
        let (r, g, b) = px(50, 50);
        assert!(r > 150 && g < 80 && b < 80, "image at (50,50): {r},{g},{b}");
        let (r, g, b) = px(50, 150);
        assert!(r > 200 && g > 200 && b > 200, "paper below: {r},{g},{b}");
    }
}

/// Renders page one at 72 dpi with poppler: its size and a pixel lookup. None if not installed.
#[allow(clippy::type_complexity)]
fn rasterise(pdf: &[u8]) -> Option<([usize; 2], Box<dyn Fn(usize, usize) -> (u8, u8, u8)>)> {
    let dir = std::env::temp_dir().join(format!("penroam-pdf-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.pdf"), pdf).unwrap();
    let status = std::process::Command::new("pdftoppm")
        .args(["-r", "72"])
        .arg(dir.join("a.pdf"))
        .arg(dir.join("out"))
        .status();
    if status.is_err() {
        let _ = std::fs::remove_dir_all(dir);
        return None;
    }
    assert!(status.unwrap().success());
    let ppm = std::fs::read(dir.join("out-1.ppm")).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    let start = ppm
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(2)
        .unwrap()
        .0
        + 1;
    let header = String::from_utf8_lossy(&ppm[..start]).into_owned();
    let dims: Vec<usize> = header
        .split_whitespace()
        .skip(1)
        .take(2)
        .map(|v| v.parse().unwrap())
        .collect();
    let width = dims[0];
    Some((
        [dims[0], dims[1]],
        Box::new(move |x, y| {
            let i = start + (y * width + x) * 3;
            (ppm[i], ppm[i + 1], ppm[i + 2])
        }),
    ))
}

#[test]
fn png_alpha_becomes_a_soft_mask() {
    let hash = hash_of(3);
    let rgba = [255, 0, 0, 128, 0, 255, 0, 0];
    let bytes = png_of(2, 1, png::ColorType::Rgba, png::BitDepth::Eight, &rgba);
    let page = plain_page(vec![image(&hash, 0.0, 0.0, 10.0, 10.0)]);
    let (pdf, report) = export_with(&[page], &[(&hash, bytes)]);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let doc = Document::load_mem(&pdf).unwrap();
    let (_, stream) = xobject(&doc, 1, "PRim0");
    assert_eq!(
        stream.decompressed_content().unwrap(),
        [255, 0, 0, 0, 255, 0]
    );
    let mask = stream.dict.get(b"SMask").unwrap().as_reference().unwrap();
    let mask = doc.get_object(mask).unwrap().as_stream().unwrap();
    assert_eq!(name_of(mask, b"Subtype"), b"Image");
    assert_eq!(name_of(mask, b"ColorSpace"), b"DeviceGray");
    assert_eq!(name_of(mask, b"Filter"), b"FlateDecode");
    assert_eq!(mask.decompressed_content().unwrap(), [128, 0]);
}

#[test]
fn every_png_colour_type_decodes_to_eight_bit_rgb() {
    use png::{BitDepth::*, ColorType::*};
    let gray16 = png_of(1, 1, Grayscale, Sixteen, &[0x80, 0xff]);
    let gray_alpha = png_of(1, 1, GrayscaleAlpha, Eight, &[7, 255]);
    let rgb16 = png_of(1, 1, Rgb, Sixteen, &[1, 0, 2, 0, 3, 0]);
    // fully opaque alpha adds nothing, so it gets no mask
    let opaque = png_of(1, 1, Rgba, Eight, &[9, 8, 7, 255]);
    let mut indexed = Vec::new();
    let mut encoder = png::Encoder::new(&mut indexed, 2, 1);
    encoder.set_color(Indexed);
    encoder.set_depth(Eight);
    encoder.set_palette(vec![10, 20, 30, 40, 50, 60]);
    encoder.set_trns(vec![255, 0]);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[0, 1]).unwrap();
    writer.finish().unwrap();

    let cases: [(Vec<u8>, Vec<u8>, bool); 5] = [
        (gray16, vec![0x80; 3], false),
        (gray_alpha, vec![7; 3], false),
        (rgb16, vec![1, 2, 3], false),
        (opaque, vec![9, 8, 7], false),
        (indexed, vec![10, 20, 30, 40, 50, 60], true),
    ];
    for (i, (bytes, expected, masked)) in cases.into_iter().enumerate() {
        let hash = hash_of(i as u8);
        let page = plain_page(vec![image(&hash, 0.0, 0.0, 10.0, 10.0)]);
        let (pdf, report) = export_with(&[page], &[(&hash, bytes)]);
        assert!(
            report.warnings.is_empty(),
            "case {i}: {:?}",
            report.warnings
        );
        let doc = Document::load_mem(&pdf).unwrap();
        let (_, stream) = xobject(&doc, 1, "PRim0");
        assert_eq!(stream.decompressed_content().unwrap(), expected, "case {i}");
        assert_eq!(stream.dict.has(b"SMask"), masked, "case {i}");
    }
}

#[test]
fn jpeg_keeps_its_bytes_and_colour_space() {
    let adobe = segment(0xee, b"Adobe\0\x64\0\0\0\0\0");
    let cases = [
        (1, Vec::new(), &b"DeviceGray"[..], false),
        (3, Vec::new(), b"DeviceRGB", false),
        (4, Vec::new(), b"DeviceCMYK", false),
        (4, adobe, b"DeviceCMYK", true),
    ];
    for (i, (components, extra, colour, inverted)) in cases.into_iter().enumerate() {
        let original = jpeg(components, &extra);
        let hash = hash_of(i as u8);
        let page = plain_page(vec![image(&hash, 5.0, 5.0, 64.0, 48.0)]);
        let (pdf, report) = export_with(&[page], &[(&hash, original.clone())]);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.images, 1);
        let doc = Document::load_mem(&pdf).unwrap();
        let (_, stream) = xobject(&doc, 1, "PRim0");
        assert_eq!(name_of(stream, b"Filter"), b"DCTDecode");
        assert_eq!(stream.content, original, "bytes untouched");
        assert_eq!(name_of(stream, b"ColorSpace"), colour);
        assert_eq!(stream.dict.get(b"Width").unwrap().as_i64().unwrap(), 640);
        assert_eq!(stream.dict.get(b"Height").unwrap().as_i64().unwrap(), 480);
        assert_eq!(stream.dict.has(b"Decode"), inverted);
    }
}

#[test]
fn jpeg_exif_orientation_turns_the_placement_not_the_bytes() {
    // every matrix sends the stored top-left corner (0, 1) where EXIF says it belongs,
    // in a box at (10, 20) sized 40 x 80
    let cases = [
        (1, "40 0 0 -80 10 100"),
        (2, "-40 0 0 -80 50 100"),
        (3, "-40 0 0 80 50 20"),
        (4, "40 0 0 80 10 20"),
        (5, "0 80 -40 0 50 20"),
        (6, "0 80 40 0 10 20"),
        (7, "0 -80 40 0 10 100"),
        (8, "0 -80 -40 0 50 100"),
        // out of range: treated as upright
        (9, "40 0 0 -80 10 100"),
    ];
    for (orientation, matrix) in cases {
        for little in [true, false] {
            let original = jpeg(3, &exif(orientation, little));
            let hash = hash_of(1);
            let page = plain_page(vec![image(&hash, 10.0, 20.0, 40.0, 80.0)]);
            let (pdf, _) = export_with(&[page], &[(&hash, original.clone())]);
            let doc = Document::load_mem(&pdf).unwrap();
            let content = first_content(&doc);
            assert!(
                content.contains(&format!("q {matrix} cm /PRim0 Do Q")),
                "orientation {orientation}: {content}"
            );
            assert_eq!(xobject(&doc, 1, "PRim0").1.content, original);
        }
    }
}

#[test]
fn an_image_is_embedded_once_however_often_it_is_used() {
    let (a, b) = (hash_of(4), hash_of(5));
    let pages = [
        plain_page(vec![
            image(&a, 0.0, 0.0, 10.0, 10.0),
            image(&b, 0.0, 20.0, 10.0, 10.0),
            image(&a, 0.0, 40.0, 10.0, 10.0),
        ]),
        plain_page(vec![image(&a, 5.0, 5.0, 20.0, 20.0)]),
    ];
    let mut reads = Vec::new();
    let (pdf, report) = export_annotated(&pages, &[], |h| {
        reads.push(h.to_string());
        Some(if h == a { red_png(4, 4) } else { red_png(2, 2) })
    })
    .unwrap();
    assert_eq!(report.images, 4);
    assert_eq!(reads.len(), 2, "each asset read once: {reads:?}");

    let doc = Document::load_mem(&pdf).unwrap();
    assert_eq!(image_count(&doc), 2);
    assert_eq!(xobject(&doc, 1, "PRim0").0, xobject(&doc, 2, "PRim0").0);
    let content = first_content(&doc);
    assert!(content.contains("/PRim1 Do"), "{content}");
    assert_eq!(content.matches("/PRim0 Do").count(), 2, "{content}");

    // printing one page keeps its image and drops the other page's
    let kept = keep_pages(&pdf, &[1]).unwrap();
    let doc = Document::load_mem(&kept).unwrap();
    assert_eq!(image_count(&doc), 1);
    assert_eq!(
        name_of(xobject(&doc, 1, "PRim0").1, b"Filter"),
        b"FlateDecode"
    );
}

#[test]
fn a_reimported_export_keeps_its_own_images() {
    let (old, new) = (hash_of(6), hash_of(7));
    let page = plain_page(vec![image(&old, 0.0, 0.0, 100.0, 100.0)]);
    let (source, _) = export_with(&[page], &[(&old, red_png(2, 2))]);
    let source_hash = penroam_storage_free_sha(&source);
    let info = inspect(&source).unwrap();
    let page = pdf_page(
        &info[0],
        &source_hash,
        vec![image(&new, 0.0, 200.0, 100.0, 100.0)],
    );
    let (pdf, report) = export_with(
        &[page],
        &[(&source_hash, source.clone()), (&new, red_png(5, 5))],
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    let doc = Document::load_mem(&pdf).unwrap();
    let width = |r| {
        let dict = &xobject(&doc, 1, r).1.dict;
        dict.get(b"Width").unwrap().as_i64().unwrap()
    };
    assert_eq!(width("PRim0"), 2, "the source page's image is untouched");
    assert_eq!(width("PRim1"), 5);
    assert!(first_content(&doc).contains("/PRim1 Do"));
}

#[test]
fn unusable_images_warn_and_the_export_still_succeeds() {
    let good = red_png(2, 2);
    let mut truncated = red_png(50, 50);
    truncated.truncate(truncated.len() - 30);
    let cases: Vec<(&str, Option<Vec<u8>>)> = vec![
        ("missing", None),
        ("damaged", Some(b"\x89PNG\r\n\x1a\n not really".to_vec())),
        ("damaged", Some(truncated)),
        ("damaged", Some(vec![0xff, 0xd8, 0xff])),
        ("damaged", Some(jpeg_with(0xc0, 8, 3, (0, 10), &[]))),
        ("not a PNG or JPEG", Some(b"GIF89a".to_vec())),
        ("100 megapixel", Some(png_header_only(20_000, 20_000))),
        ("100 megapixel", Some(png_header_only(u32::MAX >> 1, 3))),
        (
            "100 megapixel",
            Some(jpeg_with(0xc0, 8, 3, (u16::MAX, u16::MAX), &[])),
        ),
        ("JPEG variant", Some(jpeg_with(0xc9, 8, 3, (10, 10), &[]))),
        ("JPEG variant", Some(jpeg_with(0xc3, 8, 3, (10, 10), &[]))),
        ("JPEG variant", Some(jpeg_with(0xc1, 12, 3, (10, 10), &[]))),
    ];
    for (reason, bytes) in cases {
        let (bad, fine) = (hash_of(8), hash_of(9));
        let pages = [
            plain_page(vec![]),
            plain_page(vec![
                image(&bad, 0.0, 0.0, 10.0, 10.0),
                image(&bad, 0.0, 20.0, 10.0, 10.0),
                image(&fine, 0.0, 40.0, 10.0, 10.0),
            ]),
            plain_page(vec![image(&bad, 0.0, 0.0, 10.0, 10.0)]),
        ];
        let (pdf, report) = export_annotated(&pages, &[], |h| {
            if h == fine {
                Some(good.clone())
            } else {
                bytes.clone()
            }
        })
        .unwrap();
        assert_eq!(report.images, 1, "{reason}");
        assert_eq!(report.pages, 3);
        assert_eq!(report.warnings.len(), 1, "{reason}: {:?}", report.warnings);
        let warning = &report.warnings[0];
        assert!(warning.starts_with("Page 2: "), "{warning}");
        assert!(warning.contains(reason), "{reason}: {warning}");
        let doc = Document::load_mem(&pdf).unwrap();
        assert_eq!(image_count(&doc), 1);
    }
}

#[test]
fn cut_off_images_never_panic() {
    let exif_jpeg = jpeg(3, &exif(6, true));
    let mut indexed = Vec::new();
    let mut encoder = png::Encoder::new(&mut indexed, 4, 4);
    encoder.set_color(png::ColorType::Indexed);
    encoder.set_palette(vec![1, 2, 3]);
    let mut writer = encoder.write_header().unwrap();
    writer.write_image_data(&[0; 16]).unwrap();
    writer.finish().unwrap();
    let hash = hash_of(1);
    for whole in [exif_jpeg, indexed, red_png(4, 4)] {
        for len in 0..whole.len() {
            let page = plain_page(vec![image(&hash, 0.0, 0.0, 10.0, 10.0)]);
            let (_, report) = export_with(&[page], &[(&hash, whole[..len].to_vec())]);
            assert!(report.images <= 1, "len {len}");
        }
    }
}

#[test]
fn pdf_pages_on_a_canvas_are_drawn_from_their_source_under_the_ink() {
    // two source pages, the second turned a quarter
    let src = make_pdf(&[(300.0, 400.0, 0), (300.0, 400.0, 90)], false);
    let hash = hash_of(7);
    let infos = inspect(&src).unwrap();
    let el = |i: usize, x: f64, y: f64| {
        let bg = infos[i].background(&hash);
        let size = bg.display_size();
        Element::Image(ImageElement {
            id: Uuid::new_v4(),
            z: -5,
            x,
            y,
            width: size.width,
            height: size.height,
            asset_hash: hash.clone(),
            pdf_page: Some(bg),
            extra: Default::default(),
        })
    };
    let mut page = plain_page(vec![
        stroke(&[(20.0, 20.0), (80.0, 80.0)], "#111111", InkTool::Pen),
        el(0, -100.0, -50.0),
        el(1, -100.0, 400.0),
    ]);
    page.id = Uuid::from_u128(9);
    // ruled paper would be a column; grid paper is cut to the content
    page.template.kind = TemplateKind::Grid;
    let sheet = page.canvas_as_sheet(CANVAS_SHEET_MARGIN_PT);
    // cut to the content plus the margin, on whole template pitches
    assert_eq!(sheet.elements.len(), 3);
    let pitch = page.template.spacing;
    let x0 = ((-100.0 - CANVAS_SHEET_MARGIN_PT) / pitch).floor() * pitch;
    assert!(sheet.size.width >= 300.0 + 2.0 * CANVAS_SHEET_MARGIN_PT);
    let Element::Image(first) = &sheet.elements[1] else {
        panic!()
    };
    assert_eq!(first.x, -100.0 - x0);

    let (bytes, report) = export_with(&[sheet], &[(hash.as_str(), src)]);
    // a PDF page is not counted or warned about as an image
    assert_eq!(report.images, 0);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let doc = Document::load_mem(&bytes).unwrap();
    let content = first_content(&doc);
    // both source pages are form XObjects, drawn before the ink
    let first_do = content.find("/PRpg0 Do").unwrap();
    assert!(content.contains("/PRpg1 Do"));
    let ink = content.find("0.067 0.067 0.067 RG").unwrap();
    assert!(first_do < ink, "{content}");
    let (_, form) = xobject(&doc, 1, "PRpg0");
    assert_eq!(name_of(form, b"Subtype"), b"Form");
    let text = String::from_utf8_lossy(&form.decompressed_content().unwrap()).into_owned();
    assert!(text.contains("Source page 1"), "{text}");
}

#[test]
fn later_pages_of_a_ruled_column_are_ruled_from_the_top() {
    let t = Template::default();
    let first_rule = |continued| {
        let mut s = String::new();
        template_content(&mut s, &t, 612.0, 792.0, continued);
        let line = s.lines().find(|l| l.ends_with(" l S")).unwrap().to_owned();
        line.split(' ').nth(1).unwrap().parse::<f64>().unwrap()
    };
    // a first page has the header rows; the pages after carry on the rules from the one before
    assert_eq!(first_rule(false), t.spacing * 3.0);
    assert_eq!(first_rule(true), t.spacing);
}
