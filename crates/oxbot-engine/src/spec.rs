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
    /// Where the engine keeps what it saves for itself about the session
    /// (big tool results, context state): `bots/<id>/context`, outside
    /// `work/`. A practice run gets its own.
    pub context_dir: PathBuf,
    /// The bot's knowledge folder for engines that read it from files,
    /// once there is one.
    pub knowledge_dir: Option<PathBuf>,
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
    /// The router picks.
    #[default]
    Auto,
    /// A model id the owner pinned: tried first, with the router's
    /// fallbacks after it.
    Pin(String),
    /// This model and no other: no fallbacks (practice runs that compare
    /// models).
    Only(String),
}

impl ModelChoice {
    /// The model named, if any.
    #[must_use]
    pub fn named(&self) -> Option<&str> {
        match self {
            Self::Auto => None,
            Self::Pin(name) | Self::Only(name) => Some(name),
        }
    }
}

/// How to run the browser tool (`chrome-devtools-mcp`) for one bot.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserSpec {
    /// The command the engine's tool service runs.
    pub mcp: McpCommand,
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
    /// The person the task chain acts for, by id (for the meter and what
    /// the bot knows about them), if known.
    pub person: Option<String>,
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

    #[test]
    fn resume_and_model_choice() {
        assert_eq!(Resume::New("a".into()).id(), "a");
        assert!(Resume::Resume("b".into()).is_resume());
        assert_eq!(
            ModelChoice::Pin("e2e/y:free".into()).named(),
            Some("e2e/y:free")
        );
        assert_eq!(
            ModelChoice::Only("e2e/x:free".into()).named(),
            Some("e2e/x:free")
        );
        assert_eq!(ModelChoice::Auto.named(), None);
    }
}
