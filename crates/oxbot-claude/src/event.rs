//! Turns one line of `claude -p --output-format stream-json` into a
//! [`ClaudeEvent`].
//!
//! The CLI's format is not a stable public spec and gains new event types
//! often, so anything unknown is skipped rather than treated as an error.

use serde_json::Value;

/// The parts of Claude's output oxbot cares about.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeEvent {
    /// A turn started. Sent at the start of every turn.
    Init {
        /// Claude's session id; pass it to `--resume` to continue later.
        session_id: String,
        /// Model in use.
        model: String,
    },
    /// A live piece of reply text (needs `--include-partial-messages`).
    TextDelta(String),
    /// A complete assistant message: its text and any tools it called.
    Assistant {
        /// All text blocks joined together (may be empty).
        text: String,
        /// Tools Claude asked to run, in order.
        tool_uses: Vec<ToolUse>,
    },
    /// The turn ended.
    TurnResult {
        /// Whether it ended without error.
        ok: bool,
        /// Final reply, or the error.
        text: String,
        /// How long the turn took.
        duration_ms: u64,
        /// Tokens and cost of the turn.
        usage: TurnUsage,
    },
    /// The user's plan limits, as Claude last saw them (sent now and then,
    /// e.g. after each request).
    Limits(Vec<LimitWindow>),
    /// What Claude is doing right now, for a live status line.
    Activity(Activity),
    /// Claude wants to run a tool that isn't pre-allowed and asks oxbot
    /// first (needs `--permission-prompt-tool stdio`). Answer it with
    /// [`crate::ClaudeProcess::answer`]; until then the tool waits.
    Permission(PermissionRequest),
    /// A tool's result went back to the model. Also means Claude is waiting
    /// on the model again.
    ToolResult {
        /// The call it answers.
        tool_use_id: String,
        /// Its text (text blocks joined; images skipped).
        text: String,
    },
    /// A message for a tool server oxbot hosts itself (an `sdk` entry in
    /// `--mcp-config`): JSON-RPC from Claude. Answer requests with
    /// [`crate::ClaudeProcess::answer_mcp`]; notifications too, with an
    /// empty result.
    McpMessage {
        /// Id to answer with.
        request_id: String,
        /// The tool server's name, e.g. `oxbot`.
        server: String,
        /// The JSON-RPC message.
        message: Value,
    },
    /// The CLI confirmed a control request (e.g. an interrupt).
    ControlAck {
        /// The id oxbot sent with the request.
        request_id: String,
    },
}

/// Tokens and cost of one turn, as Claude reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TurnUsage {
    /// New input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write_tokens: u64,
    /// Claude's own cost estimate in US dollars (what the API would charge;
    /// a subscription counts usage against its limits instead).
    pub cost_usd: f64,
}

/// One of the user's plan limits, e.g. the 5-hour or 7-day window.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitWindow {
    /// Claude's name for it, e.g. `five_hour` or `seven_day`.
    pub name: String,
    /// How much of it is used, from 0 to 1.
    pub used: f64,
    /// When it resets, in seconds since the Unix epoch (0 if unknown).
    pub resets_at: i64,
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
}

/// What Claude is doing mid-turn, like the spinner in the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    /// Waiting on the model ("Clauding…" in the terminal).
    Requesting,
    /// Extended thinking.
    Thinking,
    /// Writing the reply.
    Writing,
    /// Calling a tool.
    UsingTool(String),
    /// The API asked it to wait and retry (e.g. rate limits).
    Retrying,
    /// Any other status the CLI reports, e.g. `compacting`.
    Other(String),
}

/// Claude asking whether it may run a tool.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// Id to answer with.
    pub request_id: String,
    /// The tool, e.g. `mcp__chrome-devtools__click`.
    pub tool: String,
    /// Its arguments.
    pub input: Value,
    /// The tool call it's for.
    pub tool_use_id: String,
}

/// A tool Claude called.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    /// The call's id, which its result and any permission request carry.
    pub id: String,
    /// Tool name, e.g. `Write`.
    pub name: String,
    /// The tool's arguments, as Claude sent them.
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

