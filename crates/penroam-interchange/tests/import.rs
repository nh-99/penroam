//! Importers and exporters, with hostile input. The Xournal and Evernote
//! documents here are written by hand from the file formats, so they carry no
//! third-party content.

use flate2::{write::GzEncoder, Compression};
use md5::{Digest, Md5};
use penroam_core::validate::validate_page;
use penroam_core::*;
use penroam_interchange::enex::import_enex;
use penroam_interchange::svg::page_to_svg;
use penroam_interchange::text_export::{export_text, strip_markdown, TextFormat};
use penroam_interchange::text_import::{escape_markdown, import_markdown, import_plain_text};
use penroam_interchange::xournal::import_xournal;
use penroam_interchange::{limits, ImportError, ImportedDocument};
use std::io::Write;
use uuid::Uuid;

fn gz(s: &[u8]) -> Vec<u8> {
    let mut e = GzEncoder::new(Vec::new(), Compression::default());
    e.write_all(s).unwrap();
    e.finish().unwrap()
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::new();
    for c in b.chunks(3) {
        let v = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        o.push(T[(v >> 18) as usize & 63] as char);
        o.push(T[(v >> 12) as usize & 63] as char);
        o.push(if c.len() > 1 {
            T[(v >> 6) as usize & 63] as char
        } else {
            '='
        });
        o.push(if c.len() > 2 {
            T[v as usize & 63] as char
        } else {
            '='
        });
    }
    o
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

fn sample() -> String {
    format!(
        r##"<?xml version="1.0" standalone="no"?>
<xournal creator="Xournal++ 1.2.3" fileversion="4">
<title>Xournal++ document</title>
<page width="595.27" height="841.89">
<background type="solid" color="#ffffffff" style="lined"/>
<layer>
<stroke tool="pen" color="#3344ccff" width="2.26 1.1 2.2 1.65">10 20 14 22 18 24 22 26</stroke>
<stroke tool="highlighter" color="#ffff00ff" width="8.5">100 100 200 100</stroke>
<stroke tool="pen" color="red" width="1.41" fill="128">5 5 6 6</stroke>
<stroke tool="eraser" color="white" width="5">1 1 2 2</stroke>
<text font="Sans" size="14" x="72.5" y="90" color="#000000ff">Line &amp; one
# two</text>
<image left="50" top="60" right="150" bottom="160">{}</image>
</layer>
<layer>
<stroke tool="pen" color="#000000ff" width="1.41">1 1 2 2</stroke>
<teximage text="x^2" left="0" top="0" right="10" bottom="10">AAAA</teximage>
</layer>
</page>
<page width="612" height="792">
<background type="pdf" domain="absolute" filename="/etc/passwd" pageno="1"/>
<layer/>
</page>
</xournal>"##,
        base64(&png(4, 4))
    )
}

#[test]
fn xournal_document_keeps_what_it_can_and_says_what_it_cannot() {
    let d = import_xournal(&gz(sample().as_bytes()), "Lecture").unwrap();
    assert_eq!(d.title, "Lecture");
    assert_eq!(d.pages.len(), 2);
    assert_eq!(d.report.kept.pages, 2);
    assert_eq!(d.report.kept.strokes, 5);
    assert_eq!(d.report.kept.text_blocks, 2, "the text and the formula");
    assert_eq!(d.report.kept.images, 1);
    assert_eq!(d.assets.len(), 1);
    assert_eq!(d.assets[0].media_type, "image/png");

    let p = &d.pages[0];
    assert!((p.size.width - 595.27).abs() < 1e-9);
    assert_eq!(p.template.kind, TemplateKind::Ruled);
    let Element::Ink(pen) = &p.elements[0] else {
        panic!()
    };
    assert!(pen.pressure);
    assert_eq!(pen.points.len(), 4);
    assert_eq!(pen.color, "#3344cc");
    // the widest segment hits full pressure, so widths come out as they were
    let w = |p: f64| penroam_core::geometry::width_for(pen.width, p, true);
    assert!((w(pen.points[1].pressure) - 2.2).abs() < 1e-9);
    assert!((w(pen.points[2].pressure) - 1.65).abs() < 1e-9);
    let Element::Ink(hi) = &p.elements[1] else {
        panic!()
    };
    assert_eq!(hi.tool, InkTool::Highlighter);
    assert_eq!(hi.opacity, 0.5);
    let Element::Text(t) = &p.elements[4] else {
        panic!()
    };
    assert_eq!((t.x, t.y, t.font_size), (72.5, 90.0, 14.0));
    assert!(t.text.contains("Line \\& one\n\\# two"), "{:?}", t.text);
    assert_eq!(strip_markdown(&t.text), "Line & one\n# two");

    let lost = d.report.lost.join(" | ");
    for needle in ["fill", "layers", "PDF background", "LaTeX", "font"] {
        assert!(lost.contains(needle), "{needle} missing from {lost}");
    }
    assert!(!lost.contains("/etc/passwd"));
    assert_eq!(d.pages[1].template.kind, TemplateKind::Blank);

    for (i, ip) in d.pages.iter().enumerate() {
        let mut page = Page::new(
            Uuid::new_v4(),
            Paper::Custom,
            ip.size,
            ip.template.clone(),
            0,
        );
        page.elements = ip.elements.clone();
        validate_page(&page).unwrap_or_else(|e| panic!("page {i}: {e}"));
    }
}

#[test]
fn uncompressed_xml_is_accepted() {
    assert_eq!(
        import_xournal(sample().as_bytes(), "x")
            .unwrap()
            .pages
            .len(),
        2
    );
}

#[test]
fn hostile_xournal_files_fail_cleanly() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("garbage", b"\x00\x01garbage".to_vec()),
        ("bad gzip", vec![0x1f, 0x8b, 8, 0, 1, 2, 3, 4]),
        ("not xournal", b"<html><body/></html>".to_vec()),
        ("no pages", b"<xournal></xournal>".to_vec()),
        ("cut off", b"<xournal><page width=\"10\" height=\"10\"><layer>".to_vec()),
        ("mismatched", b"<xournal><page></xournal>".to_vec()),
        ("huge page", b"<xournal><page width=\"1e9\" height=\"1e9\"></page></xournal>".to_vec()),
        ("nan page", b"<xournal><page width=\"NaN\" height=\"5\"></page></xournal>".to_vec()),
        ("invalid utf8", b"<xournal><page \xff\xfe></page></xournal>".to_vec()),
        (
            "entity bomb",
            br#"<!DOCTYPE x [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;&a;">]>
            <xournal><page width="10" height="10"><layer><text x="1" y="1" size="9">&b;</text></layer></page></xournal>"#.to_vec(),
        ),
        (
            "external entity",
            br#"<!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]>
            <xournal><page width="10" height="10"><layer><text x="1" y="1" size="9">&e;</text></layer></page></xournal>"#.to_vec(),
        ),
    ];
    for (name, bytes) in cases {
        assert!(
            matches!(import_xournal(&bytes, "t"), Err(ImportError::Malformed(_))),
            "{name}"
        );
    }
}

