//! Which tools' results a session may trust. Every other result (web
//! pages, files, any tool oxbot doesn't know) taints the session: what it
//! read may be asking for what it does next. The hub keeps the words that
//! say why; every engine shares these lists.

/// The prefix of oxbot's own tools as models see them.
pub const HOST_TOOL_PREFIX: &str = "mcp__oxbot__";

/// oxbot's own tools whose text oxbot writes (statuses, refusals, sign-in
/// results, whether a report was saved). The team tools mark the session
/// themselves when they hand back someone else's text.
pub const TRUSTED_HOST_TOOLS: [&str; 6] = [
    "list_bots",
    "create_bot",
    "give_task",
    "read_reply",
    "request_sign_in",
    "save_report",
];

/// The loop's own tools whose text oxbot writes or the model wrote
/// itself: its to-do list and progress notes, and stored results read
/// back (a stored result keeps the trust it had, inside the same session,
/// which already carries that taint).
pub const TRUSTED_LOOP_TOOLS: [&str; 3] = ["update_todo", "note_progress", "read_result"];

/// Claude Code's own tools that bring in no outside text: its to-do list
/// and task notes (echoing what Claude wrote), plan mode, and loading
/// tool definitions. Not `TaskList`/`TaskGet`/`TaskOutput` (another agent
/// or a background command may have written what they return).
pub const TRUSTED_BUILTINS: [&str; 6] = [
    "TodoWrite",
    "ToolSearch",
    "EnterPlanMode",
    "ExitPlanMode",
    "TaskCreate",
    "TaskUpdate",
];

/// Whether a result of tool `name` (as the model calls it) can be trusted.
#[must_use]
pub fn is_trusted_tool(name: &str) -> bool {
    name.strip_prefix(HOST_TOOL_PREFIX).is_some_and(|tool| {
        TRUSTED_HOST_TOOLS.contains(&tool) || TRUSTED_LOOP_TOOLS.contains(&tool)
    }) || TRUSTED_BUILTINS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_oxbots_own_tools_and_planning_tools_are_trusted() {
        assert!(is_trusted_tool("mcp__oxbot__give_task"));
        assert!(is_trusted_tool("TodoWrite"));
        assert!(is_trusted_tool("mcp__oxbot__update_todo"));
        assert!(is_trusted_tool("mcp__oxbot__read_result"));
        for t in [
            "give_task",
            "mcp__chrome-devtools__take_snapshot",
            "Read",
            "TaskList",
            "todowrite",
            "mcp__x__TodoWrite",
            "mcp__oxbot__unknown",
            "mcp__oxbot__find_in_page",
            "mcp__oxbot__ask_page",
        ] {
            assert!(!is_trusted_tool(t), "{t}");
        }
    }
}
