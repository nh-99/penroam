//! Keeping credentials out of logs, for programs that implement or use this protocol.
//!
//! Two tools, meant to be used together:
//!
//! * [`HTTP_CLIENT_LOG_TARGETS`]: log targets of HTTP and TLS libraries that dump raw request
//!   bytes, headers included, at DEBUG or TRACE level. `ureq` (used by the `penroam-sync`
//!   client) does this, so a bearer token can end up in a log one 16-byte line at a time.
//!   Programs that install a logger must cap these targets at INFO **in code**, so that no
//!   `RUST_LOG` setting can lift the cap. This is the primary defence.
//! * [`scrub_line`]: a backstop for one formatted log line. It redacts credentials it can
//!   recognise (`Bearer` values, JWTs, recovery codes, tokens with a known prefix) and drops
//!   lines that look like raw byte dumps, because a secret spread over a dump cannot be
//!   recognised piece by piece.
//!
//! Scrubbing is pattern-based and therefore incomplete: it cannot see a secret with no known
//! shape. Never rely on it instead of not logging secrets; credential types should also redact
//! themselves in `Debug`/`Display`.

use std::borrow::Cow;

/// Log targets (prefixes) of HTTP/TLS crates that may log raw request or response bytes at
/// DEBUG/TRACE. Cap them at INFO wherever a logger is installed.
pub const HTTP_CLIENT_LOG_TARGETS: &[&str] = &[
    "ureq",
    "ureq_proto",
    "rustls",
    "hyper",
    "hyper_util",
    "h2",
    "reqwest",
];

/// Whether `target` belongs to one of [`HTTP_CLIENT_LOG_TARGETS`] (the crate itself or one of
/// its modules, `crate::module`).
pub fn is_http_client_target(target: &str) -> bool {
    HTTP_CLIENT_LOG_TARGETS.iter().any(|t| {
        target == *t
            || target
                .strip_prefix(t)
                .is_some_and(|rest| rest.starts_with("::"))
    })
}

/// What [`scrub_line`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scrubbed<'a> {
    /// Nothing recognised; the line is unchanged.
    Clean(&'a str),
    /// One or more credentials were replaced by `[redacted]`.
    Redacted(String),
    /// The line looks like a raw byte dump and must not be written at all.
    Drop,
}

impl Scrubbed<'_> {
    /// The text to write, if any.
    pub fn text(&self) -> Option<Cow<'_, str>> {
        match self {
            Scrubbed::Clean(s) => Some(Cow::Borrowed(s)),
            Scrubbed::Redacted(s) => Some(Cow::Borrowed(s.as_str())),
            Scrubbed::Drop => None,
        }
    }
}

/// Groups of five characters in a recovery code (`PRK1-` + 11 groups).
const RECOVERY_CODE_GROUPS: usize = 11;

/// The replacement text.
pub const REDACTED: &str = "[redacted]";

