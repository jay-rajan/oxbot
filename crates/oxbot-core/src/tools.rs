//! oxbot's own tools for bots.
//!
//! - Every bot: `request_sign_in`.
//! - The central bot (the Assistant) only: `list_bots`, `create_bot`,
//!   `give_task` and `read_reply`, so it can run the team. Other bots aren't
//!   shown them and can't call them, so bots can't set each other off.
//!
//! oxbot hosts the tools inside the server: Claude Code is given an `sdk`
//! tool server (see [`mcp_entry`]) and passes each MCP message to oxbot over
//! the same stdin/stdout link it already uses. No helper process, no network
//! port.

use std::future::Future;

use serde_json::{Value, json};

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
/// Every central-only tool.
pub const CENTRAL_TOOLS: &[&str] = &[
    LIST_BOTS_TOOL,
    CREATE_BOT_TOOL,
    GIVE_TASK_TOOL,
    READ_REPLY_TOOL,
];

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

/// The tools a bot sees, as `tools/list` describes them.
fn tools(central: bool) -> Vec<Value> {
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
    if central {
        list.extend([
            json!({
                "name": LIST_BOTS_TOOL,
                "description": "List the user's bots: each one's id, name, what it's for (the start of its \
                    instructions) and whether it's working right now.",
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
                        "instructions": { "type": "string", "description": "What the bot is for and how it should work, written to the bot (\"You check…\"). A few short lines." }
                    },
                    "required": ["name", "instructions"]
                }
            }),
            json!({
                "name": GIVE_TASK_TOOL,
                "description": "Send another bot a task. It shows in that bot's chat as a message from \
                    Assistant, so the user can follow along. That bot still asks the user before any send, \
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

/// Answers one MCP (JSON-RPC) message for a bot. A notification gets an
/// empty `{}` (Claude Code wants every message answered). `central` says
/// whether the bot is the Assistant. `call(tool, arguments)` runs a tool and
/// returns whether it worked and what to tell the bot.
pub async fn handle<F, Fut>(msg: &Value, central: bool, call: F) -> Value
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
        "tools/list" => json!({ "tools": tools(central) }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let known = name == SIGN_IN_TOOL || (central && CENTRAL_TOOLS.contains(&name));
            if !known {
                return error(&id, -32602, "unknown tool");
            }
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let Some(missing) = missing_argument(name, &arguments) {
                return error(&id, -32602, &format!("{name} needs {missing}"));
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
        let reply = handle(&init, false, no_call).await;
        assert_eq!(reply["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(reply["result"]["serverInfo"]["name"], "oxbot");

        let list = json!({"jsonrpc":"2.0","id":2,"method":"tools/list"});
        assert_eq!(names(&handle(&list, false, no_call).await), [SIGN_IN_TOOL]);
        let central = names(&handle(&list, true, no_call).await);
        assert_eq!(central.len(), 5);
        assert!(central.iter().any(|n| n == CREATE_BOT_TOOL));
        assert_eq!(mcp_entry()["type"], "sdk");
        assert_eq!(full_name(GIVE_TASK_TOOL), "mcp__oxbot__give_task");
    }

    #[tokio::test]
    async fn notifications_get_an_empty_answer() {
        let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"});
        assert_eq!(handle(&note, false, no_call).await, json!({}));
    }

    #[tokio::test]
    async fn a_call_runs_the_tool_and_reports_back() {
        let call = json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":SIGN_IN_TOOL,"arguments":{"url":"https://accounts.google.com","why":"to read mail"}}});
        let reply = handle(&call, false, |tool, args| async move {
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
            handle(&create, false, no_call).await["error"]["code"],
            -32602
        );
        let reply = handle(&create, true, |tool, _| async move {
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
            handle(&unknown, true, no_call).await["error"]["code"],
            -32602
        );
        let no_url = json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":SIGN_IN_TOOL,"arguments":{}}});
        assert_eq!(
            handle(&no_url, false, no_call).await["error"]["code"],
            -32602
        );
        let no_message = json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":GIVE_TASK_TOOL,"arguments":{"bot":"x","message":" "}}});
        assert_eq!(
            handle(&no_message, true, no_call).await["error"]["code"],
            -32602
        );
        let other = json!({"jsonrpc":"2.0","id":6,"method":"resources/list"});
        assert_eq!(
            handle(&other, false, no_call).await["error"]["code"],
            -32601
        );
    }
}
