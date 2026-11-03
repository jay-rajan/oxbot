//! oxbot's own tools for bots.
//!
//! - Every bot: `request_sign_in`.
//! - Bots with the browser: `save_report`, to hand back research as a
//!   report with sources instead of a long reply.
//! - The central bot (Herder) only: `list_bots`, `create_bot`,
//!   `give_task` and `read_reply`, so it can run the team. Other bots aren't
//!   shown them and can't call them, so bots can't set each other off.
//!
//! oxbot hosts the tools inside the server: Claude Code is given an `sdk`
//! tool server (see [`mcp_entry`]) and passes each MCP message to oxbot over
//! the same stdin/stdout link it already uses. No helper process, no network
//! port.

use std::future::Future;

use serde_json::{Value, json};

use crate::engine::ToolSpec;

/// The tool server's name; Claude sees its tools as `mcp__oxbot__*`.
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

/// Newest MCP protocol version this server knows; older clients get theirs back.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The `mcpServers` entry that tells Claude Code oxbot hosts this server.
#[must_use]
pub fn mcp_entry() -> Value {
    json!({ "type": "sdk", "name": TOOL_SERVER_NAME })
}

/// Claude's name for one of oxbot's tools, e.g. `mcp__oxbot__create_bot`.
#[must_use]
pub fn full_name(tool: &str) -> String {
    format!("mcp__{TOOL_SERVER_NAME}__{tool}")
}

/// The tools a bot has, as every engine shows them to the model: the same
/// JSON as `tools/list`, with Claude's full names (`mcp__oxbot__*`).
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

/// The tools a bot sees, as `tools/list` describes them.
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

