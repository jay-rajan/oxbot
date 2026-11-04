//! oxbot's own tools for bots.
//!
//! - Every bot: `request_sign_in`.
//! - Bots with the browser: `save_report`, to hand back research as a
//!   report with sources instead of a long reply.
//! - The central bot (Herder) only: `list_bots`, `create_bot`,
//!   `give_task` and `read_reply`, so it can run the team. Other bots aren't
//!   shown them and can't call them, so bots can't set each other off.
//!
//! oxbot runs them inside the hub: every engine offers them to the model
//! under the same names ([`specs`]) and hands each call to the hub. No
//! helper process, no network port.

use serde_json::{Value, json};

use crate::engine::ToolSpec;

/// The tools' prefix: models see them as `mcp__oxbot__*`.
pub const TOOL_SERVER_NAME: &str = "oxbot";
/// The sign-in tool's name.
pub const SIGN_IN_TOOL: &str = "request_sign_in";
/// The central bot's tools.
pub const LIST_BOTS_TOOL: &str = "list_bots";
/// Creates a bot (after the user's OK on a card).
pub const CREATE_BOT_TOOL: &str = "create_bot";
/// Sends a bot a task, and by default waits for its reply.
pub const GIVE_TASK_TOOL: &str = "give_task";
/// Reads a bot's latest reply.
pub const READ_REPLY_TOOL: &str = "read_reply";
/// Saves research as a report (bots with the browser).
pub const SAVE_REPORT_TOOL: &str = "save_report";
/// Every central-only tool.
pub const CENTRAL_TOOLS: &[&str] = &[
    LIST_BOTS_TOOL,
    CREATE_BOT_TOOL,
    GIVE_TASK_TOOL,
    READ_REPLY_TOOL,
];

/// Which of oxbot's tools a bot gets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolSet {
    /// It's Herder, the central bot: the team tools.
    pub central: bool,
    /// It has the browser: `save_report` (with no browser there's no web
    /// research, and nothing it could cite).
    pub browser: bool,
}

impl From<crate::engine::HostTools> for ToolSet {
    fn from(tools: crate::engine::HostTools) -> Self {
        Self {
            central: tools.central,
            browser: tools.browser,
        }
    }
}

impl ToolSet {
    /// Whether `tool` is one of these.
    #[must_use]
    pub fn has(self, tool: &str) -> bool {
        tool == SIGN_IN_TOOL
            || (self.browser && tool == SAVE_REPORT_TOOL)
            || (self.central && CENTRAL_TOOLS.contains(&tool))
    }
}

/// The name models call one of oxbot's tools by, e.g.
/// `mcp__oxbot__create_bot`.
#[must_use]
pub fn full_name(tool: &str) -> String {
    format!("mcp__{TOOL_SERVER_NAME}__{tool}")
}

/// The tools a bot has, as every engine shows them to the model, under
/// their full names (`mcp__oxbot__*`).
#[must_use]
pub fn specs(set: ToolSet) -> Vec<ToolSpec> {
    tools(set)
        .into_iter()
        .map(|tool| ToolSpec {
            name: full_name(tool["name"].as_str().unwrap_or_default()),
            description: tool["description"].as_str().unwrap_or_default().to_owned(),
            parameters: tool["inputSchema"].clone(),
        })
        .collect()
}

/// Checks a call to one of oxbot's tools before it runs: the bot has the
/// tool (its own name, e.g. `give_task`), and every required text argument
/// is there. The error is what to tell the bot.
///
/// # Errors
/// `unknown tool`, or `{tool} needs {argument}`.
pub fn check_call(tool: &str, arguments: &Value, set: ToolSet) -> Result<(), String> {
    if !set.has(tool) {
        return Err("unknown tool".to_owned());
    }
    match missing_argument(tool, arguments) {
        Some(missing) => Err(format!("{tool} needs {missing}")),
        None => Ok(()),
    }
}

