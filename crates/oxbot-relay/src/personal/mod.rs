//! Personal details stay home: before a request leaves the Mac, every
//! personal detail in it is swapped for a stand-in (`[email 1]`,
//! `[phone 1]`, `[address 1]`, `[person 2]`, …), and every stand-in in the
//! model's answer is swapped back before Codex or the person sees it, so a
//! bot can still type the real email into a form (its approval request
//! still asks first).
//!
//! - Caught by shape ([`detect`]): emails, phone numbers (North American
//!   and international), street addresses and PO boxes, Canadian postal
//!   codes and US ZIP codes, card numbers (a card's prefix and the Luhn
//!   check), US social security and Canadian social insurance numbers,
//!   IBANs, and bank or account numbers named as such ("account 12345678").
//! - Caught by name: every person and place oxbot was told ([`Known`]):
//!   each person's whole name and each word of it, the home area and its
//!   city. A name counts with a capital first letter (or exactly as
//!   given), as a whole word ("Sam's" yes, "Samsung" no).
//! - Not caught: details only described in words ("my son's school near
//!   the lake"), names oxbot was never told, a name typed all in lower
//!   case.
//!
//! Stand-ins are numbered per kind and kept for the whole chat
//! ([`StandIns`], saved next to what was sent), so the same detail gets
//! the same stand-in in every request. Text that already reads like a
//! stand-in is itself swapped for one that means it, so the model never
//! sees two things under one name.

mod detect;
mod stream;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use stream::SseRestorer;

use detect::{STAND_IN, Span, rank};

/// What a stand-in stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An email address.
    Email,
    /// A phone number.
    Phone,
    /// A payment card number.
    Card,
    /// A bank or other account number.
    Account,
    /// A government ID number (SIN, SSN).
    IdNumber,
    /// A street address or PO box.
    Address,
    /// A postal or ZIP code.
    PostalCode,
    /// A person oxbot knows.
    Person,
    /// A place oxbot knows (the home area).
    Place,
}

impl Kind {
    /// Its word in a stand-in: `[<word> 1]`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Phone => "phone",
            Self::Card => "card",
            Self::Account => "account",
            Self::IdNumber => "id number",
            Self::Address => "address",
            Self::PostalCode => "postal code",
            Self::Person => "person",
            Self::Place => "place",
        }
    }

    /// Its name in counts of what was swapped, e.g. `postal_code`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::IdNumber => "id_number",
            Self::PostalCode => "postal_code",
            other => other.word(),
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        Some(match word.to_ascii_lowercase().as_str() {
            "email" => Self::Email,
            "phone" => Self::Phone,
            "card" => Self::Card,
            "account" => Self::Account,
            "id number" => Self::IdNumber,
            "address" => Self::Address,
            "postal code" => Self::PostalCode,
            "person" => Self::Person,
            "place" => Self::Place,
            _ => return None,
        })
    }
}

/// A name oxbot knows: a person's (as the people list has it) or a
/// place's (the home area).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Known {
    /// As oxbot has it, e.g. `Ann Lee` or `Toronto, ON`.
    pub text: String,
    /// [`Kind::Person`] or [`Kind::Place`].
    pub kind: Kind,
}

impl Known {
    /// A person's name.
    #[must_use]
    pub fn person(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            kind: Kind::Person,
        }
    }

    /// A place's name.
    #[must_use]
    pub fn place(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            kind: Kind::Place,
        }
    }

    /// Every form it's caught in: a person's whole name and each word of
    /// it; a place's whole name and its first part (`Toronto`).
    fn forms(&self) -> Vec<String> {
        let clean = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_owned()
        };
        let whole = clean(&self.text);
        let mut forms = vec![whole.clone()];
        match self.kind {
            Kind::Place => forms.extend(whole.split(',').next().map(clean)),
            _ => forms.extend(whole.split_whitespace().map(clean)),
        }
        forms
            .into_iter()
            .filter(|f| f.chars().count() >= 2 && f.chars().any(char::is_alphabetic))
            .collect()
    }
}

/// The names oxbot knows, as one pattern.
#[derive(Debug, Clone, Default)]
struct Names {
    known: Vec<Known>,
    re: Option<Regex>,
    /// Each form, lower case → its kind.
    kinds: HashMap<String, Kind>,
    /// Each form as given.
    exact: HashSet<String>,
}

