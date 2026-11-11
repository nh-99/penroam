//! Xournal++ (`.xopp`) and classic Xournal (`.xoj`) documents: gzipped XML of
//! pages, layers, strokes, text and images.
//!
//! The format has no formal specification. This reader follows the files the applications
//! write, up to `fileversion` 5, and treats anything newer as best effort, with a note in the
//! report. It has been run against Xournal++'s own test documents.
//!
//! Besides gzipped XML there is a packaged form: a ZIP holding `content.xml` and an
//! `attachments/` folder, where images and PDF backgrounds can live inside the file.
//! From `fileversion` 5, text, images and formulas are placed by a `matrix` attribute
//! (`xx yx xy yy x0 y0`), and an image's bounds are that matrix applied to its size in pixels.
//! Penroam elements can't rotate, so a rotated one becomes its bounding box.
//!
//! ```text
//! <xournal fileversion="4">
//!   <page width="612" height="792">
//!     <background type="solid" color="#ffffffff" style="lined"/>
//!     <layer>
//!       <stroke tool="pen" color="#000000ff" width="1.4 1.1 1.2">x y x y x y</stroke>
//!       <text font="Sans" size="12" x="10" y="20" color="#000000ff">hello</text>
//!       <image left="0" top="0" right="50" bottom="50">base64 PNG</image>
//!     </layer>
//!   </page>
//! </xournal>
//! ```
//!
//! **Security.** Backgrounds name a PDF by file path. That path comes from the
//! file, so it is never opened; the page is kept without its background and
//! the loss is reported. In a packaged file a path only selects an entry of that same
//! ZIP, read with a size limit, so nothing outside the file can be reached. There is no DTD or custom entity support: an unknown
//! entity makes the document malformed, which rules out entity-expansion bombs.
//! A stroke, text or image that fails a check is dropped and counted, because
//! one bad element shouldn't cost the user a whole document, but XML that
//! doesn't parse, or stops early, is refused outright.

use crate::document::{
    ImportError, ImportLoss, ImportedAsset, ImportedDocument, ImportedPage, Kept, LossLog,
};
use crate::images::{check_image, pixel_size};
use crate::limits;
use crate::text_import::escape_markdown;
use flate2::read::GzDecoder;
use penroam_core::markdown::{self, MathBox};
use penroam_core::validate::{limits as model, validate_element};
use penroam_core::{
    text, Element, ImageElement, InkPoint, InkStroke, InkTool, Size, Template, TemplateKind,
    TextAlign, TextBlock, TextFont,
};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use uuid::Uuid;

/// Newest `fileversion` this reader was written against.
const KNOWN_FILE_VERSION: u32 = 5;
/// Largest PDF kept from a packaged document.
const MAX_ATTACHED_PDF: u64 = 1 << 30;
/// Default pressure for strokes without per-point widths. Unused by
/// rendering, since those strokes are stored with pressure off.
const FLAT_PRESSURE: f64 = 0.5;
/// Largest width as a multiple of a stroke's base width at full pressure
/// (`penroam_core::geometry::width_for`).
const MAX_WIDTH_FACTOR: f64 = 1.6;
/// Handwriting samples are a few points apart; only shapes and rulers leave gaps like this.
const LONG_SEGMENT: f64 = 12.0;
/// How far inside a corner the holding points go.
const CORNER_INSET: f64 = 0.5;
/// Xournal++'s graph and dot spacing, 5 mm.
const SQUARE_SIZE: f64 = 14.17;

const ROTATED: &str = "{n} rotated or skewed item(s) were placed straight.";
const FORMULAS: &str =
    "LaTeX formulas are typeset by Penroam, so they may look a little different.";

fn malformed(e: impl std::fmt::Display) -> ImportError {
    ImportError::Malformed(format!("this Xournal document can't be read: {e}"))
}

fn inflate(bytes: &[u8]) -> Result<Vec<u8>, ImportError> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        // some tools write the XML uncompressed
        if bytes.len() as u64 > limits::MAX_XML_BYTES {
            return Err(ImportError::TooLarge("over 256 MiB of XML".into()));
        }
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::new();
    GzDecoder::new(bytes)
        .take(limits::MAX_XML_BYTES + 1)
        .read_to_end(&mut out)
        .map_err(malformed)?;
    if out.len() as u64 > limits::MAX_XML_BYTES {
        return Err(ImportError::TooLarge("expands past 256 MiB".into()));
    }
    Ok(out)
}