#[test]
fn decompression_bomb_stops_at_the_limit() {
    let mut e = GzEncoder::new(Vec::new(), Compression::fast());
    let zeros = vec![b' '; 1 << 20];
    for _ in 0..=(limits::MAX_XML_BYTES >> 20) {
        e.write_all(&zeros).unwrap();
    }
    let bomb = e.finish().unwrap();
    assert!(bomb.len() < 2 << 20);
    assert!(matches!(
        import_xournal(&bomb, "t"),
        Err(ImportError::TooLarge(_))
    ));
}

#[test]
fn bad_elements_are_dropped_and_counted() {
    let huge = png(1_000_000, 1_000_000);
    let xml = format!(
        r##"<xournal><page width="100" height="100"><layer>
        <stroke tool="pen" color="#000000ff" width="1">1 2 3</stroke>
        <stroke tool="pen" color="#000000ff" width="NaN">1 2 3 4</stroke>
        <stroke tool="pen" color="#000000ff" width="1">1e999 2</stroke>
        <stroke tool="pen" color="#000000ff" width="1">1 2 3 4</stroke>
        <image left="0" top="0" right="1" bottom="1">{}</image>
        <image left="0" top="0" right="1" bottom="1">{}</image>
        <image left="0" top="0" right="1" bottom="1">!!!</image>
        <text x="nope" y="1" size="10">t</text>
        </layer></page></xournal>"##,
        base64(&huge),
        base64(b"<svg onload=alert(1)>")
    );
    let d = import_xournal(xml.as_bytes(), "t").unwrap();
    assert_eq!(d.report.kept.strokes, 1);
    assert_eq!(d.report.kept.images, 0);
    assert!(d.assets.is_empty());
    let lost = d.report.lost.join(" ");
    assert!(lost.contains("3 stroke(s)"), "{lost}");
    assert!(lost.contains("3 image(s)"), "{lost}");
    assert!(lost.contains("1 text block(s)"), "{lost}");
}

#[test]
fn mutated_documents_never_panic() {
    let valid = gz(sample().as_bytes());
    let plain = sample().into_bytes();
    let mut seed = 0x9e3779b97f4a7c15u64;
    for round in 0..4000 {
        let mut b = if round % 2 == 0 {
            valid.clone()
        } else {
            plain.clone()
        };
        for _ in 0..1 + round % 8 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let i = seed as usize % b.len();
            b[i] = (seed >> 32) as u8;
        }
        if let Ok(d) = import_xournal(&b, "t") {
            for ip in &d.pages {
                let mut p = Page::new(
                    Uuid::new_v4(),
                    Paper::Custom,
                    ip.size,
                    ip.template.clone(),
                    0,
                );
                p.elements = ip.elements.clone();
                validate_page(&p).unwrap();
            }
        }
    }
}

#[test]
fn markdown_paginates_and_keeps_every_word() {
    let mut src = String::from("# Title\n\n");
    for i in 0..400 {
        src.push_str(&format!("Paragraph number {i} with a few words in it.\n\n"));
    }
    src.push_str(
        "```\ncode\n\n\nstill code\n```\n\n![pic](../../etc/passwd)\n\n[x](javascript:alert(1))\n",
    );
    let d = import_markdown(src.as_bytes(), "Notes").unwrap();
    assert!(d.pages.len() > 3);
    let all: String = d
        .pages
        .iter()
        .flat_map(|p| &p.elements)
        .map(|e| match e {
            Element::Text(t) => t.text.clone() + "\n\n",
            _ => panic!(),
        })
        .collect();
    for i in 0..400 {
        assert!(all.contains(&format!("number {i} ")));
    }
    assert!(
        all.contains("code\n\n\nstill code"),
        "fenced code stays whole"
    );
    assert!(d.report.lost.iter().any(|l| l.contains("1 image")));
    for ip in &d.pages {
        let Element::Text(t) = &ip.elements[0] else {
            panic!()
        };
        assert!(t.y + t.height <= ip.size.height, "fits the page");
    }
}

#[test]
fn plain_text_reads_back_literally() {
    let src = "# not a heading\n- not a list\n1. nor this\n*stars* _under_ $x$ <b> [a](b)\n";
    let d = import_plain_text(src.as_bytes(), "t").unwrap();
    let Element::Text(t) = &d.pages[0].elements[0] else {
        panic!()
    };
    assert_eq!(strip_markdown(&t.text), src.trim_end());
    assert_eq!(escape_markdown("plain words"), "plain words");
}

#[test]
fn text_encodings_and_odd_input() {
    let mut utf16 = vec![0xff, 0xfe];
    for u in "héllo".encode_utf16() {
        utf16.extend_from_slice(&u.to_le_bytes());
    }
    let d = import_plain_text(&utf16, "t").unwrap();
    let Element::Text(t) = &d.pages[0].elements[0] else {
        panic!()
    };
    assert_eq!(t.text, "héllo");
    // empty file: still one blank page
    assert_eq!(import_markdown(b"", "t").unwrap().pages.len(), 1);
    // one enormous line
    let long = "word ".repeat(200_000);
    let d = import_plain_text(long.as_bytes(), "t").unwrap();
    assert!(d.pages.len() > 10);
    // control bytes go, invalid UTF-8 is reported
    let d = import_plain_text(b"a\x00b\xffc", "t").unwrap();
    assert!(!d.report.lost.is_empty());
    assert!(matches!(
        import_markdown(&vec![b'a'; limits::MAX_TEXT_FILE + 1], "t"),
        Err(ImportError::TooLarge(_))
    ));
}

fn ink_page() -> Page {
    let d = import_xournal(sample().as_bytes(), "t").unwrap();
    let ip = &d.pages[0];
    let mut p = Page::new(
        Uuid::new_v4(),
        Paper::Custom,
        ip.size,
        ip.template.clone(),
        0,
    );
    p.elements = ip.elements.clone();
    p
}

#[test]
fn text_export_warns_about_handwriting() {
    let p = ink_page();
    let r = export_text(
        "Lecture",
        std::slice::from_ref(&p),
        TextFormat::Markdown,
        |_| None,
    );
    assert!(r.text.starts_with("# Lecture\n\n## Page 1\n\n"));
    assert_eq!(r.text_blocks, 2);
    assert_eq!(r.strokes_omitted, 5);
    assert_eq!(r.images_omitted, 1);
    assert!(r.warnings[0].contains("handwritten"));

    let r = export_text("L", std::slice::from_ref(&p), TextFormat::Plain, |_| {
        Some("seen words".into())
    });
    assert!(r.text.contains("seen words"));
    assert_eq!(r.strokes_omitted, 0);
    assert!(r.warnings.iter().any(|w| w.contains("recognition")));
    assert!(r.text.contains("Line & one"));
}

