//! Which tools' results a session may trust. Every other result (web
//! pages, files, any tool oxbot doesn't know) taints the session: what it
//! read may be asking for what it does next. The hub keeps the words that
//! say why; every engine shares these lists.

/// The prefix of oxbot's own tools as models see them.
pub const HOST_TOOL_PREFIX: &str = "mcp__oxbot__";

/// oxbot's own tools whose text oxbot writes (statuses, refusals, sign-in
/// results, whether a report was saved, schedules the person approved). The team tools mark the session
/// themselves when they hand back someone else's text.
pub const TRUSTED_HOST_TOOLS: [&str; 10] = [
    "list_bots",
    "create_bot",
    "give_task",
    "read_reply",
    "request_sign_in",
    "save_report",
    "schedule_task",
    "list_schedules",
    "pause_schedule",
    "delete_schedule",
];

/// The loop's own tools whose text oxbot writes or the model wrote
/// itself: its to-do list and progress notes, and stored results read
/// back (a stored result keeps the trust it had, inside the same session,
/// which already carries that taint).
/// `run_program`'s output is the person's approved program run on the
/// bot's own folder: it doesn't mark the session (an Always rule for small
/// scripts would otherwise apply only once per conversation).
pub const TRUSTED_LOOP_TOOLS: [&str; 4] =
    ["update_todo", "note_progress", "read_result", "run_program"];

/// Whether a result of tool `name` (as the model calls it) can be trusted.
#[must_use]
pub fn is_trusted_tool(name: &str) -> bool {
    name.strip_prefix(HOST_TOOL_PREFIX).is_some_and(|tool| {
        TRUSTED_HOST_TOOLS.contains(&tool) || TRUSTED_LOOP_TOOLS.contains(&tool)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_oxbots_own_tools_are_trusted() {
        assert!(is_trusted_tool("mcp__oxbot__give_task"));
        assert!(is_trusted_tool("mcp__oxbot__update_todo"));
        assert!(is_trusted_tool("mcp__oxbot__read_result"));
        for t in [
            "give_task",
            "mcp__chrome-devtools__take_snapshot",
            "Read",
            "TaskList",
            "TodoWrite",
            "todowrite",
            "mcp__x__TodoWrite",
            "mcp__oxbot__unknown",
            "mcp__oxbot__find_in_page",
            "mcp__oxbot__ask_page",
            "mcp__oxbot__extract_rows",
            "mcp__oxbot__query_rows",
        ] {
            assert!(!is_trusted_tool(t), "{t}");
        }
    }
}
