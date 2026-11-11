//! Markdown layout rules, and the golden file the TypeScript port is checked
//! against (`apps/desktop-mobile/tests/markdown.test.ts`). Run with
//! `UPDATE_GOLDEN=1` after changing the layout on purpose.

use penroam_core::markdown::{extract_math, layout, MathBox, Paint, MAX_DEPTH};
use penroam_core::model::{TextAlign, TextFont};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Every formula is 2 em wide, except `\wide`.
fn math(tex: &str, display: bool) -> Option<MathBox> {
    (!tex.contains("bad")).then(|| MathBox {
        width: if tex.contains("wide") { 40.0 } else { 2.0 },
        height: if display { 1.5 } else { 0.8 },
        depth: if display { 0.6 } else { 0.25 },
    })
}

fn plain(text: &str, width: f64) -> Paint {
    layout(
        text,
        TextFont::Formatted,
        16.0,
        width,
        None,
        TextAlign::Left,
        &math,
    )
}

fn rows(text: &str, width: f64) -> Paint {
    layout(
        text,
        TextFont::Formatted,
        16.0,
        width,
        Some(24.0),
        TextAlign::Left,
        &math,
    )
}

fn texts(p: &Paint) -> Vec<&str> {
    p.runs.iter().map(|r| r.text.as_str()).collect()
}

/// Exercises every construct and the places the two parsers could differ.
const CORPUS: &[&str] = &[
    "",
    "   ",
    "plain text",
    "line one\nline two",
    "first\n\nsecond\n\n\n\nthird",
    "a long paragraph that has to wrap because the box is not nearly wide enough for all of it",
    "unbreakableunbreakableunbreakableunbreakableunbreakableunbreakableunbreakable word",
    "# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six\nbody",
    "Setext\n===\n\nOther\n---",
    "*em* **strong** ***both*** _em_ __strong__ ~~gone~~ `code`",
    "**bold *nested* bold** and *italic **nested** italic*",
    "[a link](https://example.com \"title\") and ![an image](x.png)",
    "[ref][1]\n\n[1]: https://example.com",
    "hard  \nbreak\\\nagain",
    "\\*not em\\* \\$5 &amp; &copy; &#65;",
    "- one\n- two\n  - nested\n    - deeper\n- three",
    "1. one\n2. two\n\n   more of two\n3. three",
    "7. seven\n8. eight",
    "* loose\n\n* items",
    "- [ ] todo\n- [x] done\n- plain",
    "> quoted\n> text\n>\n> > nested quote",
    "> - list in quote\n>\n> ```\n> code in quote\n> ```",
    "```rust\nfn main() {\n\tprintln!(\"hi\");\n}\n```\nafter",
    "~~~\ntilde fence\n~~~",
    "    indented code\n\n    more",
    "---\n\n***",
    "| a | b |\n|---|:-:|\n| 1 | 2 |\n| longer cell text | **x** |",
    "| left | right |\n|:---|---:|\n| one | two |",
    "| a much longer header that will not fit | b |\n|---|---|\n| and a very long cell that must wrap around too | c |",
    "<div>\nraw *html*\n</div>\n\ntext <b>inline</b> html",
    "inline $x^2$ math and $\\frac{a}{b}$ again",
    "$$E = mc^2$$",
    "before\n$$\na + b\n$$\nafter",
    "$$\\sum_i i$$ trailing",
    "a formula $\\wide$ wider than the box",
    "$$\\wide$$",
    "$\\bad$ and $$\\bad$$",
    "$5 and $10, $ x$, $x $, 3$ and `$not$` code",
    "```\n$not math$\n```",
    "    $indented$",
    "- $x$ in a list\n- **$y$** in bold",
    "| $x$ | b |\n|---|---|\n| c | $$y$$ |",
    "`` ` `` and ``` `` ```",
    "tab\there",
    "Ünïcödé € “quotes” — 日本語",
    // Found by running both ports over random documents.
    "~single~ and a~~inside~~word and é~not~ and ~a~1",
    "- [x]\n- [ ] \n- [x] \n1. [ ]\n   text",
    "- [x]",
    "  <!-- indented --> html\n\n  </b> \n   y",
    "trailing tab\t \nnext",
    "1. ## heading in item\n# \nafter an empty heading",
    // spreadsheet tables
    "| Item | Qty | Price | Total |\n|---|--:|--:|--:|\n| Apples | 3 | $0.50 | =B2*C2 |\n| **Sum** | =SUM(B2:B2) | | =SUM(D2:D2) |",
    "| Done | Task |\n|:-:|---|\n| [x] | ship |\n| [ ] | =A2*2 |\n| [X] | x \\| y |\n|[ ]|[x] late|",
    "| =A2*B2*2 | 3 |\n|---|---:|\n| 2 | [ ] |\n| =1/0 | =A9 |",
    "| Price | Total |\n|---|--:|\n| 5 € | **=A2*2** |\n| ($3.00) | *~~=A3+1~~* |\n| **$ 12** | _=SUM(A2:A4)_ |",
    "- | a | b |\n  |---|---|\n  | 2 | =A2^2 |\n\n> | $x$ | =B1 |\n> |---|---|\n> | [ ] | `=1` |",
];

