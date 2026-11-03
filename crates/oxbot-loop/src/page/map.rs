//! The page map (delta §A): a small outline of a big page. No body text:
//! url, title, counts, the page list, dialogs in full, the focused node,
//! headings, landmarks with what is in them, form controls, and a hint
//! naming the page tools.

use std::fmt::Write as _;

use super::trim::{Kind, NAME_CAP, ON_TOP, Trimmed, cut, squash, thousands};
use super::{Snapshot, est};

/// The map should fit this.
const HARD_CAP: u64 = 1000;
/// Longest name on a map line.
const MAP_NAME: usize = 60;
/// Most pages listed.
const MAX_PAGES: usize = 20;
/// Form control roles.
const CONTROLS: [&str; 9] = [
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "listbox",
    "spinbutton",
    "slider",
    "switch",
];
/// Roles counted per landmark: (role, one, many). Counts show in this order.
pub(super) const COUNTED: [(&str, &str, &str); 22] = [
    ("heading", "heading", "headings"),
    ("link", "link", "links"),
    ("button", "button", "buttons"),
    ("textbox", "textbox", "textboxes"),
    ("searchbox", "searchbox", "searchboxes"),
    ("combobox", "combobox", "comboboxes"),
    ("checkbox", "checkbox", "checkboxes"),
    ("radio", "radio button", "radio buttons"),
    ("switch", "switch", "switches"),
    ("slider", "slider", "sliders"),
    ("spinbutton", "spinbutton", "spinbuttons"),
    ("listbox", "listbox", "listboxes"),
    ("option", "option", "options"),
    ("tab", "tab", "tabs"),
    ("menuitem", "menu item", "menu items"),
    ("treeitem", "tree item", "tree items"),
    ("table", "table", "tables"),
    ("row", "row", "rows"),
    ("listitem", "list item", "list items"),
    ("article", "article", "articles"),
    ("image", "image", "images"),
    ("text", "text block", "text blocks"),
];

/// How much of each part to show.
#[derive(Clone, Copy)]
struct Limits {
    headings: usize,
    controls: usize,
    landmarks: usize,
    dialog_lines: usize,
}

/// Shrink steps, tried in order until the map fits: fewer headings, fewer
/// controls, then dialogs cut to 40 lines, then fewer landmarks.
const STEPS: [Limits; 8] = [
    lim(25, 30, 30, usize::MAX),
    lim(10, 30, 30, usize::MAX),
    lim(0, 30, 30, usize::MAX),
    lim(0, 10, 30, usize::MAX),
    lim(0, 0, 30, usize::MAX),
    lim(0, 0, 30, 40),
    lim(0, 0, 10, 40),
    lim(0, 0, 0, 20),
];

const fn lim(headings: usize, controls: usize, landmarks: usize, dialog_lines: usize) -> Limits {
    Limits {
        headings,
        controls,
        landmarks,
        dialog_lines,
    }
}

/// The counted role a node falls under.
fn counted(role: &str) -> Option<usize> {
    let role = match role {
        "menuitemcheckbox" | "menuitemradio" => "menuitem",
        "grid" => "table",
        r => r,
    };
    COUNTED.iter().position(|(r, _, _)| *r == role)
}

fn counts_text(counts: &[usize]) -> String {
    let parts: Vec<String> = counts
        .iter()
        .zip(COUNTED)
        .filter(|(c, _)| **c > 0)
        .map(|(c, (_, one, many))| {
            format!("{} {}", thousands(*c), if *c == 1 { one } else { many })
        })
        .collect();
    if parts.is_empty() {
        "nothing else".to_owned()
    } else {
        parts.join(", ")
    }
}

