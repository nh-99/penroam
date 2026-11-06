//! Turns exported notebooks of prompted handwriting into an evaluation corpus.
//!
//! ```text
//! cargo run -p penroam-search --example make_eval_corpus -- \
//!     --prompts prompts.txt --out corpus.json \
//!     writer-a=a.penroam writer-b=b.penroam
//! ```
//!
//! A writer copies a fixed set of prompts into a notebook, one prompt line per line of
//! writing and one prompt page per notebook page, then exports it as a `.penroam` file. The
//! truth is therefore known without anyone reading their notes. `prompts.txt` holds pages
//! separated by blank lines.
//!
//! Each page's regions, from Penroam's own segmenter and sorted top to bottom, are matched to
//! its prompt lines. A page whose region count differs from its prompt is left out and reported,
//! not guessed at, because a misaligned truth would count as recognition errors. Fix those
//! pages by rewriting them or trimming the prompt. The strokes of a line are split into its
//! words at the largest horizontal gaps. That is an approximation which only recall@region
//! depends on; CER, WER and recall@page use the line.
//!
//! The output is the corpus format of `examples/eval.rs` (`docs/development.md`, "Search
//! evaluation"), with the writer as each page's `style`, so results come out per writer. It
//! contains real people's handwriting: keep it out of this repository.

use penroam_core::Page;
use penroam_search::segment::{regions, Rect, Region};
use penroam_storage::Library;
use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutWord {
    text: String,
    stroke_ids: Vec<Uuid>,
    bounds: Rect,
}

#[derive(Debug, Serialize)]
struct OutLine {
    text: String,
    bounds: Rect,
    words: Vec<OutWord>,
}

#[derive(Serialize)]
struct OutPage {
    id: String,
    style: String,
    page: Page,
    lines: Vec<OutLine>,
}

/// Prompt pages from the text of the prompts file: blank lines separate pages, `#` starts a
/// comment line.
fn parse_prompts(text: &str) -> Vec<Vec<String>> {
    let mut pages = vec![];
    let mut page: Vec<String> = vec![];
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            if !page.is_empty() {
                pages.push(std::mem::take(&mut page));
            }
        } else if !line.starts_with('#') {
            page.push(line.to_string());
        }
    }
    if !page.is_empty() {
        pages.push(page);
    }
    pages
}

/// Groups strokes, given as `(left, right)` extents, into `words` runs at the widest gaps.
/// Returns each run's stroke indices. Fewer runs come back when strokes overlap too much to
/// offer that many gaps.
fn split_at_gaps(extents: &[(f64, f64)], words: usize) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..extents.len()).collect();
    order.sort_by(|&a, &b| extents[a].0.total_cmp(&extents[b].0));
    // Gap before position `i` of `order`: the distance from everything to its left.
    let mut reach = f64::NEG_INFINITY;
    let mut gaps: Vec<(f64, usize)> = vec![];
    for (i, &k) in order.iter().enumerate() {
        if i > 0 && extents[k].0 > reach {
            gaps.push((extents[k].0 - reach, i));
        }
        reach = reach.max(extents[k].1);
    }
    gaps.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut cuts: Vec<usize> = gaps
        .iter()
        .take(words.saturating_sub(1))
        .map(|g| g.1)
        .collect();
    cuts.sort_unstable();
    let mut out = vec![];
    let mut start = 0;
    for cut in cuts.into_iter().chain(std::iter::once(order.len())) {
        out.push(order[start..cut].to_vec());
        start = cut;
    }
    out
}

fn stroke_bounds_of(region: &Region<'_>, ids: &[usize]) -> Rect {
    let mut rect: Option<Rect> = None;
    for &i in ids {
        for p in &region.strokes[i].points {
            let r = Rect::from_corners(p.x, p.y, p.x, p.y);
            rect = Some(rect.map_or(r, |x| x.union(&r)));
        }
    }
    rect.unwrap_or(region.bounds)
}

/// The truth lines of a page, or why the page cannot be aligned with its prompt.
fn align(page: &Page, prompt: &[String]) -> Result<Vec<OutLine>, String> {
    let mut found = regions(page);
    found.sort_by(|a, b| a.bounds.y.total_cmp(&b.bounds.y));
    if found.len() != prompt.len() {
        return Err(format!(
            "{} regions for {} prompt lines",
            found.len(),
            prompt.len()
        ));
    }
    let mut lines = vec![];
    for (region, text) in found.iter().zip(prompt) {
        let words: Vec<&str> = text.split_whitespace().collect();
        let extents: Vec<(f64, f64)> = (0..region.strokes.len())
            .map(|i| {
                let b = stroke_bounds_of(region, &[i]);
                (b.x, b.right())
            })
            .collect();
        let runs = split_at_gaps(&extents, words.len());
        if runs.len() != words.len() {
            return Err(format!("cannot split {text:?} into its words"));
        }
        lines.push(OutLine {
            text: text.clone(),
            bounds: region.bounds,
            words: words
                .iter()
                .zip(&runs)
                .map(|(w, run)| OutWord {
                    text: w.to_string(),
                    stroke_ids: run.iter().map(|&i| region.strokes[i].id).collect(),
                    bounds: stroke_bounds_of(region, run),
                })
                .collect(),
        });
    }
    Ok(lines)
}

