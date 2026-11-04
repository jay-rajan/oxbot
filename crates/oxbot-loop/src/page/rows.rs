//! `extract_rows` (round 3, change 1): every table, grid and list of a
//! stored page as rows of data, read off the trimmed tree with nothing
//! folded, so `query_rows` can filter, sort and count them exactly.
//!
//! Choices where the design is silent:
//! - A row is a `row` whose nearest table or grid is this one (a table in
//!   a cell is a dataset of its own). Header rows are the leading rows made
//!   only of `columnheader` cells; the first one names the columns.
//! - A cell's value is its name and every word under it not already in it,
//!   with footnote marks (`[12]`, `[a]`, `[note 3]`) taken out.
//! - A list's items get a `link` column when at least half of them hold a
//!   link, and `part1…partN` columns when every item is made of the same
//!   number (2 to 8) of separate texts or pictures (a forecast's day, high
//!   and low), so columns are paired by oxbot, not by the model.
//! - With no `uid` or `near`, lists inside navigation, a banner or a
//!   footer (menus) come after the rest, and lists inside a table aren't
//!   datasets.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::LazyLock;

use regex::Regex;

use super::Snapshot;
use super::trim::{Kind, Trimmed, squash};
use crate::rows::{CELL_CAP, Dataset};

/// Most columns kept.
const MAX_COLUMNS: usize = 40;
/// A list needs this many items.
const MIN_ITEMS: usize = 3;
/// Most parts a list item may be split into.
const MAX_PARTS: usize = 8;
/// Landmarks whose lists are menus.
const MENU_LANDMARKS: [&str; 3] = ["navigation", "banner", "contentinfo"];

/// Footnote marks: `[12]`, `[a]`, `[iv]`, `[note 3]`, `[citation needed]`.
static FOOTNOTE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\[\s*(?:\d{1,3}|[a-z]{1,2}|[ivx]{1,5}|(?:note|nb|n)\s*\d{1,3}|citation needed)\s*\]",
    )
    .ok()
});

/// Text without footnote marks, spaces collapsed.
pub(crate) fn clean_cell(text: &str) -> String {
    let text = match FOOTNOTE.as_ref() {
        Some(re) => re.replace_all(text, " ").into_owned(),
        None => text.to_owned(),
    };
    let text = squash(&text);
    // A mark taken out before punctuation leaves a space: "298.3 ," → "298.3,".
    let text = text
        .replace(" ,", ",")
        .replace(" .", ".")
        .replace(" ;", ";");
    match text.char_indices().nth(CELL_CAP) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text,
    }
}

/// A table or list found on the page, before it's stored.
#[derive(Debug, Clone)]
pub struct Found {
    /// Its rows.
    pub data: Dataset,
    /// Inside a menu landmark.
    menu: bool,
}

impl Trimmed {
    /// Every table, grid and list (3+ items) under `uid` (the element
    /// itself counts), under the heading or landmark matching `near`, or
    /// on the whole page, in page order (menus last). `page` and `url` are
    /// left empty for the caller.
    ///
    /// # Errors
    /// A short message for the model: no such uid, or no heading or
    /// landmark matching `near`.
    pub fn datasets(
        &self,
        snapshot: &Snapshot,
        uid: Option<&str>,
        near: Option<&str>,
    ) -> Result<Vec<Found>, String> {
        let page = self.handle.as_deref().unwrap_or("the page");
        let scoped = uid.is_some() || near.is_some();
        let scope: Range<usize> = match (uid, near) {
            (Some(uid), _) => {
                let uid = uid.trim().trim_start_matches("uid=");
                let i = self.node_of(uid).ok_or_else(|| {
                    format!("no uid={uid} in {page}; find_in_page or the page map give uids")
                })?;
                i..self.nodes[i].end
            }
            (None, Some(near)) => self.near_scope(near)?.0,
            (None, None) => 0..self.nodes.len(),
        };
        let raw = RawIndex::new(snapshot);
        let mut found = Vec::new();
        for i in scope {
            let node = &self.nodes[i];
            let data = match node.role.as_str() {
                "table" | "grid" if node.kind == Kind::Container => self.table(i, &raw),
                "list" if node.kind == Kind::Container && !self.inside(i, &["table", "grid"]) => {
                    self.list(i, &raw)
                }
                _ => None,
            };
            if let Some(data) = data {
                let menu = !scoped && self.inside(i, &MENU_LANDMARKS);
                found.push(Found { data, menu });
            }
        }
        // Menus last; otherwise page order.
        found.sort_by_key(|f| f.menu);
        Ok(found)
    }