/// Answers one MCP (JSON-RPC) message for a bot. A notification gets an
/// empty `{}` (Claude Code wants every message answered). `set` says which
/// of oxbot's tools the bot has. `call(tool, arguments)` runs a tool and
/// returns whether it worked and what to tell the bot.
pub async fn handle<F, Fut>(msg: &Value, set: ToolSet, call: F) -> Value
where
    F: FnOnce(String, Value) -> Fut,
    Fut: Future<Output = (bool, String)>,
{
    let Some(id) = msg.get("id").cloned() else {
        return json!({});
    };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": TOOL_SERVER_NAME, "version": env!("CARGO_PKG_VERSION") }
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools(set) }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let Err(message) = check_call(name, &arguments, set) {
                return error(&id, -32602, &message);
            }
            let (ok, message) = call(name.to_owned(), arguments).await;
            json!({ "content": [{ "type": "text", "text": message }], "isError": !ok })
        }
        _ => return error(&id, -32601, "method not found"),
    };
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
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

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
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

    async fn no_call(_: String, _: Value) -> (bool, String) {
        panic!("should not call")
    }

    fn names(reply: &Value) -> Vec<String> {
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn it_introduces_itself_and_lists_its_tools() {
        let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}});
        let reply = handle(&init, PLAIN, no_call).await;
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(reply["result"]["serverInfo"]["name"], "oxbot");

        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        assert_eq!(names(&handle(&list, PLAIN, no_call).await), [SIGN_IN_TOOL]);
        let central = names(&handle(&list, CENTRAL, no_call).await);
        assert_eq!(central.len(), 5);
        assert!(central.iter().any(|n| n == CREATE_BOT_TOOL));
        assert_eq!(mcp_entry()["type"], "sdk");
        assert_eq!(full_name(GIVE_TASK_TOOL), "mcp__oxbot__give_task");
    }

    #[test]
    fn specs_are_the_listed_tools_under_their_full_names() {
        let both = ToolSet {
            central: true,
            browser: true,
        };
        let listed = specs(both);
        assert_eq!(listed.len(), tools(both).len());
        assert_eq!(listed[0].name, "mcp__oxbot__request_sign_in");
        assert_eq!(listed[0].parameters, tools(both)[0]["inputSchema"]);
        assert!(listed.iter().any(|s| s.name == full_name(GIVE_TASK_TOOL)));
        assert_eq!(specs(PLAIN).len(), 1);
    }

    #[test]
    fn calls_are_checked_as_tools_call_checks_them() {
        assert_eq!(
            check_call(GIVE_TASK_TOOL, &json!({"bot":"x","message":"hi"}), PLAIN),
            Err("unknown tool".into())
        );
        assert_eq!(
            check_call(GIVE_TASK_TOOL, &json!({"bot":"x"}), CENTRAL),
            Err("give_task needs message".into())
        );
        assert_eq!(
            check_call(GIVE_TASK_TOOL, &json!({"bot":"x","message":"hi"}), CENTRAL),
            Ok(())
        );
    }

    #[tokio::test]
    async fn notifications_get_an_empty_answer() {
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert_eq!(handle(&note, PLAIN, no_call).await, json!({}));
    }

    #[tokio::test]
    async fn a_call_runs_the_tool_and_reports_back() {
        let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":SIGN_IN_TOOL,"arguments":{"url":"https://accounts.google.com","why":"to read mail"}}});
        let reply = handle(&call, PLAIN, |tool, args| async move {
            assert_eq!(tool, SIGN_IN_TOOL);
            assert_eq!(text_arg(&args, "url"), "https://accounts.google.com");
            (true, "signed in".to_owned())
        })
        .await;
        assert_eq!(reply["result"]["content"][0]["text"], "signed in");
        assert_eq!(reply["result"]["isError"], false);
    }

    #[tokio::test]
    async fn only_the_central_bot_can_use_its_tools() {
        let create = json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"name":CREATE_BOT_TOOL,"arguments":{"name":"Mail","instructions":"Check mail."}}});
        assert_eq!(
            handle(&create, PLAIN, no_call).await["error"]["code"],
            -32602
        );
        let reply = handle(&create, CENTRAL, |tool, _| async move {
            assert_eq!(tool, CREATE_BOT_TOOL);
            (true, "created mail".to_owned())
        })
        .await;
        assert_eq!(reply["result"]["isError"], false);
    }

    #[tokio::test]
    async fn bad_calls_are_errors() {
        let unknown =
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"nope"}});
        assert_eq!(
            handle(&unknown, CENTRAL, no_call).await["error"]["code"],
            -32602
        );
        let no_url = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":SIGN_IN_TOOL,"arguments":{}}});
        assert_eq!(
            handle(&no_url, PLAIN, no_call).await["error"]["code"],
            -32602
        );
        let no_message = json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":GIVE_TASK_TOOL,"arguments":{"bot":"x","message":" "}}});
        assert_eq!(
            handle(&no_message, CENTRAL, no_call).await["error"]["code"],
            -32602
        );
        let other = json!({"jsonrpc":"2.0","id":6,"method":"resources/list"});
        assert_eq!(
            handle(&other, PLAIN, no_call).await["error"]["code"],
            -32601
        );
    }

    #[tokio::test]
    async fn only_a_bot_with_the_browser_saves_reports() {
        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        let browsing = handle(&list, BROWSING, no_call).await;
        assert_eq!(names(&browsing), [SIGN_IN_TOOL, SAVE_REPORT_TOOL]);
        let schema = &browsing["result"]["tools"][1]["inputSchema"];
        assert_eq!(
            schema["required"],
            json!(["title", "summary", "markdown", "sources"])
        );
        let both = ToolSet {
            central: true,
            browser: true,
        };
        assert_eq!(names(&handle(&list, both, no_call).await).len(), 6);

        let save = |args: Value| {
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
            "params":{"name":SAVE_REPORT_TOOL,"arguments":args}})
        };
        let full = json!({"title":"Coffee","summary":"A wins.","markdown":"A [1]","sources":[{"url":"https://a.example.com/"}]});
        for set in [PLAIN, CENTRAL] {
            assert_eq!(
                handle(&save(full.clone()), set, no_call).await["error"]["code"],
                -32602
            );
        }
        for missing in ["title", "summary", "markdown"] {
            let mut args = full.clone();
            args[missing] = json!(" ");
            let reply = handle(&save(args), BROWSING, no_call).await;
            assert_eq!(
                reply["error"]["message"],
                format!("save_report needs {missing}")
            );
        }
        let reply = handle(&save(full), BROWSING, |tool, _| async move {
            assert_eq!(tool, SAVE_REPORT_TOOL);
            (true, "Saved".to_owned())
        })
        .await;
        assert_eq!(reply["result"]["isError"], false);
    }
}
