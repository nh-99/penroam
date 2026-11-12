//! Penroam durable local storage: the live library (device operation
//! journal + binary/JSON snapshots as checkpoints) and the portable
//! `.penroam` archive.
//!
//! * [`library`]: [`Library`], the open notebook library. Every change is
//!   journaled and fsynced before the call returns ("Saved locally");
//!   snapshots are checkpoints written later. Start-up recovery repairs torn
//!   writes, quarantines damage without deleting anything, and migrates the
//!   legacy layout.
//! * [`history`]: restorable page versions, kept as operations with sparse
//!   keyframes.
//! * [`journal`]: the append-only binary operation journal.
//! * [`state`]: small JSON state files (format marker, device id,
//!   checkpoint manifest, sync outbox counters).
//! * [`archive`]: `.penroam` export and defensive import.
//! * [`backup`]: full-library `.penroam-backup` files, restore preview and
//!   no-overwrite restore.
//! * [`fsutil`]: atomic/durable file primitives with crash-injection points
//!   for tests (feature `fault-injection`).
//! * [`error`]: errors and the stable codes the UI receives.
//!
//! Formats: `docs/formats/penroam-binary-v1.md` (pages, journal) and
//! `docs/formats/model-and-archive.md` (model, archive). Design:
//! `docs/architecture/overview.md`.

#![warn(missing_docs)]

pub mod archive;
pub mod backup;
pub mod error;
pub mod fsutil;
pub mod history;
pub mod journal;
mod legacy;
pub mod library;
pub mod state;

pub use archive::{ArchiveSummary, ExportOptions, ImportOptions, ImportReport, Manifest};
pub use backup::{
    BackupManifest, BackupPreview, BackupProblem, BackupReader, BackupWriter, ConflictPolicy,
    RestoreOptions, RestoreReport,
};
pub use error::{ErrorCode, Result, StorageError};
pub use history::{HistoryPoint, HistoryRetention, HistoryStats, NotebookRestore};
pub use library::{
    sha256_hex, ApplyOutcome, Library, NewAsset, NewPage, NotebookSummary, OutboxStatus, PageRead,
    RecoveryReport, CANVAS_PAGE_GAP_PT,
};
