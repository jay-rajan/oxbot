//! The page's chat history logic, run in Node.js against a do-nothing DOM
//! (`tests/page/history.js`). Skipped when Node.js isn't installed.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::PathBuf;
use std::process::Command;

#[test]
fn the_page_keeps_history_windows_straight() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipped: Node.js isn't installed");
        return;
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let page = std::fs::read_to_string(dir.join("ui/index.html")).unwrap();
    let start = page.rfind("<script>").unwrap() + "<script>".len();
    let end = page.rfind("</script>").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("page.js");
    std::fs::write(&script, &page[start..end]).unwrap();
    let out = Command::new("node")
        .arg(dir.join("tests/page/history.js"))
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
}
