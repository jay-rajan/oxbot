//! The Codex engine with the real `codex` program (skipped, saying why,
//! when it isn't installed): a browsing turn through the stand-in model
//! behind oxbot's relay and a scripted browser. Every browser step goes
//! through the gate (the same requests the loop's turns make), a refused
//! click is respected, nothing but the relay is contacted, and the thread
//! goes on after the process is started again.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers may panic"
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use oxbot_codex::{CodexEngine, CodexSetup, Found, MemoryCodexStore};
use oxbot_engine::{
    BrowserSpec, Decision, Engine, EngineEvent, GateMode, HostTools, ModelChoice, Privacy, Resume,
    SessionSpec, TurnInput,
};
use oxbot_llm::{Catalog, LlmError, ModelInfo, RawAnswer, ScriptedModel};
use oxbot_loop::store::MemoryLoopStore;
use oxbot_loop::tools::browser::{SameBrowser, ScriptedBrowser};
use oxbot_loop::{CatalogCache, LoopEngine, LoopSetup};
use oxbot_relay::{NoLog, Relay, RelaySetup, Upstream};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

const PAGE: &str = "## Pages\n1: Jobs (https://jobs.example.com/) [selected]\n## Latest page snapshot\nuid=1_1 RootWebArea \"Jobs\"\n  uid=1_3 button \"Delete account\"\n  uid=1_4 button \"Search\"";

/// The model: a snapshot, then a click on `uid`, then an answer.
#[derive(Debug)]
struct Model {
    uid: &'static str,
    bodies: Mutex<Vec<Value>>,
}

fn sse(events: &[Value]) -> RawAnswer {
    let mut text = String::new();
    for e in events {
        let _ = write!(
            text,
            "event: {}\ndata: {e}\n\n",
            e["type"].as_str().unwrap_or("")
        );
    }
    let chunks: Vec<Result<Bytes, LlmError>> = vec![Ok(Bytes::from(text))];
    RawAnswer {
        content_type: "text/event-stream".into(),
        body: futures_util::stream::iter(chunks).boxed(),
    }
}

fn call(id: &str, name: &str, args: &Value) -> Vec<Value> {
    let item = json!({"type": "function_call", "id": format!("fc_{id}"), "call_id": id,
        "name": name, "arguments": args.to_string()});
    vec![
        json!({"type": "response.output_item.added", "item": item}),
        json!({"type": "response.output_item.done", "item": item}),
    ]
}

fn text(t: &str) -> Vec<Value> {
    let item = json!({"type": "message", "id": "msg_x", "role": "assistant",
        "content": [{"type": "output_text", "text": t}]});
    vec![
        json!({"type": "response.output_item.added", "item": {"type": "message", "id": "msg_x", "role": "assistant", "content": []}}),
        json!({"type": "response.output_text.delta", "item_id": "msg_x", "delta": t}),
        json!({"type": "response.output_item.done", "item": item}),
    ]
}

