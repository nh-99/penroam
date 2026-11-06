//! PDF support for Penroam, built on `lopdf` (MIT).
//!
//! * [`inspect`] reads page boxes and rotation so imported PDF pages keep
//!   their exact size, crop box and orientation. The original bytes are stored
//!   untouched as an asset; nothing is rasterised.
//! * [`export_annotated`] writes a new PDF in notebook page order. Source PDF
//!   pages are copied intact (their content streams are wrapped in `q … Q`
//!   and left unmodified) and the Penroam ink and text are appended as
//!   **vector** content in the same document-space transform the editor uses.
//!   Blank/template pages become new PDF pages. The editable data stays in
//!   the notebook; the PDF is a one-way rendering.
//! * [`keep_pages`] cuts such an export down to chosen pages, for printing.
//!
//! # Coordinates
//!
//! Penroam pages use *display* coordinates in points: origin at the top-left
//! of the visible page, y pointing down, after the PDF's `/Rotate` has been
//! applied. PDF user space has y pointing up and is not rotated. For pages
//! with a PDF background the visible area is the crop box (clipped to the
//! media box, as PDF.js shows it), and
//! [`PdfBackgroundRef::doc_to_pdf_matrix`] maps display coordinates to user
//! space for each of the four rotations. Template-only pages use a plain
//! y-flip (`1 0 0 -1 0 height`). Export writes all annotations inside a
//! single `cm` with that matrix, so ink and text are emitted in display
//! coordinates and land exactly where the editor drew them.

#![warn(missing_docs)]

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use penroam_core::geometry::{stroke_pieces, Piece};
use penroam_core::markdown::{self, MathBox, Paint};
use penroam_core::text::{metrics, Face};
use penroam_core::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use ts_rs::TS;

/// PDF import/export failures (messages are safe to show to the user).
#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    /// The file is not a readable PDF, or a page has an unsupported size.
    #[error("this PDF could not be read: {0}")]
    Parse(String),
    /// Encrypted PDFs are refused rather than decrypted.
    #[error("this PDF is password-protected; remove the password and import it again")]
    Encrypted,
    /// The PDF has no pages (or nothing was given to export).
    #[error("this PDF has no pages")]
    NoPages,
    /// More than [`MAX_PAGES`] pages.
    #[error("this PDF has too many pages ({0}; the limit is 5000)")]
    TooManyPages(usize),
    /// Export: a page refers to a PDF asset (or page index) that is not available.
    #[error("the source PDF for a page is missing ({0})")]
    MissingSource(String),
    /// Export: serializing the output failed.
    #[error("writing the PDF failed: {0}")]
    Write(String),
}

/// Most pages accepted from one PDF.
pub const MAX_PAGES: usize = 5000;

/// Geometry of one PDF page, as needed to show it as a page background.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PdfPageInfo {
    /// Zero-based page index in the PDF.
    pub index: u32,
    /// Normalized media box `[x0, y0, x1, y1]` in PDF user space (inherited
    /// from the page tree if needed; Letter when missing).
    pub media_box: [f64; 4],
    /// Effective crop box: the declared crop box clipped to the media box
    /// (the media box if that leaves less than 1 pt).
    pub crop_box: [f64; 4],
    /// Effective `/Rotate`, normalized to 0, 90, 180 or 270 (anything that
    /// is not a multiple of 90 counts as 0).
    pub rotation: u32,
}

impl PdfPageInfo {
    /// The page background reference to store in a Penroam page for this
    /// PDF page, given the SHA-256 of the stored PDF asset.
    pub fn background(&self, asset_hash: &str) -> PdfBackgroundRef {
        PdfBackgroundRef {
            asset_hash: asset_hash.to_string(),
            page_index: self.index,
            media_box: self.media_box,
            crop_box: self.crop_box,
            rotation: self.rotation,
        }
    }
}

