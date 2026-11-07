//! What oxbot needs on the Mac, and where to find it.
//!
//! Bots run on Codex CLI (`codex app-server`, installed separately), on
//! `OpenRouter`'s models.
//!
//! An app opened from Finder gets a bare `PATH` (`/usr/bin:/bin:…`), so
//! programs installed by Homebrew or nvm aren't found. [`search_path`]
//! builds a `PATH` that includes the usual install folders; oxbot gives it
//! to every `codex`, npm and browser-tool process. [`check`] and
//! [`codex_check`] report what's there and what's missing, in plain words.

use std::path::{Path, PathBuf};

use oxbot_browser::DEFAULT_CHROME;
use oxbot_protocol::{CodexView, SetupCheck, SetupReport};

/// Folders where Codex, Node.js and npm are usually installed, after
/// whatever `PATH` oxbot was started with.
#[must_use]
pub fn search_path() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let extra = [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join(".bun/bin"),
        home.join(".asdf/shims"),
        home.join("Library/pnpm"),
    ];
    for dir in extra.into_iter().chain(newest_nvm_node(&home)) {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    for dir in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let dir = PathBuf::from(dir);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// nvm's newest installed Node.js, e.g. `~/.nvm/versions/node/v22.1.0/bin`.
fn newest_nvm_node(home: &Path) -> Option<PathBuf> {
    let root = home.join(".nvm/versions/node");
    let mut versions: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(&root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let numbers = name
                .trim_start_matches('v')
                .split('.')
                .map(|n| n.parse().ok())
                .collect::<Option<Vec<u64>>>()?;
            Some((numbers, e.path().join("bin")))
        })
        .collect();
    versions.sort();
    versions.pop().map(|(_, bin)| bin)
}

