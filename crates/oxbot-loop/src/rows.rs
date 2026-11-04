//! Rows as data: a table or list read off a page ([`Dataset`]), and
//! `query_rows` over it, run in Rust with no model call. Models are bad at
//! counting, ranking and pairing columns by eye; a filter and a sort here
//! are exact, and the answer says exactly what was asked.
//!
//! Everything is deterministic: the same dataset and query give the same
//! bytes.

use std::cmp::Ordering;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Rows `query_rows` shows by default.
pub const DEFAULT_LIMIT: usize = 20;
/// Most rows `query_rows` shows.
pub const MAX_LIMIT: usize = 50;
/// Rows shown with each dataset by `extract_rows`.
pub const PREVIEW_ROWS: usize = 5;
/// Longest cell kept.
pub const CELL_CAP: usize = 500;
/// Longest cell shown in a table.
const SHOWN_CELL: usize = 120;
/// Most conditions and sort keys.
const MAX_TERMS: usize = 12;

/// A table or list read off a stored page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dataset {
    /// Format version.
    pub v: u32,
    /// `table`, `grid` or `list`.
    pub kind: String,
    /// The element it was read from.
    pub uid: String,
    /// The stored page it was read from (`r12`).
    pub page: String,
    /// The page's address.
    pub url: String,
    /// Its caption or name, if it has one.
    pub caption: String,
    /// Where it is on the page (`main › "Tallest buildings" › "Under
    /// construction"`).
    pub section: String,
    /// Column names, unique.
    pub columns: Vec<String>,
    /// The rows, one value per column.
    pub rows: Vec<Vec<String>>,
    /// Rows (0-based) that had fewer cells than columns (row spans): their
    /// values may be shifted.
    pub short: Vec<usize>,
}

impl Dataset {
    /// The dataset as stored (JSON).
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// A stored dataset.
    ///
    /// # Errors
    /// What to tell the model.
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|_| "that table couldn't be read back".to_owned())
    }

    /// The index of column `name` (any case).
    fn column(&self, name: &str) -> Result<usize, String> {
        let wanted = name.trim();
        let lower = wanted.to_lowercase();
        self.columns
            .iter()
            .position(|c| c.to_lowercase() == lower)
            .ok_or_else(|| {
                format!(
                    "no column \"{}\"; the columns are: {}",
                    wanted.chars().take(40).collect::<String>(),
                    self.columns.join(" | ")
                )
            })
    }

    /// One line about the dataset, as `extract_rows` lists it.
    #[must_use]
    pub fn summary(&self, handle: &str) -> String {
        let mut s = format!(
            "{handle} · {} · {} {} · columns: {}",
            self.kind,
            self.rows.len(),
            if self.rows.len() == 1 { "row" } else { "rows" },
            self.columns.join(" | ")
        );
        if !self.section.is_empty() {
            let _ = write!(s, " · section: {}", self.section);
        }
        if !self.caption.is_empty() {
            let _ = write!(s, " · caption: {}", cut(&self.caption, 120));
        }
        s
    }

    /// The note on short rows, if any.
    #[must_use]
    pub fn short_note(&self) -> Option<String> {
        match self.short.len() {
            0 => None,
            1 => Some("1 row is short; its values may be shifted.".to_owned()),
            n => Some(format!("{n} rows are short; values may be shifted.")),
        }
    }

    /// The first rows as a Markdown table.
    #[must_use]
    pub fn preview(&self, n: usize) -> String {
        let rows: Vec<Vec<String>> = self.rows.iter().take(n).cloned().collect();
        markdown(&self.columns, &rows)
    }
}