/// The tools a bot sees: name, description and input schema.
fn tools(set: ToolSet) -> Vec<Value> {
    let mut list = vec![json!({
        "name": SIGN_IN_TOOL,
        "description": "Ask the user to sign in to a website in the shared browser. oxbot shows them a card; \
            they open the page in a normal Chrome window on their Mac and sign in themselves. Waits (up to \
            about 10 minutes) until they're done. Use it whenever a site shows a sign-in page, a CAPTCHA or a \
            2-step prompt. Every bot then stays signed in. Afterwards, reload the page and carry on.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The sign-in page (or the page that needs sign-in)." },
                "why": { "type": "string", "description": "Why you need it, in a few words, e.g. \"to search your September mail\"." }
            },
            "required": ["url", "why"]
        }
    })];
    if set.browser {
        list.push(save_report_schema());
    }
    if set.central {
        list.extend([
            json!({
                "name": LIST_BOTS_TOOL,
                "description": "List the bots the person asking you can use: each one's id, name, who made \
                    it, what it's for (the start of its instructions) and whether it's working right now.",
                "inputSchema": { "type": "object", "properties": {} }
            }),
            json!({
                "name": CREATE_BOT_TOOL,
                "description": "Create a new bot. oxbot shows the user a card with the name and instructions, \
                    and creates it only if they press Create. Create one only for a job that will repeat or \
                    needs its own memory; do one-off work yourself. Returns the new bot's id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "A short name, e.g. \"Gmail Checker\"." },
                        "instructions": { "type": "string", "description": "What the bot is for and how it should work, written to the bot (\"You check…\"). A few short lines." },
                        "private": { "type": "boolean", "description": "Only for the person asking: hidden from everyone else, and it uses their own browser sign-ins. Only when they ask for a private bot." }
                    },
                    "required": ["name", "instructions"]
                }
            }),
            json!({
                "name": GIVE_TASK_TOOL,
                "description": "Send another bot a task. It shows in that bot's chat as a message from \
                    Herder, so the user can follow along. That bot still asks the user before any send, \
                    post, delete or buy, and asks them to sign in when needed. By default waits (up to about \
                    10 minutes) and returns the bot's reply; with wait=false it returns at once and you can \
                    read_reply later.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "bot": { "type": "string", "description": "The bot's id, from list_bots." },
                        "message": { "type": "string", "description": "The task, written to that bot." },
                        "wait": { "type": "boolean", "description": "Wait for its reply (default true)." }
                    },
                    "required": ["bot", "message"]
                }
            }),
            json!({
                "name": READ_REPLY_TOOL,
                "description": "Read a bot's latest reply, and whether it's still working.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "bot": { "type": "string", "description": "The bot's id, from list_bots." } },
                    "required": ["bot"]
                }
            }),
        ]);
    }
    list
}

/// `save_report`, as `tools/list` describes it.
fn save_report_schema() -> Value {
    json!({
        "name": SAVE_REPORT_TOOL,
        "description": "Save research (comparing options, looking things up across sites) as a report the \
            user opens from a card in this chat, instead of pasting a long answer. Every source must be a \
            page you opened in the browser in this conversation; cite each fact from a page as [1], [2] in \
            the order of your sources. Afterwards reply in 2 to 4 lines with the answer in brief; don't \
            repeat the report.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "title": { "type": "string", "maxLength": crate::report::MAX_TITLE, "description": "A short title, e.g. \"Coffee near home\"." },
                "summary": { "type": "string", "maxLength": crate::report::MAX_SUMMARY, "description": "One line: the answer in brief, shown on the card." },
                "markdown": { "type": "string", "description": "The report in Markdown: the short answer first, then sections; a table when comparing options (e.g. price, rating, distance). Cite facts as [1], [2]. No images or HTML." },
                "sources": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": crate::report::MAX_SOURCES,
                    "description": "The pages you used, numbered from 1 in this order. Only pages you opened in the browser in this conversation.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "url": { "type": "string", "description": "The page's address, as the browser showed it." },
                            "title": { "type": "string", "description": "The page's title." }
                        },
                        "required": ["url"]
                    }
                }
            },
            "required": ["title", "summary", "markdown", "sources"]
        }
    })
}

/// The first required text argument that's missing or empty.
fn missing_argument(tool: &str, arguments: &Value) -> Option<&'static str> {
    let required: &[&'static str] = match tool {
        SIGN_IN_TOOL => &["url"],
        CREATE_BOT_TOOL => &["name", "instructions"],
        GIVE_TASK_TOOL => &["bot", "message"],
        READ_REPLY_TOOL => &["bot"],
        SAVE_REPORT_TOOL => &["title", "summary", "markdown"],
        _ => &[],
    };
    required.iter().copied().find(|k| {
        arguments
            .get(*k)
            .and_then(Value::as_str)
            .is_none_or(|v| v.trim().is_empty())
    })
}

