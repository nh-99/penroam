//! Canonical Penroam document model (schema version 1).
//!
//! All geometry is in **document points** (1 pt = 1/72 in), origin at the
//! top-left corner of the page as displayed, y pointing down. At zoom 1.0 one
//! point renders as one CSS pixel, which keeps M00's canonical stroke semantics
//! ("document units = logical px at zoom 1") unchanged: M00 strokes map 1:1.
//!
//! The JSON encoding is camelCase and documented in `docs/format/penroam-format.md`.
//! Unknown fields are preserved through `extra` maps where the type allows it,
//! so files written by a newer minor revision survive a round trip.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;
use uuid::Uuid;

/// Current document schema version. Bump only together with a migration in
/// [`crate::migrate`].
pub const SCHEMA_VERSION: u32 = 1;

pub type Extra = Map<String, Value>;

fn is_empty_map(m: &Extra) -> bool {
    m.is_empty()
}
fn default_true() -> bool {
    true
}
fn default_opacity() -> f64 {
    1.0
}

/// Milliseconds since the Unix epoch (UTC).
pub type Timestamp = i64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Notebook {
    pub schema_version: u32,
    pub id: Uuid,
    pub title: String,
    #[ts(type = "number")]
    pub created_at: Timestamp,
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    /// Live pages in reading order.
    pub page_order: Vec<Uuid>,
    /// Soft-deleted pages (restorable). Their files stay on disk.
    #[serde(default)]
    pub trashed_pages: Vec<TrashedPage>,
    /// Set when the notebook is in the trash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub deleted_at: Option<Timestamp>,
    pub defaults: PageDefaults,
    /// Cover colour (`#rrggbb`).
    pub cover: String,
    /// Immutable content-addressed attachments (original PDFs, images).
    #[serde(default)]
    pub assets: Vec<Asset>,
    #[serde(default, skip_serializing_if = "is_empty_map")]
    #[ts(type = "Record<string, unknown>", optional)]
    pub metadata: Extra,
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TrashedPage {
    pub id: Uuid,
    #[ts(type = "number")]
    pub deleted_at: Timestamp,
    /// Index in `pageOrder` at deletion time (restore puts the page back there).
    pub former_index: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PageDefaults {
    pub paper: Paper,
    pub template: Template,
}

impl Default for PageDefaults {
    fn default() -> Self {
        Self {
            paper: Paper::Letter,
            template: Template::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum Paper {
    Letter,
    A4,
    /// Size given explicitly (benchmarks, imported M00 scenes).
    Custom,
    /// Size taken from a PDF page.
    Pdf,
}

impl Paper {
    /// Portrait size in points.
    pub fn size(self) -> Option<Size> {
        match self {
            Paper::Letter => Some(Size {
                width: 612.0,
                height: 792.0,
            }),
            Paper::A4 => Some(Size {
                width: 595.276,
                height: 841.89,
            }),
            Paper::Custom | Paper::Pdf => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TemplateKind {
    Blank,
    Ruled,
    Grid,
    Dotted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Template {
    pub kind: TemplateKind,
    /// Line / grid / dot pitch in points.
    pub spacing: f64,
}

impl Default for Template {
    fn default() -> Self {
        Self {
            kind: TemplateKind::Ruled,
            spacing: 24.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Page {
    pub schema_version: u32,
    pub id: Uuid,
    pub paper: Paper,
    /// Displayed page size in points (for PDF pages: the rotated crop box).
    pub size: Size,
    pub template: Template,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background: Option<PdfBackgroundRef>,
    /// Incremented once per applied operation batch.
    #[ts(type = "number")]
    pub revision: u64,
    #[ts(type = "number")]
    pub created_at: Timestamp,
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    pub elements: Vec<Element>,
    #[serde(default, skip_serializing_if = "is_empty_map")]
    #[ts(type = "Record<string, unknown>", optional)]
    pub metadata: Extra,
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

/// A page of an immutable PDF asset rendered under the page's elements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct PdfBackgroundRef {
    /// SHA-256 (lowercase hex) of the original PDF bytes.
    pub asset_hash: String,
    /// Zero-based page index in the source PDF.
    pub page_index: u32,
    /// PDF user-space boxes `[x0, y0, x1, y1]` (unrotated, y up).
    pub media_box: [f64; 4],
    pub crop_box: [f64; 4],
    /// Effective `/Rotate` (0, 90, 180, 270), clockwise.
    pub rotation: u32,
}

impl PdfBackgroundRef {
    /// Affine matrix `[a b c d e f]` mapping displayed page points (y down,
    /// origin top-left of the crop box) to PDF user space. Used for exporting
    /// annotations; the PDF.js viewport applies the inverse when rendering.
    pub fn doc_to_pdf_matrix(&self) -> [f64; 6] {
        let [x0, y0, x1, y1] = self.crop_box;
        let (cx0, cy0, cx1, cy1) = (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1));
        match self.rotation % 360 {
            90 => [0.0, 1.0, 1.0, 0.0, cx0, cy0],
            180 => [-1.0, 0.0, 0.0, 1.0, cx1, cy0],
            270 => [0.0, -1.0, -1.0, 0.0, cx1, cy1],
            _ => [1.0, 0.0, 0.0, -1.0, cx0, cy1],
        }
    }

    /// Displayed size (crop box, rotated).
    pub fn display_size(&self) -> Size {
        let [x0, y0, x1, y1] = self.crop_box;
        let (w, h) = ((x1 - x0).abs(), (y1 - y0).abs());
        if self.rotation % 180 == 90 {
            Size {
                width: h,
                height: w,
            }
        } else {
            Size {
                width: w,
                height: h,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Asset {
    pub sha256: String,
    pub media_type: String,
    #[ts(type = "number")]
    pub byte_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub original_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
pub enum Element {
    Ink(InkStroke),
    Text(TextBlock),
    Image(ImageElement),
}

impl Element {
    pub fn id(&self) -> Uuid {
        match self {
            Element::Ink(s) => s.id,
            Element::Text(t) => t.id,
            Element::Image(i) => i.id,
        }
    }
    pub fn z(&self) -> i64 {
        match self {
            Element::Ink(s) => s.z,
            Element::Text(t) => t.z,
            Element::Image(i) => i.z,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum InkTool {
    Pen,
    Highlighter,
}

/// One handwritten stroke. `points` are the original input samples (after the
/// CSS px → document pt mapping); smoothing is derived at render time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InkStroke {
    pub id: Uuid,
    #[ts(type = "number")]
    pub z: i64,
    pub tool: InkTool,
    /// `#rrggbb`.
    pub color: String,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// Nominal width in points (M00 `brush.width`).
    pub width: f64,
    /// Whether pressure modulated the width when drawn (M00 `brush.pressure`).
    #[serde(default = "default_true")]
    pub pressure: bool,
    /// "stylus" | "mouse" | "touch" | "fixture" | ...
    pub source: String,
    /// Input diagnostics and platform metadata (M00 `input`), kept verbatim:
    /// `api`, `device`, raw pressure range, coalesced counts, Onyx stream, ...
    #[serde(default)]
    #[ts(type = "Record<string, unknown>")]
    pub input: Extra,
    /// Omitted only inside the binary page encoding's metadata section,
    /// where points live in the column block (see `crate::binary`).
    #[serde(default)]
    pub points: Vec<InkPoint>,
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

/// One pen sample. Same fields and units as M00's canonical point plus the
/// optional `twist` and `flags`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InkPoint {
    pub x: f64,
    pub y: f64,
    /// Normalised 0..1 (mouse: 0.5). Raw ranges live in the stroke `input`.
    pub pressure: f64,
    /// Degrees, W3C convention.
    #[serde(default)]
    pub tilt_x: f64,
    #[serde(default)]
    pub tilt_y: f64,
    /// Microseconds since the first sample of the stroke.
    #[serde(default)]
    #[ts(type = "number")]
    pub time_us: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub twist: Option<f64>,
    /// Bit flags, see [`point_flags`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub flags: Option<u32>,
}

pub mod point_flags {
    /// Pressure was an estimate later refined by the platform (iOS estimated properties).
    pub const ESTIMATED_PRESSURE: u32 = 1;
    /// The sample came from a coalesced event list.
    pub const COALESCED: u32 = 2;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// Plain multiline text placed on the page. Layout is deterministic: see
/// [`crate::text`]. `height` is kept in sync with the laid-out content by the
/// editor; renderers grow it if needed and never clip text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TextBlock {
    pub id: Uuid,
    #[ts(type = "number")]
    pub z: i64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub text: String,
    pub font_size: f64,
    pub color: String,
    #[serde(default = "default_align")]
    pub align: TextAlign,
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

fn default_align() -> TextAlign {
    TextAlign::Left
}

/// Minimal raster/vector image placed on a page (asset reference).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ImageElement {
    pub id: Uuid,
    #[ts(type = "number")]
    pub z: i64,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub asset_hash: String,
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

impl Page {
    pub fn new(id: Uuid, paper: Paper, size: Size, template: Template, now: Timestamp) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id,
            paper,
            size,
            template,
            background: None,
            revision: 0,
            created_at: now,
            updated_at: now,
            elements: Vec::new(),
            metadata: Extra::new(),
            extra: Extra::new(),
        }
    }

    /// Everything except the elements (used by the binary encoder).
    pub fn clone_without_elements(&self) -> Page {
        Page {
            schema_version: self.schema_version,
            id: self.id,
            paper: self.paper,
            size: self.size,
            template: self.template.clone(),
            background: self.background.clone(),
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
            elements: Vec::new(),
            metadata: self.metadata.clone(),
            extra: self.extra.clone(),
        }
    }

    pub fn max_z(&self) -> i64 {
        self.elements.iter().map(Element::z).max().unwrap_or(0)
    }

    pub fn asset_refs(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .elements
            .iter()
            .filter_map(|e| match e {
                Element::Image(i) => Some(i.asset_hash.as_str()),
                _ => None,
            })
            .collect();
        if let Some(b) = &self.background {
            v.push(b.asset_hash.as_str());
        }
        v
    }
}

impl Notebook {
    pub fn new(
        id: Uuid,
        title: String,
        defaults: PageDefaults,
        cover: String,
        now: Timestamp,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id,
            title,
            created_at: now,
            updated_at: now,
            page_order: Vec::new(),
            trashed_pages: Vec::new(),
            deleted_at: None,
            defaults,
            cover,
            assets: Vec::new(),
            metadata: Extra::new(),
            extra: Extra::new(),
        }
    }

    /// All pages with files on disk (live and trashed).
    pub fn all_page_ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.page_order
            .iter()
            .copied()
            .chain(self.trashed_pages.iter().map(|t| t.id))
    }
}
