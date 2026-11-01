use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
    #[error("stored data is damaged: {0}")]
    Corrupt(String),
    #[error("not a valid Penroam archive: {0}")]
    Archive(String),
    #[error("{0}")]
    TooLarge(String),
    #[error("the device is out of storage space ({0})")]
    StorageFull(String),
    #[error("file system error: {0}")]
    Io(String),
}

impl StorageError {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ErrorCode {
    NotFound,
    Conflict,
    Invalid,
    Corrupt,
    Archive,
    TooLarge,
    StorageFull,
    Io,
    Pdf,
    Platform,
}

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

impl From<penroam_core::ApplyError> for StorageError {
    fn from(e: penroam_core::ApplyError) -> Self {
        match e {
            penroam_core::ApplyError::RevisionConflict { .. } => StorageError::Conflict(e.to_string()),
            _ => StorageError::Invalid(e.to_string()),
        }
    }
}

impl From<penroam_core::validate::ValidationError> for StorageError {
    fn from(e: penroam_core::validate::ValidationError) -> Self {
        StorageError::Invalid(e.0)
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;