#[test]
fn svg_is_well_formed_and_escaped() {
    let mut p = ink_page();
    p.elements.push(Element::Text(TextBlock {
        id: Uuid::new_v4(),
        z: 99,
        x: 1.0,
        y: 1.0,
        width: 300.0,
        height: 20.0,
        text: "**bold** `<script>alert(1)</script>` \"q\" &".into(),
        font_size: 12.0,
        color: "#000000".into(),
        align: TextAlign::Left,
        font: Default::default(),
        row_pitch: None,
        extra: Default::default(),
    }));
    let image = png(4, 4);
    let r = page_to_svg(&p, |_| Some((image.clone(), "image/png".into())));
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(!r.svg.contains("<script"));
    assert!(r.svg.contains("data:image/png;base64,"));
    assert!(r.svg.contains("viewBox=\"0 0 595.27 841.89\""));
    // well formed: every event parses
    let mut reader = quick_xml_reader(&r.svg);
    while !reader() {}
    // an asset of another type never goes into the file
    let r = page_to_svg(&p, |_| Some((b"<svg/>".to_vec(), "image/svg+xml".into())));
    assert!(!r.svg.contains("data:image/svg"));
    assert!(!r.warnings.is_empty());
}

fn quick_xml_reader(s: &str) -> impl FnMut() -> bool + '_ {
    let mut r = quick_xml::Reader::from_str(s);
    move || matches!(r.read_event().unwrap(), quick_xml::events::Event::Eof)
}

fn jpeg(w: u16, h: u16) -> Vec<u8> {
    let mut b = vec![0xff, 0xd8, 0xff, 0xe0, 0, 2, 0xff, 0xc0, 0, 17, 8];
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&[3, 0, 0, 0]);
    b
}

fn md5_hex(b: &[u8]) -> String {
    Md5::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn enex_note(title: &str, enml: &str, extra: &str) -> String {
    format!(
        "<note><title>{title}</title><content><![CDATA[<?xml version=\"1.0\"?>\n\
         <!DOCTYPE en-note SYSTEM \"http://xml.evernote.com/pub/enml2.dtd\">\n\
         <en-note>{enml}</en-note>]]></content>{extra}</note>"
    )
}

fn enex(notes: &[String]) -> Vec<u8> {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE en-export SYSTEM \"http://xml.evernote.com/pub/evernote-export4.dtd\">\n\
         <en-export export-date=\"20240102T030405Z\" application=\"Evernote\">{}</en-export>",
        notes.concat()
    )
    .into_bytes()
}

fn resource(bytes: &[u8], mime: &str) -> String {
    format!(
        "<resource><data encoding=\"base64\">\n{}\n</data><mime>{mime}</mime>\
         <width>4</width><height>4</height><resource-attributes>\
         <file-name>../../etc/passwd</file-name></resource-attributes></resource>",
        base64(bytes)
    )
}

fn media(bytes: &[u8]) -> String {
    format!("<en-media hash=\"{}\" type=\"image/png\"/>", md5_hex(bytes))
}

