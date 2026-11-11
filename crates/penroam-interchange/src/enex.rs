//! Evernote `.enex` exports: every selected note in one XML file, with
//! attachments inline as base64.
//!
//! ```text
//! <en-export>
//!   <note>
//!     <title>Shopping</title>
//!     <content><![CDATA[<en-note><div>Milk</div><en-media hash="…" type="image/png"/></en-note>]]></content>
//!     <created>20240102T030405Z</created>
//!     <tag>home</tag>
//!     <resource><data encoding="base64">…</data><mime>image/png</mime></resource>
//!   </note>
//! </en-export>
//! ```
//!
//! **Two parsers, on purpose.** The outer file is machine-written XML and is
//! read strictly: if it doesn't parse or stops early, the whole file is refused,
//! as with Xournal. The body of a note is ENML, a dialect of XHTML that in the
//! wild carries HTML entities and unclosed tags, so it is read in a separate,
//! tolerant pass. If even that pass fails, only that note degrades to plain text
//! and the report says so.
//!
//! **Output.** One notebook for the file. Each note starts a new page with its
//! title as a heading, followed by its body as Markdown, paginated by
//! [`crate::text_import`]. Literal text always goes through
//! [`escape_markdown`], so nothing in a note can turn into Markdown structure.
//! Images are placed after the note's text, because a text block can't flow
//! around them; the report says so. Formatting Markdown has no form for
//! (underline, colour) is dropped and reported, and the words stay.
//!
//! **Attachments.** `en-media` names a resource by the MD5 of its bytes. MD5 is
//! used only to match Evernote's own ids, never to establish trust or identity:
//! a collision could at worst show one of the file's own images in the wrong
//! place. Assets are addressed by SHA-256 like any other. Resource file names
//! are ignored entirely, so nothing from the file is ever used as a path.
//!
//! **Security.** Neither pass has DTD support. An unknown entity in the outer
//! XML makes the file malformed; in ENML it sends that one note to the plain
//! text path, where it stays as literal text. So entity bombs and external
//! entities do nothing. Only `http`, `https` and `mailto` links survive, and
//! `script` and `style` contents are removed before ENML is parsed at all. See
//! [`crate::limits`] and `docs/formats/portability.md`.

use crate::document::{
    pdf_attachment, ImportError, ImportLoss, ImportedAsset, ImportedDocument, ImportedPage, Kept,
    LossLog,
};
use crate::images::{check_image, pixel_size};
use crate::limits;
use crate::text_import::{blocks, escape_markdown, fits_on_page, page_size, paginate, MARGIN};
use crate::xournal::base64;
use md5::{Digest, Md5};
use penroam_core::markdown::MAX_DEPTH;
use penroam_core::validate::limits as model;
use penroam_core::{Element, ImageElement, Template};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use std::borrow::Cow;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Most base64 kept for one resource. Larger than [`limits::MAX_IMAGE_BYTES`]
/// strictly needs, to allow for line wrapping; `check_image` is exact.
const MAX_BASE64_BYTES: usize = limits::MAX_IMAGE_BYTES / 2 * 3;
/// Most resources in one note. Evernote's own limit is far lower; this keeps a
/// file of tiny resources from costing more memory than it has bytes.
const MAX_RESOURCES_PER_NOTE: usize = 100_000;
/// Most open elements tracked in ENML. Deeper tags are ignored, and their text
/// kept. This also bounds the search for the element a closing tag belongs to.
const MAX_OPEN_ELEMENTS: usize = 256;
/// Most columns in a table. Further cells join the last column, which keeps
/// padding short rows from being quadratic in the size of the input.
const MAX_TABLE_COLUMNS: usize = 32;
/// Space between an imported image and whatever is above it, in points.
const IMAGE_GAP: f64 = 12.0;
/// Size of one image pixel in points, at the usual 96 pixels per inch.
const POINTS_PER_PIXEL: f64 = 0.75;

const DROPPED_LINK: &str =
    "{n} link(s) to addresses other than web or email kept their text but lost the address.";
const UNSUPPORTED_FORMATTING: &str =
    "{n} note(s) used underlining, colours or other formatting Penroam's text doesn't have; the words were kept.";
const TABLE: &str =
    "{n} table(s) became simple Markdown tables: the first row is now a header, and widths, colours and merged cells were lost.";
const WEB_IMAGE: &str = "{n} image(s) linked from the web were not imported.";

fn malformed(e: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("this Evernote export can't be read: {e}"))
}

/// Latin-1 entity names, in code point order from U+00A0.
const LATIN1: [&str; 96] = [
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf",
    "laquo", "not", "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro",
    "para", "middot", "cedil", "sup1", "ordm", "raquo", "frac14", "frac12", "frac34", "iquest",
    "Agrave", "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute",
    "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute",
    "Ocirc", "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute",
    "THORN", "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil",
    "egrave", "eacute", "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde",
    "ograve", "oacute", "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc",
    "uuml", "yacute", "thorn", "yuml",
];

/// Resolves the name between `&` and `;`: a character reference, or one of the
/// common HTML entities. Written out here because quick-xml's `escape-html`
/// feature would also change what the strict Xournal reader, and every other
/// crate in the build, accepts.
fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    if let Some(i) = LATIN1.iter().position(|n| *n == name) {
        return char::from_u32(0xa0 + i as u32);
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "sbquo" => '\u{201a}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        "bdquo" => '\u{201e}',
        "dagger" => '\u{2020}',
        "Dagger" => '\u{2021}',
        "bull" => '\u{2022}',
        "hellip" => '\u{2026}',
        "permil" => '\u{2030}',
        "prime" => '\u{2032}',
        "Prime" => '\u{2033}',
        "lsaquo" => '\u{2039}',
        "rsaquo" => '\u{203a}',
        "euro" => '\u{20ac}',
        "trade" => '\u{2122}',
        "larr" => '\u{2190}',
        "uarr" => '\u{2191}',
        "rarr" => '\u{2192}',
        "darr" => '\u{2193}',
        "harr" => '\u{2194}',
        "minus" => '\u{2212}',
        "ne" => '\u{2260}',
        "le" => '\u{2264}',
        "ge" => '\u{2265}',
        "infin" => '\u{221e}',
        _ => return None,
    })
}

