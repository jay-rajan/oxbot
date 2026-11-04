//! The stand-ins, run as the e2e runner runs them: the stand-in
//! `OpenRouter` (its API, streaming, routing, limits, sign-in, log) and the
//! stand-in browser tool over stdio.

#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use oxbot_e2e::fake_openrouter::challenge;
use oxbot_e2e::http::{Reply, send};
use oxbot_e2e::serve::FakeOpenRouter;
use serde_json::{Value, json};

const SCRIPT: &str = r###"
[[reply]]
when = "search jobs"
say = "Looking."
tool = "mcp__chrome-devtools__click"
args = { uid = "2_4" }
snapshot = "## Pages\n1: Jobs (https://jobs.example.com/) [selected]\n## Latest page snapshot\nuid=2_1 RootWebArea \"Jobs\"\n  uid=2_4 button \"Search\""
after = "Searched the jobs."
denied = "Okay, I didn't search."

[[reply]]
when = "make a bot"
tool = "create_bot"
args = { name = "Recipes" }
after = "Created it. {result}"

[[reply]]
when = "busy"
say = "Answered."
busy = ["e2e/tool-model:free"]
usage = { prompt = 1200, completion = 80, cost = 0.0 }

[[reply]]
when = "privacy"
say = "Not reached."
data_policy = ["e2e/tool-model:free"]

[[reply]]
when = "post it"
tool = "mcp__chrome-devtools__click"
args = { uid = "1_3" }
snapshot = "## Pages\n1: Jobs (https://jobs.example.com/apply) [selected]\n## Latest page snapshot\nuid=1_1 RootWebArea \"Jobs\"\n  uid=1_3 button \"Post\""
after = "Posted."

[[reply]]
when = "attached"
say = "Here's what I found."
read = true

[[reply]]
say = "Got it."
"###;

fn script_file(dir: &Path) -> PathBuf {
    let path = dir.join("script.toml");
    std::fs::write(&path, SCRIPT).unwrap();
    path
}

async fn start(dir: &Path, daily: Option<u32>) -> FakeOpenRouter {
    FakeOpenRouter::start(
        Path::new(env!("CARGO_BIN_EXE_oxbot-fake-openrouter")),
        &script_file(dir),
        daily,
        None,
    )
    .await
    .unwrap()
}

/// The browser tools as an OpenAI-style tool list (as the loop sends them).
fn tools() -> Vec<Value> {
    oxbot_e2e::fake_devtools::tools()
        .iter()
        .map(|t| {
            json!({"type":"function","function":{
                "name": format!("mcp__chrome-devtools__{}", t["name"].as_str().unwrap()),
                "description": t["description"],
                "parameters": t["inputSchema"],
            }})
        })
        .chain([json!({"type":"function","function":{"name":"mcp__oxbot__create_bot","parameters":{"type":"object"}}})])
        .collect()
}

async fn chat(fake: &FakeOpenRouter, body: &Value) -> Reply {
    let bearer = format!("Bearer {}", fake.canary);
    send(
        fake.port,
        "POST",
        "/api/v1/chat/completions",
        &[("Authorization", &bearer)],
        Some(&body.to_string()),
    )
    .await
    .unwrap()
}

fn request(model: &str, messages: &[Value]) -> Value {
    json!({
        "model": model,
        "messages": messages,
        "tools": tools(),
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "provider": { "data_collection": "deny", "zdr": true, "require_parameters": true },
    })
}

/// A request the backup can take: it has no `tool_choice` or
/// `parallel_tool_calls`, so oxbot leaves them out.
fn backup_request(model: &str, messages: &[Value]) -> Value {
    let mut body = request(model, messages);
    let fields = body.as_object_mut().unwrap();
    fields.remove("tool_choice");
    fields.remove("parallel_tool_calls");
    body
}

/// Runs a reply to its end as the loop would, answering each tool call
/// with `result(name)`; returns the calls and the final text.
async fn walk(
    fake: &FakeOpenRouter,
    message: &str,
    result: impl Fn(&str) -> String,
) -> (Vec<(String, Value)>, String) {
    let mut messages = vec![
        json!({"role":"system","content":"rules"}),
        json!({"role":"user","content": format!("[Jay] {message}")}),
    ];
    let mut calls = Vec::new();
    for _ in 0..10 {
        let reply = chat(fake, &request("e2e/tool-model:free", &messages)).await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let v: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(v["model"], "e2e/tool-model:free");
        let msg = &v["choices"][0]["message"];
        let Some(call) = msg["tool_calls"].get(0) else {
            assert_eq!(v["choices"][0]["finish_reason"], "stop");
            return (calls, msg["content"].as_str().unwrap_or("").to_owned());
        };
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        let name = call["function"]["name"].as_str().unwrap().to_owned();
        let args: Value =
            serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
        messages.push(msg.clone());
        messages.push(json!({"role":"tool","tool_call_id":call["id"],"content":result(&name)}));
        calls.push((name, args));
    }
    panic!("the stand-in never finished");
}