fn load(bytes: &[u8]) -> Result<Document, PdfError> {
    let doc = Document::load_mem(bytes).map_err(|e| {
        let s = e.to_string();
        if s.to_lowercase().contains("password") || s.to_lowercase().contains("decrypt") {
            PdfError::Encrypted
        } else {
            PdfError::Parse(s)
        }
    })?;
    if doc.is_encrypted() {
        return Err(PdfError::Encrypted);
    }
    Ok(doc)
}

/// Lists the pages of a PDF with their effective boxes and rotation.
pub fn inspect(bytes: &[u8]) -> Result<Vec<PdfPageInfo>, PdfError> {
    let doc = load(bytes)?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err(PdfError::NoPages);
    }
    if pages.len() > MAX_PAGES {
        return Err(PdfError::TooManyPages(pages.len()));
    }
    pages
        .values()
        .enumerate()
        .map(|(i, id)| page_info(&doc, *id, i as u32))
        .collect()
}

/// Most decompressed content bytes read per page by [`page_texts`]. A small compressed stream
/// can inflate without bound (a "decompression bomb"); a page over the limit yields no text.
pub const MAX_TEXT_CONTENT_BYTES: usize = 64 << 20;

/// The selectable text of every page, in page order (the same order and indices as
/// [`inspect`] and [`PdfBackgroundRef::page_index`]), for the local search index.
///
/// Text comes from the PDF's own text operators, so scanned pages yield an empty string. A
/// page whose content cannot be decoded, or exceeds [`MAX_TEXT_CONTENT_BYTES`], also yields an
/// empty string rather than failing the whole document. Errors are those of [`inspect`]: the
/// file is unreadable, encrypted, empty or too long.
pub fn page_texts(bytes: &[u8]) -> Result<Vec<String>, PdfError> {
    let doc = load(bytes)?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err(PdfError::NoPages);
    }
    if pages.len() > MAX_PAGES {
        return Err(PdfError::TooManyPages(pages.len()));
    }
    Ok(pages
        .keys()
        .map(|n| {
            doc.extract_text_with_limit(&[*n], MAX_TEXT_CONTENT_BYTES)
                .unwrap_or_default()
        })
        .collect())
}

fn num(doc: &Document, o: &Object) -> Option<f64> {
    let o = doc.dereference(o).ok()?.1;
    o.as_float().ok().map(f64::from)
}

fn rect(doc: &Document, o: &Object) -> Option<[f64; 4]> {
    let arr = doc.dereference(o).ok()?.1.as_array().ok()?;
    if arr.len() != 4 {
        return None;
    }
    let v: Vec<f64> = arr.iter().filter_map(|x| num(doc, x)).collect();
    if v.len() != 4 || v.iter().any(|x| !x.is_finite()) {
        return None;
    }
    Some([
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    ])
}

/// Looks up a page attribute, following `/Parent` for inheritable keys.
fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Option<&'a Object> {
    let mut id = page;
    for _ in 0..64 {
        let d = doc.get_dictionary(id).ok()?;
        if let Ok(v) = d.get(key) {
            return Some(v);
        }
        id = d.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

fn page_info(doc: &Document, id: ObjectId, index: u32) -> Result<PdfPageInfo, PdfError> {
    let media = inherited(doc, id, b"MediaBox")
        .and_then(|o| rect(doc, o))
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop = inherited(doc, id, b"CropBox")
        .and_then(|o| rect(doc, o))
        .unwrap_or(media);
    // PDF.js shows the intersection of the crop box and the media box.
    let crop = [
        crop[0].max(media[0]),
        crop[1].max(media[1]),
        crop[2].min(media[2]),
        crop[3].min(media[3]),
    ];
    let crop = if crop[2] - crop[0] < 1.0 || crop[3] - crop[1] < 1.0 {
        media
    } else {
        crop
    };
    let rot = inherited(doc, id, b"Rotate")
        .and_then(|o| num(doc, o))
        .unwrap_or(0.0) as i64;
    let rotation = if rot % 90 == 0 {
        rot.rem_euclid(360) as u32
    } else {
        0
    };
    let w = crop[2] - crop[0];
    let h = crop[3] - crop[1];
    if !(1.0..=14_400.0).contains(&w) || !(1.0..=14_400.0).contains(&h) {
        return Err(PdfError::Parse(format!(
            "page {} has an unsupported size {w}x{h}",
            index + 1
        )));
    }
    Ok(PdfPageInfo {
        index,
        media_box: media,
        crop_box: crop,
        rotation,
    })
}

// ------------------------------------------------------------------ export --

/// Summary of an annotated-PDF export.
#[derive(Debug, Clone, Default, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ExportReport {
    /// Pages written.
    pub pages: u32,
    /// Ink strokes written as vector paths.
    pub strokes: u32,
    /// Text blocks written.
    pub text_blocks: u32,
    /// Characters outside the PDF's standard-font character set that were
    /// replaced with "?" (the editable notebook keeps the original text).
    pub replaced_characters: u32,
    /// Things that could not be exported (e.g. image elements).
    pub warnings: Vec<String>,
}

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