/// The XML of a document, and the ZIP it came from if it is the packaged form.
type Source<'a> = (Vec<u8>, Option<zip::ZipArchive<Cursor<&'a [u8]>>>);

fn open_document(bytes: &[u8]) -> Result<Source<'_>, ImportError> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok((inflate(bytes)?, None));
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(malformed)?;
    let xml = entry(&mut zip, "content.xml", limits::MAX_XML_BYTES)
        .ok_or_else(|| malformed("it has no content.xml"))?;
    Ok((xml, Some(zip)))
}

/// Reads a ZIP entry by exact name, stopping at `limit` bytes actually produced.
fn entry(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str, limit: u64) -> Option<Vec<u8>> {
    let f = zip.by_name(name).ok()?;
    let mut out = Vec::new();
    f.take(limit + 1).read_to_end(&mut out).ok()?;
    (out.len() as u64 <= limit).then_some(out)
}

/// A `matrix` attribute: `xx yx xy yy x0 y0`.
fn matrix(a: &BTreeMap<String, String>) -> Option<[f64; 6]> {
    let v: Vec<f64> = a
        .get("matrix")?
        .split_ascii_whitespace()
        .map(|n| n.parse().ok().filter(|v: &f64| v.is_finite()))
        .collect::<Option<_>>()?;
    v.try_into().ok()
}

fn is_straight(m: &[f64; 6]) -> bool {
    m[1].abs() < 1e-6 && m[2].abs() < 1e-6 && m[0] > 0.0 && m[3] > 0.0
}

/// The bounding box (x, y, width, height) of a `width` by `height` box under a matrix.
fn bounds(m: &[f64; 6], width: f64, height: f64) -> (f64, f64, f64, f64) {
    let corners = [(0.0, 0.0), (width, 0.0), (0.0, height), (width, height)];
    let xs = corners.map(|(x, y)| m[0] * x + m[2] * y + m[4]);
    let ys = corners.map(|(x, y)| m[1] * x + m[3] * y + m[5]);
    let min = |v: [f64; 4]| v.into_iter().fold(f64::MAX, f64::min);
    let max = |v: [f64; 4]| v.into_iter().fold(f64::MIN, f64::max);
    (min(xs), min(ys), max(xs) - min(xs), max(ys) - min(ys))
}

/// Where a text-like element goes, and how much it is scaled. Older files use `x` and `y`.
fn text_origin(a: &BTreeMap<String, String>, log: &mut LossLog) -> Option<(f64, f64, f64)> {
    match matrix(a) {
        Some(m) => {
            if !is_straight(&m) {
                log.note(ROTATED);
            }
            Some((m[4], m[5], m[0].hypot(m[1])))
        }
        None => Some((num(a, "x")?, num(a, "y")?, 1.0)),
    }
}

fn attrs(e: &BytesStart<'_>) -> Result<BTreeMap<String, String>, ImportError> {
    let mut map = BTreeMap::new();
    for a in e.attributes() {
        let a = a.map_err(malformed)?;
        let key: &str = a.key.as_ref();
        let value = a
            .normalized_value(XmlVersion::default())
            .map_err(malformed)?
            .into_owned();
        map.insert(key.to_string(), value);
    }
    Ok(map)
}

fn num(a: &BTreeMap<String, String>, k: &str) -> Option<f64> {
    a.get(k)?
        .trim()
        .parse()
        .ok()
        .filter(|v: &f64| v.is_finite())
}

/// `#rrggbb`, `#rrggbbaa` or one of Xournal's colour names, as colour and opacity.
fn color(s: Option<&String>) -> Option<(String, f64)> {
    let s = s?.trim();
    let named = match s {
        "black" => "#000000",
        "blue" => "#3344cc",
        "red" => "#ff0000",
        "green" => "#008000",
        "gray" => "#808080",
        "lightblue" => "#00c0ff",
        "lightgreen" => "#00ff00",
        "magenta" => "#ff00ff",
        "orange" => "#ff8000",
        "yellow" => "#ffff00",
        "white" => "#ffffff",
        other => other,
    };
    let hex = named.strip_prefix('#')?;
    if !hex.is_ascii() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => Some((format!("#{}", hex.to_lowercase()), 1.0)),
        8 => {
            let alpha = u8::from_str_radix(&hex[6..], 16).ok()? as f64 / 255.0;
            Some((format!("#{}", hex[..6].to_lowercase()), alpha))
        }
        _ => None,
    }
}

