//! Canonical Penroam document model (notebook schema 2, page schema 1).
//!
//! All geometry is in **document points** (1 pt = 1/72 in), origin at the
//! top-left corner of the page as displayed, y pointing down. At zoom 1.0 one
//! point renders as one CSS pixel, which keeps the canonical stroke document semantics
//! ("document units = logical px at zoom 1") unchanged: its strokes map 1:1.
//!
//! The JSON encoding is camelCase and documented in `docs/formats/model-and-archive.md`.
//! Unknown fields are preserved through `extra` maps where the type allows it,
//! so files written by a newer minor revision survive a round trip.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;
use uuid::Uuid;

/// Current notebook (`notebook.json`) schema version. Bump only together
/// with a migration in [`crate::migrate`]. Independent of the page schema,
/// the binary file format version and the operation schema version.
///
/// * 1 — original schema (per-page snapshots + per-page JSON-lines journals).
/// * 2 — adds `orderKeys` (fractional page order keys). Also marks a
///   notebook as managed by the operation journal, so builds that only know
///   schema 1 refuse it instead of reading pages while ignoring the journal.
pub const NOTEBOOK_SCHEMA_VERSION: u32 = 2;

/// Current page schema version (the JSON page model, also the `.prp`
/// metadata section). Independent of [`crate::binary::FILE_FORMAT_VERSION`].
pub const PAGE_SCHEMA_VERSION: u32 = 1;

/// Free-form JSON object used to carry fields this build does not know, so
/// they survive a load/save round trip.
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
/// A notebook: its metadata, page list (live order, trash, order keys) and
/// assets. Page content lives in separate [`Page`] documents.
pub struct Notebook {
    /// [`NOTEBOOK_SCHEMA_VERSION`] when written by this build.
    pub schema_version: u32,
    /// Stable random id (never derived from the title).
    pub id: Uuid,
    /// User-visible title.
    pub title: String,
    /// Creation time (informational, never used for ordering).
    #[ts(type = "number")]
    pub created_at: Timestamp,
    /// Last change (informational).
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    /// Live pages in reading order. Always sorted by (`order_keys`, id).
    pub page_order: Vec<Uuid>,
    /// Fractional order key of every page, live and trashed (schema 2).
    /// Keys make concurrent inserts/moves converge (see [`crate::order_key`]);
    /// a trashed page keeps its key so restore puts it back between the
    /// same neighbours.
    #[serde(default)]
    #[ts(type = "Record<string, string>")]
    pub order_keys: std::collections::BTreeMap<Uuid, String>,
    /// Soft-deleted pages (restorable). Their files stay on disk.
    #[serde(default)]
    pub trashed_pages: Vec<TrashedPage>,
    /// Set when the notebook is in the trash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub deleted_at: Option<Timestamp>,
    /// Paper and template used for new pages.
    pub defaults: PageDefaults,
    /// Cover colour (`#rrggbb`).
    pub cover: String,
    /// Immutable content-addressed attachments (original PDFs, images).
    #[serde(default)]
    pub assets: Vec<Asset>,
    /// Application metadata (free-form, preserved).
    #[serde(default, skip_serializing_if = "is_empty_map")]
    #[ts(type = "Record<string, unknown>", optional)]
    pub metadata: Extra,
    /// Unknown top-level fields, preserved verbatim.
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

/// A page in the notebook's trash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TrashedPage {
    /// The page id.
    pub id: Uuid,
    /// When it was trashed (informational).
    #[ts(type = "number")]
    pub deleted_at: Timestamp,
    /// Index in `pageOrder` at deletion time (restore puts the page back there).
    pub former_index: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
/// Defaults for pages added to a notebook.
pub struct PageDefaults {
    /// Paper size of new pages.
    pub paper: Paper,
    /// Background template of new pages.
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
/// Page size class.
pub enum Paper {
    /// US Letter, 612 × 792 pt.
    Letter,
    /// ISO A4, 595.276 × 841.89 pt.
    A4,
    /// Size given explicitly (benchmarks, imported stroke-document scenes).
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
/// Width and height in document points.
pub struct Size {
    /// Width in points.
    pub width: f64,
    /// Height in points.
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
/// Background pattern drawn under a page's elements.
pub enum TemplateKind {
    /// Nothing.
    Blank,
    /// Horizontal lines with a left margin line.
    Ruled,
    /// Square grid.
    Grid,
    /// Dot grid.
    Dotted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
/// A page background pattern and its pitch.
pub struct Template {
    /// Which pattern.
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
/// One page: size, background and elements (ink, text, images) in z order.
/// Stored as a binary `.prp` file (see [`crate::binary`]).
pub struct Page {
    /// [`PAGE_SCHEMA_VERSION`] when written by this build.
    pub schema_version: u32,
    /// Stable random id.
    pub id: Uuid,
    /// Size class (the actual size is [`Page::size`]).
    pub paper: Paper,
    /// Displayed page size in points (for PDF pages: the rotated crop box).
    pub size: Size,
    /// Background pattern.
    pub template: Template,
    /// PDF page rendered under the elements, for imported PDFs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background: Option<PdfBackgroundRef>,
    /// Incremented once per applied operation batch.
    #[ts(type = "number")]
    pub revision: u64,
    /// Creation time (informational).
    #[ts(type = "number")]
    pub created_at: Timestamp,
    /// Last change (informational).
    #[ts(type = "number")]
    pub updated_at: Timestamp,
    /// Ink strokes, text blocks and images. Drawn in order of (`z`, id).
    pub elements: Vec<Element>,
    /// Application metadata (free-form, preserved).
    #[serde(default, skip_serializing_if = "is_empty_map")]
    #[ts(type = "Record<string, unknown>", optional)]
    pub metadata: Extra,
    /// Unknown top-level fields, preserved verbatim.
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
    /// Extension records of the binary page file that this build does not
    /// understand, kept verbatim so a re-save never strips them (see
    /// `docs/formats/penroam-binary-v1.md` §5). Not part of the JSON model.
    #[serde(skip)]
    #[ts(skip)]
    pub binary_extensions: Vec<BinaryExtension>,
}

/// An opaque extension record from a `.prp` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryExtension {
    /// Record tag; the top bits are [`Self::REQUIRED`] and [`Self::COPY_SAFE`].
    pub tag: u32,
    /// Raw record payload.
    pub data: Vec<u8>,
}

impl BinaryExtension {
    /// Readers that do not know the tag must refuse the file.
    pub const REQUIRED: u32 = 1 << 31;
    /// May be kept verbatim after the page is edited. Unknown records
    /// without this bit make the page read-only for this build.
    pub const COPY_SAFE: u32 = 1 << 30;

