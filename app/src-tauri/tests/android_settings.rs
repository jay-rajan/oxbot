//! oxbot's Android settings live outside the generated Android project,
//! which `cargo tauri android init` rewrites. This fails if they're missing
//! from it (fix: `scripts/android-apply.sh`).
#![allow(clippy::unwrap_used, reason = "test")]

use std::path::Path;

#[test]
fn the_android_project_has_oxbots_settings() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root.join("app/src-tauri/gen/android").exists() {
        return; // No Android project on this machine.
    }
    let out = std::process::Command::new(root.join("scripts/android-apply.sh"))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
