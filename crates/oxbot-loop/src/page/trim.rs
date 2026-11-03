//! Trimming (design §2.8 steps 1–4): drop noise, merge text, keep what a
//! model acts on, and mark runs of same-shaped siblings for folding.
//!
//! Choices where the design is silent:
//! - Text merges only within one block: the nearest ancestor that isn't an
//!   inline wrapper (`generic`, `strong`, …). So paragraphs stay apart and a
//!   listitem's spans join into one line.
//! - Text that repeats its kept parent's name (`link "Home"` › `"Home"`) is
//!   dropped; an unnamed listitem or cell whose only child is one text takes
//!   that text as its name.
//! - Other roles with a name (`paragraph`, `group "Size"`) become text
//!   lines; without a name they are see-through (their children move up).
//! - `pressed` is kept as state too (toggle buttons); an option's `value`
//!   is dropped when it equals its name (the browser tool copies it).
//! - A fold hides at least 3 siblings (a fold line for 1 or 2 saves
//!   nothing). A row named only by its cells' text drops that name.
//! - A slider's or spinbutton's `valuetext` (`$120k`) shows as its value.
//! - Text, images and table cells never fold (an article's paragraphs and a
//!   row's cells must stay readable), and neither does a sibling holding the
//!   focused node or a dialog.

use std::collections::{BTreeMap, HashMap};

use super::parse::{RawNode, host_of};
use super::{Snapshot, shape};

/// Longest name shown on a line.
pub(super) const NAME_CAP: usize = 200;
/// Longest value shown.
const VALUE_CAP: usize = 80;
/// Longest link query shown.
const QUERY_CAP: usize = 40;

/// Roles a model can act on.
pub(super) const INTERACTIVE: [&str; 17] = [
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "switch",
    "tab",
    "menuitem",
    "option",
    "slider",
    "spinbutton",
    "listbox",
    "menuitemcheckbox",
    "menuitemradio",
    "treeitem",
];
/// Landmarks (a `region` only counts with a name).
const LANDMARKS: [&str; 11] = [
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
];
/// Structure kept even without a name.
const CONTAINERS: [&str; 6] = ["list", "listitem", "table", "row", "grid", "article"];
/// Table cells.
const CELLS: [&str; 4] = ["cell", "gridcell", "columnheader", "rowheader"];
/// Plain text roles; these merge with their neighbours.
const TEXT_ROLES: [&str; 6] = ["", "StaticText", "text", "generic", "ignored", "none"];
/// Noise: dropped, children move up.
const NOISE: [&str; 3] = ["InlineTextBox", "LineBreak", "ListMarker"];
/// Inline wrappers: text inside them merges across them.
const INLINE: [&str; 16] = [
    "",
    "StaticText",
    "generic",
    "ignored",
    "none",
    "presentation",
    "strong",
    "emphasis",
    "mark",
    "time",
    "subscript",
    "superscript",
    "code",
    "insertion",
    "deletion",
    "abbr",
];
/// Dialog roles: always shown, never folded or cut.
pub(super) const ON_TOP: [&str; 3] = ["dialog", "alertdialog", "alert"];

/// What a kept node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Root,
    Interactive,
    Heading,
    Landmark,
    Container,
    Cell,
    Text,
    Image,
}

/// One node of the trimmed tree. Nodes are stored in document order, so a
/// subtree is the range `i..end`.
#[derive(Debug, Clone)]
pub(super) struct TNode {
    pub(super) uid: String,
    pub(super) role: String,
    pub(super) kind: Kind,
    /// Full name, whitespace collapsed (cut only when rendered).
    pub(super) name: String,
    /// Kept state, rendered (`checked`, `value="…"`, `url="/p/1"`).
    pub(super) attrs: Vec<String>,
    pub(super) level: u8,
    pub(super) focused: bool,
    pub(super) parent: Option<usize>,
    pub(super) depth: usize,
    pub(super) end: usize,
    pub(super) first_child: Option<usize>,
    pub(super) next_sibling: Option<usize>,
    /// Shape: roles of the node and its children to depth 2.
    pub(super) sig: u32,
}

/// How a node renders when repeats fold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fold {
    Show,
    /// First hidden sibling of a run: renders the fold line. `last` is the
    /// last hidden sibling.
    Start {
        count: usize,
        last: usize,
    },
    Hidden,
}

