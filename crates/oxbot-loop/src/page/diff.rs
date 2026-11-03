//! Page diffs (design §2.8): what changed since the page the model last saw.
//!
//! Choices where the design is silent:
//! - `~` and `+` lines come in the new page's order, then `-` lines in uid
//!   order (the baseline keeps lines only, not the tree).
//! - Added runs fold when 3 or more same-shaped siblings were added whole;
//!   removed runs fold when 3 or more same-role groups repeat in uid order.
//! - A changed line shows `(was: "old name")` when the name changed, else
//!   `(was: <old state>)`. Changed lines never fold.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::parse::parse_node_text;
use super::trim::{Trimmed, cut, thousands};
use super::{Baseline, Snapshot, est, origin_path};

/// Shortest run that folds in a diff.
const FOLD_MIN: usize = 3;
/// Longest group of removed lines that can repeat.
const MAX_UNIT: usize = 30;

/// The diff of `new` against `base` (design §2.8), or None when the caller
/// should show the map or the full page instead: another origin+path, under
/// half of the new uids seen before, more than 40% of the lines changed, or
/// a diff over `cap_tokens`.
#[must_use]
pub fn diff(
    base: &Baseline,
    new: &Trimmed,
    new_snap: &Snapshot,
    new_handle: &str,
    cap_tokens: u64,
) -> Option<String> {
    let url = new_snap.url();
    if origin_path(url) != base.origin_path {
        return None;
    }
    let lines = new.lines();
    let n = new.nodes.len();
    let seen = new
        .nodes
        .iter()
        .filter(|x| base.lines.contains_key(&x.uid))
        .count();
    if n == 0 || seen * 2 < n {
        return None;
    }
    let mut state = vec![State::Same; n];
    let mut changes = 0usize;
    for (i, node) in new.nodes.iter().enumerate() {
        state[i] = match base.lines.get(&node.uid) {
            None => State::Added,
            Some(old) if *old != lines[&node.uid] => State::Changed,
            Some(_) => State::Same,
        };
        if state[i] != State::Same {
            changes += 1;
        }
    }
    let mut removed: Vec<(&String, &String)> = base
        .lines
        .iter()
        .filter(|(uid, _)| !lines.contains_key(*uid))
        .collect();
    changes += removed.len();
    if changes * 10 > n * 4 {
        return None;
    }
    removed.sort_by(|a, b| uid_order(a.0, b.0));
    let shown_url = url.split('#').next().unwrap_or(url);
    let shown_url = shown_url.split_once("://").map_or(shown_url, |(_, r)| r);
    let mut out = format!(
        "## Page changes since {} ({}), full page {new_handle}",
        base.handle,
        cut(shown_url, 200)
    );
    added_and_changed(new, &state, &base.lines, new_handle, &mut out);
    removed_lines(&removed, &mut out);
    let same = state.iter().filter(|s| **s == State::Same).count();
    let _ = write!(out, "\nunchanged: {} elements", thousands(same));
    (est(&out) <= cap_tokens).then_some(out)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Same,
    Added,
    Changed,
}

/// `~` and `+` lines in the new page's order.
fn added_and_changed(
    new: &Trimmed,
    state: &[State],
    old: &BTreeMap<String, String>,
    handle: &str,
    out: &mut String,
) {
    let nodes = &new.nodes;
    // Whether each node's whole subtree is added.
    let mut whole = vec![true; nodes.len()];
    for i in (0..nodes.len()).rev() {
        if state[i] != State::Added {
            whole[i] = false;
        }
        if !whole[i]
            && let Some(p) = nodes[i].parent
        {
            whole[p] = false;
        }
    }
    let mut last_context: Option<usize> = None;
    let mut i = 0;
    while i < nodes.len() {
        match state[i] {
            State::Same => i += 1,
            State::Changed => {
                out.push_str("\n~ ");
                out.push_str(&new.line(i));
                out.push_str(&was(&old[&nodes[i].uid], &new.line(i)));
                last_context = None;
                i += 1;
            }
            State::Added => {
                let context = nodes[i].parent;
                let run = same_shape_run(new, &whole, i);
                if run.len() >= FOLD_MIN {
                    let first = run[0];
                    let last = run[run.len() - 1];
                    let like = new.line_cut(first, 40);
                    let like = like.split_once(' ').map_or(like.as_str(), |(_, r)| r);
                    let _ = write!(
                        out,
                        "\n+ {} like: {like} (uids {} … {}); find_in_page(page: \"{handle}\", …) to see them",
                        thousands(run.len()),
                        nodes[first].uid,
                        nodes[last].uid
                    );
                    push_context(new, context, &mut last_context, out);
                    i = nodes[last].end;
                    continue;
                }
                let base_depth = nodes[i].depth;
                out.push_str("\n+ ");
                out.push_str(&new.line(i));
                push_context(new, context, &mut last_context, out);
                // The rest of this added subtree, indented under it.
                let mut j = i + 1;
                while j < nodes[i].end {
                    if state[j] == State::Added {
                        out.push_str("\n+ ");
                        out.push_str(&"  ".repeat(nodes[j].depth - base_depth));
                        out.push_str(&new.line(j));
                        j += 1;
                    } else if state[j] == State::Changed {
                        out.push_str("\n~ ");
                        out.push_str(&new.line(j));
                        out.push_str(&was(&old[&nodes[j].uid], &new.line(j)));
                        j += 1;
                    } else {
                        j += 1;
                    }
                }
                i = nodes[i].end;
            }
        }
    }
}

