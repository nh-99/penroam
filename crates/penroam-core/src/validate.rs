//! Structural validation for documents received from the UI, the disk or an
//! imported archive. Validation never "repairs" data silently: invalid input
//! is rejected with a message, and callers decide what to do (refuse an
//! import, keep a damaged file untouched, …).
//!
//! The [`limits`] are deliberately generous; they exist so that a hostile or
//! corrupt file cannot make the app allocate unbounded memory or produce
//! coordinates that break rendering and PDF export.

use crate::model::*;
use std::collections::HashSet;

/// Hard limits that protect the app against hostile or corrupt files.
pub mod limits {
    /// Longest page side: 200 in, PDF's own user-space limit.
    pub const MAX_PAGE_SIDE_PT: f64 = 14_400.0;
    /// Largest absolute coordinate anywhere on a page.
    pub const MAX_COORD_PT: f64 = 1.0e6;
    /// Samples per ink stroke.
    pub const MAX_POINTS_PER_STROKE: usize = 200_000;
    /// Elements per page.
    pub const MAX_ELEMENTS_PER_PAGE: usize = 500_000;
    /// UTF-8 bytes per text block.
    pub const MAX_TEXT_BYTES: usize = 1 << 20;
    /// Characters per notebook title.
    pub const MAX_TITLE_CHARS: usize = 300;
    /// Pages per notebook (live and trashed).
    pub const MAX_PAGES: usize = 10_000;
}

/// A human-readable reason why a document is invalid.
#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct ValidationError(pub String);

fn err<T>(msg: impl Into<String>) -> Result<T, ValidationError> {
    Err(ValidationError(msg.into()))
}

fn finite(v: f64, what: &str) -> Result<(), ValidationError> {
    if !v.is_finite() || v.abs() > limits::MAX_COORD_PT {
        return err(format!("{what} is not a finite coordinate: {v}"));
    }
    Ok(())
}

/// `#rrggbb` (either case).
pub fn is_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// 64 lowercase hex digits.
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Non-empty after trimming, at most [`limits::MAX_TITLE_CHARS`], no control characters.
pub fn validate_title(t: &str) -> Result<(), ValidationError> {
    if t.trim().is_empty() {
        return err("title is empty");
    }
    if t.chars().count() > limits::MAX_TITLE_CHARS {
        return err("title is too long");
    }
    if t.chars().any(|c| c.is_control()) {
        return err("title contains control characters");
    }
    Ok(())
}

/// Spacing between 4 and 288 pt.
pub fn validate_template(t: &Template) -> Result<(), ValidationError> {
    if !t.spacing.is_finite() || !(4.0..=288.0).contains(&t.spacing) {
        return err(format!("template spacing out of range: {}", t.spacing));
    }
    Ok(())
}

/// Each side between 1 pt and [`limits::MAX_PAGE_SIDE_PT`].
pub fn validate_size(s: &Size) -> Result<(), ValidationError> {
    for v in [s.width, s.height] {
        if !v.is_finite() || !(1.0..=limits::MAX_PAGE_SIDE_PT).contains(&v) {
            return err(format!("page size out of range: {}x{}", s.width, s.height));
        }
    }
    Ok(())
}

