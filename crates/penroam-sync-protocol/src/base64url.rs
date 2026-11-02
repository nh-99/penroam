//! Minimal unpadded base64url (RFC 4648 §5) without a dependency.
//!
//! Decoding is strict: no padding, no whitespace, no `+`/`/`, and the unused
//! low bits of the final character must be zero, so every byte string has
//! exactly one accepted encoding (needed for canonical, signed fields).

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Why a string is not canonical unpadded base64url.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Base64Error {
    /// A character outside the base64url alphabet (including `=`).
    #[error("invalid base64url character at {0}")]
    InvalidChar(usize),
    /// A length of `4n + 1` characters can never be produced by an encoder.
    #[error("invalid base64url length")]
    InvalidLength,
    /// The final character has non-zero unused bits (non-canonical).
    #[error("non-canonical base64url trailing bits")]
    NonCanonical,
}

/// Encode without padding.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

fn value(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'-' => 62,
        b'_' => 63,
        _ => return None,
    } as u32)
}

/// Strictly decode unpadded base64url.
pub fn decode(s: &str) -> Result<Vec<u8>, Base64Error> {
    let bytes = s.as_bytes();
    if bytes.len() % 4 == 1 {
        return Err(Base64Error::InvalidLength);
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3 + 2);
    for (ci, chunk) in bytes.chunks(4).enumerate() {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let v = value(c).ok_or(Base64Error::InvalidChar(ci * 4 + i))?;
            n |= v << (18 - 6 * i);
        }
        let produced = chunk.len() - 1; // 2 chars -> 1 byte, 3 -> 2, 4 -> 3
        let unused_mask = match chunk.len() {
            2 => 0xffff,
            3 => 0xff,
            _ => 0,
        };
        if n & unused_mask != 0 {
            return Err(Base64Error::NonCanonical);
        }
        for i in 0..produced {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_vectors() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (raw, enc) in cases {
            assert_eq!(encode(raw), enc);
            assert_eq!(decode(enc).unwrap(), raw);
        }
    }

    #[test]
    fn url_alphabet_roundtrip() {
        let data: Vec<u8> = (0..=255u8).collect();
        let e = encode(&data);
        assert!(!e.contains('+') && !e.contains('/') && !e.contains('='));
        assert_eq!(decode(&e).unwrap(), data);
    }

    #[test]
    fn strict_rejections() {
        assert_eq!(decode("Zg=="), Err(Base64Error::InvalidChar(2)));
        assert_eq!(decode("Z"), Err(Base64Error::InvalidLength));
        assert_eq!(decode("Zh"), Err(Base64Error::NonCanonical));
        assert_eq!(decode("ab+c"), Err(Base64Error::InvalidChar(2)));
    }
}
