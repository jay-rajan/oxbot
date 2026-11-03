//! Whole turns on a scripted model, browser and hub.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use oxbot_engine::{
    BrowserSpec, Decision, Engine, EngineEvent, GateMode, HostTools, McpCommand, ModelChoice,
    Privacy, Resume, SessionSpec, ToolReply, TurnInput,
};
use oxbot_llm::{
    Catalog, DataCollection, ErrorKind, LlmError, ModelInfo, Role, ScriptedModel, Step, ToolCall,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::store::{LoopStore, MemoryLoopStore, PendingCall, TurnStatus};
use crate::tools::browser::{SameBrowser, ScriptedBrowser};
use crate::{CatalogCache, GuardQuery, LimitHit, LoopEngine, LoopSetup, UsageGuard};

const SNAPSHOT: &str = "## Pages\n1: Jobs (https://jobs.example/) [selected]\n## Latest page snapshot\nuid=1_1 RootWebArea \"Jobs\"\n  uid=1_2 button \"Search\"";

fn free_model(id: &str) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        name: id.into(),
        context_length: 128_000,
        max_completion: None,
        prompt_usd_per_token: 0.0,
        completion_usd_per_token: 0.0,
        tools: true,
        images_in: false,
        text_io: true,
        free: true,
        supported_parameters: ["tools", "tool_choice", "parallel_tool_calls", "max_tokens"]
            .map(str::to_owned)
            .to_vec(),
    }
}

type GateFn = dyn Fn(&str, &Value) -> Option<Decision> + Send + Sync;
type HostFn = dyn Fn(&str, &Value) -> ToolReply + Send + Sync;

/// What the stand-in hub does with the loop's requests.
#[derive(Clone)]
struct HubScript {
    gate: Arc<GateFn>,
    host: Arc<HostFn>,
}

impl Default for HubScript {
    fn default() -> Self {
        Self {
            gate: Arc::new(|_, _| Some(Decision::Allow)),
            host: Arc::new(|name, _| ToolReply {
                ok: true,
                text: format!("{name} done"),
            }),
        }
    }
}

/// What the hub saw, in order (permission and host requests by tool name).
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Started(String),
    Assistant(String, Vec<String>),
    ToolResult(String),
    Permission(String),
    HostTool(String),
    Done(bool, String),
}

struct Harness {
    _tmp: tempfile::TempDir,
    work: PathBuf,
    model: Arc<ScriptedModel>,
    store: Arc<MemoryLoopStore>,
    browser: Arc<ScriptedBrowser>,
    engine: LoopEngine,
}

impl Harness {
    fn new(model: ScriptedModel) -> Self {
        Self::with(model, |_| {})
    }

    fn with(model: ScriptedModel, tweak: impl FnOnce(&mut LoopSetup)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("bot/work");
        std::fs::create_dir_all(&work).unwrap();
        let model = Arc::new(model);
        let store = Arc::new(MemoryLoopStore::new());
        let browser = ScriptedBrowser::with_snapshot(SNAPSHOT);
        let mut setup = LoopSetup::new(model.clone(), store.clone());
        setup.catalog = Arc::new(CatalogCache::new(Catalog {
            models: vec![free_model("a:free"), free_model("b:free")],
        }));
        setup.browser = Some(Arc::new(SameBrowser(browser.clone())));
        setup.browser_allow = ["take_snapshot", "click", "navigate_page"]
            .map(str::to_owned)
            .to_vec();
        tweak(&mut setup);
        Self {
            _tmp: tmp,
            work,
            model,
            store,
            browser,
            engine: LoopEngine::new(setup),
        }
    }