/// `s` cut to `max` chars with `…` added when cut.
fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// A Markdown table of `rows` under `columns` (cells cut and escaped).
#[must_use]
pub fn markdown(columns: &[String], rows: &[Vec<String>]) -> String {
    let cell = |v: &str| {
        let v = cut(v, SHOWN_CELL).replace('|', "\\|");
        if v.is_empty() { " ".to_owned() } else { v }
    };
    let mut out = String::new();
    out.push('|');
    for c in columns {
        let _ = write!(out, " {} |", cell(c));
    }
    out.push_str("\n|");
    for _ in columns {
        out.push_str("---|");
    }
    for r in rows {
        out.push_str("\n|");
        for (i, _) in columns.iter().enumerate() {
            let _ = write!(out, " {} |", cell(r.get(i).map_or("", String::as_str)));
        }
    }
    out
}

/// The first number in a cell: thousands separators and thin spaces
/// dropped, `−` read as minus, units, `~` and currency signs ignored.
#[must_use]
pub fn number_in(cell: &str) -> Option<f64> {
    let text = clean_number_text(cell);
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let negative =
                i > 0 && b[i - 1] == b'-' && (i < 2 || !b[i - 2].is_ascii_alphanumeric());
            let start = if negative { i - 1 } else { i };
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j + 1 < b.len() && b[j] == b'.' && b[j + 1].is_ascii_digit() {
                j += 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            }
            return text[start..j].parse().ok();
        }
        i += 1;
    }
    None
}

/// Text with digit groups joined: `1,234` → `1234`, `1 234` (thin or
/// non-breaking space) → `1234`, `−` → `-`, `~` dropped.
#[must_use]
pub fn clean_number_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        let between_digits = i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit();
        match c {
            // A comma between digits groups thousands only when three
            // digits follow it.
            ',' if between_digits && three_digits_after(&chars, i) => {}
            '\u{2009}' | '\u{202F}' | '\u{00A0}' if between_digits => {}
            '\u{2212}' => out.push('-'),
            '~' => {}
            _ => out.push(c),
        }
    }
    out
}

fn three_digits_after(chars: &[char], i: usize) -> bool {
    let run = chars[i + 1..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    run == 3
}

/// A condition's test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Same text (any case).
    Eq,
    /// Not the same text.
    Ne,
    /// Has the text.
    Contains,
    /// Hasn't the text.
    NotContains,
    /// A number above.
    Gt,
    /// A number at least.
    Ge,
    /// A number below.
    Lt,
    /// A number at most.
    Le,
    /// Nothing in it.
    Empty,
    /// Something in it.
    NotEmpty,
}

impl Op {
    const ALL: [(&'static str, Self); 10] = [
        ("eq", Self::Eq),
        ("ne", Self::Ne),
        ("contains", Self::Contains),
        ("not_contains", Self::NotContains),
        ("gt", Self::Gt),
        ("ge", Self::Ge),
        ("lt", Self::Lt),
        ("le", Self::Le),
        ("empty", Self::Empty),
        ("not_empty", Self::NotEmpty),
    ];

    fn parse(name: &str) -> Result<Self, String> {
        let name = name.trim().to_lowercase();
        Self::ALL
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, op)| *op)
            .ok_or_else(|| {
                let names: Vec<&str> = Self::ALL.iter().map(|(n, _)| *n).collect();
                format!(
                    "op \"{}\" isn't known; use one of: {}",
                    name.chars().take(20).collect::<String>(),
                    names.join(", ")
                )
            })
    }

    fn name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, op)| *op == self)
            .map_or("", |(n, _)| n)
    }

    fn numeric(self) -> bool {
        matches!(self, Self::Gt | Self::Ge | Self::Lt | Self::Le)
    }

    fn needs_value(self) -> bool {
        !matches!(self, Self::Empty | Self::NotEmpty)
    }
}

/// One condition.
#[derive(Debug, Clone, PartialEq)]
struct Cond {
    col: usize,
    op: Op,
    text: String,
    number: f64,
}

/// One sort key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SortKey {
    col: usize,
    desc: bool,
}

/// A parsed `query_rows` call.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    conds: Vec<Cond>,
    sort: Vec<SortKey>,
    limit: usize,
    columns: Vec<usize>,
}