impl Names {
    fn new(known: Vec<Known>) -> Self {
        let mut kinds = HashMap::new();
        let mut exact = HashSet::new();
        let mut forms: Vec<String> = Vec::new();
        for k in &known {
            for f in k.forms() {
                if kinds.insert(f.to_lowercase(), k.kind).is_none() {
                    forms.push(f.clone());
                }
                exact.insert(f);
            }
        }
        // Longest first: `Ann Lee` before `Ann`.
        forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        let re = (!forms.is_empty())
            .then(|| {
                let alts: Vec<String> = forms.iter().map(|f| regex::escape(f)).collect();
                Regex::new(&format!(r"(?i)\b(?:{})\b", alts.join("|"))).ok()
            })
            .flatten();
        Self {
            known,
            re,
            kinds,
            exact,
        }
    }

    fn find(&self, text: &str, out: &mut Vec<Span>) {
        let Some(re) = &self.re else { return };
        for m in re.find_iter(text) {
            let s = m.as_str();
            let capital = s.chars().next().is_some_and(char::is_uppercase);
            if (capital || self.exact.contains(s))
                && let Some(&kind) = self.kinds.get(&s.to_lowercase())
            {
                out.push(Span::new(m.start(), m.end(), kind, rank::NAME));
            }
        }
    }
}

/// One stand-in and what it stands for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    kind: Kind,
    n: u32,
    /// As first seen (what the stand-in turns back into).
    value: String,
    /// The same detail written another way finds it: `Ann@X.com`, `ann@x.com`.
    key: String,
}

/// How a chat's stand-ins are kept on disk.
#[derive(Serialize, Deserialize)]
struct Saved {
    stand_ins: Vec<Entry>,
}

/// A chat's stand-ins: the same detail always gets the same one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StandIns {
    entries: Vec<Entry>,
    by_key: HashMap<(Kind, String), usize>,
    by_tag: HashMap<(Kind, u32), usize>,
}

/// The same detail written another way, for finding its stand-in.
fn key_of(kind: Kind, value: &str) -> String {
    let only = |keep: fn(&char) -> bool| -> String {
        value
            .chars()
            .filter(keep)
            .map(|c| c.to_ascii_uppercase())
            .collect()
    };
    match kind {
        Kind::Email => value.to_lowercase(),
        Kind::Phone => {
            let d = only(char::is_ascii_digit);
            match d.strip_prefix('1') {
                Some(rest) if d.len() == 11 => rest.to_owned(),
                _ => d,
            }
        }
        Kind::Card | Kind::IdNumber | Kind::Account | Kind::PostalCode => {
            only(char::is_ascii_alphanumeric)
        }
        Kind::Address | Kind::Person | Kind::Place => value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches('.')
            .to_lowercase(),
    }
}

/// A stand-in's text.
fn tag(kind: Kind, n: u32) -> String {
    format!("[{} {n}]", kind.word())
}