/// The trimmed tree.
#[derive(Debug, Clone)]
pub struct Trimmed {
    pub(super) nodes: Vec<TNode>,
    /// Every raw uid → the trimmed node that shows it.
    pub(super) index: HashMap<String, usize>,
    pub(super) fold: Vec<Fold>,
    /// The handle named in fold lines and headers (see [`Trimmed::with_handle`]).
    pub(super) handle: Option<String>,
}

/// What one raw node becomes.
enum Class {
    /// Not shown; children move up.
    Through,
    /// A text line; `merge` = may join the text line before it.
    Text {
        merge: bool,
    },
    Keep(Kind),
}

fn classify(n: &RawNode, has_name: bool) -> Class {
    let role = n.role.as_str();
    if NOISE.contains(&role) {
        return Class::Through;
    }
    if TEXT_ROLES.contains(&role) {
        return if has_name {
            Class::Text { merge: true }
        } else {
            Class::Through
        };
    }
    if role == "image" || role == "img" {
        return if has_name {
            Class::Keep(Kind::Image)
        } else {
            Class::Through
        };
    }
    if role == "RootWebArea" {
        return Class::Keep(Kind::Root);
    }
    if INTERACTIVE.contains(&role) {
        return Class::Keep(Kind::Interactive);
    }
    if role == "heading" {
        return Class::Keep(Kind::Heading);
    }
    if LANDMARKS.contains(&role) {
        return if role != "region" || has_name {
            Class::Keep(Kind::Landmark)
        } else {
            Class::Through
        };
    }
    if CONTAINERS.contains(&role) {
        return Class::Keep(Kind::Container);
    }
    if CELLS.contains(&role) {
        return Class::Keep(Kind::Cell);
    }
    if has_name {
        Class::Text { merge: false }
    } else {
        Class::Through
    }
}

/// Collapses runs of whitespace (newlines too) into one space.
pub(super) fn squash(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// `s` cut to `max` chars with `…` added when cut.
pub(super) fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// 1834 → "1,834".
pub(super) fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A link url as shown: path (query cut), with the host only when it
/// differs from the page's host. The fragment is dropped.
fn short_url(url: &str, page_host: &str) -> String {
    let url = squash(url);
    let Some((_, rest)) = url.split_once("://") else {
        return cut(&url, VALUE_CAP);
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    let split = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, h)| h)
        .to_ascii_lowercase();
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    let path = if path.is_empty() { "/" } else { path };
    let mut out = if host == page_host {
        String::new()
    } else {
        host
    };
    out.push_str(&cut(path, VALUE_CAP));
    if !query.is_empty() {
        out.push('?');
        out.push_str(&cut(query, QUERY_CAP));
    }
    out
}

/// The state attributes kept for a node, rendered, in name order.
fn kept_attrs(n: &RawNode, kind: Kind, name: &str, page_host: &str) -> Vec<String> {
    let mut out = Vec::new();
    // A slider's spoken value (`$120k`) says more than its number.
    let valuetext = n.attr("valuetext").filter(|t| !t.is_empty());
    for (k, v) in &n.attrs {
        let v = v.as_deref();
        match (k.as_str(), v) {
            ("checked" | "pressed", Some("mixed")) => out.push(format!("{k}=\"mixed\"")),
            ("invalid", Some(x)) if x != "true" => out.push(format!("invalid=\"{}\"", cut(x, 20))),
            (
                "checked" | "pressed" | "selected" | "expanded" | "disabled" | "focused"
                | "required" | "invalid",
                _,
            ) => out.push(k.clone()),
            ("level", Some(x)) if kind == Kind::Heading => out.push(format!("level=\"{x}\"")),
            ("url", Some(x)) if n.role == "link" => {
                out.push(format!("url=\"{}\"", short_url(x, page_host)));
            }
            ("value", Some(x)) if kind != Kind::Root => {
                let x = squash(valuetext.unwrap_or(x));
                if !(n.role == "option" && x == name) {
                    out.push(format!("value=\"{}\"", cut(&x, VALUE_CAP)));
                }
            }
            _ => {}
        }
    }
    out
}

/// Text that only repeats what its parent's name says.
fn repeats_parent(parent: &TNode, text: &str) -> bool {
    if parent.kind == Kind::Root || parent.name.is_empty() {
        return false;
    }
    if parent.name.len() > 10_000 {
        return parent.name == text;
    }
    parent.name.contains(text)
}