    fn spec(&self, resume: Resume) -> SessionSpec {
        SessionSpec {
            bot: "scout".into(),
            work_dir: self.work.clone(),
            context_dir: self.work.join("../context"),
            knowledge_dir: None,
            system_prompt: "You are Scout.".into(),
            resume,
            model: ModelChoice::Auto,
            browser: Some(BrowserSpec {
                mcp: McpCommand::default(),
                mcp_json: json!({}),
            }),
            host_tools: HostTools {
                central: true,
                browser: true,
            },
            host_tool_specs: vec![oxbot_engine::ToolSpec {
                name: "mcp__oxbot__give_task".into(),
                description: "Give a bot a task.".into(),
                parameters: json!({"type": "object", "properties": {}, "required": ["bot", "task"]}),
            }],
            privacy: Privacy::Standard,
            gate_mode: GateMode::Live,
            env: Vec::new(),
            tool_wait: Duration::from_mins(1),
        }
    }

    /// Runs one turn on a new session and returns what the hub saw.
    async fn turn(&self, spec: SessionSpec, text: &str, hub: HubScript) -> Vec<Seen> {
        let started = self.engine.start(spec).await.unwrap();
        let mut session = started.session;
        session
            .send(TurnInput {
                text: text.into(),
                user_seq: 7,
                person: Some("p1".into()),
                origin: json!({"person": "p1"}),
            })
            .await
            .unwrap();
        let seen = pump(started.events, hub).await;
        session.shutdown().await;
        seen
    }
}

/// Answers the loop's requests like the hub until the turn is done.
async fn pump(mut events: mpsc::Receiver<EngineEvent>, hub: HubScript) -> Vec<Seen> {
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        match event {
            EngineEvent::Started { model } => seen.push(Seen::Started(model)),
            EngineEvent::Assistant { text, tool_uses } => seen.push(Seen::Assistant(
                text,
                tool_uses.into_iter().map(|t| t.name).collect(),
            )),
            EngineEvent::ToolResult { text, .. } => seen.push(Seen::ToolResult(text)),
            EngineEvent::Permission(req) => {
                seen.push(Seen::Permission(req.tool.clone()));
                if let Some(d) = (hub.gate)(&req.tool, &req.input) {
                    req.reply.send(d);
                } else {
                    // Never answered: the turn waits (until stopped).
                    std::mem::forget(req);
                }
            }
            EngineEvent::HostTool(call) => {
                seen.push(Seen::HostTool(call.name.clone()));
                call.reply.send((hub.host)(&call.name, &call.args));
            }
            EngineEvent::TurnDone { ok, text, .. } => {
                seen.push(Seen::Done(ok, text));
                break;
            }
            EngineEvent::TextDelta(_)
            | EngineEvent::Limits(_)
            | EngineEvent::ModelCall(_)
            | EngineEvent::Activity(_) => {}
        }
    }
    seen
}

fn new_session() -> Resume {
    Resume::New("s1".into())
}

#[tokio::test]
async fn a_plain_answer() {
    let h = Harness::new(ScriptedModel::new().then(Step::text("Hello!")));
    let seen = h
        .turn(h.spec(new_session()), "[Sam] hi", HubScript::default())
        .await;
    assert_eq!(
        seen,
        [
            Seen::Started("a:free".into()),
            Seen::Assistant("Hello!".into(), vec![]),
            Seen::Done(true, "Hello!".into()),
        ]
    );
    let turn = h.store.turn("s1", 0).unwrap();
    assert_eq!(turn.status, TurnStatus::Done);
    assert_eq!(turn.step, 1);
    assert_eq!(turn.question, "[Sam] hi");
    let calls = h.store.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].outcome, "ok");
    assert_eq!(calls[0].person.as_deref(), Some("p1"));
    assert_eq!(calls[0].bot, "scout");
    // Shared bots ask only for providers that honor the tools.
    let req = &h.model.requests()[0];
    assert_eq!(req.provider.as_ref().unwrap().data_collection, None);
    assert_eq!(req.fallbacks, ["b:free"]);
    assert_eq!(
        req.supported.as_deref().unwrap(),
        ["tools", "tool_choice", "parallel_tool_calls", "max_tokens"],
        "only what the models take, so require_parameters keeps them"
    );
    assert_eq!(req.messages[0].role, Role::System);
    let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "mcp__oxbot__give_task",
            "mcp__chrome-devtools__click",
            "mcp__chrome-devtools__navigate_page",
            "mcp__chrome-devtools__take_snapshot",
            "Read",
            "Write",
            "Edit",
            "Glob",
            "Grep",
            "mcp__oxbot__ask_user"
        ]
    );
}