/// Replaces the entities [`entity`] knows. Anything else stays as literal text.
fn decode_entities(s: &str) -> String {
    /// Longer than any name [`entity`] knows, and than a numeric reference needs.
    const LONGEST_REFERENCE: usize = 12;
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.bytes().take(LONGEST_REFERENCE).position(|b| b == b';');
        match end.and_then(|e| Some((e, entity(&rest[1..e])?))) {
            Some((e, c)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// The start tag at the start of `s`, if it's an element whose contents are
/// never shown.
fn dropped_element(s: &str) -> Option<&'static str> {
    ["script", "style", "en-crypt"].into_iter().find(|name| {
        let tag = &s.as_bytes()[1..];
        tag.len() > name.len()
            && tag[..name.len()].eq_ignore_ascii_case(name.as_bytes())
            && (tag[name.len()].is_ascii_whitespace() || matches!(tag[name.len()], b'>' | b'/'))
    })
}

/// Whether the `<` at the start of `s` opens markup, rather than being text
/// like the one in `x < y`. A tokeniser would read such text as a tag up to the
/// next `>` and lose it. Quotes count the way the tokeniser counts them, so
/// this agrees with it about where a tag ends. Running out of input counts as
/// markup, which leaves the tokeniser to report the truncated tag.
fn opens_markup(s: &str) -> bool {
    match s.as_bytes().get(1) {
        Some(b'!' | b'?') => return true,
        Some(b) if *b == b'/' || b.is_ascii_alphabetic() => {}
        _ => return false,
    }
    let mut quote = None;
    for b in s.bytes().skip(1) {
        match (quote, b) {
            (Some(q), _) if q == b => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'>') => return true,
            (None, b'<') => return false,
            _ => {}
        }
    }
    true
}

/// Prepares ENML for parsing: removes comments and the contents of `script`,
/// `style` and `en-crypt`, counting the encrypted sections, and escapes any `<`
/// that isn't markup. This works on the raw text because a script's `<` would
/// otherwise read as markup and could swallow its own closing tag and the rest
/// of the note.
fn strip_hidden(enml: &str) -> (String, u32) {
    let mut out = String::with_capacity(enml.len());
    let mut encrypted = 0;
    let mut rest = enml;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let Some(name) = dropped_element(rest) else {
            out.push_str(if opens_markup(rest) { "<" } else { "&lt;" });
            rest = &rest[1..];
            continue;
        };
        encrypted += u32::from(name == "en-crypt");
        let Some(open_end) = rest.find('>') else {
            return (out, encrypted);
        };
        let self_closed = rest[..open_end].ends_with('/');
        rest = &rest[open_end + 1..];
        if !self_closed {
            let close = find_ignore_case(rest, &format!("</{name}"));
            let end = close.and_then(|c| rest[c..].find('>').map(|e| c + e + 1));
            // an unclosed element hides everything after it, as in a browser
            rest = end.map_or("", |e| &rest[e..]);
        }
    }
    out.push_str(rest);
    (out, encrypted)
}

/// A note's text with the markup removed, for ENML the tolerant parser gave up
/// on. Entities it doesn't know stay visible rather than being expanded.
fn plain_text(enml: &str) -> String {
    let mut out = String::with_capacity(enml.len());
    let mut rest = enml;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let tag_like = rest
            .as_bytes()
            .get(1)
            .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'/' | b'!' | b'?'));
        // with no `>` left, every remaining `<` is text too
        let Some(end) = rest.find('>') else { break };
        if !tag_like {
            out.push('<');
            rest = &rest[1..];
            continue;
        }
        let tag = rest[1..end].to_ascii_lowercase();
        rest = &rest[end + 1..];
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_ascii_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match name {
            "en-todo" if tag.contains("checked=\"true\"") => out.push_str("\n[x] "),
            "en-todo" => out.push_str("\n[ ] "),
            "td" | "th" => out.push(' '),
            "div" | "p" | "br" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre"
            | "hr" | "ul" | "ol" | "table" | "blockquote" => out.push('\n'),
            _ => {}
        }
    }
    out.push_str(rest);
    decode_entities(&out)
}

