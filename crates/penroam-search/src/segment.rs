//! Handwriting regions: groups of pen strokes that probably form one line of writing.
//!
//! Recognition runs per region, never per page, so that an edit only re-recognizes the lines
//! it touched (see [`Region::fingerprint`]). The grouping uses only vector geometry; nothing
//! is rasterised here and the strokes are never modified.
//!
//! # How strokes are grouped
//!
//! 1. Only pen strokes count; highlighter strokes mark existing writing and are skipped.
//! 2. The page's line height `h` is estimated as the median height of the strokes, clamped
//!    to 6–72 pt (18 pt when there are none).
//! 3. Strokes much larger than a line in both directions (both sides over 4 `h`) are treated
//!    as drawings and left out.
//! 4. Two strokes join the same region when they overlap vertically by at least 40 % of the
//!    smaller one (small marks count as at least 0.3 `h` tall) and the horizontal gap between
//!    them is at most 3 `h`, or when one of them is a small mark (an i-dot, a comma, an
//!    accent) touching the other within 0.3 `h` horizontally and 0.6 `h` vertically.
//!    Grouping is transitive (union–find).
//! 5. Strokes in a region are ordered by their first sample's timestamp (writing order, which
//!    stroke recognizers want), then by z.
//!
//! The thresholds are heuristics measured with `examples/eval.rs`. Because `h` comes from the
//! median stroke, which in print is often a single short letter part, word gaps of 2–2.5 `h`
//! are common; at a 2 `h` limit about one line in seven was split in two, at 3 `h` almost none.
//!
//! # Identity
//!
//! [`Region::key`] depends only on the set of stroke ids, so it survives a move; user
//! corrections are attached to it. [`Region::fingerprint`] also covers the geometry, so a
//! cached recognition is reused only while the strokes are unchanged.

use penroam_core::geometry::stroke_bounds;
use penroam_core::{Element, InkStroke, InkTool, Page};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// An axis-aligned rectangle in page points (origin top-left, y down), the page model's
/// coordinate system.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width (≥ 0).
    pub w: f64,
    /// Height (≥ 0).
    pub h: f64,
}

impl Rect {
    /// The rectangle spanning `[x0, y0]`–`[x1, y1]`.
    pub fn from_corners(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect {
            x: x0.min(x1),
            y: y0.min(y1),
            w: (x1 - x0).abs(),
            h: (y1 - y0).abs(),
        }
    }

    /// Right edge.
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    /// Bottom edge.
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, o: &Rect) -> Rect {
        Rect::from_corners(
            self.x.min(o.x),
            self.y.min(o.y),
            self.right().max(o.right()),
            self.bottom().max(o.bottom()),
        )
    }
}

/// One group of strokes that is recognized as a unit (normally one line of writing).
#[derive(Debug, Clone)]
pub struct Region<'a> {
    /// Stable identity: a hash of the sorted stroke ids (independent of position).
    pub key: String,
    /// Changes whenever any stroke in the region changes (ids, points, width).
    pub fingerprint: String,
    /// The strokes, in writing order.
    pub strokes: Vec<&'a InkStroke>,
    /// Bounds of the drawn ink (including stroke width), for highlighting.
    pub bounds: Rect,
}

impl Region<'_> {
    /// Ids of the strokes in this region, in writing order.
    pub fn stroke_ids(&self) -> Vec<Uuid> {
        self.strokes.iter().map(|s| s.id).collect()
    }
}

/// Grouping thresholds, in multiples of the estimated line height.
#[derive(Debug, Clone, Copy)]
pub struct SegmentParams {
    /// Minimum vertical overlap, relative to the smaller stroke, for two strokes to share a line.
    pub min_overlap: f64,
    /// Largest horizontal gap between strokes of one line.
    pub max_gap: f64,
    /// Strokes larger than this in both directions are drawings, not writing.
    pub drawing_size: f64,
}

impl Default for SegmentParams {
    fn default() -> Self {
        SegmentParams {
            min_overlap: 0.4,
            max_gap: 3.0,
            drawing_size: 4.0,
        }
    }
}

struct Item<'a> {
    stroke: &'a InkStroke,
    /// Bounds of the raw samples (no width), used for grouping.
    b: Rect,
}

