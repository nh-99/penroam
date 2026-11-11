//! Notebooks as Markdown or plain text. This keeps the words and nothing else:
//! handwriting is not text, so ink is left out unless the caller supplies its
//! recognised text, and the report says how much was omitted.

use penroam_core::{Element, Page};
use pulldown_cmark::{Event, Parser, TagEnd};
use serde::Serialize;
use ts_rs::TS;
use uuid::Uuid;

/// Output flavour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormat {
    /// Text blocks as written, with a heading per page.
    Markdown,
    /// Markdown formatting removed.
    Plain,
}

/// A finished text export.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TextExport {
    /// The document.
    pub text: String,
    /// Pages written.
    pub pages: u32,
    /// Typed text blocks written.
    pub text_blocks: u32,
    /// Ink strokes not in the output.
    pub strokes_omitted: u32,
    /// Images not in the output.
    pub images_omitted: u32,
    /// PDF pages (page backgrounds, and PDF pages placed on infinite
    /// canvases) whose content isn't in the output.
    pub pdf_pages_omitted: u32,
    /// Pages where recognised handwriting text was included.
    pub recognized_pages: u32,
    /// What the user loses, one sentence each.
    pub warnings: Vec<String>,
}

/// Markdown with its formatting removed.
pub fn strip_markdown(src: &str) -> String {
    let mut out = String::new();
    for ev in Parser::new(src) {
        match ev {
            Event::Text(t) | Event::Code(t) | Event::Html(t) | Event::InlineHtml(t) => {
                out.push_str(&t)
            }
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::Item
                | TagEnd::CodeBlock
                | TagEnd::BlockQuote(_),
            ) => out.push('\n'),
            _ => {}
        }
    }
    out.trim_end().to_string()
}

/// Writes pages as text. Blocks follow reading order: top to bottom, then
/// left to right. `recognized` may supply the handwriting text of a page.
pub fn export_text(
    title: &str,
    pages: &[Page],
    format: TextFormat,
    recognized: impl Fn(Uuid) -> Option<String>,
) -> TextExport {
    let mut r = TextExport {
        text: String::new(),
        pages: pages.len() as u32,
        text_blocks: 0,
        strokes_omitted: 0,
        images_omitted: 0,
        pdf_pages_omitted: 0,
        recognized_pages: 0,
        warnings: Vec::new(),
    };
    match format {
        TextFormat::Markdown => r.text.push_str(&format!("# {title}\n\n")),
        TextFormat::Plain => r.text.push_str(&format!("{title}\n\n")),
    }
    for (i, page) in pages.iter().enumerate() {
        let mut blocks: Vec<_> = page
            .elements
            .iter()
            .filter_map(|e| match e {
                Element::Text(t) => Some(t),
                _ => None,
            })
            .collect();
        blocks.sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
        let ink = page
            .elements
            .iter()
            .filter(|e| matches!(e, Element::Ink(_)))
            .count() as u32;
        let images = page
            .elements
            .iter()
            .filter(|e| matches!(e, Element::Image(i) if i.pdf_page.is_none()))
            .count() as u32;
        // PDF pages on an infinite canvas count like PDF backgrounds
        let pdf_pages = page
            .elements
            .iter()
            .filter(|e| matches!(e, Element::Image(i) if i.pdf_page.is_some()))
            .count() as u32;
        let heading = format!("Page {}", i + 1);
        match format {
            TextFormat::Markdown => r.text.push_str(&format!("## {heading}\n\n")),
            TextFormat::Plain => r.text.push_str(&format!("{heading}\n\n")),
        }
        for b in &blocks {
            let body = match format {
                TextFormat::Markdown => b.text.clone(),
                TextFormat::Plain => strip_markdown(&b.text),
            };
            r.text.push_str(body.trim_end());
            r.text.push_str("\n\n");
            r.text_blocks += 1;
        }
        let words = (ink > 0).then(|| recognized(page.id)).flatten();
        match words.filter(|w| !w.trim().is_empty()) {
            Some(w) => {
                r.text.push_str(w.trim_end());
                r.text.push_str("\n\n");
                r.recognized_pages += 1;
            }
            None => r.strokes_omitted += ink,
        }
        r.images_omitted += images;
        r.pdf_pages_omitted += page.background.is_some() as u32 + pdf_pages;
    }
    if r.strokes_omitted > 0 {
        r.warnings.push(format!(
            "{} handwritten stroke{} left out: text export can't hold ink. Keep the .penroam file or an annotated PDF for that.",
            r.strokes_omitted,
            if r.strokes_omitted == 1 { " was" } else { "s were" },
        ));
    }
    if r.recognized_pages > 0 {
        r.warnings.push(format!(
            "Handwriting on {} page(s) comes from text recognition and may contain mistakes.",
            r.recognized_pages
        ));
    }
    if r.images_omitted > 0 {
        r.warnings
            .push(format!("{} image(s) left out.", r.images_omitted));
    }
    if r.pdf_pages_omitted > 0 {
        r.warnings.push(format!(
            "{} PDF page(s) are not in this file.",
            r.pdf_pages_omitted
        ));
    }
    r
}
