//! What oxbot needs on the Mac, and where to find it.
//!
//! The agent backend is Claude Code today; Codex is listed as optional
//! until oxbot can run bots on it.
//!
//! An app opened from Finder gets a bare `PATH` (`/usr/bin:/bin:…`), so
//! programs installed by Homebrew, nvm or Claude Code's installer aren't
//! found. [`search_path`] builds a `PATH` that includes the usual install
//! folders; oxbot gives it to every `claude`, npm and browser-tool process.
//! [`check`] reports what's there and what's missing, in plain words.

use std::path::{Path, PathBuf};
use std::time::Duration;

use oxbot_browser::DEFAULT_CHROME;
use oxbot_protocol::{SetupAction, SetupCheck, SetupReport};
use tokio::io::{AsyncBufReadExt as _, BufReader};

/// Folders where Claude Code, Node.js and npm are usually installed, after
/// whatever `PATH` oxbot was started with.
#[must_use]
pub fn search_path() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let extra = [
        home.join(".local/bin"),
        home.join(".claude/local"),
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

/// Checks Claude Code (installed and signed in), Google Chrome and, unless
/// the browser tool is built into the app (`tool_built_in`), Node.js.
pub async fn check(path: &str, tool_built_in: bool) -> SetupReport {
    let claude = find("claude", path);
    let version = match &claude {
        Some(program) => version_of(program, path).await,
        None => None,
    };
    let mut checks = vec![match (&claude, &version) {
        (Some(_), Some(v)) => ok("claude", "Claude Code", v),
        (Some(p), None) => offer(
            missing(
                "claude",
                "Claude Code",
                &format!("Found {} but it didn't start.", p.display()),
                "Press Install Claude Code to install it again.",
            ),
            SetupAction::InstallClaude,
        ),
        (None, _) => offer(
            missing(
                "claude",
                "Claude Code",
                "Not installed. oxbot's bots run on it.",
                "Press Install Claude Code. oxbot runs Anthropic's installer from claude.ai.",
            ),
            SetupAction::InstallClaude,
        ),
    }];
    let signed_in = match (&claude, &version) {
        (Some(program), Some(_)) => signed_in_to_claude(program, path).await,
        _ => false,
    };
    checks.push(if signed_in {
        ok("claude_login", "Claude sign-in", "Signed in.")
    } else if version.is_some() {
        offer(
            missing(
                "claude_login",
                "Claude sign-in",
                "Claude Code isn't signed in yet.",
                "Press Sign in to Claude. Claude's sign-in page opens in your browser; sign in with your Claude account.",
            ),
            SetupAction::SignInClaude,
        )
    } else {
        missing(
            "claude_login",
            "Claude sign-in",
            "Install Claude Code first.",
            "After installing, press Sign in to Claude.",
        )
    });
    checks.push(match find("codex", path) {
        Some(_) => SetupCheck {
            optional: true,
            ..ok(
                "codex",
                "Codex (optional)",
                "Installed. oxbot will be able to run bots on it once Codex support is built.",
            )
        },
        None => SetupCheck {
            optional: true,
            ..missing(
                "codex",
                "Codex (optional)",
                "Not needed today: oxbot runs bots on Claude Code. Codex support is coming.",
                "To have it ready, in Terminal run  npm install -g @openai/codex  then  codex login",
            )
        },
    });
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

/// Whether Claude Code is signed in, as `claude auth status` reports it.
/// oxbot never sees the token. If that command can't answer (an older
/// Claude Code), falls back to [`has_claude_account`].
async fn signed_in_to_claude(program: &Path, path: &str) -> bool {
    let run = tokio::process::Command::new(program)
        .args(["auth", "status", "--json"])
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let answer = tokio::time::timeout(Duration::from_secs(15), run)
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out.stdout).ok())
        .and_then(|v| v.get("loggedIn").and_then(serde_json::Value::as_bool));
    answer.unwrap_or_else(has_claude_account)
}

