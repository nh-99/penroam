//! Deterministic Markdown layout for text blocks.
//!
//! A [`TextBlock`](crate::model::TextBlock)'s `text` is Markdown source. This
//! module turns it into a [`Paint`]: positioned text runs, line segments and
//! formula placements, in points relative to the block's top-left corner. The
//! PDF exporter draws a `Paint` directly; the editor draws the one produced by
//! the TypeScript port in `apps/desktop-mobile/src/editor/markdown.ts`. Both
//! are checked against `tests/fixtures/markdown-layout-golden.json`, so a block
//! looks the same on screen and on paper on every OS. See
//! `docs/formats/model-and-archive.md` for the rules.
//!
//! The steps are:
//!
//! 1. [`extract_math`] replaces formulas with private-use characters, using a
//!    small scanner that exists in both languages, so the two Markdown parsers
//!    never see TeX and cannot disagree about it.
//! 2. `pulldown-cmark` (CommonMark, GFM tables, strikethrough and task lists)
//!    produces a block tree. The TypeScript side uses `markdown-it` to build the
//!    same tree.
//! 3. The layout walks the tree with one vertical cursor.
//!
//! Invariants that matter:
//!
//! * A soft line break is a real line break. Text blocks are notes, and pressing
//!   Enter has to start a new line. A blank line renders as one blank line.
//! * With a row pitch, every line takes a whole number of rows and its baseline
//!   sits on the bottom row of that span. Gaps are one row. The block's `y` is
//!   therefore on a row boundary and all its baselines stay on the paper's rows.
//! * Containers nested past [`MAX_DEPTH`] are flattened, which keeps a hostile
//!   document from exhausting the stack and keeps both ports in step.
//! * Layout never reads the platform's fonts: every width is in
//!   [`crate::text::metrics`]. Each character's face comes from
//!   [`Face::styled`], so handwritten text falls back to the formatted font
//!   one character at a time, and a run never mixes the two.
//! * List bullets and numbers are text in the block's font; task checkboxes,
//!   link underlines and every other decoration are [`Segment`]s. Checkboxes in
//!   handwritten text are sketched instead of ruled: fixed strokes, never
//!   random, so both ports and every export draw the same marks.
//! * A table is a small spreadsheet ([`crate::sheet`]): a cell whose source
//!   starts with `=` shows its formula's result, and a cell that is just
//!   `[ ]` or `[x]` is a checkbox drawn like a task's. Both read the cell's
//!   source text, not its parsed inlines, so `=A2*B2` never turns italic.
//! * Formula sizes come from the caller. Without one (the math renderer hasn't
//!   run, or the formula is invalid) the source is shown in the code face.

use crate::model::{TextAlign, TextFont};
use crate::sheet;
use crate::text::{metrics, Face, MONO_SCALE};
use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use std::ops::Range;

/// Deepest container nesting that is laid out; deeper content joins its parent.
pub const MAX_DEPTH: usize = 16;

const SLOT_FIRST: u32 = 0xF0000;
const SLOT_LAST: u32 = 0xFFFFD;
/// Heading sizes relative to the block's font size.
const HEADING_SCALE: [f64; 6] = [1.75, 1.5, 1.25, 1.1, 1.0, 0.9];
/// How far below a baseline a row-locked frame or bar reaches, in em. Without it
/// frames would sit on the text's baseline.
const ROW_FRAME_DROP: f64 = 0.22;
/// Line weight of frames and decorations, in em.
const STROKE_EM: f64 = 0.06;
/// A sketched checkbox for handwritten text, as polylines in the box's unit
/// square (y down). The sides bow a little and overshoot their corners, as a
/// square drawn in four strokes does.
const HAND_BOX: [&[(f64, f64)]; 4] = [
    &[(-0.05, 0.04), (0.5, -0.03), (1.04, 0.01)],
    &[(0.97, -0.06), (1.02, 0.48), (0.98, 1.05)],
    &[(1.04, 0.98), (0.48, 1.03), (-0.03, 0.96)],
    &[(0.02, 1.04), (-0.02, 0.52), (0.03, -0.05)],
];
/// How much heavier a sketched checkbox's strokes are than a ruled one's, to
/// match the weight of the handwritten letters beside it.
const HAND_WEIGHT: f64 = 1.5;
/// The tick of a sketched checkbox, in the same unit square: a short stroke
/// down, then a long one that flicks past the box's top right corner.
const HAND_TICK: &[(f64, f64)] = &[(0.12, 0.5), (0.38, 0.84), (0.66, 0.36), (1.14, -0.2)];

/// A formula found in Markdown source.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MathSpan {
    /// TeX, without delimiters.
    pub tex: String,
    /// Set off on its own line (`$$…$$`), not inline (`$…$`).
    pub display: bool,
    /// The original text with delimiters, shown wherever the formula can't be.
    pub source: String,
}

/// Size of a rendered formula in em, measured from its baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MathBox {
    /// Advance width.
    pub width: f64,
    /// Extent above the baseline.
    pub height: f64,
    /// Extent below the baseline.
    pub depth: f64,
}

