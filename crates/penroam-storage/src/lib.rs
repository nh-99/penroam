//! Penroam durable local storage: the live library (canonical JSON files plus
//! fsynced per-page operation journals) and the portable `.penroam` archive.

pub mod archive;
pub mod error;
pub mod fsutil;
pub mod library;

pub use archive::{ImportReport, Manifest};
pub use error::{ErrorCode, Result, StorageError};
pub use library::{sha256_hex, ApplyOutcome, Library, NotebookSummary, RecoveryReport};
