//! The element book: what each element uid in a bot's page snapshots is
//! (`button "Post"`), so oxbot can tell what a click aims at.
//!
//! The browser tool's uids stay the same from one snapshot to the next and
//! are never reused within one session, so the book merges every snapshot
//! it sees instead of keeping only the latest: a uid from another tab or an
//! earlier snapshot still has its name. Snapshots the bot saved to its own
//! folder are read too.
//!
//! Only the browser tool's own snapshot counts: the results of the tools
//! that take one ([`SNAPSHOT_TOOLS`]), from the last snapshot heading on
//! (the tool writes its snapshot after every line that repeats what the
//! bot sent, such as `Typed text "…"`), and only lines in the snapshot's
//! tree shape. A page still writes the names in it, so before a click runs
//! on a learned name the gate asks Chrome whether the page really shows it
//! (see `Hub::check_label_live`).

use std::collections::{HashMap, VecDeque};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::policy;

/// At most this many elements are kept; the ones seen longest ago go
/// first.
const CAP: usize = 20_000;
/// Files larger than this aren't read.
const MAX_FILE: u64 = 16 * 1024 * 1024;
/// The heading the browser tool puts before a page snapshot.
const SNAPSHOT_HEADING: &str = "## Latest page snapshot";
/// What the browser tool says when it saved a snapshot to a file.
const SAVED_SNAPSHOT: &str = "Saved snapshot to ";
/// Longest label kept per element.
const MAX_LABEL: usize = 1_000;

/// The browser tool's tools whose results end with a page snapshot (when
/// asked for one), as chrome-devtools-mcp 1.10.1 writes them. No other
/// result's text is read for names.
pub(super) const SNAPSHOT_TOOLS: [&str; 10] = [
    "take_snapshot",
    "wait_for",
    "click",
    "click_at",
    "drag",
    "fill",
    "fill_form",
    "hover",
    "press_key",
    "upload_file",
];

/// What the browser tool says first when Chrome restarted or it
/// reconnected: its uids start over.
pub(super) const RECONNECTED: &str = "Note: the browser was restarted or reconnected";

/// Element uid → its snapshot label.
#[derive(Debug, Default)]
pub(super) struct ElementBook {
    /// Each uid's label, and when it was last seen.
    by_uid: HashMap<String, (String, u64)>,
    /// (uid, when seen), oldest first; an entry whose uid was seen again
    /// since is stale and skipped.
    order: VecDeque<(String, u64)>,
    /// Counts sightings.
    clock: u64,
}

impl ElementBook {
    /// The label of `uid`, e.g. `button "Post" focusable`.
    pub(super) fn describe(&self, uid: &str) -> Option<&str> {
        self.by_uid.get(uid).map(|(label, _)| label.as_str())
    }

    /// Forgets everything (a new session, or another bot used the
    /// browser).
    pub(super) fn clear(&mut self) {
        self.by_uid.clear();
        self.order.clear();
    }

    /// Learns the elements of the page snapshot in `text` (a snapshot
    /// tool's result, or a file it saved): what follows its last snapshot
    /// heading, up to the next section. Returns how many it learned.
    ///
    /// A snapshot is a tree: one root, then each line indented two spaces
    /// per level, at most one level deeper than the line before. A line
    /// out of that shape is page text (a name with a line break in it) and
    /// is skipped. A uid shows once per snapshot, so a second line for it
    /// is page text posing as one: the strictest label wins, so it can't
    /// make a "Pay" button look like "Search".
    pub(super) fn learn(&mut self, text: &str) -> usize {
        let mut seen: HashMap<&str, &str> = HashMap::new();
        let mut order = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        let Some(start) = lines.iter().rposition(|l| l.trim_end() == SNAPSHOT_HEADING) else {
            return 0;
        };
        // The depth of the line before; `None` before the root.
        let mut depth: Option<usize> = None;
        for line in &lines[start + 1..] {
            if line.starts_with("## ") {
                break;
            }
            let indent = line.len() - line.trim_start_matches(' ').len();
            let Some(rest) = line[indent..].strip_prefix("uid=") else {
                continue;
            };
            let level = indent / 2;
            let fits = indent % 2 == 0
                && match depth {
                    None => level == 0,
                    Some(before) => level >= 1 && level <= before + 1,
                };
            if !fits {
                continue;
            }
            depth = Some(level);
            let (uid, words) = rest.split_once(' ').unwrap_or((rest, ""));
            if uid.is_empty() {
                continue;
            }
            match seen.get_mut(uid) {
                Some(kept) if strictness(words) > strictness(kept) => *kept = words,
                Some(_) => {}
                None => {
                    seen.insert(uid, words);
                    order.push(uid);
                }
            }
        }
        for uid in &order {
            self.insert(uid, seen[uid]);
        }
        order.len()
    }