fn golden_cases() -> Vec<Value> {
    let mut out = Vec::new();
    for text in CORPUS {
        for (font, fs, width, row, align) in [
            (TextFont::Formatted, 16.0, 280.0, None, TextAlign::Left),
            (
                TextFont::Formatted,
                16.0,
                280.0,
                Some(24.0),
                TextAlign::Left,
            ),
            (
                TextFont::Formatted,
                22.0,
                120.0,
                Some(20.0),
                TextAlign::Center,
            ),
            (TextFont::Formatted, 12.0, 90.0, None, TextAlign::Right),
            (TextFont::Handwritten, 16.0, 280.0, None, TextAlign::Left),
            (
                TextFont::Handwritten,
                22.0,
                120.0,
                Some(20.0),
                TextAlign::Center,
            ),
        ] {
            let paint = layout(text, font, fs, width, row, align, &math);
            out.push(json!({
                "text": text, "font": font, "fontSize": fs, "width": width, "rowPitch": row,
                "align": align, "paint": paint,
            }));
        }
    }
    out
}

#[test]
fn golden_layout() {
    let actual = Value::Array(golden_cases());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/markdown-layout-golden.json");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !path.exists() {
        let compact: Vec<String> = actual
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        std::fs::write(&path, format!("[\n{}\n]\n", compact.join(",\n"))).unwrap();
    }
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(golden, actual);
}

#[test]
fn plain_notes_keep_their_lines() {
    assert_eq!(plain("a\nb\nc", 100.0).height, 60.0);
    assert_eq!(plain("a\n\nb", 100.0).height, 60.0);
    assert_eq!(plain("", 100.0).height, 20.0);
    assert_eq!(texts(&plain("a\nb", 100.0)), ["a", "b"]);
}

#[test]
fn inline_styles_pick_faces() {
    use penroam_core::text::Face;
    let p = plain("a **b** *c* ***d*** `e`", 400.0);
    let faces: Vec<Face> = p.runs.iter().map(|r| r.face).collect();
    assert!(faces.contains(&Face::Bold));
    assert!(faces.contains(&Face::Italic));
    assert!(faces.contains(&Face::BoldItalic));
    assert!(faces.contains(&Face::Mono));
}

fn hand(text: &str, width: f64) -> Paint {
    layout(
        text,
        TextFont::Handwritten,
        16.0,
        width,
        None,
        TextAlign::Left,
        &math,
    )
}

