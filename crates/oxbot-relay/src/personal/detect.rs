//! Finding personal details in text by their shape: emails, phone numbers,
//! card numbers (Luhn-checked), government ID numbers, bank and account
//! numbers named as such, street addresses, postal codes and ZIP codes,
//! and stand-ins already in the text. Names oxbot knows are found by
//! [`super::Names`]. Every pattern is compiled once.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use super::Kind;

/// One detail found: its byte range and kind. `literal` is set for text
/// that already reads like a stand-in (`[email 1]`), with its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
    pub literal: Option<u32>,
    rank: u8,
}

impl Span {
    pub(crate) fn new(start: usize, end: usize, kind: Kind, rank: u8) -> Self {
        Self {
            start,
            end,
            kind,
            literal: None,
            rank,
        }
    }
}

/// Which wins when two finds overlap (lower first, then the longer).
pub(crate) mod rank {
    pub const LITERAL: u8 = 0;
    pub const EMAIL: u8 = 1;
    pub const CARD: u8 = 2;
    pub const ID: u8 = 3;
    pub const ACCOUNT: u8 = 4;
    pub const PHONE: u8 = 5;
    pub const ADDRESS: u8 = 6;
    pub const POSTAL: u8 = 7;
    pub const NAME: u8 = 8;
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|err| panic!("a personal-detail pattern: {err}"))
}

/// A stand-in: `[email 1]`, `[postal code 2]`, any case.
pub(crate) static STAND_IN: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\[(email|phone|card|account|id number|address|postal code|person|place) ([0-9]{1,6})\]",
    )
});

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)*\.[A-Za-z]{2,24}\b",
    )
});

/// What follows `@` in file names, not mail domains (`logo@2x.png`).
const FILE_ENDINGS: [&str; 9] = [
    "png", "jpg", "jpeg", "gif", "svg", "webp", "js", "css", "avif",
];

static CARD: LazyLock<Regex> = LazyLock::new(|| re(r"\b[0-9](?:[ -]?[0-9]){12,18}\b"));

static SSN: LazyLock<Regex> = LazyLock::new(|| re(r"\b[0-9]{3}-[0-9]{2}-[0-9]{4}\b"));

static SIN: LazyLock<Regex> = LazyLock::new(|| re(r"\b[0-9]{3}[ -][0-9]{3}[ -][0-9]{3}\b"));

static ID_NAMED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:ssn|sin|social security(?: number| no\.?)?|social insurance(?: number| no\.?)?)\b[^0-9\n]{0,15}\b([0-9]{3}[ -]?[0-9]{2,3}[ -]?[0-9]{3,4})\b",
    )
});

static ACCOUNT_NAMED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:bank account|account|acct|a/c|routing|transit|institution|iban|chequing|checking|savings|sort code)\b(?:\s+(?:number|no\.?|num|#))?\s*[:#]?\s*(?-i:([A-Z]{2}[0-9]{2}(?: ?[A-Z0-9]){8,30}|[0-9](?:[ -]?[0-9]){3,30}))",
    )
});

static IBAN: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b[A-Z]{2}[0-9]{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,3})?\b"));

static PHONE_NANP: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:\+1[ .-]?|\b1[ .-])?(?:\([0-9]{3}\)[ .-]?|\b[0-9]{3}[ .-])[0-9]{3}[ .-][0-9]{4}\b")
});

static PHONE_INTL: LazyLock<Regex> =
    LazyLock::new(|| re(r"\+[0-9]{1,3}(?:[ .-]?\(?[0-9]{1,5}\)?){2,6}\b"));

static PHONE_NAMED: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:phone|tel|telephone|cell|mobile|call|text|sms|fax|whatsapp)\b[^0-9\n]{0,20}(\+?[0-9]{10,13})\b",
    )
});

/// Street types, as written in addresses (title case or capitals).
const STREET_TYPES: [&str; 34] = [
    "Street",
    "St",
    "Avenue",
    "Ave",
    "Road",
    "Rd",
    "Boulevard",
    "Blvd",
    "Drive",
    "Dr",
    "Lane",
    "Ln",
    "Court",
    "Ct",
    "Crescent",
    "Cres",
    "Place",
    "Pl",
    "Way",
    "Terrace",
    "Circle",
    "Cir",
    "Parkway",
    "Pkwy",
    "Highway",
    "Hwy",
    "Square",
    "Sq",
    "Trail",
    "Trl",
    "Grove",
    "Gardens",
    "Heights",
    "Close",
];