fn page_texts(d: &ImportedDocument) -> Vec<String> {
    d.pages
        .iter()
        .map(|p| {
            p.elements
                .iter()
                .filter_map(|e| match e {
                    Element::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .collect()
}

fn one_note(enml: &str) -> (String, ImportedDocument) {
    let d = import_enex(&enex(&[enex_note("T", enml, "")]), "Book").unwrap();
    (page_texts(&d).join("\n\n"), d)
}

/// Every page passes the model's validation, and nothing on it overlaps or
/// leaves the margins.
fn assert_sound(d: &ImportedDocument) {
    for ip in &d.pages {
        let mut p = Page::new(
            Uuid::new_v4(),
            Paper::Custom,
            ip.size,
            ip.template.clone(),
            0,
        );
        p.elements = ip.elements.clone();
        validate_page(&p).unwrap();
        let mut boxes: Vec<(f64, f64)> = ip
            .elements
            .iter()
            .map(|e| match e {
                Element::Text(t) => (t.y, t.y + t.height),
                Element::Image(i) => (i.y, i.y + i.height),
                Element::Ink(_) => panic!("no ink in an Evernote import"),
            })
            .collect();
        boxes.sort_by(|a, b| a.0.total_cmp(&b.0));
        for pair in boxes.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "content overlaps: {boxes:?}");
        }
        for e in &ip.elements {
            if let Element::Image(i) = e {
                assert!(i.x >= 0.0 && i.x + i.width <= ip.size.width);
                assert!(i.y >= 0.0 && i.y + i.height <= ip.size.height);
            }
        }
    }
}

fn enex_sample() -> Vec<u8> {
    let (a, b) = (png(40, 20), jpeg(2000, 1000));
    enex(&[
        enex_note(
            "First &amp; foremost",
            &format!(
                "<h2>Head</h2><div>Plain <b>bold</b> <i>it</i> <u>under</u> &nbsp;x</div>\
                 <ul><li>one</li><li><en-todo checked=\"true\"/>two</li></ul>\
                 <table><tr><th>a</th><td>b</td></tr><tr><td colspan=\"2\">c</td></tr></table>\
                 <a href=\"https://example.org/?a=1&amp;b=2\">site</a>\
                 <a href=\"javascript:alert(1)\">bad</a><pre>code &lt;x&gt;</pre>{}{}{}",
                media(&a),
                media(&b),
                media(b"nothing")
            ),
            &format!(
                "<created>20240102T030405Z</created><updated>20240102T030405Z</updated>\
                 <tag>home</tag><tag>work</tag>\
                 <note-attributes><author>Someone</author></note-attributes>{}{}{}",
                resource(&a, "image/png"),
                resource(&b, "image/jpeg"),
                resource(b"%PDF-1.7", "application/pdf")
            ),
        ),
        enex_note(
            "Second",
            "<div>broken &bogus; here",
            &resource(&a, "image/png"),
        ),
    ])
}

#[test]
fn enex_formatting_comes_through_as_markdown() {
    let (text, d) = one_note(
        "<h2>Head #</h2>\
         <p>one <b>bold </b><i>it<s>both</s></i> <strong><em>x</em></strong></p>\
         <p>*not* 1. <code>a`|b</code> &lt;tag&gt; &amp; &eacute;&#233;&#xe9;&nbsp;&mdash;</p>\
         <div>line one</div><div>line two<br/>line three</div><div><br/></div><div>new para</div>\
         <ul><li>a<ul><li>b</li></ul></li><ul><li>c</li></ul></ul>\
         <ol><li>first</li><li>second</li></ol>\
         <blockquote>said</blockquote><hr/>\
         <pre>let x = 1;\n\n\n```\nend</pre>\
         <table><tr><th>A</th><th>B|C</th></tr><tr><td>1</td><td><b>2</b><br/>3</td></tr>\
         <tr><td>only</td></tr></table>",
    );
    for block in [
        "# T",
        "## Head \\#",
        "one **bold** *it~~both~~ **x***",
        "\\*not\\* 1. ``a`|b`` \\<tag> \\& ééé —",
        "line one\nline two\nline three",
        "new para",
        "- a\n  - b\n  - c",
        "1. first\n2. second",
        "> said",
        "***",
        "````\nlet x = 1;\n\n\n```\nend\n````",
        "| A | B\\|C |\n| --- | --- |\n| 1 | **2** 3 |\n| only |  |",
    ] {
        assert!(
            text.split("\n\n").any(|b| b == block) || text.contains(block),
            "{block:?} missing from {text:?}"
        );
    }
    assert!(strip_markdown(&text).contains("*not* 1. a`|b <tag> & ééé —"));
    assert_eq!(d.report.format, "Evernote");
    assert!(d.report.lost.iter().any(|l| l.contains("1 table(s)")));
    assert_sound(&d);
}

#[test]
fn enex_output_parses_as_the_structure_it_came_from() {
    use pulldown_cmark::{Event, Options, Parser, Tag};
    let (text, _) = one_note(
        "<p><b>bold</b> <i>it</i> <s>gone</s> <a href=\"https://e.org/$1\">l</a> $x$ <b> pad </b>x</p>\
         <div><en-todo/>task</div>\
         <table><tr><td>a</td></tr><tr><td>b</td></tr></table>",
    );
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let (mut strong, mut em, mut strike, mut links, mut tasks, mut tables) = (0, 0, 0, 0, 0, 0);
    for ev in Parser::new_ext(&text, opts) {
        match ev {
            Event::Start(Tag::Strong) => strong += 1,
            Event::Start(Tag::Emphasis) => em += 1,
            Event::Start(Tag::Strikethrough) => strike += 1,
            Event::Start(Tag::Link { dest_url, .. }) => {
                assert_eq!(&*dest_url, "https://e.org/%241");
                links += 1;
            }
            Event::TaskListMarker(false) => tasks += 1,
            Event::Start(Tag::Table(_)) => tables += 1,
            _ => {}
        }
    }
    assert_eq!(
        (strong, em, strike, links, tasks, tables),
        (2, 1, 1, 1, 1, 1)
    );
    // no formula can come out of literal text
    assert!(penroam_core::markdown::math_in(&text).is_empty(), "{text}");
}

#[test]
fn enex_notes_start_pages_and_titles_are_literal() {
    let d = import_enex(
        &enex(&[
            enex_note("# **One** [x](y)", "<div>a</div>", ""),
            enex_note("", "", ""),
            "<note/>".to_string(),
        ]),
        "Book",
    )
    .unwrap();
    assert_eq!(d.title, "Book");
    assert_eq!(d.pages.len(), 3);
    let texts = page_texts(&d);
    assert!(texts[0].starts_with("# \\# \\*\\*One\\*\\* \\[x\\](y)\n\na"));
    assert_eq!(
        strip_markdown(texts[0].lines().next().unwrap()),
        "# **One** [x](y)"
    );
    assert_eq!(texts[1], "# Untitled note");
    assert_eq!(texts[2], "# Untitled note");
    assert_eq!(d.report.kept.pages, 3);
    assert_eq!(d.report.kept.text_blocks, 3);
}

#[test]
fn enex_tasks_become_task_lists() {
    let (text, _) = one_note(
        "<div><en-todo checked=\"true\"/>done</div><div><en-todo/>open</div>\
         <div><en-todo checked=\"false\"></en-todo><b>bold</b></div><div>after</div>\
         <ul><li><en-todo/>in a list</li></ul>before <en-todo checked=\"TRUE\"/>inline\
         <table><tr><td><en-todo/>cell</td></tr></table>",
    );
    assert!(
        text.contains("- [x] done\n- [ ] open\n- [ ] **bold**\n\nafter"),
        "{text}"
    );
    assert!(text.contains("\n\n- [ ] in a list\n\n"), "{text}");
    assert!(text.contains("before\n\n- [x] inline"), "{text}");
    assert!(text.contains("| \\[ \\] cell |"), "{text}");
}

#[test]
fn enex_links_keep_only_safe_addresses() {
    let (text, d) = one_note(
        "<div><a href=\"https://example.org/a(b)?x=1&amp;copy;=2&amp;y=$3 z\"><b>site</b> now</a> \
         <a href=\"MAILTO:me@example.org\">mail</a> <a href=\"http://a.b\"></a>\
         <a href=\"javascript:alert(1)\">js</a> <a href=\" JaVaScRiPt:alert(1)\">js2</a> \
         <a href=\"java&#10;script:alert(1)\">js3</a> <a href=\"data:text/html,x\">data</a> \
         <a href=\"evernote:///view/1\">other note</a> <a href=\"relative\">rel</a> \
         <a name=\"anchor\">named</a></div>",
    );
    assert!(
        text.contains("[**site** now](https://example.org/a%28b%29?x=1\\&copy;=2\\&y=%243%20z)"),
        "{text}"
    );
    assert!(text.contains("[mail](MAILTO:me@example.org)"), "{text}");
    assert!(
        text.ends_with("js js2 js3 data other note rel named"),
        "{text}"
    );
    for bad in ["script:", "data:", "evernote", "relative"] {
        assert!(!text.to_lowercase().contains(bad), "{text}");
    }
    let lost = d.report.lost.join(" ");
    assert!(lost.contains("6 link(s)"), "{lost}");
}

#[test]
fn enex_images_are_matched_by_hash_and_validated() {
    let (small, wide, tall) = (png(40, 20), jpeg(4000, 1000), png(1000, 8000));
    let bad = png(1_000_000, 1_000_000);
    let spoofed = b"<svg onload=alert(1)>".to_vec();
    let notes = [enex_note(
        "Pics",
        &format!(
            "<div>before</div>{}{}<div>middle</div>{}{}{}{}",
            media(&small),
            media(&small),
            media(&tall),
            media(&wide)
                .to_uppercase()
                .replace("EN-MEDIA HASH", "en-media hash"),
            media(&bad),
            media(&spoofed),
        ),
        &[
            resource(&wide, "image/jpeg"),
            resource(&small, "image/png"),
            resource(&small, "image/png"),
            resource(&tall, "image/png"),
            resource(&bad, "image/png"),
            resource(&spoofed, "image/png"),
            resource(&jpeg(10, 10), "image/jpeg"),
        ]
        .concat(),
    )];
    let d = import_enex(&enex(&notes), "t").unwrap();
    assert_sound(&d);
    assert_eq!(d.report.kept.images, 5, "{:?}", d.report);
    assert_eq!(d.assets.len(), 4, "identical images share one asset");
    let mut media_types: Vec<_> = d.assets.iter().map(|a| a.media_type.as_str()).collect();
    media_types.sort();
    assert_eq!(
        media_types,
        ["image/jpeg", "image/jpeg", "image/png", "image/png"]
    );
    let images: Vec<&ImageElement> = d
        .pages
        .iter()
        .flat_map(|p| &p.elements)
        .filter_map(|e| match e {
            Element::Image(i) => Some(i),
            _ => None,
        })
        .collect();
    let sha = |b: &[u8]| {
        d.assets
            .iter()
            .find(|a| a.bytes == b)
            .map(|a| a.sha256.clone())
            .unwrap()
    };
    assert_eq!(images[0].asset_hash, sha(&small));
    assert_eq!((images[0].width, images[0].height), (30.0, 15.0));
    assert_eq!(images[1].asset_hash, sha(&small));
    assert_eq!(images[2].asset_hash, sha(&tall));
    assert_eq!(images[3].asset_hash, sha(&wide));
    assert!(images[3].width <= 504.0 + 1e-9);
    assert_eq!(images[4].width, 7.5, "an image no note refers to is kept");
    let lost = d.report.lost.join(" ");
    assert!(lost.contains("2 image attachment(s)"), "{lost}");
    assert!(lost.contains("moved to the end"), "{lost}");
    assert!(!lost.contains("reference(s)"), "{lost}");
    assert!(!lost.contains("passwd"), "{lost}");
}

#[test]
fn enex_reports_what_it_left_behind() {
    let d = import_enex(&enex_sample(), "t").unwrap();
    assert_sound(&d);
    assert_eq!(d.report.kept.pages as usize, d.pages.len());
    assert_eq!(d.report.kept.text_blocks, 2);
    assert_eq!(d.report.kept.images, 3);
    let lost = d.report.lost.join(" | ");
    for needle in [
        "2 note tag(s)",
        "1 note(s) lost their created and updated dates",
        "1 note(s) lost details",
        "1 attachment(s)",
        "1 reference(s) to attachments missing",
        "1 table(s)",
        "1 note(s) used underlining",
        "1 link(s)",
        "1 note(s) were damaged",
        "moved to the end",
    ] {
        assert!(lost.contains(needle), "{needle} missing from {lost}");
    }
}

#[test]
fn enex_a_reference_to_a_lost_attachment_counts_once() {
    let gif = b"GIF89a".to_vec();
    let d = import_enex(
        &enex(&[enex_note("t", &media(&gif), &resource(&gif, "image/gif"))]),
        "t",
    )
    .unwrap();
    assert_eq!(
        d.report.lost,
        ["1 image attachment(s) were damaged, of an unsupported type or too large, and were left out."]
    );
    assert!(d.assets.is_empty());
    let notes = [enex_note(
        "t",
        "<en-media hash=\"00\"/>",
        "<resource><data>not base64!</data><mime>audio/wav</mime></resource>",
    )];
    let lost = import_enex(&enex(&notes), "t").unwrap().report.lost;
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert!(lost[0].starts_with("1 attachment(s)"));
}

#[test]
fn enex_tolerates_the_html_found_in_the_wild() {
    let (text, d) = one_note(
        "<div>x < y and 1<2 and AT&T rocks; ok</div><div>a<b and c</div>\
         <p>one<p>two<br>three<DIV>a &copy; b &unknown-free R&D</DIV>\
         <ul><li>x<li>y</ul><b>open across</div>next</p></nothing>\
         <table><tr><td>1<td>2<tr><td>3</table>\
         <div unquoted=yes class=\"a\" hidden>attrs</div>",
    );
    assert!(
        !d.report.lost.iter().any(|l| l.contains("damaged")),
        "{:?}",
        d.report.lost
    );
    for block in [
        "x \\< y and 1\\<2 and AT\\&T rocks; ok\na\\<b and c",
        "one",
        "two\nthree\na © b",
        "- x\n- y",
        "| 1 | 2 |\n| --- | --- |\n| 3 |  |",
        "attrs",
    ] {
        assert!(text.contains(block), "{block:?} missing from {text:?}");
    }
    assert!(text.contains("**open across"), "{text}");
}

#[test]
fn enex_script_and_style_contents_are_dropped() {
    let (text, d) = one_note(
        "<div>kept</div><script>if (a<b) steal();</script>\
         <STYLE type=\"text/css\">p > a { color: red }</STYLE><style/>\
         <!-- <script> hidden comment -->\
         <div>also kept</div><en-crypt hint=\"h\" cipher=\"AES\">Q0lQSEVS</en-crypt>\
         <script src=\"x\"/><div>and this</div><script>unterminated",
    );
    assert!(text.contains("kept\nalso kept"), "{text}");
    assert!(text.contains("and this"), "{text}");
    for gone in [
        "steal",
        "color",
        "comment",
        "CIPHER",
        "Q0lQ",
        "unterminated",
    ] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
    assert!(d.report.lost.iter().any(|l| l.contains("1 encrypted")));
}

#[test]
fn enex_malformed_enml_costs_only_that_note() {
    let d = import_enex(&enex_sample(), "t").unwrap();
    let texts = page_texts(&d);
    assert!(texts[0].contains("# First \\& foremost"), "{texts:?}");
    assert!(
        texts[0].contains("[site](https://example.org/?a=1\\&b=2)"),
        "{texts:?}"
    );
    assert!(texts[0].contains(")bad"), "{texts:?}");
    let last = texts.iter().rev().find(|t| t.contains("Second")).unwrap();
    assert!(last.contains("broken \\&bogus; here"), "{last}");

    // a tag that never ends can't be tokenised at all
    let notes = [
        enex_note(
            "Bad",
            "<div>kept <b>words</b> a &lt; b<br/>next\n<a href=\"x",
            "",
        ),
        enex_note("Good", "<b>fine</b>", ""),
    ];
    let d = import_enex(&enex(&notes), "t").unwrap();
    let texts = page_texts(&d);
    assert!(texts[0].contains("kept words a \\< b\n"), "{texts:?}");
    assert!(texts[1].contains("**fine**"), "{texts:?}");
    assert_eq!(
        d.report.lost,
        ["1 note(s) were damaged and were kept as plain text."]
    );
}

#[test]
fn enex_bad_outer_xml_fails_the_file() {
    let good = enex_note("t", "<div/>", "");
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("garbage", b"\x00\x01garbage".to_vec()),
        ("not an export", b"<html><note/></html>".to_vec()),
        ("no notes", b"<en-export></en-export>".to_vec()),
        ("no notes, empty root", b"<en-export/>".to_vec()),
        ("cut off", format!("<en-export>{good}").into_bytes()),
        ("cut off in a note", b"<en-export><note><title>t".to_vec()),
        (
            "invalid utf8",
            b"<en-export><note>\xff</note></en-export>".to_vec(),
        ),
        (
            "unknown entity",
            b"<en-export><note><title>&nbsp;</title></note></en-export>".to_vec(),
        ),
        (
            "unknown entity outside a field",
            b"<en-export><note><source>&nope;</source></note></en-export>".to_vec(),
        ),
        (
            "entity bomb",
            br#"<!DOCTYPE x [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;&a;">]>
            <en-export><note><title>&b;</title></note></en-export>"#
                .to_vec(),
        ),
        (
            "external entity",
            br#"<!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]>
            <en-export><note><content>&e;</content></note></en-export>"#
                .to_vec(),
        ),
    ];
    for (name, bytes) in cases {
        assert!(
            matches!(import_enex(&bytes, "t"), Err(ImportError::Malformed(_))),
            "{name}"
        );
    }
}

#[test]
fn enex_entities_inside_a_note_expand_to_nothing() {
    let (text, d) = one_note(
        "<!DOCTYPE x [<!ENTITY a \"aaaaaaaa\"><!ENTITY e SYSTEM \"file:///etc/passwd\">]>\
         <div>&a;&e;</div>",
    );
    assert!(!text.contains("aaaa"), "{text}");
    assert!(!text.contains("root:"), "{text}");
    assert!(d.report.lost.iter().any(|l| l.contains("plain text")));
    assert_sound(&d);
}

#[test]
fn enex_stops_at_its_limits() {
    let too_big = vec![0u8; limits::MAX_ENEX_BYTES as usize + 1];
    assert!(matches!(
        import_enex(&too_big, "t"),
        Err(ImportError::TooLarge(_))
    ));
    drop(too_big);

    let long = format!(
        "<en-export><note><content>{}</content></note></en-export>",
        "a".repeat(limits::MAX_TEXT_FILE + 1)
    );
    assert!(matches!(
        import_enex(long.as_bytes(), "t"),
        Err(ImportError::TooLarge(_))
    ));

    let notes = vec!["<note/>".to_string(); 10_001];
    assert!(matches!(
        import_enex(&enex(&notes), "t"),
        Err(ImportError::TooLarge(_))
    ));

    let image = png(600, 800);
    let notes = [enex_note(
        "t",
        &media(&image).repeat(10_001),
        &resource(&image, "image/png"),
    )];
    assert!(matches!(
        import_enex(&enex(&notes), "t"),
        Err(ImportError::TooLarge(_))
    ));

    // one huge attachment is lost and counted, and the note is kept
    let huge = format!(
        "<resource><data>{}</data><mime>video/mp4</mime></resource>",
        "A".repeat(limits::MAX_IMAGE_BYTES * 2)
    );
    let d = import_enex(
        &enex(&[enex_note("t", "<en-media hash=\"ab\"/>", &huge)]),
        "t",
    )
    .unwrap();
    assert_eq!(d.report.lost.len(), 1, "{:?}", d.report.lost);
    assert!(d.report.lost[0].starts_with("1 attachment(s)"));
}

#[test]
fn enex_long_code_and_tables_stay_whole_across_pages() {
    let code: String = (0..200)
        .map(|i| format!("line {i} *not markdown*\n\n"))
        .collect();
    let rows: String = (0..200)
        .map(|i| format!("<tr><td>r{i}</td><td>v</td></tr>"))
        .collect();
    let (_, d) = one_note(&format!(
        "<pre>{code}</pre><table><tr><th>Name</th><th>Value</th></tr>{rows}</table>"
    ));
    assert!(d.pages.len() > 3);
    assert_sound(&d);
    let texts = page_texts(&d);
    let all = texts.join("\n");
    for i in 0..200 {
        assert!(all.contains(&format!("line {i} *not markdown*\n")), "{i}");
        assert!(all.contains(&format!("| r{i} | v |")), "{i}");
    }
    for t in &texts {
        assert_eq!(t.matches("```").count() % 2, 0, "balanced fences: {t}");
        if t.contains("| r") {
            assert!(t.contains("| Name | Value |"), "{t}");
        }
    }
}

#[test]
fn enex_hostile_structure_stays_cheap() {
    let deep = "<ul><li><b><table><tr><td><blockquote>".repeat(50_000);
    let (_, d) = one_note(&format!("{deep}x"));
    assert_sound(&d);
    let closes = "</x></div></b>".repeat(500_000);
    let (_, d) = one_note(&format!("{}{closes}", "<div>".repeat(100_000)));
    assert_sound(&d);
    let wide = "<td>c</td>".repeat(100_000);
    let (text, d) = one_note(&format!(
        "<table><tr>{wide}</tr><tr><td>1</td></tr></table>"
    ));
    assert!(
        text.lines().nth(2).unwrap().matches(" --- ").count() <= 32,
        "{}",
        &text[..80]
    );
    assert_sound(&d);
    let digits = format!("<div>{}{}</div>", "1".repeat(1 << 20), ".".repeat(1 << 20));
    let (_, d) = one_note(&digits);
    assert_sound(&d);
}

#[test]
fn enex_mutated_files_never_panic() {
    let valid = enex_sample();
    let mut seed = 0x2545f4914f6cdd1du64;
    for round in 0..4000 {
        let mut b = valid.clone();
        for _ in 0..1 + round % 8 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let i = seed as usize % b.len();
            b[i] = if round % 3 == 0 {
                b"<>&;\"/!-[]"[(seed >> 40) as usize % 10]
            } else {
                (seed >> 32) as u8
            };
        }
        if let Ok(d) = import_enex(&b, "t") {
            assert!(!d.pages.is_empty());
            assert_sound(&d);
        }
    }
}

fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
        h[156] = b'0';
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|b| *b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    out
}

fn joplin_note(id: &str, parent: &str, title: &str, body: &str) -> Vec<u8> {
    format!("{title}\n\n{body}\n\nid: {id}\nparent_id: {parent}\ncreated_time: 2026-01-01T00:00:00.000Z\nis_todo: 0\ntype_: 1").into_bytes()
}

fn sample_jex() -> Vec<u8> {
    let png = png(8, 8);
    tar(&[
        ("aaa.md", b"Work\n\nid: f1\ntype_: 2".as_slice()),
        ("bbb.md", b"Home\n\nid: f2\ntype_: 2".as_slice()),
        (
            "n1.md",
            &joplin_note(
                "n1",
                "f1",
                "Plan",
                "Some **bold** words\n\n![chart](:/r1)\n\nAfter\n\n[other](:/n2)",
            ),
        ),
        ("n2.md", &joplin_note("n2", "f2", "Groceries", "- [ ] milk")),
        (
            "r1.md",
            b"chart.png\n\nid: r1\nmime: image/png\nfile_extension: png\ntype_: 4".as_slice(),
        ),
        (
            "r2.md",
            b"doc.pdf\n\nid: r2\nmime: application/pdf\ntype_: 4".as_slice(),
        ),
        ("resources/r1.png", &png),
        ("resources/r2.pdf", b"%PDF"),
    ])
}

