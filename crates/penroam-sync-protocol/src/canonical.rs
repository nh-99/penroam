//! Deterministic canonical byte encodings.
//!
//! These are the exact bytes a device will sign and the exact bytes to be used
//! as AEAD associated data. **No cryptography happens here** — this module
//! only fixes the layouts so every implementation (Rust, TypeScript, Kotlin,
//! Swift) produces identical bytes. The layouts are part of envelope version 1
//! and frozen together with it.
//!
//! Encoding rules (both layouts):
//! * A fixed ASCII domain-separation prefix ending in `\0` (not length-prefixed).
//! * Fields in a fixed order; never JSON, never string concatenation.
//! * Every integer is encoded as **u64 little-endian** regardless of its Rust
//!   width, so widening a field later does not change the bytes.
//! * Every string is encoded as **u32 little-endian byte length** followed by
//!   its UTF-8 bytes (ids are ASCII by construction).
//! * `depends_on` is encoded as a u32 LE count followed by the entries sorted
//!   by `(device_id bytes, device_seq)` — the input order does not matter.
//!
//! Callers must [`crate::EnvelopeHeader::validate`] a header before signing or
//! verifying it; the encoders themselves are total and never fail.

use crate::envelope::EnvelopeHeader;

/// Domain-separation prefix of the signing layout (20 bytes, includes `\0`).
pub const SIGNING_DOMAIN: &[u8] = b"penroam/envelope/v1\0";
/// Domain-separation prefix of the AEAD associated-data layout (16 bytes).
pub const AAD_DOMAIN: &[u8] = b"penroam/aead/v1\0";
/// Domain prefix of [`canonical_checkpoint_bytes`].
pub const CHECKPOINT_DOMAIN: &[u8] = b"penroam/checkpoint/v1\0";

struct Writer(Vec<u8>);

impl Writer {
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        // Ids are bounded to 128 bytes and the hash to 64, so this cannot
        // truncate; saturate anyway rather than panic on hostile input.
        self.u32(u32::try_from(s.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(s.as_bytes());
    }
    /// Dependencies in canonical order (by device id bytes, then sequence),
    /// so the same set always encodes the same bytes whatever the wire order.
    fn deps(&mut self, h: &EnvelopeHeader) {
        let deps = h.sorted_depends_on();
        self.u32(u32::try_from(deps.len()).unwrap_or(u32::MAX));
        for d in deps {
            self.str(d.device_id.as_str());
            self.u64(d.device_seq);
        }
    }
}

/// Bytes covered by the device signature: every header field except
/// `signature`.
///
/// Layout (see the byte table in `docs/protocol/sync-protocol-v1.md`):
///
/// | # | field | encoding |
/// |---|---|---|
/// | 0 | prefix | `"penroam/envelope/v1\0"` (20 raw bytes) |
/// | 1 | envelope_version | u64 LE |
/// | 2 | op_id | u32 LE len + bytes |
/// | 3 | device_id | u32 LE len + bytes |
/// | 4 | device_seq | u64 LE |
/// | 5 | notebook_id | u32 LE len + bytes |
/// | 6 | depends_on | u32 LE count, then sorted `(u32 len + device_id, u64 seq)` |
/// | 7 | key_epoch | u64 LE |
/// | 8 | op_schema_version | u64 LE |
/// | 9 | ciphertext_sha256 | u32 LE len (64) + ASCII lowercase hex |
/// | 10 | ciphertext_length | u64 LE |
pub fn canonical_signing_bytes(h: &EnvelopeHeader) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(256));
    w.0.extend_from_slice(SIGNING_DOMAIN);
    w.u64(h.envelope_version.into());
    w.str(h.op_id.as_str());
    w.str(h.device_id.as_str());
    w.u64(h.device_seq);
    w.str(h.notebook_id.as_str());
    w.deps(h);
    w.u64(h.key_epoch.into());
    w.u64(h.op_schema_version.into());
    w.str(&h.ciphertext_sha256);
    w.u64(h.ciphertext_length);
    w.0
}

/// **AEAD associated-data layout** for the operation payload (the sync engine will feed
/// these bytes to the AEAD; nothing is encrypted here).
///
/// Binds the ciphertext to its identity, versions, key epoch, length and
/// causal dependencies so a server cannot splice a payload under another
/// header. It excludes `ciphertext_sha256` (that is computed over the AEAD
/// output) and `signature`. `ciphertext_length` is known before encryption
/// for a fixed-tag AEAD (`plaintext_len + tag_len`).
///
/// | # | field | encoding |
/// |---|---|---|
/// | 0 | prefix | `"penroam/aead/v1\0"` (16 raw bytes) |
/// | 1 | envelope_version | u64 LE |
/// | 2 | op_schema_version | u64 LE |
/// | 3 | key_epoch | u64 LE |
/// | 4 | op_id | u32 LE len + bytes |
/// | 5 | device_id | u32 LE len + bytes |
/// | 6 | device_seq | u64 LE |
/// | 7 | notebook_id | u32 LE len + bytes |
/// | 8 | ciphertext_length | u64 LE |
/// | 9 | depends_on | u32 LE count, then sorted `(u32 len + device_id, u64 seq)` |
pub fn canonical_aad_bytes(h: &EnvelopeHeader) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(256));
    w.0.extend_from_slice(AAD_DOMAIN);
    w.u64(h.envelope_version.into());
    w.u64(h.op_schema_version.into());
    w.u64(h.key_epoch.into());
    w.str(h.op_id.as_str());
    w.str(h.device_id.as_str());
    w.u64(h.device_seq);
    w.str(h.notebook_id.as_str());
    w.u64(h.ciphertext_length);
    w.deps(h);
    w.0
}