/// Builds the annotated PDF for `pages`, in the given order. `asset`
/// returns the bytes of a stored asset by SHA-256 (the PDF backgrounds).
///
/// Formulas are drawn from `math` (see [`RenderedMath`]); one that isn't listed
/// is written as its TeX source.
///
/// Each source PDF is merged into the output once (its objects renumbered,
/// inherited page attributes resolved first) and its pages are reused with
/// their original content wrapped in `q … Q`, followed by one Penroam content
/// stream. Opacity (highlighter) uses `ExtGState` entries; text uses the
/// standard Helvetica and Courier fonts with WinAnsi encoding.
pub fn export_annotated(
    pages: &[Page],
    math: &[RenderedMath],
    mut asset: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Result<(Vec<u8>, ExportReport), PdfError> {
    let mut out = Document::with_version("1.7");
    let mut report = ExportReport::default();
    let pages_root = out.new_object_id();
    let formulas: MathTable<'_> = math
        .iter()
        .map(|m| ((m.tex.as_str(), m.display), m))
        .collect();
    let fonts = FONTS.map(|(_, _, base)| {
        out.add_object(Dictionary::from_iter(vec![
            ("Type", name("Font")),
            ("Subtype", name("Type1")),
            ("BaseFont", name(base)),
            ("Encoding", name("WinAnsiEncoding")),
        ]))
    });
    // source documents merged into `out`, keyed by asset hash -> page object ids
    let mut sources: HashMap<String, Vec<ObjectId>> = HashMap::new();
    let mut kids = Vec::new();

    for page in pages {
        // ExtGState names (one per distinct opacity) used by this page's annotations
        let mut gs: BTreeMap<String, f64> = BTreeMap::new();
        let annot = annotation_content(page, &formulas, &mut gs, &mut report);
        let annot_id = out
            .add_object(Stream::new(Dictionary::new(), annot.into_bytes()).with_compression(true));
        let mut ext = Dictionary::new();
        for (n, alpha) in &gs {
            ext.set(
                n.as_bytes().to_vec(),
                Object::Dictionary(Dictionary::from_iter(vec![
                    ("Type", name("ExtGState")),
                    ("CA", Object::Real(*alpha as f32)),
                    ("ca", Object::Real(*alpha as f32)),
                ])),
            );
        }

        let page_dict = if let Some(bg) = &page.background {
            if !sources.contains_key(&bg.asset_hash) {
                let bytes = asset(&bg.asset_hash)
                    .ok_or_else(|| PdfError::MissingSource(bg.asset_hash.clone()))?;
                let mut src = load(&bytes)?;
                // move the source's object ids above ours so both can live in one document
                src.renumber_objects_with(out.max_id + 1);
                let ids: Vec<ObjectId> = src.get_pages().values().copied().collect();
                out.max_id = out.max_id.max(src.max_id);
                // resolve inherited attributes now, while the source page tree exists
                let mut resolved = Vec::new();
                for id in &ids {
                    let mut d = src
                        .get_dictionary(*id)
                        .map_err(|e| PdfError::Parse(e.to_string()))?
                        .clone();
                    for key in [&b"Resources"[..], b"MediaBox", b"CropBox", b"Rotate"] {
                        if !d.has(key) {
                            if let Some(v) = inherited(&src, *id, key) {
                                d.set(key.to_vec(), v.clone());
                            }
                        }
                    }
                    resolved.push((*id, d));
                }
                out.objects.extend(src.objects);
                for (id, d) in resolved {
                    out.objects.insert(id, Object::Dictionary(d));
                }
                sources.insert(bg.asset_hash.clone(), ids);
            }
            let src_id = *sources[&bg.asset_hash]
                .get(bg.page_index as usize)
                .ok_or_else(|| {
                    PdfError::MissingSource(format!(
                        "page {} of {}",
                        bg.page_index + 1,
                        bg.asset_hash
                    ))
                })?;
            let mut d = out
                .get_dictionary(src_id)
                .map_err(|e| PdfError::Parse(e.to_string()))?
                .clone();
            // wrap the original content so its graphics state cannot leak into ours
            let q = out.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
            let qq = out.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()));
            let mut contents = vec![Object::Reference(q)];
            match d.get(b"Contents") {
                Ok(Object::Reference(r)) => match out.get_object(*r) {
                    Ok(Object::Array(a)) => contents.extend(a.iter().cloned()),
                    _ => contents.push(Object::Reference(*r)),
                },
                Ok(Object::Array(a)) => contents.extend(a.iter().cloned()),
                _ => {}
            }
            contents.push(Object::Reference(qq));
            contents.push(Object::Reference(annot_id));
            d.set("Contents", Object::Array(contents));
            let mut res = match d.get(b"Resources") {
                Ok(o) => out
                    .dereference(o)
                    .ok()
                    .and_then(|(_, o)| o.as_dict().ok().cloned())
                    .unwrap_or_default(),
                Err(_) => Dictionary::new(),
            };
            merge_resources(&out, &mut res, &fonts, ext);
            d.set("Resources", Object::Dictionary(res));
            d.set("Parent", Object::Reference(pages_root));
            d
        } else {
            let mut res = Dictionary::new();
            merge_resources(&out, &mut res, &fonts, ext);
            Dictionary::from_iter(vec![
                ("Type", name("Page")),
                ("Parent", Object::Reference(pages_root)),
                (
                    "MediaBox",
                    Object::Array(vec![
                        0.into(),
                        0.into(),
                        Object::Real(page.size.width as f32),
                        Object::Real(page.size.height as f32),
                    ]),
                ),
                ("Contents", Object::Reference(annot_id)),
                ("Resources", Object::Dictionary(res)),
            ])
        };
        kids.push(Object::Reference(out.add_object(page_dict)));
        report.pages += 1;
    }
    if kids.is_empty() {
        return Err(PdfError::NoPages);
    }
    out.objects.insert(
        pages_root,
        Object::Dictionary(Dictionary::from_iter(vec![
            ("Type", name("Pages")),
            ("Count", Object::Integer(kids.len() as i64)),
            ("Kids", Object::Array(kids)),
        ])),
    );
    let catalog = out.add_object(Dictionary::from_iter(vec![
        ("Type", name("Catalog")),
        ("Pages", Object::Reference(pages_root)),
    ]));
    out.trailer = Dictionary::new();
    out.trailer.set("Root", Object::Reference(catalog));
    out.prune_objects();
    out.compress();
    let mut buf = Vec::new();
    out.save_to(&mut buf)
        .map_err(|e| PdfError::Write(e.to_string()))?;
    Ok((buf, report))
}

