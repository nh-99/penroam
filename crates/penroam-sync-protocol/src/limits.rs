//! Size limits with documented defaults and hard maxima.
//!
//! A server advertises its effective [`Limits`] in `GET /v1/meta`. A server
//! may lower any value below the default but can never raise one above the
//! hard maximum in [`Limits::HARD_MAX`]; clients call [`Limits::validate`] on
//! what they receive and refuse to sync with a server advertising values
//! outside the hard maxima (a hostile server must not make clients allocate
//! unbounded memory). Pricing/quota policy is *not* expressed here.

use serde::{Deserialize, Serialize};

use crate::ids::ID_MAX_LEN;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// Protocol size limits. All byte counts are bytes of ciphertext / wire data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum number of envelopes in one push batch or pull page frame.
    /// Default 256, hard max 1024.
    pub max_batch_envelopes: u32,
    /// Maximum total size of one framed batch body in bytes.
    /// Default 8 MiB, hard max 64 MiB.
    pub max_batch_bytes: u64,
    /// Maximum ciphertext bytes of a single operation envelope.
    /// Default 1 MiB, hard max 16 MiB.
    pub max_ciphertext_bytes_per_op: u64,
    /// Maximum JSON control-header bytes per framed item.
    /// Default 16 KiB, hard max 64 KiB.
    pub max_header_json_bytes: u32,
    /// Maximum size of one encrypted blob (asset) in bytes.
    /// Default 64 MiB, hard max 1 GiB. (Blob transfer is not implemented yet.)
    pub max_blob_bytes: u64,
    /// Maximum number of `depends_on` causal references per envelope.
    /// Default 64, hard max 256.
    pub max_depends_on: u32,
    /// Maximum `limit` accepted by `GET /v1/ops` (larger values are clamped).
    /// Default 500, hard max 1000.
    pub max_pull_page_size: u32,
    /// Maximum byte length of any opaque id. Default and hard max 128.
    pub max_id_length: u32,
}

/// A limit advertised outside its hard maximum (or zero).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("limit `{field}` = {value} is outside 1..={hard_max}")]
pub struct LimitsError {
    /// Field name.
    pub field: &'static str,
    /// Advertised value.
    pub value: u64,
    /// Hard maximum.
    pub hard_max: u64,
}

impl Limits {
    /// Documented defaults.
    pub const DEFAULT: Limits = Limits {
        max_batch_envelopes: 256,
        max_batch_bytes: 8 * MIB,
        max_ciphertext_bytes_per_op: MIB,
        max_header_json_bytes: 16 * KIB as u32,
        max_blob_bytes: 64 * MIB,
        max_depends_on: 64,
        max_pull_page_size: 500,
        max_id_length: ID_MAX_LEN as u32,
    };

    /// Hard maxima. No conforming server may advertise more.
    pub const HARD_MAX: Limits = Limits {
        max_batch_envelopes: 1024,
        max_batch_bytes: 64 * MIB,
        max_ciphertext_bytes_per_op: 16 * MIB,
        max_header_json_bytes: 64 * KIB as u32,
        max_blob_bytes: 1024 * MIB,
        max_depends_on: 256,
        max_pull_page_size: 1000,
        max_id_length: ID_MAX_LEN as u32,
    };

    fn fields(&self) -> [(&'static str, u64); 8] {
        [
            ("max_batch_envelopes", self.max_batch_envelopes as u64),
            ("max_batch_bytes", self.max_batch_bytes),
            (
                "max_ciphertext_bytes_per_op",
                self.max_ciphertext_bytes_per_op,
            ),
            ("max_header_json_bytes", self.max_header_json_bytes as u64),
            ("max_blob_bytes", self.max_blob_bytes),
            ("max_depends_on", self.max_depends_on as u64),
            ("max_pull_page_size", self.max_pull_page_size as u64),
            ("max_id_length", self.max_id_length as u64),
        ]
    }

    /// Check every value is in `1..=HARD_MAX`.
    pub fn validate(&self) -> Result<(), LimitsError> {
        for ((field, value), (_, hard_max)) in
            self.fields().into_iter().zip(Self::HARD_MAX.fields())
        {
            if value == 0 || value > hard_max {
                return Err(LimitsError {
                    field,
                    value,
                    hard_max,
                });
            }
        }
        Ok(())
    }

    /// Return a copy with every value clamped into `1..=HARD_MAX`.
    pub fn clamped(&self) -> Limits {
        let h = Self::HARD_MAX;
        Limits {
            max_batch_envelopes: self.max_batch_envelopes.clamp(1, h.max_batch_envelopes),
            max_batch_bytes: self.max_batch_bytes.clamp(1, h.max_batch_bytes),
            max_ciphertext_bytes_per_op: self
                .max_ciphertext_bytes_per_op
                .clamp(1, h.max_ciphertext_bytes_per_op),
            max_header_json_bytes: self.max_header_json_bytes.clamp(1, h.max_header_json_bytes),
            max_blob_bytes: self.max_blob_bytes.clamp(1, h.max_blob_bytes),
            max_depends_on: self.max_depends_on.clamp(1, h.max_depends_on),
            max_pull_page_size: self.max_pull_page_size.clamp(1, h.max_pull_page_size),
            max_id_length: self.max_id_length.clamp(1, h.max_id_length),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_hard_max_are_valid() {
        Limits::DEFAULT.validate().unwrap();
        Limits::HARD_MAX.validate().unwrap();
    }

    #[test]
    fn defaults_do_not_exceed_hard_max() {
        for ((name, d), (_, h)) in Limits::DEFAULT
            .fields()
            .into_iter()
            .zip(Limits::HARD_MAX.fields())
        {
            assert!(d <= h, "{name}");
        }
    }

    #[test]
    fn rejects_out_of_range() {
        let mut l = Limits::DEFAULT;
        l.max_batch_envelopes = 1025;
        assert_eq!(l.validate().unwrap_err().field, "max_batch_envelopes");
        let mut l = Limits::DEFAULT;
        l.max_pull_page_size = 0;
        assert_eq!(l.validate().unwrap_err().field, "max_pull_page_size");
        let mut l = Limits::DEFAULT;
        l.max_ciphertext_bytes_per_op = u64::MAX;
        assert!(l.validate().is_err());
        assert_eq!(l.clamped().max_ciphertext_bytes_per_op, 16 * MIB);
        l.clamped().validate().unwrap();
    }
}
