//! Parsing chrome-devtools-mcp text: sections, the page list, and the
//! snapshot's `uid=` node lines.

use std::collections::HashMap;

use super::{PageEntry, PageNews, Snapshot};

/// The heading the browser tool puts before a page snapshot.
pub(super) const SNAPSHOT_HEADING: &str = "## Latest page snapshot";
/// The heading of the open pages list.
const PAGES_HEADING: &str = "## Pages";
/// What the formatter adds to the node picked in the Elements panel.
const PANEL_SUFFIX: &str = " [selected in the DevTools Elements panel]";
/// Most attribute tokens one node line can have (keeps parsing linear).
const MAX_ATTRS: usize = 48;
/// The lines that say where the page went.
const NAV_PREFIXES: [&str; 4] = [
    "Successfully navigated to ",
    "Successfully navigated back to ",
    "Successfully navigated forward to ",
    "Page navigated to ",
];

/// One node of the raw snapshot.
#[derive(Debug, Clone)]
pub(super) struct RawNode {
    pub(super) uid: String,
    pub(super) parent: Option<usize>,
    /// Role as printed (`ignored` for `none`); empty when absent.
    pub(super) role: String,
    /// Name as printed (may hold newlines).
    pub(super) name: String,
    /// Attributes in printed order; `None` value = a bare boolean.
    pub(super) attrs: Vec<(String, Option<String>)>,
    /// Text of lines that posed as nodes (a repeated uid), kept as text.
    pub(super) extra: String,
}

impl RawNode {
    /// The value of attribute `key`, `Some("")` for a bare one.
    pub(super) fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_deref().unwrap_or(""))
    }
}

/// One `uid=` line and the lines that continue it.
struct Record<'a> {
    uid: &'a str,
    indent: usize,
    words: &'a str,
    more: Vec<&'a str>,
}

impl Record<'_> {
    /// The full text after `uid=<id> `, continuation lines joined by `\n`.
    fn full(&self) -> String {
        let mut s = String::with_capacity(self.words.len());
        s.push_str(self.words);
        for m in &self.more {
            s.push('\n');
            s.push_str(m);
        }
        s
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Before,
    Snapshot,
    After,
}

/// See [`Snapshot::parse`]. A `## ` line ends the snapshot section, the
/// same rule as the element book, so the model and the gate see the same
/// nodes. The page list counts only before the snapshot, where the browser
/// tool prints it (page text can't fake it).
pub(super) fn parse_snapshot(raw: &str, strictness: &dyn Fn(&str) -> u8) -> Option<Snapshot> {
    let mut part = Part::Before;
    let mut in_pages = false;
    let mut found = false;
    let mut pages = Vec::new();
    let mut other: Vec<&str> = Vec::new();
    let mut records: Vec<Record<'_>> = Vec::new();
    for line in raw.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim_end() == SNAPSHOT_HEADING {
            part = Part::Snapshot;
            found = true;
            continue;
        }
        if line.starts_with("## ") {
            if part == Part::Snapshot {
                part = Part::After;
            }
            in_pages = part == Part::Before && line.trim_end() == PAGES_HEADING;
            other.push(line);
            continue;
        }
        match part {
            Part::Before | Part::After => {
                if in_pages && let Some(p) = parse_page_line(line) {
                    pages.push(p);
                }
                other.push(line);
            }
            Part::Snapshot => {
                let trimmed = line.trim_start();
                let rec = trimmed.strip_prefix("uid=").and_then(|rest| {
                    let (uid, words) = rest.split_once(' ').unwrap_or((rest, ""));
                    (!uid.is_empty()).then_some(Record {
                        uid,
                        indent: line.len() - trimmed.len(),
                        words,
                        more: Vec::new(),
                    })
                });
                match (rec, records.last_mut()) {
                    (Some(r), _) => records.push(r),
                    (None, Some(last)) => last.more.push(line),
                    (None, None) => other.push(line),
                }
            }
        }
    }
    if !found {
        return None;
    }
    // Trailing empty lines belong to the layout, not to a name.
    for r in &mut records {
        while r.more.last().is_some_and(|m| m.trim().is_empty()) {
            r.more.pop();
        }
    }
    while other.last().is_some_and(|l| l.trim().is_empty()) {
        other.pop();
    }
    let mut other = other.join("\n");
    let nodes = build_nodes(&records, strictness, &mut other);
    let root = nodes.first().filter(|n| n.role == "RootWebArea");
    let root_title = root.map(|n| n.name.clone()).unwrap_or_default();
    let root_url = root
        .and_then(|n| n.attr("url"))
        .unwrap_or_default()
        .to_owned();
    Some(Snapshot {
        pages,
        other,
        nodes,
        root_title,
        root_url,
    })
}

