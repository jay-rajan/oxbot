//! A stand-in for the `claude` program in end-to-end tests: it speaks
//! Claude Code's stream-json protocol and answers each message from a
//! script, so tests are free, fast and give the same result every time.
//!
//! The script (`OXBOT_FAKE_SCRIPT`, a TOML file) is a list of replies; the
//! first whose `when` appears in the message (any case) is used, and one
//! without `when` is the fallback:
//!
//! ```toml
//! [[reply]]
//! when = "make a bot"
//! tool = "create_bot"                      # oxbot's own tool, or a full mcp__… name
//! args = { name = "Recipes", instructions = "Find recipes." }
//! say = "Making it…"                        # said before the tool (optional)
//! snapshots = ["## Pages\n1: A (https://a.example.com/) [selected]"]  # pages "read" first
//! after = "Done: {result}"                  # said after it worked
//! denied = "Okay, I won't."                 # said if the user said no
//!
//! [[reply]]
//! when = "half"
//! say = "About half."
//! limits = { five_hour = 0.55, seven_day = 0.2 } # plan usage reported first
//! model = "claude-sonnet-4-5-20250929"           # the model it says it is
//!
//! [[reply]]
//! when = "what's in"
//! read = true                               # open each attached file (see below)
//!
//! [[reply]]
//! say = "Hello from the stand-in."
//! ```
//!
//! With `read`, it opens every file oxbot's note lists, as Claude would with
//! Read, and says what it found in each: the first line of a text file or of
//! the text oxbot made of it, how many still frames a video became, or the
//! kind of a picture or PDF. That shows what oxbot made of a file without a
//! real Claude.

use std::io::{BufRead, Write};

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Default, Deserialize)]
struct Script {
    #[serde(default)]
    reply: Vec<Reply>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct Reply {
    when: Option<String>,
    say: Option<String>,
    tool: Option<String>,
    #[serde(default)]
    args: toml::Table,
    /// A fake page snapshot to "read" first (for browser tools).
    snapshot: Option<String>,
    /// More fake page snapshots to "read" first, in order (after
    /// `snapshot`), e.g. the pages a report cites.
    #[serde(default)]
    snapshots: Vec<String>,
    after: Option<String>,
    denied: Option<String>,
    /// Wait this long before answering (to see "working" states).
    #[serde(default)]
    delay_ms: u64,
    /// Open the attached files and say what's in them.
    #[serde(default)]
    read: bool,
    /// Plan usage to report before answering, as Claude's `rate_limit_event`.
    limits: Option<Limits>,
    /// The model it says it is (default `fake`).
    model: Option<String>,
}

/// The plan's windows, from 0 to 1. The 5-hour one resets in
/// `resets_in_min` (default 3 hours), the weekly one in 3 days.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Limits {
    five_hour: Option<f64>,
    seven_day: Option<f64>,
    resets_in_min: Option<i64>,
}

impl Limits {
    fn event(&self) -> Value {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        let mut windows = serde_json::Map::new();
        let five_at = now + self.resets_in_min.unwrap_or(180) * 60;
        for (name, used, at) in [
            ("five_hour", self.five_hour, five_at),
            ("seven_day", self.seven_day, now + 3 * 86_400),
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
    let script: Script = std::env::var("OXBOT_FAKE_SCRIPT")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| toml::from_str(&t).ok())
        .unwrap_or_default();
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
                let reply = pick(&script, &text);
                let model = reply.model.as_deref().unwrap_or("fake");
                out.line(
                    &json!({"type":"system","subtype":"init","session_id":"fake","model":model}),
                );
                if let Some(limits) = &reply.limits {
                    out.line(&limits.event());
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
                            out.done(
                                reply
                                    .denied
                                    .as_deref()
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

/// The message's text, without the `[Name] ` oxbot puts in front.
fn user_text(content: &Value) -> String {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };
    match text.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((_, rest)) => rest.to_owned(),
        None => text,
    }
}

fn pick(script: &Script, text: &str) -> Reply {
    let lower = text.to_lowercase();
    script
        .reply
        .iter()
        .find(|r| {
            r.when
                .as_ref()
                .is_some_and(|w| lower.contains(&w.to_lowercase()))
        })
        .or_else(|| script.reply.iter().find(|r| r.when.is_none()))
        .cloned()
        .unwrap_or_else(|| Reply {
            say: Some(format!("(stand-in) You said: {text}")),
            ..Reply::default()
        })
}

/// Opens each file oxbot's note lists (a `- /path (…)` line), as Read
/// would, and says what's in it, one line each.
fn read_files(out: &mut Out, text: &str) -> String {
    let mut said = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("- /") else {
            continue;
        };
        let (path, about) = rest.split_once(" (").unwrap_or((rest, ""));
        let path = format!("/{path}");
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let about = about
            .strip_suffix(')')
            .unwrap_or(about)
            .trim_end_matches(')');
        let readable = about.rsplit_once(": ").map(|(_, p)| p.to_owned());
        let (opened, found) = if let Some(frames) = about
            .split_once("still frames: ")
            .or_else(|| about.split_once("still frame: "))
        {
            let dir = frames.1.split("/01.jpg").next().unwrap_or("");
            let n = std::fs::read_dir(dir).map_or(0, Iterator::count);
            (format!("{dir}/01.jpg"), format!("{n} frames of {name}"))
        } else if let Some(txt) = readable.filter(|p| {
            std::path::Path::new(p)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("txt"))
        }) {
            let first = first_line(&txt);
            (txt, format!("{name}: {first}"))
        } else if about == "text" {
            (path.clone(), format!("{name}: {}", first_line(&path)))
        } else {
            let kind = about.split([',', ':']).next().unwrap_or("file");
            (path.clone(), format!("{name} ({kind})"))
        };
        let n = out.next();
        out.line(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":format!("r{n}"),"name":"Read","input":{"file_path":opened}}]},"parent_tool_use_id":null}));
        out.line(&json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("r{n}"),"content":found}]},"parent_tool_use_id":null}));
        said.push(format!("Read {found}"));
    }
    if said.is_empty() {
        "I don't see any files.".to_owned()
    } else {
        said.join("\n")
    }
}

/// The first line of a file that has words.
fn first_line(path: &str) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| t.lines().find(|l| !l.trim().is_empty()).map(str::to_owned))
        .unwrap_or_else(|| "(can't read it)".to_owned())
}

fn start(out: &mut Out, reply: Reply, text: &str) -> Waiting {
    if reply.read {
        let found = read_files(out, text);
        let said = match &reply.say {
            Some(say) => format!("{say}\n{found}"),
            None => found,
        };
        out.done(&said);
        return Waiting::Nothing;
    }
    let Some(tool) = reply.tool.clone() else {
        out.done(reply.say.as_deref().unwrap_or(""));
        return Waiting::Nothing;
    };
    if let Some(say) = &reply.say {
        out.say(say);
    }
    let name = if tool.starts_with("mcp__") {
        tool
    } else {
        format!("mcp__oxbot__{tool}")
    };
    for snapshot in reply.snapshot.iter().chain(&reply.snapshots) {
        let n = out.next();
        out.line(&json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":format!("s{n}"),"name":"mcp__chrome-devtools__take_snapshot","input":{}}]},"parent_tool_use_id":null}));
        out.line(&json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":format!("s{n}"),"content":snapshot}]},"parent_tool_use_id":null}));
    }
    let input = Value::Object(
        reply
            .args
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::to_value(v).unwrap_or(Value::Null)))
            .collect(),
    );
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
