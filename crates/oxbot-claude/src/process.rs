//! One long-lived `claude -p` child process per bot.
//!
//! oxbot writes each user message to the child's stdin as a stream-json line
//! and reads [`ClaudeEvent`]s from its stdout. The child keeps the
//! conversation in memory between turns; after a restart, oxbot starts a new
//! child with `--resume <session id>` and Claude picks up where it left off.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::event::{ClaudeEvent, parse_line};
use crate::{DENIED_TOOLS, PRIVACY_ENV};

/// Tools a bot may use in milestone 2: files inside its own folder only.
pub const DEFAULT_TOOLS: &[&str] = &["Read", "Write", "Edit", "Glob", "Grep"];

/// Which Claude session the child should use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Session {
    /// Start a new session with this id (a UUID oxbot picked).
    New(String),
    /// Continue an existing session.
    Resume(String),
}

impl Session {
    /// The session id either way.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::New(id) | Self::Resume(id) => id,
        }
    }
}

/// How to start a `claude` child.
#[derive(Debug, Clone)]
pub struct ClaudeConfig {
    /// The `claude` program. Tests point this at a fake.
    pub program: PathBuf,
    /// Working folder: the bot's own folder.
    pub cwd: PathBuf,
    /// New or resumed session.
    pub session: Session,
    /// The bot's instructions, appended to Claude's system prompt.
    pub instructions: Option<String>,
    /// Tools the bot may use.
    pub tools: Vec<String>,
    /// Model alias (e.g. `sonnet`); `None` uses Claude Code's default.
    pub model: Option<String>,
    /// MCP servers as JSON (`--mcp-config`), e.g. the browser tool.
    pub mcp_config: Option<String>,
    /// Tools allowed without asking, e.g. `mcp__oxbot`.
    pub allowed_tools: Vec<String>,
    /// Ask oxbot (as [`ClaudeEvent::Permission`]) before any other tool
    /// runs, instead of refusing it. Off, tools that would need a yes are
    /// refused.
    pub ask_oxbot: bool,
    /// Extra environment variables (tests use this).
    pub extra_env: Vec<(String, String)>,
}

impl ClaudeConfig {
    /// A config with oxbot's defaults: the `claude` on `PATH` and
    /// [`DEFAULT_TOOLS`].
    #[must_use]
    pub fn new(cwd: impl Into<PathBuf>, session: Session) -> Self {
        Self {
            program: PathBuf::from("claude"),
            cwd: cwd.into(),
            session,
            instructions: None,
            tools: DEFAULT_TOOLS.iter().map(|t| (*t).to_owned()).collect(),
            model: None,
            mcp_config: None,
            allowed_tools: Vec::new(),
            ask_oxbot: false,
            extra_env: Vec::new(),
        }
    }

    /// The command-line arguments for this config.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            // Ignore the user's own settings, hooks and MCP servers: a bot gets
            // exactly what oxbot gives it. (`--safe-mode` would also switch off
            // the MCP servers oxbot passes in, so it can't be used.)
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--permission-mode",
            "acceptEdits",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        match &self.session {
            Session::New(id) => args.extend(["--session-id".to_owned(), id.clone()]),
            Session::Resume(id) => args.extend(["--resume".to_owned(), id.clone()]),
        }
        args.push("--tools".to_owned());
        args.push(self.tools.join(","));
        if let Some(mcp) = &self.mcp_config {
            args.extend(["--mcp-config".to_owned(), mcp.clone()]);
        }
        if !self.allowed_tools.is_empty() {
            args.push("--allowedTools".to_owned());
            args.extend(self.allowed_tools.iter().cloned());
        }
        if self.ask_oxbot {
            args.extend(["--permission-prompt-tool".to_owned(), "stdio".to_owned()]);
        }
        args.push("--disallowedTools".to_owned());
        args.extend(DENIED_TOOLS.iter().map(|t| (*t).to_owned()));
        if let Some(model) = &self.model {
            args.extend(["--model".to_owned(), model.clone()]);
        }
        if let Some(text) = &self.instructions {
            args.extend(["--append-system-prompt".to_owned(), text.clone()]);
        }
        args
    }
}

/// oxbot's answer to a [`ClaudeEvent::Permission`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run the tool as asked.
    Allow,
    /// Don't; the bot is told `message`.
    Deny(String),
}

/// Errors from running Claude.
#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    /// `claude` couldn't be started (not installed, or not on `PATH`).
    #[error("couldn't start {program}: {source}")]
    Spawn {
        /// The program oxbot tried to run.
        program: String,
        /// Why it failed.
        source: std::io::Error,
    },
    /// Writing to the child failed; it has most likely exited.
    #[error("claude stopped: {0}")]
    Write(#[from] std::io::Error),
}

/// A running `claude` child.
#[derive(Debug)]
pub struct ClaudeProcess {
    child: Child,
    stdin: ChildStdin,
    next_request: u64,
}