#[test]
fn handwritten_text_uses_the_hand_faces_but_code_stays_mono() {
    use penroam_core::text::Face;
    let p = hand("a **b** *c* ***d*** `e`", 400.0);
    let faces: Vec<Face> = p.runs.iter().map(|r| r.face).collect();
    for f in [
        Face::Hand,
        Face::HandBold,
        Face::HandItalic,
        Face::HandBoldItalic,
        Face::Mono,
    ] {
        assert!(faces.contains(&f), "{f:?} in {faces:?}");
    }
    assert!(!faces.contains(&Face::Regular));
}

#[test]
fn handwritten_lists_keep_their_markers_in_the_hand_font() {
    use penroam_core::text::Face;
    let bullets = hand("- one\n  - two\n1. three", 300.0);
    for marker in ["\u{2022}", "\u{2013}", "1."] {
        let run = bullets.runs.iter().find(|r| r.text == marker).unwrap();
        assert_eq!(run.face, Face::Hand, "{marker}");
    }
    // checkboxes are drawn, not typed: ruled squares in formatted text, sketched
    // ones (four bowed strokes and a longer tick) over the same spot in handwriting
    let tasks = "- [ ] todo\n- [x] done";
    let (h, f) = (hand(tasks, 300.0), plain(tasks, 300.0));
    assert_eq!(f.segments.len(), 4 + 4 + 2);
    assert_eq!(h.segments.len(), 8 + 8 + 3);
    let reach = |s: &[penroam_core::markdown::Segment]| {
        s.iter()
            .fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, l| {
                (
                    b.0.min(l.x0.min(l.x1)),
                    b.1.min(l.y0.min(l.y1)),
                    b.2.max(l.x0.max(l.x1)),
                    b.3.max(l.y0.max(l.y1)),
                )
            })
    };
    let (hb, fb) = (reach(&h.segments[..8]), reach(&f.segments[..4]));
    let side = fb.2 - fb.0;
    for (a, b) in [(hb.0, fb.0), (hb.1, fb.1), (hb.2, fb.2), (hb.3, fb.3)] {
        assert!((a - b).abs() < 0.1 * side, "{hb:?} near {fb:?}");
    }
    assert!(h.runs.iter().all(|r| r.face == Face::Hand));
}

#[test]
fn handwritten_links_are_underlined_to_their_hand_width() {
    let p = hand("[a link](https://example.com)", 300.0);
    let run = &p.runs[0];
    let width = penroam_core::text::metrics().advance_in(penroam_core::text::Face::Hand, 'a')
        * 16.0
        / 1000.0;
    assert_eq!(p.segments.len(), 1);
    let s = &p.segments[0];
    assert_eq!(s.x0, run.x);
    assert!(s.x1 > width, "the underline spans the whole link");
    assert_ne!(s.x1, plain("[a link](x)", 300.0).segments[0].x1);
}

#[test]
fn handwritten_text_falls_back_to_formatted_for_missing_glyphs() {
    use penroam_core::text::Face;
    // Excalifont has no section sign
    let p = hand("a\u{a7}b", 300.0);
    let faces: Vec<(&str, Face)> = p.runs.iter().map(|r| (r.text.as_str(), r.face)).collect();
    assert_eq!(
        faces,
        [
            ("a", Face::Hand),
            ("\u{a7}", Face::Regular),
            ("b", Face::Hand)
        ]
    );
}

#[test]
fn row_lock_puts_every_baseline_on_a_row() {
    for text in [
        "one\ntwo\n\nthree",
        "# Big\nbody\n- item\n- [x] task",
        "> quote\n\n```\ncode\n```",
        "| a |\n|---|\n| b |",
    ] {
        let p = rows(text, 200.0);
        assert!(p.height % 24.0 == 0.0, "{text}: {}", p.height);
        for r in &p.runs {
            assert!(r.y % 24.0 == 0.0, "{text}: {} at {}", r.text, r.y);
        }
    }
}

