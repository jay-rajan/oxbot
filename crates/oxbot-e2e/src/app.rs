//! The Mac app check: the built `oxbot.app` starts on the seed data, its
//! server answers, its window opens and shows the page, and its menu-bar
//! icon is there.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, anyhow};
use serde::Serialize;
use tokio::process::Command;

use crate::spec::Config;

/// One thing the app check looked at.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    /// What, in words.
    pub what: String,
    /// `pass`, `fail` or `skipped`.
    pub status: &'static str,
    /// What was found.
    pub detail: String,
}

/// The app check's result.
#[derive(Debug, Clone, Serialize)]
pub struct AppResult {
    /// Every check, in order.
    pub checks: Vec<Check>,
    /// A picture of the app's window, relative to the run folder.
    pub shot: Option<String>,
}

/// Lists the windows a process owns: `id layer width height` per line.
/// Window ids and sizes need no permission (titles would).
const WINDOWS_SWIFT: &str = r#"
import CoreGraphics
let pid = Int(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
for w in list where (w[kCGWindowOwnerPID as String] as? Int) == pid {
  let b = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
  let on = (w[kCGWindowIsOnscreen as String] as? Bool) ?? false
  print(w[kCGWindowNumber as String] ?? 0, w[kCGWindowLayer as String] ?? 0, b["Width"] ?? 0, b["Height"] ?? 0, on ? 1 : 0)
}
"#;

fn pass(what: &str, detail: impl Into<String>) -> Check {
    Check {
        what: what.to_owned(),
        status: "pass",
        detail: detail.into(),
    }
}
fn fail(what: &str, detail: impl Into<String>) -> Check {
    Check {
        what: what.to_owned(),
        status: "fail",
        detail: detail.into(),
    }
}

/// Runs the app check, writing its screenshot into `run_dir/shots/app/`.
///
/// # Errors
/// Fails only if the check itself can't be set up (no app built).
pub async fn check(repo: &Path, config: &Config, run_dir: &Path) -> anyhow::Result<AppResult> {
    let app = repo.join(
        config
            .app
            .as_ref()
            .ok_or_else(|| anyhow!("pr_test.toml has no `app`"))?,
    );
    let macos = app.join("Contents/MacOS");
    let exe = std::fs::read_dir(&macos)
        .with_context(|| format!("no app at {}; build it first", app.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        // The app itself, not its Mac helper next to it.
        .find(|p| p.is_file() && p.file_name().is_some_and(|n| n != HELPER))
        .ok_or_else(|| anyhow!("no program in {}", macos.display()))?;
    let home = tempfile::tempdir()?;
    crate::seed::make_home(repo, home.path())?;
    let port = crate::serve::free_port()?;
    let script = repo.join("e2e/scripts/default.toml");
    let mut checks = Vec::new();
    // The stand-in OpenRouter too, so the app never reaches the real one.
    let openrouter = match &config.fake_openrouter {
        Some(program) => Some(
            crate::serve::FakeOpenRouter::start(&repo.join(program), &script, None, None).await?,
        ),
        None => None,
    };
    let mut cmd = Command::new(&exe);
    if let Some(fake) = &openrouter {
        crate::serve::openrouter_env(&mut cmd, repo, config, fake, true);
    }
    let mut child = cmd
        .env("OXBOT_HOME", home.path())
        .env("OXBOT_PORT", port.to_string())
        .env("OXBOT_E2E", "1")
        .env("OXBOT_DEV_TOOLS", "1")
        .env("OXBOT_PERSON", &config.person)
        .env("OXBOT_FAKE_SCRIPT", &script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("starting the app")?;
    let pid = child.id().unwrap_or(0);

    let mut answered = false;
    for _ in 0..75 {
        if let Ok((200, page)) = crate::http::request(port, "GET", "/", None).await {
            answered = page.contains("oxbot");
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    checks.push(if answered {
        pass(
            "The app starts and its server answers",
            format!("port {port}, seed data"),
        )
    } else {
        fail(
            "The app starts and its server answers",
            "no answer within 15 s",
        )
    });

    tokio::time::sleep(Duration::from_secs(3)).await;
    let alive = matches!(child.try_wait(), Ok(None));
    checks.push(if alive {
        pass("It keeps running", format!("pid {pid}"))
    } else {
        fail("It keeps running", "the app quit by itself")
    });

    let (more, shot) = window_checks(pid, run_dir).await?;
    checks.extend(more);
    checks.extend(helper_checks(&app, port, home.path()).await);

    let _ = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    if tokio::time::timeout(Duration::from_secs(8), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    let pattern = format!("user-data-dir={}", home.path().display());
    let _ = std::process::Command::new("pkill")
        .args(["-f", &pattern])
        .status();
    Ok(AppResult { checks, shot })
}

/// oxbot's Mac helper, in `Contents/MacOS`.
const HELPER: &str = "oxbot-mac";

/// The Mac helper is inside and signed, the app says why it may use the
/// mic and speech, and its page offers the mic exactly where the helper
/// says this Mac can dictate on-device.
async fn helper_checks(app: &Path, port: u16, home: &Path) -> Vec<Check> {
    let mut checks = Vec::new();
    let helper = app.join("Contents/MacOS").join(HELPER);
    let what = "Its Mac helper (video frames, dictation) is inside";
    let version = Command::new(&helper).arg("version").output().await;
    match version {
        Ok(out) if String::from_utf8_lossy(&out.stdout).contains("\"version\"") => {
            checks.push(pass(
                what,
                String::from_utf8_lossy(&out.stdout).trim().to_owned(),
            ));
        }
        _ => {
            checks.push(fail(
                what,
                "Contents/MacOS/oxbot-mac is missing or doesn't run",
            ));
            return checks;
        }
    }
    let signed = Command::new("codesign")
        .args(["--verify", "--strict"])
        .arg(&helper)
        .status()
        .await
        .is_ok_and(|s| s.success());
    checks.push(if signed {
        pass("The helper is signed", "codesign --verify --strict")
    } else {
        fail("The helper is signed", "codesign says no")
    });
    let plist = app.join("Contents/Info.plist");
    let mut missing = Vec::new();
    for key in [
        "NSMicrophoneUsageDescription",
        "NSSpeechRecognitionUsageDescription",
    ] {
        let found = Command::new("plutil")
            .args(["-extract", key, "raw"])
            .arg(&plist)
            .output()
            .await
            .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty());
        if !found {
            missing.push(key);
        }
    }
    let what = "It says why it may use the microphone and speech recognition";
    checks.push(if missing.is_empty() {
        pass(what, "both usage strings are in Info.plist")
    } else {
        fail(what, format!("missing: {}", missing.join(", ")))
    });
    let can = Command::new(&helper)
        .arg("can-dictate")
        .output()
        .await
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(o.stdout.trim_ascii()).ok())
        .and_then(|v| v["ok"].as_bool())
        .unwrap_or(false);
    let what = "The mic shows only where on-device dictation exists";
    checks.push(match farm_settings(port, home).await {
        Some(settings) if settings["dictation"].as_bool() == Some(can) => pass(
            what,
            if can {
                "this Mac can dictate on-device: the Farm's page has the mic"
            } else {
                "this Mac can't dictate on-device: no mic"
            },
        ),
        Some(settings) => fail(
            what,
            format!(
                "the helper says {can}, the page was told {}",
                settings["dictation"]
            ),
        ),
        None => fail(what, "couldn't read the app's settings"),
    });
    checks
}

/// The settings the app's own window gets (it carries the Farm secret).
async fn farm_settings(port: u16, home: &Path) -> Option<serde_json::Value> {
    use futures_util::StreamExt as _;
    let key = std::fs::read_to_string(home.join("farm-key")).ok()?;
    let url = format!("ws://127.0.0.1:{port}/ws?k={}", key.trim());
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.ok()?;
    let read = async {
        while let Some(Ok(msg)) = ws.next().await {
            let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                continue;
            };
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            if v["type"] == "settings" {
                return Some(v);
            }
        }
        None
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .ok()?
}

/// `1280` or `1280.0` → 1280.
fn whole(text: &str) -> Option<u64> {
    text.split('.').next()?.parse().ok()
}

/// Looks at the app's windows: its main window (and a picture of it) and
/// its menu-bar icon.
async fn window_checks(pid: u32, run_dir: &Path) -> anyhow::Result<(Vec<Check>, Option<String>)> {
    let mut checks = Vec::new();
    let windows = list_windows(pid).await;
    // The app's window: its biggest on-screen normal window (helper
    // windows are small or off screen).
    let main = windows
        .iter()
        .filter(|w| w.layer == 0 && w.onscreen && w.width > 300 && w.height > 200)
        .max_by_key(|w| w.width * w.height);
    let tray = windows.iter().any(|w| w.layer == 25);
    let mut shot = None;
    match (&windows, main) {
        (_, Some(w)) => {
            checks.push(pass(
                "Its window opens",
                format!(
                    "{}×{} on screen: {}",
                    w.width,
                    w.height,
                    if w.onscreen { "yes" } else { "no" }
                ),
            ));
            let dir = run_dir.join("shots/app");
            std::fs::create_dir_all(&dir)?;
            let file = dir.join("window.png");
            let ok = Command::new("screencapture")
                .args(["-x", "-o", "-l", &w.id.to_string()])
                .arg(&file)
                .status()
                .await
                .is_ok_and(|s| s.success())
                && std::fs::metadata(&file).is_ok_and(|m| m.len() > 2000);
            checks.push(if ok {
                shot = Some("shots/app/window.png".to_owned());
                pass("A picture of the window", "see below; it should show the oxbot page")
            } else {
                Check {
                    what: "A picture of the window".to_owned(),
                    status: "skipped",
                    detail: "macOS didn't allow the screenshot: give the terminal Screen Recording permission (System Settings → Privacy & Security)".to_owned(),
                }
            });
        }
        (w, None) if w.is_empty() => checks.push(Check {
            what: "Its window opens".to_owned(),
            status: "skipped",
            detail: "couldn't list the app's windows (Swift not available?)".to_owned(),
        }),
        (_, None) => checks.push(fail("Its window opens", "no window")),
    }
    checks.push(if tray {
        pass("Its menu-bar icon is there", "found")
    } else {
        fail("Its menu-bar icon is there", "not found")
    });

    Ok((checks, shot))
}

#[derive(Debug)]
struct Window {
    id: u64,
    layer: i64,
    width: u64,
    height: u64,
    onscreen: bool,
}

async fn list_windows(pid: u32) -> Vec<Window> {
    let Ok(dir) = tempfile::tempdir() else {
        return Vec::new();
    };
    let script = dir.path().join("windows.swift");
    if std::fs::write(&script, WINDOWS_SWIFT).is_err() {
        return Vec::new();
    }
    let Ok(out) = Command::new("swift")
        .arg(&script)
        .arg(pid.to_string())
        .output()
        .await
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Some(Window {
                id: f.first()?.parse().ok()?,
                layer: f.get(1)?.parse().ok()?,
                width: whole(f.get(2)?)?,
                height: whole(f.get(3)?)?,
                onscreen: f.get(4)? == &"1",
            })
        })
        .collect()
}
