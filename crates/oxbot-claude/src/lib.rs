//! Runs Claude Code (`claude -p`) as a long-lived child process for oxbot.
//!
//! - [`process`] starts and talks to the child.
//! - [`event`] turns its output lines into [`ClaudeEvent`]s.
//!
//! Every child gets [`PRIVACY_ENV`] and has [`DENIED_TOOLS`] blocked, so the
//! only place it sends data is Anthropic.

pub mod event;
pub mod process;

pub use event::{
    Activity, ClaudeEvent, LimitWindow, PermissionRequest, ToolUse, TurnUsage, parse_line,
};
pub use process::{ClaudeConfig, ClaudeError, ClaudeProcess, DEFAULT_TOOLS, Decision, Session};

/// Environment variables set on every `claude` child process, so that only
/// Anthropic receives data. Each one is a presence flag: any value disables.
///
/// Source: `docs/research/research-claude-cli.md` (Claude Code data-usage docs).
pub const PRIVACY_ENV: &[(&str, &str)] = &[
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
    ("DISABLE_TELEMETRY", "1"),
    ("DISABLE_ERROR_REPORTING", "1"),
    ("DISABLE_FEEDBACK_COMMAND", "1"),
    ("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1"),
    ("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "1"),
    ("DISABLE_AUTOUPDATER", "1"),
];

/// Tools no oxbot run may use, because they reach sites other than Anthropic
/// outside the oxbot browser. Bots browse through the shared Chrome instead.
pub const DENIED_TOOLS: &[&str] = &["WebFetch", "WebSearch"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_env_turns_off_telemetry_and_error_reports() {
        let keys: Vec<_> = PRIVACY_ENV.iter().map(|(k, _)| *k).collect();
        assert!(keys.contains(&"CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"));
        assert!(keys.contains(&"DISABLE_TELEMETRY"));
        assert!(keys.contains(&"DISABLE_ERROR_REPORTING"));
    }

    #[test]
    fn web_tools_are_denied() {
        assert_eq!(DENIED_TOOLS, ["WebFetch", "WebSearch"]);
    }
}