/// A link destination that is safe to write into Markdown, or `None` if its
/// scheme isn't allowed. Everything that could end the destination, or mean
/// something to the Markdown, math or table scanners, is encoded away.
fn safe_link(href: &str) -> Option<String> {
    let href = href.trim();
    let (scheme, _) = href.split_once(':')?;
    if !["http", "https", "mailto"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
    {
        return None;
    }
    let mut out = String::with_capacity(href.len());
    for c in href.chars() {
        let encode = c.is_control()
            || c.is_whitespace()
            || matches!(c, '(' | ')' | '<' | '>' | '\\' | '"' | '`' | '|' | '$');
        if encode {
            let mut utf8 = [0; 4];
            for b in c.encode_utf8(&mut utf8).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else if c == '&' {
            // otherwise `&copy;` in a query string would decode to a character
            out.push_str("\\&");
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `text` as a code span. The delimiter is longer than any run of backticks
/// inside it, which is the only escaping code spans have.
fn code_span(text: &str, in_cell: bool) -> String {
    let longest = text
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let ticks = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    // the one escape GFM honours inside code, because cells split first
    let body = if in_cell {
        text.replace('|', "\\|")
    } else {
        text.to_string()
    };
    format!("{ticks}{pad}{body}{pad}{ticks}")
}

/// `# text`, with a trailing `#` escaped so it isn't read as a closing sequence.
fn heading_line(level: usize, text: &str) -> String {
    let hashes = "#".repeat(level);
    match text.strip_suffix('#') {
        Some(head) => format!("{hashes} {head}\\#"),
        None => format!("{hashes} {text}"),
    }
}

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const STRIKE: u8 = 4;
/// Inline marks, outermost first when opened.
const MARKS: [(u8, &str); 3] = [(BOLD, "**"), (ITALIC, "*"), (STRIKE, "~~")];

/// A stretch of literal text with one style.
struct Run {
    text: String,
    marks: u8,
    code: bool,
    link: Option<usize>,
}

/// An ENML element that changes how what's inside it is written. Everything
/// else, such as `span` or `en-note`, is transparent.
#[derive(Clone, Copy, PartialEq)]
enum Open {
    Bold,
    Italic,
    Strike,
    Code,
    Link,
    Heading,
    Paragraph,
    /// A `div`, which in Evernote is usually one line of text.
    Line,
    Pre,
    Quote,
    List,
    Item,
    Table,
    Row,
    Cell,
}

impl Open {
    fn of(name: &str) -> Option<Self> {
        Some(match name {
            "b" | "strong" => Open::Bold,
            "i" | "em" | "cite" | "var" => Open::Italic,
            "s" | "strike" | "del" => Open::Strike,
            "code" | "tt" | "kbd" | "samp" => Open::Code,
            "a" => Open::Link,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Open::Heading,
            "p" => Open::Paragraph,
            "div" => Open::Line,
            "pre" => Open::Pre,
            "blockquote" => Open::Quote,
            "ul" | "ol" => Open::List,
            "li" => Open::Item,
            "table" => Open::Table,
            "tr" => Open::Row,
            "td" | "th" => Open::Cell,
            _ => return None,
        })
    }
}

/// A Markdown container that prefixes the lines inside it.
enum Container {
    Quote,
    List {
        ordered: bool,
        next: u32,
    },
    /// A list item, with its marker until the first line has used it.
    Item {
        indent: usize,
        marker: Option<String>,
    },
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: Option<Vec<String>>,
    span: usize,
    /// Tables open, this one included. Inner ones are flattened into its cell.
    depth: u32,
}

/// Converts ENML to Markdown blocks. Tags may be unbalanced: a closing tag
/// closes the nearest matching element and everything inside it, one with no
/// match is ignored, and whatever is open at the end is closed there.
///
/// Lines are built from [`Run`]s and written out only when complete, so a mark
/// that spans a line break becomes one pair per line and whitespace can be moved
/// outside the delimiters, where CommonMark needs it.
#[derive(Default)]
struct Enml {
    chunks: Vec<String>,
    /// Lines of the top-level block being built while in a container.
    container_block: String,
    open: Vec<(Open, u8)>,
    containers: Vec<Container>,
    runs: Vec<Run>,
    /// Text so far ends in a space, or is at the start of a line.
    at_gap: bool,
    bold: u32,
    italic: u32,
    strike: u32,
    code: u32,
    link: Option<usize>,
    links: Vec<String>,
    task: Option<bool>,
    para: Vec<String>,
    para_is_tasks: bool,
    heading: Option<(usize, Vec<String>)>,
    pre: Option<String>,
    table: Option<Table>,
    media: Vec<String>,
    notes: Vec<&'static str>,
}

/// What [`Enml`] found in a note.
struct Converted {
    chunks: Vec<String>,
    /// Lowercase hashes of the `en-media` elements, in order.
    media: Vec<String>,
    /// Report sentences, kept back until the conversion has succeeded.
    notes: Vec<&'static str>,
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    let mut attributes = e.html_attributes();
    attributes.with_checks(false);
    attributes
        .flatten()
        .find(|a| {
            let key: &str = a.key.as_ref();
            key.eq_ignore_ascii_case(name)
        })
        .map(|a| decode_entities(&a.value))
}

fn close_marks(out: &mut String, open: &mut Vec<u8>, keep: u8) {
    let Some(first) = open.iter().position(|m| keep & m == 0) else {
        return;
    };
    for mark in open.drain(first..).rev() {
        out.push_str(MARKS.iter().find(|(m, _)| *m == mark).map_or("", |m| m.1));
    }
}

impl Enml {
    fn new() -> Self {
        Self {
            at_gap: true,
            ..Self::default()
        }
    }

    fn in_cell(&self) -> bool {
        self.table.as_ref().is_some_and(|t| t.cell.is_some())
    }

    fn can_nest(&self) -> bool {
        // a list is two containers per level
        !self.in_cell() && self.heading.is_none() && self.containers.len() < 2 * MAX_DEPTH
    }

    fn note_once(&mut self, what: &'static str) {
        if !self.notes.contains(&what) {
            self.notes.push(what);
        }
    }

    fn text(&mut self, s: &str) {
        if let Some(pre) = &mut self.pre {
            pre.extend(
                s.chars()
                    .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
                    .map(|c| if c == '\u{a0}' { ' ' } else { c }),
            );
            return;
        }
        let flag = |count: u32, mark: u8| if count > 0 { mark } else { 0 };
        let marks = flag(self.bold, BOLD) | flag(self.italic, ITALIC) | flag(self.strike, STRIKE);
        let code = self.code > 0;
        for c in s.chars() {
            let c = if c.is_ascii_whitespace() || c == '\u{a0}' {
                ' '
            } else {
                c
            };
            if c.is_control() || (c == ' ' && self.at_gap) {
                continue;
            }
            self.at_gap = c == ' ';
            match self.runs.last_mut() {
                Some(r) if r.marks == marks && r.code == code && r.link == self.link => {
                    r.text.push(c)
                }
                _ => self.runs.push(Run {
                    text: c.to_string(),
                    marks,
                    code,
                    link: self.link,
                }),
            }
        }
    }

    /// The current line as Markdown. Emphasis delimiters can't sit next to the
    /// space they enclose, so spaces are held back and written outside them.
    fn render(&self) -> String {
        let in_cell = self.in_cell();
        let mut out = String::new();
        let mut open: Vec<u8> = Vec::new();
        let mut link = None;
        let mut space = false;
        let close_link = |out: &mut String, link: Option<usize>| {
            if let Some(i) = link {
                out.push_str("](");
                out.push_str(&self.links[i]);
                out.push(')');
            }
        };
        for run in &self.runs {
            let core = run.text.trim_matches(' ');
            if core.is_empty() {
                space = true;
                continue;
            }
            space |= run.text.starts_with(' ');
            if run.link != link {
                close_marks(&mut out, &mut open, 0);
                close_link(&mut out, link);
            } else {
                close_marks(&mut out, &mut open, run.marks);
            }
            if space && !out.is_empty() {
                out.push(' ');
            }
            if run.link != link {
                link = run.link;
                if link.is_some() {
                    out.push('[');
                }
            }
            for (mark, delimiter) in MARKS {
                if run.marks & mark != 0 && !open.contains(&mark) {
                    open.push(mark);
                    out.push_str(delimiter);
                }
            }
            if run.code {
                out.push_str(&code_span(core, in_cell));
            } else {
                out.push_str(&escape_markdown(core));
            }
            space = run.text.ends_with(' ');
        }
        close_marks(&mut out, &mut open, 0);
        close_link(&mut out, link);
        out
    }

    /// Ends the current line and files it where it belongs.
    fn finish_line(&mut self) {
        let task = self.task.take();
        let md = self.render();
        self.runs.clear();
        self.at_gap = true;
        if md.is_empty() && task.is_none() {
            return;
        }
        if let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
            cell.push(md);
            return;
        }
        if let Some((_, lines)) = &mut self.heading {
            lines.push(md);
            return;
        }
        // a plain line right after a task would continue that item
        if !self.para.is_empty() && self.para_is_tasks != task.is_some() {
            self.flush_para();
        }
        self.para_is_tasks = task.is_some();
        let line = match task {
            None => md,
            Some(done) => {
                let item_start = self.para.is_empty()
                    && matches!(
                        self.containers.last(),
                        Some(Container::Item {
                            marker: Some(_),
                            ..
                        })
                    );
                let bullet = if item_start { "" } else { "- " };
                let check = if done { "[x]" } else { "[ ]" };
                format!("{bullet}{check} {md}").trim_end().to_string()
            }
        };
        self.para.push(line);
    }

    fn flush_para(&mut self) {
        if !self.para.is_empty() {
            let md = self.para.join("\n");
            self.para.clear();
            self.emit(&md);
        }
    }

    fn end_para(&mut self) {
        self.finish_line();
        self.flush_para();
    }

    /// Writes a finished block, inside whatever containers are open.
    fn emit(&mut self, md: &str) {
        if self.containers.is_empty() {
            self.chunks.push(md.to_string());
            return;
        }
        for line in md.split('\n') {
            if !self.container_block.is_empty() {
                self.container_block.push('\n');
            }
            for c in &mut self.containers {
                match c {
                    Container::Quote => self.container_block.push_str("> "),
                    Container::List { .. } => {}
                    Container::Item { indent, marker } => match marker.take() {
                        Some(m) => self.container_block.push_str(&m),
                        None => self.container_block.push_str(&" ".repeat(*indent)),
                    },
                }
            }
            self.container_block.push_str(line);
        }
    }

    /// Writes a block whose `body` lines mustn't be cut anywhere but between
    /// lines, repeating `head` and `tail` around each page's worth.
    fn emit_framed(&mut self, head: &[String], body: Vec<String>, tail: &[String]) {
        let framed = |part: &[String]| [head, part, tail].concat().join("\n");
        if !self.containers.is_empty() {
            let md = framed(&body);
            self.emit(&md);
            return;
        }
        let mut todo = vec![&body[..]];
        while let Some(part) = todo.pop() {
            let block = framed(part);
            if part.len() < 2 || fits_on_page(&block) {
                self.chunks.push(block);
            } else {
                let (first, second) = part.split_at(part.len() / 2);
                todo.push(second);
                todo.push(first);
            }
        }
    }

    fn emit_code(&mut self, code: &str) {
        let code = code.trim_matches('\n');
        if code.is_empty() {
            return;
        }
        let longest = code
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or_default();
        let fence = "`".repeat((longest + 1).max(3));
        let body = code.split('\n').map(str::to_string).collect();
        self.emit_framed(
            std::slice::from_ref(&fence),
            body,
            std::slice::from_ref(&fence),
        );
    }

    fn emit_table(&mut self, mut table: Table) {
        if !table.row.is_empty() {
            table.rows.push(std::mem::take(&mut table.row));
        }
        if table.rows.is_empty() {
            return;
        }
        self.notes.push(TABLE);
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(1);
        let line = |row: &[String]| {
            let mut md = String::from("|");
            for i in 0..columns {
                md.push(' ');
                md.push_str(row.get(i).map_or("", String::as_str));
                md.push_str(" |");
            }
            md
        };
        let head = [
            line(&table.rows[0]),
            format!("|{}", " --- |".repeat(columns)),
        ];
        let body: Vec<String> = table.rows[1..].iter().map(|r| line(r)).collect();
        if body.is_empty() {
            let md = head.join("\n");
            self.emit(&md);
        } else {
            self.emit_framed(&head, body, &[]);
        }
    }

    fn pop_containers(&mut self, count: u8) {
        for _ in 0..count {
            self.containers.pop();
        }
        if self.containers.is_empty() && !self.container_block.is_empty() {
            self.chunks.push(std::mem::take(&mut self.container_block));
        }
    }

    fn close_cell(&mut self) {
        self.finish_line();
        let Some(table) = self.table.as_mut() else {
            return;
        };
        let Some(lines) = table.cell.take() else {
            return;
        };
        let text = lines.join(" ");
        if table.row.len() >= MAX_TABLE_COLUMNS {
            if let Some(last) = table.row.last_mut() {
                last.push(' ');
                last.push_str(&text);
            }
            return;
        }
        table.row.push(text);
        for _ in 1..table.span {
            if table.row.len() < MAX_TABLE_COLUMNS {
                table.row.push(String::new());
            }
        }
    }

    /// Applies an element's opening. The result is what to undo at its close:
    /// containers pushed, or a variant-specific flag.
    fn enter(&mut self, kind: Open, name: &str, e: &BytesStart<'_>) -> u8 {
        match kind {
            Open::Bold => self.bold += 1,
            Open::Italic => self.italic += 1,
            Open::Strike => self.strike += 1,
            Open::Code => self.code += 1,
            Open::Link => {
                if let Some(href) = attr(e, "href") {
                    self.link = safe_link(&href).map(|url| {
                        self.links.push(url);
                        self.links.len() - 1
                    });
                    if self.link.is_none() {
                        self.notes.push(DROPPED_LINK);
                    }
                }
            }
            Open::Heading => {
                self.end_para();
                if self.heading.is_none() && !self.in_cell() {
                    let level = name[1..].parse().unwrap_or(1);
                    self.heading = Some((level, Vec::new()));
                }
            }
            Open::Paragraph => self.end_para(),
            Open::Line => self.finish_line(),
            Open::Pre => {
                self.finish_line();
                if self.in_cell() || self.heading.is_some() {
                    self.code += 1;
                    return 2;
                }
                self.flush_para();
                if self.pre.is_none() {
                    self.pre = Some(String::new());
                    return 1;
                }
            }
            Open::Quote => {
                self.end_para();
                if self.can_nest() {
                    self.containers.push(Container::Quote);
                    return 1;
                }
            }
            Open::List => {
                self.end_para();
                if self.can_nest() {
                    let mut pushed = 1;
                    // Evernote nests `ul` straight inside `ul`, with no item
                    if matches!(self.containers.last(), Some(Container::List { .. })) {
                        self.containers.push(Container::Item {
                            indent: 2,
                            marker: None,
                        });
                        pushed += 1;
                    }
                    self.containers.push(Container::List {
                        ordered: name == "ol",
                        next: 1,
                    });
                    return pushed;
                }
            }
            Open::Item => {
                self.end_para();
                if self.can_nest() {
                    let marker = match self.containers.last_mut() {
                        Some(Container::List {
                            ordered: true,
                            next,
                        }) => {
                            *next = next.saturating_add(1);
                            format!("{}. ", *next - 1)
                        }
                        _ => "- ".to_string(),
                    };
                    self.containers.push(Container::Item {
                        indent: marker.len(),
                        marker: Some(marker),
                    });
                    return 1;
                }
            }
            Open::Table => {
                self.finish_line();
                match &mut self.table {
                    Some(t) => t.depth += 1,
                    None => {
                        self.flush_para();
                        self.table = Some(Table {
                            depth: 1,
                            ..Table::default()
                        });
                    }
                }
            }
            Open::Row => {}
            Open::Cell => {
                self.finish_line();
                if let Some(t) = self.table.as_mut().filter(|t| t.depth == 1) {
                    t.cell = Some(Vec::new());
                    t.span = attr(e, "colspan")
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(1)
                        .clamp(1, MAX_TABLE_COLUMNS);
                }
            }
        }
        0
    }

    fn leave(&mut self, kind: Open, undo: u8) {
        match kind {
            Open::Bold => self.bold -= 1,
            Open::Italic => self.italic -= 1,
            Open::Strike => self.strike -= 1,
            Open::Code => self.code -= 1,
            Open::Link => self.link = None,
            Open::Heading => {
                self.finish_line();
                if !self.in_cell() {
                    if let Some((level, lines)) = self.heading.take() {
                        if !lines.is_empty() {
                            let md = heading_line(level, &lines.join(" "));
                            self.emit(&md);
                        }
                    }
                }
            }
            Open::Paragraph => self.end_para(),
            Open::Line => match &mut self.pre {
                Some(pre) => pre.push('\n'),
                None => self.finish_line(),
            },
            Open::Pre => match undo {
                1 => {
                    if let Some(code) = self.pre.take() {
                        self.emit_code(&code);
                    }
                }
                2 => {
                    self.finish_line();
                    self.code -= 1;
                }
                _ => {}
            },
            Open::Quote | Open::List | Open::Item => {
                self.end_para();
                self.pop_containers(undo);
            }
            Open::Table => {
                self.finish_line();
                let inner = self.table.as_mut().filter(|t| t.depth > 1);
                if let Some(t) = inner {
                    t.depth -= 1;
                } else if let Some(table) = self.table.take() {
                    self.flush_para();
                    self.emit_table(table);
                }
            }
            Open::Row => {
                if let Some(t) = self.table.as_mut().filter(|t| t.depth == 1) {
                    if !t.row.is_empty() {
                        let row = std::mem::take(&mut t.row);
                        t.rows.push(row);
                    }
                }
            }
            Open::Cell => {
                if self.table.as_ref().is_some_and(|t| t.depth == 1) {
                    self.close_cell();
                } else {
                    self.finish_line();
                }
            }
        }
    }

    /// Closes the innermost open `kind` and everything inside it. With `stop`,
    /// looks no further out than the nearest of those kinds.
    fn close(&mut self, kind: Open, stop: &[Open]) {
        let found = self
            .open
            .iter()
            .rev()
            .take_while(|(k, _)| !stop.contains(k))
            // the `div` Evernote marks as a code block closes with `</div>`
            .position(|(k, _)| *k == kind || (kind == Open::Line && *k == Open::Pre));
        if let Some(from_top) = found {
            for _ in 0..=from_top {
                if let Some((k, undo)) = self.open.pop() {
                    self.leave(k, undo);
                }
            }
        }
    }

    fn start(&mut self, e: &BytesStart<'_>) {
        let local = e.local_name();
        let local: &str = local.as_ref();
        let name = local.to_ascii_lowercase();
        match name.as_str() {
            "br" => return self.line_break(),
            "hr" => {
                if !self.in_cell() && self.pre.is_none() {
                    self.end_para();
                    // not `---`, which would make the line above a heading
                    self.emit("***");
                }
                return;
            }
            "en-todo" => return self.todo(e),
            "en-media" => {
                // `attr` keeps the case; Evernote writes lowercase hex, others may not
                let hash = attr(e, "hash").unwrap_or_default();
                self.media.push(hash.trim().to_ascii_lowercase());
                return;
            }
            "img" => return self.notes.push(WEB_IMAGE),
            "u" | "ins" | "sub" | "sup" | "mark" | "small" | "big" | "font" => {
                self.note_once(UNSUPPORTED_FORMATTING)
            }
            "span" if attr(e, "style").is_some() => self.note_once(UNSUPPORTED_FORMATTING),
            _ => {}
        }
        let Some(mut kind) = Open::of(&name) else {
            return;
        };
        if self.open.len() >= MAX_OPEN_ELEMENTS {
            return;
        }
        if kind == Open::Line
            && attr(e, "style").is_some_and(|style| style.contains("en-codeblock"))
        {
            kind = Open::Pre;
        }
        // unclosed siblings end where the next one starts, as in HTML
        match kind {
            Open::Paragraph => self.close(kind, &[Open::Item, Open::Cell, Open::Quote]),
            Open::Item => self.close(kind, &[Open::List]),
            Open::Row => self.close(kind, &[Open::Table]),
            Open::Cell => self.close(kind, &[Open::Row, Open::Table]),
            _ => {}
        }
        let undo = self.enter(kind, &name, e);
        self.open.push((kind, undo));
    }

    fn end(&mut self, name: &str) {
        if let Some(kind) = Open::of(&name.to_ascii_lowercase()) {
            self.close(kind, &[]);
        }
    }

    fn line_break(&mut self) {
        if let Some(pre) = &mut self.pre {
            pre.push('\n');
        } else if self.runs.is_empty()
            && self.task.is_none()
            && !self.in_cell()
            && self.heading.is_none()
        {
            // Evernote's blank line is a `div` holding only a `br`
            self.flush_para();
        } else {
            self.finish_line();
        }
    }

    fn todo(&mut self, e: &BytesStart<'_>) {
        let done = attr(e, "checked").is_some_and(|c| c.trim().eq_ignore_ascii_case("true"));
        if self.in_cell() {
            self.text(if done { "[x] " } else { "[ ] " });
        } else {
            self.finish_line();
            self.task = Some(done);
        }
    }

    fn finish(mut self) -> Converted {
        while let Some((kind, undo)) = self.open.pop() {
            self.leave(kind, undo);
        }
        self.end_para();
        Converted {
            chunks: self.chunks,
            media: self.media,
            notes: self.notes,
        }
    }
}

/// Converts ENML that has already been through [`strip_hidden`]. Errors when it
/// can't be tokenised, or uses an entity [`entity`] doesn't know.
fn convert(enml: &str) -> Result<Converted, ()> {
    let mut reader = Reader::from_str(enml);
    let config = reader.config_mut();
    config.check_end_names = false;
    config.allow_unmatched_ends = true;
    config.allow_dangling_amp = true;
    config.expand_empty_elements = true;
    let mut out = Enml::new();
    loop {
        match reader.read_event().map_err(|_| ())? {
            Event::Start(e) => out.start(&e),
            Event::End(e) => {
                let local = e.local_name();
                out.end(local.as_ref());
            }
            Event::Text(t) => out.text(&t.xml10_content()),
            Event::CData(t) => out.text(&t.xml10_content()),
            Event::GeneralRef(r) => {
                let name = r.xml10_content();
                let name: &str = name.as_ref();
                match entity(name) {
                    Some(c) => out.text(c.encode_utf8(&mut [0; 4])),
                    // `AT&T rocks;` is a stray ampersand, not an entity
                    None if name.contains(|c: char| !c.is_alphanumeric() && c != '#') => {
                        out.text(&format!("&{name};"))
                    }
                    None => return Err(()),
                }
            }
            Event::Eof => return Ok(out.finish()),
            _ => {}
        }
    }
}

/// Markdown blocks for a note that [`convert`] refused.
fn plain_blocks(enml: &str) -> Vec<String> {
    let escaped: Vec<String> = plain_text(enml)
        .split('\n')
        .map(|line| {
            let line: String = line
                .chars()
                .map(|c| if c == '\u{a0}' { ' ' } else { c })
                .filter(|c| !c.is_control() || *c == '\t')
                .collect();
            escape_markdown(line.trim())
        })
        .collect();
    blocks(&escaped.join("\n"))
}

/// An image ready to be placed.
#[derive(Clone)]
pub(crate) struct Image {
    pub(crate) asset: String,
    pub(crate) width: u64,
    pub(crate) height: u64,
}

/// A resource of the note being read, by the MD5 of its bytes.
struct Resource {
    /// Position in the note, for images nothing in the body refers to.
    order: usize,
    /// Set when the resource is a usable image.
    image: Option<Image>,
    /// Set when the resource is a readable PDF: its pages.
    pdf: Vec<ImportedPage>,
    referenced: bool,
}

/// The note being read.
#[derive(Default)]
struct Note {
    title: String,
    content: String,
    has_dates: bool,
    has_details: bool,
    tags: u32,
    resources: BTreeMap<String, Resource>,
    /// Resources whose bytes were never decoded, so have no hash to match.
    unreadable: u32,
}

/// The resource being read.
#[derive(Default)]
struct RawResource {
    mime: String,
    data: String,
    oversized: bool,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Field {
    #[default]
    None,
    Title,
    Content,
    Mime,
    Data,
}

/// Size of an image on the page: its natural size, shrunk to fit the margins.
fn fitted(image: &Image) -> (f64, f64) {
    let size = page_size();
    let (width, height) = (
        image.width as f64 * POINTS_PER_PIXEL,
        image.height as f64 * POINTS_PER_PIXEL,
    );
    let scale = ((size.width - 2.0 * MARGIN) / width)
        .min((size.height - 2.0 * MARGIN) / height)
        .min(1.0);
    (width * scale, height * scale)
}

fn page_limit() -> ImportError {
    ImportError::TooLarge("more than 10 000 pages".into())
}

fn blank_page() -> ImportedPage {
    ImportedPage {
        size: page_size(),
        template: Template::default(),
        elements: Vec::new(),
        pdf: None,
    }
}

/// Where the next image can go on a page: below everything already on it.
fn next_free_y(page: &ImportedPage) -> f64 {
    page.elements
        .iter()
        .map(|e| match e {
            Element::Text(t) => t.y + t.height + IMAGE_GAP,
            Element::Image(i) => i.y + i.height + IMAGE_GAP,
            Element::Ink(_) => MARGIN,
        })
        .fold(MARGIN, f64::max)
}

/// Puts an image below what the last page holds, or on a new page if it won't fit.
pub(crate) fn place_image(pages: &mut Vec<ImportedPage>, image: &Image) -> Result<(), ImportError> {
    let (width, height) = fitted(image);
    let bottom = page_size().height - MARGIN;
    let fits =
        |page: &ImportedPage| page.elements.is_empty() || next_free_y(page) + height <= bottom;
    if !pages.last().is_some_and(fits) {
        if pages.len() >= model::MAX_PAGES {
            return Err(page_limit());
        }
        pages.push(blank_page());
    }
    let Some(page) = pages.last_mut() else {
        return Ok(());
    };
    let y = next_free_y(page);
    page.elements.push(Element::Image(ImageElement {
        id: Uuid::new_v4(),
        z: page.elements.len() as i64 + 1,
        x: MARGIN,
        y,
        width,
        height,
        asset_hash: image.asset.clone(),
        pdf_page: None,
        extra: Default::default(),
    }));
    Ok(())
}

/// The file as read so far.
#[derive(Default)]
struct Import {
    log: LossLog,
    pages: Vec<ImportedPage>,
    assets: BTreeMap<String, ImportedAsset>,
    note: Option<Note>,
    resource: Option<RawResource>,
    field: Field,
    field_depth: usize,
    buffer: String,
    saw_root: bool,
    saw_note: bool,
}

impl Import {
    fn capture(&mut self, field: Field, depth: usize) {
        self.field = field;
        self.field_depth = depth;
        self.buffer.clear();
    }

    fn open(&mut self, name: &str, depth: usize) -> Result<(), ImportError> {
        match (depth, name) {
            (1, "en-export") => self.saw_root = true,
            (2, "note") if self.saw_root => {
                self.note = Some(Note::default());
                self.saw_note = true;
            }
            _ => {}
        }
        let Some(note) = &mut self.note else {
            return Ok(());
        };
        match (depth, name, self.resource.is_some()) {
            (3, "title", _) => self.capture(Field::Title, depth),
            (3, "content", _) => self.capture(Field::Content, depth),
            (3, "created" | "updated", _) => note.has_dates = true,
            (3, "tag", _) => note.tags += 1,
            (3, "resource", _) => {
                if note.resources.len() >= MAX_RESOURCES_PER_NOTE {
                    return Err(ImportError::TooLarge(
                        "a note has too many attachments".into(),
                    ));
                }
                self.resource = Some(RawResource::default());
            }
            (4, "mime", true) => self.capture(Field::Mime, depth),
            (4, "data", true) => self.capture(Field::Data, depth),
            (4, _, false) => note.has_details = true,
            _ => {}
        }
        Ok(())
    }

    /// Adds text to the field being captured. A resource over its limit is
    /// forgotten and counted later; text that long ends the import.
    fn push(&mut self, s: &str) -> Result<(), ImportError> {
        match self.field {
            Field::None => {}
            Field::Data => {
                let Some(resource) = &mut self.resource else {
                    return Ok(());
                };
                resource.oversized |= resource.data.len() + s.len() > MAX_BASE64_BYTES;
                if resource.oversized {
                    resource.data = String::new();
                } else {
                    resource.data.push_str(s);
                }
            }
            _ => {
                if self.buffer.len() + s.len() > limits::MAX_TEXT_FILE {
                    return Err(ImportError::TooLarge("a note is over 64 MiB".into()));
                }
                self.buffer.push_str(s);
            }
        }
        Ok(())
    }

    fn close(&mut self, name: &str, depth: usize) -> Result<(), ImportError> {
        if self.field != Field::None && depth == self.field_depth {
            let text = std::mem::take(&mut self.buffer);
            match (self.field, &mut self.note, &mut self.resource) {
                (Field::Title, Some(note), _) => note.title = text,
                (Field::Content, Some(note), _) => note.content = text,
                (Field::Mime, _, Some(resource)) => resource.mime = text,
                _ => {}
            }
            self.field = Field::None;
        }
        match (depth, name) {
            (3, "resource") => {
                if let Some(resource) = self.resource.take() {
                    self.finish_resource(resource);
                }
            }
            (2, "note") => {
                if let Some(note) = self.note.take() {
                    self.finish_note(note)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn finish_resource(&mut self, raw: RawResource) {
        let Some(note) = &mut self.note else {
            return;
        };
        let is_image = raw.mime.trim().starts_with("image/");
        let unusable = if is_image {
            "{n} image attachment(s) were damaged, of an unsupported type or too large, and were left out."
        } else {
            "{n} attachment(s), such as PDFs, audio or oversized files, were not imported."
        };
        let decoded = (!raw.oversized).then(|| base64(&raw.data)).flatten();
        let Some(bytes) = decoded else {
            self.log.note(unusable);
            note.unreadable += 1;
            return;
        };
        drop(raw.data);
        let md5: String = Md5::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let order = note.resources.len();
        if raw.mime.trim() == "application/pdf" {
            match pdf_attachment(bytes) {
                Some((asset, pdf)) => {
                    self.assets.entry(asset.sha256.clone()).or_insert(asset);
                    note.resources.entry(md5).or_insert(Resource {
                        order,
                        image: None,
                        pdf,
                        referenced: false,
                    });
                }
                None => self.log.note(unusable),
            }
            return;
        }
        let checked = check_image(&bytes)
            .ok()
            .and_then(|media| Some((media, pixel_size(&bytes)?)));
        let image = match checked {
            Some((media, (width, height))) => {
                let asset = ImportedAsset::new(bytes, media);
                let hash = asset.sha256.clone();
                self.assets.entry(hash.clone()).or_insert(asset);
                Some(Image {
                    asset: hash,
                    width,
                    height,
                })
            }
            None => {
                self.log.note(unusable);
                None
            }
        };
        note.resources.entry(md5).or_insert(Resource {
            order,
            image,
            pdf: Vec::new(),
            referenced: false,
        });
    }

    fn finish_note(&mut self, mut note: Note) -> Result<(), ImportError> {
        for _ in 0..note.tags {
            self.log.note("{n} note tag(s) were not imported.");
        }
        if note.has_dates {
            self.log
                .note("{n} note(s) lost their created and updated dates.");
        }
        if note.has_details {
            self.log
                .note("{n} note(s) lost details such as author and source link.");
        }
        let (body, encrypted) = strip_hidden(&note.content);
        for _ in 0..encrypted {
            self.log
                .note("{n} encrypted section(s) can't be read and were left out.");
        }
        let title = note.title.split_whitespace().collect::<Vec<_>>().join(" ");
        let title: String = title.chars().filter(|c| !c.is_control()).collect();
        let title = if title.is_empty() {
            "Untitled note".to_string()
        } else {
            escape_markdown(&title)
        };
        let mut chunks = vec![heading_line(1, &title)];
        let media = match convert(&body) {
            Ok(converted) => {
                chunks.extend(converted.chunks);
                for sentence in converted.notes {
                    self.log.note(sentence);
                }
                converted.media
            }
            Err(()) => {
                self.log
                    .note("{n} note(s) were damaged and were kept as plain text.");
                chunks.extend(plain_blocks(&body));
                Vec::new()
            }
        };
        let pages = paginate(chunks)?;
        if self.pages.len() + pages.len() > model::MAX_PAGES {
            return Err(page_limit());
        }
        self.pages.extend(pages);

        let mut images = Vec::new();
        let mut missing = 0u32;
        for hash in &media {
            match note.resources.get_mut(hash) {
                Some(resource) => {
                    resource.referenced = true;
                    images.extend(resource.image.clone());
                }
                None => missing += 1,
            }
        }
        // the plain text path finds no references, and some files carry images
        // no note refers to: either way they are the user's, so they stay
        let mut resources: Vec<_> = note.resources.into_values().collect();
        resources.sort_by_key(|r| r.order);
        let pdf_pages: Vec<_> = resources
            .iter_mut()
            .flat_map(|r| std::mem::take(&mut r.pdf))
            .collect();
        images.extend(
            resources
                .into_iter()
                .filter(|r| !r.referenced)
                .filter_map(|r| r.image),
        );
        // a reference to a resource already counted as lost isn't a second loss
        for _ in 0..missing.saturating_sub(note.unreadable) {
            self.log
                .note("{n} reference(s) to attachments missing from the file were dropped.");
        }
        if !images.is_empty() {
            self.log.note_once(
                "Images were moved to the end of their note, so their place in the text was not kept.",
            );
        }
        for image in &images {
            self.place(image)?;
        }
        if !pdf_pages.is_empty() {
            self.log
                .note_once("PDF attachments became pages after their note.");
        }
        if self.pages.len() + pdf_pages.len() > model::MAX_PAGES {
            return Err(page_limit());
        }
        self.pages.extend(pdf_pages);
        Ok(())
    }

    /// Puts an image below the last thing on the last page, or on a new page
    /// when it doesn't fit, so nothing overlaps.
    fn place(&mut self, image: &Image) -> Result<(), ImportError> {
        place_image(&mut self.pages, image)
    }

    fn finish(self, title: &str) -> Result<ImportedDocument, ImportError> {
        if !self.saw_root {
            return Err(malformed("it is not an Evernote export"));
        }
        if !self.saw_note {
            return Err(malformed("it has no notes"));
        }
        let mut kept = Kept {
            pages: self.pages.len() as u32,
            ..Kept::default()
        };
        for element in self.pages.iter().flat_map(|p| &p.elements) {
            match element {
                Element::Text(_) => kept.text_blocks += 1,
                Element::Image(_) => kept.images += 1,
                Element::Ink(_) => kept.strokes += 1,
            }
        }
        Ok(ImportedDocument {
            title: title.to_string(),
            pages: self.pages,
            assets: self.assets.into_values().collect(),
            report: ImportLoss {
                format: "Evernote".into(),
                kept,
                lost: self.log.sentences(),
            },
        })
    }
}

/// Some exports wrap note bodies that already hold CDATA in another CDATA section, which
/// ends the outer one early. This takes the inner markers out of every `<content>`.
fn repair_nested_cdata(xml: &str) -> Cow<'_, str> {
    const OPEN: &str = "<![CDATA[";
    const CLOSE: &str = "]]>";
    let mut out = String::new();
    let mut copied = 0;
    let mut from = 0;
    while let Some(start) = xml[from..].find("<content>").map(|i| i + from) {
        let Some(end) = xml[start..].find("</content>").map(|i| i + start) else {
            break;
        };
        from = end;
        let body = &xml[start..end];
        let (Some(first), Some(last)) = (body.find(OPEN), body.rfind(CLOSE)) else {
            continue;
        };
        if first + OPEN.len() > last {
            continue;
        }
        let inner = &body[first + OPEN.len()..last];
        if inner.contains(OPEN) || inner.contains(CLOSE) {
            out.push_str(&xml[copied..start + first + OPEN.len()]);
            out.push_str(&inner.replace(OPEN, "").replace(CLOSE, ""));
            copied = start + last;
        }
    }
    if copied == 0 {
        return Cow::Borrowed(xml);
    }
    out.push_str(&xml[copied..]);
    Cow::Owned(out)
}

/// Imports an Evernote `.enex` export as one notebook, with `title` as its
/// title and a new page for each note. Errors when the file is too large or
/// isn't a readable export. A note whose body can't be parsed is kept as plain
/// text, and everything lost is listed in the report.
pub fn import_enex(bytes: &[u8], title: &str) -> Result<ImportedDocument, ImportError> {
    if bytes.len() as u64 > limits::MAX_ENEX_BYTES {
        return Err(ImportError::TooLarge("over 1 GiB".into()));
    }
    let xml = std::str::from_utf8(bytes).map_err(malformed)?;
    let xml = repair_nested_cdata(xml.strip_prefix('\u{feff}').unwrap_or(xml));
    let mut reader = Reader::from_str(&xml);
    // Real exports contain bare `&` in metadata and stray closing tags, and neither
    // makes the notes unreadable. Open elements are tracked below instead, so a file
    // that ends early is still refused.
    reader.config_mut().allow_dangling_amp = true;
    reader.config_mut().check_end_names = false;
    reader.config_mut().allow_unmatched_ends = true;
    let mut import = Import::default();
    let mut open: Vec<String> = Vec::new();
    loop {
        match reader.read_event().map_err(malformed)? {
            Event::Start(e) => {
                let local = e.local_name();
                let name: &str = local.as_ref();
                open.push(name.to_string());
                import.open(name, open.len())?;
            }
            Event::Empty(e) => {
                let local = e.local_name();
                import.open(local.as_ref(), open.len() + 1)?;
                import.close(local.as_ref(), open.len() + 1)?;
            }
            Event::End(e) => {
                let local = e.local_name();
                let name: &str = local.as_ref();
                // closes anything left open inside it; a tag that closes nothing is ignored
                if let Some(at) = open.iter().rposition(|n| n == name) {
                    while open.len() > at {
                        let depth = open.len();
                        let closing = open.pop().expect("not empty");
                        import.close(&closing, depth)?;
                    }
                }
            }
            Event::Text(t) => import.push(&t.xml10_content())?,
            Event::CData(t) => import.push(&t.xml10_content())?,
            Event::GeneralRef(r) => {
                let name = r.xml10_content();
                if let Some(c) = r.resolve_char_ref().map_err(malformed)? {
                    import.push(c.encode_utf8(&mut [0; 4]))?;
                } else if let Some(s) = quick_xml::escape::resolve_xml_entity(name.as_ref()) {
                    import.push(s)?;
                } else {
                    return Err(malformed(format!("unknown entity {name}")));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !open.is_empty() {
        return Err(malformed("the file ends early"));
    }
    import.finish(title)
}