/// Shows `(in: form "Filters")` once for a run of added lines under the
/// same kept parent.
fn push_context(new: &Trimmed, context: Option<usize>, last: &mut Option<usize>, out: &mut String) {
    let Some(c) = context else {
        return;
    };
    if *last == Some(c) {
        return;
    }
    *last = Some(c);
    let node = &new.nodes[c];
    if node.name.is_empty() {
        let _ = write!(out, "  (in: {} uid={})", node.role, node.uid);
    } else {
        let _ = write!(out, "  (in: {} \"{}\")", node.role, cut(&node.name, 40));
    }
}

/// Wholly added siblings from `i` on with `i`'s shape.
fn same_shape_run(new: &Trimmed, whole: &[bool], i: usize) -> Vec<usize> {
    let nodes = &new.nodes;
    let mut run = Vec::new();
    let mut c = Some(i);
    while let Some(ci) = c {
        if !whole[ci] || nodes[ci].sig != nodes[i].sig {
            break;
        }
        run.push(ci);
        c = nodes[ci].next_sibling;
    }
    run
}

/// `(was: "old name")` when the name changed, else the old state.
fn was(old: &str, new: &str) -> String {
    let (or, on, oa) = split_line(old);
    let (nr, nn, na) = split_line(new);
    if or != nr {
        return format!("  (was: {})", old.split_once(' ').map_or(old, |(_, r)| r));
    }
    if on != nn {
        return format!("  (was: \"{}\")", cut(&on, 80));
    }
    if oa != na {
        let state = if oa.is_empty() {
            "no state".to_owned()
        } else {
            oa.join(" ")
        };
        return format!("  (was: {state})");
    }
    String::new()
}

/// Role, name and attribute tokens of one trimmed line.
fn split_line(line: &str) -> (String, String, Vec<String>) {
    let rest = line.split_once(' ').map_or("", |(_, r)| r);
    let (role, name, attrs) = parse_node_text(rest);
    let attrs = attrs
        .into_iter()
        .map(|(k, v)| match v {
            Some(v) => format!("{k}=\"{v}\""),
            None => k,
        })
        .collect();
    (role, name, attrs)
}

/// The role word of a trimmed line.
fn role_of(line: &str) -> &str {
    line.split(' ').nth(1).unwrap_or("")
}

/// `-` lines, repeated same-role groups folded.
fn removed_lines(removed: &[(&String, &String)], out: &mut String) {
    let mut i = 0;
    while i < removed.len() {
        let role = role_of(removed[i].1);
        let unit = removed[i + 1..]
            .iter()
            .take(MAX_UNIT)
            .position(|(_, l)| role_of(l) == role)
            .map(|p| p + 1);
        if let Some(len) = unit {
            let shape = |k: usize| -> Option<Vec<&str>> {
                let part = removed.get(i + k * len..i + (k + 1) * len)?;
                Some(part.iter().map(|(_, l)| role_of(l)).collect())
            };
            let first = shape(0);
            let mut count = 1;
            while shape(count).is_some() && shape(count) == first {
                count += 1;
            }
            if count >= FOLD_MIN {
                let like = cut(removed[i].1.split_once(' ').map_or("", |(_, r)| r), 60);
                let _ = write!(
                    out,
                    "\n- {} like: {like} (uids {} … {})",
                    thousands(count),
                    removed[i].0,
                    removed[i + (count - 1) * len].0
                );
                i += count * len;
                continue;
            }
        }
        out.push_str("\n- ");
        out.push_str(removed[i].1);
        i += 1;
    }
}

/// uids in page order: `3_9` before `3_10` (numbers compared as numbers).
pub(super) fn uid_order(left: &str, right: &str) -> Ordering {
    let mut lefts = left.split('_');
    let mut rights = right.split('_');
    loop {
        match (lefts.next(), rights.next()) {
            (None, None) => return left.cmp(right),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(lp), Some(rp)) => {
                let order = match (lp.parse::<u64>(), rp.parse::<u64>()) {
                    (Ok(ln), Ok(rn)) => ln.cmp(&rn),
                    _ => lp.cmp(rp),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}