/// Turns records into nodes: one node per uid (the strictest label; the
/// first on a tie), the other lines become text of the node before them.
fn build_nodes(
    records: &[Record<'_>],
    strictness: &dyn Fn(&str) -> u8,
    other: &mut String,
) -> Vec<RawNode> {
    let mut best: HashMap<&str, (usize, u8)> = HashMap::with_capacity(records.len());
    for (i, r) in records.iter().enumerate() {
        let s = strictness(r.words);
        best.entry(r.uid)
            .and_modify(|b| {
                if s > b.1 {
                    *b = (i, s);
                }
            })
            .or_insert((i, s));
    }
    let mut nodes: Vec<RawNode> = Vec::with_capacity(best.len());
    // (indent, node index) of the open ancestors.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (i, r) in records.iter().enumerate() {
        let full = r.full();
        if best.get(r.uid).map(|b| b.0) != Some(i) {
            let text = format!("uid={} {full}", r.uid);
            if let Some(prev) = nodes.last_mut() {
                if !prev.extra.is_empty() {
                    prev.extra.push(' ');
                }
                prev.extra.push_str(&text);
            } else {
                if !other.is_empty() {
                    other.push('\n');
                }
                other.push_str(&text);
            }
            continue;
        }
        while stack.last().is_some_and(|&(d, _)| d >= r.indent) {
            stack.pop();
        }
        let (role, name, attrs) = parse_node_text(&full);
        nodes.push(RawNode {
            uid: r.uid.to_owned(),
            parent: stack.last().map(|&(_, p)| p),
            role,
            name,
            attrs,
            extra: String::new(),
        });
        stack.push((r.indent, nodes.len() - 1));
    }
    nodes
}

/// Splits the text after `uid=<id> ` into role, name and attributes.
///
/// The name is printed raw, so it may hold `"` and newlines. It ends at the
/// first `"` after which the rest reads as attributes (known shapes, sorted
/// by name as the formatter prints them). If nothing reads cleanly, the name
/// runs to the last `"` and nothing is lost.
pub(super) fn parse_node_text(s: &str) -> (String, String, Vec<(String, Option<String>)>) {
    let s = s.strip_suffix(PANEL_SUFFIX).unwrap_or(s);
    let (role, tail) = if s.starts_with('"') {
        ("", s)
    } else {
        match s.find(' ') {
            Some(i) => (&s[..i], &s[i + 1..]),
            None => (s, ""),
        }
    };
    let role = if role.contains('\n') { "" } else { role };
    let tail = if role.is_empty() && !s.starts_with('"') {
        s
    } else {
        tail
    };
    if let Some(inner) = tail.strip_prefix('"') {
        let (name, attrs) = split_name(inner);
        return (role.to_owned(), name.to_owned(), attrs);
    }
    let spaced = format!(" {tail}");
    let quotes = closing_quotes(&spaced);
    match parse_attrs(&spaced, 0, &quotes) {
        Some(attrs) => (role.to_owned(), String::new(), attrs),
        None => (role.to_owned(), tail.trim().to_owned(), Vec::new()),
    }
}

/// `inner` is the text after the opening quote of a name.
fn split_name(inner: &str) -> (&str, Vec<(String, Option<String>)>) {
    let quotes = closing_quotes(inner);
    for &q in &quotes {
        if let Some(attrs) = parse_attrs(inner, q + 1, &quotes) {
            return (&inner[..q], attrs);
        }
    }
    match inner.rfind('"') {
        Some(q) => (&inner[..q], Vec::new()),
        None => (inner, Vec::new()),
    }
}

/// Positions of `"` followed by a space or the end: the only places a
/// quoted name or value can end.
fn closing_quotes(s: &str) -> Vec<usize> {
    let b = s.as_bytes();
    b.iter()
        .enumerate()
        .filter(|&(i, &c)| c == b'"' && (i + 1 == b.len() || b[i + 1] == b' '))
        .map(|(i, _)| i)
        .collect()
}

/// The attribute a companion boolean sorts with (`focusable` → `focused`).
fn sort_key(token: &str) -> &str {
    match token {
        "focusable" => "focused",
        "disableable" => "disabled",
        "expandable" => "expanded",
        "selectable" => "selected",
        "checkable" => "checked",
        other => other,
    }
}

