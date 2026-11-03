//! `find_in_page` (delta §B1) and `read_section` (delta §B2) over the
//! trimmed tree. Folded nodes are searched and read like any other.
//!
//! Choices where the design is silent:
//! - Most specific hits: a node is listed only when nothing under it also
//!   matches, so "lamp" lists the link, not its listitem; "lamp $24" lists
//!   the listitem (no single child has both words).
//! - A listitem, row, article or cell without a name that matches through
//!   its children shows their text after it: `(text: "Desk lamp · $24.00")`.
//! - Words match names and state, not link urls.
//! - `near` picks the heading or landmark whose name equals the text, else
//!   the first whose name contains it. A heading with nothing after it at its
//!   own level looks after its parent (up to 3 levels up).

use std::fmt::Write as _;
use std::ops::Range;

use super::trim::{Kind, Trimmed, cut, thousands};
use super::{FindQuery, est};

/// Hit lines keep names to this many chars.
const HIT_NAME: usize = 120;
/// Longest path entry name.
const PATH_NAME: usize = 40;
/// Path entries shown.
const PATH_LEN: usize = 4;
/// Most query words used.
const MAX_WORDS: usize = 32;
/// Most lines `read_section` shows.
const MAX_SECTION: usize = 150;
/// Roles whose descendants' text counts as their own when searching.
const HOLDERS: [&str; 6] = ["listitem", "row", "article", "cell", "gridcell", "treeitem"];

impl Trimmed {
    /// `find_in_page` (delta §B1): matching nodes, one line each with its
    /// section path, capped at `cap_tokens`.
    ///
    /// # Errors
    /// A short message for the model: no query and no role, or no heading
    /// or landmark matching `near`.
    pub fn find(&self, q: &FindQuery<'_>, cap_tokens: u64) -> Result<String, String> {
        let words: Vec<String> = q
            .query
            .unwrap_or("")
            .split_whitespace()
            .take(MAX_WORDS)
            .map(str::to_lowercase)
            .collect();
        let roles: Vec<String> = q
            .roles
            .iter()
            .map(|r| r.trim().to_lowercase())
            .filter(|r| !r.is_empty())
            .map(|r| match r.as_str() {
                "img" => "image".to_owned(),
                "statictext" => "text".to_owned(),
                _ => r,
            })
            .collect();
        if words.is_empty() && roles.is_empty() {
            return Err("give query or role (or both)".to_owned());
        }
        let (scope, anchor) = match q.near.map(str::trim).filter(|s| !s.is_empty()) {
            Some(near) => {
                let (range, a) = self.near_scope(near)?;
                (range, Some((near, a)))
            }
            None => (0..self.nodes.len(), None),
        };
        let limit = if q.limit == 0 { 20 } else { q.limit.min(40) };
        let mut any = false;
        let mut hits = self.hits(&words, &roles, &scope, true);
        if hits.is_empty() && words.len() > 1 {
            hits = self.hits(&words, &roles, &scope, false);
            any = true;
        }
        let mut what = Vec::new();
        if let Some(query) = q.query.filter(|_| !words.is_empty()) {
            let query = cut(query.trim(), 80);
            what.push(if any {
                format!("any word of \"{query}\" (none had all words)")
            } else {
                format!("\"{query}\"")
            });
        }
        if !roles.is_empty() {
            what.push(format!("role {}", roles.join(", ")));
        }
        if let Some((near, a)) = anchor {
            what.push(format!(
                "near \"{}\" ({} uid={})",
                cut(near, 40),
                self.nodes[a].role,
                self.nodes[a].uid
            ));
        }
        let page = self.handle.as_deref().unwrap_or("the page");
        let mut out = format!(
            "{} {} in {page} for {}",
            thousands(hits.len()),
            if hits.len() == 1 { "match" } else { "matches" },
            what.join(", ")
        );
        let mut shown = 0;
        for &h in hits.iter().take(limit) {
            let line = self.hit_line(h);
            let more = hits.len() - shown - 1;
            let footer = if more > 0 {
                more_line(more).len() + 1
            } else {
                0
            };
            if est(&out) + est(&line) + 1 + (footer as u64).div_ceil(3) > cap_tokens {
                break;
            }
            out.push('\n');
            out.push_str(&line);
            shown += 1;
        }
        if shown < hits.len() {
            out.push('\n');
            out.push_str(&more_line(hits.len() - shown));
        }
        Ok(out)
    }

