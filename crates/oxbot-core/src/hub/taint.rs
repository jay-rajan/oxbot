//! Taint: whether a bot's Claude session has read text oxbot doesn't trust,
//! which may be asking for what the bot does next. Herder then needs its
//! person's OK on a card before passing a task to their private bot.
//!
//! Untrusted, so it taints the session:
//! - every tool result except the trusted tools below: web pages from the
//!   browser tool, files from `Read`, `Grep` and `Glob`, what `Write` and
//!   `Edit` echo back, and any tool oxbot doesn't know. A browser call is
//!   marked when Claude makes it as well as when its result comes back, so
//!   a team tool called right after it can't miss it;
//! - another bot's reply, from `give_task` or `read_reply`;
//! - `list_bots` output with bot names or instructions written by someone
//!   other than the person Herder acts for.
//!
//! Trusted ([`TRUSTED_TOOLS`]): oxbot's own tools, whose text oxbot writes
//! (statuses, refusals, sign-in results, whether a report was saved); the team tools mark the session
//! themselves when they hand back someone else's text. Also Claude Code's
//! planning tools ([`TRUSTED_BUILTINS`]), which only echo Claude's own plan
//! or load tool definitions, never outside text.
//!
//! Claude remembers what it read for the whole session, across turns and
//! restarts, so the taint does too: it's saved with the session and only a
//! new session clears it.

use oxbot_browser::MCP_SERVER_NAME;

use super::claude::tool_label;
use super::{Hub, Runtime};
use crate::id::BotId;
use crate::tools::{
    self, CREATE_BOT_TOOL, GIVE_TASK_TOOL, LIST_BOTS_TOOL, READ_REPLY_TOOL, SAVE_REPORT_TOOL,
    SIGN_IN_TOOL,
};

/// oxbot's own tools: their results never taint a session.
const TRUSTED_TOOLS: [&str; 6] = [
    LIST_BOTS_TOOL,
    CREATE_BOT_TOOL,
    GIVE_TASK_TOOL,
    READ_REPLY_TOOL,
    SIGN_IN_TOOL,
    SAVE_REPORT_TOOL,
];

/// Claude Code's own tools that bring in no outside text: its to-do list
/// and task notes (echoing what Claude wrote), plan mode, and loading
/// tool definitions. Not `TaskList`/`TaskGet`/`TaskOutput` (another agent
/// or a background command may have written what they return).
const TRUSTED_BUILTINS: [&str; 6] = [
    "TodoWrite",
    "ToolSearch",
    "EnterPlanMode",
    "ExitPlanMode",
    "TaskCreate",
    "TaskUpdate",
];

/// Why a call to tool `name` taints the session (`page`: the page the
/// browser is on, if known), or `None` for a trusted tool. Shown on the
/// card after "Herder has read".
pub(super) fn untrusted_tool(name: &str, page: Option<&(String, String)>) -> Option<String> {
    if TRUSTED_TOOLS.iter().any(|t| name == tools::full_name(t)) || TRUSTED_BUILTINS.contains(&name)
    {
        return None;
    }
    if name.starts_with(&format!("mcp__{MCP_SERVER_NAME}__")) {
        return Some(match page {
            Some((_, url)) if !url.is_empty() => format!("a web page ({url})"),
            _ => "a web page".to_owned(),
        });
    }
    Some(format!("a file or tool result ({})", tool_label(name)))
}

/// Why a result for a tool call oxbot didn't see taints the session.
pub(super) const UNKNOWN_RESULT: &str = "a tool result";

/// Why `list_bots` taints the session.
pub(super) const OTHER_PEOPLES_BOTS: &str = "other people's bot names and instructions";

impl Runtime {
    /// Records that the session has read `why`, text oxbot doesn't trust.
    /// Returns whether that changed what's recorded.
    pub(super) fn mark_untrusted(&mut self, why: String) -> bool {
        if self.taint.as_deref() == Some(why.as_str()) {
            return false;
        }
        self.taint = Some(why);
        true
    }

    /// A new Claude session remembers nothing, so it starts trusted.
    pub(super) fn reset_for_new_session(&mut self) {
        self.taint = None;
    }
}

impl Hub {
    /// Marks `bot`'s session as having read `why`, and saves that with the
    /// session.
    pub(super) async fn mark_untrusted(&self, bot: &BotId, why: String) {
        let changed = self
            .runtimes
            .lock()
            .await
            .get_mut(bot)
            .is_some_and(|r| r.mark_untrusted(why.clone()));
        if changed && let Err(err) = self.store.set_session_taint(bot, Some(&why)) {
            tracing::warn!(%bot, %err, "couldn't save what the session has read");
        }
    }

    /// What `bot`'s session has read that oxbot doesn't trust, if anything.
    pub(super) async fn untrusted(&self, bot: &BotId) -> Option<String> {
        self.runtimes
            .lock()
            .await
            .get(bot)
            .and_then(|r| r.taint.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_oxbots_own_tools_are_trusted() {
        let page = ("Jobs".to_owned(), "https://jobs.example.com".to_owned());
        for t in TRUSTED_TOOLS {
            assert_eq!(untrusted_tool(&tools::full_name(t), Some(&page)), None);
        }
        assert_eq!(
            untrusted_tool("mcp__chrome-devtools__take_snapshot", Some(&page)).as_deref(),
            Some("a web page (https://jobs.example.com)")
        );
        assert_eq!(
            untrusted_tool("mcp__chrome-devtools__navigate_page", None).as_deref(),
            Some("a web page")
        );
        for t in ["Read", "Grep", "Glob", "Edit", "Bash", "mcp__other__x"] {
            assert!(untrusted_tool(t, None).is_some(), "{t}");
        }
    }

    #[test]
    fn claude_codes_planning_tools_are_trusted_but_not_its_readers() {
        for t in TRUSTED_BUILTINS {
            assert_eq!(untrusted_tool(t, None), None, "{t}");
        }
        for t in [
            "Read",
            "Grep",
            "Glob",
            "WebFetch",
            "WebSearch",
            "Bash",
            "Task",
            "TaskList",
            "TaskGet",
            "TaskOutput",
            "todowrite",
            "mcp__x__TodoWrite",
        ] {
            assert!(untrusted_tool(t, None).is_some(), "{t}");
        }
    }
}