/// Parses ` key key="value" …` from `pos` to the end of `s`, or None if it
/// doesn't read as attributes. Keys must come in the formatter's order.
fn parse_attrs(s: &str, mut pos: usize, quotes: &[usize]) -> Option<Vec<(String, Option<String>)>> {
    let b = s.as_bytes();
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    let mut prev: Option<&str> = None;
    let mut first = true;
    while pos < b.len() {
        if b[pos] != b' ' || out.len() >= MAX_ATTRS {
            return None;
        }
        pos += 1;
        // Our own trimmed lines put `(+1,834 chars)` after a cut name.
        if first && let Some(n) = cut_marker_len(&s[pos..]) {
            pos += n;
            first = false;
            continue;
        }
        first = false;
        let start = pos;
        while pos < b.len() && matches!(b[pos], b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_') {
            pos += 1;
        }
        if pos == start || !b[start].is_ascii_lowercase() {
            return None;
        }
        let key = &s[start..pos];
        if let Some(p) = prev {
            let (pk, ck) = (sort_key(p), sort_key(key));
            if pk > ck || p == key || (pk == ck && sort_key(p) == p) {
                return None;
            }
        }
        prev = Some(key);
        if pos == b.len() || b[pos] == b' ' {
            out.push((key.to_owned(), None));
            continue;
        }
        if !s[pos..].starts_with("=\"") {
            return None;
        }
        let vstart = pos + 2;
        let k = quotes.partition_point(|&q| q < vstart);
        let end = *quotes.get(k)?;
        out.push((key.to_owned(), Some(s[vstart..end].to_owned())));
        pos = end + 1;
    }
    Some(out)
}

/// Length of a `(+1,834 chars)` marker at the start of `s`.
fn cut_marker_len(s: &str) -> Option<usize> {
    let rest = s.strip_prefix("(+")?;
    let digits = rest
        .bytes()
        .take_while(|c| c.is_ascii_digit() || *c == b',')
        .count();
    let after = rest[digits..].strip_prefix(" chars)")?;
    (digits > 0).then_some(s.len() - after.len())
}

/// One `## Pages` line: `N: Title (url) [selected]` or `N: url [selected]`,
/// maybe followed by ` isolatedContext=…`.
pub(super) fn parse_page_line(line: &str) -> Option<PageEntry> {
    let (id, label) = line.trim().split_once(": ")?;
    let id = id.parse::<u32>().ok()?;
    let mut label = label.trim_end();
    if let Some(i) = label.rfind(" isolatedContext=")
        && !label[i + 1..].contains(' ')
    {
        label = &label[..i];
    }
    let selected = match label.strip_suffix(" [selected]") {
        Some(l) => {
            label = l;
            true
        }
        None => false,
    };
    let (title, url) = match label.rfind(" (") {
        Some(i) if label.ends_with(')') => (&label[..i], &label[i + 2..label.len() - 1]),
        _ => ("", label),
    };
    Some(PageEntry {
        id,
        title: title.to_owned(),
        url: url.to_owned(),
        selected,
    })
}

/// Reads what a tool result says about the page: the selected entry of a
/// `## Pages` list (`N: Title (url) [selected]` or `N: url [selected]`, with
/// anything after `[selected]`), and the last `Successfully navigated to
/// <url>.`, `Successfully navigated back/forward to <url>.` or `Page
/// navigated to <url>.` line. Only text the browser tool prints counts:
/// navigation lines before any `## ` section, and the page list before the
/// snapshot.
#[must_use]
pub fn page_news(text: &str) -> PageNews {
    let mut news = PageNews::default();
    let mut in_pages = false;
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line == SNAPSHOT_HEADING {
            break;
        }
        if line.starts_with("## ") {
            in_section = true;
            in_pages = line == PAGES_HEADING;
            continue;
        }
        if in_pages {
            if let Some(p) = parse_page_line(line)
                && p.selected
                && news.selected.is_none()
            {
                news.selected = Some((p.id, p.url, p.title));
            }
            continue;
        }
        if in_section {
            continue;
        }
        for prefix in NAV_PREFIXES {
            if let Some(rest) = line.trim_start().strip_prefix(prefix) {
                let url = rest.strip_suffix('.').unwrap_or(rest).trim();
                if !url.is_empty() {
                    news.navigated_to = Some(url.to_owned());
                }
            }
        }
    }
    news
}

/// "origin+path" of a url: `shop.example.com/search` (scheme, user, query
/// and fragment dropped, host lowercased). Used for the "navigation shows
/// the full page" rule and for tab keys.
#[must_use]
pub fn origin_path(url: &str) -> String {
    let url = url.trim();
    let url = url.split(['#', '?']).next().unwrap_or(url);
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    format!("{}{path}", host.to_ascii_lowercase())
}

/// The host of a url (lowercased), or "" when it has none.
pub(super) fn host_of(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else {
        return String::new();
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    host.to_ascii_lowercase()
}