/// Whether Claude Code has an account or a key, without reading either.
fn has_claude_account() -> bool {
    if std::env::var_os("ANTHROPIC_API_KEY").is_some() {
        return true;
    }
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    if home.join(".claude/.credentials.json").exists() {
        return true;
    }
    // Claude Code records the signed-in account (not the key) here.
    std::fs::read_to_string(home.join(".claude.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|v| v.get("oauthAccount").is_some_and(|a| !a.is_null()))
}

/// Anthropic's installer for Claude Code. oxbot runs it the way Anthropic's
/// instructions do; it checks the download's checksum itself.
pub const CLAUDE_INSTALLER: &str = "curl -fsSL https://claude.ai/install.sh | bash";

/// Runs a setup step, passing each line of output to `progress`.
/// Installing takes a few minutes; signing in waits up to 15 minutes for
/// the user to finish in the browser.
///
/// # Errors
/// Says, in plain words, what went wrong.
pub async fn run(
    action: SetupAction,
    path: &str,
    mut progress: impl FnMut(String),
) -> Result<(), String> {
    // Both steps run through bash so their errors come out with their output.
    let mut cmd = tokio::process::Command::new("/bin/bash");
    match action {
        SetupAction::InstallClaude => {
            cmd.args(["-c", &format!("{CLAUDE_INSTALLER} 2>&1")]);
        }
        SetupAction::SignInClaude => {
            let program = find("claude", path).ok_or("Claude Code isn't installed yet.")?;
            cmd.args(["-c", r#""$OXBOT_CLAUDE_BIN" auth login 2>&1"#])
                .env("OXBOT_CLAUDE_BIN", program);
        }
    }
    let mut child = cmd
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't start it: {e}"))?;
    let work = async {
        if let Some(stdout) = child.stdout.take() {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(text) = progress_line(action, &plain(&line)) {
                    progress(text);
                }
            }
        }
        child.wait().await
    };
    match tokio::time::timeout(Duration::from_mins(15), work).await {
        Err(_) => {
            Err("It took too long, so oxbot stopped it. Press the button to try again.".to_owned())
        }
        Ok(Err(e)) => Err(format!("It stopped unexpectedly: {e}")),
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(format!("It didn't finish ({status}).")),
    }
}

/// What to show for a line of output, if anything. Signing in prints a
/// backup link that only works with a Terminal (it asks to paste a code), so
/// it's replaced with what to do.
fn progress_line(action: SetupAction, line: &str) -> Option<String> {
    if line.is_empty() {
        return None;
    }
    if action == SetupAction::SignInClaude {
        if line.starts_with("Opening browser") {
            return Some(
                "Claude's sign-in page opened in your browser. Sign in there; this finishes by itself."
                    .to_owned(),
            );
        }
        if line.starts_with("If the browser didn't open") {
            return Some(
                "No page? Make sure a browser is set as your default, then press Sign in to Claude again."
                    .to_owned(),
            );
        }
    }
    Some(line.to_owned())
}

/// A line of installer output without terminal colors or prompts.
fn plain(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip an escape sequence like ESC [ 31 m.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !c.is_control() {
            out.push(c);
        }
    }
    out.trim()
        .trim_end_matches("Paste code here if prompted >")
        .trim()
        .to_owned()
}

/// `check`, offering `action` as a button.
fn offer(check: SetupCheck, action: SetupAction) -> SetupCheck {
    SetupCheck {
        action: Some(action),
        ..check
    }
}

async fn version_of(program: &Path, path: &str) -> Option<String> {
    let run = tokio::process::Command::new(program)
        .arg("--version")
        .env("PATH", path)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then(|| format!("Installed ({text})."))
}

fn ok(id: &str, name: &str, detail: &str) -> SetupCheck {
    SetupCheck {
        id: id.to_owned(),
        name: name.to_owned(),
        ok: true,
        detail: detail.to_owned(),
        fix: None,
        optional: false,
        action: None,
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
        action: None,
    }
}

/// The name messages from this Mac are sent as: the first name of the Mac
/// account (`Jayadev`); if it has none, the device's name (`device`, e.g.
/// `Jay's MacBook Air`), so each device still shows up as someone. Phones
/// will do the same with the name given at pairing, then the phone's name.
#[must_use]
pub fn local_person(device: &str) -> String {
    let full = std::process::Command::new("id")
        .arg("-F")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();
    pick_name(&full, device)
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
    fn the_terminal_only_sign_in_link_is_not_shown() {
        let backup =
            "If the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true";
        let shown = progress_line(SetupAction::SignInClaude, backup).unwrap();
        assert!(!shown.contains("https://"), "{shown}");
        assert_eq!(progress_line(SetupAction::InstallClaude, ""), None);
    }

    #[test]
    fn installer_output_loses_colors_and_prompts() {
        assert_eq!(plain("\u{1b}[31mError\u{1b}[0m: no"), "Error: no");
        assert_eq!(plain("Paste code here if prompted >"), "");
        assert_eq!(
            plain("  ✅ Installation complete!  "),
            "✅ Installation complete!"
        );
    }

    #[tokio::test]
    async fn a_bare_path_reports_what_is_missing() {
        let report = check("/nowhere", false).await;
        let claude = report.checks.iter().find(|c| c.id == "claude").unwrap();
        assert!(!claude.ok && claude.fix.is_some());
        assert_eq!(
            claude.action,
            Some(SetupAction::InstallClaude),
            "a button installs it"
        );
        assert!(!report.ready());
        assert_eq!(report.checks.len(), 5);
        // With the browser tool built in, Node.js isn't asked for.
        let built_in = check("/nowhere", true).await;
        assert!(built_in.checks.iter().all(|c| c.id != "node"));
        let codex = report.checks.iter().find(|c| c.id == "codex").unwrap();
        assert!(codex.optional && !codex.ok, "Codex is optional");
    }
}