/// A run of one face and size, drawn from its left end on `y`, the baseline.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    /// Left end in points.
    pub x: f64,
    /// Baseline in points.
    pub y: f64,
    /// Typeface.
    pub face: Face,
    /// Font size in points.
    pub size: f64,
    /// The characters.
    pub text: String,
}

/// A straight line in the block's colour: rules, frames, decorations.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    /// Start x.
    pub x0: f64,
    /// Start y.
    pub y0: f64,
    /// End x.
    pub x1: f64,
    /// End y.
    pub y1: f64,
    /// Line weight in points.
    pub width: f64,
}

/// A formula placed on a line.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MathMark {
    /// Left end in points.
    pub x: f64,
    /// Baseline in points.
    pub y: f64,
    /// Font size in points, reduced when the formula would not otherwise fit.
    pub size: f64,
    /// TeX.
    pub tex: String,
    /// Display style.
    pub display: bool,
}

/// Everything needed to draw a block, relative to its top-left corner.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Paint {
    /// Height of the laid-out content.
    pub height: f64,
    /// Lowest ink. Past `height` when descenders hang below the last row.
    pub bottom: f64,
    /// Text.
    pub runs: Vec<Run>,
    /// Lines.
    pub segments: Vec<Segment>,
    /// Formulas.
    pub math: Vec<MathMark>,
}

fn is_slot(c: char) -> bool {
    (SLOT_FIRST..=SLOT_LAST).contains(&(c as u32))
}

fn slot_index(c: char) -> usize {
    (c as u32 - SLOT_FIRST) as usize
}

/// Length of a fence marker at the start of `line` (after up to three spaces),
/// with its character and whether only spaces follow it.
fn fence_at(line: &[char]) -> Option<(char, usize, bool)> {
    let start = line.iter().position(|&c| c != ' ')?;
    if start > 3 || !matches!(line[start], '`' | '~') {
        return None;
    }
    let ch = line[start];
    let len = line[start..].iter().take_while(|&&c| c == ch).count();
    if len < 3 {
        return None;
    }
    let bare = line[start + len..].iter().all(|c| c.is_whitespace());
    Some((ch, len, bare))
}

/// The end of the formula opening at `chars[i]`: (TeX start, TeX end, next index, display).
fn scan_math(chars: &[char], i: usize) -> Option<(usize, usize, usize, bool)> {
    let n = chars.len();
    if chars.get(i + 1) == Some(&'$') {
        let mut j = i + 2;
        while j < n {
            match chars[j] {
                '\\' => j += 2,
                '$' if chars.get(j + 1) == Some(&'$') => return Some((i + 2, j, j + 2, true)),
                '\n' => {
                    // a blank line ends the paragraph, and the formula with it
                    let mut k = j + 1;
                    while k < n && chars[k] != '\n' && chars[k].is_whitespace() {
                        k += 1;
                    }
                    if k >= n || chars[k] == '\n' {
                        return None;
                    }
                    j += 1;
                }
                _ => j += 1,
            }
        }
        return None;
    }
    if chars.get(i + 1).is_none_or(|c| c.is_whitespace()) {
        return None;
    }
    let mut j = i + 1;
    while j < n {
        match chars[j] {
            '\n' => return None,
            '\\' => j += 2,
            '$' => {
                // "$5 and $10": a price closes nothing
                let closes = !chars[j - 1].is_whitespace()
                    && !chars.get(j + 1).is_some_and(|c| c.is_ascii_digit());
                return closes.then_some((i + 1, j, j + 1, false));
            }
            _ => j += 1,
        }
    }
    None
}