#[tokio::test]
async fn a_browsing_reply_is_acted_out_one_call_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let fake = start(dir.path(), None).await;
    let (calls, text) = walk(&fake, "search jobs please", |_| "done".to_owned()).await;
    let names: Vec<&str> = calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "mcp__chrome-devtools__navigate_page",
            "mcp__chrome-devtools__take_snapshot",
            "mcp__chrome-devtools__click"
        ]
    );
    assert_eq!(
        calls[0].1,
        json!({"type":"url","url":"https://jobs.example.com/","pageId":1})
    );
    assert_eq!(calls[2].1, json!({"uid":"2_4","pageId":1}));
    assert_eq!(text, "Searched the jobs.");

    // Refused: the reply's `denied`, right after the refusal.
    let (calls, text) = walk(&fake, "search jobs", |name| {
        if name.ends_with("click") {
            "[oxbot said no] The person said no.".to_owned()
        } else {
            "done".to_owned()
        }
    })
    .await;
    assert_eq!(calls.len(), 3);
    assert_eq!(text, "Okay, I didn't search.");
    // A tool error ends it too.
    let (calls, text) = walk(&fake, "post it", |_| "[tool error] no page".to_owned()).await;
    assert_eq!(calls.len(), 1);
    assert_eq!(text, "That didn't work: no page");
    // oxbot's own tools, with the result filled in.
    let (calls, text) = walk(&fake, "make a bot", |_| "Created Recipes.".to_owned()).await;
    assert_eq!(calls[0].0, "mcp__oxbot__create_bot");
    assert_eq!(text, "Created it. Created Recipes.");
    // No tool: just the words.
    let (calls, text) = walk(&fake, "hello", |_| String::new()).await;
    assert_eq!(calls.len(), 0);
    assert_eq!(text, "Got it.");

    // Attached files are Read, then described.
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "Buy milk\n").unwrap();
    let message = format!("files attached:\n- {} (text)", notes.display());
    let (calls, text) = walk(&fake, &message, |_| "Buy milk".to_owned()).await;
    assert_eq!(
        calls,
        [(
            "Read".to_owned(),
            json!({"file_path": notes.display().to_string()})
        )]
    );
    assert_eq!(text, "Here's what I found.\nRead notes.txt: Buy milk");

    let log = fake.requests();
    assert!(log.contains(r#""data_collection":"deny""#), "{log}");
    assert!(log.contains(r#""bearer":"ok""#));
    assert!(
        !log.contains(oxbot_e2e::canary::needle(&fake.canary)),
        "the log never has the key"
    );
    assert!(fake.bodies().contains("search jobs please"));
}

#[tokio::test]
async fn streams_come_in_pieces_like_openrouters() {
    let dir = tempfile::tempdir().unwrap();
    let fake = start(dir.path(), None).await;
    let mut body = request(
        "e2e/tool-model:free",
        &[json!({"role":"user","content":"[Jay] make a bot"})],
    );
    body["stream"] = json!(true);
    let reply = chat(&fake, &body).await;
    assert_eq!(reply.status, 200);
    assert!(
        reply
            .header("content-type")
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert!(reply.body.starts_with(": OPENROUTER PROCESSING\n\n"));
    assert!(reply.body.ends_with("data: [DONE]\n\n"));
    let mut name = String::new();
    let mut args = String::new();
    let mut usage = Value::Null;
    for event in reply.body.split("\n\n") {
        let Some(data) = event.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            continue;
        }
        let v: Value = serde_json::from_str(data).unwrap();
        let call = &v["choices"][0]["delta"]["tool_calls"][0]["function"];
        name.push_str(call["name"].as_str().unwrap_or(""));
        args.push_str(call["arguments"].as_str().unwrap_or(""));
        if v["usage"].is_object() {
            usage = v["usage"].clone();
        }
    }
    assert_eq!(name, "mcp__oxbot__create_bot");
    assert_eq!(
        serde_json::from_str::<Value>(&args).unwrap(),
        json!({"name":"Recipes"})
    );
    // The request counted at a token per 3 bytes.
    assert_eq!(usage["prompt_tokens"], body.to_string().len() / 3);
}

#[tokio::test]
async fn busy_models_fall_back_and_bad_requests_fail() {
    let dir = tempfile::tempdir().unwrap();
    let fake = start(dir.path(), Some(2)).await;
    let user = [json!({"role":"user","content":"[Jay] busy now?"})];

    let mut body = request("e2e/tool-model:free", &user);
    let reply = chat(&fake, &body).await;
    assert_eq!(reply.status, 429);
    assert_eq!(reply.header("retry-after"), Some("1"));

    body["models"] = json!(["e2e/backup:free"]);
    // The backup takes no tool_choice: with require_parameters, sending it
    // rules the backup out, and only the busy model is left.
    let reply = chat(&fake, &body).await;
    assert_eq!(reply.status, 429, "{}", reply.body);
    let mut body = backup_request("e2e/tool-model:free", &user);
    body["models"] = json!(["e2e/backup:free"]);
    let reply = chat(&fake, &body).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let v: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(v["model"], "e2e/backup:free");
    assert_eq!(v["usage"]["prompt_tokens"], 1200);
    assert_eq!(v["choices"][0]["message"]["content"], "Answered.");

    // The account's privacy settings rule out a model: another may
    // answer; with none left, OpenRouter's 404 and its words.
    let privacy = [json!({"role":"user","content":"[Jay] privacy?"})];
    let body = request("e2e/tool-model:free", &privacy);
    let reply = chat(&fake, &body).await;
    assert_eq!(reply.status, 404);
    assert!(
        reply
            .body
            .contains("No endpoints found matching your data policy"),
        "{}",
        reply.body
    );

    // Unknown models, orphan tool results and no key are refused.
    let reply = chat(&fake, &request("e2e/nope", &user)).await;
    assert_eq!(reply.status, 400);
    let orphan = [
        user[0].clone(),
        json!({"role":"tool","tool_call_id":"x","content":"?"}),
    ];
    assert_eq!(
        chat(&fake, &request("e2e/backup:free", &orphan))
            .await
            .status,
        400
    );
    let reply = send(
        fake.port,
        "POST",
        "/api/v1/chat/completions",
        &[],
        Some(&request("e2e/backup:free", &user).to_string()),
    )
    .await
    .unwrap();
    assert_eq!(reply.status, 401);
    let wrong = send(
        fake.port,
        "GET",
        "/api/v1/key",
        &[("Authorization", "Bearer sk-or-v1-wrong")],
        None,
    )
    .await
    .unwrap();
    assert_eq!(wrong.status, 401);

    // Two free requests a day: the second answered, the third refused.
    let reply = chat(&fake, &backup_request("e2e/backup:free", &user)).await;
    assert_eq!(reply.status, 200);
    let reply = chat(&fake, &backup_request("e2e/backup:free", &user)).await;
    assert_eq!(reply.status, 429);
    assert!(reply.body.contains("free-models-per-day"), "{}", reply.body);
    // The paid model still answers.
    assert_eq!(chat(&fake, &request("e2e/paid", &user)).await.status, 200);

    let bearer = format!("Bearer {}", fake.canary);
    let key = send(
        fake.port,
        "GET",
        "/api/v1/key",
        &[("Authorization", &bearer)],
        None,
    )
    .await
    .unwrap();
    let key: Value = serde_json::from_str(&key.body).unwrap();
    assert_eq!(
        key["data"]["free_model_daily_requests"],
        json!({"used":2,"limit":2,"remaining":0})
    );
    assert_eq!(key["data"]["is_free_tier"], true);

    let count = send(fake.port, "GET", "/count", &[], None).await.unwrap();
    let count: Value = serde_json::from_str(&count.body).unwrap();
    assert_eq!(count["ok"], 3);
    assert_eq!(count["by_model"]["e2e/backup:free"], 2);

    let models = send(fake.port, "GET", "/api/v1/models", &[], None)
        .await
        .unwrap();
    let models: Value = serde_json::from_str(&models.body).unwrap();
    assert_eq!(models["data"].as_array().unwrap().len(), 5);
}

/// The value of `name="…"`'s input on a page.
fn form_value(page: &str, name: &str) -> String {
    let at = page.find(&format!("name={name} value=\"")).unwrap();
    let rest = &page[at + name.len() + 13..];
    rest[..rest.find('"').unwrap()].to_owned()
}

async fn sign_in_code(fake: &FakeOpenRouter, verifier: &str, wrong_state: bool) -> (Reply, String) {
    let path = format!(
        "/auth?callback_url=http%3A%2F%2Flocalhost%3A7878%2Fopenrouter%2Fcallback&code_challenge={}&code_challenge_method=S256&state=st4te&key_label=oxbot%20on%20Mac",
        challenge(verifier)
    );
    let page = send(fake.port, "GET", &path, &[], None).await.unwrap();
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Authorize oxbot"));
    assert!(page.body.contains("oxbot on Mac"));
    let code = form_value(&page.body, "code");
    let extra = if wrong_state { "&wrong_state=1" } else { "" };
    let back = send(
        fake.port,
        "GET",
        &format!("/auth/approve?code={code}{extra}"),
        &[],
        None,
    )
    .await
    .unwrap();
    (back, code)
}

#[tokio::test]
async fn sign_in_gives_the_canary_only_for_the_right_verifier() {
    let dir = tempfile::tempdir().unwrap();
    let fake = start(dir.path(), None).await;
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk-and-more-random-text-0123456789";
    let (back, code) = sign_in_code(&fake, verifier, false).await;
    assert_eq!(back.status, 302);
    assert_eq!(
        back.header("location").unwrap(),
        format!("http://localhost:7878/openrouter/callback?code={code}&state=st4te")
    );
    let exchange = |code: &str, verifier: &str| {
        json!({"code": code, "code_verifier": verifier, "code_challenge_method": "S256"})
            .to_string()
    };
    let wrong = send(
        fake.port,
        "POST",
        "/api/v1/auth/keys",
        &[],
        Some(&exchange(&code, "nope")),
    )
    .await
    .unwrap();
    assert_eq!(wrong.status, 400);
    // A code works once: the failed try used it up.
    let again = send(
        fake.port,
        "POST",
        "/api/v1/auth/keys",
        &[],
        Some(&exchange(&code, verifier)),
    )
    .await
    .unwrap();
    assert_eq!(again.status, 400);

    let (back, code) = sign_in_code(&fake, verifier, true).await;
    assert!(back.header("location").unwrap().ends_with("state=st4tex"));
    let ok = send(
        fake.port,
        "POST",
        "/api/v1/auth/keys",
        &[],
        Some(&exchange(&code, verifier)),
    )
    .await
    .unwrap();
    assert_eq!(ok.status, 200);
    let v: Value = serde_json::from_str(&ok.body).unwrap();
    assert_eq!(v["key"], fake.canary.as_str());
    assert_eq!(
        std::fs::read_to_string(fake.key_file()).unwrap(),
        fake.canary
    );

    // Only loopback callbacks and S256.
    let bad = send(
        fake.port,
        "GET",
        "/auth?callback_url=https%3A%2F%2Fevil.example%2F&code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&state=s",
        &[],
        None,
    )
    .await
    .unwrap();
    assert_eq!(bad.status, 400);
    assert!(!fake.requests().contains(&code), "codes aren't logged");
}

/// A stand-in on stdio: send a line, read lines.
struct Stdio2 {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Stdio2 {
    fn start(program: &str, script: &Path, env: &[(&str, &Path)]) -> Self {
        let mut cmd = Command::new(program);
        cmd.env("OXBOT_FAKE_SCRIPT", script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }

    fn send(&mut self, v: &Value) {
        writeln!(self.input, "{v}").unwrap();
        self.input.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for Stdio2 {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_browser_stand_in_speaks_mcp_over_stdio() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("devtools.jsonl");
    let mut dt = Stdio2::start(
        env!("CARGO_BIN_EXE_oxbot-fake-devtools"),
        &script_file(dir.path()),
        &[("OXBOT_FAKE_DT_LOG", &log)],
    );
    dt.send(&json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}));
    assert_eq!(dt.read()["result"]["serverInfo"]["name"], "chrome_devtools");
    dt.send(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    dt.send(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}));
    assert_eq!(dt.read()["result"]["tools"].as_array().unwrap().len(), 17);
    dt.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"navigate_page","arguments":{"pageId":1,"type":"url","url":"https://jobs.example.com/apply"}}}));
    assert_eq!(dt.read()["id"], 2);
    dt.send(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"take_snapshot","arguments":{"pageId":1}}}));
    let snap = dt.read();
    assert!(
        snap["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("uid=1_3 button \"Post\"")
    );
    drop(dt);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls.lines().count(), 2);
    assert!(calls.contains("\"tool\":\"take_snapshot\""));
}
