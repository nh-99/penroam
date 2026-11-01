//! Penroam core: the canonical, versioned notebook document model and the pure
//! logic around it (validation, semantic operations, brush geometry, text
//! layout, migrations). No I/O lives here; see `penroam-storage`.

pub mod geometry;
pub mod migrate;
pub mod model;
pub mod ops;
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
