//! Rust port of the shared M00 brush geometry (reference implementation:
//! `tests/fixtures/generate-fixtures.mjs`, golden: `geometry-golden.json`).
//! Used by the annotated-PDF exporter so exported ink matches on-screen ink.

use crate::model::InkPoint;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Piece {
    Dot { x: f64, y: f64, width: f64 },
    Line { x0: f64, y0: f64, x1: f64, y1: f64, width: f64 },
    Quad { x0: f64, y0: f64, cx: f64, cy: f64, x1: f64, y1: f64, width: f64 },
}

impl Piece {
    pub fn width(&self) -> f64 {
        match *self {
            Piece::Dot { width, .. } | Piece::Line { width, .. } | Piece::Quad { width, .. } => width,
        }
    }
}

/// `pressureEnabled ? max(0.5, base * (0.4 + 1.2 * p)) : base`
pub fn width_for(base: f64, pressure: f64, pressure_enabled: bool) -> f64 {
    if !pressure_enabled {
        return base;
    }
    let p = pressure.clamp(0.0, 1.0);
    (base * (0.4 + 1.2 * p)).max(0.5)
}

pub fn piece_count(n: usize) -> usize {
    match n {
        0 => 0,
        1 | 2 => 1,
        _ => n,
    }
}

pub fn piece(pts: &[InkPoint], k: usize, base: f64, pe: bool) -> Piece {
    let n = pts.len();
    let w = |p: &InkPoint| width_for(base, p.pressure, pe);
    if n == 1 {
        return Piece::Dot { x: pts[0].x, y: pts[0].y, width: w(&pts[0]) };
    }
    if n == 2 {
        return Piece::Line { x0: pts[0].x, y0: pts[0].y, x1: pts[1].x, y1: pts[1].y, width: w(&pts[0]) };
    }
    if k == 0 {
        let (a, b) = (&pts[0], &pts[1]);
        return Piece::Line { x0: a.x, y0: a.y, x1: (a.x + b.x) / 2.0, y1: (a.y + b.y) / 2.0, width: w(a) };
    }
    if k == n - 1 {
        let (a, b) = (&pts[n - 2], &pts[n - 1]);
        return Piece::Line { x0: (a.x + b.x) / 2.0, y0: (a.y + b.y) / 2.0, x1: b.x, y1: b.y, width: w(b) };
    }
    let (p0, p1, p2) = (&pts[k - 1], &pts[k], &pts[k + 1]);
    Piece::Quad {
        x0: (p0.x + p1.x) / 2.0,
        y0: (p0.y + p1.y) / 2.0,
        cx: p1.x,
        cy: p1.y,
        x1: (p1.x + p2.x) / 2.0,
        y1: (p1.y + p2.y) / 2.0,
        width: w(p1),
    }
}

pub fn stroke_pieces(pts: &[InkPoint], base: f64, pe: bool) -> Vec<Piece> {
    (0..piece_count(pts.len())).map(|k| piece(pts, k, base, pe)).collect()
}

/// Bounds `[minX, minY, maxX, maxY]` including half the maximum width + 1.
pub fn stroke_bounds(pts: &[InkPoint], base: f64, pe: bool) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut max_w: f64 = 0.0;
    for p in pts {
        b[0] = b[0].min(p.x);
        b[1] = b[1].min(p.y);
        b[2] = b[2].max(p.x);
        b[3] = b[3].max(p.y);
        max_w = max_w.max(width_for(base, p.pressure, pe));
    }
    let pad = max_w / 2.0 + 1.0;
    [b[0] - pad, b[1] - pad, b[2] + pad, b[3] + pad]
}
