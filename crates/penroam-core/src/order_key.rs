//! Fractional order keys for page order.
//!
//! A key is a non-empty string over the base-62 digits `0-9A-Za-z` (ASCII
//! order = digit order) that does not end in `0`. Keys compare as plain
//! byte strings, and between any two distinct keys there is always another
//! one, so a page can be inserted or moved by writing *its own* key only:
//! concurrent moves of different pages never conflict, and replicas that
//! apply the same set of key assignments sort pages identically (ties —
//! two devices picking the same key — are broken by page id).

/// The key alphabet, in sort order.
pub const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// Hard limit on key length (hostile-input guard; ~10^1800 positions).
pub const MAX_KEY_LEN: usize = 1024;

fn digit(c: u8) -> Option<usize> {
    DIGITS.iter().position(|d| *d == c)
}

/// Whether `k` is a well-formed key.
pub fn is_valid(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= MAX_KEY_LEN
        && k.bytes().all(|c| digit(c).is_some())
        && !k.ends_with('0')
}

/// `a < result < b` (`None` = open end). Panics if `a >= b` or a key is
/// malformed; callers pass keys of pages sorted by key.
pub fn between(a: Option<&str>, b: Option<&str>) -> String {
    let a = a.unwrap_or("");
    if let Some(b) = b {
        assert!(a < b, "order keys out of order: {a:?} >= {b:?}");
    }
    let out = midpoint(a.as_bytes(), b.map(str::as_bytes));
    String::from_utf8(out).expect("ascii")
}

/// A key strictly between `a` and `b` (`None` = +∞), as short as possible:
/// copy the common prefix, then pick a digit halfway between the first
/// differing digits; if they are adjacent, keep `a`'s digit and recurse on the
/// rest of `a` against +∞ (or use `b`'s first digit when `b` is longer).
fn midpoint(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    if let Some(b) = b {
        // common prefix (a padded with '0')
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(b'0') == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = b[..n].to_vec();
            out.extend(midpoint(a.get(n..).unwrap_or(&[]), Some(&b[n..])));
            return out;
        }
    }
    let da = a.first().and_then(|c| digit(*c)).unwrap_or(0);
    let db = b
        .and_then(|b| b.first())
        .and_then(|c| digit(*c))
        .unwrap_or(DIGITS.len());
    if db - da > 1 {
        vec![DIGITS[(da + db).div_ceil(2)]]
    } else if let Some(b) = b.filter(|b| b.len() > 1) {
        vec![b[0]]
    } else {
        let mut out = vec![DIGITS[da]];
        out.extend(midpoint(a.get(1..).unwrap_or(&[]), None));
        out
    }
}

/// `n` strictly increasing keys strictly between `a` and `b` (balanced, so
/// key length grows with log(n), e.g. one key per page of a big PDF).
pub fn n_between(a: Option<&str>, b: Option<&str>, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let mid = between(a, b);
    let left_n = (n - 1) / 2;
    let mut out = n_between(a, Some(&mid), left_n);
    let right = n_between(Some(&mid), b, n - 1 - left_n);
    out.push(mid);
    out.extend(right);
    out
}

/// `n` evenly spaced, strictly increasing keys (for migrations and bulk
/// inserts; repeated [`between`] at the end would grow keys linearly).
pub fn spread(n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let mut width = 1u32;
    while 62u128.pow(width) <= (n as u128) + 1 {
        width += 1;
    }
    let space = 62u128.pow(width);
    (1..=n as u128)
        .map(|i| {
            let mut v = i * space / (n as u128 + 1);
            let mut digits = vec![b'0'; width as usize];
            for d in digits.iter_mut().rev() {
                *d = DIGITS[(v % 62) as usize];
                v /= 62;
            }
            while digits.last() == Some(&b'0') {
                digits.pop();
            }
            String::from_utf8(digits).expect("ascii")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn between_is_strictly_between_and_valid() {
        let mut keys = vec![between(None, None)];
        // repeated inserts at the front, the back and in the middle
        for i in 0..300 {
            let k = match i % 3 {
                0 => between(None, Some(&keys[0])),
                1 => between(Some(keys.last().unwrap()), None),
                _ => {
                    let m = keys.len() / 2;
                    between(Some(&keys[m - 1]), Some(&keys[m]))
                }
            };
            assert!(is_valid(&k), "{k:?}");
            let pos = keys.partition_point(|x| x < &k);
            assert!(pos == keys.len() || keys[pos] != k);
            keys.insert(pos, k);
        }
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        assert!(keys.iter().all(|k| k.len() < 64), "keys stay short");
    }

    #[test]
    fn adjacent_digits_and_prefixes() {
        for (a, b) in [
            ("1", "2"),
            ("1", "11"),
            ("V", "V1"),
            ("z", "zz"),
            ("01", "1"),
        ] {
            let k = between(Some(a), Some(b));
            assert!(a < k.as_str() && k.as_str() < b, "{a} < {k} < {b}");
            assert!(is_valid(&k));
        }
    }

    #[test]
    fn spread_is_sorted_and_valid() {
        for n in [0, 1, 2, 61, 62, 63, 1000, 10_000] {
            let k = spread(n);
            assert_eq!(k.len(), n);
            assert!(k.iter().all(|k| is_valid(k)));
            assert!(k.windows(2).all(|w| w[0] < w[1]));
        }
    }

    #[test]
    fn n_between_is_balanced() {
        for (a, b) in [
            (None, None),
            (Some("V"), Some("W")),
            (Some("z"), None),
            (None, Some("1")),
        ] {
            let k = n_between(a, b, 10_000);
            assert_eq!(k.len(), 10_000);
            assert!(k.windows(2).all(|w| w[0] < w[1]));
            assert!(k.iter().all(|x| is_valid(x)
                && a.is_none_or(|a| a < x.as_str())
                && b.is_none_or(|b| x.as_str() < b)));
            assert!(k.iter().map(String::len).max().unwrap() < 40);
        }
    }

    #[test]
    fn validity() {
        assert!(!is_valid(""));
        assert!(!is_valid("a0"));
        assert!(!is_valid("a-b"));
        assert!(is_valid("a01"));
    }
}
