//! Markdown layout rules, and the golden file the TypeScript port is checked
//! against (`apps/desktop-mobile/tests/markdown.test.ts`). Run with
//! `UPDATE_GOLDEN=1` after changing the layout on purpose.

use penroam_core::markdown::{extract_math, layout, MathBox, Paint, MAX_DEPTH};
use penroam_core::model::TextAlign;
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
    layout(text, 16.0, width, None, TextAlign::Left, &math)
}

fn rows(text: &str, width: f64) -> Paint {
    layout(text, 16.0, width, Some(24.0), TextAlign::Left, &math)
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
];

fn golden_cases() -> Vec<Value> {
    let mut out = Vec::new();
    for text in CORPUS {
        for (fs, width, row, align) in [
            (16.0, 280.0, None, TextAlign::Left),
            (16.0, 280.0, Some(24.0), TextAlign::Left),
            (22.0, 120.0, Some(20.0), TextAlign::Center),
            (12.0, 90.0, None, TextAlign::Right),
        ] {
            let paint = layout(text, fs, width, row, align, &math);
            out.push(json!({
                "text": text, "fontSize": fs, "width": width, "rowPitch": row,
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
        layout("x", 22.0, 100.0, Some(24.0), TextAlign::Left, &math).height,
        24.0
    );
    assert_eq!(
        layout("x", 32.0, 100.0, Some(24.0), TextAlign::Left, &math).height,
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