#[tokio::test]
async fn browsing_runs_each_tool_after_the_gate() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::tool("mcp__chrome-devtools__take_snapshot", json!({})))
            .then(Step::tool(
                "mcp__chrome-devtools__click",
                json!({"uid": "1_2"}),
            ))
            .then(Step::text("Searched the jobs.")),
    );
    let seen = h
        .turn(h.spec(new_session()), "search jobs", HubScript::default())
        .await;
    assert_eq!(
        seen,
        [
            Seen::Started("a:free".into()),
            Seen::Assistant(
                String::new(),
                vec!["mcp__chrome-devtools__take_snapshot".into()]
            ),
            Seen::Permission("mcp__chrome-devtools__take_snapshot".into()),
            Seen::ToolResult(SNAPSHOT.into()),
            Seen::Assistant(String::new(), vec!["mcp__chrome-devtools__click".into()]),
            Seen::Permission("mcp__chrome-devtools__click".into()),
            Seen::ToolResult("Done: click".into()),
            Seen::Assistant("Searched the jobs.".into(), vec![]),
            Seen::Done(true, "Searched the jobs.".into()),
        ]
    );
    let calls: Vec<String> = h.browser.calls().into_iter().map(|c| c.0).collect();
    assert_eq!(calls, ["take_snapshot", "click"]);
    // The model got the page as data.
    let req = &h.model.requests()[1];
    let last = req.messages.last().unwrap();
    assert_eq!(last.role, Role::Tool);
    assert!(
        last.text()
            .starts_with("<web_page url=\"https://jobs.example/\">## Pages"),
        "{}",
        last.text()
    );
    let turn = h.store.turn("s1", 0).unwrap();
    assert_eq!((turn.step, turn.pending), (3, None));
    // Outside text is marked in the saved conversation.
    let msgs = h.store.messages("s1").unwrap();
    assert!(msgs[2].untrusted && msgs[2].message.role == Role::Tool);
    assert!(!msgs[0].untrusted);
}

#[tokio::test]
async fn a_refusal_reaches_the_model_and_skips_the_rest() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::tools(vec![
                ToolCall::new("c1", "mcp__chrome-devtools__click", json!({"uid": "1_2"})),
                ToolCall::new("c2", "mcp__chrome-devtools__take_snapshot", json!({})),
            ]))
            .then(Step::text("Okay, I didn't post it.")),
    );
    let hub = HubScript {
        gate: Arc::new(|tool, _| {
            Some(if tool.ends_with("click") {
                Decision::Deny("The user said no.".into())
            } else {
                Decision::Allow
            })
        }),
        ..HubScript::default()
    };
    let seen = h.turn(h.spec(new_session()), "post it", hub).await;
    assert!(seen.contains(&Seen::ToolResult(
        "[oxbot said no] The user said no.".into()
    )));
    assert!(seen.contains(&Seen::ToolResult(crate::runner::SKIPPED.into())));
    assert_eq!(
        seen.iter()
            .filter(|s| matches!(s, Seen::Permission(_)))
            .count(),
        1
    );
    assert_eq!(h.browser.calls(), Vec::new(), "nothing ran");
    let req = &h.model.requests()[1];
    let n = req.messages.len();
    assert_eq!(
        req.messages[n - 2].text(),
        "[oxbot said no] The user said no."
    );
}

#[tokio::test]
async fn a_practice_run_records_instead_of_running() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::tool(
                "mcp__chrome-devtools__click",
                json!({"uid": "1_2"}),
            ))
            .then(Step::text("Done.")),
    );
    let hub = HubScript {
        gate: Arc::new(|_, _| {
            Some(Decision::Simulate(
                "(practice run) would click button \"Search\"".into(),
            ))
        }),
        ..HubScript::default()
    };
    let mut spec = h.spec(new_session());
    spec.gate_mode = GateMode::DryRun;
    let seen = h.turn(spec, "search", hub).await;
    assert!(seen.contains(&Seen::ToolResult(
        "(practice run) would click button \"Search\"".into()
    )));
    assert_eq!(h.browser.calls(), Vec::new());
    assert!(h.store.turn("s1", 0).unwrap().dry_run);
}

