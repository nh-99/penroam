//! The public envelope control header and its validation.
//!
//! The header is the only part of an operation the server can read. The
//! document operation itself is opaque ciphertext carried next to the header
//! in a framed batch (see [`crate::framing`]).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::base64url;
use crate::error::ErrorCode;
use crate::hex;
use crate::ids::{DeviceId, NotebookRoutingId, OpId};
use crate::limits::Limits;
use crate::version::{ENVELOPE_VERSION, OP_SCHEMA_MIN_SUPPORTED, OP_SCHEMA_VERSION};

/// Maximum length of the base64url `signature` field, in characters.
/// (An Ed25519 signature is 86 characters; this leaves room for future
/// schemes without letting a header grow unbounded.)
pub const SIGNATURE_MAX_LEN: usize = 512;

/// A causal reference to another operation by its emitting device and that
/// device's sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OpRef {
    /// Device that emitted the referenced operation.
    pub device_id: DeviceId,
    /// That device's `device_seq` of the referenced operation.
    pub device_seq: u64,
}

impl OpRef {
    /// Canonical sort key: device id bytes, then sequence.
    pub fn canonical_cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.device_id.as_str().as_bytes(), self.device_seq)
            .cmp(&(other.device_id.as_str().as_bytes(), other.device_seq))
    }
}

/// Public envelope control header (protocol v1, envelope version 1).
///
/// Field set and meaning are frozen for `envelope_version = 1`; see
/// `docs/protocol/sync-protocol-v1.md`. Unknown JSON fields are rejected so a
/// header can never carry unsigned data the receiver silently ignores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvelopeHeader {
    /// Container version; must equal [`ENVELOPE_VERSION`].
    pub envelope_version: u16,
    /// Client-generated globally unique id, stable across retries.
    pub op_id: OpId,
    /// Emitting device.
    pub device_id: DeviceId,
    /// Emitting device's persisted, strictly increasing counter.
    pub device_seq: u64,
    /// Opaque notebook routing id.
    pub notebook_id: NotebookRoutingId,
    /// Bounded causal dependencies (order on the wire is not significant;
    /// canonical encodings sort them).
    pub depends_on: Vec<OpRef>,
    /// Content key epoch used to encrypt the payload.
    pub key_epoch: u32,
    /// Schema version of the encrypted semantic operation payload.
    pub op_schema_version: u16,
    /// SHA-256 of the ciphertext, 64 lowercase hex characters.
    pub ciphertext_sha256: String,
    /// Exact ciphertext length in bytes.
    pub ciphertext_length: u64,
    /// Device signature over [`crate::canonical_signing_bytes`], unpadded
    /// base64url. Not verified yet (device signatures are not implemented).
    pub signature: String,
}

/// Why an envelope header is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    /// `envelope_version` is not one this implementation understands.
    #[error("unsupported envelope_version {found}")]
    UnsupportedEnvelopeVersion {
        /// The value found.
        found: u16,
    },
    /// `op_schema_version` is below the minimum ever defined (0 is invalid).
    #[error("invalid op_schema_version {found}")]
    InvalidOpSchemaVersion {
        /// The value found.
        found: u16,
    },
    /// The payload schema is newer than this client can apply.
    /// Returned only by [`EnvelopeHeader::check_op_schema_supported`].
    #[error("op_schema_version {found} is newer than supported {max}")]
    UnsupportedOpSchemaVersion {
        /// The value found.
        found: u16,
        /// Highest supported version.
        max: u16,
    },
    /// An id is longer than the server's advertised `max_id_length`.
    #[error("{field} is {len} bytes, maximum is {max}")]
    IdTooLong {
        /// Field name.
        field: &'static str,
        /// Actual length.
        len: usize,
        /// Maximum length.
        max: usize,
    },
    /// Too many causal references.
    #[error("depends_on has {count} entries, maximum is {max}")]
    TooManyDependencies {
        /// Number of entries.
        count: usize,
        /// Maximum allowed.
        max: usize,
    },
    /// The same reference appears twice.
    #[error("duplicate depends_on entry {device_id}:{device_seq}")]
    DuplicateDependency {
        /// Device of the duplicated reference.
        device_id: String,
        /// Sequence of the duplicated reference.
        device_seq: u64,
    },
    /// A reference to the emitting device that is not strictly earlier
    /// (would create a self-dependency or a cycle).
    #[error("depends_on references own device at seq {dep_seq} >= own device_seq {own_seq}")]
    DependencyCycle {
        /// The offending referenced sequence.
        dep_seq: u64,
        /// The envelope's own `device_seq`.
        own_seq: u64,
    },
    /// Ciphertext length is zero (an AEAD ciphertext always has a tag).
    #[error("ciphertext_length is zero")]
    EmptyCiphertext,
    /// Ciphertext length exceeds `max_ciphertext_bytes_per_op`.
    #[error("ciphertext_length {length} exceeds maximum {max}")]
    CiphertextTooLarge {
        /// Declared length.
        length: u64,
        /// Maximum allowed.
        max: u64,
    },
    /// `ciphertext_sha256` is not 64 lowercase hex characters.
    #[error("ciphertext_sha256 must be 64 lowercase hex characters")]
    InvalidCiphertextHash,
    /// `signature` is empty, too long or not canonical unpadded base64url.
    #[error(
        "signature must be 1..={SIGNATURE_MAX_LEN} characters of canonical unpadded base64url"
    )]
    InvalidSignatureEncoding,
}