#[test]
fn row_lock_gives_large_text_more_rows() {
    assert_eq!(rows("x", 100.0).height, 24.0);
    assert_eq!(
        layout(
            "x",
            TextFont::Formatted,
            22.0,
            100.0,
            Some(24.0),
            TextAlign::Left,
            &math
        )
        .height,
        24.0
    );
    assert_eq!(
        layout(
            "x",
            TextFont::Formatted,
            32.0,
            100.0,
            Some(24.0),
            TextAlign::Left,
            &math
        )
        .height,
        48.0
    );
    assert_eq!(rows("# Title", 300.0).height, 48.0); // 28 pt heading
}

#[test]
fn row_lock_reports_descenders_below_the_last_row() {
    let p = rows("gyp", 100.0);
    assert!(p.bottom > p.height);
}

#[test]
fn math_is_found_and_left_alone_where_it_should_be() {
    let tex = |s: &str| {
        extract_math(s)
            .1
            .into_iter()
            .map(|m| m.tex)
            .collect::<Vec<_>>()
    };
    assert_eq!(tex("$x$ and $$y$$"), ["x", "y"]);
    assert!(tex("$5 and $10").is_empty());
    assert!(tex("\\$x\\$").is_empty());
    assert!(tex("`$x$`").is_empty());
    assert!(tex("```\n$x$\n```").is_empty());
    assert!(tex("$$a\n\nb$$").is_empty());
    assert_eq!(tex("$$a\nb$$"), ["a\nb"]);
}

#[test]
fn source_cannot_forge_a_formula_slot() {
    let forged = "\u{F0000} $x$";
    let (src, spans) = extract_math(forged);
    assert_eq!(spans.len(), 1);
    assert_eq!(src.chars().filter(|c| *c as u32 >= 0xF0000).count(), 1);
    assert!(src.starts_with('\u{FFFD}'));
}

#[test]
fn unknown_formulas_show_their_source() {
    let p = plain("$\\bad$", 400.0);
    assert!(p.math.is_empty());
    assert_eq!(texts(&p), ["$\\bad$"]);
}

#[test]
fn hostile_nesting_is_flattened() {
    let deep = format!("{}x", "> ".repeat(100_000));
    assert_eq!(texts(&plain(&deep, 400.0)), ["x"]);
    let deep = format!("{}x", "- ".repeat(5_000));
    assert!(!plain(&deep, 400.0).runs.is_empty());
    let indent = plain(&format!("{}x", "> ".repeat(MAX_DEPTH + 40)), 4000.0);
    assert!(indent.runs.last().unwrap().x <= 16.0 * (MAX_DEPTH + 1) as f64);
}

#[test]
fn table_formulas_show_their_results() {
    let p = plain(
        "| Qty | Price | Total |\n|---|---|---|\n| 3 | $0.50 | =A2*B2 |\n| =A2*2 | | =SUM(C2:C2) |",
        400.0,
    );
    let t = texts(&p);
    assert!(t.contains(&"$1.50"), "{t:?}");
    assert!(t.contains(&"6"), "{t:?}");
    assert!(!t.iter().any(|s| s.contains('=')), "{t:?}");
    // the source, not the parsed inlines: `*` never makes italics
    assert!(p
        .runs
        .iter()
        .all(|r| r.face != penroam_core::text::Face::Italic));
}

#[test]
fn table_checkboxes_are_drawn_not_written() {
    let boxed = plain("| a |\n|---|\n| [x] |", 400.0);
    let text = plain("| a |\n|---|\n| x |", 400.0);
    assert_eq!(texts(&boxed), ["a"]);
    // a box and its tick, on top of the table's own lines
    assert_eq!(boxed.segments.len(), text.segments.len() + 6);
    let open = plain("| a |\n|---|\n| [ ] |", 400.0);
    assert_eq!(open.segments.len(), text.segments.len() + 4);
    // anything more than the brackets is text
    assert!(texts(&plain("| a |\n|---|\n| [x] done |", 400.0)).contains(&"[x] done"));
}
