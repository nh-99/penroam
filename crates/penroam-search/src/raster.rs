//! Rendering a handwriting region to a small grayscale image for image-based recognizers
//! (the Penroam handwriting model, Apple Vision).
//!
//! The rendering is normalised so that a model sees the same kind of picture whatever the
//! pen, zoom or page size: the strokes' sample bounds are scaled to a fixed height, every
//! stroke is drawn with the same width, ink is white-on-black coverage (255 = ink), and
//! pressure, colour and tool are ignored. Nothing here changes the strokes.
//!
//! # Exact algorithm (the training pipeline reimplements it; keep both in step)
//!
//! Given [`RasterSpec`] `{height H, padding P, max_width W, stroke_width L}` and the region's
//! sample bounds `(x0, y0, w, h)` (raw points, no stroke width):
//!
//! 1. `s = (H − 2P) / max(h, 8)`; if `w·s + 2P > W` then `s = (W − 2P) / w`.
//! 2. Output width `ceil(w·s) + 2P`, at least `2P + 1`; height `H`.
//! 3. A sample maps to `((x − x0)·s + P, (y − y0)·s + P + ((H − 2P) − h·s) / 2)`: vertically
//!    centred when the width limit made it shorter.
//! 4. Each segment between consecutive samples (a lone sample is a zero-length segment) is
//!    drawn as a capsule of radius `r = L / 2`: a pixel whose centre `(i + 0.5, j + 0.5)` is at
//!    distance `d` from the segment gets coverage `clamp(r + 0.5 − d, 0, 1)`, and the pixel
//!    keeps the maximum coverage over all segments, stored as `round(coverage · 255)`.

use crate::segment::Region;
use penroam_core::InkStroke;

/// An 8-bit grayscale image, row-major, 255 = ink, 0 = background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height` coverage values.
    pub pixels: Vec<u8>,
}

impl GrayImage {
    /// Encodes the image as binary PGM (`P5`), handy for debugging and golden tests.
    pub fn to_pgm(&self) -> Vec<u8> {
        let mut out = format!("P5\n{} {}\n255\n", self.width, self.height).into_bytes();
        out.extend_from_slice(&self.pixels);
        out
    }
}

/// How to render (see the module docs for the exact algorithm).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RasterSpec {
    /// Output height in pixels.
    pub height: u32,
    /// Empty border on every side, in pixels.
    pub padding: u32,
    /// Largest output width; longer lines are scaled down to fit.
    pub max_width: u32,
    /// Stroke width in output pixels.
    pub stroke_width: f32,
}

impl Default for RasterSpec {
    fn default() -> Self {
        RasterSpec {
            height: 40,
            padding: 4,
            max_width: 1600,
            stroke_width: 2.0,
        }
    }
}

/// Renders the strokes of a region (see the module docs).
pub fn render_region(region: &Region<'_>, spec: &RasterSpec) -> GrayImage {
    render_strokes(&region.strokes, spec)
}

