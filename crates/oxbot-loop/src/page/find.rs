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
    /// section path, capped at `cap_tokens`. When not every match fits,
    /// the first line says so.
    ///
    /// # Errors
    /// A short message for the model: no query and no role, or no heading
    /// or landmark matching `near`.
    pub fn find(&self, q: &FindQuery<'_>, cap_tokens: u64) -> Result<String, String> {
        let query = q.query.map(str::trim).filter(|s| !s.is_empty());
        let mut terms: Vec<Term> = match query {
            Some(phrase) if q.exact => vec![Term::Phrase(phrase.to_lowercase())],
            Some(words) => words
                .split_whitespace()
                .take(MAX_WORDS)
                .map(|w| Term::Word(w.to_lowercase()))
                .collect(),
            None => Vec::new(),
        };
        let words = terms.len();
        let pattern = q.regex.map(str::trim).filter(|r| !r.is_empty());
        if let Some(pattern) = pattern {
            terms.push(Term::Regex(compile(pattern)?));
        }
        let roles = roles_of(&q.roles)?;
        if terms.is_empty() && roles.is_empty() {
            return Err("give query, regex or role (or more than one)".to_owned());
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
        let mut hits = self.hits(&terms, &roles, &scope, true);
        if hits.is_empty() && words > 1 && !q.exact {
            hits = self.hits(&terms, &roles, &scope, false);
            any = true;
        }
        let mut what = Vec::new();
        if let Some(query) = query {
            let query = cut(query, 80);
            what.push(if q.exact {
                format!("the phrase \"{query}\"")
            } else if any {
                format!("any word of \"{query}\" (none had all words)")
            } else {
                format!("\"{query}\"")
            });
        }
        if let Some(pattern) = pattern {
            what.push(format!("regex /{}/", cut(pattern, 80)));
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
        let total = thousands(hits.len());
        let noun = if hits.len() == 1 { "match" } else { "matches" };
        let what = what.join(", ");
        // Room for the longest first line.
        let mut used = est(&format!(
            "Showing {total} of {total} {noun} in {page} for {what}. {NOT_ALL}"
        )) + 1;
        let mut lines: Vec<String> = Vec::new();
        for &h in hits.iter().take(limit) {
            let line = self.hit_line(h);
            if used + est(&line) + 1 > cap_tokens {
                break;
            }
            used += est(&line) + 1;
            lines.push(line);
        }
        // When some are left out, the first line says so: a model reading
        // the top must not take the list for all there is.
        let mut out = if lines.len() < hits.len() {
            format!(
                "Showing {} of {total} {noun} in {page} for {what}. {NOT_ALL}",
                thousands(lines.len())
            )
        } else {
            format!("{total} {noun} in {page} for {what}")
        };
        for line in lines {
            out.push('\n');
            out.push_str(&line);
        }
        Ok(out)
    }

    /// The most specific nodes in `scope` that match (`all`: every term,
    /// else any).
    fn hits(
        &self,
        terms: &[Term],
        roles: &[String],
        scope: &Range<usize>,
        all: bool,
    ) -> Vec<usize> {
        let nodes = &self.nodes;
        let n = nodes.len();
        let full: u64 = if terms.len() >= 64 {
            u64::MAX
        } else {
            (1u64 << terms.len()) - 1
        };
        let mut own = vec![0u64; n];
        if !terms.is_empty() {
            for i in scope.clone() {
                let text = self.search_text(i);
                for (w, term) in terms.iter().enumerate() {
                    if term.matches(&text) {
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
            let words_ok = terms.is_empty() || if all { mask == full } else { mask != 0 };
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
        // Every word: what the task looks for may be at the end.
        let mut s = self.line_cut(i, usize::MAX);
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
            }
            if !text.is_empty() {
                let _ = write!(s, " (text: \"{text}\")");
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
    pub(super) fn near_scope(&self, near: &str) -> Result<(Range<usize>, usize), String> {
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

    /// `read_section`: the subtree under `uid` with every item (nothing
    /// folded) and every word (long text wraps onto `…` lines),
    /// line-numbered from `from_line` (1-based), at most `max_lines`
    /// (≤ 150) lines and `cap_tokens`; the last line says where to go on.
    ///
    /// # Errors
    /// A short message for the model when the page has no such uid.
    pub fn section(
        &self,
        uid: &str,
        from_line: usize,
        max_lines: usize,
        cap_tokens: u64,
    ) -> Result<String, String> {
        let page = self.handle.as_deref().unwrap_or("the page");
        let uid = uid.trim();
        let uid = uid.strip_prefix("uid=").unwrap_or(uid);
        let Some(i) = self.node_of(uid) else {
            return Err(format!(
                "no uid={uid} in {page}; find_in_page or the page map give uids"
            ));
        };
        let node = &self.nodes[i];
        let lines: Vec<String> = self
            .rendered(i..node.end, false, None, usize::MAX)
            .into_iter()
            .flat_map(|l| wrap_line(&l.text))
            .collect();
        let max = max_lines.clamp(1, MAX_SECTION);
        let from = from_line.max(1);
        let mut out = format!(
            "Section uid={} ({}) in {page}: {} lines",
            node.uid,
            node.role,
            thousands(lines.len())
        );
        if node.uid != uid {
            let _ = write!(out, " (uid={uid} is shown as part of uid={})", node.uid);
        }
        if from > lines.len() {
            let _ = write!(out, "\n(it has only {} lines)", thousands(lines.len()));
            return Ok(out);
        }
        let last = (from - 1 + max).min(lines.len());
        let width = last.to_string().len();
        let mut shown_to = from - 1;
        for (k, l) in lines.iter().enumerate().take(last).skip(from - 1) {
            let row = format!("{:>width$}  {l}", k + 1);
            let footer = if k + 1 < lines.len() { 40 } else { 0 };
            if shown_to >= from && est(&out) + est(&row) + 1 + footer > cap_tokens {
                break;
            }
            out.push('\n');
            out.push_str(&row);
            shown_to = k + 1;
        }
        if shown_to < lines.len() {
            let _ = write!(
                out,
                "\n[lines {from}–{shown_to} of {}; continue with read_section(page: \"{page}\", uid: \"{}\", from_line: {})]",
                thousands(lines.len()),
                node.uid,
                shown_to + 1
            );
        }
        Ok(out)
    }
}

/// Most characters of one section line; longer text wraps.
const WRAP: usize = 400;

/// `line` as lines of at most [`WRAP`] characters, broken at spaces; the
/// rest of a long text goes on `…` lines under it.
fn wrap_line(line: &str) -> Vec<String> {
    if line.chars().count() <= WRAP {
        return vec![line.to_owned()];
    }
    let indent: String = line.chars().take_while(|c| *c == ' ').collect();
    let mut out = Vec::new();
    let mut rest = line;
    let mut first = true;
    while !rest.is_empty() {
        let room = if first { WRAP } else { WRAP - indent.len() - 4 };
        let cut = match rest.char_indices().nth(room) {
            None => rest.len(),
            Some((at, _)) => rest[..at]
                .rfind(' ')
                .filter(|i| *i > at / 2)
                .map_or(at, |i| i + 1),
        };
        let (piece, tail) = rest.split_at(cut);
        out.push(if first {
            piece.trim_end().to_owned()
        } else {
            format!("{indent}  … {}", piece.trim())
        });
        rest = tail;
        first = false;
    }
    out
}

/// The role a word names: a role as hits print it (`listitem`), or as the
/// map counts it (`list item`, `list items`, `text blocks`).
fn role_of(word: &str) -> Option<String> {
    let word = word.trim();
    match word {
        "img" => return Some("image".to_owned()),
        "statictext" => return Some("text".to_owned()),
        _ => {}
    }
    if let Some((role, _, _)) = super::map::COUNTED
        .iter()
        .find(|(role, one, many)| [*role, *one, *many].contains(&word))
    {
        return Some((*role).to_owned());
    }
    // Any other role hits can show (landmarks, regions…), as printed.
    KNOWN_ROLES.contains(&word).then(|| word.to_owned())
}

/// Roles a trimmed page keeps besides the counted ones.
const KNOWN_ROLES: [&str; 18] = [
    "main",
    "navigation",
    "dialog",
    "alertdialog",
    "alert",
    "form",
    "search",
    "banner",
    "contentinfo",
    "complementary",
    "region",
    "list",
    "cell",
    "columnheader",
    "rowheader",
    "grid",
    "rootwebarea",
    "menuitemcheckbox",
];

/// Roles as the model gave them, checked.
fn roles_of(given: &[String]) -> Result<Vec<String>, String> {
    let mut roles = Vec::new();
    for r in given {
        let r = r.trim().to_lowercase();
        if r.is_empty() {
            continue;
        }
        roles.push(role_of(&r).ok_or_else(|| {
            format!(
                "\"{r}\" isn't a role here; use one of the words the map and hits print: {}",
                super::map::role_words().join(", ")
            )
        })?);
    }
    Ok(roles)
}

/// What a cut list of hits says on its first line.
const NOT_ALL: &str = "This is NOT all of them; narrow with role, near or a longer query.";

/// Longest regex pattern.
const MAX_PATTERN: usize = 200;

/// One thing a node's text must hold.
enum Term {
    /// A word, anywhere.
    Word(String),
    /// A phrase, whole: not inside a longer word.
    Phrase(String),
    /// A regular expression (any case).
    Regex(regex::Regex),
}

impl Term {
    /// Whether lowercased `text` holds it.
    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Word(w) => text.contains(w.as_str()),
            Self::Phrase(p) => text.match_indices(p.as_str()).any(|(at, _)| {
                let before = text[..at].chars().next_back();
                let after = text[at + p.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            }),
            Self::Regex(re) => re.is_match(text),
        }
    }
}

/// A model's pattern, compiled with size limits (the regex crate runs in
/// linear time: no backtracking).
fn compile(pattern: &str) -> Result<regex::Regex, String> {
    if pattern.chars().count() > MAX_PATTERN {
        return Err(format!("regex is longer than {MAX_PATTERN} characters"));
    }
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(1 << 20)
        .dfa_size_limit(1 << 20)
        .build()
        .map_err(|e| {
            let why = e.to_string();
            let why = why.lines().last().unwrap_or("").trim().to_owned();
            format!("regex couldn't be read: {}", cut(&why, 120))
        })
}
