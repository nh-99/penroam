//! Binary batch framing for push and pull bodies.
//!
//! JSON control headers travel next to raw binary ciphertext, never base64
//! inside JSON. Layout (all integers little-endian):
//!
//! ```text
//! magic   "PRSB"                  4 bytes
//! version u16 = 1                 2 bytes
//! count   u32                     4 bytes
//! count x {
//!   header_len  u32               4 bytes
//!   header_json [header_len]      UTF-8 JSON object
//!   ct_len      u32               4 bytes
//!   ciphertext  [ct_len]          opaque bytes
//! }
//! (no trailing bytes)
//! ```
//!
//! For push bodies the item header JSON is an [`crate::EnvelopeHeader`]; for
//! pull frames it is a [`crate::PulledOp`] (`{server_seq, header}`).
//!
//! The decoder is strict: it rejects bad magic/version, truncation anywhere,
//! trailing bytes, counts above the limit or larger than the remaining input
//! could possibly hold, and any oversize item — all **before** allocating.

use serde::{de::DeserializeOwned, Serialize};

use crate::error::ErrorCode;
use crate::limits::Limits;

/// Frame magic.
pub const MAGIC: &[u8; 4] = b"PRSB";
/// Frame layout version.
pub const FRAME_VERSION: u16 = 1;
/// Fixed preamble size (magic + version + count).
pub const PREAMBLE_LEN: usize = 10;
/// Smallest possible encoded item (two empty length prefixes).
pub const MIN_ITEM_LEN: usize = 8;
/// Media type of a framed body (same as [`crate::routes::MEDIA_BATCH`]).
pub const CONTENT_TYPE: &str = "application/vnd.penroam.batch.v1";

/// One framed item: raw header JSON bytes and raw ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameItem {
    /// UTF-8 JSON control header.
    pub header_json: Vec<u8>,
    /// Opaque ciphertext.
    pub ciphertext: Vec<u8>,
}

/// A framing failure. Structural failures reject the whole body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// Body larger than `max_batch_bytes`.
    #[error("batch is {len} bytes, maximum is {max}")]
    BatchTooLarge {
        /// Body length.
        len: u64,
        /// Maximum.
        max: u64,
    },
    /// Input ended early.
    #[error("truncated batch at offset {offset}: need {needed} more bytes")]
    Truncated {
        /// Offset where reading failed.
        offset: usize,
        /// Bytes needed.
        needed: usize,
    },
    /// Wrong magic.
    #[error("bad batch magic")]
    BadMagic,
    /// Unknown frame version.
    #[error("unsupported batch frame version {0}")]
    UnsupportedVersion(u16),
    /// Count above `max_batch_envelopes`.
    #[error("batch has {count} items, maximum is {max}")]
    TooManyItems {
        /// Declared count.
        count: u64,
        /// Maximum.
        max: u64,
    },
    /// Count cannot fit in the remaining input (hostile count).
    #[error("batch declares {count} items but only {remaining} bytes remain")]
    CountExceedsInput {
        /// Declared count.
        count: u64,
        /// Remaining bytes.
        remaining: usize,
    },
    /// Empty header JSON.
    #[error("item {index}: empty header")]
    EmptyHeader {
        /// Item index.
        index: usize,
    },
    /// Header JSON above `max_header_json_bytes`.
    #[error("item {index}: header is {len} bytes, maximum is {max}")]
    HeaderTooLarge {
        /// Item index.
        index: usize,
        /// Length.
        len: u64,
        /// Maximum.
        max: u64,
    },
    /// Ciphertext above `max_ciphertext_bytes_per_op`.
    #[error("item {index}: ciphertext is {len} bytes, maximum is {max}")]
    CiphertextTooLarge {
        /// Item index.
        index: usize,
        /// Length.
        len: u64,
        /// Maximum.
        max: u64,
    },
    /// Bytes after the last item.
    #[error("{0} trailing bytes after batch")]
    TrailingBytes(usize),
    /// Header JSON did not parse as the expected type.
    #[error("item {index}: invalid header JSON: {message}")]
    InvalidHeaderJson {
        /// Item index.
        index: usize,
        /// Parser message.
        message: String,
    },
}

impl FrameError {
    /// Normalized wire error code.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            Self::BatchTooLarge { .. }
            | Self::TooManyItems { .. }
            | Self::HeaderTooLarge { .. }
            | Self::CiphertextTooLarge { .. } => ErrorCode::PayloadTooLarge,
            _ => ErrorCode::MalformedBatch,
        }
    }
}

/// Per-item length checks, shared by encoder and decoder. The decoder calls
/// it with the declared lengths *before* slicing or allocating.
fn check_item(index: usize, h: u64, c: u64, limits: &Limits) -> Result<(), FrameError> {
    if h == 0 {
        return Err(FrameError::EmptyHeader { index });
    }
    if h > u64::from(limits.max_header_json_bytes) {
        return Err(FrameError::HeaderTooLarge {
            index,
            len: h,
            max: limits.max_header_json_bytes.into(),
        });
    }
    if c > limits.max_ciphertext_bytes_per_op {
        return Err(FrameError::CiphertextTooLarge {
            index,
            len: c,
            max: limits.max_ciphertext_bytes_per_op,
        });
    }
    Ok(())
}