/// Renders any set of strokes as one line (see the module docs). An empty set gives a blank
/// `2P + 1` wide image.
pub fn render_strokes(strokes: &[&InkStroke], spec: &RasterSpec) -> GrayImage {
    let pts = strokes.iter().flat_map(|s| s.points.iter());
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in pts {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    let pad = spec.padding as f64;
    let inner_h = spec.height as f64 - 2.0 * pad;
    if x0 > x1 {
        let width = 2 * spec.padding + 1;
        return GrayImage {
            width,
            height: spec.height,
            pixels: vec![0; (width * spec.height) as usize],
        };
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let mut s = inner_h / h.max(8.0);
    if w * s + 2.0 * pad > spec.max_width as f64 {
        s = (spec.max_width as f64 - 2.0 * pad) / w;
    }
    let width = ((w * s).ceil() as u32 + 2 * spec.padding).max(2 * spec.padding + 1);
    let height = spec.height;
    let dy = (inner_h - h * s) / 2.0;
    let mut cov = vec![0f32; (width * height) as usize];
    let r = spec.stroke_width as f64 / 2.0;
    let map = |x: f64, y: f64| ((x - x0) * s + pad, (y - y0) * s + pad + dy);
    for st in strokes {
        let mapped: Vec<(f64, f64)> = st.points.iter().map(|p| map(p.x, p.y)).collect();
        if mapped.len() == 1 {
            capsule(&mut cov, width, height, mapped[0], mapped[0], r);
        }
        for seg in mapped.windows(2) {
            capsule(&mut cov, width, height, seg[0], seg[1], r);
        }
    }
    GrayImage {
        width,
        height,
        pixels: cov.iter().map(|c| (c * 255.0).round() as u8).collect(),
    }
}

fn capsule(cov: &mut [f32], width: u32, height: u32, a: (f64, f64), b: (f64, f64), r: f64) {
    let reach = r + 0.5;
    let lo_x = (a.0.min(b.0) - reach).floor().max(0.0) as i64;
    let hi_x = ((a.0.max(b.0) + reach).ceil() as i64).min(width as i64 - 1);
    let lo_y = (a.1.min(b.1) - reach).floor().max(0.0) as i64;
    let hi_y = ((a.1.max(b.1) + reach).ceil() as i64).min(height as i64 - 1);
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let len2 = vx * vx + vy * vy;
    for j in lo_y..=hi_y {
        for i in lo_x..=hi_x {
            let (px, py) = (i as f64 + 0.5, j as f64 + 0.5);
            let t = if len2 > 0.0 {
                (((px - a.0) * vx + (py - a.1) * vy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = (px - (a.0 + t * vx)).hypot(py - (a.1 + t * vy));
            let c = (r + 0.5 - d).clamp(0.0, 1.0) as f32;
            let k = (j as u32 * width + i as u32) as usize;
            if c > cov[k] {
                cov[k] = c;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penroam_core::{InkPoint, InkTool};
    use uuid::Uuid;

    fn stroke(pts: &[(f64, f64)]) -> InkStroke {
        InkStroke {
            id: Uuid::nil(),
            z: 0,
            tool: InkTool::Pen,
            color: "#000".into(),
            opacity: 1.0,
            width: 3.0,
            pressure: false,
            source: "test".into(),
            input: Default::default(),
            points: pts
                .iter()
                .map(|&(x, y)| InkPoint {
                    x,
                    y,
                    pressure: 0.5,
                    tilt_x: 0.0,
                    tilt_y: 0.0,
                    time_us: 0,
                    twist: None,
                    flags: None,
                })
                .collect(),
            extra: Default::default(),
        }
    }

    #[test]
    fn scales_to_height_and_draws_ink() {
        // a 64 × 16 pt "box" outline
        let s = stroke(&[
            (100.0, 50.0),
            (164.0, 50.0),
            (164.0, 66.0),
            (100.0, 66.0),
            (100.0, 50.0),
        ]);
        let img = render_strokes(&[&s], &RasterSpec::default());
        assert_eq!(img.height, 40);
        // h = 16 → s = 32 / 16 = 2 → width 128 + 8
        assert_eq!(img.width, 136);
        let at = |x: u32, y: u32| img.pixels[(y * img.width + x) as usize];
        assert_eq!(at(4, 20), 255, "left edge on the stroke"); // x = 4.5 centre, d = 0.5 < r+0.5? (r=1)
        assert_eq!(at(60, 20), 0, "inside the box is empty");
        assert_eq!(at(60, 4), 255, "top edge");
        assert_eq!(at(0, 0), 0);
    }

    #[test]
    fn long_lines_are_limited_and_centred() {
        let s = stroke(&[(0.0, 0.0), (5000.0, 10.0)]);
        let spec = RasterSpec::default();
        let img = render_strokes(&[&s], &spec);
        assert!(img.width <= spec.max_width);
        // short after scaling: the ink sits in the middle rows
        let row_has_ink =
            |y: u32| (0..img.width).any(|x| img.pixels[(y * img.width + x) as usize] > 0);
        assert!(row_has_ink(20));
        assert!(!row_has_ink(2) && !row_has_ink(37));
        let empty = render_strokes(&[], &spec);
        assert_eq!(
            (empty.width, empty.pixels.iter().all(|p| *p == 0)),
            (9, true)
        );
        assert!(img.to_pgm().starts_with(b"P5\n"));
    }
}