#[test]
fn joplin_export_becomes_pages_with_images_and_a_report() {
    use penroam_interchange::jex::import_jex;
    let d = import_jex(&sample_jex(), "Joplin").unwrap();
    assert_eq!(d.report.format, "Joplin");
    assert_eq!(d.assets.len(), 1);
    assert_eq!(d.report.kept.images, 1);
    let text: Vec<String> = d
        .pages
        .iter()
        .flat_map(|p| &p.elements)
        .filter_map(|e| match e {
            Element::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        text[0].starts_with("# Groceries"),
        "folders sort first: {text:?}"
    );
    assert!(text[0].contains("- [ ] milk"));
    assert!(text[1].contains("Some **bold** words"));
    assert!(
        !text[1].contains("(:/r1)"),
        "the image moved out of the text"
    );
    assert!(text[1].contains("[other](:/n2)"), "note links stay as text");
    assert!(
        !text.iter().any(|t| t.contains("id: n1")),
        "metadata stays out"
    );
    let lost = d.report.lost.join(" | ");
    for needle in [
        "folders",
        "dates",
        "1 attachment",
        "1 image(s) moved",
        "no longer lead",
    ] {
        assert!(lost.contains(needle), "{needle} missing from {lost}");
    }
}

#[test]
fn hostile_joplin_files_fail_cleanly() {
    use penroam_interchange::jex::import_jex;
    assert!(import_jex(b"not a tar at all", "t").is_err());
    assert!(import_jex(&[0u8; 2048], "t").is_err(), "no notes");
    let valid = sample_jex();
    assert!(import_jex(&valid[..700], "t").is_err(), "cut off");
    // a path in an entry name selects nothing outside the table
    let evil = tar(&[
        (
            "../../etc/passwd.md",
            &joplin_note("n", "", "Fine", "text")[..],
        ),
        ("resources/../../x.png", b"x".as_slice()),
    ]);
    assert_eq!(import_jex(&evil, "t").unwrap().pages.len(), 1);
    // a huge declared size can't read past the end
    let mut big = tar(&[("a.md", b"x".as_slice())]);
    big[124..136].copy_from_slice(b"77777777777\0");
    big[148..156].copy_from_slice(b"        ");
    let sum: u32 = big[..512].iter().map(|b| *b as u32).sum();
    big[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    assert!(import_jex(&big, "t").is_err());
}

#[test]
fn mutated_joplin_exports_never_panic() {
    use penroam_interchange::jex::import_jex;
    let valid = sample_jex();
    let mut seed = 0x1234_5678_9abc_def1u64;
    for round in 0..3000 {
        let mut b = valid.clone();
        for _ in 0..1 + round % 8 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let i = seed as usize % b.len();
            b[i] = (seed >> 32) as u8;
        }
        if let Ok(d) = import_jex(&b, "t") {
            for ip in &d.pages {
                let mut p = Page::new(
                    Uuid::new_v4(),
                    Paper::Custom,
                    ip.size,
                    ip.template.clone(),
                    0,
                );
                p.elements = ip.elements.clone();
                validate_page(&p).unwrap();
            }
        }
    }
}

fn packaged(content: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    z.start_file("mimetype", opts).unwrap();
    z.write_all(b"application/xournal++").unwrap();
    z.start_file("content.xml", opts).unwrap();
    z.write_all(content.as_bytes()).unwrap();
    for (name, bytes) in files {
        z.start_file(*name, opts).unwrap();
        z.write_all(bytes).unwrap();
    }
    z.finish().unwrap().into_inner()
}

#[test]
fn packaged_documents_bring_their_attachments() {
    let xml = r##"<xournal fileversion="4">
    <page width="612" height="792"><background type="pdf" domain="attach" filename="attachments/bg.pdf" pageno="1ll"/>
    <layer><image left="10" top="20" right="50" bottom="60"><attachment path="attachments/a.png"/></image>
    <image left="0" top="0" right="1" bottom="1"><attachment path="../../etc/passwd"/></image></layer></page>
    <page width="612" height="792"><background type="pdf" pageno="2ll"/><layer/></page>
    <page width="612" height="792"><background type="solid" color="#ffffffff" style=":copy"/><layer/></page>
    </xournal>"##;
    let png = png(4, 4);
    let bytes = packaged(
        xml,
        &[
            ("attachments/bg.pdf", b"%PDF-1.4 x"),
            ("attachments/a.png", &png),
        ],
    );
    let d = import_xournal(&bytes, "t").unwrap();
    let pdf = d
        .assets
        .iter()
        .find(|a| a.media_type == "application/pdf")
        .unwrap();
    assert_eq!(d.pages[0].pdf, Some((pdf.sha256.clone(), 0)));
    assert_eq!(
        d.pages[1].pdf,
        Some((pdf.sha256.clone(), 1)),
        "no file name repeated"
    );
    assert_eq!(d.pages[2].pdf, None);
    assert_eq!(d.report.kept.images, 1);
    assert!(
        d.report.lost.iter().any(|l| l.contains("1 image(s)")),
        "the path leads nowhere"
    );
}

#[test]
fn matrices_place_elements_and_rotation_is_reported() {
    let image = base64(&png(100, 50));
    let xml = format!(
        r##"<xournal fileversion="5"><page width="612" height="792"><background type="solid" color="#ffffffff" style="plain"/><layer>
        <text font="Sans" size="12" matrix="2 0 0 2 130 96" color="#ff0000ff" wrap="100">wrapped</text>
        <text font="Sans" size="12" matrix="1 0 0 1 10 10" color="#000000ff" align="right">right</text>
        <image matrix="0.5 0 0 0.5 20 30">{image}</image>
        <image matrix="0 1 -1 0 100 100">{image}</image>
        <link align="center" font="Sans" size="12" matrix="1 0 0 1 5 6" color="#ff00ffff" url="https://example.com/a?b=1&amp;c=2">Site
two</link>
        <link font="Sans" size="12" matrix="1 0 0 1 5 6" color="#ff00ffff" url="javascript:alert(1)">bad</link>
        <teximage text="x^2" matrix="1 0 0 1 14 15">AAAA</teximage>
        <teximage text="y" left="1" top="2" right="31" bottom="22">AAAA</teximage>
        </layer></page></xournal>"##
    );
    let d = import_xournal(xml.as_bytes(), "t").unwrap();
    let els = &d.pages[0].elements;
    let Element::Text(t) = &els[0] else { panic!() };
    assert_eq!((t.x, t.y, t.font_size, t.width), (130.0, 96.0, 24.0, 200.0));
    let Element::Text(t) = &els[1] else { panic!() };
    assert_eq!(t.align, TextAlign::Right);
    let Element::Image(i) = &els[2] else { panic!() };
    assert_eq!((i.x, i.y, i.width, i.height), (20.0, 30.0, 50.0, 25.0));
    let Element::Image(i) = &els[3] else { panic!() };
    assert_eq!(
        (i.x, i.y, i.width, i.height),
        (50.0, 100.0, 50.0, 100.0),
        "a quarter turn"
    );
    let Element::Text(t) = &els[4] else { panic!() };
    assert_eq!(t.align, TextAlign::Center);
    assert_eq!(
        t.text,
        "[Site](<https://example.com/a?b=1&c=2>)\n[two](<https://example.com/a?b=1&c=2>)"
    );
    let Element::Text(t) = &els[5] else { panic!() };
    assert_eq!(t.text, "bad", "only web and mail links");
    let Element::Text(t) = &els[6] else { panic!() };
    assert_eq!((t.text.as_str(), t.x), ("$x^2$", 14.0));
    let Element::Text(t) = &els[7] else { panic!() };
    assert_eq!((t.text.as_str(), t.x, t.y), ("$y$", 1.0, 2.0));
    let lost = d.report.lost.join(" | ");
    assert!(lost.contains("1 rotated"), "{lost}");
    assert!(lost.contains("LaTeX formulas"));
}

#[test]
fn enex_tolerates_what_real_exports_contain() {
    let nested = br#"<en-export><note><title>T</title><content><![CDATA[<en-note>one<br/><![CDATA[two]]></en-note>]]></content></note></en-export>"#;
    let d = import_enex(nested, "t").unwrap();
    let Element::Text(t) = &d.pages[0].elements[0] else {
        panic!()
    };
    assert!(
        t.text.contains("one") && t.text.contains("two"),
        "{}",
        t.text
    );

    let amp = br#"<en-export><note><title>A & B</title><content><![CDATA[<en-note>x</en-note>]]></content><note-attributes><source>a & b</source></note-attributes></note></en-export>"#;
    assert_eq!(import_enex(amp, "t").unwrap().pages.len(), 1);

    let stray = br#"<en-export><note><title>T</title><content><![CDATA[<en-note>x</en-note>]]></content></note></stray></en-export>"#;
    assert_eq!(import_enex(stray, "t").unwrap().pages.len(), 1);

    // ending early is still refused
    let cut =
        br#"<en-export><note><title>T</title><content><![CDATA[<en-note>x</en-note>]]></content>"#;
    assert!(import_enex(cut, "t").is_err());
}

#[test]
fn strokes_survive_bad_pressure_samples() {
    let xml = r##"<xournal><page width="100" height="100"><layer>
    <stroke tool="pen" color="#000000ff" width="1.41 0 0.2 nan -nan">1 2 3 4 5 6 7 8 9 10</stroke>
    <stroke tool="pen" color="#000000ff" width="1.41 0 0">1 2 3 4 5 6</stroke>
    </layer></page></xournal>"##;
    let d = import_xournal(xml.as_bytes(), "t").unwrap();
    assert_eq!(d.report.kept.strokes, 2);
    let Element::Ink(s) = &d.pages[0].elements[0] else {
        panic!()
    };
    assert!(s.pressure && s.points[0].pressure == 0.0);
    let Element::Ink(s) = &d.pages[0].elements[1] else {
        panic!()
    };
    assert!(!s.pressure, "nothing to go on, so the nominal width");
    assert_eq!(s.width, 1.41);
}

#[test]
fn joplin_todos_and_tags() {
    use penroam_interchange::jex::import_jex;
    let todo =
        b"Buy milk\n\nid: t\nparent_id: f\nis_todo: 1\ntodo_completed: 0\ntype_: 1".as_slice();
    let done = b"Paid\n\nid: d\nparent_id: f\nis_todo: 1\ntodo_completed: 1793922000000\ntype_: 1"
        .as_slice();
    let tag = b"groceries\n\nid: g\ntype_: 5".as_slice();
    let d = import_jex(&tar(&[("a.md", todo), ("b.md", done), ("c.md", tag)]), "t").unwrap();
    let text = |i: usize| match &d.pages[i].elements[0] {
        Element::Text(t) => t.text.clone(),
        _ => panic!(),
    };
    assert!(text(0).starts_with("- [ ] Buy milk"), "{}", text(0));
    assert!(text(1).starts_with("- [x] Paid"), "{}", text(1));
    assert!(d.report.lost.iter().any(|l| l.contains("1 tag")));
}

#[test]
fn shapes_keep_corners_and_whiteout_stays_white() {
    let xml = r##"<xournal><page width="400" height="400">
    <background type="solid" color="#ffffffff" style="graph"/><layer>
    <stroke tool="pen" color="#000000ff" width="2">10 10 110 10 110 110 10 110 10 10</stroke>
    <stroke tool="pen" color="#000000ff" width="2">1 1 3 2 5 1 7 2</stroke>
    <stroke tool="eraser" color="white" width="10">50 50 60 60</stroke>
    </layer></page></xournal>"##;
    let d = import_xournal(xml.as_bytes(), "t").unwrap();
    assert_eq!(d.pages[0].template.spacing, 14.17);
    let Element::Ink(square) = &d.pages[0].elements[0] else {
        panic!()
    };
    // the drawn path passes within half a point of the corner, not 25 points off it
    let pieces = penroam_core::geometry::stroke_pieces(&square.points, square.width, false);
    let near = pieces.iter().any(|p| match *p {
        penroam_core::geometry::Piece::Quad {
            x0,
            y0,
            cx,
            cy,
            x1,
            y1,
            ..
        } => {
            let (mx, my) = (
                0.25 * x0 + 0.5 * cx + 0.25 * x1,
                0.25 * y0 + 0.5 * cy + 0.25 * y1,
            );
            (mx - 110.0).hypot(my - 10.0) < 0.5
        }
        _ => false,
    });
    assert!(near);
    let Element::Ink(hand) = &d.pages[0].elements[1] else {
        panic!()
    };
    assert_eq!(hand.points.len(), 4, "handwriting samples aren't touched");
    let Element::Ink(white) = &d.pages[0].elements[2] else {
        panic!()
    };
    assert_eq!(white.color, "#ffffff");
}

fn sample_pdf() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/pdf/mixed-sizes-rotations.pdf"
    ))
    .unwrap()
}