    /// The record has the [`Self::REQUIRED`] bit.
    pub fn required(&self) -> bool {
        self.tag & Self::REQUIRED != 0
    }
    /// The record has the [`Self::COPY_SAFE`] bit.
    pub fn copy_safe(&self) -> bool {
        self.tag & Self::COPY_SAFE != 0
    }
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
    /// Visible region in PDF user space (same convention as `media_box`).
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
/// An immutable attachment stored once per notebook under its hash.
pub struct Asset {
    /// SHA-256 (lowercase hex) of the bytes; also the file name on disk.
    pub sha256: String,
    /// MIME type, e.g. `application/pdf`.
    pub media_type: String,
    /// Size in bytes.
    #[ts(type = "number")]
    pub byte_size: u64,
    /// File name the user imported, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub original_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(export)]
/// Something drawn on a page. JSON-tagged by `type`.
pub enum Element {
    /// A handwritten stroke.
    Ink(InkStroke),
    /// A typed text block.
    Text(TextBlock),
    /// An image referencing an asset.
    Image(ImageElement),
}

impl Element {
    /// The element's stable id.
    pub fn id(&self) -> Uuid {
        match self {
            Element::Ink(s) => s.id,
            Element::Text(t) => t.id,
            Element::Image(i) => i.id,
        }
    }
    /// Stacking position (higher draws on top; ties broken by id).
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
/// How a stroke is rendered.
pub enum InkTool {
    /// Opaque ink whose width may follow pressure.
    Pen,
    /// Translucent, fixed-width marker.
    Highlighter,
}

/// One handwritten stroke. `points` are the original input samples (after the
/// CSS px → document pt mapping); smoothing is derived at render time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InkStroke {
    /// Stable id.
    pub id: Uuid,
    /// Stacking position.
    #[ts(type = "number")]
    pub z: i64,
    /// Pen or highlighter.
    pub tool: InkTool,
    /// `#rrggbb`.
    pub color: String,
    /// 0..1; highlighters are translucent.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    /// Nominal width in points (stroke document `brush.width`).
    pub width: f64,
    /// Whether pressure modulated the width when drawn (stroke document `brush.pressure`).
    #[serde(default = "default_true")]
    pub pressure: bool,
    /// "stylus" | "mouse" | "touch" | "fixture" | ...
    pub source: String,
    /// Input diagnostics and platform metadata (stroke document `input`), kept verbatim:
    /// `api`, `device`, raw pressure range, coalesced counts, Onyx stream, ...
    #[serde(default)]
    #[ts(type = "Record<string, unknown>")]
    pub input: Extra,
    /// Omitted only inside the binary page encoding's metadata section,
    /// where points live in the column block (see `crate::binary`).
    #[serde(default)]
    pub points: Vec<InkPoint>,
    /// Unknown fields, preserved verbatim.
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

/// One pen sample. Same fields and units as the stroke document's canonical point plus the
/// optional `twist` and `flags`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct InkPoint {
    /// Document x in points.
    pub x: f64,
    /// Document y in points (down).
    pub y: f64,
    /// Normalised 0..1 (mouse: 0.5). Raw ranges live in the stroke `input`.
    pub pressure: f64,
    /// Degrees, W3C convention.
    #[serde(default)]
    pub tilt_x: f64,
    /// Degrees, W3C convention.
    #[serde(default)]
    pub tilt_y: f64,
    /// Microseconds since the first sample of the stroke.
    #[serde(default)]
    #[ts(type = "number")]
    pub time_us: i64,
    /// Barrel rotation in degrees, when the platform reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub twist: Option<f64>,
    /// Bit flags, see [`point_flags`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub flags: Option<u32>,
}

/// Bits of [`InkPoint::flags`].
pub mod point_flags {
    /// Pressure was an estimate later refined by the platform (iOS estimated properties).
    pub const ESTIMATED_PRESSURE: u32 = 1;
    /// The sample came from a coalesced event list.
    pub const COALESCED: u32 = 2;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
/// Horizontal alignment of text lines inside a [`TextBlock`].
pub enum TextAlign {
    /// Flush left.
    Left,
    /// Centered.
    Center,
    /// Flush right.
    Right,
}

/// Plain multiline text placed on the page. Layout is deterministic: see
/// [`crate::text`]. `height` is kept in sync with the laid-out content by the
/// editor; renderers grow it if needed and never clip text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TextBlock {
    /// Stable id.
    pub id: Uuid,
    /// Stacking position.
    #[ts(type = "number")]
    pub z: i64,
    /// Left edge in points.
    pub x: f64,
    /// Top edge in points.
    pub y: f64,
    /// Wrapping width in points.
    pub width: f64,
    /// Box height in points (see the type docs).
    pub height: f64,
    /// Plain text; `\n` starts a new line.
    pub text: String,
    /// Font size in points.
    pub font_size: f64,
    /// `#rrggbb`.
    pub color: String,
    /// Line alignment.
    #[serde(default = "default_align")]
    pub align: TextAlign,
    /// Unknown fields, preserved verbatim.
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
    /// Stable id.
    pub id: Uuid,
    /// Stacking position.
    #[ts(type = "number")]
    pub z: i64,
    /// Left edge in points.
    pub x: f64,
    /// Top edge in points.
    pub y: f64,
    /// Width in points.
    pub width: f64,
    /// Height in points.
    pub height: f64,
    /// SHA-256 of the image asset in the notebook.
    pub asset_hash: String,
    /// Unknown fields, preserved verbatim.
    #[serde(flatten)]
    #[ts(skip)]
    pub extra: Extra,
}

impl Page {
    /// An empty page at revision 0.
    pub fn new(id: Uuid, paper: Paper, size: Size, template: Template, now: Timestamp) -> Self {
        Self {
            schema_version: PAGE_SCHEMA_VERSION,
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
            binary_extensions: Vec::new(),
        }
    }