/// Decodes base64, ignoring whitespace and padding. `None` on any other byte.
pub(crate) fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut bits, mut n) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        };
        bits = (bits << 6) | v as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    Some(out)
}

fn background(
    a: &BTreeMap<String, String>,
    previous: Option<&Template>,
    log: &mut LossLog,
) -> Template {
    if a.get("style").is_some_and(|s| s == ":copy") {
        // "the same paper as the page before"
        if let Some(t) = previous {
            return t.clone();
        }
    }
    let kind = match a.get("type").map(String::as_str) {
        Some("pdf") => TemplateKind::Blank,
        Some("pixmap") => {
            log.note("{n} page(s) use an image background that was not imported.");
            TemplateKind::Blank
        }
        _ => {
            let solid = a.get("color").and_then(|c| color(Some(c)));
            if solid.is_some_and(|(c, _)| c != "#ffffff") {
                log.note("{n} page(s) have a coloured paper that became white.");
            }
            match a.get("style").map(String::as_str) {
                Some("lined" | "ruled") => TemplateKind::Ruled,
                Some("graph") => TemplateKind::Grid,
                Some("dotted") => TemplateKind::Dotted,
                Some("plain") | None => TemplateKind::Blank,
                Some(_) => {
                    log.note(
                        "{n} page(s) use a paper pattern Penroam doesn't have; they became blank.",
                    );
                    TemplateKind::Blank
                }
            }
        }
    };
    let spacing = match kind {
        TemplateKind::Grid | TemplateKind::Dotted => SQUARE_SIZE,
        _ => Template::default().spacing,
    };
    Template { kind, spacing }
}

/// Xournal++ joins samples with straight lines, while Penroam smooths through them. That makes
/// no difference to handwriting, but the widely spaced corners of a drawn rectangle would melt
/// into a curve, so a long segment gets a point just inside each end to hold the corner.
fn keep_corners(points: Vec<InkPoint>) -> Vec<InkPoint> {
    if points.len() < 3 {
        return points;
    }
    let mut out = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        out.push(*p);
        let Some(q) = points.get(i + 1) else { break };
        let length = (q.x - p.x).hypot(q.y - p.y);
        if length > LONG_SEGMENT {
            let inset = |from: &InkPoint, to: &InkPoint| InkPoint {
                x: from.x + (to.x - from.x) * CORNER_INSET / length,
                y: from.y + (to.y - from.y) * CORNER_INSET / length,
                ..*from
            };
            out.push(inset(p, q));
            out.push(inset(q, p));
        }
    }
    out
}