    /// The most specific nodes in `scope` that match.
    fn hits(
        &self,
        words: &[String],
        roles: &[String],
        scope: &Range<usize>,
        all: bool,
    ) -> Vec<usize> {
        let nodes = &self.nodes;
        let n = nodes.len();
        let full: u64 = if words.len() >= 64 {
            u64::MAX
        } else {
            (1u64 << words.len()) - 1
        };
        let mut own = vec![0u64; n];
        if !words.is_empty() {
            for i in scope.clone() {
                let text = self.search_text(i);
                for (w, word) in words.iter().enumerate() {
                    if text.contains(word.as_str()) {
                        own[i] |= 1 << w;
                    }
                }
            }
        }
        let mut sub = own.clone();
        for i in scope.clone().rev() {
            if let Some(p) = nodes[i].parent
                && scope.contains(&p)
            {
                sub[p] |= sub[i];
            }
        }
        let mut ok = vec![false; n];
        let mut below = vec![false; n];
        for i in scope.clone().rev() {
            let node = &nodes[i];
            let role_ok = roles.is_empty() || roles.iter().any(|r| *r == node.role.to_lowercase());
            let mask = if HOLDERS.contains(&node.role.as_str()) {
                sub[i]
            } else {
                own[i]
            };
            let words_ok = words.is_empty() || if all { mask == full } else { mask != 0 };
            ok[i] = role_ok && words_ok;
            if (ok[i] || below[i])
                && let Some(p) = node.parent
            {
                below[p] = true;
            }
        }
        scope.clone().filter(|&i| ok[i] && !below[i]).collect()
    }

    /// Lowercased name and state of node `i` (link urls left out: on a
    /// search page every link's url holds the query words).
    fn search_text(&self, i: usize) -> String {
        let n = &self.nodes[i];
        let mut s = n.name.to_lowercase();
        for a in n.attrs.iter().filter(|a| !a.starts_with("url=")) {
            s.push(' ');
            s.push_str(&a.to_lowercase());
        }
        s
    }

    /// One hit: the node's line, its children's text for an unnamed
    /// holder, and its section path.
    fn hit_line(&self, i: usize) -> String {
        let n = &self.nodes[i];
        let mut s = self.line_cut(i, HIT_NAME);
        if n.name.is_empty() && HOLDERS.contains(&n.role.as_str()) {
            let mut text = String::new();
            for d in i + 1..n.end {
                let name = &self.nodes[d].name;
                if name.is_empty() {
                    continue;
                }
                if !text.is_empty() {
                    text.push_str(" · ");
                }
                text.push_str(name);
                if text.chars().count() > HIT_NAME {
                    break;
                }
            }
            if !text.is_empty() {
                let _ = write!(s, " (text: \"{}\")", cut(&text, HIT_NAME));
            }
        }
        let path = self.path(i);
        if !path.is_empty() {
            s.push_str("  — ");
            s.push_str(&path);
        }
        s
    }

