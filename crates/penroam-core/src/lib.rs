//! Penroam core: the canonical, versioned notebook document model and the pure
//! logic around it. No I/O lives here; storage is `penroam-storage`.
//!
//! * [`model`]: notebooks, pages and elements (ink, text, images) in document
//!   points; the JSON shape is documented in `docs/formats/model-and-archive.md`.
//! * [`binary`]: the compact lossless `.prp` page encoding
//!   (`docs/formats/penroam-binary-v1.md`).
//! * [`ops`]: editor batches applied atomically to one page (single writer).
//! * [`operation`]: the journaled, sync-ready operations those batches and
//!   notebook edits become, with the local (single-writer) appliers.
//! * [`replica`]: order-independent merge of operations from several devices,
//!   with conflicts kept as alternates.
//! * [`order_key`]: fractional keys that make page order converge.
//! * [`validate`], [`migrate`]: hostile-input limits and schema migrations.
//! * [`geometry`], [`text`]: brush geometry and text layout shared with the
//!   renderer and the PDF exporter.
//!
//! Architecture overview: `docs/architecture/overview.md`.

#![warn(missing_docs)]

pub mod binary;
pub mod geometry;
pub mod migrate;
pub mod model;
pub mod operation;
pub mod ops;
pub mod order_key;
pub mod replica;
pub mod text;
pub mod validate;

pub use model::*;
pub use ops::{apply_batch, ApplyError, OpKind, OperationBatch, PageOperation};

/// Current time in ms since the Unix epoch.
pub fn now_ms() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