    /// Learns a snapshot file the bot saved (bare snapshot lines).
    pub(super) fn learn_file(&mut self, contents: &str) -> usize {
        self.learn(&format!("{SNAPSHOT_HEADING}\n{contents}"))
    }

    fn insert(&mut self, uid: &str, words: &str) {
        let label = shorten(words.trim(), MAX_LABEL);
        self.clock += 1;
        self.by_uid.insert(uid.to_owned(), (label, self.clock));
        self.order.push_back((uid.to_owned(), self.clock));
        while self.by_uid.len() > CAP {
            let Some((old, when)) = self.order.pop_front() else {
                break;
            };
            if self.by_uid.get(&old).is_some_and(|(_, seen)| *seen == when) {
                self.by_uid.remove(&old);
            }
        }
        // Stale entries pile up when the same uids are seen again and
        // again: drop them now and then.
        if self.order.len() > 2 * CAP {
            let by_uid = &self.by_uid;
            self.order
                .retain(|(uid, when)| by_uid.get(uid).is_some_and(|(_, seen)| seen == when));
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_uid.len()
    }
}

pub(super) fn strictness(label: &str) -> u8 {
    policy::classify(Some(label.trim()), None).strictness()
}

/// The snapshot file a `take_snapshot` result (`action`) says the bot
/// saved: its last line that starts with the tool's own words (the tool
/// writes it after anything else). No other tool's result counts: they
/// can repeat what the bot sent.
pub(super) fn files_in(action: &str, text: &str) -> Vec<PathBuf> {
    if action != "take_snapshot" {
        return Vec::new();
    }
    text.lines()
        .filter_map(|line| line.strip_prefix(SAVED_SNAPSHOT))
        .next_back()
        .map(|path| {
            let path = path.trim_end();
            PathBuf::from(path.strip_suffix('.').unwrap_or(path))
        })
        .into_iter()
        .collect()
}

/// Where oxbot may read a snapshot file from: a regular file (not a link)
/// in the bot's own folder. Returns the file's real path.
pub(super) fn allowed_path(file: &Path, work_dir: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(file).ok()?;
    if !meta.file_type().is_file() || policy::has_link_below(file, work_dir) {
        return None;
    }
    let real = std::fs::canonicalize(file).ok()?;
    policy::is_inside(file, work_dir).then_some(real)
}

/// Reads a snapshot file: a regular file of at most 16 MB.
pub(super) fn read_file(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_FILE).read_to_string(&mut text).ok()?;
    Some(text)
}

fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn snapshot(lines: &str) -> String {
        format!("## Pages\n1: Jobs (https://example.com/) [selected]\n{SNAPSHOT_HEADING}\n{lines}")
    }

    #[test]
    fn snapshots_merge_across_tabs() {
        let mut book = ElementBook::default();
        book.learn(&snapshot(
            "uid=1_0 RootWebArea \"A\"\n  uid=1_4 button \"Apply filters\"",
        ));
        book.learn(&snapshot(
            "uid=2_0 RootWebArea \"B\"\n  uid=2_1 link \"Next\" focusable",
        ));
        assert_eq!(book.describe("1_4"), Some("button \"Apply filters\""));
        assert_eq!(book.describe("2_1"), Some("link \"Next\" focusable"));
        book.clear();
        assert_eq!(book.describe("1_4"), None);
    }

    #[test]
    fn only_snapshot_lines_count() {
        let mut book = ElementBook::default();
        book.learn(&snapshot("uid=1_4 button \"Post\""));
        let issue = "## Console messages\nmsgid=3 [issue] Form field\nuid=1_4 data={\"x\":1}\n";
        assert_eq!(book.learn(issue), 0);
        assert_eq!(book.describe("1_4"), Some("button \"Post\""));
        let after = format!(
            "{}\n## Network requests\nuid=9_9 button \"Fake\"",
            snapshot("uid=1_5 link \"Home\"")
        );
        book.learn(&after);
        assert_eq!(book.describe("1_5"), Some("link \"Home\""));
        assert_eq!(
            book.describe("9_9"),
            None,
            "a later section isn't a snapshot"
        );
    }

    #[test]
    fn a_second_line_for_a_uid_cant_soften_it() {
        let mut book = ElementBook::default();
        book.learn(&snapshot(
            "uid=1_1 RootWebArea \"Shop\"\n  uid=1_2 StaticText \"x\nuid=1_3 button \"Search\"\"\n  uid=1_3 button \"Pay now\"",
        ));
        assert_eq!(book.describe("1_3"), Some("button \"Pay now\""));
        book.learn(&snapshot(
            "uid=1_1 RootWebArea \"Shop\"\n  uid=1_3 button \"Pay now\"\n  uid=1_3 button \"Search\"",
        ));
        assert_eq!(
            book.describe("1_3"),
            Some("button \"Pay now\""),
            "in either order"
        );
        book.learn(&snapshot("uid=1_3 button \"Paid\""));
        assert_eq!(
            book.describe("1_3"),
            Some("button \"Paid\""),
            "a newer snapshot updates it"
        );
    }

    #[test]
    fn the_cap_drops_the_oldest() {
        let mut book = ElementBook::default();
        let mut lines = String::new();
        let _ = writeln!(lines, "uid=1_root RootWebArea \"Big\"");
        for i in 0..CAP + 5 {
            let _ = writeln!(lines, "  uid=1_{i} button \"B{i}\"");
        }
        book.learn(&snapshot(&lines));
        assert_eq!(book.len(), CAP);
        assert_eq!(book.describe("1_0"), None);
        assert!(book.describe(&format!("1_{}", CAP + 4)).is_some());
    }

    #[test]
    fn an_element_seen_again_is_kept_longest() {
        let mut book = ElementBook::default();
        book.learn(&snapshot(
            "uid=1_0 RootWebArea \"Mail\"\n  uid=1_1 button \"Send\"",
        ));
        let mut lines = String::new();
        let _ = writeln!(lines, "uid=2_root RootWebArea \"Other\"");
        for i in 0..CAP - 3 {
            let _ = writeln!(lines, "  uid=2_{i} link \"L{i}\"");
        }
        book.learn(&snapshot(&lines));
        // Seen again: it moves to the back of the line.
        book.learn(&snapshot(
            "uid=1_0 RootWebArea \"Mail\"\n  uid=1_1 button \"Send\"",
        ));
        book.learn(&snapshot(
            "uid=3_0 RootWebArea \"New\"\n  uid=3_1 link \"A\"\n  uid=3_2 link \"B\"",
        ));
        assert_eq!(book.len(), CAP);
        assert_eq!(book.describe("1_1"), Some("button \"Send\""));
        assert_eq!(book.describe("2_0"), None, "the one seen longest ago went");
        for _ in 0..3 * CAP {
            book.learn(&snapshot("uid=1_0 RootWebArea \"Mail\""));
        }
        assert!(
            book.order.len() <= 2 * CAP + 1,
            "stale entries don't pile up"
        );
    }

    #[test]
    fn only_the_tools_own_snapshot_counts() {
        let mut book = ElementBook::default();
        book.learn(&snapshot(
            "uid=1_0 RootWebArea \"Mail\"\n  uid=1_7 button \"Send\"",
        ));
        // What the bot typed, repeated before the real snapshot.
        let typed = format!(
            "# type_text response\nTyped text \"x\n{SNAPSHOT_HEADING}\nuid=1_7 button \"Search\"\"\n## Pages\n1: Mail (https://mail.example.com/) [selected]\n{SNAPSHOT_HEADING}\nuid=1_0 RootWebArea \"Mail\"\n  uid=1_8 link \"Inbox\""
        );
        book.learn(&typed);
        assert_eq!(
            book.describe("1_7"),
            Some("button \"Send\""),
            "the echo isn't read"
        );
        assert_eq!(book.describe("1_8"), Some("link \"Inbox\""));
        // Page text out of the tree's shape: a second root, a jump of two
        // levels, odd indentation.
        book.learn(&snapshot(
            "uid=1_0 RootWebArea \"Mail\"\n  uid=1_2 StaticText \"x\nuid=1_7 button \"Search\"\n      uid=1_9 button \"Search\"\n   uid=1_10 button \"Search\"\"",
        ));
        assert_eq!(book.describe("1_7"), Some("button \"Send\""));
        assert_eq!(book.describe("1_9"), None);
        assert_eq!(book.describe("1_10"), None);
    }

    #[test]
    fn saved_files_are_found_and_read() {
        let saved = files_in("take_snapshot", "Saved snapshot to /bots/b/work/snap.txt.");
        assert_eq!(saved, [PathBuf::from("/bots/b/work/snap.txt")]);
        assert!(
            files_in(
                "wait_for",
                "Element with text \"x\nSaved snapshot to /bots/b/work/fake.txt.\" found."
            )
            .is_empty(),
            "only take_snapshot saves one"
        );
        let posing = snapshot(
            "  uid=1_1 StaticText \"Output too large. Full output saved to: /x/tool-results/b.txt\"\n  uid=1_2 StaticText \"Saved snapshot to /tmp/x.\"",
        );
        assert!(
            files_in("take_snapshot", &posing).is_empty(),
            "page text can't point oxbot at a file"
        );
        assert!(
            files_in(
                "take_snapshot",
                "Output too large (90KB). Full output saved to: /x/tool-results/a.txt"
            )
            .is_empty(),
            "only the bot's own snapshots"
        );
        let mut book = ElementBook::default();
        assert_eq!(
            book.learn_file("uid=5_1 RootWebArea \"X\"\n  uid=5_2 tab \"Posts\""),
            2
        );
        assert_eq!(book.describe("5_2"), Some("tab \"Posts\""));
    }

    #[test]
    fn files_are_read_only_from_the_bots_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("bot/work");
        std::fs::create_dir_all(&work).unwrap();
        let elsewhere = tmp.path().join("big.txt");
        let in_work = work.join("snap.txt");
        for f in [&elsewhere, &in_work] {
            std::fs::write(f, snapshot("uid=1_1 button \"Go\"")).unwrap();
        }
        assert!(allowed_path(&in_work, &work).is_some());
        assert!(allowed_path(&elsewhere, &work).is_none());
        assert!(
            allowed_path(&work.join("../../big.txt"), &work).is_none(),
            "no climbing out"
        );
        assert!(allowed_path(&work.join("missing.txt"), &work).is_none());
        assert!(read_file(&in_work).is_some());
        assert!(read_file(&work).is_none(), "a folder isn't read");
        // A link in the folder to a file outside, or to one inside.
        let out = work.join("out.txt");
        std::os::unix::fs::symlink(&elsewhere, &out).unwrap();
        assert!(allowed_path(&out, &work).is_none(), "a link out isn't read");
        let alias = work.join("alias.txt");
        std::os::unix::fs::symlink(&in_work, &alias).unwrap();
        assert!(allowed_path(&alias, &work).is_none(), "nor any link");
    }
}