fn main() {
    let mut prompts = None;
    let mut out = None;
    let mut writers = BTreeMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--prompts" => prompts = args.next(),
            "--out" => out = args.next(),
            _ => match a.split_once('=') {
                Some((writer, file)) => {
                    writers.insert(writer.to_string(), file.to_string());
                }
                None => panic!("expected writer=file.penroam, got {a}"),
            },
        }
    }
    let (Some(prompts), Some(out)) = (prompts, out) else {
        eprintln!("usage: make_eval_corpus --prompts prompts.txt --out corpus.json writer=file.penroam ...");
        std::process::exit(2);
    };
    let prompts = parse_prompts(&std::fs::read_to_string(prompts).expect("read prompts"));
    let dir = tempfile::tempdir().expect("temporary library");
    let mut library = Library::open(dir.path().join("library")).expect("library");
    let mut pages = vec![];
    let mut skipped = 0;
    for (writer, file) in &writers {
        let report = library
            .import_archive(std::fs::File::open(file).expect("open notebook"))
            .expect("import notebook");
        let nb = report.notebook;
        for (i, (&page_id, prompt)) in nb.page_order.iter().zip(&prompts).enumerate() {
            let page = library
                .read_page_detached(nb.id, page_id)
                .expect("read page");
            match align(&page, prompt) {
                Ok(lines) => pages.push(OutPage {
                    id: format!("{writer}-{i:02}"),
                    style: writer.clone(),
                    page,
                    lines,
                }),
                Err(why) => {
                    eprintln!("left out {writer} page {}: {why}", i + 1);
                    skipped += 1;
                }
            }
        }
    }
    let count = pages.len();
    let json = serde_json::json!({ "generator": "make_eval_corpus", "pages": pages });
    std::fs::write(&out, serde_json::to_vec(&json).expect("serialize")).expect("write corpus");
    println!("{count} pages written to {out}, {skipped} left out");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page with one short pen stroke at each `(x, y)`.
    fn page_of(strokes: &[(f64, f64)]) -> Page {
        let elements: Vec<_> = strokes
            .iter()
            .enumerate()
            .map(|(i, (x, y))| {
                let point = |x: f64, y: f64| {
                    serde_json::json!({
                        "x": x, "y": y, "pressure": 0.5, "tiltX": 0, "tiltY": 0, "timeUs": 0,
                    })
                };
                serde_json::json!({
                    "type": "ink", "id": Uuid::from_u128(i as u128 + 1), "z": i, "tool": "pen",
                    "color": "#000000", "opacity": 1, "width": 1.6, "pressure": false, "source": "test",
                    "input": {},
                    "points": [point(*x, *y), point(x + 12.0, y + 16.0)],
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 1, "id": Uuid::from_u128(99), "paper": "letter",
            "size": { "width": 612, "height": 792 }, "template": { "kind": "ruled", "spacing": 24 },
            "revision": 1, "createdAt": 0, "updatedAt": 0, "elements": elements,
        }))
        .expect("page")
    }

    #[test]
    fn regions_align_with_prompt_lines_from_the_top() {
        let page = page_of(&[(10.0, 200.0), (40.0, 200.0), (10.0, 100.0)]);
        let prompt = vec!["top".to_string(), "bottom row".to_string()];
        let lines = align(&page, &prompt).expect("aligned");
        assert_eq!(lines[0].words[0].stroke_ids, vec![Uuid::from_u128(3)]);
        assert_eq!(lines[1].words.len(), 2);
    }

    #[test]
    fn a_page_that_does_not_match_its_prompt_is_refused() {
        let page = page_of(&[(10.0, 100.0)]);
        let prompt = vec!["one".to_string(), "two".to_string()];
        assert!(align(&page, &prompt).unwrap_err().contains("1 regions"));
    }

    #[test]
    fn prompts_are_pages_of_lines() {
        let got = parse_prompts("# note\none line\ntwo line\n\n\nthree\n");
        assert_eq!(got, vec![vec!["one line", "two line"], vec!["three"]]);
    }

    #[test]
    fn words_split_at_the_widest_gaps() {
        let extents = [(0.0, 5.0), (6.0, 10.0), (30.0, 40.0), (55.0, 60.0)];
        assert_eq!(split_at_gaps(&extents, 2), vec![vec![0, 1], vec![2, 3]]);
        assert_eq!(
            split_at_gaps(&extents, 3),
            vec![vec![0, 1], vec![2], vec![3]]
        );
    }

    #[test]
    fn overlapping_strokes_offer_no_gap() {
        let extents = [(0.0, 10.0), (2.0, 8.0), (9.0, 20.0)];
        assert_eq!(split_at_gaps(&extents, 2).len(), 1);
    }

    #[test]
    fn a_stroke_spanning_others_is_not_split_by_their_gaps() {
        // a long crossbar over two letters: the letters' own gap is covered
        let extents = [(0.0, 30.0), (2.0, 8.0), (20.0, 28.0), (50.0, 60.0)];
        assert_eq!(split_at_gaps(&extents, 2), vec![vec![0, 1, 2], vec![3]]);
    }
}