/// Adds the Penroam fonts and opacity states to a page's resources without
/// disturbing the source page's own entries.
fn merge_resources(out: &Document, res: &mut Dictionary, fonts: &[ObjectId], ext: Dictionary) {
    let sub = |key: &[u8], res: &Dictionary| -> Dictionary {
        res.get(key)
            .ok()
            .and_then(|o| out.dereference(o).ok())
            .and_then(|(_, o)| o.as_dict().ok().cloned())
            .unwrap_or_default()
    };
    let mut dict = sub(b"Font", res);
    for ((_, resource, _), id) in FONTS.iter().zip(fonts) {
        dict.set(*resource, Object::Reference(*id));
    }
    res.set("Font", Object::Dictionary(dict));
    if !ext.is_empty() {
        let mut gs = sub(b"ExtGState", res);
        for (k, v) in ext.iter() {
            gs.set(k.clone(), v.clone());
        }
        res.set("ExtGState", Object::Dictionary(gs));
    }
}

fn rgb(hex: &str) -> (f64, f64, f64) {
    let p = |i: usize| {
        u8::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f64 / 255.0
    };
    (p(1), p(3), p(5))
}

fn f(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Cuts a PDF written by [`export_annotated`] down to the zero-based pages in `keep`, in
/// document order. Used to print part of a notebook.
///
/// This relies on [`export_annotated`] giving every page its own resolved attributes, so a page
/// loses nothing when it moves directly under the root of the page tree. Do not use it on PDFs
/// from elsewhere, whose pages may inherit attributes from intermediate tree nodes.
///
/// # Errors
/// [`PdfError::NoPages`] if `keep` names no page of the document, [`PdfError::Parse`] if the
/// bytes are not a readable PDF and [`PdfError::Write`] if saving the result fails.
pub fn keep_pages(pdf: &[u8], keep: &[u32]) -> Result<Vec<u8>, PdfError> {
    let mut doc = Document::load_mem(pdf).map_err(|e| PdfError::Parse(e.to_string()))?;
    let pages = doc.get_pages();
    let mut kept: Vec<ObjectId> = keep
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        // lopdf numbers pages from 1
        .filter_map(|&i| pages.get(&(i + 1)).copied())
        .collect();
    if kept.is_empty() {
        return Err(PdfError::NoPages);
    }
    let root = doc
        .catalog()
        .and_then(|c| c.get(b"Pages"))
        .and_then(Object::as_reference)
        .map_err(|e| PdfError::Parse(e.to_string()))?;
    for id in &kept {
        doc.get_dictionary_mut(*id)
            .map_err(|e| PdfError::Parse(e.to_string()))?
            .set("Parent", Object::Reference(root));
    }
    let tree = doc
        .get_dictionary_mut(root)
        .map_err(|e| PdfError::Parse(e.to_string()))?;
    tree.set("Count", kept.len() as i64);
    tree.set(
        "Kids",
        kept.drain(..)
            .map(Object::Reference)
            .collect::<Vec<Object>>(),
    );
    // Page content is what makes notes sensitive: nothing dropped may stay in the file.
    doc.prune_objects();
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| PdfError::Write(e.to_string()))?;
    Ok(out)
}

