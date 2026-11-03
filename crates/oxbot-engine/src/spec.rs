//! What a session is started with, and what a turn is sent with.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Everything an engine needs to start one bot's session. The hub builds
/// it the same way for every engine.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// The bot's id.
    pub bot: String,
    /// The bot's own `work/` folder: the only folder its file tools reach.
    pub work_dir: PathBuf,
    /// The bot's system prompt.
    pub system_prompt: String,
    /// A new session, or the saved one to go on with.
    pub resume: Resume,
    /// Which model.
    pub model: ModelChoice,
    /// The browser tool for the bot's owner's Chrome, if it started.
    pub browser: Option<BrowserSpec>,
    /// Which of oxbot's own tools the bot gets.
    pub host_tools: HostTools,
    /// Those tools as the model sees them (the same JSON on every engine).
    pub host_tool_specs: Vec<ToolSpec>,
    /// Whether the bot is private (the loop then asks providers not to
    /// keep data).
    pub privacy: Privacy,
    /// Live, or a practice run.
    pub gate_mode: GateMode,
    /// Environment for the engine's child processes (e.g. a `PATH` that
    /// finds Node.js).
    pub env: Vec<(String, String)>,
    /// The longest an oxbot tool call may wait (a sign-in, a task for
    /// another bot, a card).
    pub tool_wait: Duration,
}

/// Which session to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// Start a new session with this id (oxbot picked it).
    New(String),
    /// Go on with this saved session.
    Resume(String),
}

impl Resume {
    /// The session id either way.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::New(id) | Self::Resume(id) => id,
        }
    }

    /// Whether this goes on with a saved session.
    #[must_use]
    pub fn is_resume(&self) -> bool {
        matches!(self, Self::Resume(_))
    }
}

/// Which model a session uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelChoice {
    /// The engine picks (Claude Code's default; the loop's router).
    #[default]
    Auto,
    /// This one: a Claude alias such as `sonnet`, or a model id the owner
    /// pinned for the loop.
    Named(String),
}

impl ModelChoice {
    /// The named model, if any.
    #[must_use]
    pub fn named(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::Named(name) => Some(name),
        }
    }
}

/// How to run the browser tool (`chrome-devtools-mcp`) for one bot.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserSpec {
    /// The command, for engines that run it themselves.
    pub mcp: McpCommand,
    /// The same as an `mcpServers` entry, for Claude's `--mcp-config`.
    pub mcp_json: Value,
}

/// A program to run as an MCP server over stdio.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpCommand {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// Its whole environment besides `PATH`.
    pub env: Vec<(String, String)>,
}

impl McpCommand {
    /// The command an `mcpServers` entry describes (`command`, `args`,
    /// `env`). `None` without a `command`.
    #[must_use]
    pub fn from_server_entry(entry: &Value) -> Option<Self> {
        let program = entry.get("command").and_then(Value::as_str)?;
        let args = entry
            .get("args")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(str::to_owned))
            .collect();
        let env = entry
            .get("env")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
        Some(Self {
            program: PathBuf::from(program),
            args,
            env,
        })
    }
}

/// Which of oxbot's own tools a bot gets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostTools {
    /// It's a Herder: the team tools.
    pub central: bool,
    /// It has a browser: `save_report`.
    pub browser: bool,
}

/// One tool as a model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Its name.
    pub name: String,
    /// What it does, for the model.
    pub description: String,
    /// Its arguments, as JSON Schema.
    pub parameters: Value,
}

/// Whether a bot is private to one person.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Privacy {
    /// Shared.
    #[default]
    Standard,
    /// Private: model requests ask providers not to keep data.
    Private,
}

/// Whether tools really run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GateMode {
    /// They do.
    #[default]
    Live,
    /// A practice run: changes are only recorded.
    DryRun,
}

/// One turn for a session.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnInput {
    /// The message, already marked by the hub (who sent it, files).
    pub text: String,
    /// The number of the chat event that holds it.
    pub user_seq: u64,
    /// Who started the task chain, as the hub writes it. Engines keep it
    /// and hand it back on resume, never read it.
    pub origin: Value,
}

/// A turn that was running when oxbot stopped, which a resumed session
/// goes on with.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumedTurn {
    /// The number of the chat event that started it.
    pub user_seq: u64,
    /// Who started its task chain, as the hub wrote it.
    pub origin: Value,
    /// What it was asked, as the chat shows it.
    pub question: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_server_entry_gives_its_command() {
        let entry = json!({"command": "/tools/node", "args": ["mcp.js", "--x=1"],
            "env": {"A": "1", "B": 2}});
        assert_eq!(
            McpCommand::from_server_entry(&entry),
            Some(McpCommand {
                program: PathBuf::from("/tools/node"),
                args: vec!["mcp.js".into(), "--x=1".into()],
                env: vec![("A".into(), "1".into())],
            })
        );
        assert_eq!(McpCommand::from_server_entry(&json!({"type": "sdk"})), None);
    }

    #[test]
    fn resume_and_model_choice() {
        assert_eq!(Resume::New("a".into()).id(), "a");
        assert!(Resume::Resume("b".into()).is_resume());
        assert_eq!(ModelChoice::Named("sonnet".into()).named(), Some("sonnet"));
        assert_eq!(ModelChoice::Auto.named(), None);
    }
}
