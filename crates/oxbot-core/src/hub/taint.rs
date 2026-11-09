//! Taint: whether a bot's session has read text oxbot doesn't trust,
//! which may be asking for what the bot does next. The Assistant then needs its
//! person's OK on a card before passing a task to their private bot.
//!
//! Untrusted, so it taints the session:
//! - every tool result except the trusted tools below: web pages from the
//!   browser tool, files from `Read`, `Grep` and `Glob`, what `Write` and
//!   `Edit` echo back, and any tool oxbot doesn't know. A browser call is
//!   marked when the bot makes it as well as when its result comes back, so
//!   a team tool called right after it can't miss it;
//! - another bot's reply, from `give_task` or `read_reply`;
//! - `list_bots` output with bot names or instructions written by someone
//!   other than the person the Assistant acts for.
//!
//! Trusted (`crate::engine::trust`, shared by every engine): oxbot's own tools, whose text oxbot writes
//! (statuses, refusals, sign-in results, whether a report was saved); the team tools mark the session
//! themselves when they hand back someone else's text; and the loop's own
//! to-do list, progress notes and stored results read back.
//!
//! A session remembers what it read for the whole session, across turns and
//! restarts, so the taint does too: it's saved with the session and only a
//! new session clears it.

use oxbot_browser::MCP_SERVER_NAME;

use super::session::tool_label;
use super::{Hub, Runtime};
use crate::engine::trust::is_trusted_tool;
use crate::id::BotId;
#[cfg(test)]
use crate::tools::{
    self, CREATE_BOT_TOOL, DELETE_SCHEDULE_TOOL, GIVE_TASK_TOOL, LIST_BOTS_TOOL,
    LIST_SCHEDULES_TOOL, PAUSE_SCHEDULE_TOOL, READ_REPLY_TOOL, SAVE_REPORT_TOOL,
    SCHEDULE_TASK_TOOL, SIGN_IN_TOOL,
};

/// oxbot's own tools: their results never taint a session (the same list
/// every engine uses, [`crate::engine::trust::TRUSTED_HOST_TOOLS`]).
#[cfg(test)]
const TRUSTED_TOOLS: [&str; 10] = [
    LIST_BOTS_TOOL,
    CREATE_BOT_TOOL,
    GIVE_TASK_TOOL,
    READ_REPLY_TOOL,
    SIGN_IN_TOOL,
    SAVE_REPORT_TOOL,
    SCHEDULE_TASK_TOOL,
    LIST_SCHEDULES_TOOL,
    PAUSE_SCHEDULE_TOOL,
    DELETE_SCHEDULE_TOOL,
];

/// Why a call to tool `name` taints the session (`page`: the page the
/// browser is on, if known), or `None` for a trusted tool. Shown on the
/// card after "The Assistant has read".
pub(super) fn untrusted_tool(name: &str, page: Option<&(String, String)>) -> Option<String> {
    if is_trusted_tool(name) {
        return None;
    }
    // The loop's page tools hand back the stored page's text.
    let page_tool = name
        .strip_prefix(crate::engine::trust::HOST_TOOL_PREFIX)
        .is_some_and(|t| {
            matches!(
                t,
                "find_in_page"
                    | "read_section"
                    | "ask_page"
                    | "extract_rows"
                    | "query_rows"
                    | "read_pages"
                    | "read_document"
                    | "page_steps"
            )
        });
    if page_tool || name.starts_with(&format!("mcp__{MCP_SERVER_NAME}__")) {
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

    /// A new session remembers nothing, so it starts trusted.
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
    use crate::engine::trust::TRUSTED_HOST_TOOLS;

    #[test]
    fn the_shared_list_names_oxbots_own_tools() {
        let mut ours = TRUSTED_TOOLS;
        let mut shared = TRUSTED_HOST_TOOLS;
        ours.sort_unstable();
        shared.sort_unstable();
        assert_eq!(ours, shared);
        assert_eq!(crate::engine::trust::HOST_TOOL_PREFIX, tools::full_name(""));
    }

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
        // The loop's page tools hand back web text; its notes and list don't.
        for t in [
            "find_in_page",
            "read_section",
            "ask_page",
            "extract_rows",
            "query_rows",
        ] {
            assert_eq!(
                untrusted_tool(&tools::full_name(t), Some(&page)).as_deref(),
                Some("a web page (https://jobs.example.com)")
            );
        }
        for t in ["update_todo", "note_progress", "read_result"] {
            assert_eq!(untrusted_tool(&tools::full_name(t), None), None, "{t}");
        }
    }

    #[test]
    fn no_other_tool_is_trusted() {
        for t in [
            "TodoWrite",
            "ToolSearch",
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