/// The nearest raw ancestor that is not an inline wrapper (text merges
/// within it).
fn block_of(raw: &[RawNode], i: usize, block: &[Option<usize>]) -> Option<usize> {
    let p = raw[i].parent?;
    if INLINE.contains(&raw[p].role.as_str()) {
        block[p]
    } else {
        Some(p)
    }
}

/// See [`Snapshot::trim`].
pub(super) fn build(snap: &Snapshot) -> Trimmed {
    let raw = &snap.nodes;
    let page_host = host_of(snap.url());
    let mut nodes: Vec<TNode> = Vec::new();
    // Where each raw node's children attach, and which node shows it.
    let mut attach: Vec<Option<usize>> = vec![None; raw.len()];
    let mut shown: Vec<Option<usize>> = vec![None; raw.len()];
    let mut block: Vec<Option<usize>> = vec![None; raw.len()];
    // For text nodes: the block they merge within.
    let mut text_block: Vec<Option<usize>> = Vec::new();
    for (i, n) in raw.iter().enumerate() {
        block[i] = block_of(raw, i, &block);
        let parent = n.parent.and_then(|p| attach[p]);
        let name = if n.extra.is_empty() {
            squash(&n.name)
        } else {
            squash(&format!("{} {}", n.name, n.extra))
        };
        attach[i] = parent;
        // A see-through node shows where its parent shows (an InlineTextBox
        // shows in its text line).
        shown[i] = n.parent.and_then(|p| shown[p]);
        match classify(n, !name.is_empty()) {
            Class::Through => {}
            Class::Text { merge } => {
                if parent.is_some_and(|p| repeats_parent(&nodes[p], &name)) {
                    continue;
                }
                let last = nodes.len().checked_sub(1);
                if let Some(l) = last
                    && merge
                    && nodes[l].kind == Kind::Text
                    && nodes[l].parent == parent
                    && text_block[l].is_some()
                    && text_block[l] == block[i]
                {
                    nodes[l].name.push(' ');
                    nodes[l].name.push_str(&name);
                    shown[i] = Some(l);
                    continue;
                }
                shown[i] = Some(nodes.len());
                text_block.push(if merge { block[i] } else { None });
                nodes.push(new_node(n, "text", Kind::Text, name, Vec::new(), parent));
            }
            Class::Keep(kind) => {
                let attrs = kept_attrs(n, kind, &name, &page_host);
                let role = if kind == Kind::Image {
                    "image"
                } else {
                    &n.role
                };
                let mut node = new_node(n, role, kind, name, attrs, parent);
                node.focused = n.attr("focused").is_some();
                if kind == Kind::Heading {
                    node.level = n.attr("level").and_then(|l| l.parse().ok()).unwrap_or(2);
                }
                attach[i] = Some(nodes.len());
                shown[i] = Some(nodes.len());
                text_block.push(None);
                nodes.push(node);
            }
        }
    }
    let (nodes, remap) = tidy(nodes);
    let mut index = HashMap::with_capacity(raw.len());
    for (i, n) in raw.iter().enumerate() {
        if let Some(t) = shown[i].and_then(|s| remap[s]) {
            index.insert(n.uid.clone(), t);
        }
    }
    let mut t = Trimmed {
        nodes,
        index,
        fold: Vec::new(),
        handle: None,
    };
    shape::link(&mut t.nodes);
    shape::plain_rows(&mut t.nodes);
    t.fold = shape::folds(&t.nodes);
    t
}

fn new_node(
    n: &RawNode,
    role: &str,
    kind: Kind,
    name: String,
    attrs: Vec<String>,
    parent: Option<usize>,
) -> TNode {
    TNode {
        uid: n.uid.clone(),
        role: role.to_owned(),
        kind,
        name,
        attrs,
        level: 0,
        focused: false,
        parent,
        depth: 0,
        end: 0,
        first_child: None,
        next_sibling: None,
        sig: 0,
    }
}