/// Encode items into a frame, enforcing the same limits as the decoder.
pub fn encode_batch(items: &[FrameItem], limits: &Limits) -> Result<Vec<u8>, FrameError> {
    if items.len() as u64 > u64::from(limits.max_batch_envelopes) {
        return Err(FrameError::TooManyItems {
            count: items.len() as u64,
            max: limits.max_batch_envelopes.into(),
        });
    }
    let mut total = PREAMBLE_LEN as u64;
    for (index, it) in items.iter().enumerate() {
        check_item(
            index,
            it.header_json.len() as u64,
            it.ciphertext.len() as u64,
            limits,
        )?;
        total += (MIN_ITEM_LEN + it.header_json.len() + it.ciphertext.len()) as u64;
    }
    if total > limits.max_batch_bytes {
        return Err(FrameError::BatchTooLarge {
            len: total,
            max: limits.max_batch_bytes,
        });
    }
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FRAME_VERSION.to_le_bytes());
    // Bounded by max_batch_envelopes (<= 1024) and per-item limits (< 4 GiB).
    out.extend_from_slice(&(items.len() as u32).to_le_bytes());
    for it in items {
        out.extend_from_slice(&(it.header_json.len() as u32).to_le_bytes());
        out.extend_from_slice(&it.header_json);
        out.extend_from_slice(&(it.ciphertext.len() as u32).to_le_bytes());
        out.extend_from_slice(&it.ciphertext);
    }
    Ok(out)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], FrameError> {
        if self.remaining() < n {
            return Err(FrameError::Truncated {
                offset: self.pos,
                needed: n - self.remaining(),
            });
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, FrameError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Decode a frame into owned items.
pub fn decode_batch(bytes: &[u8], limits: &Limits) -> Result<Vec<FrameItem>, FrameError> {
    if bytes.len() as u64 > limits.max_batch_bytes {
        return Err(FrameError::BatchTooLarge {
            len: bytes.len() as u64,
            max: limits.max_batch_bytes,
        });
    }
    let mut r = Reader { buf: bytes, pos: 0 };
    if r.take(4)? != MAGIC {
        return Err(FrameError::BadMagic);
    }
    let v = r.take(2)?;
    let version = u16::from_le_bytes([v[0], v[1]]);
    if version != FRAME_VERSION {
        return Err(FrameError::UnsupportedVersion(version));
    }
    let count = u64::from(r.u32()?);
    if count > u64::from(limits.max_batch_envelopes) {
        return Err(FrameError::TooManyItems {
            count,
            max: limits.max_batch_envelopes.into(),
        });
    }
    if count * MIN_ITEM_LEN as u64 > r.remaining() as u64 {
        return Err(FrameError::CountExceedsInput {
            count,
            remaining: r.remaining(),
        });
    }
    let mut items = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let h = r.u32()? as usize;
        check_item(index, h as u64, 0, limits)?;
        let header = r.take(h)?;
        let c = r.u32()? as usize;
        check_item(index, h as u64, c as u64, limits)?;
        let ct = r.take(c)?;
        items.push(FrameItem {
            header_json: header.to_vec(),
            ciphertext: ct.to_vec(),
        });
    }
    if r.remaining() != 0 {
        return Err(FrameError::TrailingBytes(r.remaining()));
    }
    Ok(items)
}

/// Encode typed headers (compact JSON) with their ciphertexts.
pub fn encode_json_batch<H: Serialize>(
    items: &[(H, Vec<u8>)],
    limits: &Limits,
) -> Result<Vec<u8>, FrameError> {
    let raw: Vec<FrameItem> = items
        .iter()
        .enumerate()
        .map(|(index, (h, ct))| {
            Ok(FrameItem {
                header_json: serde_json::to_vec(h).map_err(|e| FrameError::InvalidHeaderJson {
                    index,
                    message: e.to_string(),
                })?,
                ciphertext: ct.clone(),
            })
        })
        .collect::<Result<_, FrameError>>()?;
    encode_batch(&raw, limits)
}

/// Decode a frame and parse each header as `H`. Any unparsable header
/// rejects the whole batch (its `op_id` cannot be trusted for a per-op ack).
pub fn decode_json_batch<H: DeserializeOwned>(
    bytes: &[u8],
    limits: &Limits,
) -> Result<Vec<(H, Vec<u8>)>, FrameError> {
    decode_batch(bytes, limits)?
        .into_iter()
        .enumerate()
        .map(|(index, it)| {
            let h = serde_json::from_slice(&it.header_json).map_err(|e| {
                FrameError::InvalidHeaderJson {
                    index,
                    message: e.to_string(),
                }
            })?;
            Ok((h, it.ciphertext))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::tests::sample;
    use crate::EnvelopeHeader;

    fn items() -> Vec<FrameItem> {
        vec![
            FrameItem {
                header_json: b"{\"a\":1}".to_vec(),
                ciphertext: vec![1, 2, 3],
            },
            FrameItem {
                header_json: b"{}".to_vec(),
                ciphertext: vec![],
            },
        ]
    }

    #[test]
    fn roundtrip() {
        let l = Limits::DEFAULT;
        let b = encode_batch(&items(), &l).unwrap();
        assert_eq!(&b[..4], b"PRSB");
        assert_eq!(decode_batch(&b, &l).unwrap(), items());
        let empty = encode_batch(&[], &l).unwrap();
        assert_eq!(empty.len(), PREAMBLE_LEN);
        assert!(decode_batch(&empty, &l).unwrap().is_empty());
    }

    #[test]
    fn every_truncation_rejected() {
        let l = Limits::DEFAULT;
        let b = encode_batch(&items(), &l).unwrap();
        for n in 0..b.len() {
            let e = decode_batch(&b[..n], &l).unwrap_err();
            assert!(
                matches!(
                    e,
                    FrameError::Truncated { .. } | FrameError::CountExceedsInput { .. }
                ),
                "prefix {n}: {e:?}"
            );
            assert_eq!(e.error_code(), ErrorCode::MalformedBatch);
        }
    }

    #[test]
    fn trailing_bytes_rejected() {
        let l = Limits::DEFAULT;
        let mut b = encode_batch(&items(), &l).unwrap();
        b.push(0);
        assert_eq!(decode_batch(&b, &l), Err(FrameError::TrailingBytes(1)));
    }

    #[test]
    fn bad_magic_and_version() {
        let l = Limits::DEFAULT;
        let mut b = encode_batch(&items(), &l).unwrap();
        b[0] = b'X';
        assert_eq!(decode_batch(&b, &l), Err(FrameError::BadMagic));
        let mut b = encode_batch(&items(), &l).unwrap();
        b[4] = 2;
        assert_eq!(decode_batch(&b, &l), Err(FrameError::UnsupportedVersion(2)));
    }

    #[test]
    fn hostile_counts() {
        let l = Limits::DEFAULT;
        let mut b = Vec::new();
        b.extend_from_slice(b"PRSB");
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode_batch(&b, &l),
            Err(FrameError::TooManyItems { .. })
        ));
        let mut b2 = b[..6].to_vec();
        b2.extend_from_slice(&200u32.to_le_bytes());
        b2.extend_from_slice(&[0u8; 16]);
        assert!(matches!(
            decode_batch(&b2, &l),
            Err(FrameError::CountExceedsInput { count: 200, .. })
        ));
    }

    #[test]
    fn oversize_items_rejected_before_reading() {
        let l = Limits::DEFAULT;
        let mut b = Vec::new();
        b.extend_from_slice(b"PRSB");
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&u32::MAX.to_le_bytes()); // header len, no data follows
        b.extend_from_slice(&[0u8; 4]);
        assert!(matches!(
            decode_batch(&b, &l),
            Err(FrameError::HeaderTooLarge { .. })
        ));

        let mut b = b[..10].to_vec();
        b.extend_from_slice(&2u32.to_le_bytes());
        b.extend_from_slice(b"{}");
        b.extend_from_slice(&u32::MAX.to_le_bytes()); // ciphertext len
        let e = decode_batch(&b, &l).unwrap_err();
        assert!(matches!(e, FrameError::CiphertextTooLarge { .. }));
        assert_eq!(e.error_code(), ErrorCode::PayloadTooLarge);
    }

    #[test]
    fn empty_header_rejected() {
        let l = Limits::DEFAULT;
        let mut b = b"PRSB".to_vec();
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[0u8; 8]);
        assert_eq!(
            decode_batch(&b, &l),
            Err(FrameError::EmptyHeader { index: 0 })
        );
    }

    #[test]
    fn batch_byte_limit() {
        let mut l = Limits::DEFAULT;
        l.max_batch_bytes = 20;
        let b = encode_batch(&items(), &Limits::DEFAULT).unwrap();
        assert!(matches!(
            decode_batch(&b, &l),
            Err(FrameError::BatchTooLarge { .. })
        ));
        assert!(matches!(
            encode_batch(&items(), &l),
            Err(FrameError::BatchTooLarge { .. })
        ));
    }

    #[test]
    fn encoder_enforces_limits() {
        let mut l = Limits::DEFAULT;
        l.max_batch_envelopes = 1;
        assert!(matches!(
            encode_batch(&items(), &l),
            Err(FrameError::TooManyItems { .. })
        ));
    }

    #[test]
    fn typed_roundtrip_and_bad_json() {
        let l = Limits::DEFAULT;
        let h = sample();
        let b = encode_json_batch(&[(h.clone(), b"ciphertext".to_vec())], &l).unwrap();
        let out: Vec<(EnvelopeHeader, Vec<u8>)> = decode_json_batch(&b, &l).unwrap();
        assert_eq!(out, vec![(h, b"ciphertext".to_vec())]);
        let bad = encode_batch(&items(), &l).unwrap();
        assert!(matches!(
            decode_json_batch::<EnvelopeHeader>(&bad, &l),
            Err(FrameError::InvalidHeaderJson { index: 0, .. })
        ));
    }
}
