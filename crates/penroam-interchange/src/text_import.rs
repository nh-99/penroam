//! Markdown and plain text into notebooks.
//!
//! Text blocks already hold Markdown, so a Markdown file stays as it is and
//! plain text is escaped so it reads back literally. Pages are filled greedily
//! using the same layout code as the renderer, breaking only between blocks
//! when it can, and never inside a fenced code block unless that block alone
//! is taller than a page.
//!
//! Local image references are not followed: a single imported file gets no
//! access to the folder it came from. They are counted in the loss report and
//! their source text stays visible.

use crate::document::{ImportError, ImportLoss, ImportedDocument, ImportedPage, Kept, LossLog};
use crate::limits;
use penroam_core::markdown::{self, MathBox};
use penroam_core::validate::limits::{MAX_PAGES, MAX_TEXT_BYTES};
use penroam_core::{Element, Paper, Size, Template, TextAlign, TextBlock, TextFont};
use pulldown_cmark::{Event, Parser, Tag};
use uuid::Uuid;

/// Space kept clear on every side of an imported page, in points.
pub(crate) const MARGIN: f64 = 54.0;
const FONT_SIZE: f64 = 12.0;
const INK: &str = "#111827";

/// The paper imported text is laid out on.
pub(crate) fn page_size() -> Size {
    Paper::Letter.size().expect("Letter has a size")
}

/// Makes text safe to store as a Markdown block that reads back literally.
pub fn escape_markdown(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut at_start = true;
    // tracked as we go: rescanning the prefix is quadratic on a hostile line
    let mut only_digits = true;
    for (i, c) in line.char_indices() {
        let special = matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '$' | '~' | '|' | '&'
        ) || (at_start && matches!(c, '#' | '>' | '-' | '+' | '='))
            || (c == '.' || c == ')') && only_digits && i > 0;
        if special {
            out.push('\\');
        }
        out.push(c);
        if !c.is_whitespace() {
            at_start = false;
        }
        only_digits &= c.is_ascii_digit();
    }
    out
}

