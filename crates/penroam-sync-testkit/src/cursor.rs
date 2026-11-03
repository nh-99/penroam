//! Tamper-evident pull cursors for the fake server.
//!
//! **Testkit only — not production crypto design.** A cursor is
//! `base64url(payload || tag)` where `tag` is HMAC-SHA256 (hand-rolled from
//! `sha2`, truncated to 16 bytes) over the payload, keyed with a per-server
//! random secret. The payload binds the cursor to the account, the stream
//! and the protocol major version.

use sha2::{Digest, Sha256};

use penroam_sync_protocol::base64url;
use penroam_sync_protocol::{AccountId, Cursor};

const CURSOR_FORMAT: u8 = 1;
/// Stream id of the account op log.
pub const STREAM_OPS: u8 = 1;
const TAG_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorPayload {
    pub account: String,
    pub stream: u8,
    pub protocol_major: u16,
    /// Last delivered `server_seq` (0 = before the first op).
    pub position: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorError {
    /// Malformed, tampered, or minted by another server.
    Invalid,
}

/// Textbook HMAC-SHA256 (RFC 2104) over `sha2`, to avoid another dependency
/// in test scaffolding.
fn hmac_sha256(key: &[u8; 32], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    k[..32].copy_from_slice(key);
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// `format ‖ protocol major ‖ stream ‖ account (u8 length + bytes) ‖ position`.
fn encode_payload(p: &CursorPayload) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + p.account.len());
    v.push(CURSOR_FORMAT);
    v.extend_from_slice(&p.protocol_major.to_le_bytes());
    v.push(p.stream);
    // Account ids are at most 128 bytes (validated newtype).
    v.push(p.account.len() as u8);
    v.extend_from_slice(p.account.as_bytes());
    v.extend_from_slice(&p.position.to_le_bytes());
    v
}

pub fn mint(secret: &[u8; 32], p: &CursorPayload) -> Cursor {
    let mut bytes = encode_payload(p);
    let tag = hmac_sha256(secret, &bytes);
    bytes.extend_from_slice(&tag[..TAG_LEN]);
    Cursor::new(base64url::encode(&bytes)).expect("minted cursors are bounded base64url")
}

pub fn open(secret: &[u8; 32], c: &Cursor) -> Result<CursorPayload, CursorError> {
    let bytes = base64url::decode(c.as_str()).map_err(|_| CursorError::Invalid)?;
    if bytes.len() < TAG_LEN + 13 {
        return Err(CursorError::Invalid);
    }
    let (payload, tag) = bytes.split_at(bytes.len() - TAG_LEN);
    let expected = hmac_sha256(secret, payload);
    // Constant-time comparison.
    let diff = expected[..TAG_LEN]
        .iter()
        .zip(tag)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if diff != 0 {
        return Err(CursorError::Invalid);
    }
    if payload[0] != CURSOR_FORMAT {
        return Err(CursorError::Invalid);
    }
    let protocol_major = u16::from_le_bytes([payload[1], payload[2]]);
    let stream = payload[3];
    let alen = payload[4] as usize;
    if payload.len() != 5 + alen + 8 {
        return Err(CursorError::Invalid);
    }
    let account = std::str::from_utf8(&payload[5..5 + alen]).map_err(|_| CursorError::Invalid)?;
    AccountId::new(account).map_err(|_| CursorError::Invalid)?;
    let mut pos = [0u8; 8];
    pos.copy_from_slice(&payload[5 + alen..]);
    Ok(CursorPayload {
        account: account.to_owned(),
        stream,
        protocol_major,
        position: u64::from_le_bytes(pos),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rfc4231_case2_prefix_free_key() {
        // RFC 4231 test case 2 uses a 4-byte key "Jefe"; zero padding to 32
        // bytes is equivalent for keys shorter than the block size.
        let mut key = [0u8; 32];
        key[..4].copy_from_slice(b"Jefe");
        let mac = hmac_sha256(&key, b"what do ya want for nothing?");
        assert_eq!(
            penroam_sync_protocol::hex::encode(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn roundtrip_and_tamper() {
        let s = [9u8; 32];
        let p = CursorPayload {
            account: "acct-1".into(),
            stream: STREAM_OPS,
            protocol_major: 1,
            position: 77,
        };
        let c = mint(&s, &p);
        assert_eq!(open(&s, &c), Ok(p));
        assert_eq!(open(&[8u8; 32], &c), Err(CursorError::Invalid));
        let mut raw = base64url::decode(c.as_str()).unwrap();
        for i in 0..raw.len() {
            raw[i] ^= 1;
            let t = Cursor::new(base64url::encode(&raw)).unwrap();
            assert_eq!(open(&s, &t), Err(CursorError::Invalid), "byte {i}");
            raw[i] ^= 1;
        }
    }
}
