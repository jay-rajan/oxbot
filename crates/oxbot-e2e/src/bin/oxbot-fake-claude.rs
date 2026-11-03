//! A stand-in for the `claude` program in end-to-end tests: it speaks
//! Claude Code's stream-json protocol and answers each message from a
//! script, so tests are free, fast and give the same result every time.
//!
//! The script (`OXBOT_FAKE_SCRIPT`, a TOML file) is a list of replies; see
//! [`oxbot_e2e::script`] for its format. The stand-in `OpenRouter` reads the
//! same scripts, so a test's replies are the same on either engine.

use std::io::{BufRead, Write};

use oxbot_e2e::script::{Limits, Reply, Script, attached, read_answer, user_text};
use serde_json::{Value, json};

/// Claude's `rate_limit_event` for the plan usage a reply reports.
fn limits_event(limits: &Limits) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let mut windows = serde_json::Map::new();
    let five_at = now + limits.resets_in_min.unwrap_or(180) * 60;
    for (name, used, at) in [
        ("five_hour", limits.five_hour, five_at),
        ("seven_day", limits.seven_day, now + 3 * 86_400),
    ] {
        if let Some(used) = used {
            windows.insert(
                name.to_owned(),
                json!({"utilization": used, "resetsAt": at, "status": "allowed"}),
            );
        }
    }
    json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","unifiedWindows":windows}})
}

/// What the stand-in is waiting for.
enum Waiting {
    Nothing,
    /// A yes or no for the tool call.
    Permission(Reply, String, Value),
    /// The answer to an oxbot tool call.
    Mcp(Reply),
}

struct Out(std::io::Stdout, u64);

impl Out {
    fn line(&mut self, v: &Value) {
        let mut lock = self.0.lock();
        let _ = writeln!(lock, "{v}");
        let _ = lock.flush();
    }
    fn next(&mut self) -> u64 {
        self.1 += 1;
        self.1
    }
    fn say(&mut self, text: &str) {
        if !text.is_empty() {
            self.line(&json!({"type":"assistant","message":{"content":[{"type":"text","text":text}]},"parent_tool_use_id":null}));
        }
    }
    fn done(&mut self, text: &str) {
        self.say(text);
        self.line(&json!({"type":"result","subtype":"success","is_error":false,"result":text,
            "duration_ms":5,"total_cost_usd":0.0,
            "usage":{"input_tokens":3,"output_tokens":7,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}));
    }
}

fn main() {
    let script = Script::from_env();
    let mut out = Out(std::io::stdout(), 0);
    let mut waiting = Waiting::Nothing;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match msg["type"].as_str() {
            Some("user") => {
                let text = user_text(&msg["message"]["content"]);
                let reply = script.pick(&text);
                let model = reply.model.as_deref().unwrap_or("fake");
                out.line(
                    &json!({"type":"system","subtype":"init","session_id":"fake","model":model}),
                );
                if let Some(limits) = &reply.limits {
                    out.line(&limits_event(limits));
                }
                std::thread::sleep(std::time::Duration::from_millis(reply.delay_ms));
                waiting = start(&mut out, reply, &text);
            }
            Some("control_request") => {
                let id = msg["request_id"].clone();
                out.line(&json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{}}}));
                if msg["request"]["subtype"] == "interrupt" {
                    waiting = Waiting::Nothing;
                    out.done("Stopped.");
                }
            }
            Some("control_response") => {
                let response = &msg["response"]["response"];
                waiting = match std::mem::replace(&mut waiting, Waiting::Nothing) {
                    Waiting::Permission(reply, name, input) => {
                        if response["behavior"] == "allow" {
                            run_tool(&mut out, reply, &name, &input)
                        } else {
                            let why = response["message"].as_str().unwrap_or("");
                            // A practice run's "no" says it was recorded as done.
                            let dry = why
                                .starts_with("[test run]")
                                .then_some(reply.dry.as_deref())
                                .flatten();
                            out.done(
                                dry.or(reply.denied.as_deref())
                                    .unwrap_or(&format!("I didn't: {why}")),
                            );
                            Waiting::Nothing
                        }
                    }
                    Waiting::Mcp(reply) => {
                        let result = &response["mcp_response"]["result"];
                        let text = result["content"][0]["text"].as_str().unwrap_or("");
                        let failed = result["isError"] == true
                            || response["mcp_response"]["error"].is_object();
                        let text = if text.is_empty() {
                            response["mcp_response"]["error"]["message"]
                                .as_str()
                                .unwrap_or("")
                        } else {
                            text
                        };
                        let after = if failed {
                            format!("That didn't work: {text}")
                        } else {
                            reply
                                .after
                                .as_deref()
                                .unwrap_or("Done.")
                                .replace("{result}", text)
                        };
                        out.done(&after);
                        Waiting::Nothing
                    }
                    Waiting::Nothing => Waiting::Nothing,
                };
            }
            _ => {}
        }
    }
}

/// Opens each file oxbot's note lists, as Read would, and says what's in
/// it, one line each.
fn read_files(out: &mut Out, text: &str, say: Option<&str>) -> String {
    let files = attached(text);
    for file in &files {
        let n = out.next();
        out.line(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":format!("r{n}"),"name":"Read","input":{"file_path":file.opened}}]},"parent_tool_use_id":null}));
        out.line(&json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("r{n}"),"content":file.found}]},"parent_tool_use_id":null}));
    }
    read_answer(say, &files)
}

fn start(out: &mut Out, reply: Reply, text: &str) -> Waiting {
    if reply.read {
        let said = read_files(out, text, reply.say.as_deref());
        out.done(&said);
        return Waiting::Nothing;
    }
    let Some(name) = reply.tool_name() else {
        out.done(reply.say.as_deref().unwrap_or(""));
        return Waiting::Nothing;
    };
    if let Some(say) = &reply.say {
        out.say(say);
    }
    for snapshot in reply.page_snapshots() {
        let n = out.next();
        out.line(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":format!("s{n}"),"name":"mcp__chrome-devtools__take_snapshot","input":{}}]},"parent_tool_use_id":null}));
        out.line(&json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("s{n}"),"content":snapshot}]},"parent_tool_use_id":null}));
    }
    let input = reply.args_json();
    let n = out.next();
    out.line(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":format!("c{n}"),"name":name,"input":input}]},"parent_tool_use_id":null}));
    out.line(&json!({"type":"control_request","request_id":format!("r{n}"),"request":{"subtype":"can_use_tool","tool_name":name,"input":input,"tool_use_id":format!("c{n}")}}));
    Waiting::Permission(reply, name, input)
}

fn run_tool(out: &mut Out, reply: Reply, name: &str, input: &Value) -> Waiting {
    let Some(tool) = name.strip_prefix("mcp__oxbot__") else {
        // A browser tool: pretend it worked.
        let n = out.next();
        out.line(&json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("c{n}"),"content":"done"}]},"parent_tool_use_id":null}));
        out.done(
            &reply
                .after
                .clone()
                .unwrap_or_else(|| "Done.".to_owned())
                .replace("{result}", "done"),
        );
        return Waiting::Nothing;
    };
    let n = out.next();
    out.line(&json!({"type":"control_request","request_id":format!("m{n}"),"request":{"subtype":"mcp_message","server_name":"oxbot",
        "message":{"jsonrpc":"2.0","id":n,"method":"tools/call","params":{"name":tool,"arguments":input}}}}));
    Waiting::Mcp(reply)
}