    /// Why this build must not modify the page, if it must not: the file
    /// carries extension data this build cannot keep consistent.
    pub fn read_only_reason(&self) -> Option<String> {
        self.binary_extensions
            .iter()
            .find(|e| !e.copy_safe())
            .map(|e| {
                format!(
                    "this page contains data from a newer Penroam version (extension {:#010x}); it is read-only here",
                    e.tag
                )
            })
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
            binary_extensions: self.binary_extensions.clone(),
        }
    }

    /// Highest `z` of any element (0 for an empty page).
    pub fn max_z(&self) -> i64 {
        self.elements.iter().map(Element::z).max().unwrap_or(0)
    }

    /// Hashes of every asset the page uses (images and PDF background).
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
    /// An empty notebook (no pages) at the current schema.
    pub fn new(
        id: Uuid,
        title: String,
        defaults: PageDefaults,
        cover: String,
        now: Timestamp,
    ) -> Self {
        Self {
            schema_version: NOTEBOOK_SCHEMA_VERSION,
            id,
            title,
            created_at: now,
            updated_at: now,
            page_order: Vec::new(),
            order_keys: Default::default(),
            trashed_pages: Vec::new(),
            deleted_at: None,
            defaults,
            cover,
            assets: Vec::new(),
            metadata: Extra::new(),
            extra: Extra::new(),
        }
    }