/// Decodes a text file: UTF-8 (with or without a BOM) or UTF-16 with a BOM.
/// Control characters other than tab and newline are removed.
fn decode(bytes: &[u8], log: &mut LossLog) -> String {
    let utf16 = |be: bool| -> String {
        let units = bytes[2..].as_chunks::<2>().0.iter().map(|c| {
            if be {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        });
        char::decode_utf16(units)
            .map(|r| r.unwrap_or('\u{fffd}'))
            .collect()
    };
    let text = match bytes {
        [0xff, 0xfe, ..] => utf16(false),
        [0xfe, 0xff, ..] => utf16(true),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    };
    if text.contains('\u{fffd}') {
        log.note("Some characters could not be read and were replaced with \u{fffd}.");
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

/// Source-level blocks: runs of lines separated by blank lines, with fenced
/// code kept together.
pub(crate) fn blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut fence: Option<char> = None;
    for line in text.split('\n') {
        let t = line.trim_start();
        let marker = t.chars().next().filter(|c| matches!(c, '`' | '~'));
        if let Some(m) = marker {
            if t.chars().take_while(|c| *c == m).count() >= 3 {
                match fence {
                    None => fence = Some(m),
                    Some(open) if open == m => fence = None,
                    _ => {}
                }
            }
        }
        if fence.is_none() && line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(line);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn height(text: &str, width: f64) -> f64 {
    let no_math = |_: &str, _: bool| -> Option<MathBox> { None };
    markdown::layout(
        text,
        TextFont::Formatted,
        FONT_SIZE,
        width,
        None,
        TextAlign::Left,
        &no_math,
    )
    .height
}

/// Whether `block` fits on one page as it is. Callers with structure of their
/// own (a fenced code block, a table) use this to split it themselves, because
/// [`paginate`] would cut it anywhere.
pub(crate) fn fits_on_page(block: &str) -> bool {
    let size = page_size();
    block.len() <= MAX_TEXT_BYTES
        && height(block, size.width - 2.0 * MARGIN) <= size.height - 2.0 * MARGIN
}

/// Splits text that is too tall or too long for one page, at line ends and,
/// for a single enormous line, at character boundaries.
fn split_oversized(block: &str, width: f64, max_height: f64) -> Vec<String> {
    let per_page = (((max_height / (FONT_SIZE * 1.5)) * (width / (FONT_SIZE * 0.35))) as usize)
        .clamp(64, MAX_TEXT_BYTES / 8);
    let mut pieces = Vec::new();
    let mut cur = String::new();
    let mut count = 0usize;
    for ch in block.chars() {
        cur.push(ch);
        count += 1;
        if ch == '\n' && count > per_page / 2 || count >= per_page {
            pieces.push(std::mem::take(&mut cur));
            count = 0;
        }
    }
    if !cur.is_empty() {
        pieces.push(cur);
    }
    // the character estimate is deliberately loose: halve anything still too tall
    let mut out = Vec::new();
    while let Some(p) = pieces.pop() {
        if height(&p, width) <= max_height || p.chars().count() < 2 {
            out.push(p);
            continue;
        }
        let mid = p
            .char_indices()
            .nth(p.chars().count() / 2)
            .map_or(0, |x| x.0);
        pieces.push(p[..mid].to_string());
        pieces.push(p[mid..].to_string());
    }
    out.reverse();
    out.iter_mut()
        .for_each(|p| *p = p.trim_end_matches('\n').to_string());
    out.retain(|p| !p.is_empty());
    out
}

/// Lays Markdown blocks onto as many pages as they need, one text block per
/// page, and always at least one page. Errors past the model's page limit.
pub(crate) fn paginate(source: Vec<String>) -> Result<Vec<ImportedPage>, ImportError> {
    let size = page_size();
    let (w, h) = (size.width - 2.0 * MARGIN, size.height - 2.0 * MARGIN);
    let mut chunks: Vec<(String, f64)> = Vec::with_capacity(source.len());
    for b in source {
        let bh = if b.len() > MAX_TEXT_BYTES {
            f64::MAX
        } else {
            height(&b, w)
        };
        if bh <= h {
            chunks.push((b, bh));
        } else {
            for p in split_oversized(&b, w, h) {
                let ph = height(&p, w);
                chunks.push((p, ph));
            }
        }
    }
    let gap = FONT_SIZE * 1.5;
    let mut pages: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut used = 0.0;
    for (text, bh) in chunks {
        let need = if cur.is_empty() { bh } else { bh + gap };
        if !cur.is_empty() && (used + need > h || cur.len() + text.len() + 2 > MAX_TEXT_BYTES) {
            pages.push(std::mem::take(&mut cur));
            used = 0.0;
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
            used += gap;
        }
        cur.push_str(&text);
        used += bh;
        if pages.len() >= MAX_PAGES {
            return Err(ImportError::TooLarge("more than 10 000 pages".into()));
        }
    }
    if !cur.is_empty() || pages.is_empty() {
        pages.push(cur);
    }
    Ok(pages
        .into_iter()
        .map(|text| {
            let height = self::height(&text, w).max(FONT_SIZE);
            ImportedPage {
                size,
                template: Template::default(),
                pdf: None,
                elements: vec![Element::Text(TextBlock {
                    id: Uuid::new_v4(),
                    z: 1,
                    x: MARGIN,
                    y: MARGIN,
                    width: w,
                    height,
                    text,
                    font_size: FONT_SIZE,
                    color: INK.into(),
                    align: TextAlign::Left,
                    font: Default::default(),
                    row_pitch: None,
                    extra: Default::default(),
                })],
            }
        })
        .collect())
}

fn finish(title: &str, format: &str, pages: Vec<ImportedPage>, log: LossLog) -> ImportedDocument {
    let kept = Kept {
        pages: pages.len() as u32,
        text_blocks: pages.len() as u32,
        ..Default::default()
    };
    ImportedDocument {
        title: title.to_string(),
        pages,
        assets: Vec::new(),
        report: ImportLoss {
            format: format.into(),
            kept,
            lost: log.sentences(),
        },
    }
}

/// Imports a Markdown file. See the module docs for what happens to images.
pub fn import_markdown(bytes: &[u8], title: &str) -> Result<ImportedDocument, ImportError> {
    if bytes.len() > limits::MAX_TEXT_FILE {
        return Err(ImportError::TooLarge("over 64 MiB of text".into()));
    }
    let mut log = LossLog::default();
    let text = decode(bytes, &mut log);
    for ev in Parser::new(&text) {
        if let Event::Start(Tag::Image { .. }) = ev {
            log.note("{n} image reference(s) were not imported; add the images by hand.");
        }
    }
    let pages = paginate(blocks(&text))?;
    Ok(finish(title, "Markdown", pages, log))
}

/// Imports a plain text file. Line breaks are kept and nothing is interpreted
/// as Markdown.
pub fn import_plain_text(bytes: &[u8], title: &str) -> Result<ImportedDocument, ImportError> {
    if bytes.len() > limits::MAX_TEXT_FILE {
        return Err(ImportError::TooLarge("over 64 MiB of text".into()));
    }
    let mut log = LossLog::default();
    let text = decode(bytes, &mut log);
    let escaped: Vec<String> = text.split('\n').map(escape_markdown).collect();
    let pages = paginate(blocks(&escaped.join("\n")))?;
    Ok(finish(title, "Plain text", pages, log))
}