/// Content stream with the page template, ink and text in display
/// coordinates (y down), mapped to PDF user space by one `cm`.
fn annotation_content(
    page: &Page,
    math: &MathTable<'_>,
    gs: &mut BTreeMap<String, f64>,
    report: &mut ExportReport,
) -> String {
    let mut s = String::new();
    let m = page
        .background
        .as_ref()
        .map(PdfBackgroundRef::doc_to_pdf_matrix)
        .unwrap_or([1.0, 0.0, 0.0, -1.0, 0.0, page.size.height]);
    let (w, h) = (page.size.width, page.size.height);
    let _ = writeln!(
        s,
        "q\n{} {} {} {} {} {} cm",
        f(m[0]),
        f(m[1]),
        f(m[2]),
        f(m[3]),
        f(m[4]),
        f(m[5])
    );
    let _ = writeln!(s, "0 0 {} {} re W n", f(w), f(h));
    template_content(&mut s, &page.template, w, h);
    s.push_str("1 J 1 j\n");
    // paint order matches the editor: ink (by z), then images, then text (by z)
    let rank = |e: &Element| match e {
        Element::Ink(_) => 0,
        Element::Image(_) => 1,
        Element::Text(_) => 2,
    };
    let mut ordered: Vec<&Element> = page.elements.iter().collect();
    ordered.sort_by_key(|e| rank(e)); // stable: keeps z order within a kind
    for e in ordered {
        match e {
            Element::Ink(st) => {
                report.strokes += 1;
                ink_content(&mut s, st, gs);
            }
            Element::Text(t) => {
                report.text_blocks += 1;
                text_content(&mut s, t, math, report);
            }
            Element::Image(_) => report
                .warnings
                .push("image elements are not included in the PDF export yet".into()),
        }
    }
    s.push_str("Q\n");
    report.warnings.dedup();
    s
}