#[tokio::test]
async fn broken_calls_are_shown_back_then_stop() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::broken_tool("mcp__chrome-devtools__click", "{uid: "))
            .then(Step::tool("Bash", json!({"command": "ls"})))
            .then(Step::tool(
                "mcp__chrome-devtools__evaluate_script",
                json!({}),
            )),
    );
    let seen = h
        .turn(h.spec(new_session()), "go", HubScript::default())
        .await;
    assert!(
        !seen.iter().any(|s| matches!(s, Seen::Permission(_))),
        "the gate never saw them: {seen:?}"
    );
    assert_eq!(
        seen.last().unwrap(),
        &Seen::Done(
            false,
            "The model kept making tool calls oxbot couldn't run, so the task stopped.".into()
        )
    );
    let req = &h.model.requests()[1];
    let last = req.messages.last().unwrap().text();
    assert!(
        last.starts_with("[tool error] Your call to mcp__chrome-devtools__click couldn't be read"),
        "{last}"
    );
}

#[tokio::test]
async fn ask_user_ends_the_turn_with_the_question() {
    let h = Harness::new(ScriptedModel::new().then(Step::tool(
        "mcp__oxbot__ask_user",
        json!({"question": "Which city?"}),
    )));
    let seen = h
        .turn(h.spec(new_session()), "find jobs", HubScript::default())
        .await;
    assert_eq!(
        &seen[seen.len() - 2..],
        [
            Seen::Assistant("Which city?".into(), vec![]),
            Seen::Done(true, "Which city?".into())
        ]
    );
    assert!(seen.contains(&Seen::Permission("mcp__oxbot__ask_user".into())));
    assert_eq!(
        h.store.turn("s1", 0).unwrap().status,
        TurnStatus::WaitingUser
    );
}

#[tokio::test]
async fn host_tools_run_in_the_hub() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::tool(
                "mcp__oxbot__give_task",
                json!({"bot": "news", "task": "x"}),
            ))
            .then(Step::text("Asked News.")),
    );
    let seen = h
        .turn(h.spec(new_session()), "ask news", HubScript::default())
        .await;
    assert!(seen.contains(&Seen::HostTool("give_task".into())));
    assert!(seen.contains(&Seen::ToolResult("give_task done".into())));
}

#[tokio::test]
async fn failed_host_tools_are_marked_as_errors() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::tool(
                "mcp__oxbot__give_task",
                json!({"bot": "nobody", "task": "x"}),
            ))
            .then(Step::text("It didn't work.")),
    );
    let hub = HubScript {
        host: Arc::new(|_, _| ToolReply {
            ok: false,
            text: "There is no bot called nobody.".into(),
        }),
        ..HubScript::default()
    };
    let seen = h.turn(h.spec(new_session()), "ask nobody", hub).await;
    assert!(seen.contains(&Seen::ToolResult(
        "[tool error] There is no bot called nobody.".into()
    )));
}

#[tokio::test]
async fn stopping_while_the_gate_waits() {
    let h = Harness::new(ScriptedModel::new().then(Step::tool(
        "mcp__chrome-devtools__click",
        json!({"uid": "1_2"}),
    )));
    let started = h.engine.start(h.spec(new_session())).await.unwrap();
    let mut session = started.session;
    session
        .send(TurnInput {
            text: "go".into(),
            user_seq: 1,
            person: None,
            origin: json!({}),
        })
        .await
        .unwrap();
    let hub = HubScript {
        gate: Arc::new(|_, _| None),
        ..HubScript::default()
    };
    let pumping = tokio::spawn(pump(started.events, hub));
    tokio::time::sleep(Duration::from_millis(50)).await;
    session.interrupt().await.unwrap();
    let seen = pumping.await.unwrap();
    assert_eq!(seen.last().unwrap(), &Seen::Done(false, "Stopped.".into()));
    assert_eq!(h.browser.calls(), Vec::new());
    assert_eq!(h.store.turn("s1", 0).unwrap().status, TurnStatus::Stopped);
}