impl Query {
    /// The query in `args` (`where`, `sort`, `limit`, `columns`) for `data`.
    ///
    /// # Errors
    /// What to tell the model: an unknown column (with the list), op or
    /// a missing value.
    pub fn parse(args: &Value, data: &Dataset) -> Result<Self, String> {
        let mut conds = Vec::new();
        for c in list(args.get("where")).into_iter().take(MAX_TERMS) {
            let col_name = c
                .get("col")
                .or_else(|| c.get("column"))
                .and_then(Value::as_str)
                .ok_or("each where condition needs col and op")?;
            let col = data.column(col_name)?;
            let op = Op::parse(
                c.get("op")
                    .and_then(Value::as_str)
                    .ok_or("each where condition needs op")?,
            )?;
            let text = match c.get("value") {
                Some(Value::String(s)) => s.trim().to_owned(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                _ => String::new(),
            };
            if op.needs_value() && text.is_empty() {
                return Err(format!("op {} needs a value", op.name()));
            }
            let number = if op.numeric() {
                number_in(&text)
                    .ok_or_else(|| format!("op {} needs a number, not \"{text}\"", op.name()))?
            } else {
                0.0
            };
            conds.push(Cond {
                col,
                op,
                text,
                number,
            });
        }
        let mut sort = Vec::new();
        for s in list(args.get("sort")).into_iter().take(MAX_TERMS) {
            let (name, desc) = match &s {
                Value::String(name) => (name.as_str(), false),
                other => (
                    other
                        .get("col")
                        .or_else(|| other.get("column"))
                        .and_then(Value::as_str)
                        .ok_or("each sort key needs col")?,
                    other.get("desc").and_then(Value::as_bool).unwrap_or(false),
                ),
            };
            sort.push(SortKey {
                col: data.column(name)?,
                desc,
            });
        }
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);
        let mut columns = Vec::new();
        for c in list(args.get("columns")) {
            let name = c.as_str().ok_or("columns are names")?;
            let i = data.column(name)?;
            if !columns.contains(&i) {
                columns.push(i);
            }
        }
        if columns.is_empty() {
            columns = (0..data.columns.len()).collect();
        }
        Ok(Self {
            conds,
            sort,
            limit,
            columns,
        })
    }

    /// The query in words: `where Year not_empty, sorted by Height m desc,
    /// first 5`.
    fn words(&self, data: &Dataset) -> String {
        let mut parts = Vec::new();
        if !self.conds.is_empty() {
            let conds: Vec<String> = self
                .conds
                .iter()
                .map(|c| {
                    let col = &data.columns[c.col];
                    if c.op.needs_value() {
                        format!("{col} {} \"{}\"", c.op.name(), cut(&c.text, 60))
                    } else {
                        format!("{col} {}", c.op.name())
                    }
                })
                .collect();
            parts.push(format!("where {}", conds.join(" and ")));
        }
        if !self.sort.is_empty() {
            let keys: Vec<String> = self
                .sort
                .iter()
                .map(|k| {
                    format!(
                        "{} {}",
                        data.columns[k.col],
                        if k.desc { "desc" } else { "asc" }
                    )
                })
                .collect();
            parts.push(format!("sorted by {}", keys.join(", then ")));
        }
        parts.push(format!("first {}", self.limit));
        parts.join(", ")
    }
}

/// A value given as one item or a list.
fn list(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![other.clone()],
    }
}

fn matches(cell: &str, c: &Cond) -> Option<bool> {
    let cell_l = cell.trim().to_lowercase();
    let text_l = c.text.to_lowercase();
    Some(match c.op {
        Op::Eq => cell_l == text_l,
        Op::Ne => cell_l != text_l,
        Op::Contains => cell_l.contains(&text_l),
        Op::NotContains => !cell_l.contains(&text_l),
        Op::Empty => cell_l.is_empty(),
        Op::NotEmpty => !cell_l.is_empty(),
        Op::Gt | Op::Ge | Op::Lt | Op::Le => {
            let n = number_in(cell)?;
            match c.op {
                Op::Gt => n > c.number,
                Op::Ge => n >= c.number,
                Op::Lt => n < c.number,
                _ => n <= c.number,
            }
        }
    })
}

