//! Penroam durable local storage: the live library (device operation
//! journal + binary/JSON snapshots as checkpoints) and the portable
//! `.penroam` archive.

pub mod archive;
pub mod error;
pub mod fsutil;
pub mod journal;
mod legacy;
pub mod library;
pub mod state;

pub use archive::{ImportReport, Manifest};
pub use error::{ErrorCode, Result, StorageError};
pub use library::{
    sha256_hex, ApplyOutcome, Library, NotebookSummary, OutboxStatus, RecoveryReport,
};