impl StandIns {
    /// Read back from [`Self::to_json`].
    ///
    /// # Errors
    /// Not a saved set of stand-ins.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let saved: Saved = serde_json::from_str(json)?;
        let mut out = Self::default();
        for e in saved.stand_ins {
            if !out.by_tag.contains_key(&(e.kind, e.n))
                && !out.by_key.contains_key(&(e.kind, e.key.clone()))
            {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// For keeping on disk.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(&Saved {
            stand_ins: self.entries.clone(),
        })
        .unwrap_or_default()
    }

    /// How many there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What `[<kind> <n>]` stands for.
    #[must_use]
    pub fn value(&self, kind: Kind, n: u32) -> Option<&str> {
        self.by_tag
            .get(&(kind, n))
            .map(|&i| self.entries[i].value.as_str())
    }

    fn push(&mut self, e: Entry) {
        let i = self.entries.len();
        self.by_key.insert((e.kind, e.key.clone()), i);
        self.by_tag.insert((e.kind, e.n), i);
        self.entries.push(e);
    }

    fn next(&self, kind: Kind) -> u32 {
        self.entries
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.n)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// `value`'s stand-in, made if it has none yet.
    fn stand_in(&mut self, kind: Kind, value: &str) -> String {
        let key = key_of(kind, value);
        if let Some(&i) = self.by_key.get(&(kind, key.clone())) {
            return tag(kind, self.entries[i].n);
        }
        let n = self.next(kind);
        self.push(Entry {
            kind,
            n,
            value: value.to_owned(),
            key,
        });
        tag(kind, n)
    }

    /// The stand-in for text that already reads like stand-in `n`: that
    /// very number if it's free (it then means itself), else a new one.
    fn literal(&mut self, kind: Kind, n: u32, text: &str) -> String {
        let key = format!("\u{0}{}", text.to_lowercase());
        if let Some(&i) = self.by_key.get(&(kind, key.clone())) {
            return tag(kind, self.entries[i].n);
        }
        let n = if self.by_tag.contains_key(&(kind, n)) {
            self.next(kind)
        } else {
            n
        };
        self.push(Entry {
            kind,
            n,
            value: text.to_owned(),
            key,
        });
        tag(kind, n)
    }

    /// `text` with every stand-in turned back into its detail; `json`:
    /// written for the inside of a JSON string (function call arguments).
    fn restore_with(&self, text: &str, json: bool) -> String {
        if self.is_empty() || !text.contains('[') {
            return text.to_owned();
        }
        STAND_IN
            .replace_all(text, |c: &regex::Captures<'_>| {
                let found = Kind::from_word(&c[1])
                    .zip(c[2].parse::<u32>().ok())
                    .and_then(|(kind, n)| self.value(kind, n));
                match found {
                    Some(v) if json => {
                        // `"…"`: the inside, escaped.
                        let quoted = serde_json::to_string(v).unwrap_or_default();
                        quoted
                            .strip_prefix('"')
                            .and_then(|q| q.strip_suffix('"'))
                            .unwrap_or_default()
                            .to_owned()
                    }
                    Some(v) => v.to_owned(),
                    None => c[0].to_owned(),
                }
            })
            .into_owned()
    }

    /// `text` with every stand-in turned back into its detail.
    #[must_use]
    pub fn restore(&self, text: &str) -> String {
        self.restore_with(text, false)
    }

    /// The same, for text inside a JSON string: details are escaped.
    #[must_use]
    pub fn restore_in_json_string(&self, text: &str) -> String {
        self.restore_with(text, true)
    }

    /// Every string in `v` restored (`arguments` as the JSON it holds).
    pub fn restore_value(&self, v: &mut Value) {
        self.restore_value_at(v, None);
    }

    fn restore_value_at(&self, v: &mut Value, key: Option<&str>) {
        match v {
            Value::String(s) => {
                *s = self.restore_with(s, key == Some("arguments"));
            }
            Value::Array(items) => {
                for x in items {
                    self.restore_value_at(x, key);
                }
            }
            Value::Object(map) => {
                for (k, x) in map.iter_mut() {
                    self.restore_value_at(x, Some(k));
                }
            }
            _ => {}
        }
    }

    /// A whole (not streamed) answer restored: JSON if it is, else text.
    #[must_use]
    pub fn restore_body(&self, body: &[u8]) -> Vec<u8> {
        if self.is_empty() {
            return body.to_vec();
        }
        if let Ok(mut v) = serde_json::from_slice::<Value>(body) {
            self.restore_value(&mut v);
            return serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec());
        }
        match std::str::from_utf8(body) {
            Ok(text) => self.restore(text).into_bytes(),
            Err(_) => body.to_vec(),
        }
    }
}

/// Fields of a request's conversation that aren't text anyone wrote:
/// kinds, ids, tool names, the model's sealed reasoning, file data.
const NOT_TEXT: [&str; 14] = [
    "type",
    "role",
    "id",
    "call_id",
    "name",
    "status",
    "encrypted_content",
    "image_url",
    "file_data",
    "file_id",
    "file_url",
    "detail",
    "format",
    "server_label",
];

/// Swaps personal details for stand-ins, keeping a chat's stand-ins.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    names: Names,
    stand_ins: StandIns,
    /// The stand-ins used since the last [`Redactor::redact_request`]
    /// began, for counting what it swapped.
    used: std::collections::BTreeSet<(Kind, String)>,
}