static ADDRESS: LazyLock<Regex> = LazyLock::new(|| {
    let types: Vec<String> = STREET_TYPES
        .iter()
        .flat_map(|t| [(*t).to_owned(), t.to_uppercase()])
        .collect();
    re(&format!(
        r"\b(?:[0-9]{{1,5}} ?- ?)?[0-9]{{1,6}}[A-Za-z]?,? +(?:(?:[A-Z][A-Za-z'.-]*|[0-9]{{1,3}}(?:st|nd|rd|th)) +){{1,4}}(?:{})\b\.?(?: +(?:North|South|East|West|NE|NW|SE|SW|N|S|E|W)\b)?(?:,? *(?:Apt|Apartment|Unit|Suite|Ste|#) *\.? *[A-Za-z0-9-]+)?",
        types.join("|")
    ))
});

static PO_BOX: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(?:p\.? ?o\.? ?box|post office box) *#? *[0-9]{1,6}\b"));

static POSTAL_CA: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b[ABCEGHJ-NPRSTVXY][0-9][ABCEGHJ-NPRSTV-Z] ?[0-9][ABCEGHJ-NPRSTV-Z][0-9]\b")
});

static ZIP4: LazyLock<Regex> = LazyLock::new(|| re(r"\b[0-9]{5}-[0-9]{4}\b"));

static ZIP_STATE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\b(?:AL|AK|AZ|AR|CA|CO|CT|DE|DC|FL|GA|HI|ID|IL|IN|IA|KS|KY|LA|ME|MD|MA|MI|MN|MS|MO|MT|NE|NV|NH|NJ|NM|NY|NC|ND|OH|OK|OR|PA|RI|SC|SD|TN|TX|UT|VT|VA|WA|WV|WI|WY),? +([0-9]{5}(?:-[0-9]{4})?)\b",
    )
});

/// Whether `start..end` is part of something longer (a version number,
/// an order number, a path): a letter or digit next to it, or one past a
/// `-`, `.`, `/` or `_`.
fn glued(text: &str, start: usize, end: usize) -> bool {
    let joins = |c: char| matches!(c, '-' | '.' | '/' | '_');
    let mut before = text[..start].chars().rev();
    if let Some(c) = before.next()
        && (c.is_alphanumeric() || (joins(c) && before.next().is_some_and(char::is_alphanumeric)))
    {
        return true;
    }
    let mut after = text[end..].chars();
    if let Some(c) = after.next()
        && (c.is_alphanumeric() || (joins(c) && after.next().is_some_and(char::is_alphanumeric)))
    {
        return true;
    }
    false
}

/// The digits in `text`, as numbers.
fn digits(text: &str) -> Vec<u8> {
    text.bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect()
}