/// Whether a column sorts as numbers: at least half its filled cells hold
/// one.
fn numeric_column(data: &Dataset, rows: &[usize], col: usize) -> bool {
    let filled: Vec<&str> = rows
        .iter()
        .map(|&r| cell_of(data, r, col))
        .filter(|v| !v.trim().is_empty())
        .collect();
    !filled.is_empty()
        && filled.iter().filter(|v| number_in(v).is_some()).count() * 2 >= filled.len()
}

fn cell_of(data: &Dataset, row: usize, col: usize) -> &str {
    data.rows
        .get(row)
        .and_then(|r| r.get(col))
        .map_or("", String::as_str)
}

/// Sorts row numbers `kept` by `keys`: as numbers when a column mostly
/// holds them (rows with no number last), else as text, any case; ties
/// keep page order.
fn sort_rows(data: &Dataset, keys: &[SortKey], kept: &mut [usize]) {
    let numeric: Vec<bool> = keys
        .iter()
        .map(|k| numeric_column(data, kept, k.col))
        .collect();
    kept.sort_by(|&a, &b| {
        for (k, &num) in keys.iter().zip(&numeric) {
            let (left, right) = (cell_of(data, a, k.col), cell_of(data, b, k.col));
            let ord = if num {
                // Cells with no number go last, whatever the direction.
                match (number_in(left), number_in(right)) {
                    (Some(x), Some(y)) => {
                        let o = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
                        if k.desc { o.reverse() } else { o }
                    }
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                }
            } else {
                let o = left.trim().to_lowercase().cmp(&right.trim().to_lowercase());
                if k.desc { o.reverse() } else { o }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        a.cmp(&b)
    });
}

/// Runs `query` over `data` (stored as `handle`): `N of M rows match`, the
/// query in words, the rows as a Markdown table with a leading `#` (rank
/// in the result), and notes on cells with no number. Cut to fit
/// `cap_tokens`.
#[must_use]
pub fn run(data: &Dataset, handle: &str, query: &Query, cap_tokens: u64) -> String {
    let mut no_number: Vec<(usize, usize)> = Vec::new();
    let mut kept: Vec<usize> = Vec::new();
    for r in 0..data.rows.len() {
        let mut ok = true;
        for c in &query.conds {
            match matches(cell_of(data, r, c.col), c) {
                Some(true) => {}
                Some(false) => ok = false,
                None => {
                    ok = false;
                    match no_number.iter_mut().find(|(col, _)| *col == c.col) {
                        Some((_, n)) => *n += 1,
                        None => no_number.push((c.col, 1)),
                    }
                }
            }
        }
        if ok {
            kept.push(r);
        }
    }
    sort_rows(data, &query.sort, &mut kept);
    let total = data.rows.len();
    let mut out = format!(
        "{} of {total} rows of {handle} match ({}).",
        kept.len(),
        query.words(data)
    );
    let mut header = vec!["#".to_owned()];
    header.extend(query.columns.iter().map(|&c| data.columns[c].clone()));
    let shown_rows: Vec<Vec<String>> = kept
        .iter()
        .take(query.limit)
        .enumerate()
        .map(|(rank, &r)| {
            let mut row = vec![(rank + 1).to_string()];
            row.extend(
                query
                    .columns
                    .iter()
                    .map(|&c| cell_of(data, r, c).to_owned()),
            );
            row
        })
        .collect();
    let mut notes = Vec::new();
    for (col, n) in &no_number {
        notes.push(format!(
            "{n} {} had no number in {}.",
            if *n == 1 { "row" } else { "rows" },
            data.columns[*col]
        ));
    }
    let short_kept = kept.iter().filter(|r| data.short.contains(r)).count();
    if short_kept > 0 {
        notes.push(format!(
            "{short_kept} of these rows {} short; values may be shifted.",
            if short_kept == 1 { "is" } else { "are" }
        ));
    }
    if !shown_rows.is_empty() {
        // Fewer rows when the table doesn't fit.
        let mut n = shown_rows.len();
        loop {
            let table = markdown(&header, &shown_rows[..n]);
            let more = kept.len().min(query.limit) - n;
            let tail = if more > 0 {
                format!(
                    "\n({more} more rows didn't fit; ask for fewer columns or a smaller limit.)"
                )
            } else {
                String::new()
            };
            let fits = crate::budget::est(&out) + crate::budget::est(&table) + 40 <= cap_tokens;
            if fits || n == 1 {
                out.push('\n');
                out.push_str(&table);
                out.push_str(&tail);
                break;
            }
            n = (n / 2).max(1);
        }
    }
    for note in notes {
        out.push('\n');
        out.push_str(&note);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn towers() -> Dataset {
        Dataset {
            v: 1,
            kind: "table".into(),
            uid: "4_10".into(),
            page: "r3".into(),
            url: "https://wiki.example.com/tallest".into(),
            caption: String::new(),
            section: "main › \"Tallest buildings\"".into(),
            columns: vec![
                "Rank".into(),
                "Name".into(),
                "Height m".into(),
                "Year".into(),
            ],
            rows: vec![
                vec![
                    "1".into(),
                    "First Place".into(),
                    "298.3".into(),
                    "1975".into(),
                ],
                vec![
                    "2".into(),
                    "Tower Two".into(),
                    "276.9".into(),
                    "2009".into(),
                ],
                vec![
                    "3".into(),
                    "Plaza Three".into(),
                    "275".into(),
                    "1988".into(),
                ],
                vec![
                    "4".into(),
                    "Unfinished".into(),
                    "351.8".into(),
                    String::new(),
                ],
                vec!["5".into(), "Aura".into(), "271.9".into(), "2014".into()],
                vec!["6".into(), "Spire".into(), "—".into(), "2001".into()],
            ],
            short: vec![5],
        }
    }

    fn q(data: &Dataset, args: &Value) -> String {
        run(data, "d3", &Query::parse(args, data).unwrap(), 4_000)
    }

    #[test]
    fn numbers_are_read_like_people_write_them() {
        for (cell, n) in [
            ("1,234 ft", 1234.0),
            ("$28.99", 28.99),
            ("~312 m", 312.0),
            ("\u{2212}5 °C", -5.0),
            ("1\u{2009}024", 1024.0),
            ("21°C", 21.0),
            ("Rank 3 of 9", 3.0),
            ("3.10", 3.1),
            ("1,5", 1.0),
            ("x-5", 5.0),
        ] {
            assert_eq!(number_in(cell), Some(n), "{cell}");
        }
        assert_eq!(number_in("—"), None);
        assert_eq!(number_in(""), None);
    }

    #[test]
    fn filters_sorts_and_counts_exactly() {
        let d = towers();
        let out = q(
            &d,
            &json!({"where": [{"col": "year", "op": "not_empty"}], "sort": [{"col": "Height m", "desc": true}], "limit": 3}),
        );
        assert_eq!(
            out,
            "5 of 6 rows of d3 match (where Year not_empty, sorted by Height m desc, first 3).\n\
             | # | Rank | Name | Height m | Year |\n\
             |---|---|---|---|---|\n\
             | 1 | 1 | First Place | 298.3 | 1975 |\n\
             | 2 | 2 | Tower Two | 276.9 | 2009 |\n\
             | 3 | 3 | Plaza Three | 275 | 1988 |\n\
             1 of these rows is short; values may be shifted."
        );
        // Same bytes every time.
        assert_eq!(
            out,
            q(
                &d,
                &json!({"where": [{"col": "year", "op": "not_empty"}], "sort": [{"col": "Height m", "desc": true}], "limit": 3})
            )
        );
    }

    #[test]
    fn number_ops_skip_cells_without_a_number_and_say_so() {
        let d = towers();
        let out = q(
            &d,
            &json!({"where": [{"col": "Height m", "op": "gt", "value": 275}], "columns": ["Name"]}),
        );
        assert!(
            out.starts_with("3 of 6 rows of d3 match (where Height m gt \"275\", first 20)."),
            "{out}"
        );
        assert!(out.contains("| 1 | First Place |"));
        assert!(out.contains("| 3 | Unfinished |"));
        assert!(out.ends_with("1 row had no number in Height m."), "{out}");
        let le = q(
            &d,
            &json!({"where": {"col": "Height m", "op": "le", "value": "275 m"}}),
        );
        assert!(le.starts_with("2 of 6"), "{le}");
    }

    #[test]
    fn text_ops_ignore_case() {
        let d = towers();
        assert!(
            q(
                &d,
                &json!({"where": [{"col": "Name", "op": "contains", "value": "TOWER"}]})
            )
            .starts_with("1 of 6")
        );
        assert!(
            q(
                &d,
                &json!({"where": [{"col": "Name", "op": "not_contains", "value": "a"}]})
            )
            .starts_with("3 of 6")
        );
        assert!(
            q(
                &d,
                &json!({"where": [{"col": "Name", "op": "eq", "value": "aura"}]})
            )
            .starts_with("1 of 6")
        );
        assert!(q(&d, &json!({"where": [{"col": "Year", "op": "empty"}]})).starts_with("1 of 6"));
        assert!(
            q(
                &d,
                &json!({"where": [{"col": "Year", "op": "ne", "value": "2009"}]})
            )
            .starts_with("5 of 6")
        );
        // Text sort, ascending by default; a string names a sort column.
        let by_name = q(
            &d,
            &json!({"sort": "Name", "limit": 1, "columns": ["Name"]}),
        );
        assert!(by_name.contains("| 1 | Aura |"), "{by_name}");
    }

    #[test]
    fn mistakes_name_what_is_there() {
        let d = towers();
        let err = Query::parse(
            &json!({"where": [{"col": "Height", "op": "gt", "value": 1}]}),
            &d,
        )
        .unwrap_err();
        assert_eq!(
            err,
            "no column \"Height\"; the columns are: Rank | Name | Height m | Year"
        );
        assert!(
            Query::parse(
                &json!({"where": [{"col": "Year", "op": "after", "value": 1}]}),
                &d
            )
            .unwrap_err()
            .starts_with("op \"after\" isn't known")
        );
        assert!(
            Query::parse(
                &json!({"where": [{"col": "Year", "op": "gt", "value": "soon"}]}),
                &d
            )
            .unwrap_err()
            .contains("needs a number")
        );
        assert!(
            Query::parse(&json!({"where": [{"col": "Year", "op": "eq"}]}), &d)
                .unwrap_err()
                .contains("needs a value")
        );
    }

    #[test]
    fn a_small_cap_shows_fewer_rows_and_says_so() {
        let mut d = towers();
        for i in 0..50 {
            d.rows.push(vec![
                format!("{}", 7 + i),
                "x".repeat(100),
                "1".into(),
                "2000".into(),
            ]);
        }
        let out = run(
            &d,
            "d3",
            &Query::parse(&json!({"limit": 50}), &d).unwrap(),
            400,
        );
        assert!(
            crate::budget::est(&out) <= 400,
            "{}",
            crate::budget::est(&out)
        );
        assert!(out.contains("more rows didn't fit"), "{out}");
    }

    #[test]
    fn cells_are_escaped_in_markdown() {
        let t = markdown(&["a|b".into()], &[vec!["x|y".into()], vec![String::new()]]);
        assert_eq!(t, "| a\\|b |\n|---|\n| x\\|y |\n|   |");
    }

    #[test]
    fn datasets_round_trip() {
        let d = towers();
        assert_eq!(Dataset::from_json(&d.to_json()).unwrap(), d);
        assert!(Dataset::from_json("nope").is_err());
        assert_eq!(
            d.summary("d3"),
            "d3 · table · 6 rows · columns: Rank | Name | Height m | Year · section: main › \"Tallest buildings\""
        );
        assert_eq!(
            d.short_note().unwrap(),
            "1 row is short; its values may be shifted."
        );
    }
}