    /// Order key of a page (live or trashed).
    pub fn key_of(&self, page: Uuid) -> Option<&str> {
        self.order_keys.get(&page).map(String::as_str)
    }

    /// Sorts `page_order` by (order key, page id); pages without a key go last.
    pub fn sort_page_order(&mut self) {
        let keys = &self.order_keys;
        self.page_order
            .sort_by(|a, b| match (keys.get(a), keys.get(b)) {
                (Some(x), Some(y)) => x.cmp(y).then(a.cmp(b)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
    }

    /// Whether `page_order` is consistent with `order_keys`.
    pub fn order_is_consistent(&self) -> bool {
        self.all_page_ids().all(|p| {
            self.order_keys
                .get(&p)
                .is_some_and(|k| crate::order_key::is_valid(k))
        }) && self.page_order.windows(2).all(|w| {
            let (ka, kb) = (&self.order_keys[&w[0]], &self.order_keys[&w[1]]);
            ka < kb || (ka == kb && w[0] < w[1])
        })
    }

    /// A fresh key that sorts at live index `at` (0 = first, len = last),
    /// strictly between the neighbours' keys.
    pub fn key_for_index(&self, at: usize) -> String {
        let at = at.min(self.page_order.len());
        let before = at
            .checked_sub(1)
            .and_then(|i| self.key_of(self.page_order[i]));
        let after = self.page_order.get(at).and_then(|p| self.key_of(*p));
        match (before, after) {
            // equal neighbour keys (concurrent inserts): go after both
            (Some(a), Some(b)) if a >= b => crate::order_key::between(Some(a), None),
            (a, b) => crate::order_key::between(a, b),
        }
    }

    /// Replaces every order key with evenly spaced keys that reproduce the
    /// current `page_order`; trashed pages get keys at their former index.
    /// Used by the v1 → v2 migration and to repair inconsistent files.
    pub fn rebuild_order_keys(&mut self) {
        let mut seq: Vec<Uuid> = self.page_order.clone();
        for t in &self.trashed_pages {
            let at = (t.former_index as usize).min(seq.len());
            seq.insert(at, t.id);
        }
        self.order_keys = seq
            .into_iter()
            .zip(crate::order_key::spread(
                self.page_order.len() + self.trashed_pages.len(),
            ))
            .collect();
    }

    /// All pages with files on disk (live and trashed).
    pub fn all_page_ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.page_order
            .iter()
            .copied()
            .chain(self.trashed_pages.iter().map(|t| t.id))
    }
}