impl EnvelopeError {
    /// Stable snake_case identifier of the failure (used in fixtures and
    /// in `ErrorBody.message` details).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UnsupportedEnvelopeVersion { .. } => "unsupported_envelope_version",
            Self::InvalidOpSchemaVersion { .. } => "invalid_op_schema_version",
            Self::UnsupportedOpSchemaVersion { .. } => "unsupported_op_schema_version",
            Self::IdTooLong { .. } => "id_too_long",
            Self::TooManyDependencies { .. } => "too_many_dependencies",
            Self::DuplicateDependency { .. } => "duplicate_dependency",
            Self::DependencyCycle { .. } => "dependency_cycle",
            Self::EmptyCiphertext => "empty_ciphertext",
            Self::CiphertextTooLarge { .. } => "ciphertext_too_large",
            Self::InvalidCiphertextHash => "invalid_ciphertext_hash",
            Self::InvalidSignatureEncoding => "invalid_signature_encoding",
        }
    }

    /// The normalized wire error code for this failure.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::UnsupportedEnvelopeVersion { .. } | Self::UnsupportedOpSchemaVersion { .. } => {
                ErrorCode::UpgradeRequired
            }
            Self::CiphertextTooLarge { .. } => ErrorCode::PayloadTooLarge,
            _ => ErrorCode::InvalidEnvelope,
        }
    }
}

impl EnvelopeHeader {
    /// Structural validation against the given (server-advertised) limits.
    ///
    /// Checks: envelope version, op schema >= minimum, id lengths, bounded
    /// `depends_on` without duplicates, self-references strictly earlier than
    /// the own `device_seq` (no cycles), `ciphertext_length` in
    /// `1..=max_ciphertext_bytes_per_op`, hash and signature encodings.
    ///
    /// It deliberately does **not** reject a *newer* `op_schema_version`:
    /// the payload is opaque to servers, which must relay it so newer clients
    /// can sync. Clients that decrypt and apply call
    /// [`Self::check_op_schema_supported`] and pause with `upgrade_required`.
    pub fn validate(&self, limits: &Limits) -> Result<(), EnvelopeError> {
        if self.envelope_version != ENVELOPE_VERSION {
            return Err(EnvelopeError::UnsupportedEnvelopeVersion {
                found: self.envelope_version,
            });
        }
        // Op schema versions start at 1; 0 is never valid.
        if self.op_schema_version == 0 {
            return Err(EnvelopeError::InvalidOpSchemaVersion {
                found: self.op_schema_version,
            });
        }
        let max_id = limits.max_id_length as usize;
        let mut ids: Vec<(&'static str, &str)> = vec![
            ("op_id", self.op_id.as_str()),
            ("device_id", self.device_id.as_str()),
            ("notebook_id", self.notebook_id.as_str()),
        ];
        ids.extend(
            self.depends_on
                .iter()
                .map(|d| ("depends_on.device_id", d.device_id.as_str())),
        );
        for (field, id) in ids {
            if id.len() > max_id {
                return Err(EnvelopeError::IdTooLong {
                    field,
                    len: id.len(),
                    max: max_id,
                });
            }
        }
        let max_deps = limits.max_depends_on as usize;
        if self.depends_on.len() > max_deps {
            return Err(EnvelopeError::TooManyDependencies {
                count: self.depends_on.len(),
                max: max_deps,
            });
        }
        let mut seen = HashSet::with_capacity(self.depends_on.len());
        for dep in &self.depends_on {
            if !seen.insert((dep.device_id.as_str(), dep.device_seq)) {
                return Err(EnvelopeError::DuplicateDependency {
                    device_id: dep.device_id.to_string(),
                    device_seq: dep.device_seq,
                });
            }
            if dep.device_id == self.device_id && dep.device_seq >= self.device_seq {
                return Err(EnvelopeError::DependencyCycle {
                    dep_seq: dep.device_seq,
                    own_seq: self.device_seq,
                });
            }
        }
        if self.ciphertext_length == 0 {
            return Err(EnvelopeError::EmptyCiphertext);
        }
        if self.ciphertext_length > limits.max_ciphertext_bytes_per_op {
            return Err(EnvelopeError::CiphertextTooLarge {
                length: self.ciphertext_length,
                max: limits.max_ciphertext_bytes_per_op,
            });
        }
        if !hex::is_sha256_hex(&self.ciphertext_sha256) {
            return Err(EnvelopeError::InvalidCiphertextHash);
        }
        if self.signature.is_empty()
            || self.signature.len() > SIGNATURE_MAX_LEN
            || base64url::decode(&self.signature).is_err()
        {
            return Err(EnvelopeError::InvalidSignatureEncoding);
        }
        Ok(())
    }