fn is_b64url(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// A hex dump line: at least four groups of exactly four lowercase or uppercase hex digits
/// separated by single spaces (`3a20 4265 6172 6572 …`), the layout byte-dump loggers use.
fn looks_like_hex_dump(line: &str) -> bool {
    let b = line.as_bytes();
    let group = |i: usize| i + 4 <= b.len() && b[i..i + 4].iter().all(u8::is_ascii_hexdigit);
    let mut i = 0;
    while i + 19 <= b.len() {
        let starts_word = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if starts_word && group(i) {
            let mut n = 1;
            let mut j = i + 4;
            while j + 5 <= b.len() && b[j] == b' ' && group(j + 1) {
                n += 1;
                j += 5;
            }
            let ends_word = j == b.len() || !b[j].is_ascii_alphanumeric();
            if n >= 4 && ends_word {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Length of the token starting at `i` (base64url characters plus `.`, for JWTs).
fn token_len(b: &[u8], i: usize, allow_dot: bool) -> usize {
    b[i..]
        .iter()
        .take_while(|&&c| is_b64url(c) || (allow_dot && c == b'.'))
        .count()
}

/// Redacts recognised credentials in one log line, or asks to drop it (see the module docs).
///
/// Recognised: the value after `Bearer` (at least eight characters; also with `.`, `:` or `=` as the
/// separator, as in byte dumps and query strings), compact JWTs (`eyJ….….…`), recovery codes (`PRK1-` and groups of
/// five, any case, `-` or space separated, at most the 11 groups of a full code) and tokens starting with one of `token_prefixes`
/// (for example a server's `pat_`/`prt_`), at least eight characters long after the prefix.
pub fn scrub_line<'a>(line: &'a str, token_prefixes: &[&str]) -> Scrubbed<'a> {
    if looks_like_hex_dump(line) {
        return Scrubbed::Drop;
    }
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut changed = false;
    let mut i = 0;
    while i < b.len() {
        let rest = &line[i..];
        let word_start = i == 0 || !is_b64url(b[i - 1]);
        // Bearer <token>
        if word_start && rest.len() > 7 && rest.as_bytes()[..6].eq_ignore_ascii_case(b"bearer") {
            let sep = b[i + 6];
            if matches!(sep, b' ' | b'.' | b':' | b'=') {
                let mut j = i + 7;
                while j < b.len() && b[j] == b' ' {
                    j += 1;
                }
                let n = token_len(b, j, true);
                // real bearer values are long; this keeps prose ("the bearer of …") intact
                if n >= 8 {
                    out.push_str(&line[i..j]);
                    out.push_str(REDACTED);
                    i = j + n;
                    changed = true;
                    continue;
                }
            }
        }
        // compact JWT: header.payload[.signature], header starting with {" in base64url
        if word_start && rest.starts_with("eyJ") {
            let n = token_len(b, i, true);
            if line[i..i + n].matches('.').count() >= 1 && n > 20 {
                out.push_str(REDACTED);
                i += n;
                changed = true;
                continue;
            }
        }
        // recovery code
        if word_start && rest.len() >= 10 && rest.as_bytes()[..4].eq_ignore_ascii_case(b"prk1") {
            let mut j = i + 4;
            let mut groups = 0;
            // a code is 34 bytes = 55 base32 characters = 11 groups; never read past that
            while groups < RECOVERY_CODE_GROUPS {
                let k = if j < b.len() && matches!(b[j], b'-' | b' ') {
                    j + 1
                } else {
                    j
                };
                let g = b[k..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .count();
                let ends = k + g == b.len() || !b[k + g].is_ascii_alphanumeric();
                if g == 5 && ends {
                    j = k + 5;
                    groups += 1;
                } else {
                    break;
                }
            }
            if groups > 0 {
                out.push_str(&line[i..i + 4]);
                out.push('-');
                out.push_str(REDACTED);
                i = j;
                changed = true;
                continue;
            }
        }
        // prefixed tokens
        if word_start {
            if let Some(p) = token_prefixes
                .iter()
                .find(|p| !p.is_empty() && rest.starts_with(**p))
            {
                let n = token_len(b, i + p.len(), false);
                if n >= 8 {
                    out.push_str(p);
                    out.push_str(REDACTED);
                    i += p.len() + n;
                    changed = true;
                    continue;
                }
            }
        }
        // copy one UTF-8 character
        let c = rest.chars().next().expect("non-empty");
        out.push(c);
        i += c.len_utf8();
    }
    if changed {
        Scrubbed::Redacted(out)
    } else {
        Scrubbed::Clean(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrub(s: &str) -> String {
        match scrub_line(s, &["pat_", "prt_"]) {
            Scrubbed::Clean(c) => c.to_string(),
            Scrubbed::Redacted(r) => r,
            Scrubbed::Drop => "<dropped>".into(),
        }
    }

    #[test]
    fn bearer_values() {
        assert_eq!(
            scrub("Authorization: Bearer abc.DEF-123_x"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(scrub("bearer=abcdef0123"), "bearer=[redacted]");
        assert_eq!(
            scrub(r#"{"h":"Bearer  tok3n-value"}"#),
            r#"{"h":"Bearer  [redacted]"}"#
        );
        assert_eq!(
            scrub("the bearer of news"),
            "the bearer of news",
            "prose stays"
        );
    }

    #[test]
    fn jwts_recovery_codes_and_prefixed_tokens() {
        let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.c2ln";
        assert_eq!(scrub(&format!("got {jwt} ok")), "got [redacted] ok");
        let code = format!("PRK1-{}", ["ABCDE"; 11].join("-"));
        assert_eq!(
            scrub(&format!("code {code} shown")),
            "code PRK1-[redacted] shown"
        );
        let typed = format!("prk1 {}", ["abcde"; 11].join(" "));
        assert_eq!(scrub(&format!("{typed} typed")), "prk1-[redacted] typed");
        // a partial code is still redacted (erring on the side of redaction)
        assert_eq!(scrub("PRK1-ABCDE-FGHJK"), "PRK1-[redacted]");
        assert_eq!(
            scrub("t=pat_Zm9vYmFyYmF6cXV4 r=prt_abcdefgh1234"),
            "t=pat_[redacted] r=prt_[redacted]"
        );
        assert_eq!(scrub("pat_short"), "pat_short", "too short to be a token");
        assert_eq!(
            scrub("compat_mode=on"),
            "compat_mode=on",
            "prefix inside a word"
        );
    }

    #[test]
    fn byte_dumps_are_dropped() {
        // the exact shape ureq_proto writes at TRACE
        assert_eq!(
            scrub("3a20 4265 6172 6572 2070 6174 5f42 5772  :.Bearer.pat_BWr"),
            "<dropped>"
        );
        assert_eq!(
            scrub(r#"{"message":"7a6c 4e71 3054 5a56  zlNq0TZV"}"#),
            "<dropped>"
        );
        // ordinary hex is fine
        let sha = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        assert_eq!(scrub(&format!("blob {sha}")), format!("blob {sha}"));
        assert_eq!(
            scrub("id 0c9f4f3e-6f0e-4b4e-9b1a-6a2f7c1e0a01"),
            "id 0c9f4f3e-6f0e-4b4e-9b1a-6a2f7c1e0a01"
        );
        assert_eq!(
            scrub("ports 8080 9100 5556"),
            "ports 8080 9100 5556",
            "three groups only"
        );
    }

    #[test]
    fn targets() {
        assert!(is_http_client_target("ureq_proto::util"));
        assert!(is_http_client_target("ureq"));
        assert!(!is_http_client_target("ureqx"));
        assert!(!is_http_client_target("cloud_api::app"));
    }

    #[test]
    fn non_ascii_text_survives() {
        assert_eq!(scrub("Notiz für später"), "Notiz für später");
        // multi-byte characters right where the keyword checks look (byte slicing must not split
        // them); every position of every sample is visited
        for t in [
            "é→ok…",
            "ab→cdefghij",
            "Bea→rer x",
            "PRK→1-ABCDE",
            "x→Bearer abcdefgh1234",
        ] {
            for k in 0..=t.len() {
                if t.is_char_boundary(k) {
                    let _ = scrub_line(&t[k..], &["pat_"]);
                }
            }
        }
        assert_eq!(scrub("x→Bearer abcdefgh1234 →"), "x→Bearer [redacted] →");
    }

    #[test]
    fn never_panics_on_mixed_input() {
        // deterministic xorshift; alphabet mixes keyword fragments, token characters and
        // multi-byte characters
        let alphabet = [
            "Bearer", "bearer", " ", ".", ":", "=", "PRK1", "prk1", "-", "eyJ", "pat_", "a", "Z",
            "7", "_", "é", "→", "…", "😀", "3a20", "\"", "\n",
        ];
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..5000 {
            let mut t = String::new();
            for _ in 0..(x % 40) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                t.push_str(alphabet[(x % alphabet.len() as u64) as usize]);
            }
            let _ = scrub_line(&t, &["pat_", "prt_"]);
        }
    }
}