/// One element: finite coordinates, sane widths/opacity/colours, bounded sizes.
pub fn validate_element(e: &Element) -> Result<(), ValidationError> {
    match e {
        Element::Ink(s) => {
            if s.points.is_empty() {
                return err(format!("stroke {} has no points", s.id));
            }
            if s.points.len() > limits::MAX_POINTS_PER_STROKE {
                return err(format!("stroke {} has too many points", s.id));
            }
            if !s.width.is_finite() || s.width <= 0.0 || s.width > 500.0 {
                return err(format!("stroke {} width out of range", s.id));
            }
            if !s.opacity.is_finite() || !(0.0..=1.0).contains(&s.opacity) {
                return err(format!("stroke {} opacity out of range", s.id));
            }
            if !is_color(&s.color) {
                return err(format!(
                    "stroke {} has an invalid colour {:?}",
                    s.id, s.color
                ));
            }
            for p in &s.points {
                finite(p.x, "x")?;
                finite(p.y, "y")?;
                if !p.pressure.is_finite() || !(0.0..=1.0).contains(&p.pressure) {
                    return err(format!(
                        "stroke {} pressure out of range: {}",
                        s.id, p.pressure
                    ));
                }
                if !p.tilt_x.is_finite()
                    || !p.tilt_y.is_finite()
                    || p.twist.is_some_and(|t| !t.is_finite())
                {
                    return err(format!("stroke {} has a non-finite tilt/twist", s.id));
                }
            }
        }
        Element::Text(t) => {
            for (v, n) in [(t.x, "x"), (t.y, "y")] {
                finite(v, n)?;
            }
            if !(t.width.is_finite() && t.width >= 1.0 && t.width <= limits::MAX_PAGE_SIDE_PT) {
                return err(format!("text {} width out of range", t.id));
            }
            if !(t.height.is_finite() && t.height >= 0.0 && t.height <= limits::MAX_COORD_PT) {
                return err(format!("text {} height out of range", t.id));
            }
            if !(t.font_size.is_finite() && (2.0..=400.0).contains(&t.font_size)) {
                return err(format!("text {} font size out of range", t.id));
            }
            if t.text.len() > limits::MAX_TEXT_BYTES {
                return err(format!("text {} is too long", t.id));
            }
            if !is_color(&t.color) {
                return err(format!("text {} has an invalid colour", t.id));
            }
        }
        Element::Image(i) => {
            for (v, n) in [(i.x, "x"), (i.y, "y")] {
                finite(v, n)?;
            }
            if !(i.width.is_finite() && i.width > 0.0 && i.height.is_finite() && i.height > 0.0) {
                return err(format!("image {} has an invalid size", i.id));
            }
            if !is_sha256_hex(&i.asset_hash) {
                return err(format!("image {} has an invalid asset hash", i.id));
            }
        }
    }
    Ok(())
}

/// A whole page: schema, size, template, background and every element
/// (unique ids, bounded counts).
pub fn validate_page(p: &Page) -> Result<(), ValidationError> {
    if p.schema_version != PAGE_SCHEMA_VERSION {
        return err(format!(
            "page schema version {} (expected {PAGE_SCHEMA_VERSION})",
            p.schema_version
        ));
    }
    validate_size(&p.size)?;
    validate_template(&p.template)?;
    if p.elements.len() > limits::MAX_ELEMENTS_PER_PAGE {
        return err("page has too many elements");
    }
    let mut ids = HashSet::new();
    for e in &p.elements {
        validate_element(e)?;
        if !ids.insert(e.id()) {
            return err(format!("duplicate element id {}", e.id()));
        }
    }
    if let Some(bg) = &p.background {
        if !is_sha256_hex(&bg.asset_hash) {
            return err("PDF background has an invalid asset hash");
        }
        if ![0, 90, 180, 270].contains(&bg.rotation) {
            return err(format!(
                "PDF background rotation {} is not a multiple of 90",
                bg.rotation
            ));
        }
        for v in bg.crop_box.iter().chain(bg.media_box.iter()) {
            finite(*v, "PDF box")?;
        }
    }
    Ok(())
}

/// A notebook: schema, title, cover, page lists (no duplicates, live and
/// trashed disjoint), order keys and assets. Missing or inconsistent order
/// keys are repaired by storage on load, not rejected here.
pub fn validate_notebook(n: &Notebook) -> Result<(), ValidationError> {
    if n.schema_version != NOTEBOOK_SCHEMA_VERSION {
        return err(format!(
            "notebook schema version {} (expected {NOTEBOOK_SCHEMA_VERSION})",
            n.schema_version
        ));
    }
    validate_title(&n.title)?;
    validate_template(&n.defaults.template)?;
    if !is_color(&n.cover) {
        return err("invalid cover colour");
    }
    let mut ids = HashSet::new();
    for id in n.all_page_ids() {
        if !ids.insert(id) {
            return err(format!("page {id} is listed twice"));
        }
    }
    if ids.len() > limits::MAX_PAGES {
        return err("too many pages");
    }
    for (page, key) in &n.order_keys {
        if !ids.contains(page) {
            return err(format!("order key for unknown page {page}"));
        }
        if !crate::order_key::is_valid(key) {
            return err(format!("invalid order key for page {page}"));
        }
    }
    // a missing key or keys that disagree with pageOrder are repaired by
    // storage on load (pageOrder is authoritative locally), not rejected
    let mut hashes = HashSet::new();
    for a in &n.assets {
        if !is_sha256_hex(&a.sha256) || !hashes.insert(a.sha256.as_str()) {
            return err(format!("invalid or duplicate asset {}", a.sha256));
        }
    }
    Ok(())
}