/// Drops empty unnamed containers and lets an unnamed listitem or cell take
/// its single text child as its name. Returns the kept nodes and old index →
/// new index (a dropped node maps to its nearest kept ancestor).
fn tidy(mut nodes: Vec<TNode>) -> (Vec<TNode>, Vec<Option<usize>>) {
    let n = nodes.len();
    let mut keep = vec![true; n];
    let mut kids = vec![0usize; n];
    let mut only = vec![0usize; n];
    for i in (0..n).rev() {
        let absorbs = nodes[i].name.is_empty()
            && (nodes[i].kind == Kind::Cell || nodes[i].role == "listitem");
        if absorbs && kids[i] == 1 && nodes[only[i]].kind == Kind::Text {
            let c = only[i];
            nodes[i].name = std::mem::take(&mut nodes[c].name);
            keep[c] = false;
            kids[i] = 0;
        }
        let droppable = matches!(nodes[i].kind, Kind::Landmark | Kind::Container);
        if droppable && kids[i] == 0 && nodes[i].name.is_empty() {
            keep[i] = false;
        }
        if keep[i]
            && let Some(p) = nodes[i].parent
        {
            kids[p] += 1;
            only[p] = i;
        }
    }
    let mut remap: Vec<Option<usize>> = vec![None; n];
    let mut next = 0;
    for i in 0..n {
        remap[i] = if keep[i] {
            next += 1;
            Some(next - 1)
        } else {
            nodes[i].parent.and_then(|p| remap[p])
        };
    }
    let mut out = Vec::with_capacity(next);
    for (i, mut node) in nodes.into_iter().enumerate() {
        if keep[i] {
            node.parent = node.parent.and_then(|p| remap[p]);
            out.push(node);
        }
    }
    (out, remap)
}

impl Trimmed {
    /// Names the handle (like `r12`) used in fold lines, `find` and
    /// `section` headers. Without it they say "the page".
    #[must_use]
    pub fn with_handle(mut self, handle: &str) -> Self {
        self.handle = Some(handle.to_owned());
        self
    }

    /// The uid of the first node whose name has `text` (tests).
    #[cfg(test)]
    pub(super) fn node_uid_with(&self, text: &str) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| n.name.contains(text))
            .map(|n| n.uid.clone())
    }

    /// uid → normalized trimmed line (no indent), every node including
    /// folded ones, for diffs.
    #[must_use]
    pub fn lines(&self) -> BTreeMap<String, String> {
        (0..self.nodes.len())
            .map(|i| (self.nodes[i].uid.clone(), self.line(i)))
            .collect()
    }

    /// One node's line: `uid=3_8 link "Desk lamp" url="/p/1"`.
    pub(super) fn line(&self, i: usize) -> String {
        self.line_cut(i, NAME_CAP)
    }

    /// The same with names cut at `max` chars.
    pub(super) fn line_cut(&self, i: usize, max: usize) -> String {
        let n = &self.nodes[i];
        let mut s = format!("uid={} {}", n.uid, n.role);
        if !n.name.is_empty() {
            s.push_str(" \"");
            push_name(&mut s, &n.name, max);
        }
        for a in &n.attrs {
            s.push(' ');
            s.push_str(a);
        }
        s
    }

    /// The fold line for a run starting at hidden node `i`.
    pub(super) fn fold_line(
        &self,
        i: usize,
        count: usize,
        last: usize,
        handle: Option<&str>,
    ) -> String {
        // How to see them: the parent's section shows every item.
        let parent = self.nodes[i]
            .parent
            .map_or_else(|| self.nodes[0].uid.clone(), |p| self.nodes[p].uid.clone());
        let how = match handle {
            Some(h) => format!("read_section(page: \"{h}\", uid: \"{parent}\") shows them all"),
            None => format!("read_section(uid: \"{parent}\") shows them all"),
        };
        format!(
            "… {} more like the above (uids {} … {}); {how}",
            thousands(count),
            self.nodes[i].uid,
            self.nodes[last].uid
        )
    }

    /// The trimmed node that shows `uid` (a merged or see-through uid maps
    /// to the node that holds it).
    pub(super) fn node_of(&self, uid: &str) -> Option<usize> {
        self.index.get(uid).copied()
    }
}

/// Pushes `name"` or, when long, `first 200…" (+1,834 chars)`.
pub(super) fn push_name(s: &mut String, name: &str, max: usize) {
    if let Some((i, _)) = name.char_indices().nth(max) {
        let more = name[i..].chars().count();
        s.push_str(&name[..i]);
        s.push_str("…\" (+");
        s.push_str(&thousands(more));
        s.push_str(" chars)");
    } else {
        s.push_str(name);
        s.push('"');
    }
}
