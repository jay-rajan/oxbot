//! Page snapshots for the loop.
//!
//! chrome-devtools-mcp prints a page as an accessibility tree, one `uid=` line
//! per node. Big pages are far too large to show a model as they are, so this
//! module parses the text, trims it to what matters (controls, headings,
//! landmarks, short text), and renders it in a few forms:
//!
//! - the whole trimmed page ([`Trimmed::render`]), for small pages;
//! - a small page map ([`Trimmed::map`]), for big pages;
//! - `find_in_page` hits ([`Trimmed::find`]) and `read_section` parts
//!   ([`Trimmed::section`]), so the model can look inside a big page;
//! - a diff against the last page the model saw ([`diff`]).
//!
//! Everything is generic: roles, names and uids are the same on every site,
//! and the model brings the task-specific query. Output is deterministic:
//! the same input gives the same bytes.

mod diff;
mod find;
mod map;
mod parse;
mod render;
mod rows;
mod shape;
mod trim;

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) mod tests_golden;

use std::collections::BTreeMap;

pub use diff::diff;
pub use parse::{origin_path, page_news};
pub use rows::Found;
pub use trim::Trimmed;

/// Token estimate used everywhere (the loop's own).
pub(crate) fn est(text: &str) -> u64 {
    crate::budget::est(text)
}

/// One parsed tool result: the page list, the snapshot nodes and any other
/// text.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pages: Vec<PageEntry>,
    other: String,
    nodes: Vec<parse::RawNode>,
    root_title: String,
    root_url: String,
}

/// One line of the `## Pages` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageEntry {
    /// The page id (`pageId` for the browser tools).
    pub id: u32,
    /// The page title; empty when the line had only a url.
    pub title: String,
    /// The page url.
    pub url: String,
    /// Whether this is the selected page.
    pub selected: bool,
}

impl Snapshot {
    /// Parses one tool result. `strictness` ranks a node label (the line
    /// after `uid=<id> `), higher = stricter; when a uid shows twice, the
    /// strictest label wins (same rule as the element book). None when the
    /// text has no `## Latest page snapshot` section.
    #[must_use]
    pub fn parse(raw: &str, strictness: &dyn Fn(&str) -> u8) -> Option<Self> {
        parse::parse_snapshot(raw, strictness)
    }

    /// The `## Pages` list, in the order printed.
    #[must_use]
    pub fn pages(&self) -> &[PageEntry] {
        &self.pages
    }

    /// The selected page, if the list names one.
    #[must_use]
    pub fn selected(&self) -> Option<&PageEntry> {
        self.pages.iter().find(|p| p.selected)
    }

    /// The page's own title: from the selected page entry, else the
    /// `RootWebArea` name.
    #[must_use]
    pub fn title(&self) -> &str {
        match self.selected() {
            Some(p) if !p.title.is_empty() => &p.title,
            _ => &self.root_title,
        }
    }

    /// The page's own url: from the selected page entry, else the
    /// `RootWebArea` url.
    #[must_use]
    pub fn url(&self) -> &str {
        match self.selected() {
            Some(p) if !p.url.is_empty() => &p.url,
            _ => &self.root_url,
        }
    }

    /// Number of nodes in the raw snapshot (a repeated uid counts once).
    #[must_use]
    pub fn element_count(&self) -> usize {
        self.nodes.len()
    }

    /// Text outside the snapshot section (for example
    /// `Successfully navigated to …` or `## Console messages`), lines joined
    /// with `\n`.
    #[must_use]
    pub fn other_text(&self) -> &str {
        &self.other
    }

    /// Trims the snapshot (steps 1–4 of the design): drops noise, merges
    /// text, keeps controls, headings and landmarks with their state, and
    /// marks runs of same-shaped siblings for folding.
    #[must_use]
    pub fn trim(&self) -> Trimmed {
        trim::build(self)
    }
}

/// A `find_in_page` query.
#[derive(Debug, Clone, Default)]
pub struct FindQuery<'a> {
    /// Words to look for, case-insensitive; all must match (else any word).
    pub query: Option<&'a str>,
    /// `query` is one phrase, matched whole (any case).
    pub exact: bool,
    /// A regular expression (any case) the node must match too.
    pub regex: Option<&'a str>,
    /// Roles to keep; empty = any role.
    pub roles: Vec<String>,
    /// Look only under the heading or landmark whose name contains this.
    pub near: Option<&'a str>,
    /// Most hits to list; the caller clamps it to 1..=40 (default 20).
    pub limit: usize,
}

/// A diff baseline: the trimmed lines a tab last showed, and that
/// snapshot's handle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Baseline {
    /// The handle of the snapshot, like `r14`.
    pub handle: String,
    /// [`origin_path`] of that page's url.
    pub origin_path: String,
    /// uid → normalized trimmed line ([`Trimmed::lines`]).
    pub lines: BTreeMap<String, String>,
}

/// What a tool result says about the page (see [`page_news`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageNews {
    /// The selected page from a `## Pages` list: (id, url, title).
    pub selected: Option<(u32, String, String)>,
    /// The url from the last `… navigated to <url>.` line.
    pub navigated_to: Option<String>,
}