    /// Check the payload schema is one this client can apply. A failure
    /// means: keep the ciphertext and the outbox, pause sync, surface
    /// `upgrade_required`. Never drop the operation.
    pub fn check_op_schema_supported(&self) -> Result<(), EnvelopeError> {
        if self.op_schema_version < OP_SCHEMA_MIN_SUPPORTED {
            return Err(EnvelopeError::InvalidOpSchemaVersion {
                found: self.op_schema_version,
            });
        }
        if self.op_schema_version > OP_SCHEMA_VERSION {
            return Err(EnvelopeError::UnsupportedOpSchemaVersion {
                found: self.op_schema_version,
                max: OP_SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    /// `depends_on` sorted canonically (device id bytes, then sequence).
    pub fn sorted_depends_on(&self) -> Vec<&OpRef> {
        let mut deps: Vec<&OpRef> = self.depends_on.iter().collect();
        deps.sort_by(|a, b| a.canonical_cmp(b));
        deps
    }
}

/// Ciphertext does not match the header's declared length or digest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntegrityError {
    /// Byte length differs from `ciphertext_length`.
    #[error("ciphertext is {actual} bytes, header declares {declared}")]
    LengthMismatch {
        /// Declared length.
        declared: u64,
        /// Actual length.
        actual: u64,
    },
    /// SHA-256 differs from `ciphertext_sha256`.
    #[error("ciphertext SHA-256 does not match header")]
    HashMismatch,
}

impl IntegrityError {
    /// Wire error code ([`ErrorCode::IntegrityMismatch`]).
    pub fn error_code(&self) -> ErrorCode {
        ErrorCode::IntegrityMismatch
    }
}

/// Lowercase hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(&Sha256::digest(data))
}

/// Verify `ciphertext` against the header's declared length and SHA-256.
/// This is integrity only (transport corruption, buggy clients); it is not
/// authentication — that is the device signature (not implemented yet).
pub fn verify_ciphertext(header: &EnvelopeHeader, ciphertext: &[u8]) -> Result<(), IntegrityError> {
    let actual = ciphertext.len() as u64;
    if actual != header.ciphertext_length {
        return Err(IntegrityError::LengthMismatch {
            declared: header.ciphertext_length,
            actual,
        });
    }
    if sha256_hex(ciphertext) != header.ciphertext_sha256 {
        return Err(IntegrityError::HashMismatch);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample() -> EnvelopeHeader {
        let ct = b"ciphertext";
        EnvelopeHeader {
            envelope_version: 1,
            op_id: OpId::new("0190c6f2-7b1a-7cc0-9d1e-2f3a4b5c6d7e").unwrap(),
            device_id: DeviceId::new("dev-a").unwrap(),
            device_seq: 42,
            notebook_id: NotebookRoutingId::new("nb-1").unwrap(),
            depends_on: vec![
                OpRef {
                    device_id: DeviceId::new("dev-b").unwrap(),
                    device_seq: 7,
                },
                OpRef {
                    device_id: DeviceId::new("dev-a").unwrap(),
                    device_seq: 41,
                },
            ],
            key_epoch: 1,
            op_schema_version: 1,
            ciphertext_sha256: sha256_hex(ct),
            ciphertext_length: ct.len() as u64,
            signature: base64url::encode(&[0u8; 64]),
        }
    }

    fn err(h: &EnvelopeHeader) -> EnvelopeError {
        h.validate(&Limits::DEFAULT).unwrap_err()
    }

    #[test]
    fn valid_sample() {
        sample().validate(&Limits::DEFAULT).unwrap();
        sample().check_op_schema_supported().unwrap();
        verify_ciphertext(&sample(), b"ciphertext").unwrap();
    }

    #[test]
    fn version_checks() {
        let mut h = sample();
        h.envelope_version = 2;
        assert_eq!(
            err(&h),
            EnvelopeError::UnsupportedEnvelopeVersion { found: 2 }
        );
        assert_eq!(err(&h).error_code(), ErrorCode::UpgradeRequired);
        let mut h = sample();
        h.op_schema_version = 0;
        assert_eq!(err(&h), EnvelopeError::InvalidOpSchemaVersion { found: 0 });
        let mut h = sample();
        h.op_schema_version = OP_SCHEMA_VERSION + 1;
        h.validate(&Limits::DEFAULT).unwrap(); // servers relay newer payloads
        assert_eq!(
            h.check_op_schema_supported().unwrap_err().error_code(),
            ErrorCode::UpgradeRequired
        );
    }

    #[test]
    fn dependency_bounds() {
        let mut h = sample();
        h.depends_on = (0..65)
            .map(|i| OpRef {
                device_id: DeviceId::new("dev-b").unwrap(),
                device_seq: i,
            })
            .collect();
        assert_eq!(
            err(&h),
            EnvelopeError::TooManyDependencies { count: 65, max: 64 }
        );
        h.depends_on.pop();
        h.validate(&Limits::DEFAULT).unwrap();
    }

    #[test]
    fn duplicate_dependency() {
        let mut h = sample();
        h.depends_on.push(h.depends_on[0].clone());
        assert_eq!(err(&h).kind(), "duplicate_dependency");
    }

    #[test]
    fn self_reference_must_be_earlier() {
        let mut h = sample();
        h.depends_on[1].device_seq = 42;
        assert_eq!(
            err(&h),
            EnvelopeError::DependencyCycle {
                dep_seq: 42,
                own_seq: 42
            }
        );
        h.depends_on[1].device_seq = 43;
        assert_eq!(err(&h).kind(), "dependency_cycle");
        h.depends_on[1].device_seq = 0;
        h.validate(&Limits::DEFAULT).unwrap();
        // Other devices may have any sequence.
        h.depends_on[0].device_seq = u64::MAX;
        h.validate(&Limits::DEFAULT).unwrap();
    }

    #[test]
    fn ciphertext_bounds() {
        let mut h = sample();
        h.ciphertext_length = 0;
        assert_eq!(err(&h), EnvelopeError::EmptyCiphertext);
        h.ciphertext_length = Limits::DEFAULT.max_ciphertext_bytes_per_op + 1;
        assert_eq!(err(&h).error_code(), ErrorCode::PayloadTooLarge);
        h.ciphertext_length = Limits::DEFAULT.max_ciphertext_bytes_per_op;
        h.validate(&Limits::DEFAULT).unwrap();
    }

    #[test]
    fn hash_format() {
        for bad in [
            String::new(),
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
        ] {
            let mut h = sample();
            h.ciphertext_sha256 = bad;
            assert_eq!(err(&h), EnvelopeError::InvalidCiphertextHash);
        }
    }

    #[test]
    fn signature_format() {
        for bad in [
            String::new(),
            "abc=".into(),
            "a+b/".into(),
            "A".repeat(513),
            "Zh".into(),
        ] {
            let mut h = sample();
            h.signature = bad;
            assert_eq!(err(&h), EnvelopeError::InvalidSignatureEncoding);
        }
    }

    #[test]
    fn id_length_respects_advertised_limit() {
        let mut limits = Limits::DEFAULT;
        limits.max_id_length = 4;
        let e = sample().validate(&limits).unwrap_err();
        assert!(matches!(e, EnvelopeError::IdTooLong { field: "op_id", .. }));
    }

    #[test]
    fn integrity() {
        let h = sample();
        assert_eq!(
            verify_ciphertext(&h, b"ciphertexT"),
            Err(IntegrityError::HashMismatch)
        );
        assert_eq!(
            verify_ciphertext(&h, b"cipher"),
            Err(IntegrityError::LengthMismatch {
                declared: 10,
                actual: 6
            })
        );
    }

    #[test]
    fn unknown_fields_rejected_and_roundtrip() {
        let h = sample();
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<EnvelopeHeader>(&json).unwrap(), h);
        let extra = json.replacen('{', "{\"extra\":1,", 1);
        assert!(serde_json::from_str::<EnvelopeHeader>(&extra).is_err());
    }
}