/// Replaces each formula in `src` with one private-use character, returning the
/// new source and the formulas (slot `n` is formula `n`).
///
/// `$…$` must close on its line, opens before a non-space, closes after one and
/// not before a digit. `$$…$$` may span lines up to a blank line. `\$`, code
/// spans and fenced code are left alone. Private-use characters already in the
/// source are replaced, so a document can't forge a slot.
pub fn extract_math(src: &str) -> (String, Vec<MathSpan>) {
    let chars: Vec<char> = src
        .chars()
        .map(|c| if is_slot(c) { '\u{FFFD}' } else { c })
        .collect();
    let n = chars.len();
    let mut out = String::with_capacity(src.len());
    let mut spans = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut line_start = true;
    let mut i = 0;
    while i < n {
        if line_start {
            line_start = false;
            let end = chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(n, |p| i + p);
            let marker = fence_at(&chars[i..end]);
            let was_fenced = fence.is_some();
            match (fence, marker) {
                (None, Some((ch, len, _))) => fence = Some((ch, len)),
                (Some((c, l)), Some((ch, len, true))) if c == ch && len >= l => fence = None,
                _ => {}
            }
            if was_fenced || fence.is_some() {
                out.extend(&chars[i..end]);
                if end < n {
                    out.push('\n');
                }
                i = end + 1;
                line_start = true;
                continue;
            }
        }
        match chars[i] {
            '\n' => {
                out.push('\n');
                line_start = true;
                i += 1;
            }
            '\\' => {
                out.push('\\');
                i += 1;
                if i < n && chars[i] != '\n' {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '`' => {
                let run = chars[i..].iter().take_while(|&&c| c == '`').count();
                let close = find_backticks(&chars, i + run, run);
                let end = close.unwrap_or(i + run);
                out.extend(&chars[i..end]);
                i = end;
            }
            '$' => {
                let found = scan_math(&chars, i).and_then(|(s, e, next, display)| {
                    let tex: String = chars[s..e].iter().collect::<String>().trim().to_owned();
                    (!tex.is_empty() && spans.len() <= (SLOT_LAST - SLOT_FIRST) as usize)
                        .then_some((tex, next, display))
                });
                if let Some((tex, next, display)) = found {
                    let slot = char::from_u32(SLOT_FIRST + spans.len() as u32).expect("in range");
                    out.push(slot);
                    spans.push(MathSpan {
                        tex,
                        display,
                        source: chars[i..next].iter().collect(),
                    });
                    i = next;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    (out, spans)
}

/// The index just past a closing run of exactly `run` backticks on this line.
fn find_backticks(chars: &[char], from: usize, run: usize) -> Option<usize> {
    let mut j = from;
    while j < chars.len() && chars[j] != '\n' {
        if chars[j] == '`' {
            let len = chars[j..].iter().take_while(|&&c| c == '`').count();
            if len == run {
                return Some(j + len);
            }
            j += len;
        } else {
            j += 1;
        }
    }
    None
}

/// The formulas in `text`, for rendering ahead of a PDF export.
pub fn math_in(text: &str) -> Vec<MathSpan> {
    extract_math(text).1
}

const BOLD: u8 = 1;
const ITALIC: u8 = 2;
const MONO: u8 = 4;
const STRIKE: u8 = 8;
const UNDERLINE: u8 = 16;

#[derive(Debug, Clone, PartialEq)]
enum Inline {
    Text(String, u8),
    Break,
    Math(usize),
    /// A spreadsheet checkbox, the whole of its table cell.
    Check(bool),
}

/// A table row: its cells' inline content.
type Row = Vec<Vec<Inline>>;

#[derive(Debug)]
struct Item {
    task: Option<bool>,
    blocks: Vec<Block>,
}

#[derive(Debug)]
enum Block {
    Para {
        inlines: Vec<Inline>,
        level: usize,
    },
    Code(String),
    Rule,
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        tight: bool,
        items: Vec<Item>,
    },
    Table {
        aligns: Vec<TextAlign>,
        rows: Vec<Row>,
    },
}

#[derive(Debug)]
enum Frame {
    Root(Vec<Block>),
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        tight: bool,
        items: Vec<Item>,
    },
    Item(Item),
}

/// Builds the block tree from parser events.
struct Builder<'a> {
    spans: &'a [MathSpan],
    /// The source the parser reads, formulas already replaced by slots.
    src: &'a str,
    stack: Vec<Frame>,
    /// One entry per open container start: whether it pushed a frame.
    pushed: Vec<bool>,
    inlines: Vec<Inline>,
    style: [u32; 5],
    level: usize,
    code: Option<String>,
    html: Option<String>,
    table: Option<(Vec<TextAlign>, Vec<Row>)>,
    row: Row,
    /// Source of the open table's cells, row by row, for [`sheet`].
    raws: Vec<Vec<String>>,
    raw_row: Vec<String>,
    /// Where the open table cell is in `src`.
    cell: Range<usize>,
}

impl Builder<'_> {
    fn flags(&self) -> u8 {
        self.style
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .fold(0, |f, (i, _)| f | 1 << i)
    }

    /// `text` with its slots back in source form, for literal contexts.
    fn restore(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match self.spans.get(if is_slot(c) {
                slot_index(c)
            } else {
                usize::MAX
            }) {
                Some(m) => out.push_str(&m.source),
                None => out.push(c),
            }
        }
        out
    }

    fn push_text(&mut self, text: &str) {
        let flags = self.flags();
        let mut cur = String::new();
        for c in text.chars() {
            if is_slot(c) && slot_index(c) < self.spans.len() {
                if !cur.is_empty() {
                    self.inlines
                        .push(Inline::Text(std::mem::take(&mut cur), flags));
                }
                self.inlines.push(Inline::Math(slot_index(c)));
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            self.inlines.push(Inline::Text(cur, flags));
        }
    }

    fn blocks(&mut self) -> &mut Vec<Block> {
        match self.stack.last_mut().expect("root frame") {
            Frame::Root(b) | Frame::Quote(b) => b,
            Frame::Item(i) => &mut i.blocks,
            Frame::List { items, .. } => {
                // A list holds only items; a flattened child lands in the last one.
                if items.is_empty() {
                    items.push(Item {
                        task: None,
                        blocks: Vec::new(),
                    });
                }
                &mut items.last_mut().expect("just pushed").blocks
            }
        }
    }

    /// Closes the open inline content: a paragraph, or a tight item's bare text.
    fn flush(&mut self) {
        if self.inlines.is_empty() {
            return;
        }
        let inlines = std::mem::take(&mut self.inlines);
        let level = std::mem::take(&mut self.level);
        self.blocks().push(Block::Para { inlines, level });
    }

    fn open(&mut self, frame: Frame) {
        self.flush();
        let room = self.stack.len() <= MAX_DEPTH;
        self.pushed.push(room);
        if room {
            self.stack.push(frame);
        }
    }

    fn close(&mut self) {
        self.flush();
        if !self.pushed.pop().unwrap_or(false) {
            return;
        }
        let frame = self.stack.pop().expect("frame for every push");
        match frame {
            Frame::Quote(b) => self.blocks().push(Block::Quote(b)),
            Frame::List {
                start,
                tight,
                items,
            } => self.blocks().push(Block::List {
                start,
                tight,
                items,
            }),
            Frame::Item(item) => match self.stack.last_mut() {
                Some(Frame::List { items, .. }) => items.push(item),
                _ => self.blocks().extend(item.blocks),
            },
            Frame::Root(_) => unreachable!("the root frame stays"),
        }
    }

    fn event(&mut self, ev: Event<'_>, range: Range<usize>) {
        match ev {
            Event::Start(Tag::TableCell) => {
                self.inlines.clear();
                self.cell = range;
            }
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(s) => {
                if let Some(c) = &mut self.code {
                    c.push_str(&s);
                } else if let Some(h) = &mut self.html {
                    h.push_str(&s); // the block's indent
                } else {
                    self.push_text(&s);
                }
            }
            Event::Code(s) => {
                let text = self.restore(&s);
                self.inlines.push(Inline::Text(text, self.flags() | MONO));
            }
            Event::Html(s) => {
                if let Some(h) = &mut self.html {
                    h.push_str(&s);
                }
            }
            Event::InlineHtml(s) => {
                let text = self.restore(&s);
                self.inlines.push(Inline::Text(text, self.flags()));
            }
            Event::SoftBreak | Event::HardBreak => self.inlines.push(Inline::Break),
            Event::Rule => {
                self.flush();
                self.blocks().push(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item(i)) = self.stack.last_mut() {
                    i.task = Some(done);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.flush();
                if let Some(Frame::List { tight, .. }) = self.stack.iter_mut().rev().nth(1) {
                    *tight = false;
                }
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
            }
            Tag::BlockQuote(_) => self.open(Frame::Quote(Vec::new())),
            Tag::List(start) => self.open(Frame::List {
                start,
                tight: true,
                items: Vec::new(),
            }),
            Tag::Item => self.open(Frame::Item(Item {
                task: None,
                blocks: Vec::new(),
            })),
            Tag::CodeBlock(_) => {
                self.flush();
                self.code = Some(String::new());
            }
            Tag::HtmlBlock => {
                self.flush();
                self.html = Some(String::new());
            }
            Tag::Table(aligns) => {
                self.flush();
                let aligns = aligns
                    .iter()
                    .map(|a| match a {
                        Alignment::Center => TextAlign::Center,
                        Alignment::Right => TextAlign::Right,
                        _ => TextAlign::Left,
                    })
                    .collect();
                self.table = Some((aligns, Vec::new()));
                self.raws.clear();
            }
            Tag::Emphasis => self.style[1] += 1,
            Tag::Strong => self.style[0] += 1,
            Tag::Strikethrough => self.style[3] += 1,
            Tag::Link { .. } => self.style[4] += 1,
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.level = 0; // an empty heading flushes nothing
            }
            TagEnd::BlockQuote(_) | TagEnd::List(_) | TagEnd::Item => self.close(),
            TagEnd::CodeBlock => {
                let code = self.code.take().unwrap_or_default();
                let text = self.restore(code.strip_suffix('\n').unwrap_or(&code));
                self.blocks().push(Block::Code(text));
            }
            TagEnd::HtmlBlock => {
                let html = self.html.take().unwrap_or_default();
                let text = self.restore(html.trim_end());
                let mut inlines = Vec::new();
                for (i, line) in text.split('\n').enumerate() {
                    if i > 0 {
                        inlines.push(Inline::Break);
                    }
                    let line = line.trim_start();
                    if !line.is_empty() {
                        inlines.push(Inline::Text(line.to_owned(), 0));
                    }
                }
                self.blocks().push(Block::Para { inlines, level: 0 });
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.inlines);
                self.row.push(cell);
                // markdown-it's cell content: trimmed, with `\|` unescaped
                let raw = self
                    .src
                    .get(self.cell.clone())
                    .unwrap_or("")
                    .replace(r"\|", "|");
                let raw = self.restore(sheet::trim(&raw));
                self.raw_row.push(raw);
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                let row = std::mem::take(&mut self.row);
                let raw = std::mem::take(&mut self.raw_row);
                if let Some((_, rows)) = &mut self.table {
                    rows.push(row);
                    self.raws.push(raw);
                }
            }
            TagEnd::Table => {
                if let Some((aligns, mut rows)) = self.table.take() {
                    let raws = std::mem::take(&mut self.raws);
                    let results = sheet::evaluate(&raws);
                    for ((row, raw), result) in rows.iter_mut().zip(&raws).zip(results) {
                        for ((cell, raw), result) in row.iter_mut().zip(raw).zip(result) {
                            if let Some(text) = result {
                                let s = sheet::unstyle(raw).1;
                                let flags = if s.bold { BOLD } else { 0 }
                                    | if s.italic { ITALIC } else { 0 }
                                    | if s.strike { STRIKE } else { 0 };
                                *cell = vec![Inline::Text(text, flags)];
                            } else if let Some(done) = sheet::checkbox(raw) {
                                *cell = vec![Inline::Check(done)];
                            }
                        }
                    }
                    self.blocks().push(Block::Table { aligns, rows });
                }
            }
            TagEnd::Emphasis => self.style[1] -= 1,
            TagEnd::Strong => self.style[0] -= 1,
            TagEnd::Strikethrough => self.style[3] -= 1,
            TagEnd::Link => self.style[4] -= 1,
            _ => {}
        }
    }
}

fn parse(src: &str, spans: &[MathSpan]) -> Vec<Block> {
    let mut b = Builder {
        spans,
        src,
        stack: vec![Frame::Root(Vec::new())],
        pushed: Vec::new(),
        inlines: Vec::new(),
        style: [0; 5],
        level: 0,
        code: None,
        html: None,
        table: None,
        row: Vec::new(),
        raws: Vec::new(),
        raw_row: Vec::new(),
        cell: 0..0,
    };
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        b.event(ev, range);
    }
    b.flush();
    match b.stack.swap_remove(0) {
        Frame::Root(blocks) => blocks,
        _ => unreachable!("the root frame stays at the bottom"),
    }
}