/// Bytes a device signs for a [`crate::dto::CheckpointRecord`]: the domain
/// prefix, then every field except `signature` in declaration order, with the
/// same integer (u64 LE) and string (u32 LE length + UTF-8) encoding as the
/// envelope layouts.
pub fn canonical_checkpoint_bytes(r: &crate::dto::CheckpointRecord) -> Vec<u8> {
    let mut w = Writer(Vec::with_capacity(256));
    w.0.extend_from_slice(CHECKPOINT_DOMAIN);
    w.str(r.covers_cursor.as_str());
    w.u64(r.covers_server_seq);
    w.str(r.blob_id.as_str());
    w.str(&r.blob_sha256);
    w.u64(r.blob_length);
    w.u64(r.key_epoch.into());
    w.str(r.created_by.as_str());
    w.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::tests::sample;
    use crate::ids::DeviceId;
    use crate::OpRef;

    #[test]
    fn prefix_lengths() {
        assert_eq!(SIGNING_DOMAIN.len(), 20);
        assert_eq!(AAD_DOMAIN.len(), 16);
    }

    #[test]
    fn deterministic_and_order_independent() {
        let a = sample();
        let mut b = sample();
        b.depends_on.reverse();
        assert_ne!(a.depends_on, b.depends_on);
        assert_eq!(canonical_signing_bytes(&a), canonical_signing_bytes(&a));
        assert_eq!(canonical_signing_bytes(&a), canonical_signing_bytes(&b));
        assert_eq!(canonical_aad_bytes(&a), canonical_aad_bytes(&b));
    }

    #[test]
    fn signature_not_covered_everything_else_is() {
        let base = sample();
        let base_bytes = canonical_signing_bytes(&base);
        let mut s = base.clone();
        s.signature = "AAAA".into();
        assert_eq!(canonical_signing_bytes(&s), base_bytes);

        type Mutation = Box<dyn Fn(&mut crate::EnvelopeHeader)>;
        let mutations: Vec<Mutation> = vec![
            Box::new(|h| h.envelope_version = 2),
            Box::new(|h| h.op_id = "x".parse().unwrap()),
            Box::new(|h| h.device_id = "x".parse().unwrap()),
            Box::new(|h| h.device_seq += 1),
            Box::new(|h| h.notebook_id = "x".parse().unwrap()),
            Box::new(|h| h.depends_on.pop().map(drop).unwrap_or(())),
            Box::new(|h| h.depends_on[0].device_seq += 1),
            Box::new(|h| h.key_epoch += 1),
            Box::new(|h| h.op_schema_version += 1),
            Box::new(|h| h.ciphertext_sha256 = "0".repeat(64)),
            Box::new(|h| h.ciphertext_length += 1),
        ];
        for (i, m) in mutations.iter().enumerate() {
            let mut h = base.clone();
            m(&mut h);
            assert_ne!(canonical_signing_bytes(&h), base_bytes, "mutation {i}");
        }
    }

    #[test]
    fn length_prefix_prevents_boundary_ambiguity() {
        // "ab" + "c" must not encode like "a" + "bc".
        let mut x = sample();
        let mut y = sample();
        x.op_id = "ab".parse().unwrap();
        x.device_id = "c".parse().unwrap();
        y.op_id = "a".parse().unwrap();
        y.device_id = "bc".parse().unwrap();
        x.depends_on.clear();
        y.depends_on.clear();
        assert_ne!(canonical_signing_bytes(&x), canonical_signing_bytes(&y));
    }

    #[test]
    fn exact_layout_small_header() {
        let mut h = sample();
        h.op_id = "o".parse().unwrap();
        h.device_id = "d".parse().unwrap();
        h.notebook_id = "n".parse().unwrap();
        h.device_seq = 2;
        h.depends_on = vec![OpRef {
            device_id: DeviceId::new("e").unwrap(),
            device_seq: 3,
        }];
        h.key_epoch = 4;
        h.op_schema_version = 1;
        h.ciphertext_sha256 = "a".repeat(64);
        h.ciphertext_length = 5;
        let mut exp = Vec::new();
        exp.extend_from_slice(b"penroam/envelope/v1\0");
        exp.extend_from_slice(&1u64.to_le_bytes());
        exp.extend_from_slice(&[1, 0, 0, 0, b'o']);
        exp.extend_from_slice(&[1, 0, 0, 0, b'd']);
        exp.extend_from_slice(&2u64.to_le_bytes());
        exp.extend_from_slice(&[1, 0, 0, 0, b'n']);
        exp.extend_from_slice(&[1, 0, 0, 0]);
        exp.extend_from_slice(&[1, 0, 0, 0, b'e']);
        exp.extend_from_slice(&3u64.to_le_bytes());
        exp.extend_from_slice(&4u64.to_le_bytes());
        exp.extend_from_slice(&1u64.to_le_bytes());
        exp.extend_from_slice(&[64, 0, 0, 0]);
        exp.extend_from_slice("a".repeat(64).as_bytes());
        exp.extend_from_slice(&5u64.to_le_bytes());
        assert_eq!(canonical_signing_bytes(&h), exp);
    }

    #[test]
    fn signing_and_aad_are_domain_separated() {
        let h = sample();
        assert_ne!(canonical_signing_bytes(&h), canonical_aad_bytes(&h));
        assert!(canonical_aad_bytes(&h).starts_with(AAD_DOMAIN));
    }
}
