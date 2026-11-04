//! What an engine reports while a session runs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::reply::{Decision, Reply, ToolReply};

/// What an engine reports, in order. Mirrors Claude's output event for
/// event, plus what only oxbot's own loop reports.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "a meter row per model request; events are few and short-lived"
)]
pub enum EngineEvent {
    /// A turn started on `model` (Claude: every turn).
    Started {
        /// The model in use.
        model: String,
    },
    /// A live piece of reply text.
    TextDelta(String),
    /// A complete assistant message: its text and the tools it called.
    Assistant {
        /// All its text (may be empty).
        text: String,
        /// Tools it asked to run, in order.
        tool_uses: Vec<ToolUse>,
    },
    /// A tool's result went back to the model. The full text: the hub reads
    /// page lists, element names and taint from it.
    ToolResult {
        /// The call it answers.
        tool_use_id: String,
        /// Its text.
        text: String,
    },
    /// The gate: may this tool run? Answer before it runs.
    Permission(PermissionRequest),
    /// One of oxbot's own tools (sign-in, report, team tools), run by the
    /// hub.
    HostTool(HostToolCall),
    /// Claude's plan windows, as last seen.
    Limits(Vec<LimitWindow>),
    /// One model request (loop engine), for the meter.
    ModelCall(CallRecord),
    /// The turn ended.
    TurnDone {
        /// Whether it ended without error.
        ok: bool,
        /// The final reply, or the error.
        text: String,
        /// How long it took.
        duration_ms: u64,
        /// Tokens and cost of the turn, if the engine counts them per turn.
        usage: Option<TurnUsage>,
    },
    /// What it's doing right now, for a live status line.
    Activity(Activity),
}

/// An engine asking whether a tool may run. Nothing runs until `reply` is
/// answered; dropping it is a no.
#[derive(Debug)]
pub struct PermissionRequest {
    /// The tool, e.g. `mcp__chrome-devtools__click` (the same names on
    /// every engine).
    pub tool: String,
    /// Its arguments.
    pub input: Value,
    /// The tool call it's for.
    pub tool_use_id: String,
    /// Where the answer goes.
    pub reply: Reply<Decision>,
}

/// A call to one of oxbot's own tools, run by the hub. Dropping `reply`
/// unanswered tells the bot it didn't run.
#[derive(Debug)]
pub struct HostToolCall {
    /// The tool's own name, without the `mcp__oxbot__` prefix, e.g.
    /// `give_task`.
    pub name: String,
    /// Its arguments.
    pub args: Value,
    /// Where the result goes.
    pub reply: Reply<ToolReply>,
}

/// Tokens and cost of one turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnUsage {
    /// New input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write_tokens: u64,
    /// Cost estimate in US dollars (Claude: what the API would charge; a
    /// subscription counts usage against its limits instead).
    pub cost_usd: f64,
}

/// One usage limit, e.g. Claude's 5-hour or 7-day window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitWindow {
    /// Its name, e.g. `five_hour` or `seven_day`.
    pub name: String,
    /// How much of it is used, from 0 to 1.
    pub used: f64,
    /// When it resets, in seconds since the Unix epoch (0 if unknown).
    pub resets_at: i64,
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
}

/// What an engine is doing mid-turn, like the spinner in a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// Waiting on the model.
    Requesting,
    /// Extended thinking.
    Thinking,
    /// Writing the reply.
    Writing,
    /// Calling a tool.
    UsingTool(String),
    /// The model's API asked it to wait and retry (e.g. rate limits).
    Retrying,
    /// Any other status, e.g. `compacting`.
    Other(String),
}

/// A tool the model called.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    /// The call's id, which its result and any permission request carry.
    pub id: String,
    /// Tool name, e.g. `Write`.
    pub name: String,
    /// The tool's arguments, as the model sent them.
    pub input: Value,
}

impl ToolUse {
    /// One short line about what the tool did, for showing in a chat.
    #[must_use]
    pub fn summary(&self) -> String {
        let field = |k: &str| self.input.get(k).and_then(Value::as_str);
        let text = field("file_path")
            .map(file_name)
            .or_else(|| field("pattern"))
            .or_else(|| field("path").map(file_name))
            .or_else(|| field("command"))
            .or_else(|| field("url"))
            .unwrap_or("");
        shorten(text, 80)
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

/// One model request by oxbot's own loop, for the meter (a `model_calls`
/// row). No prompt text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CallRecord {
    /// When it was sent, in milliseconds since the Unix epoch.
    pub ts_ms: i64,
    /// Its UTC day, `YYYY-MM-DD` (free daily counts reset at 00:00 UTC).
    pub day: String,
    /// The bot's id.
    pub bot: String,
    /// The person the turn's task chain started with, by id.
    pub person: Option<String>,
    /// The loop session.
    pub session: Option<String>,
    /// The turn in that session.
    pub turn: Option<u32>,
    /// The step in that turn.
    pub step: Option<u32>,
    /// Who served it: `openrouter` (a local provider later).
    pub provider: String,
    /// The model asked for.
    pub requested: String,
    /// The model that answered (empty if none did).
    pub model: String,
    /// Whether it was a free model.
    pub free: bool,
    /// Whether it asked providers not to keep the data.
    pub private: bool,
    /// The provider's id for the request.
    pub generation_id: Option<String>,
    /// Input tokens.
    pub prompt_tokens: u64,
    /// Output tokens.
    pub completion_tokens: u64,
    /// Input tokens read from the provider's cache.
    pub cached_tokens: u64,
    /// What it cost, in US dollars (0 for free models).
    pub cost_usd: f64,
    /// How long it took.
    pub latency_ms: u64,
    /// `ok`, `rate_limited`, `busy`, `daily_limit`, `error:<kind>` or
    /// `cancelled`.
    pub outcome: String,
    /// What the request was for.
    #[serde(default)]
    pub kind: CallKind,
    /// The engine that made it: `openrouter` (oxbot's loop), `codex`,
    /// `claude` (empty: from before engines were recorded).
    #[serde(default)]
    pub engine: String,
}

/// What a model request was for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
    /// A step of a bot's turn.
    #[default]
    Chat,
    /// Shortening a long conversation.
    Compaction,
    /// Learning from a finished task.
    Reflection,
    /// Judging a practice run.
    Judge,
    /// A question about one stored web page (`ask_page`).
    PageQuestion,
}

impl CallKind {
    /// Its name in the database, e.g. `chat`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Compaction => "compaction",
            Self::Reflection => "reflection",
            Self::Judge => "judge",
            Self::PageQuestion => "page_question",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summaries_name_the_file_or_page() {
        let tool = |input: Value| ToolUse {
            id: String::new(),
            name: "Write".into(),
            input,
        };
        assert_eq!(
            tool(json!({"file_path": "/bots/x/work/notes.md"})).summary(),
            "notes.md"
        );
        assert_eq!(
            tool(json!({"url": "https://example.com/"})).summary(),
            "https://example.com/"
        );
        assert_eq!(tool(json!({})).summary(), "");
        assert_eq!(
            tool(json!({ "command": "x".repeat(100) }))
                .summary()
                .chars()
                .count(),
            81
        );
    }
}
