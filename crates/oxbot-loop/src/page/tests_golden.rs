//! Golden tests over realistic fixtures in `tests/fixtures/snapshots/`
//! (made by `tests/fixtures/make_snapshots.py`). Run with
//! `UPDATE_GOLDEN=1 cargo test` to rewrite the expected files, then review
//! them by eye: they are what a model sees.

use std::path::PathBuf;

use std::fmt::Write as _;

use super::{FindQuery, Snapshot, est};

/// One find case: query, roles, near, limit.
type Case<'a> = (Option<&'a str>, &'a [&'a str], Option<&'a str>, usize);

/// The fixture pages.
pub(super) const PAGES: [&str; 7] = [
    "search", "product", "article", "table", "feed", "modal", "form",
];

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/snapshots")
}

/// The raw text of fixture `name`.
pub(crate) fn fixture(name: &str) -> String {
    let path = dir().join(format!("{name}.txt"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Compares `actual` with `<name>.<kind>.txt`, or rewrites it when
/// `UPDATE_GOLDEN=1`.
pub(crate) fn golden(name: &str, kind: &str, actual: &str) {
    let path = dir().join(format!("{name}.{kind}.txt"));
    if std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {} (run with UPDATE_GOLDEN=1): {e}", path.display()));
    if expected != actual {
        let line = expected
            .lines()
            .zip(actual.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| expected.lines().count().min(actual.lines().count()));
        panic!(
            "{} differs at line {}:\nexpected: {:?}\nactual:   {:?}\n(UPDATE_GOLDEN=1 rewrites it)",
            path.display(),
            line + 1,
            expected.lines().nth(line),
            actual.lines().nth(line)
        );
    }
}

/// A strictness function like the gate's: paying beats everything.
pub(crate) fn strict(label: &str) -> u8 {
    let l = label.to_lowercase();
    if l.contains("pay") || l.contains("buy") || l.contains("delete") {
        2
    } else {
        u8::from(l.starts_with("button") || l.starts_with("link"))
    }
}

pub(super) fn snap(name: &str) -> Snapshot {
    Snapshot::parse(&fixture(name), &strict).unwrap()
}

#[test]
fn maps_and_full_renders_match_goldens() {
    for name in PAGES {
        let s = snap(name);
        let t = s.trim().with_handle("r12");
        let map = t.map(&s, "r12");
        assert!(est(&map) <= 1000, "{name} map is {} tokens", est(&map));
        golden(name, "map", &format!("{map}\n"));
        golden(name, "full", &format!("{}\n", t.render()));
    }
}

#[test]
fn search_tools_match_goldens() {
    let s = snap("search");
    let t = s.trim().with_handle("r12");
    let mut out = String::new();
    let queries: [Case<'_>; 5] = [
        (Some("model 250"), &[], None, 20),
        (Some("lamp $24"), &[], None, 5),
        (None, &["checkbox"], None, 20),
        (None, &["link"], Some("Pagination"), 20),
        (Some("next"), &["link", "button"], None, 20),
    ];
    for (query, roles, near, limit) in queries {
        let q = FindQuery {
            exact: false,
            regex: None,
            query,
            roles: roles.iter().map(|r| (*r).to_owned()).collect(),
            near,
            limit,
        };
        let _ = writeln!(
            out,
            "### find {query:?} {roles:?} near {near:?} limit {limit}"
        );
        out.push_str(&t.find(&q, 4000).unwrap());
        out.push_str("\n\n");
    }
    out.push_str("### read_section 3_61 (the results list), 30 lines\n");
    out.push_str(&t.section("3_61", 1, 30, 4000).unwrap());
    out.push('\n');
    golden("search", "tools", &out);
}

#[test]
fn render_within_matches_golden() {
    let s = snap("search");
    let t = s.trim();
    let (text, omitted) = t.render_within(600, "r12");
    assert!(est(&text) <= 600);
    assert!(omitted > 0);
    golden("search", "within", &format!("{text}\n"));
}
