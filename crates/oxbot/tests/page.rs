//! The page's logic, run in Node.js against a do-nothing DOM
//! (`tests/page/*.js`). Skipped when Node.js isn't installed.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::PathBuf;
use std::process::Command;

/// Runs `tests/page/<name>` on the page's script; panics unless it says
/// "ALL OK". Returns false when Node.js isn't installed.
fn run_page_test(name: &str) -> bool {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipped: Node.js isn't installed");
        return false;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let page = std::fs::read_to_string(dir.join("ui/index.html")).unwrap();
    let start = page.rfind("<script>").unwrap() + "<script>".len();
    let end = page.rfind("</script>").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("page.js");
    std::fs::write(&script, &page[start..end]).unwrap();
    let out = Command::new("node")
        .arg(dir.join("tests/page").join(name))
        .arg(&script)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("ALL OK"), "{text}");
    true
}

#[test]
fn the_page_keeps_history_windows_straight() {
    run_page_test("history.js");
}

#[test]
fn the_page_puts_the_newest_card_nearest_the_box() {
    run_page_test("cards.js");
}

#[test]
fn the_page_groups_files_and_finds_bots() {
    run_page_test("sidebar.js");
}

#[test]
fn the_page_turns_a_changed_schedule_back_into_its_rule() {
    run_page_test("schedules.js");
}
