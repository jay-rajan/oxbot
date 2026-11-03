//! Tree shape: links, subtree ranges, shape signatures, and which runs of
//! same-shaped siblings fold.

use std::collections::HashMap;

use super::trim::{Fold, Kind, ON_TOP, TNode};

/// Folding starts when a run is longer than this.
const FOLD_OVER: usize = 6;
/// Folded runs keep this many at the start (and one at the end).
const FOLD_HEAD: usize = 5;
/// A fold hides at least this many siblings.
const FOLD_MIN_HIDDEN: usize = 3;

/// Fills depth, subtree ends, child and sibling links, and shapes.
pub(super) fn link(nodes: &mut [TNode]) {
    let n = nodes.len();
    let mut last_child: Vec<Option<usize>> = vec![None; n];
    let mut last_top: Option<usize> = None;
    for i in 0..n {
        nodes[i].end = i + 1;
        let prev = match nodes[i].parent {
            Some(p) => {
                nodes[i].depth = nodes[p].depth + 1;
                if nodes[p].first_child.is_none() {
                    nodes[p].first_child = Some(i);
                }
                last_child[p].replace(i)
            }
            None => last_top.replace(i),
        };
        if let Some(s) = prev {
            nodes[s].next_sibling = Some(i);
        }
    }
    for i in (0..n).rev() {
        if let Some(p) = nodes[i].parent {
            nodes[p].end = nodes[p].end.max(nodes[i].end);
        }
    }
    // Shapes: depth-1 then depth-2 signatures, interned to small ids.
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut intern = |s: String| -> u32 {
        let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        *ids.entry(s).or_insert(next)
    };
    let mut sig1 = vec![0u32; n];
    for i in 0..n {
        let mut s = format!("{}(", nodes[i].role);
        let mut c = nodes[i].first_child;
        while let Some(ci) = c {
            s.push_str(&nodes[ci].role);
            s.push(',');
            c = nodes[ci].next_sibling;
        }
        sig1[i] = intern(s);
    }
    for i in 0..n {
        let mut s = format!("{}[", nodes[i].role);
        let mut c = nodes[i].first_child;
        while let Some(ci) = c {
            s.push_str(&sig1[ci].to_string());
            s.push(',');
            c = nodes[ci].next_sibling;
        }
        nodes[i].sig = intern(s);
    }
}

/// A row named just by its cells' text drops the name (the cells show it).
pub(super) fn plain_rows(nodes: &mut [TNode]) {
    for i in 0..nodes.len() {
        if nodes[i].role != "row" || nodes[i].name.is_empty() {
            continue;
        }
        let mut joined = String::new();
        let mut c = nodes[i].first_child;
        while let Some(ci) = c {
            if !joined.is_empty() {
                joined.push(' ');
            }
            joined.push_str(&nodes[ci].name);
            if joined.len() > nodes[i].name.len() {
                break;
            }
            c = nodes[ci].next_sibling;
        }
        if joined == nodes[i].name {
            nodes[i].name.clear();
        }
    }
}

/// Whether each node is, or holds, the focused node or a dialog.
pub(super) fn holds_top(nodes: &[TNode]) -> Vec<bool> {
    let mut top: Vec<bool> = nodes
        .iter()
        .map(|n| n.focused || ON_TOP.contains(&n.role.as_str()))
        .collect();
    for i in (0..nodes.len()).rev() {
        if top[i]
            && let Some(p) = nodes[i].parent
        {
            top[p] = true;
        }
    }
    top
}

/// Marks runs of more than 6 same-shaped siblings: the first 5 and the last
/// one show, the middle folds into one line.
pub(super) fn folds(nodes: &[TNode]) -> Vec<Fold> {
    let n = nodes.len();
    let mut fold = vec![Fold::Show; n];
    let top = holds_top(nodes);
    let foldable =
        |i: usize| !top[i] && !matches!(nodes[i].kind, Kind::Text | Kind::Image | Kind::Cell);
    let mut firsts: Vec<usize> = nodes.iter().filter_map(|x| x.first_child).collect();
    if n > 0 {
        firsts.push(0);
    }
    let mut run: Vec<usize> = Vec::new();
    for first in firsts {
        let mut c = Some(first);
        run.clear();
        while let Some(ci) = c {
            if run
                .last()
                .is_some_and(|&l| !(foldable(ci) && nodes[l].sig == nodes[ci].sig))
            {
                mark_run(&run, &mut fold);
                run.clear();
            }
            if foldable(ci) {
                run.push(ci);
            }
            c = nodes[ci].next_sibling;
        }
        mark_run(&run, &mut fold);
    }
    fold
}

fn mark_run(run: &[usize], fold: &mut [Fold]) {
    // A fold line must stand for at least 3 lines to be worth it.
    if run.len() <= FOLD_OVER || run.len() - FOLD_HEAD - 1 < FOLD_MIN_HIDDEN {
        return;
    }
    let hidden = &run[FOLD_HEAD..run.len() - 1];
    for &h in hidden {
        fold[h] = Fold::Hidden;
    }
    fold[hidden[0]] = Fold::Start {
        count: hidden.len(),
        last: hidden[hidden.len() - 1],
    };
}