fn sample_bounds(s: &InkStroke) -> Option<Rect> {
    let first = s.points.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for p in &s.points {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    [x0, y0, x1, y1]
        .iter()
        .all(|v| v.is_finite())
        .then(|| Rect::from_corners(x0, y0, x1, y1))
}

/// The page's estimated line height (see the module docs).
pub fn line_height(page: &Page) -> f64 {
    let mut hs: Vec<f64> = pen_strokes(page)
        .filter_map(sample_bounds)
        .map(|b| b.h)
        .filter(|h| *h > 1.0)
        .collect();
    if hs.is_empty() {
        return 18.0;
    }
    hs.sort_by(f64::total_cmp);
    hs[hs.len() / 2].clamp(6.0, 72.0)
}

fn pen_strokes(page: &Page) -> impl Iterator<Item = &InkStroke> {
    page.elements.iter().filter_map(|e| match e {
        Element::Ink(s) if s.tool == InkTool::Pen && !s.points.is_empty() => Some(s),
        _ => None,
    })
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

fn linked(a: &Rect, b: &Rect, h: f64, p: &SegmentParams) -> bool {
    let gx = (a.x.max(b.x) - a.right().min(b.right())).max(0.0);
    let gy = (a.y.max(b.y) - a.bottom().min(b.bottom())).max(0.0);
    let small = 0.5 * h;
    if a.w.max(a.h).min(b.w.max(b.h)) < small && gx <= 0.3 * h && gy <= 0.6 * h {
        return true;
    }
    // vertical overlap of the bands, small marks counted as at least 0.3 h tall
    let band = |r: &Rect| {
        let t = r.h.max(0.3 * h);
        let c = r.y + r.h / 2.0;
        (c - t / 2.0, c + t / 2.0)
    };
    let (a0, a1) = band(a);
    let (b0, b1) = band(b);
    let ov = (a1.min(b1) - a0.max(b0)).max(0.0);
    ov >= p.min_overlap * (a1 - a0).min(b1 - b0) && gx <= p.max_gap * h
}

/// Groups the page's pen strokes into regions with the default thresholds.
pub fn regions(page: &Page) -> Vec<Region<'_>> {
    regions_with(page, &SegmentParams::default())
}

/// Groups the page's pen strokes into regions (see the module docs). Regions are returned
/// top to bottom, then left to right.
pub fn regions_with<'a>(page: &'a Page, p: &SegmentParams) -> Vec<Region<'a>> {
    let h = line_height(page);
    let mut items: Vec<Item<'a>> = pen_strokes(page)
        .filter_map(|s| sample_bounds(s).map(|b| Item { stroke: s, b }))
        .filter(|it| !(it.b.w > p.drawing_size * h && it.b.h > p.drawing_size * h))
        .collect();
    items.sort_by(|a, b| a.b.y.total_cmp(&b.b.y));
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        // sorted by top edge: later strokes starting below this one's band (plus the
        // small-mark allowance) cannot link to it
        let limit = items[i].b.bottom().max(items[i].b.y + 0.3 * h) + 0.6 * h;
        for j in i + 1..n {
            if items[j].b.y > limit {
                break;
            }
            if linked(&items[i].b, &items[j].b, h, p) {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[rj] = ri;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Region<'a>> = groups
        .into_values()
        .map(|members| {
            let mut strokes: Vec<&InkStroke> = members.iter().map(|&i| items[i].stroke).collect();
            strokes.sort_by_key(|s| (s.points[0].time_us, s.z));
            make_region(strokes)
        })
        .collect();
    out.sort_by(|a, b| {
        // same line when vertical centres are within half a line height
        let (ca, cb) = (a.bounds.y + a.bounds.h / 2.0, b.bounds.y + b.bounds.h / 2.0);
        if (ca - cb).abs() < h / 2.0 {
            a.bounds.x.total_cmp(&b.bounds.x)
        } else {
            ca.total_cmp(&cb)
        }
    });
    out
}

fn make_region(strokes: Vec<&InkStroke>) -> Region<'_> {
    let mut ids: Vec<Uuid> = strokes.iter().map(|s| s.id).collect();
    ids.sort();
    let mut k = Sha256::new();
    for id in &ids {
        k.update(id.as_bytes());
    }
    let key = hex16(&k.finalize());
    let mut f = Sha256::new();
    f.update(key.as_bytes());
    let mut bounds: Option<Rect> = None;
    for s in &strokes {
        f.update(s.id.as_bytes());
        f.update(s.width.to_le_bytes());
        f.update((s.points.len() as u64).to_le_bytes());
        for pt in &s.points {
            f.update(pt.x.to_le_bytes());
            f.update(pt.y.to_le_bytes());
        }
        let b = stroke_bounds(&s.points, s.width, s.pressure);
        let r = Rect::from_corners(b[0], b[1], b[2], b[3]);
        bounds = Some(bounds.map_or(r, |u| u.union(&r)));
    }
    Region {
        key,
        fingerprint: hex16(&f.finalize()),
        strokes,
        bounds: bounds.unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        }),
    }
}

fn hex16(d: &[u8]) -> String {
    d[..16].iter().map(|b| format!("{b:02x}")).collect()
}
