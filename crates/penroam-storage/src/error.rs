//! Storage errors and the stable error codes the UI receives.
//!
//! [`StorageError`] carries a human-readable message (safe to show; it never
//! contains note content beyond ids and titles the user typed). The app maps
//! it to `{ code, message }` with [`StorageError::code`], so the frontend can
//! branch on [`ErrorCode`] without parsing text.

use serde::Serialize;
use ts_rs::TS;

/// Everything that can go wrong in the library or archive code.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// A notebook, page, asset or file does not exist.
    #[error("{0} not found")]
    NotFound(String),
    /// The request clashes with the current state: a stale page revision
    /// (the editor must reload the page), an id that already exists, a page
    /// that is read-only, or the library being open in another process.
    #[error("{0}")]
    Conflict(String),
    /// The request itself is malformed (failed validation).
    #[error("{0}")]
    Invalid(String),
    /// Data on disk could not be decoded or failed a checksum. Nothing is
    /// deleted when this happens.
    #[error("stored data is damaged: {0}")]
    Corrupt(String),
    /// An imported `.penroam` file is not a valid archive.
    #[error("not a valid Penroam archive: {0}")]
    Archive(String),
    /// An input exceeded a size limit (archive entries, thumbnails, …).
    #[error("{0}")]
    TooLarge(String),
    /// A write failed because the disk is full. The operation was rolled
    /// back; the library keeps working once space is freed.
    #[error("the device is out of storage space ({0})")]
    StorageFull(String),
    /// Any other file-system error.
    #[error("file system error: {0}")]
    Io(String),
}

impl StorageError {
    /// The stable code for this error (sent to the UI next to the message).
    pub fn code(&self) -> ErrorCode {
        match self {
            StorageError::NotFound(_) => ErrorCode::NotFound,
            StorageError::Conflict(_) => ErrorCode::Conflict,
            StorageError::Invalid(_) => ErrorCode::Invalid,
            StorageError::Corrupt(_) => ErrorCode::Corrupt,
            StorageError::Archive(_) => ErrorCode::Archive,
            StorageError::TooLarge(_) => ErrorCode::TooLarge,
            StorageError::StorageFull(_) => ErrorCode::StorageFull,
            StorageError::Io(_) => ErrorCode::Io,
        }
    }
}

/// Stable, machine-readable error category sent to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ErrorCode {
    /// See [`StorageError::NotFound`].
    NotFound,
    /// See [`StorageError::Conflict`].
    Conflict,
    /// See [`StorageError::Invalid`].
    Invalid,
    /// See [`StorageError::Corrupt`].
    Corrupt,
    /// See [`StorageError::Archive`].
    Archive,
    /// See [`StorageError::TooLarge`].
    TooLarge,
    /// See [`StorageError::StorageFull`].
    StorageFull,
    /// See [`StorageError::Io`].
    Io,
    /// A PDF could not be read or written (used by the app for `penroam-pdf` errors).
    Pdf,
    /// A platform service failed (file dialogs, native ink, …; used by the app).
    Platform,
}

/// Classifies I/O errors so "disk full" and "missing file" get their own codes.
impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        if crate::fsutil::is_storage_full(&e) {
            StorageError::StorageFull(e.to_string())
        } else if e.kind() == std::io::ErrorKind::NotFound {
            StorageError::NotFound(e.to_string())
        } else {
            StorageError::Io(e.to_string())
        }
    }
}

/// A stale base revision is a [`StorageError::Conflict`] (the editor reloads
/// and retries); every other apply error means the batch itself is invalid.
impl From<penroam_core::ApplyError> for StorageError {
    fn from(e: penroam_core::ApplyError) -> Self {
        match e {
            penroam_core::ApplyError::RevisionConflict { .. } => {
                StorageError::Conflict(e.to_string())
            }
            _ => StorageError::Invalid(e.to_string()),
        }
    }
}

impl From<penroam_core::validate::ValidationError> for StorageError {
    fn from(e: penroam_core::validate::ValidationError) -> Self {
        StorageError::Invalid(e.0)
    }
}

/// Result type of this crate.
pub type Result<T> = std::result::Result<T, StorageError>;
