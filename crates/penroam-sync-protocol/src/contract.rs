//! Contract manifest and the shared fixture document types.
//!
//! The fixtures under `fixtures/v1/` are produced by
//! `cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures` and
//! checked by this crate's `tests/fixtures.rs` (the public schema
//! compatibility check). Fixture JSON is always written with
//! [`to_fixture_json`] so it re-serializes byte-identically.

use serde::{Deserialize, Serialize};

use crate::envelope::EnvelopeHeader;
use crate::error::ErrorCode;
use crate::version::{
    ProtocolVersion, ARCHIVE_FORMAT_VERSION, ENVELOPE_VERSION, FILE_FORMAT_VERSION,
    JOURNAL_FORMAT_VERSION, NOTEBOOK_SCHEMA_VERSION, OP_SCHEMA_MIN_SUPPORTED, OP_SCHEMA_VERSION,
    PAGE_SCHEMA_VERSION,
};

/// Op schema range advertised by a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpSchemaRange {
    /// Oldest applicable op schema.
    pub min: u16,
    /// Newest op schema.
    pub max: u16,
}

/// Local plaintext format versions (independent numbers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatVersions {
    /// `.prp` page file format.
    pub file_format: u16,
    /// Local op journal format.
    pub journal_format: u16,
    /// `.penroam` archive format.
    pub archive_format: u16,
    /// Notebook model schema.
    pub notebook_schema: u16,
    /// Page model schema.
    pub page_schema: u16,
}

/// `contract-manifest.json`: what this public client release speaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractManifest {
    /// Protocol version.
    pub protocol: ProtocolVersion,
    /// Envelope container version.
    pub envelope_version: u16,
    /// Supported op schema range.
    pub op_schema: OpSchemaRange,
    /// Local format versions.
    pub formats: FormatVersions,
    /// Git commit of the release; `null` in committed fixtures (filled by
    /// release tooling).
    pub git_sha: Option<String>,
}

impl ContractManifest {
    /// The manifest of this crate version (with `git_sha: None`).
    pub fn current() -> Self {
        Self {
            protocol: ProtocolVersion::CURRENT,
            envelope_version: ENVELOPE_VERSION,
            op_schema: OpSchemaRange {
                min: OP_SCHEMA_MIN_SUPPORTED,
                max: OP_SCHEMA_VERSION,
            },
            formats: FormatVersions {
                file_format: FILE_FORMAT_VERSION,
                journal_format: JOURNAL_FORMAT_VERSION,
                archive_format: ARCHIVE_FORMAT_VERSION,
                notebook_schema: NOTEBOOK_SCHEMA_VERSION,
                page_schema: PAGE_SCHEMA_VERSION,
            },
            git_sha: None,
        }
    }
}

/// An invalid-envelope fixture: a header that parses but must fail
/// [`EnvelopeHeader::validate`] with default limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidEnvelopeCase {
    /// What is wrong.
    pub description: String,
    /// The header.
    pub header: EnvelopeHeader,
    /// Expected [`crate::EnvelopeError::kind`].
    pub expected_error: String,
    /// Expected wire error code.
    pub expected_error_code: ErrorCode,
}

/// One row of `error-codes.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorCodeEntry {
    /// Wire code.
    pub code: ErrorCode,
    /// HTTP status.
    pub http_status: u16,
    /// Automatically retryable.
    pub retryable: bool,
}

/// The full error-code table.
pub fn error_code_table() -> Vec<ErrorCodeEntry> {
    ErrorCode::ALL
        .iter()
        .map(|&code| ErrorCodeEntry {
            code,
            http_status: code.http_status(),
            retryable: code.is_retryable(),
        })
        .collect()
}

/// Serialize a fixture document: pretty JSON (2-space indent) plus a
/// trailing newline. Deterministic for the types in this crate (no maps
/// with unordered keys, no floats).
pub fn to_fixture_json<T: Serialize>(value: &T) -> String {
    let mut s = serde_json::to_string_pretty(value).expect("fixture types always serialize");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_numbers_are_the_documented_ones() {
        let m = ContractManifest::current();
        assert_eq!((m.protocol.major, m.protocol.minor), (1, 5));
        assert_eq!(m.envelope_version, 1);
        assert_eq!((m.op_schema.min, m.op_schema.max), (1, 2));
        assert_eq!(
            m.formats,
            FormatVersions {
                file_format: 1,
                journal_format: 1,
                archive_format: 2,
                notebook_schema: 2,
                page_schema: 1
            }
        );
        assert!(to_fixture_json(&m).contains("\"git_sha\": null"));
    }
}
