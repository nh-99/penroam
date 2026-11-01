//! Deterministic plain-text layout shared by the on-screen renderer (TS port in
//! `apps/desktop-mobile/src/editor/textLayout.ts`) and the PDF exporter.
//!
//! Widths come from `data/text-metrics.json` (Liberation Sans, metric-compatible
//! with Helvetica), never from the platform's font rasteriser, so wrapping is
//! identical on every OS and in exported PDFs. Golden: `tests/fixtures/text-layout-golden.json`.
//!
//! Rules: `\n` forces a break; otherwise greedy wrapping at spaces; a word
//! wider than the box is broken between characters; trailing spaces of a
//! wrapped line do not count towards its width.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::OnceLock;

pub const METRICS_JSON: &str = include_str!("../data/text-metrics.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMetrics {
    pub units_per_em: f64,
    pub ascent: f64,
    pub descent: f64,
    pub line_height: f64,
    pub first_baseline: f64,
    pub fallback_width: f64,
    widths: HashMap<String, f64>,
    #[serde(skip)]
    by_char: HashMap<char, f64>,
}

pub fn metrics() -> &'static TextMetrics {
    static M: OnceLock<TextMetrics> = OnceLock::new();
    M.get_or_init(|| {
        let mut m: TextMetrics = serde_json::from_str(METRICS_JSON).expect("text-metrics.json");
        m.by_char = m
            .widths
            .iter()
            .filter_map(|(k, v)| {
                k.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .map(|c| (c, *v))
            })
            .collect();
        m
    })
}

impl TextMetrics {
    /// Advance width of one character in em/1000 units.
    pub fn advance(&self, c: char) -> f64 {
        if c == '\t' {
            return self.advance(' ') * 4.0;
        }
        *self.by_char.get(&c).unwrap_or(&self.fallback_width)
    }
    /// Whether the character has a metric (and a WinAnsi glyph in Helvetica).
    pub fn known(&self, c: char) -> bool {
        self.by_char.contains_key(&c)
    }
    pub fn line_height(&self, font_size: f64) -> f64 {
        font_size * self.line_height / self.units_per_em
    }
    pub fn first_baseline(&self, font_size: f64) -> f64 {
        font_size * self.first_baseline / self.units_per_em
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Line {
    /// Character (not byte) range into the source text.
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// Width in points, excluding trailing spaces.
    pub width: f64,
}

pub fn measure(s: &str, font_size: f64) -> f64 {
    let m = metrics();
    s.chars().map(|c| m.advance(c)).sum::<f64>() * font_size / m.units_per_em
}

/// Lays `text` out in a box `box_width` points wide.
pub fn layout(text: &str, font_size: f64, box_width: f64) -> Vec<Line> {
    let m = metrics();
    let scale = font_size / m.units_per_em;
    let chars: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    let mut para_start = 0;
    loop {
        let para_end = chars[para_start..]
            .iter()
            .position(|&c| c == '\n')
            .map(|i| para_start + i)
            .unwrap_or(chars.len());
        wrap_paragraph(
            &chars, para_start, para_end, box_width, scale, m, &mut lines,
        );
        if para_end >= chars.len() {
            break;
        }
        para_start = para_end + 1;
    }
    lines
}

fn wrap_paragraph(
    chars: &[char],
    start: usize,
    end: usize,
    box_w: f64,
    scale: f64,
    m: &TextMetrics,
    out: &mut Vec<Line>,
) {
    let push = |out: &mut Vec<Line>, s: usize, e: usize| {
        // trim trailing spaces from the measured width (not from the text range)
        let mut te = e;
        while te > s && chars[te - 1] == ' ' {
            te -= 1;
        }
        let width = chars[s..te].iter().fold(0.0, |a, &c| a + m.advance(c)) * scale;
        // rounded so the TS port and the golden file agree bit-for-bit
        let width = (width * 1e6).round() / 1e6 + 0.0;
        out.push(Line {
            start: s,
            end: e,
            text: chars[s..e].iter().collect(),
            width,
        });
    };
    if start == end {
        push(out, start, end);
        return;
    }
    let mut line_start = start;
    let mut i = start;
    let mut width = 0.0; // width of chars[line_start..i]
    let mut last_break: Option<usize> = None; // index after a space
    while i < end {
        let c = chars[i];
        let w = m.advance(c) * scale;
        if c == ' ' {
            width += w;
            i += 1;
            last_break = Some(i);
            continue;
        }
        if width + w > box_w && i > line_start {
            if let Some(b) = last_break.filter(|&b| b > line_start) {
                push(out, line_start, b);
                line_start = b;
            } else {
                push(out, line_start, i);
                line_start = i;
            }
            last_break = None;
            width = chars[line_start..i]
                .iter()
                .map(|&c| m.advance(c))
                .sum::<f64>()
                * scale;
            continue;
        }
        width += w;
        i += 1;
    }
    push(out, line_start, end);
}

/// Laid-out content height for a text block.
pub fn content_height(text: &str, font_size: f64, box_width: f64) -> f64 {
    let n = layout(text, font_size, box_width).len().max(1);
    n as f64 * metrics().line_height(font_size)
}
