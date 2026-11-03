//! Rendering the trimmed tree, and fitting it to a budget (design §2.8
//! step 5).
//!
//! Fitting works on rendered lines. Each line gets a removal class, and
//! lines go in class order, bottom of the page first, until the text fits:
//! 1. text-only parts whose nearest control is more than 3 levels away,
//! 2. `contentinfo`, 3. `banner`, 4. `navigation` past its first 8 links,
//! 5. `region`s and `complementary` outside `main`, 6. the rest,
//! 7. `main`'s controls and headings (only when nothing else is left; the design says
//!    never, but a page with thousands of links in `main` would blow the
//!    budget otherwise), never: dialogs, alerts and the focused node's
//!    ancestry.
//!
//! A line's class is at least that of everything under it, so a container
//! never goes before its children; a container left with no lines under it
//! goes too.

use super::est;
use super::trim::{Fold, Kind, NAME_CAP, ON_TOP, Trimmed};

/// Never removed.
const NEVER: u8 = 8;
/// `main`'s controls.
const MAIN_CONTROL: u8 = 7;

/// One rendered line.
pub(super) struct Line {
    pub(super) text: String,
    /// The node it shows (for a fold line: the first hidden node).
    pub(super) node: usize,
    /// For a fold line: the end of the hidden range.
    pub(super) hidden_end: Option<usize>,
}

impl Trimmed {
    /// All trimmed lines, repeats folded, as text (two-space indent per
    /// depth), no budget.
    #[must_use]
    pub fn render(&self) -> String {
        join(&self.rendered(0..self.nodes.len(), true, self.handle.as_deref(), NAME_CAP))
    }

    /// Estimated tokens of [`Trimmed::render`].
    #[must_use]
    pub fn est_tokens(&self) -> u64 {
        est(&self.render())
    }