impl Redactor {
    /// A redactor for a new chat that knows these names.
    #[must_use]
    pub fn new(known: Vec<Known>) -> Self {
        Self::resume(known, StandIns::default())
    }

    /// A redactor for a chat that has stand-ins already.
    #[must_use]
    pub fn resume(known: Vec<Known>, stand_ins: StandIns) -> Self {
        Self {
            names: Names::new(known),
            stand_ins,
            used: std::collections::BTreeSet::new(),
        }
    }

    /// The names it knows changed (someone joined, the home area moved).
    pub fn set_known(&mut self, known: Vec<Known>) {
        if known != self.names.known {
            self.names = Names::new(known);
        }
    }

    /// The chat's stand-ins so far.
    #[must_use]
    pub fn stand_ins(&self) -> &StandIns {
        &self.stand_ins
    }

    /// `text` with each personal detail swapped for its stand-in.
    pub fn redact(&mut self, text: &str) -> String {
        let mut spans = Vec::new();
        detect::shaped(text, &mut spans);
        self.names.find(text, &mut spans);
        let spans = detect::settle(spans);
        if spans.is_empty() {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for s in spans {
            out.push_str(&text[at..s.start]);
            let found = &text[s.start..s.end];
            let stand_in = match s.literal {
                Some(n) => self.stand_ins.literal(s.kind, n, found),
                None => self.stand_ins.stand_in(s.kind, found),
            };
            out.push_str(&stand_in);
            self.used.insert((s.kind, stand_in));
            at = s.end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// `text` with every stand-in turned back.
    #[must_use]
    pub fn restore(&self, text: &str) -> String {
        self.stand_ins.restore(text)
    }

    /// What the last [`Redactor::redact_request`] swapped: each kind
    /// ([`Kind::key`]) → how many different details of it. Never the
    /// details.
    #[must_use]
    pub fn swapped(&self) -> std::collections::BTreeMap<String, u32> {
        let mut counts = std::collections::BTreeMap::new();
        for (kind, _) in &self.used {
            *counts.entry(kind.key().to_owned()).or_insert(0) += 1;
        }
        counts
    }

    /// Function call arguments: the strings in the JSON they hold, so
    /// details stay valid JSON; plain text if they aren't JSON.
    fn redact_arguments(&mut self, text: &str) -> String {
        match serde_json::from_str::<Value>(text) {
            Ok(mut v) if v.is_object() || v.is_array() => {
                self.redact_strings(&mut v);
                serde_json::to_string(&v).unwrap_or_else(|_| self.redact(text))
            }
            _ => self.redact(text),
        }
    }

    /// Every string in `v`, keys aside.
    fn redact_strings(&mut self, v: &mut Value) {
        match v {
            Value::String(s) => *s = self.redact(s),
            Value::Array(items) => items.iter_mut().for_each(|x| self.redact_strings(x)),
            Value::Object(map) => map.values_mut().for_each(|x| self.redact_strings(x)),
            _ => {}
        }
    }

    /// Every text a person, a page or a model wrote in `v`.
    fn redact_value(&mut self, v: &mut Value, key: Option<&str>) {
        match v {
            Value::String(s) if key == Some("arguments") => *s = self.redact_arguments(s),
            Value::String(s) => *s = self.redact(s),
            Value::Array(items) => {
                for x in items {
                    self.redact_value(x, key);
                }
            }
            Value::Object(map) => {
                for (k, x) in map.iter_mut() {
                    if !NOT_TEXT.contains(&k.as_str()) {
                        self.redact_value(x, Some(k));
                    }
                }
            }
            _ => {}
        }
    }

    /// A Responses API request's text fields redacted in place: the
    /// instructions and the whole conversation (messages, tool results
    /// such as pages and files, function call arguments, reasoning
    /// summaries). Tool definitions are oxbot's own and stay.
    pub fn redact_request(&mut self, request: &mut Map<String, Value>) {
        self.used.clear();
        for key in ["instructions", "input"] {
            if let Some(v) = request.get_mut(key) {
                self.redact_value(v, Some(key));
            }
        }
    }
}