fn stroke(
    a: &BTreeMap<String, String>,
    data: &str,
    z: i64,
    log: &mut LossLog,
) -> Option<(Element, usize)> {
    let tool = match a.get("tool").map(String::as_str) {
        Some("highlighter") => InkTool::Highlighter,
        _ => InkTool::Pen,
    };
    // the old whiteout eraser paints paper colour, and what it covers is still underneath
    let whiteout = a.get("tool").is_some_and(|t| t == "eraser");
    if a.contains_key("fill") {
        log.note("{n} filled stroke(s) lost their fill.");
    }
    if a.contains_key("style") {
        log.note("{n} dashed stroke(s) became solid.");
    }
    if a.get("fn").is_some_and(|f| !f.is_empty()) {
        log.note("{n} link(s) to audio recordings were lost.");
    }
    let mut widths = a.get("width")?.split_ascii_whitespace();
    let nominal: f64 = widths
        .next()?
        .parse()
        .ok()
        .filter(|w: &f64| w.is_finite() && *w > 0.0)?;
    // Real files have segments of width 0, and even "nan", from a pen lifting off or a bug in
    // an old version. Xournal++ keeps those strokes, and one bad sample shouldn't cost a
    // stroke, so they count as the lightest touch.
    let segments: Vec<f64> = widths
        .map(|w| {
            w.parse()
                .ok()
                .filter(|v: &f64| v.is_finite() && *v > 0.0)
                .unwrap_or(0.0)
        })
        .collect();
    let coords: Vec<f64> = data
        .split_ascii_whitespace()
        .map(|c| c.parse().ok().filter(|v: &f64| v.is_finite()))
        .collect::<Option<_>>()?;
    if coords.len() < 2
        || !coords.len().is_multiple_of(2)
        || coords.len() / 2 > model::MAX_POINTS_PER_STROKE
    {
        return None;
    }
    let (hex, alpha) = match whiteout {
        true => ("#ffffff".to_string(), 1.0),
        false => color(a.get("color")).unwrap_or_else(|| ("#000000".into(), 1.0)),
    };
    let widest = segments.iter().cloned().fold(0.0, f64::max);
    let pressure = widest > 0.0 && tool == InkTool::Pen;
    // Xournal stores a width per segment; Penroam stores pressure per sample.
    // The base is chosen so the widest segment lands exactly on full pressure.
    let base = if pressure {
        widest / MAX_WIDTH_FACTOR
    } else {
        nominal
    };
    let points = coords
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
        .map(|(i, c)| InkPoint {
            x: c[0],
            y: c[1],
            pressure: match segments.get(i.min(segments.len().saturating_sub(1))) {
                Some(w) if pressure => ((w / base - 0.4) / 1.2).clamp(0.0, 1.0),
                _ => FLAT_PRESSURE,
            },
            tilt_x: 0.0,
            tilt_y: 0.0,
            time_us: 0,
            twist: None,
            flags: None,
        })
        .collect::<Vec<_>>();
    let points = keep_corners(points);
    if points.len() > model::MAX_POINTS_PER_STROKE {
        return None;
    }
    let n = points.len();
    let opacity = match tool {
        InkTool::Highlighter if alpha > 0.99 => 0.5,
        _ => alpha,
    };
    Some((
        Element::Ink(InkStroke {
            id: Uuid::new_v4(),
            z,
            tool,
            color: hex,
            opacity,
            width: base,
            pressure,
            source: "import".into(),
            input: Default::default(),
            points,
            extra: Default::default(),
        }),
        n,
    ))
}

/// Builds a text block from Markdown that is already escaped.
fn block(
    origin: (f64, f64, f64),
    a: &BTreeMap<String, String>,
    plain: &str,
    content: String,
    z: i64,
) -> Option<Element> {
    if content.len() > model::MAX_TEXT_BYTES {
        return None;
    }
    let (x, y, scale) = origin;
    let font_size = (num(a, "size").unwrap_or(12.0) * scale).clamp(4.0, 200.0);
    let natural = plain
        .lines()
        .map(|l| text::measure(l, font_size))
        .fold(font_size, f64::max)
        + font_size;
    let width = num(a, "wrap").map_or(natural, |w| (w * scale).max(font_size));
    let align = match a.get("align").map(String::as_str) {
        Some("center") => TextAlign::Center,
        Some("right") => TextAlign::Right,
        _ => TextAlign::Left,
    };
    let no_math = |_: &str, _: bool| -> Option<MathBox> { None };
    let height = markdown::layout(
        &content,
        TextFont::Formatted,
        font_size,
        width,
        None,
        align,
        &no_math,
    )
    .height;
    Some(Element::Text(TextBlock {
        id: Uuid::new_v4(),
        z,
        x,
        y,
        width,
        height,
        text: content,
        font_size,
        color: color(a.get("color")).map_or("#000000".into(), |c| c.0),
        align,
        font: Default::default(),
        row_pitch: None,
        extra: Default::default(),
    }))
}

fn text_block(
    a: &BTreeMap<String, String>,
    body: &str,
    z: i64,
    log: &mut LossLog,
) -> Option<Element> {
    let origin = text_origin(a, log)?;
    let escaped: Vec<String> = body.split('\n').map(escape_markdown).collect();
    block(origin, a, body, escaped.join("\n"), z)
}

/// A hyperlink. Markdown links can't span lines, so each line links on its own, and a link
/// to anything but the web or mail keeps its text.
fn link_block(
    a: &BTreeMap<String, String>,
    body: &str,
    z: i64,
    log: &mut LossLog,
) -> Option<Element> {
    let origin = text_origin(a, log)?;
    let url = a.get("url").map(|u| u.trim()).unwrap_or("");
    let lower = url.to_ascii_lowercase();
    let safe = ["http://", "https://", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s))
        && !url.contains(['<', '>', '\n', '\r']);
    if !safe {
        log.note("{n} link(s) to something other than the web or email kept their text only.");
    }
    let lines: Vec<String> = body
        .split('\n')
        .map(|l| match (safe, l.trim().is_empty()) {
            (true, false) => format!("[{}](<{url}>)", escape_markdown(l)),
            _ => escape_markdown(l),
        })
        .collect();
    block(origin, a, body, lines.join("\n"), z)
}