impl Upstream for Model {
    fn send(
        &self,
        body: Vec<u8>,
        _stream: bool,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RawAnswer, LlmError>> {
        let req: Value = serde_json::from_slice(&body).unwrap();
        self.bodies.lock().unwrap().push(req.clone());
        let input = req["input"].as_array().cloned().unwrap_or_default();
        let last_user = input.iter().rposition(|i| i["role"] == "user").unwrap_or(0);
        let outputs: Vec<&Value> = input[last_user..]
            .iter()
            .filter(|i| i["type"] == "function_call_output")
            .collect();
        let checking = input[last_user]["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.starts_with("[oxbot] Before you answer"));
        let mut events = vec![json!({"type": "response.created", "response": {"id": "r"}})];
        let n = outputs.len();
        events.extend(match (checking, n) {
            (true, _) => text("Checked: the button was handled."),
            (false, 0) => call("c1", "take_snapshot", &json!({})),
            (false, 1) => call("c2", "click", &json!({"uid": self.uid})),
            _ => {
                let last = outputs.last().unwrap()["output"].as_str().unwrap_or("");
                if last.contains("[oxbot said no]") {
                    text("Okay, I didn't click it.")
                } else {
                    text("Clicked it.")
                }
            }
        });
        events.push(json!({"type": "response.completed", "response": {"id": "r",
            "model": "e2e/tool-model:free",
            "usage": {"input_tokens": 100, "output_tokens": 5, "total_tokens": 105}}}));
        Box::pin(async move { Ok(sse(&events)) })
    }
}

fn codex() -> Option<PathBuf> {
    std::env::var_os("OXBOT_CODEX")
        .map(PathBuf::from)
        .or_else(|| oxbot_codex::binary::locate(None, std::env::var("PATH").ok().as_deref()))
}

struct World {
    engine: CodexEngine,
    relay: Arc<Relay>,
    model: Arc<Model>,
    browser: Arc<ScriptedBrowser>,
    dir: tempfile::TempDir,
}

async fn world(uid: &'static str) -> Option<World> {
    let Some(program) = codex() else {
        eprintln!("SKIPPED: the codex program isn't installed (set OXBOT_CODEX to run this)");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let found = oxbot_codex::binary::find(Some(&program), None, &dir.path().join("probe")).await;
    let Found::Ready(_) = &found else {
        eprintln!(
            "SKIPPED: {}",
            found.problem().unwrap_or("codex isn't usable")
        );
        return None;
    };
    let store = Arc::new(MemoryLoopStore::new());
    let browser = ScriptedBrowser::with_snapshot(PAGE);
    let mut setup = LoopSetup::new(Arc::new(ScriptedModel::new()), store);
    setup.catalog = Arc::new(CatalogCache::new(Catalog {
        models: vec![ModelInfo {
            id: "e2e/tool-model:free".into(),
            name: "E2E".into(),
            context_length: 131_072,
            max_completion: Some(8192),
            prompt_usd_per_token: 0.0,
            completion_usd_per_token: 0.0,
            tools: true,
            images_in: false,
            text_io: true,
            free: true,
            supported_parameters: ["tools", "tool_choice", "max_tokens"]
                .map(str::to_owned)
                .to_vec(),
        }],
    }));
    setup.browser = Some(Arc::new(SameBrowser(Arc::clone(&browser))));
    setup.browser_allow = ["take_snapshot", "click", "navigate_page"]
        .map(str::to_owned)
        .to_vec();
    let loop_engine = Arc::new(LoopEngine::new(setup));
    let model = Arc::new(Model {
        uid,
        bodies: Mutex::new(Vec::new()),
    });
    let relay = Arc::new(
        Relay::start(RelaySetup {
            gateway: loop_engine.gateway(),
            upstream: Arc::clone(&model) as _,
            log: Arc::new(NoLog),
        })
        .await
        .unwrap(),
    );
    let engine = CodexEngine::new(CodexSetup {
        loop_engine,
        relay: Arc::clone(&relay),
        store: Arc::new(MemoryCodexStore::default()),
        found,
        idle_stop: Duration::from_mins(15),
        max_turn: Duration::from_mins(5),
    })
    .await
    .unwrap();
    Some(World {
        engine,
        relay,
        model,
        browser,
        dir,
    })
}

fn spec(dir: &Path, session: Resume) -> SessionSpec {
    let bot = dir.join("bots/scout");
    std::fs::create_dir_all(bot.join("work")).unwrap();
    SessionSpec {
        bot: "scout".into(),
        work_dir: bot.join("work"),
        context_dir: bot.join("context"),
        knowledge_dir: None,
        system_prompt: "You are \"Scout\", a bot running inside oxbot.".into(),
        resume: session,
        model: ModelChoice::Auto,
        browser: Some(BrowserSpec {
            mcp: oxbot_engine::McpCommand::default(),
        }),
        host_tools: HostTools::default(),
        host_tool_specs: Vec::new(),
        privacy: Privacy::Private,
        gate_mode: GateMode::Live,
        env: Vec::new(),
        tool_wait: Duration::from_mins(1),
    }
}

/// Runs one turn, answering the gate with `allow`; returns what the gate
/// was asked, and how the turn ended.
async fn turn(
    session: &mut Box<dyn oxbot_engine::Session>,
    events: &mut tokio::sync::mpsc::Receiver<EngineEvent>,
    text: &str,
    allow: impl Fn(&str, &Value) -> bool,
) -> (Vec<String>, bool, String, usize) {
    session
        .send(TurnInput {
            text: text.into(),
            user_seq: 1,
            person: Some("p_sam".into()),
            origin: Value::Null,
        })
        .await
        .unwrap();
    let mut asked = Vec::new();
    let mut meter = 0;
    let end = tokio::time::timeout(Duration::from_secs(90), async {
        while let Some(event) = events.recv().await {
            match event {
                EngineEvent::Permission(req) => {
                    asked.push(req.tool.clone());
                    let yes = allow(&req.tool, &req.input);
                    req.reply.send(if yes {
                        Decision::Allow
                    } else {
                        Decision::Deny("The person said no.".into())
                    });
                }
                EngineEvent::ModelCall(_) => meter += 1,
                EngineEvent::TurnDone { ok, text, .. } => return (ok, text),
                _ => {}
            }
        }
        (false, "events closed".to_owned())
    })
    .await
    .expect("the turn ended in time");
    (asked, end.0, end.1, meter)
}

#[tokio::test]
async fn a_codex_bot_browses_through_the_gate_and_only_the_relay_is_contacted() {
    let Some(w) = world("1_4").await else { return };
    let started = w
        .engine
        .start(spec(w.dir.path(), Resume::New("s1".into())))
        .await
        .unwrap();
    let (mut session, mut events) = (started.session, started.events);
    let (asked, ok, text, meter) =
        turn(&mut session, &mut events, "[Sam] search", |_, _| true).await;
    assert!(ok, "{text}");
    assert_eq!(text, "Checked: the button was handled.");
    // Both calls went to the gate under oxbot's own names.
    assert_eq!(
        asked,
        [
            "mcp__chrome-devtools__take_snapshot",
            "mcp__chrome-devtools__click"
        ]
    );
    assert_eq!(w.browser.calls().len(), 2);
    // Four requests: two steps, the answer, and the answer again once
    // oxbot sent it back to be checked.
    assert_eq!(meter, 4);
    let bodies = w.model.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 4);
    let tools: Vec<&str> = bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        tools.contains(&"click") && tools.contains(&"find_in_page"),
        "{tools:?}"
    );
    // Page scripts never; Codex's own shell (sandboxed, every program
    // asks first) for small scripts.
    assert!(!tools.iter().any(|t| t.contains("evaluate")), "{tools:?}");
    assert!(tools.contains(&"exec_command"), "{tools:?}");
    for body in &bodies {
        let text = body.to_string();
        assert_eq!(
            body["instructions"],
            "You are \"Scout\", a bot running inside oxbot."
        );
        assert!(
            !text.contains("prompt_cache_key") && !text.contains("client_metadata"),
            "{text}"
        );
        assert_eq!(body["provider"]["data_collection"], "deny");
    }
    // The page came back marked as someone else's text.
    let first_result = bodies[1]["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["type"] == "function_call_output")
        .unwrap()["output"]
        .to_string();
    assert!(first_result.contains("web_page"), "{first_result}");
    assert_eq!(w.relay.requests_seen(), 4);
    assert_eq!(
        w.engine.refused_connections(),
        0,
        "Codex tried to reach something else"
    );
    session.shutdown().await;
}

#[tokio::test]
async fn a_refused_click_is_respected_and_the_thread_goes_on_after_a_restart() {
    let Some(w) = world("1_3").await else { return };
    let started = w
        .engine
        .start(spec(w.dir.path(), Resume::New("s2".into())))
        .await
        .unwrap();
    let (mut session, mut events) = (started.session, started.events);
    let (asked, ok, text, _) = turn(&mut session, &mut events, "[Sam] delete it", |tool, _| {
        !tool.ends_with("click")
    })
    .await;
    assert!(ok, "{text}");
    assert_eq!(text, "Okay, I didn't click it.");
    assert_eq!(asked.len(), 2);
    // The click never reached the browser.
    assert!(w.browser.calls().iter().all(|(name, _)| name != "click"));
    session.shutdown().await;
    // A new session on the same saved one: the thread goes on in a new
    // process, with what it had.
    let started = w
        .engine
        .start(spec(w.dir.path(), Resume::Resume("s2".into())))
        .await
        .unwrap();
    let (mut session, mut events) = (started.session, started.events);
    let before = w.model.bodies.lock().unwrap().len();
    let (_, ok, text, _) = turn(&mut session, &mut events, "[Sam] what now?", |_, _| true).await;
    assert!(ok, "{text}");
    let bodies = w.model.bodies.lock().unwrap().clone();
    let resumed = bodies[before]["input"].to_string();
    assert!(
        resumed.contains("delete it"),
        "the thread went on: {resumed}"
    );
    assert_eq!(w.engine.refused_connections(), 0);
    session.shutdown().await;
}