impl Trimmed {
    /// The page map (delta §A) for `handle` (like `r12`). Target ≤ 600
    /// tokens, hard cap 1000: when over, it shows fewer headings, then fewer
    /// controls, then cuts dialogs to 40 lines.
    #[must_use]
    pub fn map(&self, snap: &Snapshot, handle: &str) -> String {
        let mut text = String::new();
        for limits in STEPS {
            text = self.map_with(snap, handle, limits);
            if est(&text) <= HARD_CAP {
                return text;
            }
        }
        // Still too big (a huge page list or long dialog lines): cut at a
        // line break, keep the hint.
        let hint = hint(handle);
        let budget = usize::try_from(HARD_CAP * 3).unwrap_or(usize::MAX) - hint.len() - 32;
        let mut end = budget.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let end = text[..end].rfind('\n').unwrap_or(0);
        text.truncate(end);
        text.push_str("\n[map cut]\n");
        text.push_str(&hint);
        text
    }

    fn map_with(&self, snap: &Snapshot, handle: &str, limits: Limits) -> String {
        let mut out = String::new();
        let title = cut(&squash(snap.title()), 120);
        let url = cut(snap.url(), 200);
        let _ = writeln!(
            out,
            "Page map of {handle} (a big page: this is its outline, not its text)"
        );
        let _ = writeln!(out, "Page: \"{title}\" {url}");
        let _ = writeln!(
            out,
            "Elements: {} ({} lines when trimmed)",
            thousands(snap.element_count()),
            thousands(self.nodes.len())
        );
        Self::map_pages(&mut out, snap);
        self.map_on_top(&mut out, handle, limits.dialog_lines);
        if let Some(f) = self.nodes.iter().position(|n| n.focused) {
            let _ = writeln!(out, "Focused: {}", self.line_cut(f, MAP_NAME));
        }
        self.map_headings(&mut out, limits.headings);
        self.map_landmarks(&mut out, limits.landmarks);
        self.map_controls(&mut out, limits.controls);
        out.push_str(&hint(handle));
        out
    }

    fn map_pages(out: &mut String, snap: &Snapshot) {
        let pages = snap.pages();
        if pages.is_empty() {
            return;
        }
        out.push_str("Pages (pageId: title):\n");
        for p in pages.iter().take(MAX_PAGES) {
            let sel = if p.selected { " [selected]" } else { "" };
            let url = cut(&p.url, 120);
            if p.title.is_empty() {
                let _ = writeln!(out, "  {}: {url}{sel}", p.id);
            } else {
                let _ = writeln!(out, "  {}: {} ({url}){sel}", p.id, cut(&p.title, MAP_NAME));
            }
        }
        if pages.len() > MAX_PAGES {
            let _ = writeln!(out, "  + {} more pages", pages.len() - MAX_PAGES);
        }
    }

    /// Dialogs and alerts in full (trimmed, repeats folded), cut to
    /// `max_lines` lines.
    fn map_on_top(&self, out: &mut String, handle: &str, max_lines: usize) {
        let mut tops = Vec::new();
        let mut i = 0;
        while i < self.nodes.len() {
            if ON_TOP.contains(&self.nodes[i].role.as_str()) {
                tops.push(i);
                i = self.nodes[i].end;
            } else {
                i += 1;
            }
        }
        if tops.is_empty() {
            return;
        }
        out.push_str("On top:\n");
        let mut shown = 0;
        for t in tops {
            let lines = self.rendered(t..self.nodes[t].end, true, Some(handle), NAME_CAP);
            let room = max_lines.saturating_sub(shown);
            for l in lines.iter().take(room) {
                let _ = writeln!(out, "  {}", l.text);
            }
            shown += lines.len().min(room);
            if lines.len() > room {
                let _ = writeln!(
                    out,
                    "  … {} more lines; read_section(page: \"{handle}\", uid: \"{}\")",
                    lines.len() - room,
                    self.nodes[t].uid
                );
            }
        }
    }