impl ClaudeProcess {
    /// Starts the child. Its events arrive on the returned channel, which
    /// closes when the child exits.
    ///
    /// # Errors
    /// Returns [`ClaudeError::Spawn`] if the program can't be started.
    pub fn spawn(
        config: &ClaudeConfig,
    ) -> Result<(Self, mpsc::Receiver<ClaudeEvent>), ClaudeError> {
        let mut cmd = Command::new(&config.program);
        cmd.args(config.args())
            .current_dir(&config.cwd)
            .envs(PRIVACY_ENV.iter().copied())
            .envs(
                config
                    .extra_env
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str())),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|source| ClaudeError::Spawn {
            program: config.program.display().to_string(),
            source,
        })?;
        tracing::info!(
            pid = child.id(),
            session = config.session.id(),
            "claude started"
        );

        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            unreachable!("all three pipes were requested");
        };

        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match parse_line(&line) {
                    Ok(Some(event)) => {
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(err) => tracing::warn!(%err, "unreadable line from claude"),
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!(target: "claude_stderr", "{line}");
            }
        });

        Ok((
            Self {
                child,
                stdin,
                next_request: 1,
            },
            rx,
        ))
    }

    /// Sends the user's message. The reply arrives as events.
    ///
    /// # Errors
    /// Returns [`ClaudeError::Write`] if the child has exited.
    pub async fn send(&mut self, text: &str) -> Result<(), ClaudeError> {
        let line = json!({"type": "user", "message": {"role": "user", "content": text}});
        self.write_line(&line).await
    }

    /// Asks Claude to stop the current turn. The child keeps running.
    ///
    /// # Errors
    /// Returns [`ClaudeError::Write`] if the child has exited.
    pub async fn interrupt(&mut self) -> Result<(), ClaudeError> {
        let id = format!("oxbot-{}", self.next_request);
        self.next_request += 1;
        let line = json!({"type": "control_request", "request_id": id, "request": {"subtype": "interrupt"}});
        self.write_line(&line).await
    }

    /// Answers a permission request. `input` is the tool's arguments, passed
    /// back unchanged when allowing.
    ///
    /// # Errors
    /// Returns [`ClaudeError::Write`] if the child has exited.
    pub async fn answer(
        &mut self,
        request_id: &str,
        input: &serde_json::Value,
        decision: &Decision,
    ) -> Result<(), ClaudeError> {
        let response = match decision {
            Decision::Allow => json!({"behavior": "allow", "updatedInput": input}),
            Decision::Deny(message) => json!({"behavior": "deny", "message": message}),
        };
        let line = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request_id, "response": response}});
        self.write_line(&line).await
    }

    /// Answers a [`crate::ClaudeEvent::McpMessage`] with `response`, the
    /// JSON-RPC answer (for a notification, an empty `{}`).
    ///
    /// # Errors
    /// Returns [`ClaudeError::Write`] if the child has exited.
    pub async fn answer_mcp(
        &mut self,
        request_id: &str,
        response: serde_json::Value,
    ) -> Result<(), ClaudeError> {
        let line = json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request_id,
            "response": {"mcp_response": response}}});
        self.write_line(&line).await
    }

    /// Closes stdin so the child exits, waiting up to 5 seconds before
    /// killing it.
    pub async fn shutdown(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_err()
        {
            tracing::warn!("claude didn't exit in time; killing it");
            let _ = self.child.kill().await;
        }
    }

    async fn write_line(&mut self, value: &serde_json::Value) -> Result<(), ClaudeError> {
        let mut line = value.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_lock_the_bot_down() {
        let mut config = ClaudeConfig::new("/tmp/bot", Session::New("abc".into()));
        config.instructions = Some("Be brief.".into());
        let args = config.args();
        let joined = args.join(" ");
        assert!(joined.contains("--session-id abc"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--setting-sources" && w[1].is_empty())
        );
        assert!(
            !joined.contains("--safe-mode"),
            "safe mode would switch off the browser tool"
        );
        assert!(joined.contains("--strict-mcp-config"));
        assert!(joined.contains("--tools Read,Write,Edit,Glob,Grep"));
        assert!(joined.contains("--disallowedTools WebFetch WebSearch"));
        assert_eq!(args.last().map(String::as_str), Some("Be brief."));
    }

    #[test]
    fn browser_tool_is_passed_and_allowed() {
        let mut config = ClaudeConfig::new("/tmp", Session::New("x".into()));
        config.mcp_config = Some("{}".into());
        config.allowed_tools = vec!["mcp__chrome-devtools".into()];
        let args = config.args();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--mcp-config" && w[1] == "{}")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--allowedTools" && w[1] == "mcp__chrome-devtools")
        );
    }

    #[test]
    fn asking_oxbot_adds_the_stdio_prompt_tool() {
        let mut config = ClaudeConfig::new("/tmp", Session::New("x".into()));
        assert!(
            !config
                .args()
                .iter()
                .any(|a| a == "--permission-prompt-tool")
        );
        config.ask_oxbot = true;
        assert!(
            config
                .args()
                .windows(2)
                .any(|w| w[0] == "--permission-prompt-tool" && w[1] == "stdio")
        );
    }

    #[test]
    fn resume_uses_the_resume_flag() {
        let args = ClaudeConfig::new("/tmp", Session::Resume("abc".into())).args();
        assert!(args.windows(2).any(|w| w[0] == "--resume" && w[1] == "abc"));
        assert!(!args.iter().any(|a| a == "--session-id"));
    }
}