#[tokio::test(start_paused = true)]
async fn a_busy_model_cools_down_and_the_next_answers() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::Fail(LlmError {
                retry_after: Some(Duration::from_secs(60)),
                ..LlmError::new(ErrorKind::RateLimited, "slow down")
            }))
            .then(Step::text("Hi.").from_model("b:free")),
    );
    let seen = h
        .turn(h.spec(new_session()), "hi", HubScript::default())
        .await;
    assert_eq!(seen.last().unwrap(), &Seen::Done(true, "Hi.".into()));
    let reqs = h.model.requests();
    assert_eq!(reqs[0].model, "a:free");
    assert_eq!(reqs[1].model, "b:free");
    let outcomes: Vec<String> = h.store.calls().into_iter().map(|c| c.outcome).collect();
    assert_eq!(outcomes, ["rate_limited", "ok"]);
}

#[tokio::test(start_paused = true)]
async fn private_bots_never_drop_deny() {
    let no_provider = || {
        Step::error(
            ErrorKind::NoProvider,
            "No endpoints found matching your data policy",
        )
    };
    let h = Harness::new(ScriptedModel::new().then(no_provider()).then(no_provider()));
    let mut spec = h.spec(new_session());
    spec.privacy = Privacy::Private;
    let seen = h.turn(spec, "read my mail", HubScript::default()).await;
    let reqs = h.model.requests();
    for r in &reqs {
        let p = r.provider.as_ref().unwrap();
        assert_eq!(p.data_collection, Some(DataCollection::Deny));
    }
    // First with zero retention (both models, one on OpenRouter's side),
    // then once more without it.
    let zdr: Vec<Option<bool>> = reqs
        .iter()
        .map(|r| r.provider.as_ref().unwrap().zdr)
        .collect();
    assert_eq!(zdr, [Some(true), None]);
    let Seen::Done(false, text) = seen.last().unwrap() else {
        panic!("{seen:?}")
    };
    assert!(
        text.starts_with("No free model takes private work"),
        "{text}"
    );
    assert!(h.store.calls().iter().all(|c| c.private));
}

#[derive(Debug)]
struct TwoPerBot(Mutex<u32>);

impl UsageGuard for TwoPerBot {
    fn check(&self, q: &GuardQuery<'_>) -> Result<(), LimitHit> {
        let mut n = self.0.lock().unwrap();
        *n += 1;
        if *n > 2 {
            return Err(LimitHit(format!(
                "{} has used its 2 requests for today.",
                q.bot
            )));
        }
        Ok(())
    }
}

#[tokio::test]
async fn limits_stop_a_turn() {
    let h = Harness::with(
        ScriptedModel::new()
            .then(Step::tool("mcp__chrome-devtools__take_snapshot", json!({})))
            .then(Step::tool("mcp__chrome-devtools__take_snapshot", json!({}))),
        |s| s.guard = Arc::new(TwoPerBot(Mutex::new(0))),
    );
    let seen = h
        .turn(h.spec(new_session()), "go", HubScript::default())
        .await;
    assert_eq!(
        seen.last().unwrap(),
        &Seen::Done(false, "scout has used its 2 requests for today.".into())
    );
    assert_eq!(h.model.requests().len(), 2);
}

