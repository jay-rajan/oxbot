//! Values in text: the numbers an answer or a summary states, and whether
//! each appears in what the bot read. Used to check a final answer before
//! it's accepted and to drop invented facts from a compaction.
//!
//! Text is normalized first ([`crate::rows::clean_number_text`]: digit
//! groups joined, `−` as minus), and numbers match as whole tokens: `3.1`
//! is not in `3.10`, `12` is not in `125`.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use crate::rows::clean_number_text;

/// Bare addresses: numbers in them are part of the address.
static URL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:https?://|www\.)\S+").ok());

/// What a number is, for checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    /// A plain number.
    Plain,
    /// Money (a currency sign before it), a percent or a temperature.
    Marked,
    /// Not a value: a list marker (`1.` or `2)` starting a line, `#1`), an
    /// ordinal (`2nd`), or part of a web address.
    Skip,
}

/// One number as written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    /// As written, normalized (`-5.2`, `1200`, `3.10`).
    text: String,
    /// Dots between its digits: 1 for a decimal, more for a version or a
    /// date (`1.2.3`).
    dots: u8,
    /// Its whole part, if it fits.
    whole: Option<u64>,
    sort: Sort,
}

/// Whether only spaces stand between `at` and the start of its line.
fn starts_line(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .rev()
        .find(|c| !matches!(c, ' ' | '\t'))
        .is_none_or(|c| c == '\n' || c == '\r')
}

fn url_ranges(text: &str) -> Vec<Range<usize>> {
    URL.as_ref()
        .map(|re| re.find_iter(text).map(|m| m.range()).collect())
        .unwrap_or_default()
}

/// Every number token in normalized `text`. `glued`: also digits right
/// after a letter (`v2026`, `B12`), for what was read.
fn tokens(text: &str, glued: bool) -> Vec<Token> {
    let urls = url_ranges(text);
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let prev = text[..i].chars().next_back();
        // Inside a word or an id (`r12`, `3_14`, `x86`): not a number.
        if prev.is_some_and(|c| c.is_ascii_digit() || c == '_' || (!glued && c.is_alphanumeric())) {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            continue;
        }
        let start = i;
        let mut j = i;
        let mut dots = 0u8;
        loop {
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j + 1 < b.len() && b[j] == b'.' && b[j + 1].is_ascii_digit() {
                dots = dots.saturating_add(1);
                j += 1;
            } else {
                break;
            }
        }
        let next = text[j..].chars().next();
        if next == Some('_') {
            i = j + 1;
            continue;
        }
        let negative = start > 0
            && b[start - 1] == b'-'
            && (start < 2 || !(b[start - 2].is_ascii_alphanumeric()));
        let digits = &text[start..j];
        let letters: String = text[j..]
            .chars()
            .take_while(|c| c.is_alphabetic())
            .collect::<String>()
            .to_lowercase();
        let ordinal = matches!(letters.as_str(), "st" | "nd" | "rd" | "th") && dots == 0;
        let after = text[j..].trim_start_matches(' ');
        let unit = after.starts_with('%') || after.starts_with('°');
        let before = text[..start]
            .trim_end_matches(['-', '+'])
            .trim_end_matches(' ');
        let money = before.ends_with(['$', '€', '£', '¥', '₹']);
        let marker = prev == Some('#')
            || (dots == 0
                && text[j..].starts_with(['.', ')'])
                && text[j + 1..].chars().next().is_none_or(char::is_whitespace)
                && starts_line(text, start));
        let sort = if marker || ordinal || urls.iter().any(|r| r.contains(&start)) {
            Sort::Skip
        } else if money || unit {
            Sort::Marked
        } else {
            Sort::Plain
        };
        out.push(Token {
            text: if negative {
                format!("-{digits}")
            } else {
                digits.to_owned()
            },
            dots,
            whole: digits.split('.').next().and_then(|w| w.parse().ok()),
            sort,
        });
        i = j;
    }
    out
}

/// The values `text` states that need checking: numbers of 10 or more,
/// numbers with a decimal point, money, percents and temperatures. Not:
/// list markers (`1.`, `#1`), ordinals (`2nd`), versions and dates written
/// with dots, numbers in web addresses, or small plain counts. In order,
/// each once.
#[must_use]
pub fn claims(text: &str) -> Vec<String> {
    let text = clean_number_text(text);
    let mut out: Vec<String> = Vec::new();
    for t in tokens(&text, false) {
        if t.sort == Sort::Skip || t.dots > 1 {
            continue;
        }
        let big = t.whole.is_none_or(|w| w >= 10);
        if (t.dots == 1 || big || t.sort == Sort::Marked) && !out.contains(&t.text) {
            out.push(t.text);
        }
    }
    out
}

/// Every number in some texts, as whole tokens: what was read.
#[derive(Debug, Clone, Default)]
pub struct Evidence(HashSet<String>);

impl Evidence {
    /// Nothing read yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds every number in `text` (signed and unsigned; the parts of a
    /// dotted one too).
    pub fn add(&mut self, text: &str) {
        let text = clean_number_text(text);
        for t in tokens(&text, true) {
            let unsigned = t.text.trim_start_matches('-').to_owned();
            if t.dots > 1 {
                for part in unsigned.split('.') {
                    self.0.insert(part.to_owned());
                }
            }
            self.0.insert(unsigned);
            self.0.insert(t.text);
        }
    }

    /// Whether `value` (a [`claims`] item) appears, with or without its
    /// sign.
    #[must_use]
    pub fn has(&self, value: &str) -> bool {
        self.0.contains(value) || self.0.contains(value.trim_start_matches('-'))
    }

    /// The values of `claims` not in it, at most `max`.
    #[must_use]
    pub fn missing(&self, claims: &[String], max: usize) -> Vec<String> {
        claims
            .iter()
            .filter(|c| !self.has(c))
            .take(max)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_are_the_values_worth_checking() {
        assert_eq!(
            claims(
                "1. First Place, 298.3 m (1975)\n2. Tower, 276.9 m\n#3 is Plaza at 275 m.\n\
                 It costs $28.99, or $5 off; 21°C on Nov 14, 2021, 5% less, 3 rooms, the 2nd \
                 and 21st floors, version 1.2.3, see https://shop.example.com/p/12345 or r12 \
                 and uid=3_14."
            ),
            [
                "298.3", "1975", "276.9", "275", "28.99", "5", "21", "14", "2021"
            ]
        );
        // As written: 3.10 stays 3.10, groups are joined, minus is a sign.
        assert_eq!(claims("3.10 and 3.1"), ["3.10", "3.1"]);
        assert_eq!(
            claims("1,234 people; −12 °C; 312.0 m"),
            ["1234", "-12", "312.0"]
        );
        assert_eq!(claims("Low 13°, high 9"), ["13"]);
        assert_eq!(claims("14-15 Nov"), ["14", "15"]);
        assert_eq!(claims("1) one\n2) two"), Vec::<String>::new());
    }

    #[test]
    fn evidence_matches_whole_tokens_only() {
        let mut e = Evidence::new();
        e.add("Height 298.3[3] m · $1,200 · 3.10 · Low −5° · v2026.11.03 · model 125");
        for v in ["298.3", "1200", "3.10", "-5", "5", "2026", "11", "125"] {
            assert!(e.has(v), "{v}");
        }
        for v in ["3.1", "12", "298", "25", "1.2"] {
            assert!(!e.has(v), "{v}");
        }
        assert_eq!(
            e.missing(&["298.3".into(), "13".into(), "14".into()], 1),
            ["13"]
        );
    }
}