/// `program` in `path` (a `PATH` value), if it's there.
#[must_use]
pub fn find(program: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

/// Checks Google Chrome and, unless the browser tool is built into the
/// app (`tool_built_in`), Node.js.
#[must_use]
pub fn check(path: &str, tool_built_in: bool) -> SetupReport {
    let mut checks = Vec::new();
    checks.push(if Path::new(DEFAULT_CHROME).exists() {
        ok("chrome", "Google Chrome", "Installed.")
    } else {
        missing(
            "chrome",
            "Google Chrome",
            "Not installed.",
            "Get it from google.com/chrome and drag it into Applications.",
        )
    });
    if !tool_built_in {
        checks.push(match (find("node", path), find("npm", path)) {
            (Some(_), Some(_)) => ok("node", "Node.js", "Installed."),
            _ => missing(
                "node",
                "Node.js",
                "Not installed. oxbot uses it once to set up the browser tool.",
                "Get the LTS installer from nodejs.org and run it.",
            ),
        });
    }
    SetupReport { checks }
}

/// Codex, as the hub found it (see `Hub::codex_view`): bots run on it.
#[must_use]
pub fn codex_check(view: &CodexView) -> SetupCheck {
    match (&view.problem, &view.version) {
        (None, Some(version)) => ok("codex", "AI helper", &format!("Installed ({version}).")),
        (None, None) => ok("codex", "AI helper", "Installed."),
        (Some(why), _) => missing(
            "codex",
            "AI helper",
            why,
            "Install the AI helper, version 0.148 (in Terminal:  brew install codex  ), or say \
             where it is in Settings → General. Never sign in to it: oxbot gives it its own \
             settings.",
        ),
    }
}

/// Whether this oxbot can take still frames from video and dictate (both
/// through its Mac helper). Nice to have, not needed.
#[must_use]
pub fn mac_helper_check(helper: bool, dictation: bool, why_not: &str) -> SetupCheck {
    let check = if !helper {
        missing(
            "mac_helper",
            "Video and dictation",
            "Not available on the home computer: this oxbot was built without its video and dictation helper.",
            "Build oxbot with Xcode's Command Line Tools installed:  xcode-select --install  ",
        )
    } else if dictation {
        ok(
            "mac_helper",
            "Video and dictation",
            "Videos become still frames, and the microphone uses the home computer's on-device dictation.",
        )
    } else {
        SetupCheck {
            fix: Some(
                "Turn on Dictation in System Settings → Keyboard, then Check again.".to_owned(),
            ),
            ..missing(
                "mac_helper",
                "Video and dictation",
                &format!("Videos work. No microphone: {why_not}"),
                "",
            )
        }
    };
    SetupCheck {
        optional: true,
        ..check
    }
}

fn ok(id: &str, name: &str, detail: &str) -> SetupCheck {
    SetupCheck {
        id: id.to_owned(),
        name: name.to_owned(),
        ok: true,
        detail: detail.to_owned(),
        fix: None,
        optional: false,
    }
}

fn missing(id: &str, name: &str, detail: &str, fix: &str) -> SetupCheck {
    SetupCheck {
        id: id.to_owned(),
        name: name.to_owned(),
        ok: false,
        detail: detail.to_owned(),
        fix: Some(fix.to_owned()),
        optional: false,
    }
}

/// The name messages from this Mac are sent as: the first name of the Mac
/// account (`Jayadev`); if it has none, the device's name (`device`, e.g.
/// `Jay's MacBook Air`), so each device still shows up as someone. Phones
/// will do the same with the name given at pairing, then the phone's name.
/// Also says whether the name is the Mac account's own (`false` for the
/// device name or `Me`, which only stand in for one).
#[must_use]
pub fn local_person_from(device: &str) -> (String, bool) {
    let full = std::process::Command::new("id")
        .arg("-F")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    (pick_name(&full, device), first_name(&full).is_some())
}

/// The account's first name, else the device name, else `Me`.
fn pick_name(account: &str, device: &str) -> String {
    first_name(account)
        .or_else(|| Some(device.trim().to_owned()).filter(|d| !d.is_empty()))
        .unwrap_or_else(|| "Me".to_owned())
}

fn first_name(full: &str) -> Option<String> {
    full.split_whitespace().next().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_path_has_the_usual_install_folders() {
        let path = search_path();
        for dir in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            ".local/bin",
        ] {
            assert!(path.contains(dir), "{dir} missing from {path}");
        }
    }

    #[test]
    fn programs_are_found_on_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("node"), "").unwrap();
        let path = format!("/nowhere:{}", tmp.path().display());
        assert_eq!(find("node", &path), Some(tmp.path().join("node")));
        assert_eq!(find("npm", &path), None);
    }

    #[test]
    fn the_first_name_is_used_then_the_device_name() {
        assert_eq!(pick_name("Jayadev Rajan", "Jay's Mac"), "Jayadev");
        assert_eq!(pick_name("  ", "Jay's Mac"), "Jay's Mac");
        assert_eq!(pick_name("", " "), "Me");
    }

    #[test]
    fn a_bare_path_reports_what_is_missing() {
        let report = check("/nowhere", false);
        let node = report.checks.iter().find(|c| c.id == "node").unwrap();
        assert!(!node.ok && node.fix.is_some());
        assert!(!report.ready());
        assert!(report.checks.iter().all(|c| !c.id.contains("claude")));
        // With the browser tool built in, Node.js isn't asked for.
        let built_in = check("/nowhere", true);
        assert!(built_in.checks.iter().all(|c| c.id != "node"));
    }

    #[test]
    fn codex_is_needed_and_says_what_is_wrong() {
        let found = codex_check(&CodexView {
            path: Some("/opt/homebrew/bin/codex".into()),
            version: Some("0.148.0".into()),
            problem: None,
            configured: String::new(),
        });
        assert!(found.ok && !found.optional);
        assert_eq!(found.name, "AI helper");
        assert_eq!(found.detail, "Installed (0.148.0).");
        let old = codex_check(&CodexView {
            path: Some("/usr/local/bin/codex".into()),
            version: Some("0.140.0".into()),
            problem: Some("The AI helper 0.140.0 isn't a version oxbot speaks.".into()),
            configured: String::new(),
        });
        assert!(!old.ok && !old.optional);
        assert!(old.fix.as_deref().unwrap().contains("brew install codex"));
    }
}
