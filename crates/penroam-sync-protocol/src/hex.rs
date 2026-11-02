//! Minimal lowercase hex helpers (no dependency).

/// Encode bytes as lowercase hex.
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 0x0f) as usize] as char);
    }
    s
}

/// Decode **lowercase** hex. Uppercase, odd length or other characters fail.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    (0..b.len())
        .step_by(2)
        .map(|i| Some((nibble(b[i])? << 4) | nibble(b[i + 1])?))
        .collect()
}

/// `true` if `s` is exactly 64 lowercase hex characters (a SHA-256 digest).
pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&data)).unwrap(), data);
        assert_eq!(encode(&[0xde, 0xad]), "dead");
    }

    #[test]
    fn rejects_bad_hex() {
        assert!(decode("DEAD").is_none());
        assert!(decode("abc").is_none());
        assert!(decode("zz").is_none());
        assert!(!is_sha256_hex(&"A".repeat(64)));
        assert!(is_sha256_hex(&"a".repeat(64)));
        assert!(!is_sha256_hex(&"a".repeat(63)));
    }
}