/// The Luhn check card numbers and Canadian SINs pass.
pub(crate) fn luhn(digits: &[u8]) -> bool {
    if digits.len() < 2 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            let d = u32::from(d);
            if i % 2 == 1 {
                if d * 2 > 9 { d * 2 - 9 } else { d * 2 }
            } else {
                d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

/// Whether `d` starts like a card a bank issues, at its length.
fn card_prefix(d: &[u8]) -> bool {
    let len = d.len();
    let p = |n: usize| d[..n].iter().fold(0u32, |a, &x| a * 10 + u32::from(x));
    match d[0] {
        4 => matches!(len, 13 | 16 | 19),
        5 => (51..=55).contains(&p(2)) && len == 16,
        2 => (2221..=2720).contains(&p(4)) && len == 16,
        3 => {
            (matches!(p(2), 34 | 37) && len == 15)
                || ((3528..=3589).contains(&p(4)) && (16..=19).contains(&len))
                || ((matches!(p(2), 36 | 38 | 39) || (300..=305).contains(&p(3)))
                    && (14..=19).contains(&len))
        }
        6 => {
            (p(4) == 6011 || p(2) == 65 || p(2) == 62 || (644..=649).contains(&p(3)))
                && (16..=19).contains(&len)
        }
        _ => false,
    }
}

/// A card number: 13 to 19 digits, grouped one way, a card's prefix, and
/// the Luhn check.
fn is_card(text: &str) -> bool {
    let d = digits(text);
    let mut seps = text.bytes().filter(|b| !b.is_ascii_digit());
    let first = seps.next();
    (13..=19).contains(&d.len()) && seps.all(|b| Some(b) == first) && card_prefix(&d) && luhn(&d)
}

/// A US social security number's parts as the SSA issues them.
fn is_ssn(text: &str) -> bool {
    let d = digits(text);
    let area = u32::from(d[0]) * 100 + u32::from(d[1]) * 10 + u32::from(d[2]);
    area != 0 && area != 666 && area < 900 && d[3..5] != [0, 0] && d[5..] != [0, 0, 0, 0]
}

/// A Canadian social insurance number: grouped one way, Luhn-checked.
fn is_sin(text: &str) -> bool {
    let d = digits(text);
    let seps: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_digit()).collect();
    d.len() == 9 && seps.windows(2).all(|w| w[0] == w[1]) && !matches!(d[0], 0 | 8) && luhn(&d)
}

/// A North American number: area code and exchange don't start with 0
/// or 1.
fn is_nanp(text: &str) -> bool {
    let mut d = digits(text);
    if d.len() == 11 && d[0] == 1 {
        d.remove(0);
    }
    d.len() == 10 && d[0] >= 2 && d[3] >= 2
}

/// An IBAN: its check digits work out (ISO 13616, mod 97).
fn is_iban(text: &str) -> bool {
    let s: Vec<u8> = text.bytes().filter(u8::is_ascii_alphanumeric).collect();
    if !(15..=34).contains(&s.len()) {
        return false;
    }
    let mut rem: u32 = 0;
    for &b in s[4..].iter().chain(&s[..4]) {
        let v = if b.is_ascii_digit() {
            u32::from(b - b'0')
        } else {
            u32::from(b - b'A') + 10
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

/// Every detail `text` has by its shape, overlapping finds included.
pub(crate) fn shaped(text: &str, out: &mut Vec<Span>) {
    for m in STAND_IN.captures_iter(text) {
        let (Some(all), Some(kind), Some(n)) = (m.get(0), m.get(1), m.get(2)) else {
            continue;
        };
        if let (Some(kind), Ok(n)) = (Kind::from_word(kind.as_str()), n.as_str().parse()) {
            out.push(Span {
                literal: Some(n),
                ..Span::new(all.start(), all.end(), kind, rank::LITERAL)
            });
        }
    }
    for m in EMAIL.find_iter(text) {
        let tld = m.as_str().rsplit('.').next().unwrap_or_default();
        if !FILE_ENDINGS.contains(&tld.to_ascii_lowercase().as_str()) {
            out.push(Span::new(m.start(), m.end(), Kind::Email, rank::EMAIL));
        }
    }
    let mut add = |re: &Regex, kind: Kind, rank: u8, ok: &dyn Fn(&str) -> bool| {
        for m in re.find_iter(text) {
            if !glued(text, m.start(), m.end()) && ok(m.as_str()) {
                out.push(Span::new(m.start(), m.end(), kind, rank));
            }
        }
    };
    add(&CARD, Kind::Card, rank::CARD, &is_card);
    add(&SSN, Kind::IdNumber, rank::ID, &is_ssn);
    add(&SIN, Kind::IdNumber, rank::ID, &is_sin);
    add(&IBAN, Kind::Account, rank::ACCOUNT, &is_iban);
    add(&PHONE_NANP, Kind::Phone, rank::PHONE, &is_nanp);
    add(&PHONE_INTL, Kind::Phone, rank::PHONE, &|t| {
        (8..=15).contains(&digits(t).len())
    });
    add(&ZIP4, Kind::PostalCode, rank::POSTAL, &|_| true);
    for m in ADDRESS.find_iter(text).chain(PO_BOX.find_iter(text)) {
        out.push(Span::new(m.start(), m.end(), Kind::Address, rank::ADDRESS));
    }
    for m in POSTAL_CA.find_iter(text) {
        out.push(Span::new(
            m.start(),
            m.end(),
            Kind::PostalCode,
            rank::POSTAL,
        ));
    }
    let mut named = |re: &Regex, kind: Kind, rank: u8, ok: &dyn Fn(&str) -> bool| {
        for c in re.captures_iter(text) {
            if let Some(m) = c.get(1)
                && !glued(text, m.start(), m.end())
                && ok(m.as_str())
            {
                out.push(Span::new(m.start(), m.end(), kind, rank));
            }
        }
    };
    named(&ID_NAMED, Kind::IdNumber, rank::ID, &|t| {
        digits(t).len() == 9
    });
    named(&ACCOUNT_NAMED, Kind::Account, rank::ACCOUNT, &|t| {
        digits(t).len() >= 5
    });
    named(&PHONE_NAMED, Kind::Phone, rank::PHONE, &|t| {
        let n = digits(t).len();
        n > 11 || is_nanp(t)
    });
    named(&ZIP_STATE, Kind::PostalCode, rank::POSTAL, &|_| true);
}

/// `spans` without overlaps (the better find wins: see [`rank`]), in
/// text order.
pub(crate) fn settle(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|s| (s.rank, std::cmp::Reverse(s.end - s.start), s.start));
    let mut kept: BTreeMap<usize, Span> = BTreeMap::new();
    for s in spans {
        let clashes = kept
            .range(..s.end)
            .next_back()
            .is_some_and(|(_, k)| k.end > s.start);
        if !clashes {
            kept.insert(s.start, s);
        }
    }
    kept.into_values().collect()
}