/// Appends the PDF operators that draw a page template (ruled lines, grid
/// or dots) in display coordinates (y down; the caller sets up the `cm`).
/// Keep in sync with the editor's `editor/templates.ts`: ruled lines start
/// three spacings from the top with a margin line three spacings from the left.
pub fn template_content(s: &mut String, t: &Template, w: f64, h: f64) {
    let sp = t.spacing;
    match t.kind {
        TemplateKind::Blank => {}
        TemplateKind::Ruled => {
            s.push_str("0.78 0.824 0.871 RG 0.5 w\n");
            let mut y = sp * 3.0;
            while y <= h - sp {
                let _ = writeln!(s, "0 {} m {} {} l S", f(y), f(w), f(y));
                y += sp;
            }
            let _ = writeln!(
                s,
                "0.941 0.706 0.706 RG 0.6 w {} 0 m {} {} l S",
                f(sp * 3.0),
                f(sp * 3.0),
                f(h)
            );
        }
        TemplateKind::Grid => {
            s.push_str("0.835 0.867 0.902 RG 0.4 w\n");
            let mut x = sp;
            while x < w {
                let _ = writeln!(s, "{} 0 m {} {} l S", f(x), f(x), f(h));
                x += sp;
            }
            let mut y = sp;
            while y < h {
                let _ = writeln!(s, "0 {} m {} {} l S", f(y), f(w), f(y));
                y += sp;
            }
        }
        TemplateKind::Dotted => {
            s.push_str("0.604 0.655 0.71 RG 1.8 w 1 J\n");
            let mut y = sp;
            while y < h {
                let mut x = sp;
                while x < w {
                    let _ = writeln!(s, "{} {} m {} {} l S", f(x), f(y), f(x), f(y));
                    x += sp;
                }
                y += sp;
            }
            s.push_str("0 J\n");
        }
    }
}

/// Appends one geometry piece of a stroke as path operators. `mv` starts a
/// new subpath (`m`); otherwise the piece continues the current one. Dots
/// are zero-length lines, which round caps turn into discs.
fn piece_path(s: &mut String, p: &Piece, mv: bool) {
    match *p {
        Piece::Dot { x, y, .. } => {
            let _ = write!(s, "{} {} m {} {} l ", f(x), f(y), f(x), f(y));
        }
        Piece::Line { x0, y0, x1, y1, .. } => {
            if mv {
                let _ = write!(s, "{} {} m ", f(x0), f(y0));
            }
            let _ = write!(s, "{} {} l ", f(x1), f(y1));
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
            if mv {
                let _ = write!(s, "{} {} m ", f(x0), f(y0));
            }
            // PDF has no quadratic curves: raise the degree (exact conversion)
            let (c1x, c1y) = (x0 + 2.0 / 3.0 * (cx - x0), y0 + 2.0 / 3.0 * (cy - y0));
            let (c2x, c2y) = (x1 + 2.0 / 3.0 * (cx - x1), y1 + 2.0 / 3.0 * (cy - y1));
            let _ = write!(
                s,
                "{} {} {} {} {} {} c ",
                f(c1x),
                f(c1y),
                f(c2x),
                f(c2y),
                f(x1),
                f(y1)
            );
        }
    }
}