    /// Section path of node `i`: kept landmark and container ancestors, and
    /// the heading each part sits under, the last 4, outermost first.
    pub(super) fn path(&self, i: usize) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut child = i;
        while let Some(a) = self.nodes[child].parent {
            if let Some(h) = self.heading_before(a, child) {
                parts.push(format!("\"{}\"", cut(&self.nodes[h].name, PATH_NAME)));
            }
            let node = &self.nodes[a];
            let on_path = match node.kind {
                Kind::Landmark => true,
                Kind::Container => {
                    matches!(node.role.as_str(), "listitem" | "row" | "article")
                        || !node.name.is_empty()
                }
                _ => false,
            };
            if on_path {
                parts.push(if node.name.is_empty() {
                    node.role.clone()
                } else {
                    format!("{} \"{}\"", node.role, cut(&node.name, PATH_NAME))
                });
            }
            child = a;
        }
        parts.truncate(PATH_LEN);
        parts.reverse();
        parts.join(" › ")
    }

    /// The nearest heading among `parent`'s children before `child`.
    fn heading_before(&self, parent: usize, child: usize) -> Option<usize> {
        let mut found = None;
        let mut c = self.nodes[parent].first_child;
        while let Some(ci) = c {
            if ci == child {
                break;
            }
            if self.nodes[ci].kind == Kind::Heading {
                found = Some(ci);
            }
            c = self.nodes[ci].next_sibling;
        }
        found
    }

    /// The node range `near` points at, and the anchor node.
    fn near_scope(&self, near: &str) -> Result<(Range<usize>, usize), String> {
        let needle = near.to_lowercase();
        let anchors = || {
            (0..self.nodes.len())
                .filter(|&i| matches!(self.nodes[i].kind, Kind::Heading | Kind::Landmark))
        };
        let exact = anchors().find(|&i| self.nodes[i].name.to_lowercase() == needle);
        let a = exact
            .or_else(|| anchors().find(|&i| self.nodes[i].name.to_lowercase().contains(&needle)))
            .ok_or_else(|| {
                format!(
                    "no heading or landmark named like \"{}\"; the page map lists them",
                    cut(near, 40)
                )
            })?;
        if self.nodes[a].kind == Kind::Landmark {
            return Ok((a + 1..self.nodes[a].end, a));
        }
        let level = self.nodes[a].level;
        let mut x = a;
        for _ in 0..4 {
            let start = self.nodes[x].end;
            let mut stop = self.nodes[x]
                .parent
                .map_or(self.nodes.len(), |p| self.nodes[p].end);
            let mut s = self.nodes[x].next_sibling;
            while let Some(si) = s {
                if self.nodes[si].kind == Kind::Heading && self.nodes[si].level <= level {
                    stop = si;
                    break;
                }
                s = self.nodes[si].next_sibling;
            }
            if stop > start {
                return Ok((start..stop, a));
            }
            match self.nodes[x].parent {
                Some(p) => x = p,
                None => break,
            }
        }
        Ok((a..a, a))
    }

    /// `read_section` (delta §B2): the trimmed subtree under `uid`, repeats
    /// folded, line-numbered, at most `max_lines` (≤ 150) lines, capped at
    /// `cap_tokens`.
    ///
    /// # Errors
    /// A short message for the model when the page has no such uid.
    pub fn section(&self, uid: &str, max_lines: usize, cap_tokens: u64) -> Result<String, String> {
        let page = self.handle.as_deref().unwrap_or("the page");
        let uid = uid.trim();
        let uid = uid.strip_prefix("uid=").unwrap_or(uid);
        let Some(i) = self.node_of(uid) else {
            return Err(format!(
                "no uid={uid} in {page}; find_in_page or the page map give uids"
            ));
        };
        let node = &self.nodes[i];
        let lines = self.rendered(i..node.end, true, self.handle.as_deref());
        let max = max_lines.clamp(1, MAX_SECTION);
        let mut out = format!(
            "Section uid={} ({}) in {page}: {} lines",
            node.uid,
            node.role,
            thousands(lines.len())
        );
        if node.uid != uid {
            let _ = write!(out, " (uid={uid} is shown as part of uid={})", node.uid);
        }
        let width = lines.len().min(max).to_string().len();
        let mut shown = 0;
        for (k, l) in lines.iter().take(max).enumerate() {
            let row = format!("{:>width$}  {}", k + 1, l.text);
            let footer = if k + 1 < lines.len() { 30 } else { 0 };
            if est(&out) + est(&row) + 1 + footer > cap_tokens {
                break;
            }
            out.push('\n');
            out.push_str(&row);
            shown += 1;
        }
        if shown < lines.len() {
            let _ = write!(
                out,
                "\n[showing lines 1–{shown} of {}; read_section a child uid, or find_in_page, to see more]",
                thousands(lines.len())
            );
        }
        Ok(out)
    }
}

fn more_line(n: usize) -> String {
    format!("{} more; narrow with role or near", thousands(n))
}