#[tokio::test]
async fn a_restart_goes_on_without_repeating_a_click() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::tools(vec![ToolCall::new(
                "c1",
                "mcp__chrome-devtools__click",
                json!({"uid": "1_2"}),
            )]))
            .then(Step::text("Posted.")),
    );
    h.turn(h.spec(new_session()), "post it", HubScript::default())
        .await;
    assert_eq!(h.browser.calls().len(), 1);
    // As if oxbot stopped while the click ran: its result and the answer
    // after it were never saved.
    h.store.drop_last("s1", 2);
    h.store.set_status("s1", 0, TurnStatus::Running);
    h.store.set_pending(
        "s1",
        0,
        Some(PendingCall {
            id: "c1".into(),
            name: "mcp__chrome-devtools__click".into(),
            args: "{}".into(),
            side_effect: true,
        }),
    );
    h.model.push(Step::text("I may have posted it; checking."));
    let started = h
        .engine
        .start(h.spec(Resume::Resume("s1".into())))
        .await
        .unwrap();
    let resumed = started.resumed_turn.clone().unwrap();
    assert_eq!(resumed.user_seq, 7);
    assert_eq!(resumed.question, "post it");
    let mut session = started.session;
    session
        .send(TurnInput {
            text: resumed.question.clone(),
            user_seq: resumed.user_seq,
            person: None,
            origin: resumed.origin,
        })
        .await
        .unwrap();
    let seen = pump(started.events, HubScript::default()).await;
    session.shutdown().await;
    assert_eq!(h.browser.calls().len(), 1, "the click didn't run again");
    assert!(seen.contains(&Seen::ToolResult(crate::runner::RESTARTED_MID_CALL.into())));
    assert_eq!(
        seen.last().unwrap(),
        &Seen::Done(true, "I may have posted it; checking.".into())
    );
    let turn = h.store.turn("s1", 0).unwrap();
    assert_eq!((turn.status, turn.resumes), (TurnStatus::Done, 1));
    // The model saw the whole turn again, with the restart note.
    let req = h.model.requests().last().unwrap().clone();
    assert_eq!(
        req.messages.len(),
        4,
        "system, user, assistant call, result"
    );
}

#[tokio::test]
async fn a_long_conversation_drops_its_oldest_turns() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::text("one"))
            .then(Step::text("two")),
    );
    let big = "x".repeat(300_000);
    let started = h.engine.start(h.spec(new_session())).await.unwrap();
    let mut session = started.session;
    let mut events = started.events;
    for seq in [1, 2] {
        session
            .send(TurnInput {
                text: big.clone(),
                user_seq: seq,
                person: None,
                origin: json!({}),
            })
            .await
            .unwrap();
        loop {
            match events.recv().await.unwrap() {
                EngineEvent::TurnDone { ok, text, .. } => {
                    assert!(ok, "{text}");
                    break;
                }
                EngineEvent::Permission(r) => r.reply.send(Decision::Allow),
                _ => {}
            }
        }
    }
    session.shutdown().await;
    let reqs = h.model.requests();
    // The second request starts after the first turn.
    assert_eq!(reqs[1].messages.len(), 2, "system and the new message");
    assert_eq!(h.store.context("s1").unwrap()["from_idx"], 2);
}

#[tokio::test]
async fn not_signed_in_is_not_ready() {
    let key = oxbot_llm::KeyCell::new();
    let h = Harness::with(ScriptedModel::new(), |s| s.key = Some(key.clone()));
    assert!(h.engine.ready().is_err());
    assert!(h.engine.start(h.spec(new_session())).await.is_err());
    key.set(oxbot_llm::ApiKey::new("sk-or-v1-test".into()).unwrap());
    assert!(h.engine.ready().is_ok());
}

#[tokio::test]
async fn one_shot_uses_the_same_privacy() {
    let h = Harness::new(ScriptedModel::new().then(Step::text("A lesson.")));
    let text = h
        .engine
        .one_shot(crate::OneShot {
            bot: "scout".into(),
            person: None,
            privacy: Privacy::Private,
            kind: oxbot_engine::CallKind::Reflection,
            model: ModelChoice::Auto,
            messages: vec![oxbot_llm::Message::user("reflect")],
            max_tokens: 500,
        })
        .await
        .unwrap();
    assert_eq!(text, "A lesson.");
    let req = &h.model.requests()[0];
    assert!(req.provider.as_ref().unwrap().denies_data_collection());
    assert_eq!(req.max_tokens, Some(500));
    assert_eq!(req.tools, Vec::new());
    assert_eq!(h.store.calls()[0].kind, oxbot_engine::CallKind::Reflection);
}