/// A LaTeX image as a Penroam formula: the source is kept and typeset here, so the formula
/// stays editable. The rendered copy in the file isn't needed.
fn formula_block(a: &BTreeMap<String, String>, z: i64, log: &mut LossLog) -> Option<Element> {
    let tex = a.get("text")?.trim().trim_matches('$').trim();
    if tex.is_empty() {
        return None;
    }
    log.note_once(FORMULAS);
    // old files give a box, about one line tall for an ordinary formula
    let boxed = num(a, "left").zip(num(a, "top"));
    let tall = num(a, "bottom").zip(num(a, "top")).map(|(b, t)| b - t);
    let wide = num(a, "right").zip(num(a, "left")).map(|(r, l)| r - l);
    let (x, y, scale) = match boxed {
        Some((x, y)) => (x, y, 1.0),
        None => text_origin(a, log)?,
    };
    let font_size = tall.map_or(14.0 * scale, |h| h * 0.7).clamp(8.0, 72.0);
    let content = format!("${tex}$");
    let width = wide
        .unwrap_or(0.0)
        .max(tex.chars().count() as f64 * font_size * 0.7)
        .max(font_size * 2.0);
    if content.len() > model::MAX_TEXT_BYTES {
        return None;
    }
    Some(Element::Text(TextBlock {
        id: Uuid::new_v4(),
        z,
        x,
        y,
        width,
        height: font_size * 1.5,
        text: content,
        font_size,
        color: color(a.get("color")).map_or("#000000".into(), |c| c.0),
        align: TextAlign::Left,
        font: Default::default(),
        row_pitch: None,
        extra: Default::default(),
    }))
}

fn image(
    a: &BTreeMap<String, String>,
    bytes: Vec<u8>,
    z: i64,
    assets: &mut BTreeMap<String, ImportedAsset>,
    log: &mut LossLog,
) -> Option<Element> {
    let media = check_image(&bytes).ok()?;
    let (x, y, width, height) = match matrix(a) {
        Some(m) => {
            if !is_straight(&m) {
                log.note(ROTATED);
            }
            let (w, h) = pixel_size(&bytes)?;
            bounds(&m, w as f64, h as f64)
        }
        None => {
            let (x, y) = (num(a, "left")?, num(a, "top")?);
            (x, y, num(a, "right")? - x, num(a, "bottom")? - y)
        }
    };
    let asset = ImportedAsset::new(bytes, media);
    let asset_hash = asset.sha256.clone();
    assets.entry(asset_hash.clone()).or_insert(asset);
    Some(Element::Image(ImageElement {
        id: Uuid::new_v4(),
        z,
        x,
        y,
        width,
        height,
        asset_hash,
        pdf_page: None,
        extra: Default::default(),
    }))
}

/// Applies a `<background>` to the page being read, bringing in a PDF that is packaged with
/// the document. Any other PDF is named by a path on someone's disk, which is never followed.
fn apply_background(
    a: &BTreeMap<String, String>,
    pages: &mut [ImportedPage],
    package: &mut Option<zip::ZipArchive<Cursor<&[u8]>>>,
    assets: &mut BTreeMap<String, ImportedAsset>,
    current_pdf: &mut Option<String>,
    log: &mut LossLog,
) {
    let previous = pages
        .len()
        .checked_sub(2)
        .map(|i| pages[i].template.clone());
    let template = background(a, previous.as_ref(), log);
    let mut pdf = None;
    if a.get("type").is_some_and(|t| t == "pdf") {
        if let (Some(zip), Some(path)) = (package.as_mut(), a.get("filename")) {
            let bytes = entry(zip, path, MAX_ATTACHED_PDF).filter(|b| b.starts_with(b"%PDF"));
            if let Some(bytes) = bytes {
                let asset = ImportedAsset::new(bytes, "application/pdf");
                *current_pdf = Some(asset.sha256.clone());
                assets.entry(asset.sha256.clone()).or_insert(asset);
            }
        }
        // some versions write the number with a C++ suffix: "2ll"
        let number: Option<u32> = a.get("pageno").and_then(|n| {
            let digits: String = n.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        });
        pdf = current_pdf
            .clone()
            .zip(number.and_then(|n| n.checked_sub(1)));
        if pdf.is_none() {
            log.note("{n} page(s) use a PDF background, which is stored outside the document. Those pages were imported without it; use Import PDF to bring the PDF in.");
        }
    }
    if let Some(p) = pages.last_mut() {
        p.template = template;
        p.pdf = pdf;
    }
}