/// Parses one output line.
///
/// Returns `Ok(None)` for lines oxbot doesn't need: unknown event types,
/// rate-limit notices, user echoes, and anything from subagents.
///
/// # Errors
/// Returns an error if the line isn't valid JSON.
pub fn parse_line(line: &str) -> Result<Option<ClaudeEvent>, serde_json::Error> {
    let v: Value = serde_json::from_str(line)?;
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();

    // Subagent traffic carries the id of the tool call that started it.
    if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
        return Ok(None);
    }

    let event = match v.get("type").and_then(Value::as_str) {
        Some("system") => match v.get("subtype").and_then(Value::as_str) {
            Some("init") => Some(ClaudeEvent::Init {
                session_id: s(&v, "session_id"),
                model: s(&v, "model"),
            }),
            Some("status") => Some(ClaudeEvent::Activity(
                match v.get("status").and_then(Value::as_str) {
                    Some("requesting") | None => Activity::Requesting,
                    Some(other) => Activity::Other(other.to_owned()),
                },
            )),
            Some("api_retry") => Some(ClaudeEvent::Activity(Activity::Retrying)),
            _ => None,
        },
        Some("stream_event") => {
            let event = v.get("event").unwrap_or(&Value::Null);
            match event.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => match event.get("delta") {
                    Some(d) if d.get("type").and_then(Value::as_str) == Some("text_delta") => {
                        Some(ClaudeEvent::TextDelta(s(d, "text")))
                    }
                    _ => None,
                },
                Some("content_block_start") => {
                    let block = event.get("content_block").unwrap_or(&Value::Null);
                    match block.get("type").and_then(Value::as_str) {
                        Some("thinking" | "redacted_thinking") => {
                            Some(ClaudeEvent::Activity(Activity::Thinking))
                        }
                        Some("text") => Some(ClaudeEvent::Activity(Activity::Writing)),
                        Some("tool_use") => {
                            Some(ClaudeEvent::Activity(Activity::UsingTool(s(block, "name"))))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        // A tool finished and its result went back to the model.
        Some("user") => Some(tool_result(&v)),
        Some("assistant") => {
            let blocks = v
                .pointer("/message/content")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice);
            let mut text = String::new();
            let mut tool_uses = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        text.push_str(b.get("text").and_then(Value::as_str).unwrap_or(""));
                    }
                    Some("tool_use") => tool_uses.push(ToolUse {
                        id: s(b, "id"),
                        name: s(b, "name"),
                        input: b.get("input").cloned().unwrap_or(Value::Null),
                    }),
                    _ => {}
                }
            }
            Some(ClaudeEvent::Assistant { text, tool_uses })
        }
        Some("result") => Some(ClaudeEvent::TurnResult {
            ok: !v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
                && v.get("subtype").and_then(Value::as_str) == Some("success"),
            text: s(&v, "result"),
            duration_ms: v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
            usage: turn_usage(&v),
        }),
        Some("rate_limit_event") => {
            let windows = limit_windows(v.get("rate_limit_info").unwrap_or(&Value::Null));
            (!windows.is_empty()).then_some(ClaudeEvent::Limits(windows))
        }
        Some("control_request") => control_request(&v),
        Some("control_response") => Some(ClaudeEvent::ControlAck {
            request_id: v
                .pointer("/response/request_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        }),
        _ => None,
    };
    Ok(event)
}

/// A request from the CLI to oxbot: a permission question, or a message for
/// a tool server oxbot hosts.
fn control_request(v: &Value) -> Option<ClaudeEvent> {
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let r = v.get("request").unwrap_or(&Value::Null);
    match r.get("subtype").and_then(Value::as_str) {
        Some("can_use_tool") => Some(ClaudeEvent::Permission(PermissionRequest {
            request_id: s(v, "request_id"),
            tool: s(r, "tool_name"),
            input: r.get("input").cloned().unwrap_or(Value::Null),
            tool_use_id: s(r, "tool_use_id"),
        })),
        Some("mcp_message") => Some(ClaudeEvent::McpMessage {
            request_id: s(v, "request_id"),
            server: s(r, "server_name"),
            message: r.get("message").cloned().unwrap_or(Value::Null),
        }),
        _ => None,
    }
}

fn turn_usage(v: &Value) -> TurnUsage {
    let n = |k: &str| {
        v.pointer(&format!("/usage/{k}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    TurnUsage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
        cost_usd: v
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    }
}

/// The plan windows in a `rate_limit_info`: every entry of
/// `unifiedWindows`, or else the single window it describes.
fn limit_windows(info: &Value) -> Vec<LimitWindow> {
    let window = |name: &str, w: &Value| LimitWindow {
        name: name.to_owned(),
        used: w.get("utilization").and_then(Value::as_f64).unwrap_or(0.0),
        resets_at: w.get("resetsAt").and_then(Value::as_i64).unwrap_or(0),
        status: w
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    };
    if let Some(all) = info.get("unifiedWindows").and_then(Value::as_object) {
        let mut windows: Vec<_> = all.iter().map(|(name, w)| window(name, w)).collect();
        windows.sort_by_key(|w| w.resets_at);
        return windows;
    }
    match info.get("rateLimitType").and_then(Value::as_str) {
        Some(name) => vec![window(name, info)],
        None => Vec::new(),
    }
}

/// A `user` line: a tool result going back to the model. Lines without one
/// (rare) still mean Claude is waiting on the model.
fn tool_result(v: &Value) -> ClaudeEvent {
    let blocks = v
        .pointer("/message/content")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let Some(block) = blocks
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
    else {
        return ClaudeEvent::Activity(Activity::Requesting);
    };
    let text = match block.get("content") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    ClaudeEvent::ToolResult {
        tool_use_id: block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse_all(fixture: &str) -> Vec<ClaudeEvent> {
        fixture
            .lines()
            .filter_map(|l| parse_line(l).unwrap())
            .collect()
    }

    #[test]
    fn two_turns_give_two_inits_and_two_results() {
        let events = parse_all(include_str!("../tests/fixtures/two-turns.jsonl"));
        let inits = events
            .iter()
            .filter(|e| matches!(e, ClaudeEvent::Init { .. }))
            .count();
        let results: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ClaudeEvent::TurnResult { ok, text, .. } => Some((*ok, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(inits, 2);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|(ok, _)| *ok));
    }

    #[test]
    fn streaming_deltas_add_up_to_the_full_message() {
        let events = parse_all(include_str!("../tests/fixtures/streaming.jsonl"));
        let mut deltas = String::new();
        let mut full = String::new();
        for e in &events {
            match e {
                ClaudeEvent::TextDelta(t) => deltas.push_str(t),
                ClaudeEvent::Assistant { text, .. } => full.push_str(text),
                _ => {}
            }
        }
        assert_ne!(full, "");
        assert_eq!(deltas, full);
    }

    #[test]
    fn tool_use_is_parsed_and_unknown_events_skipped() {
        let events = parse_all(include_str!("../tests/fixtures/tool-use.jsonl"));
        let ClaudeEvent::Assistant { text, tool_uses } = &events[1] else {
            panic!("expected an assistant message, got {:?}", events[1]);
        };
        assert_eq!(text, "I'll save that note.");
        assert_eq!(tool_uses[0].name, "Write");
        assert_eq!(tool_uses[0].summary(), "notes.md");
        assert!(matches!(
            events.last(),
            Some(ClaudeEvent::TurnResult {
                ok: true,
                duration_ms: 2300,
                ..
            })
        ));
        // init, assistant, tool result, assistant, result; the unknown event is skipped
        assert_eq!(events.len(), 5);
        assert!(
            matches!(&events[2], ClaudeEvent::ToolResult { tool_use_id, .. } if tool_use_id == &tool_uses[0].id)
        );
    }

    #[test]
    fn status_and_block_starts_become_activity() {
        let events = parse_all(include_str!("../tests/fixtures/streaming.jsonl"));
        assert!(events.contains(&ClaudeEvent::Activity(Activity::Requesting)));
        assert!(events.contains(&ClaudeEvent::Activity(Activity::Writing)));
        let tool = json!({"type":"stream_event","event":{"type":"content_block_start","index":1,
            "content_block":{"type":"tool_use","name":"mcp__chrome-devtools__navigate_page"}}});
        assert_eq!(
            parse_line(&tool.to_string()).unwrap(),
            Some(ClaudeEvent::Activity(Activity::UsingTool(
                "mcp__chrome-devtools__navigate_page".into()
            )))
        );
    }

    #[test]
    fn subagent_lines_are_skipped() {
        let line =
            json!({"type":"assistant","parent_tool_use_id":"toolu_9","message":{"content":[]}});
        assert_eq!(parse_line(&line.to_string()).unwrap(), None);
    }

    #[test]
    fn usage_and_plan_limits_are_read() {
        let result = json!({"type":"result","subtype":"success","is_error":false,"result":"hi","duration_ms":5,
            "total_cost_usd":0.0035,"usage":{"input_tokens":4,"cache_creation_input_tokens":15316,
            "cache_read_input_tokens":6251,"output_tokens":662}});
        let Some(ClaudeEvent::TurnResult { usage, .. }) = parse_line(&result.to_string()).unwrap()
        else {
            panic!("expected a result")
        };
        assert_eq!(
            usage,
            TurnUsage {
                input_tokens: 4,
                output_tokens: 662,
                cache_read_tokens: 6251,
                cache_write_tokens: 15316,
                cost_usd: 0.0035
            }
        );
        let limits = json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1_793_207_400,
            "rateLimitType":"five_hour","unifiedWindows":{
                "seven_day":{"utilization":0.31,"resetsAt":1_793_754_000,"status":"allowed"},
                "five_hour":{"utilization":0.02,"resetsAt":1_793_207_400,"status":"allowed"}}}});
        let Some(ClaudeEvent::Limits(windows)) = parse_line(&limits.to_string()).unwrap() else {
            panic!("expected limits")
        };
        assert_eq!(windows.len(), 2);
        assert_eq!(
            (windows[0].name.as_str(), windows[0].used),
            ("five_hour", 0.02)
        );
        assert_eq!(
            (windows[1].name.as_str(), windows[1].used),
            ("seven_day", 0.31)
        );
        let single = json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning",
            "resetsAt":5,"rateLimitType":"seven_day","utilization":0.8}});
        let Some(ClaudeEvent::Limits(windows)) = parse_line(&single.to_string()).unwrap() else {
            panic!("expected limits")
        };
        assert_eq!(windows[0].status, "allowed_warning");
    }

    #[test]
    fn permission_requests_are_parsed() {
        let line = json!({"type":"control_request","request_id":"r1","request":{
            "subtype":"can_use_tool","tool_name":"mcp__chrome-devtools__click",
            "input":{"uid":"1_4"},"tool_use_id":"toolu_1"}});
        assert_eq!(
            parse_line(&line.to_string()).unwrap(),
            Some(ClaudeEvent::Permission(PermissionRequest {
                request_id: "r1".into(),
                tool: "mcp__chrome-devtools__click".into(),
                input: json!({"uid":"1_4"}),
                tool_use_id: "toolu_1".into(),
            }))
        );
        let mcp = json!({"type":"control_request","request_id":"m1","request":{
            "subtype":"mcp_message","server_name":"oxbot",
            "message":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}});
        assert_eq!(
            parse_line(&mcp.to_string()).unwrap(),
            Some(ClaudeEvent::McpMessage {
                request_id: "m1".into(),
                server: "oxbot".into(),
                message: json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            })
        );
        let other = json!({"type":"control_request","request_id":"r2","request":{"subtype":"hook_callback"}});
        assert_eq!(parse_line(&other.to_string()).unwrap(), None);
    }

    #[test]
    fn tool_results_carry_their_text() {
        let line = json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result",
            "tool_use_id":"toolu_2","content":[{"type":"text","text":"a"},{"type":"image"},{"type":"text","text":"b"}]}]}});
        assert_eq!(
            parse_line(&line.to_string()).unwrap(),
            Some(ClaudeEvent::ToolResult {
                tool_use_id: "toolu_2".into(),
                text: "a\nb".into()
            })
        );
    }

    #[test]
    fn long_summaries_are_shortened() {
        let tool = ToolUse {
            id: String::new(),
            name: "Bash".into(),
            input: json!({ "command": "x".repeat(100) }),
        };
        assert_eq!(tool.summary().chars().count(), 81);
    }

    #[test]
    fn bad_json_is_an_error() {
        assert!(parse_line("not json").is_err());
    }
}