    fn map_headings(&self, out: &mut String, max: usize) {
        let heads: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| self.nodes[i].kind == Kind::Heading && self.nodes[i].level <= 3)
            .collect();
        if heads.is_empty() {
            return;
        }
        out.push_str("Headings:\n");
        for &h in heads.iter().take(max) {
            let n = &self.nodes[h];
            let indent = "  ".repeat(usize::from(n.level.max(1)));
            let _ = writeln!(
                out,
                "{indent}h{} uid={} \"{}\"",
                n.level,
                n.uid,
                cut(&n.name, MAP_NAME)
            );
        }
        if heads.len() > max {
            let _ = writeln!(
                out,
                "  + {} more headings; find_in_page(role: \"heading\") lists them",
                heads.len() - max
            );
        }
    }

    /// Landmarks in document order (nested ones indented), each with the
    /// counts of what is in it but not in a landmark inside it.
    fn map_landmarks(&self, out: &mut String, max: usize) {
        let nodes = &self.nodes;
        let n = nodes.len();
        let is_mark =
            |i: usize| nodes[i].kind == Kind::Landmark && !ON_TOP.contains(&nodes[i].role.as_str());
        // Owner landmark (or usize::MAX for none), and the dialog flag.
        let mut owner = vec![usize::MAX; n];
        let mut in_top = vec![false; n];
        let mut marks: Vec<usize> = Vec::new();
        let mut slot = vec![usize::MAX; n];
        let mut nest = vec![0usize; n];
        for i in 0..n {
            if let Some(p) = nodes[i].parent {
                owner[i] = owner[p];
                in_top[i] = in_top[p];
                nest[i] = nest[p];
            }
            if ON_TOP.contains(&nodes[i].role.as_str()) {
                in_top[i] = true;
            }
            if is_mark(i) && !in_top[i] {
                slot[i] = marks.len();
                marks.push(i);
                owner[i] = i;
                nest[i] += 1;
            }
        }
        let mut counts = vec![[0usize; COUNTED.len()]; marks.len() + 1];
        let outside = marks.len();
        for i in 0..n {
            if in_top[i] || slot[i] != usize::MAX || nodes[i].kind == Kind::Root {
                continue;
            }
            if let Some(c) = counted(&nodes[i].role) {
                let s = if owner[i] == usize::MAX {
                    outside
                } else {
                    slot[owner[i]]
                };
                counts[s][c] += 1;
            }
        }
        if marks.is_empty() && counts[outside].iter().all(|c| *c == 0) {
            return;
        }
        out.push_str("Landmarks (what each holds):\n");
        for &m in marks.iter().take(max) {
            let node = &nodes[m];
            let indent = "  ".repeat(nest[m]);
            let name = if node.name.is_empty() {
                String::new()
            } else {
                format!(" \"{}\"", cut(&node.name, MAP_NAME))
            };
            let _ = writeln!(
                out,
                "{indent}{}{name} uid={}: {}",
                node.role,
                node.uid,
                counts_text(&counts[slot[m]])
            );
        }
        if marks.len() > max {
            let _ = writeln!(out, "  + {} more landmarks", marks.len() - max);
        }
        if counts[outside].iter().any(|c| *c > 0) {
            let _ = writeln!(
                out,
                "  (outside landmarks): {}",
                counts_text(&counts[outside])
            );
        }
    }

    /// Form controls with uid, name and state, then counts by role.
    fn map_controls(&self, out: &mut String, max: usize) {
        let controls: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| CONTROLS.contains(&self.nodes[i].role.as_str()))
            .collect();
        if controls.is_empty() {
            return;
        }
        let _ = writeln!(out, "Form controls ({}):", thousands(controls.len()));
        for &c in controls.iter().take(max) {
            let _ = writeln!(out, "  {}", self.line_cut(c, MAP_NAME));
        }
        if controls.len() > max {
            let rest = &controls[max..];
            let parts: Vec<String> = CONTROLS
                .iter()
                .filter_map(|role| {
                    let k = rest
                        .iter()
                        .filter(|&&c| self.nodes[c].role == *role)
                        .count();
                    (k > 0).then(|| format!("{k} {role}"))
                })
                .collect();
            let _ = writeln!(out, "  + {} more: {}", rest.len(), parts.join(", "));
        }
    }
}

/// The last line of a map.
fn hint(handle: &str) -> String {
    format!(
        "Look inside with find_in_page(page: \"{handle}\", query or role), read_section(page: \"{handle}\", uid), or ask_page(page: \"{handle}\", question)."
    )
}

/// The role words `find_in_page` takes, as hits print them.
pub(super) fn role_words() -> Vec<&'static str> {
    COUNTED.iter().map(|(role, _, _)| *role).collect()
}
