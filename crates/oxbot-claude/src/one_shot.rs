//! One question to Claude outside any bot: no tools, no MCP servers, no
//! user settings, no saved session, in an empty folder. For judging a
//! practice run (and, later, reflecting on a finished task).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

use crate::{DENIED_TOOLS, PRIVACY_ENV};

/// Why a one-shot question got no answer, in words for the person.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct OneShotError(pub String);

/// One question.
#[derive(Debug, Clone)]
pub struct OneShot {
    /// The `claude` program.
    pub program: PathBuf,
    /// Model alias, e.g. `sonnet`.
    pub model: String,
    /// The system prompt (replaces Claude Code's own).
    pub system: String,
    /// The message.
    pub input: String,
    /// Extra environment (tests; a `PATH` that finds Node.js).
    pub env: Vec<(String, String)>,
    /// The longest it may take.
    pub timeout: Duration,
}

/// The arguments for `shot` (the message goes on stdin).
#[must_use]
pub fn args(shot: &OneShot) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--tools",
        "",
        "--strict-mcp-config",
        "--setting-sources",
        "",
        "--no-session-persistence",
        "--model",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    args.push(shot.model.clone());
    args.push("--system-prompt".to_owned());
    args.push(shot.system.clone());
    args.push("--disallowedTools".to_owned());
    args.extend(DENIED_TOOLS.iter().map(|t| (*t).to_owned()));
    args
}

/// Asks Claude `shot` and returns its answer's text.
///
/// # Errors
/// [`OneShotError`] if `claude` can't start, fails, times out, or its
/// output can't be read.
pub async fn one_shot(shot: &OneShot) -> Result<String, OneShotError> {
    let dir = empty_dir()?;
    let result = run(shot, &dir).await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

async fn run(shot: &OneShot, dir: &Path) -> Result<String, OneShotError> {
    let mut child = Command::new(&shot.program)
        .args(args(shot))
        .current_dir(dir)
        .envs(PRIVACY_ENV.iter().copied())
        .envs(shot.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| OneShotError(format!("couldn't start {}: {e}", shot.program.display())))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(shot.input.as_bytes())
            .await
            .map_err(|e| OneShotError(format!("Claude stopped: {e}")))?;
    }
    let output = tokio::time::timeout(shot.timeout, child.wait_with_output())
        .await
        .map_err(|_| OneShotError("Claude didn't answer in time".to_owned()))?
        .map_err(|e| OneShotError(format!("Claude stopped: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let reply: Value = serde_json::from_str(stdout.trim()).map_err(|_| {
        let stderr = String::from_utf8_lossy(&output.stderr);
        OneShotError(format!(
            "Claude's answer couldn't be read{}",
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr.trim().chars().take(200).collect::<String>())
            }
        ))
    })?;
    if reply.get("is_error").and_then(Value::as_bool) == Some(true) || !output.status.success() {
        return Err(OneShotError(format!(
            "Claude couldn't answer: {}",
            reply
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        )));
    }
    reply
        .get("result")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| OneShotError("Claude gave no answer".to_owned()))
}

/// A new, empty, private folder for the child to run in.
fn empty_dir() -> Result<PathBuf, OneShotError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("oxbot-one-shot-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir).map_err(|e| OneShotError(format!("Claude stopped: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| OneShotError(format!("Claude stopped: {e}")))?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(program: PathBuf) -> OneShot {
        OneShot {
            program,
            model: "sonnet".into(),
            system: "Grade it.".into(),
            input: "the run".into(),
            env: Vec::new(),
            timeout: Duration::from_secs(10),
        }
    }

    #[test]
    fn no_tools_no_servers_no_settings_no_session() {
        let a = args(&shot("claude".into()));
        let pair = |flag: &str| a.iter().position(|x| x == flag).map(|i| a[i + 1].as_str());
        assert_eq!(pair("--tools"), Some(""));
        assert_eq!(pair("--setting-sources"), Some(""));
        assert_eq!(pair("--model"), Some("sonnet"));
        assert_eq!(pair("--system-prompt"), Some("Grade it."));
        assert_eq!(pair("--output-format"), Some("json"));
        assert!(a.contains(&"--strict-mcp-config".to_owned()));
        assert!(a.contains(&"--no-session-persistence".to_owned()));
        assert!(a.contains(&"WebFetch".to_owned()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_answer_is_read_from_claudes_json() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("claude");
        // Echoes the message back as the result, and says where it ran.
        std::fs::write(
            &fake,
            "#!/bin/sh\nread -r line\nprintf '{\"type\":\"result\",\"is_error\":false,\"result\":\"%s in %s %s\"}' \"$line\" \"$(ls -A | wc -l | tr -d ' ')\" \"$DISABLE_TELEMETRY\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let answer = one_shot(&shot(fake.clone())).await.unwrap();
        assert_eq!(answer, "the run in 0 1", "an empty folder, telemetry off");

        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '{\"is_error\":true,\"result\":\"limit reached\"}'\n",
        )
        .unwrap();
        let err = one_shot(&shot(fake.clone())).await.unwrap_err();
        assert!(err.to_string().contains("limit reached"), "{err}");
        std::fs::write(&fake, "#!/bin/sh\necho nope\n").unwrap();
        assert!(one_shot(&shot(fake)).await.is_err());
    }
}