#[test]
fn enex_pdf_attachments_become_pages_after_their_note() {
    let pdf = sample_pdf();
    let count = penroam_pdf::inspect(&pdf).unwrap().len();
    let xml = format!(
        r#"<en-export>
        <note><title>First</title><content><![CDATA[<en-note>words</en-note>]]></content>
        <resource><data encoding="base64">{}</data><mime>application/pdf</mime></resource></note>
        <note><title>Second</title><content><![CDATA[<en-note>more</en-note>]]></content>
        <resource><data encoding="base64">{}</data><mime>application/pdf</mime></resource></note>
        </en-export>"#,
        base64(&pdf),
        base64(b"%PDF-1.4 not really a pdf"),
    );
    let d = import_enex(xml.as_bytes(), "t").unwrap();
    assert_eq!(
        d.pages.len(),
        1 + count + 1,
        "second note after the PDF's pages"
    );
    assert!(d.pages[0].pdf.is_none());
    let hash = &d.assets[0].sha256;
    for i in 0..count {
        assert_eq!(d.pages[1 + i].pdf, Some((hash.clone(), i as u32)));
    }
    assert!(d.pages[1 + count].pdf.is_none());
    assert_eq!(d.assets.len(), 1);
    let lost = d.report.lost.join(" | ");
    assert!(lost.contains("became pages"), "{lost}");
    assert!(
        lost.contains("1 attachment(s)"),
        "the damaged PDF is reported: {lost}"
    );
}

#[test]
fn joplin_pdf_attachments_become_pages() {
    use penroam_interchange::jex::import_jex;
    let pdf = sample_pdf();
    let count = penroam_pdf::inspect(&pdf).unwrap().len();
    let jex = tar(&[
        ("n.md", &joplin_note("n", "", "Paper", "[paper.pdf](:/r9)")),
        (
            "r.md",
            b"paper.pdf\n\nid: r9\nmime: application/pdf\ntype_: 4".as_slice(),
        ),
        ("resources/r9.pdf", &pdf),
    ]);
    let d = import_jex(&jex, "t").unwrap();
    assert_eq!(d.pages.len(), 1 + count);
    assert!(d.pages[1].pdf.is_some());
    assert!(!d
        .report
        .lost
        .join(" ")
        .contains("attachment(s) were not imported"));
}