#[derive(PartialEq)]
enum Body {
    None,
    Stroke,
    Text,
    Link,
    Image,
    Formula,
}

/// Imports a Xournal++ or Xournal document. `title` is used for the notebook.
/// Errors when the file isn't a readable document; partial losses are in the
/// report.
pub fn import_xournal(bytes: &[u8], title: &str) -> Result<ImportedDocument, ImportError> {
    let (xml, mut package) = open_document(bytes)?;
    let xml = std::str::from_utf8(&xml).map_err(malformed)?;
    let mut reader = Reader::from_str(xml);
    let mut log = LossLog::default();
    let mut pages: Vec<ImportedPage> = Vec::new();
    let mut assets: BTreeMap<String, ImportedAsset> = BTreeMap::new();
    let mut kept = Kept::default();
    let (mut depth, mut saw_root, mut total_points) = (0usize, false, 0usize);
    let mut in_page = false;
    let mut layers = 0u32;
    let mut z = 0i64;
    let mut body = Body::None;
    let mut body_attrs = BTreeMap::new();
    let mut buf = String::new();
    // set by an <attachment> inside an <image>
    let mut attachment: Option<String> = None;
    // later pages of a PDF background don't repeat its file name
    let mut current_pdf: Option<String> = None;

    let drop_element = |log: &mut LossLog, what: Body| match what {
        Body::Stroke => {
            log.note("{n} stroke(s) were damaged or over the size limit and were left out.")
        }
        Body::Text | Body::Link | Body::Formula => {
            log.note("{n} text block(s) were damaged or over the size limit and were left out.")
        }
        Body::Image => log.note(
            "{n} image(s) were damaged, of an unsupported type or too large, and were left out.",
        ),
        Body::None => {}
    };

    loop {
        let ev = reader.read_event().map_err(malformed)?;
        match ev {
            Event::Start(e) => {
                depth += 1;
                let a = attrs(&e)?;
                match <_ as AsRef<str>>::as_ref(&e.local_name()) {
                    "xournal" if depth == 1 => {
                        saw_root = true;
                        if num(&a, "fileversion").is_some_and(|v| v > KNOWN_FILE_VERSION as f64) {
                            log.note("This file comes from a newer Xournal++ than Penroam knows. Anything new in it may be missing.");
                        }
                    }
                    "page" if depth == 2 => {
                        if pages.len() >= model::MAX_PAGES {
                            return Err(ImportError::TooLarge("more than 10 000 pages".into()));
                        }
                        let (w, h) = (num(&a, "width"), num(&a, "height"));
                        let range = 1.0..=model::MAX_PAGE_SIDE_PT;
                        let (Some(width), Some(height)) = (
                            w.filter(|v| range.contains(v)),
                            h.filter(|v| range.contains(v)),
                        ) else {
                            return Err(malformed("a page has unsupported dimensions"));
                        };
                        pages.push(ImportedPage {
                            size: Size { width, height },
                            template: Template {
                                kind: TemplateKind::Blank,
                                ..Template::default()
                            },
                            elements: Vec::new(),
                            pdf: None,
                        });
                        in_page = true;
                        layers = 0;
                        z = 0;
                    }
                    "background" if in_page => {
                        apply_background(
                            &a,
                            &mut pages,
                            &mut package,
                            &mut assets,
                            &mut current_pdf,
                            &mut log,
                        );
                    }
                    "layer" if in_page => {
                        layers += 1;
                        if layers == 2 {
                            log.note("{n} page(s) had several layers, which were merged into one.");
                        }
                    }
                    "stroke" if in_page => (body, body_attrs) = (Body::Stroke, a),
                    "text" if in_page => (body, body_attrs) = (Body::Text, a),
                    "image" if in_page => {
                        (body, body_attrs) = (Body::Image, a);
                        attachment = None;
                    }
                    "link" if in_page => (body, body_attrs) = (Body::Link, a),
                    "teximage" if in_page => (body, body_attrs) = (Body::Formula, a),
                    _ => {}
                }
                buf.clear();
            }
            Event::Empty(e) => {
                let a = attrs(&e)?;
                let local = e.local_name();
                let name: &str = local.as_ref();
                if in_page && name == "background" {
                    apply_background(
                        &a,
                        &mut pages,
                        &mut package,
                        &mut assets,
                        &mut current_pdf,
                        &mut log,
                    );
                } else if name == "attachment" && body == Body::Image {
                    attachment = a.get("path").cloned();
                } else if in_page && name == "audio" {
                    log.note("{n} link(s) to audio recordings were lost.");
                }
            }
            Event::Text(t) if body != Body::None => {
                buf.push_str(&t.xml10_content());
            }
            Event::CData(t) if body != Body::None => {
                buf.push_str(&t.xml10_content());
            }
            Event::GeneralRef(r) if body != Body::None => {
                let name = r.xml10_content();
                if let Some(c) = r.resolve_char_ref().map_err(malformed)? {
                    buf.push(c);
                } else if let Some(s) = quick_xml::escape::resolve_predefined_entity(name.as_ref())
                {
                    buf.push_str(s);
                } else {
                    return Err(malformed(format!("unknown entity {name}")));
                }
            }
            Event::GeneralRef(_) => {}
            Event::End(e) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| malformed("unbalanced XML"))?;
                let local = e.local_name();
                let name: &str = local.as_ref();
                if name == "page" && depth == 1 {
                    in_page = false;
                }
                let closing = matches!(
                    (&body, name),
                    (Body::Stroke, "stroke")
                        | (Body::Text, "text")
                        | (Body::Link, "link")
                        | (Body::Image, "image")
                        | (Body::Formula, "teximage")
                );
                if closing {
                    z += 1;
                    let what = std::mem::replace(&mut body, Body::None);
                    let page = pages.last_mut().expect("in a page");
                    let tool_is_eraser = body_attrs.get("tool").is_some_and(|t| t == "eraser");
                    let made = match what {
                        Body::Stroke => stroke(&body_attrs, &buf, z, &mut log).map(|(el, n)| {
                            total_points += n;
                            el
                        }),
                        Body::Text => text_block(&body_attrs, &buf, z, &mut log),
                        Body::Link => link_block(&body_attrs, &buf, z, &mut log),
                        Body::Formula => formula_block(&body_attrs, z, &mut log),
                        Body::Image => {
                            let bytes = match attachment.take() {
                                Some(path) => package.as_mut().and_then(|zip| {
                                    entry(zip, &path, limits::MAX_IMAGE_BYTES as u64)
                                }),
                                None if buf.len() / 4 * 3 > limits::MAX_IMAGE_BYTES => None,
                                None => base64(&buf),
                            };
                            bytes.and_then(|b| image(&body_attrs, b, z, &mut assets, &mut log))
                        }
                        Body::None => None,
                    };
                    if total_points > limits::MAX_TOTAL_POINTS {
                        return Err(ImportError::TooLarge("too many ink samples".into()));
                    }
                    match made.filter(|el| validate_element(el).is_ok()) {
                        Some(_) if page.elements.len() >= model::MAX_ELEMENTS_PER_PAGE => {
                            log.note("{n} element(s) over the per-page limit were left out.");
                        }
                        Some(el) => {
                            match el {
                                Element::Ink(_) => kept.strokes += 1,
                                Element::Text(_) => kept.text_blocks += 1,
                                Element::Image(_) => kept.images += 1,
                            }
                            page.elements.push(el);
                        }
                        None if tool_is_eraser => {}
                        None => drop_element(&mut log, what),
                    }
                    buf.clear();
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_root {
        return Err(malformed("it is not a Xournal document"));
    }
    if depth != 0 {
        return Err(malformed("the file ends early"));
    }
    if pages.is_empty() {
        return Err(malformed("it has no pages"));
    }
    if kept.text_blocks > 0 {
        log.note_once("Text uses Penroam's font, so line breaks may fall differently.");
    }
    kept.pages = pages.len() as u32;
    Ok(ImportedDocument {
        title: title.to_string(),
        pages,
        assets: assets.into_values().collect(),
        report: ImportLoss {
            format: "Xournal++".into(),
            kept,
            lost: log.sentences(),
        },
    })
}