/// Appends one ink stroke using the shared stroke geometry
/// (`penroam_core::geometry::stroke_pieces`), so the PDF matches the canvas.
/// Pressure-sensitive pen strokes become one stroked piece per width change;
/// other strokes are a single path with one width. Translucency (the
/// highlighter) is set through a named `ExtGState` recorded in `gs`.
fn ink_content(s: &mut String, st: &InkStroke, gs: &mut BTreeMap<String, f64>) {
    let (r, g, b) = rgb(&st.color);
    s.push_str("q ");
    let alpha = if st.tool == InkTool::Highlighter {
        st.opacity.min(1.0)
    } else {
        st.opacity
    };
    if alpha < 0.999 {
        let key = format!("PRgs{}", (alpha * 1000.0).round() as i64);
        gs.insert(key.clone(), alpha);
        let _ = write!(s, "/{key} gs ");
    }
    let _ = writeln!(
        s,
        "{} {} {} RG {} {} {} rg",
        f(r),
        f(g),
        f(b),
        f(r),
        f(g),
        f(b)
    );
    let pieces = stroke_pieces(&st.points, st.width, st.pressure);
    let variable = st.pressure && st.tool == InkTool::Pen && pieces.len() > 1;
    if variable {
        // one stroked path per piece with its own width (same as the canvas renderer)
        for p in &pieces {
            let _ = write!(s, "{} w ", f(p.width()));
            piece_path(s, p, true);
            s.push_str("S\n");
        }
    } else {
        let _ = write!(
            s,
            "{} w ",
            f(pieces.first().map(|p| p.width()).unwrap_or(st.width))
        );
        for (i, p) in pieces.iter().enumerate() {
            piece_path(s, p, i == 0);
        }
        s.push_str("S\n");
    }
    s.push_str("Q\n");
}

/// Windows-1252 byte for a character, if Helvetica/WinAnsi can show it.
fn winansi(c: char) -> Option<u8> {
    let cp = c as u32;
    match cp {
        0x20..=0x7e | 0xa0..=0xff => Some(cp as u8),
        _ => Some(match cp {
            0x20ac => 0x80,
            0x201a => 0x82,
            0x192 => 0x83,
            0x201e => 0x84,
            0x2026 => 0x85,
            0x2020 => 0x86,
            0x2021 => 0x87,
            0x2c6 => 0x88,
            0x2030 => 0x89,
            0x160 => 0x8a,
            0x2039 => 0x8b,
            0x152 => 0x8c,
            0x17d => 0x8e,
            0x2018 => 0x91,
            0x2019 => 0x92,
            0x201c => 0x93,
            0x201d => 0x94,
            0x2022 => 0x95,
            0x2013 => 0x96,
            0x2014 => 0x97,
            0x2dc => 0x98,
            0x2122 => 0x99,
            0x161 => 0x9a,
            0x203a => 0x9b,
            0x153 => 0x9c,
            0x17e => 0x9e,
            0x178 => 0x9f,
            _ => return None,
        }),
    }
}

/// A formula drawn ahead of export by the app's math renderer, which runs in
/// the webview. Formulas aren't stored in notebooks, so the app asks for this
/// table with every export and PDFs always match the current renderer.
#[derive(Debug, Clone, PartialEq, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RenderedMath {
    /// TeX, as [`markdown::MathSpan::tex`].
    pub tex: String,
    /// Display style.
    pub display: bool,
    /// Advance width in em.
    pub width: f64,
    /// Extent above the baseline in em.
    pub height: f64,
    /// Extent below the baseline in em.
    pub depth: f64,
    /// Filled outline in em from the baseline origin, y down: `M x y`, `L x y`,
    /// `C x1 y1 x2 y2 x y` and `Z`, separated by spaces.
    pub path: String,
}

type MathTable<'a> = HashMap<(&'a str, bool), &'a RenderedMath>;

/// The standard fonts: (face, resource name, PDF base font).
const FONTS: [(Face, &str, &str); 5] = [
    (Face::Regular, "PRHelv", "Helvetica"),
    (Face::Bold, "PRHelvB", "Helvetica-Bold"),
    (Face::Italic, "PRHelvI", "Helvetica-Oblique"),
    (Face::BoldItalic, "PRHelvBI", "Helvetica-BoldOblique"),
    (Face::Mono, "PRCour", "Courier"),
];