fn face_of(font: TextFont, flags: u8, c: char) -> Face {
    Face::styled(
        font,
        flags & BOLD != 0,
        flags & ITALIC != 0,
        flags & MONO != 0,
        c,
    )
}

#[derive(Debug, Clone)]
enum Atom {
    Char {
        c: char,
        flags: u8,
        face: Face,
        size: f64,
        width: f64,
    },
    Math {
        index: usize,
        size: f64,
        width: f64,
        height: f64,
        depth: f64,
    },
}

impl Atom {
    fn width(&self) -> f64 {
        match self {
            Atom::Char { width, .. } | Atom::Math { width, .. } => *width,
        }
    }
    fn is_space(&self) -> bool {
        matches!(self, Atom::Char { c: ' ', .. })
    }
    fn is_blank(&self) -> bool {
        matches!(self, Atom::Char { c: ' ' | '\t', .. })
    }
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6 + 0.0
}

struct Layout<'a, F> {
    fs: f64,
    font: TextFont,
    row: Option<f64>,
    align: TextAlign,
    spans: &'a [MathSpan],
    math: &'a F,
    y: f64,
    list_depth: usize,
    /// A list marker waiting for its item's first line.
    marker: Option<Marker>,
    paint: Paint,
}

struct Marker {
    x: f64,
    face: Face,
    text: Option<String>,
    task: Option<bool>,
}

