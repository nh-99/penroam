//! Turning what the user typed into an FTS5 query.
//!
//! User input is never passed to FTS5 as query syntax. It is split into words the same way
//! the index tokenizer (`unicode61`) splits text: runs of letters and digits, lowercased.
//! Every word becomes a quoted FTS5 string, so operators and punctuation in the input are
//! inert. Bare words match as prefixes (`"wor"*`), except single characters; text inside
//! double quotes matches as an exact phrase. All parts must match (AND).

use std::collections::HashMap;

/// A parsed search query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedQuery {
    /// Bare words (prefix matches), lowercased.
    pub terms: Vec<String>,
    /// Quoted phrases, each a list of lowercased words.
    pub phrases: Vec<Vec<String>>,
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Parses user input (see the module docs). An unterminated quote runs to the end.
pub fn parse(input: &str) -> ParsedQuery {
    let mut q = ParsedQuery::default();
    for (i, part) in input.split('"').enumerate() {
        let ws = words(part);
        if i % 2 == 1 {
            match ws.len() {
                0 => {}
                1 => q.terms.extend(ws),
                _ => q.phrases.push(ws),
            }
        } else {
            q.terms.extend(ws);
        }
    }
    q.terms.dedup();
    q
}

impl ParsedQuery {
    /// Nothing to search for.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.phrases.is_empty()
    }

    /// The FTS5 MATCH expression. `expansions` maps a bare term to similar indexed words that
    /// may stand in for it (fuzzy matching).
    pub fn to_fts(&self, expansions: &HashMap<String, Vec<String>>) -> String {
        let mut parts = Vec::new();
        for t in &self.terms {
            let base = if t.chars().count() >= 2 {
                format!("\"{t}\"*")
            } else {
                format!("\"{t}\"")
            };
            match expansions.get(t) {
                Some(alts) if !alts.is_empty() => {
                    let mut ors = vec![base];
                    ors.extend(alts.iter().map(|a| format!("\"{}\"", a.replace('"', ""))));
                    parts.push(format!("({})", ors.join(" OR ")));
                }
                _ => parts.push(base),
            }
        }
        for p in &self.phrases {
            parts.push(format!("\"{}\"", p.join(" ")));
        }
        parts.join(" AND ")
    }

    /// Bare words long enough for fuzzy expansion (four or more characters).
    pub fn fuzzy_candidates(&self) -> Vec<String> {
        self.terms
            .iter()
            .filter(|t| t.chars().count() >= 4)
            .cloned()
            .collect()
    }

    /// Whether one recognized word matches some part of the query (used to pick which
    /// handwritten words to highlight).
    pub fn matches_word(&self, word: &str, expansions: &HashMap<String, Vec<String>>) -> bool {
        words(word).iter().any(|w| {
            self.terms.iter().any(|t| {
                w.starts_with(t.as_str())
                    || expansions
                        .get(t)
                        .is_some_and(|alts| alts.iter().any(|a| a == w))
            }) || self.phrases.iter().flatten().any(|p| p == w)
        })
    }
}

/// Levenshtein distance between two strings, counted in characters.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_syntax_is_inert() {
        let q = parse(r#"meeting "next week" NOT* (x) OR-y"#);
        assert_eq!(q.terms, vec!["meeting", "not", "x", "or", "y"]);
        assert_eq!(q.phrases, vec![vec!["next".to_string(), "week".into()]]);
        assert_eq!(
            q.to_fts(&HashMap::new()),
            r#""meeting"* AND "not"* AND "x" AND "or"* AND "y" AND "next week""#
        );
        assert!(parse(" \"\" ,.; ").is_empty());
    }

    #[test]
    fn distances() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("meeting", "meetinq"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("über", "uber"), 1);
    }
}