    /// Whether node `i` sits under a node with one of `roles`.
    fn inside(&self, i: usize, roles: &[&str]) -> bool {
        let mut p = self.nodes[i].parent;
        while let Some(a) = p {
            if roles.contains(&self.nodes[a].role.as_str()) {
                return true;
            }
            p = self.nodes[a].parent;
        }
        false
    }

    /// The nearest table or grid above node `i`.
    fn table_of(&self, i: usize) -> Option<usize> {
        let mut p = self.nodes[i].parent;
        while let Some(a) = p {
            if matches!(self.nodes[a].role.as_str(), "table" | "grid") {
                return Some(a);
            }
            p = self.nodes[a].parent;
        }
        None
    }

    /// Children of node `i`, in order.
    fn children(&self, i: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut c = self.nodes[i].first_child;
        while let Some(ci) = c {
            out.push(ci);
            c = self.nodes[ci].next_sibling;
        }
        out
    }

    /// A node's value: its name and every word under it not in it yet.
    fn merged(&self, i: usize) -> String {
        let mut text = squash(&self.nodes[i].name);
        for d in i + 1..self.nodes[i].end {
            let name = &self.nodes[d].name;
            if name.is_empty() || text.contains(name.as_str()) {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(name);
        }
        clean_cell(&text)
    }

    /// Where node `i` is, and its caption.
    fn context(&self, i: usize, raw: &RawIndex<'_>) -> (String, String) {
        let section = self.path(i);
        let caption = raw
            .caption(&self.nodes[i].uid)
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| clean_cell(&self.nodes[i].name));
        (section, caption)
    }

    /// Table or grid `t` as rows.
    fn table(&self, t: usize, raw: &RawIndex<'_>) -> Option<Dataset> {
        let rows: Vec<usize> = (t + 1..self.nodes[t].end)
            .filter(|&r| self.nodes[r].role == "row" && self.table_of(r) == Some(t))
            .collect();
        let mut header: Option<Vec<String>> = None;
        let mut body: Vec<Vec<String>> = Vec::new();
        for r in rows {
            let cells: Vec<usize> = self
                .children(r)
                .into_iter()
                .filter(|&c| self.nodes[c].kind == Kind::Cell)
                .collect();
            if cells.is_empty() {
                continue;
            }
            // Header rows (and headers repeated further down) aren't data.
            let all_headers = cells.iter().all(|&c| self.nodes[c].role == "columnheader");
            if all_headers {
                if header.is_none() && body.is_empty() {
                    header = Some(cells.iter().map(|&c| self.merged(c)).collect());
                }
                continue;
            }
            let values: Vec<String> = cells.iter().map(|&c| self.merged(c)).collect();
            if values.iter().all(String::is_empty) {
                continue;
            }
            body.push(values);
        }
        if body.is_empty() {
            return None;
        }
        let width = body
            .iter()
            .map(Vec::len)
            .chain(header.iter().map(Vec::len))
            .max()
            .unwrap_or(0)
            .min(MAX_COLUMNS);
        let columns = column_names(header.as_deref().unwrap_or(&[]), width);
        let mut short = Vec::new();
        for (n, row) in body.iter_mut().enumerate() {
            if row.len() < width {
                short.push(n);
            }
            row.resize(width, String::new());
        }
        let (section, caption) = self.context(t, raw);
        Some(Dataset {
            v: 1,
            kind: if self.nodes[t].role == "grid" {
                "grid"
            } else {
                "table"
            }
            .to_owned(),
            uid: self.nodes[t].uid.clone(),
            caption,
            section,
            columns,
            rows: body,
            short,
            ..Dataset::default()
        })
    }