impl<F: Fn(&str, bool) -> Option<MathBox>> Layout<'_, F> {
    fn gap(&self) -> f64 {
        self.row.unwrap_or_else(|| metrics().line_height(self.fs))
    }

    fn drop(&self) -> f64 {
        if self.row.is_some() {
            ROW_FRAME_DROP * self.fs
        } else {
            0.0
        }
    }

    fn segment(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, width: f64) {
        self.paint.segments.push(Segment {
            x0: round6(x0),
            y0: round6(y0),
            x1: round6(x1),
            y1: round6(y1),
            width: round6(width),
        });
    }

    fn run(&mut self, x: f64, y: f64, face: Face, size: f64, text: String) {
        self.paint.runs.push(Run {
            x: round6(x),
            y: round6(y),
            face,
            size: round6(size),
            text,
        });
    }

    fn char_atom(&self, c: char, flags: u8, size: f64) -> Atom {
        let m = metrics();
        let face = face_of(self.font, flags, c);
        // the code face is set a little smaller: its letters are wide and tall
        let size = if flags & MONO != 0 {
            size * MONO_SCALE
        } else {
            size
        };
        let width = m.advance_in(face, c) * size / m.units_per_em;
        Atom::Char {
            c,
            flags,
            face,
            size,
            width,
        }
    }

    /// The source of a formula in the code face, for when it can't be drawn.
    fn source_atoms(&self, index: usize, size: f64, out: &mut Vec<Atom>) {
        out.extend(
            self.spans[index]
                .source
                .chars()
                .map(|c| self.char_atom(if c == '\n' { ' ' } else { c }, MONO, size)),
        );
    }

    fn math_atom(&self, index: usize, size: f64, avail: f64, out: &mut Vec<Atom>) {
        let s = &self.spans[index];
        let Some(b) = (self.math)(&s.tex, s.display) else {
            return self.source_atoms(index, size, out);
        };
        let wide = b.width * size;
        let fit = if wide > avail && wide > 0.0 {
            avail.max(1.0) / wide
        } else {
            1.0
        };
        let size = size * fit;
        out.push(Atom::Math {
            index,
            size,
            width: b.width * size,
            height: b.height * size,
            depth: b.depth * size,
        });
    }

    /// Places one finished line at the cursor.
    fn place(&mut self, atoms: &[Atom], x: f64, avail: f64, align: TextAlign, base: f64) {
        let m = metrics();
        let mut trimmed = atoms.len();
        while trimmed > 0 && atoms[trimmed - 1].is_blank() {
            trimmed -= 1;
        }
        let atoms = &atoms[..trimmed];
        let width: f64 = atoms.iter().map(Atom::width).sum();
        let mut size = 0.0_f64;
        let mut tall = 0.0_f64;
        let mut deep = 0.0_f64;
        for a in atoms {
            match a {
                Atom::Char { size: s, .. } => size = size.max(*s),
                Atom::Math { height, depth, .. } => {
                    tall = tall.max(*height);
                    deep = deep.max(*depth);
                }
            }
        }
        if size == 0.0 {
            size = base;
        }
        let (baseline, advance) = match self.row {
            Some(r) => {
                let need = size.max(tall + deep);
                let rows = (need / r - 1e-9).ceil().max(1.0);
                (self.y + rows * r, rows * r)
            }
            None => {
                let first = m.first_baseline(size);
                let up = first.max(tall);
                let down = (m.line_height(size) - first).max(deep);
                (self.y + up, up + down)
            }
        };
        let dx = match align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (avail - width) / 2.0,
            TextAlign::Right => avail - width,
        };
        let mut cx = x + dx;
        let mut i = 0;
        while i < atoms.len() {
            match &atoms[i] {
                Atom::Math {
                    index, size, width, ..
                } => {
                    let s = &self.spans[*index];
                    self.paint.math.push(MathMark {
                        x: round6(cx),
                        y: round6(baseline),
                        size: round6(*size),
                        tex: s.tex.clone(),
                        display: s.display,
                    });
                    cx += width;
                    i += 1;
                }
                Atom::Char {
                    flags, face, size, ..
                } => {
                    let (flags, face, size) = (*flags, *face, *size);
                    let start = cx;
                    let mut text = String::new();
                    while let Some(Atom::Char {
                        c,
                        flags: fl,
                        face: fc,
                        size: sz,
                        width,
                    }) = atoms.get(i)
                    {
                        if *fl != flags || *fc != face || *sz != size {
                            break;
                        }
                        if *c == '\t' {
                            text.push_str("    ");
                        } else {
                            text.push(*c);
                        }
                        cx += width;
                        i += 1;
                    }
                    self.run(start, baseline, face, size, text);
                    let weight = STROKE_EM * size;
                    if flags & UNDERLINE != 0 {
                        let y = baseline + 0.12 * size;
                        self.segment(start, y, cx, y, weight);
                    }
                    if flags & STRIKE != 0 {
                        let y = baseline - 0.28 * size;
                        self.segment(start, y, cx, y, weight);
                    }
                }
            }
        }
        if let Some(mk) = self.marker.take() {
            if let Some(text) = mk.text {
                self.run(mk.x, baseline, mk.face, self.fs, text);
            }
            if let Some(done) = mk.task {
                let side = 0.68 * self.fs;
                let (x0, y0, x1, y1) = (mk.x, baseline - side, mk.x + side, baseline);
                let w = STROKE_EM * self.fs;
                if self.font == TextFont::Handwritten {
                    for line in HAND_BOX {
                        self.polyline(x0, y0, side, line, w * HAND_WEIGHT);
                    }
                    if done {
                        self.polyline(x0, y0, side, HAND_TICK, w * 1.4 * HAND_WEIGHT);
                    }
                } else {
                    self.segment(x0, y0, x1, y0, w);
                    self.segment(x1, y0, x1, y1, w);
                    self.segment(x1, y1, x0, y1, w);
                    self.segment(x0, y1, x0, y0, w);
                    if done {
                        let (mx, my) = (x0 + side * 0.4, y1 - side * 0.2);
                        self.segment(x0 + side * 0.18, y1 - side * 0.5, mx, my, w * 1.4);
                        self.segment(mx, my, x1 - side * 0.14, y0 + side * 0.2, w * 1.4);
                    }
                }
            }
        }
        let bottom = baseline + (-m.descent_of(self.font) * size / m.units_per_em).max(deep);
        self.paint.bottom = self.paint.bottom.max(round6(bottom));
        self.y += advance;
    }

    /// Strokes `points`, given in a unit square, scaled to `side` at `(x, y)`.
    fn polyline(&mut self, x: f64, y: f64, side: f64, points: &[(f64, f64)], width: f64) {
        for p in points.windows(2) {
            let (a, b) = (p[0], p[1]);
            self.segment(
                x + side * a.0,
                y + side * a.1,
                x + side * b.0,
                y + side * b.1,
                width,
            );
        }
    }

    /// Greedy wrapping: the rules of [`crate::text::layout`], over styled atoms.
    fn wrap(&mut self, atoms: &[Atom], x: f64, avail: f64, align: TextAlign, base: f64) {
        let end = atoms
            .iter()
            .rposition(|a| !a.is_blank())
            .map_or(0, |i| i + 1);
        let atoms = &atoms[..end];
        if atoms.is_empty() {
            return self.place(atoms, x, avail, align, base);
        }
        let mut line_start = 0;
        let mut i = 0;
        let mut width = 0.0;
        let mut last_break: Option<usize> = None;
        while i < atoms.len() {
            let w = atoms[i].width();
            if atoms[i].is_space() {
                width += w;
                i += 1;
                last_break = Some(i);
                continue;
            }
            if width + w > avail && i > line_start {
                let at = match last_break {
                    Some(b) if b > line_start => b,
                    _ => i,
                };
                self.place(&atoms[line_start..at], x, avail, align, base);
                line_start = at;
                last_break = None;
                width = atoms[line_start..i].iter().map(Atom::width).sum();
                continue;
            }
            width += w;
            i += 1;
        }
        self.place(&atoms[line_start..], x, avail, align, base);
    }

    /// Inline content: hard breaks and display formulas split it into lines.
    fn flow(
        &mut self,
        inlines: &[Inline],
        x: f64,
        avail: f64,
        align: TextAlign,
        size: f64,
        extra: u8,
    ) {
        let mut line: Vec<Atom> = Vec::new();
        let mut placed = false;
        let mut after_display = false;
        for (i, item) in inlines.iter().enumerate() {
            match item {
                Inline::Text(t, flags) => {
                    line.extend(t.chars().map(|c| self.char_atom(c, flags | extra, size)));
                    after_display = false;
                }
                Inline::Break => {
                    // the break around a display formula is part of its line
                    let next_display = matches!(inlines.get(i + 1), Some(Inline::Math(n)) if self.spans[*n].display);
                    if !(after_display || line.is_empty() && next_display) {
                        self.wrap(&line, x, avail, align, size);
                        placed = true;
                    }
                    line.clear();
                    after_display = false;
                }
                Inline::Math(n) if self.spans[*n].display => {
                    if !line.is_empty() {
                        self.wrap(&line, x, avail, align, size);
                        line.clear();
                    }
                    let mut atoms = Vec::new();
                    self.math_atom(*n, size, avail, &mut atoms);
                    self.wrap(&atoms, x, avail, TextAlign::Center, size);
                    placed = true;
                    after_display = true;
                }
                Inline::Math(n) => {
                    self.math_atom(*n, size, avail, &mut line);
                    after_display = false;
                }
                // only ever a whole table cell, which `table` draws itself
                Inline::Check(_) => {}
            }
        }
        if !line.is_empty() || !placed {
            self.wrap(&line, x, avail, align, size);
        }
    }

    fn blocks(&mut self, blocks: &[Block], x: f64, avail: f64, gaps: bool) {
        for (i, b) in blocks.iter().enumerate() {
            if i > 0 && gaps {
                self.y += self.gap();
            }
            self.block(b, x, avail);
        }
    }

    fn block(&mut self, block: &Block, x: f64, avail: f64) {
        match block {
            Block::Para { inlines, level } => {
                let (size, bold) = match level {
                    0 => (self.fs, 0),
                    l => (self.fs * HEADING_SCALE[l - 1], BOLD),
                };
                self.flow(inlines, x, avail, self.align, size, bold);
            }
            Block::Code(text) => {
                let pad = 0.4 * self.fs;
                let top = self.y + self.drop();
                for line in text.split('\n') {
                    let atoms: Vec<Atom> = line
                        .chars()
                        .map(|c| self.char_atom(c, MONO, self.fs))
                        .collect();
                    let base = self.fs * MONO_SCALE;
                    self.wrap(&atoms, x + pad, avail - 2.0 * pad, TextAlign::Left, base);
                }
                let bottom = self.y + self.drop();
                let right = x + avail;
                let w = STROKE_EM * self.fs;
                self.segment(x, top, right, top, w);
                self.segment(right, top, right, bottom, w);
                self.segment(right, bottom, x, bottom, w);
                self.segment(x, bottom, x, top, w);
                self.paint.bottom = self.paint.bottom.max(round6(bottom));
            }
            Block::Rule => {
                let h = self.gap();
                let y = self.y + h / 2.0;
                self.segment(x, y, x + avail, y, STROKE_EM * self.fs);
                self.y += h;
            }
            Block::Quote(inner) => {
                let top = self.y + self.drop();
                let indent = self.fs;
                self.blocks(inner, x + indent, avail - indent, true);
                if inner.is_empty() {
                    self.y += self.gap();
                }
                let bx = x + 0.35 * self.fs;
                let bottom = self.y + self.drop();
                self.segment(bx, top, bx, bottom, 0.12 * self.fs);
            }
            Block::List {
                start,
                tight,
                items,
            } => {
                let gutter = 1.5 * self.fs;
                self.list_depth += 1;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 && !tight {
                        self.y += self.gap();
                    }
                    let label = match (item.task, start) {
                        (Some(_), _) => None,
                        (None, Some(n)) => Some(format!("{}.", n.saturating_add(i as u64))),
                        (None, None) => Some(
                            if self.list_depth % 2 == 1 {
                                "\u{2022}"
                            } else {
                                "\u{2013}"
                            }
                            .to_owned(),
                        ),
                    };
                    // the whole label in one face: "10." never mixes fonts
                    let face = match &label {
                        Some(l) if l.chars().all(|c| face_of(self.font, 0, c).is_hand()) => {
                            Face::Hand
                        }
                        _ => Face::Regular,
                    };
                    let w = label.as_deref().map_or(0.68 * self.fs, |l| {
                        let m = metrics();
                        l.chars().map(|c| m.advance_in(face, c)).sum::<f64>() * self.fs
                            / m.units_per_em
                    });
                    self.marker = Some(Marker {
                        x: x.max(x + gutter - 0.4 * self.fs - w),
                        face,
                        text: label,
                        task: item.task,
                    });
                    self.blocks(&item.blocks, x + gutter, avail - gutter, !tight);
                    if self.marker.is_some() {
                        self.place(&[], x + gutter, avail - gutter, TextAlign::Left, self.fs);
                    }
                }
                self.list_depth -= 1;
            }
            Block::Table { aligns, rows } => self.table(aligns, rows, x, avail),
        }
    }

    fn table(&mut self, aligns: &[TextAlign], rows: &[Row], x: f64, avail: f64) {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let pad = 0.4 * self.fs;
        let mut widths = vec![2.0 * pad; columns];
        for (r, row) in rows.iter().enumerate() {
            let extra = if r == 0 { BOLD } else { 0 };
            for (c, cell) in row.iter().enumerate() {
                widths[c] = widths[c].max(self.natural_width(cell, extra) + 2.0 * pad);
            }
        }
        let total: f64 = widths.iter().sum();
        if total > avail {
            for w in &mut widths {
                *w *= avail / total;
            }
        }
        let table_w: f64 = widths.iter().sum();
        let weight = STROKE_EM * self.fs;
        let drop = self.drop();
        let top = self.y + drop;
        self.segment(x, top, x + table_w, top, weight);
        for (r, row) in rows.iter().enumerate() {
            let extra = if r == 0 { BOLD } else { 0 };
            let start = self.y;
            let mut end = start;
            let mut cx = x;
            for (c, width) in widths.iter().enumerate() {
                self.y = start;
                let cell = row.get(c).map_or(&[][..], Vec::as_slice);
                let align = aligns.get(c).copied().unwrap_or(TextAlign::Left);
                let inner = width - 2.0 * pad;
                if let [Inline::Check(done)] = cell {
                    // the checkbox is a list marker on the cell's one empty line
                    let side = 0.68 * self.fs;
                    let dx = match align {
                        TextAlign::Left => 0.0,
                        TextAlign::Center => (inner - side) / 2.0,
                        TextAlign::Right => inner - side,
                    };
                    self.marker = Some(Marker {
                        x: cx + pad + dx.max(0.0),
                        face: Face::Regular,
                        text: None,
                        task: Some(*done),
                    });
                }
                self.flow(cell, cx + pad, inner, align, self.fs, extra);
                end = end.max(self.y);
                cx += width;
            }
            self.y = end;
            self.segment(x, end + drop, x + table_w, end + drop, weight);
        }
        let bottom = self.y + drop;
        let mut cx = x;
        self.segment(cx, top, cx, bottom, weight);
        for w in &widths {
            cx += w;
            self.segment(cx, top, cx, bottom, weight);
        }
        self.paint.bottom = self.paint.bottom.max(round6(bottom));
    }

    /// Width of a table cell on one line, breaking only where it must.
    fn natural_width(&self, cell: &[Inline], extra: u8) -> f64 {
        let mut best = 0.0_f64;
        let mut line = 0.0;
        for item in cell {
            let mut atoms = Vec::new();
            match item {
                Inline::Text(t, flags) => {
                    atoms.extend(t.chars().map(|c| self.char_atom(c, flags | extra, self.fs)));
                }
                Inline::Math(n) => self.math_atom(*n, self.fs, f64::INFINITY, &mut atoms),
                Inline::Break => {
                    best = best.max(line);
                    line = 0.0;
                }
                Inline::Check(_) => line += 0.68 * self.fs,
            }
            line += atoms.iter().map(Atom::width).sum::<f64>();
        }
        best.max(line)
    }
}

/// Lays Markdown out in `font`. `row_pitch` locks lines to rows (see the module
/// docs); `math` gives a formula's size in em, or `None` to show its source.
pub fn layout<F>(
    text: &str,
    font: TextFont,
    font_size: f64,
    box_width: f64,
    row_pitch: Option<f64>,
    align: TextAlign,
    math: &F,
) -> Paint
where
    F: Fn(&str, bool) -> Option<MathBox>,
{
    let (src, spans) = extract_math(text);
    let blocks = parse(&src, &spans);
    let mut l = Layout {
        fs: font_size,
        font,
        row: row_pitch,
        align,
        spans: &spans,
        math,
        y: 0.0,
        list_depth: 0,
        marker: None,
        paint: Paint::default(),
    };
    l.blocks(&blocks, 0.0, box_width, true);
    if blocks.is_empty() {
        l.place(&[], 0.0, box_width, align, font_size);
    }
    l.paint.height = round6(l.y);
    l.paint.bottom = l.paint.bottom.max(l.paint.height);
    l.paint
}