fn font_resource(face: Face) -> &'static str {
    FONTS
        .iter()
        .find(|(f, _, _)| *f == face)
        .map_or("PRHelv", |(_, r, _)| r)
}

/// The same layout the editor draws for `t` (see `penroam_core::markdown`).
fn paint_of(t: &TextBlock, math: &MathTable<'_>) -> Paint {
    let size = |tex: &str, display: bool| {
        math.get(&(tex, display)).map(|m| MathBox {
            width: m.width,
            height: m.height,
            depth: m.depth,
        })
    };
    markdown::layout(&t.text, t.font_size, t.width, t.row_pitch, t.align, &size)
}

/// Writes a string as a PDF literal, in WinAnsi. Characters the standard fonts
/// cannot show become `?` and are counted in the report.
fn pdf_literal(text: &str, report: &mut ExportReport) -> String {
    let m = metrics();
    let mut lit = String::new();
    for c in text.chars() {
        let byte = match winansi(c) {
            Some(b) if m.known(c) || c == ' ' => b,
            _ => {
                report.replaced_characters += 1;
                b'?'
            }
        };
        match byte {
            b'(' | b')' | b'\\' => {
                lit.push('\\');
                lit.push(byte as char);
            }
            0x20..=0x7e => lit.push(byte as char),
            _ => {
                let _ = write!(lit, "\\{byte:03o}");
            }
        }
    }
    lit
}

/// Appends a formula's outline, scaled from em to points at (`x`, `y`).
fn math_content(s: &mut String, m: &RenderedMath, x: f64, y: f64, size: f64) {
    let mut tokens = m.path.split_ascii_whitespace();
    while let Some(command) = tokens.next() {
        let (args, op) = match command {
            "M" => (2, "m"),
            "L" => (2, "l"),
            "C" => (6, "c"),
            "Z" => (0, "h"),
            _ => continue,
        };
        for i in 0..args {
            let v: f64 = tokens.next().and_then(|t| t.parse().ok()).unwrap_or(0.0);
            let origin = if i % 2 == 0 { x } else { y };
            let _ = write!(s, "{} ", f(origin + v * size));
        }
        s.push_str(op);
        s.push('\n');
    }
    s.push_str("f\n");
}

/// Appends a text block from the same layout the editor draws.
fn text_content(s: &mut String, t: &TextBlock, math: &MathTable<'_>, report: &mut ExportReport) {
    let paint = paint_of(t, math);
    let (r, g, b) = rgb(&t.color);
    let _ = writeln!(
        s,
        "q {r} {g} {b} rg {r} {g} {b} RG",
        r = f(r),
        g = f(g),
        b = f(b)
    );
    if !paint.runs.is_empty() {
        s.push_str("BT\n");
        for run in &paint.runs {
            let lit = pdf_literal(&run.text, report);
            let _ = writeln!(
                s,
                "/{} {} Tf 1 0 0 -1 {} {} Tm ({lit}) Tj",
                font_resource(run.face),
                f(run.size),
                f(t.x + run.x),
                f(t.y + run.y)
            );
        }
        s.push_str("ET\n");
    }
    for l in &paint.segments {
        let _ = writeln!(
            s,
            "{} w {} {} m {} {} l S",
            f(l.width),
            f(t.x + l.x0),
            f(t.y + l.y0),
            f(t.x + l.x1),
            f(t.y + l.y1)
        );
    }
    for mark in &paint.math {
        if let Some(m) = math.get(&(mark.tex.as_str(), mark.display)) {
            math_content(s, m, t.x + mark.x, t.y + mark.y, mark.size);
        }
    }
    s.push_str("Q\n");
    if report.replaced_characters > 0
        && !report
            .warnings
            .iter()
            .any(|w| w.starts_with("Some characters"))
    {
        report.warnings.push("Some characters are outside the PDF standard font and were exported as \"?\" (the notebook keeps the original text).".into());
    }
}
