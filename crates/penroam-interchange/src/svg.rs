//! One page as SVG.
//!
//! Ink comes from [`penroam_core::geometry::stroke_pieces`] and text from
//! [`penroam_core::markdown::layout`], so the result matches the canvas and PDF
//! export. Paint order is the same too: template, ink, images, text. All text
//! and attribute values are escaped; images are embedded as `data:` URIs and
//! only with PNG or JPEG types, so the file references nothing outside itself.
//!
//! A PDF background can't be drawn here without a PDF renderer, so it is left
//! out with a warning; annotated PDF export or PNG export keep it.

use penroam_core::geometry::{stroke_pieces, Piece};
use penroam_core::markdown::{self, MathBox, Paint};
use penroam_core::text::Face;
use penroam_core::{Element, InkStroke, InkTool, Page, TemplateKind, TextBlock};
use serde::Serialize;
use std::fmt::Write;
use ts_rs::TS;

/// A rendered page.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SvgPage {
    /// The SVG document.
    pub svg: String,
    /// What this file doesn't show.
    pub warnings: Vec<String>,
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => o.push(c),
        }
    }
    o
}

fn n(v: f64) -> String {
    let s = format!("{:.3}", v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(b.len().div_ceil(3) * 4);
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

fn template(s: &mut String, page: &Page) {
    let (w, h, gap) = (
        page.size.width,
        page.size.height,
        page.template.spacing.max(4.0),
    );
    let line = "stroke=\"#d1d5db\" stroke-width=\"0.5\"";
    let mut y = gap;
    match page.template.kind {
        TemplateKind::Blank => {}
        TemplateKind::Ruled | TemplateKind::Grid => {
            while y < h {
                let _ = writeln!(s, "<path d=\"M0 {y}H{w}\" {line}/>", y = n(y), w = n(w));
                y += gap;
            }
            if page.template.kind == TemplateKind::Grid {
                let mut x = gap;
                while x < w {
                    let _ = writeln!(s, "<path d=\"M{x} 0V{h}\" {line}/>", x = n(x), h = n(h));
                    x += gap;
                }
            }
        }
        TemplateKind::Dotted => {
            while y < h {
                let mut x = gap;
                while x < w {
                    let _ = writeln!(
                        s,
                        "<circle cx=\"{}\" cy=\"{}\" r=\"0.7\" fill=\"#c4c8d0\"/>",
                        n(x),
                        n(y)
                    );
                    x += gap;
                }
                y += gap;
            }
        }
    }
}

fn piece_d(d: &mut String, p: &Piece, start: bool) {
    match *p {
        Piece::Dot { x, y, .. } => {
            let _ = write!(d, "M{} {}h0", n(x), n(y));
        }
        Piece::Line { x0, y0, x1, y1, .. } => {
            if start {
                let _ = write!(d, "M{} {}", n(x0), n(y0));
            }
            let _ = write!(d, "L{} {}", n(x1), n(y1));
        }
        Piece::Quad {
            x0,
            y0,
            cx,
            cy,
            x1,
            y1,
            ..
        } => {
            if start {
                let _ = write!(d, "M{} {}", n(x0), n(y0));
            }
            let _ = write!(d, "Q{} {} {} {}", n(cx), n(cy), n(x1), n(y1));
        }
    }
}

fn ink(s: &mut String, st: &InkStroke) {
    let pieces = stroke_pieces(&st.points, st.width, st.pressure);
    let opacity = if st.opacity < 0.999 {
        format!(" opacity=\"{}\"", n(st.opacity))
    } else {
        String::new()
    };
    let attrs = format!(
        "fill=\"none\" stroke=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"{opacity}",
        esc(&st.color)
    );
    if st.pressure && st.tool == InkTool::Pen && pieces.len() > 1 {
        let _ = writeln!(s, "<g {attrs}>");
        for p in &pieces {
            let mut d = String::new();
            piece_d(&mut d, p, true);
            let _ = writeln!(s, "<path d=\"{d}\" stroke-width=\"{}\"/>", n(p.width()));
        }
        s.push_str("</g>\n");
    } else {
        let mut d = String::new();
        for (i, p) in pieces.iter().enumerate() {
            piece_d(&mut d, p, i == 0);
        }
        let w = pieces.first().map_or(st.width, |p| p.width());
        let _ = writeln!(s, "<path d=\"{d}\" stroke-width=\"{}\" {attrs}/>", n(w));
    }
}

fn text_block(s: &mut String, t: &TextBlock) -> bool {
    let no_math = |_: &str, _: bool| -> Option<MathBox> { None };
    let p: Paint = markdown::layout(
        &t.text,
        t.font_size,
        t.width,
        t.row_pitch,
        t.align,
        &no_math,
    );
    let _ = writeln!(
        s,
        "<g transform=\"translate({} {})\" fill=\"{}\">",
        n(t.x),
        n(t.y),
        esc(&t.color)
    );
    for r in &p.runs {
        let (family, weight, style) = match r.face {
            Face::Regular => ("sans", "", ""),
            Face::Bold => ("sans", " font-weight=\"bold\"", ""),
            Face::Italic => ("sans", "", " font-style=\"italic\""),
            Face::BoldItalic => ("sans", " font-weight=\"bold\"", " font-style=\"italic\""),
            Face::Mono => ("mono", "", ""),
        };
        let family = if family == "mono" {
            "'Liberation Mono',Courier,monospace"
        } else {
            "'Liberation Sans',Helvetica,Arial,sans-serif"
        };
        let _ = writeln!(
            s,
            "<text x=\"{}\" y=\"{}\" font-size=\"{}\" font-family=\"{family}\"{weight}{style} xml:space=\"preserve\">{}</text>",
            n(r.x), n(r.y), n(r.size), esc(&r.text)
        );
    }
    for l in &p.segments {
        let _ = writeln!(
            s,
            "<path d=\"M{} {}L{} {}\" stroke=\"{}\" stroke-width=\"{}\"/>",
            n(l.x0),
            n(l.y0),
            n(l.x1),
            n(l.y1),
            esc(&t.color),
            n(l.width)
        );
    }
    s.push_str("</g>\n");
    !p.math.is_empty()
}

/// Renders a page. `asset` returns the bytes and MIME type of an attachment.
pub fn page_to_svg(page: &Page, asset: impl Fn(&str) -> Option<(Vec<u8>, String)>) -> SvgPage {
    let (w, h) = (page.size.width, page.size.height);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}pt\" height=\"{h}pt\" viewBox=\"0 0 {w} {h}\">",
        w = n(w),
        h = n(h)
    );
    let _ = writeln!(
        s,
        "<rect width=\"{}\" height=\"{}\" fill=\"#ffffff\"/>",
        n(w),
        n(h)
    );
    template(&mut s, page);
    let mut warnings = Vec::new();
    if page.background.is_some() {
        warnings.push(
            "The PDF behind this page is not in the SVG. Use PNG or annotated PDF to keep it."
                .to_string(),
        );
    }
    let rank = |e: &Element| match e {
        Element::Ink(_) => 0,
        Element::Image(_) => 1,
        Element::Text(_) => 2,
    };
    let mut ordered: Vec<&Element> = page.elements.iter().collect();
    ordered.sort_by_key(|e| rank(e));
    let mut math = false;
    for e in ordered {
        match e {
            Element::Ink(st) => ink(&mut s, st),
            Element::Text(t) => math |= text_block(&mut s, t),
            Element::Image(i) => match asset(&i.asset_hash) {
                Some((bytes, media)) if matches!(media.as_str(), "image/png" | "image/jpeg") => {
                    let _ = writeln!(
                        s,
                        "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" href=\"data:{media};base64,{}\"/>",
                        n(i.x), n(i.y), n(i.width), n(i.height), base64(&bytes)
                    );
                }
                _ => warnings.push("An image could not be included.".to_string()),
            },
        }
    }
    if math {
        warnings.push("Formulas appear as their LaTeX source.".to_string());
    }
    warnings.dedup();
    s.push_str("</svg>\n");
    SvgPage { svg: s, warnings }
}