    /// List `l` as rows, if it has 3 items or more.
    fn list(&self, l: usize, raw: &RawIndex<'_>) -> Option<Dataset> {
        let items: Vec<usize> = self
            .children(l)
            .into_iter()
            .filter(|&c| self.nodes[c].role == "listitem")
            .collect();
        if items.len() < MIN_ITEMS {
            return None;
        }
        let links: Vec<Option<String>> = items
            .iter()
            .map(|&i| {
                (i + 1..self.nodes[i].end)
                    .find(|&d| self.nodes[d].role == "link")
                    .map(|d| clean_cell(&self.nodes[d].name))
            })
            .collect();
        let with_link = links.iter().filter(|l| l.is_some()).count() * 2 >= items.len();
        let parts: Vec<Vec<String>> = items
            .iter()
            .map(|&i| raw.parts(&self.nodes[i].uid))
            .collect();
        let k = parts.first().map_or(0, Vec::len);
        let split = (2..=MAX_PARTS).contains(&k) && parts.iter().all(|p| p.len() == k);
        let mut columns = vec!["text".to_owned()];
        if with_link {
            columns.push("link".to_owned());
        }
        if split {
            columns.extend((1..=k).map(|n| format!("part{n}")));
        }
        let rows: Vec<Vec<String>> = items
            .iter()
            .zip(&links)
            .zip(&parts)
            .map(|((&i, link), part)| {
                let mut row = vec![self.merged(i)];
                if with_link {
                    row.push(link.clone().unwrap_or_default());
                }
                if split {
                    row.extend(part.iter().cloned());
                }
                row
            })
            .collect();
        let (section, caption) = self.context(l, raw);
        Some(Dataset {
            v: 1,
            kind: "list".to_owned(),
            uid: self.nodes[l].uid.clone(),
            caption,
            section,
            columns,
            rows,
            ..Dataset::default()
        })
    }
}

/// Column names: the header's, `colN` where empty or missing, and `_2`,
/// `_3`… after a repeated name.
fn column_names(header: &[String], width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(width);
    for n in 0..width {
        let base = header
            .get(n)
            .map(|h| h.chars().take(60).collect::<String>())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| format!("col{}", n + 1));
        let mut name = base.clone();
        let mut k = 2;
        while out.iter().any(|o| o.eq_ignore_ascii_case(&name)) {
            name = format!("{base}_{k}");
            k += 1;
        }
        out.push(name);
    }
    out
}

/// The raw snapshot, looked up by uid with subtree ranges: for captions
/// and for the separate texts of a list item (the trimmed tree merges
/// them).
struct RawIndex<'a> {
    snapshot: &'a Snapshot,
    at: HashMap<&'a str, usize>,
    end: Vec<usize>,
}

impl<'a> RawIndex<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        let nodes = &snapshot.nodes;
        let at = nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.uid.as_str(), i))
            .collect();
        let mut end: Vec<usize> = (1..=nodes.len()).collect();
        for j in (0..nodes.len()).rev() {
            if let Some(p) = nodes[j].parent {
                end[p] = end[p].max(end[j]);
            }
        }
        Self { snapshot, at, end }
    }

    /// The caption of the table with `uid`: a `caption` node's name, or
    /// its texts.
    fn caption(&self, uid: &str) -> Option<String> {
        let nodes = &self.snapshot.nodes;
        let &i = self.at.get(uid)?;
        let c = (i + 1..self.end[i]).find(|&d| nodes[d].role == "caption")?;
        if !nodes[c].name.trim().is_empty() {
            return Some(clean_cell(&nodes[c].name));
        }
        let texts: Vec<String> = (c + 1..self.end[c])
            .filter(|&d| leaf_text(&nodes[d].role))
            .map(|d| squash(&nodes[d].name))
            .filter(|t| !t.is_empty())
            .collect();
        Some(clean_cell(&texts.join(" ")))
    }

    /// The separate texts and picture names under `uid`, in order.
    fn parts(&self, uid: &str) -> Vec<String> {
        let nodes = &self.snapshot.nodes;
        let Some(&i) = self.at.get(uid) else {
            return Vec::new();
        };
        (i + 1..self.end[i])
            .filter(|&d| {
                leaf_text(&nodes[d].role) || matches!(nodes[d].role.as_str(), "image" | "img")
            })
            .map(|d| clean_cell(&nodes[d].name))
            .filter(|t| !t.is_empty())
            .take(MAX_PARTS + 1)
            .collect()
    }
}

/// Raw roles that carry a piece of text.
fn leaf_text(role: &str) -> bool {
    matches!(role, "StaticText" | "text" | "")
}