/// A text argument, or `""`.
#[must_use]
pub fn text_arg<'a>(arguments: &'a Value, key: &str) -> &'a str {
    arguments.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: ToolSet = ToolSet {
        central: false,
        browser: false,
    };
    const CENTRAL: ToolSet = ToolSet {
        central: true,
        browser: false,
    };
    const BROWSING: ToolSet = ToolSet {
        central: false,
        browser: true,
    };
    const BOTH: ToolSet = ToolSet {
        central: true,
        browser: true,
    };

    fn names(set: ToolSet) -> Vec<String> {
        specs(set).into_iter().map(|s| s.name).collect()
    }

    #[test]
    fn specs_are_the_tools_under_their_full_names() {
        let listed = specs(BOTH);
        assert_eq!(listed.len(), tools(BOTH).len());
        assert_eq!(listed.len(), 6);
        assert_eq!(listed[0].name, "mcp__oxbot__request_sign_in");
        assert_eq!(listed[0].parameters, tools(BOTH)[0]["inputSchema"]);
        assert!(listed.iter().any(|s| s.name == full_name(GIVE_TASK_TOOL)));
        assert_eq!(names(PLAIN), [full_name(SIGN_IN_TOOL)]);
        let central = names(CENTRAL);
        assert_eq!(central.len(), 5);
        assert!(central.contains(&full_name(CREATE_BOT_TOOL)));
        assert_eq!(full_name(GIVE_TASK_TOOL), "mcp__oxbot__give_task");
    }

    #[test]
    fn calls_are_checked_before_they_run() {
        assert_eq!(
            check_call(GIVE_TASK_TOOL, &json!({"bot":"x","message":"hi"}), PLAIN),
            Err("unknown tool".into())
        );
        assert_eq!(
            check_call(GIVE_TASK_TOOL, &json!({"bot":"x"}), CENTRAL),
            Err("give_task needs message".into())
        );
        assert_eq!(
            check_call(GIVE_TASK_TOOL, &json!({"bot":"x","message":" "}), CENTRAL),
            Err("give_task needs message".into())
        );
        assert_eq!(
            check_call(GIVE_TASK_TOOL, &json!({"bot":"x","message":"hi"}), CENTRAL),
            Ok(())
        );
        assert_eq!(
            check_call("nope", &json!({}), BOTH),
            Err("unknown tool".into())
        );
        assert_eq!(
            check_call(SIGN_IN_TOOL, &json!({}), PLAIN),
            Err("request_sign_in needs url".into())
        );
        assert_eq!(
            check_call(
                CREATE_BOT_TOOL,
                &json!({"name":"Mail","instructions":"Check mail."}),
                PLAIN
            ),
            Err("unknown tool".into()),
            "only the central bot can use its tools"
        );
    }

    #[test]
    fn only_a_bot_with_the_browser_saves_reports() {
        assert_eq!(
            names(BROWSING),
            [full_name(SIGN_IN_TOOL), full_name(SAVE_REPORT_TOOL)]
        );
        let schema = &specs(BROWSING)[1].parameters;
        assert_eq!(
            schema["required"],
            json!(["title", "summary", "markdown", "sources"])
        );
        let full = json!({"title":"Coffee","summary":"A wins.","markdown":"A [1]","sources":[{"url":"https://a.example.com/"}]});
        for set in [PLAIN, CENTRAL] {
            assert_eq!(
                check_call(SAVE_REPORT_TOOL, &full, set),
                Err("unknown tool".into())
            );
        }
        for missing in ["title", "summary", "markdown"] {
            let mut args = full.clone();
            args[missing] = json!(" ");
            assert_eq!(
                check_call(SAVE_REPORT_TOOL, &args, BROWSING),
                Err(format!("save_report needs {missing}"))
            );
        }
        assert_eq!(check_call(SAVE_REPORT_TOOL, &full, BROWSING), Ok(()));
        assert_eq!(text_arg(&full, "title"), "Coffee");
        assert_eq!(text_arg(&full, "nothing"), "");
    }
}