    /// Like [`Trimmed::render`], then fits `cap_tokens` (step 5 of the
    /// design; see the module notes for the order). Ends with `[trimmed: N
    /// lines omitted; full page <handle>]` when anything went. Returns the
    /// text and the number of lines omitted.
    #[must_use]
    pub fn render_within(&self, cap_tokens: u64, handle: &str) -> (String, usize) {
        let lines = self.rendered(0..self.nodes.len(), true, Some(handle), NAME_CAP);
        let trailer = |n: usize| format!("[trimmed: {n} lines omitted; full page {handle}]");
        let (kept, omitted) = self.fit(&lines, cap_tokens, |n| trailer(n).len() + 1, false);
        let mut text = join_kept(&lines, &kept);
        if omitted > 0 {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&trailer(omitted));
        }
        (text, omitted)
    }

    /// Like [`Trimmed::render`] but without folding repeats (for
    /// `ask_page`: the question may need every item), cut to `cap_tokens`
    /// with step 5's order first, then at a line break. Returns the text and
    /// whether it was cut.
    #[must_use]
    pub fn render_unfolded_within(&self, cap_tokens: u64) -> (String, bool) {
        // Every item and every word: the question may need any of them.
        let lines = self.rendered(0..self.nodes.len(), false, None, usize::MAX);
        let (kept, omitted) = self.fit(&lines, cap_tokens, |_| 0, true);
        (join_kept(&lines, &kept), omitted > 0)
    }

    /// Renders nodes `range` (a subtree or the whole tree), indented from
    /// the range's first depth.
    pub(super) fn rendered(
        &self,
        range: std::ops::Range<usize>,
        fold: bool,
        handle: Option<&str>,
        names: usize,
    ) -> Vec<Line> {
        let base = self.nodes.get(range.start).map_or(0, |n| n.depth);
        let mut out = Vec::new();
        let mut i = range.start;
        while i < range.end {
            let n = &self.nodes[i];
            let indent = "  ".repeat(n.depth.saturating_sub(base));
            // The range's own first node always shows.
            let state = if i == range.start {
                Fold::Show
            } else {
                self.fold[i]
            };
            match state {
                Fold::Hidden if fold => i = n.end,
                Fold::Start { count, last } if fold => {
                    out.push(Line {
                        text: format!("{indent}{}", self.fold_line(i, count, last, handle)),
                        node: i,
                        hidden_end: Some(self.nodes[last].end),
                    });
                    i = self.nodes[last].end;
                }
                _ => {
                    out.push(Line {
                        text: format!("{indent}{}", self.line_cut(i, names)),
                        node: i,
                        hidden_end: None,
                    });
                    i += 1;
                }
            }
        }
        out
    }

    /// Chooses lines to keep so the text (plus `extra(omitted)` bytes)
    /// fits `cap`. `strict` also cuts never-removed lines at the end when
    /// they alone don't fit. Returns keep flags and the omitted count.
    fn fit(
        &self,
        lines: &[Line],
        cap: u64,
        extra: impl Fn(usize) -> usize,
        strict: bool,
    ) -> (Vec<bool>, usize) {
        let mut kept = vec![true; lines.len()];
        let mut bytes: usize = lines
            .iter()
            .map(|l| l.text.len() + 1)
            .sum::<usize>()
            .saturating_sub(1);
        let fits = |bytes: usize, omitted: usize| {
            let total = if omitted == 0 {
                bytes
            } else {
                bytes + extra(omitted)
            };
            (total as u64).div_ceil(3) <= cap
        };
        if fits(bytes, 0) {
            return (kept, 0);
        }
        let class = self.classes();
        let line_class: Vec<u8> = lines
            .iter()
            .map(|l| match l.hidden_end {
                Some(end) => class[l.node..end].iter().copied().max().unwrap_or(1),
                None => class[l.node],
            })
            .collect();
        let mut order: Vec<usize> = (0..lines.len())
            .filter(|&i| line_class[i] < NEVER)
            .collect();
        order.sort_by_key(|&i| (line_class[i], std::cmp::Reverse(i)));
        let mut omitted = 0;
        for i in order {
            if fits(bytes, omitted) {
                break;
            }
            kept[i] = false;
            bytes = bytes.saturating_sub(lines[i].text.len() + 1);
            omitted += 1;
        }
        // A container whose lines all went goes too (no empty shells).
        let mut has_kept = vec![false; self.nodes.len()];
        for i in (0..lines.len()).rev() {
            let node = lines[i].node;
            let parent_line = lines[i].hidden_end.is_none() && self.nodes[node].end > node + 1;
            if kept[i] && parent_line && !has_kept[node] && line_class[i] < NEVER {
                kept[i] = false;
                bytes = bytes.saturating_sub(lines[i].text.len() + 1);
                omitted += 1;
            }
            if kept[i]
                && let Some(p) = self.nodes[node].parent
            {
                has_kept[p] = true;
            }
        }
        if strict {
            for i in (0..lines.len()).rev() {
                if fits(bytes, omitted) {
                    break;
                }
                if kept[i] {
                    kept[i] = false;
                    bytes = bytes.saturating_sub(lines[i].text.len() + 1);
                    omitted += 1;
                }
            }
        }
        (kept, omitted)
    }

    /// The removal class of every node (see the module notes).
    fn classes(&self) -> Vec<u8> {
        let nodes = &self.nodes;
        let n = nodes.len();
        // Distance to the nearest control, down then up the tree.
        let mut dist = vec![u32::MAX / 2; n];
        let mut text_only = vec![false; n];
        for i in 0..n {
            if nodes[i].kind == Kind::Interactive {
                dist[i] = 0;
            }
            text_only[i] = matches!(
                nodes[i].kind,
                Kind::Text | Kind::Image | Kind::Container | Kind::Cell
            );
        }
        for i in (0..n).rev() {
            if let Some(p) = nodes[i].parent {
                dist[p] = dist[p].min(dist[i] + 1);
                if !text_only[i] {
                    text_only[p] = false;
                }
            }
        }
        // Where each node sits.
        let (dialog, main, footer, banner, nav, region) = (1u8, 2u8, 4u8, 8u8, 16u8, 32u8);
        let mut ctx = vec![0u8; n];
        let mut nav_root = vec![usize::MAX; n];
        let mut nav_links = vec![0u32; n];
        let mut class = vec![6u8; n];
        for i in 0..n {
            let mut c = nodes[i].parent.map_or(0, |p| ctx[p]);
            if let Some(p) = nodes[i].parent {
                dist[i] = dist[i].min(dist[p] + 1);
                nav_root[i] = nav_root[p];
            }
            match nodes[i].role.as_str() {
                r if ON_TOP.contains(&r) => c |= dialog,
                "main" => c |= main,
                "contentinfo" => c |= footer,
                "banner" => c |= banner,
                "navigation" => {
                    c |= nav;
                    nav_root[i] = i;
                }
                "region" | "complementary" => c |= region,
                _ => {}
            }
            ctx[i] = c;
            let mut link_rank = u32::MAX;
            if nodes[i].role == "link" && nav_root[i] != usize::MAX {
                nav_links[nav_root[i]] += 1;
                link_rank = nav_links[nav_root[i]];
            }
            class[i] = if c & dialog != 0 || nodes[i].focused {
                NEVER
            } else if matches!(nodes[i].kind, Kind::Interactive | Kind::Heading) && c & main != 0 {
                MAIN_CONTROL
            } else if text_only[i] && dist[i] > 3 {
                1
            } else if c & footer != 0 {
                2
            } else if c & banner != 0 {
                3
            } else if c & nav != 0 {
                if link_rank <= 8 { 6 } else { 4 }
            } else if c & region != 0 && c & main == 0 {
                5
            } else {
                6
            };
        }
        for i in (0..n).rev() {
            if let Some(p) = nodes[i].parent {
                class[p] = class[p].max(class[i]);
            }
        }
        class
    }
}

/// Lines joined with `\n`.
pub(super) fn join(lines: &[Line]) -> String {
    let mut s = String::with_capacity(lines.iter().map(|l| l.text.len() + 1).sum());
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            s.push('\n');
        }
        s.push_str(&l.text);
    }
    s
}

fn join_kept(lines: &[Line], kept: &[bool]) -> String {
    let mut s = String::new();
    for (l, _) in lines.iter().zip(kept).filter(|(_, k)| **k) {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&l.text);
    }
    s
}
