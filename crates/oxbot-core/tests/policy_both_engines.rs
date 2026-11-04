//! The same policy for both engines: each case runs on Claude (the fake
//! `claude` that asks oxbot first) and on oxbot's own loop (a scripted
//! model and browser), and must raise the same cards and refusals.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_browser::{Tools, ToolsConfig};
use oxbot_core::engine::EngineKind;
use oxbot_core::{Bot, BotId, Hub, HubConfig, LoopParts, Store, Viewer};
use oxbot_llm::{ScriptedModel, Step};
use oxbot_loop::tools::browser::{SameBrowser, ScriptedBrowser};
use oxbot_protocol::{Answer, EventKind, ServerMsg};
use serde_json::{Value, json};

/// The page both engines read (the fake `claude`'s snapshot).
const SNAPSHOT: &str = "## Pages\n1: Jobs (https://jobs.example.com/apply) [selected]\n## Latest page snapshot\nuid=1_1 RootWebArea \"Jobs\"\n  uid=1_2 button \"Upload CV\"\n  uid=1_3 button \"Post\"\n  uid=1_4 textbox \"Password\"";

fn claude_tests() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../oxbot-claude/tests")
}

fn tools(dir: &Path) -> Arc<Tools> {
    let mut tools = ToolsConfig::in_home(&dir.join("home"));
    tools.chrome = PathBuf::from("/no/chrome");
    let package = tools.tools_dir.join("node_modules/chrome-devtools-mcp");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("package.json"), r#"{"version":"1.10.1"}"#).unwrap();
    std::fs::create_dir_all(tools.tools_dir.join("node_modules/.bin")).unwrap();
    std::fs::write(tools.mcp_program(), "").unwrap();
    Tools::new(tools)
}

/// A hub whose bot `id` runs on `engine`: Claude by the fake with
/// `FAKE_TOOL=tool`, or the loop on `model`.
fn hub(
    dir: &Path,
    id: &str,
    engine: EngineKind,
    tool: &str,
    model: Arc<ScriptedModel>,
) -> Arc<Hub> {
    let bots = dir.join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let mut bot = Bot::create(&bots, BotId::new(id).unwrap(), id, "").unwrap();
    bot.engine = engine;
    bot.save().unwrap();
    let browser = ScriptedBrowser::with_snapshot(SNAPSHOT);
    let config = HubConfig {
        claude_program: claude_tests().join("fake-claude-perm.sh"),
        browser: Some(tools(dir)),
        approval_timeout: Duration::from_secs(5),
        extra_env: vec![("FAKE_TOOL".to_owned(), tool.to_owned())],
        openrouter: Some(LoopParts {
            browser: Some(Arc::new(SameBrowser(browser))),
            free_per_minute: 1000,
            ..LoopParts::new(model, None, None)
        }),
        pre_engine_tests: false,
        ..HubConfig::for_app(&bots)
    };
    let hub = Hub::new(vec![bot], Store::open_in_memory().unwrap(), config);
    let catalog = json!({"models": [{"id": "m:free", "name": "M", "context_length": 128_000,
        "max_completion": null, "prompt_usd_per_token": 0.0, "completion_usd_per_token": 0.0,
        "tools": true, "images_in": false, "text_io": true, "free": true}]});
    hub.set_model_catalog(&catalog.to_string(), 1).unwrap();
    hub
}

/// What a turn showed the person: each card (kind, action, target, site)
/// and each refusal, in order. Cards are answered no.
async fn run(hub: &Arc<Hub>, bot: &str) -> Vec<String> {
    let mut updates = hub.subscribe();
    hub.send_as(bot, "go", &Viewer::farm("Jay")).await.unwrap();
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match updates.recv().await.unwrap() {
                ServerMsg::ApprovalNeeded(a) => {
                    seen.push(format!(
                        "card {:?} {} {} on {}",
                        a.kind, a.action, a.target, a.site
                    ));
                    hub.answer(a.id, Answer::Deny);
                }
                ServerMsg::Event(e) if e.bot == bot => match e.kind {
                    EventKind::Blocked { message } => seen.push(message),
                    EventKind::TurnDone { .. } => return,
                    _ => {}
                },
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    seen
}

/// The loop's model: read the page, make `call`, then answer (again after
/// the answer check, when both calls ran).
fn loop_model(name: &str, args: Value) -> Arc<ScriptedModel> {
    Arc::new(
        ScriptedModel::new()
            .then(Step::tool("mcp__chrome-devtools__take_snapshot", json!({})))
            .then(Step::tool(name, args))
            .then(Step::text("Okay."))
            .then(Step::text("Okay.")),
    )
}

/// Runs a case on both engines; returns what each showed.
/// The Farm's Herder runs on Claude, so the loop's Herder cases use a
/// person's own Herder.
async fn both(
    id: &str,
    fake_tool: &str,
    call: &str,
    args: impl Fn(&Path) -> Value,
) -> [Vec<String>; 2] {
    let tmp = tempfile::tempdir().unwrap();
    let claude = hub(
        &tmp.path().join("c"),
        id,
        EngineKind::Claude,
        fake_tool,
        Arc::new(ScriptedModel::new()),
    );
    let on_claude = run(&claude, id).await;
    let loop_id = if id == "assistant" {
        oxbot_core::bot::personal_herder_id("Jay").to_string()
    } else {
        id.to_owned()
    };
    let dir = tmp.path().join("l");
    let work = dir.join("bots").join(&loop_id).join("work");
    let looped = hub(
        &dir,
        &loop_id,
        EngineKind::OpenRouter,
        "none",
        loop_model(call, args(&work)),
    );
    let on_loop = run(&looped, &loop_id).await;
    [on_claude, on_loop]
}

#[tokio::test]
async fn post_asks_with_the_same_card() {
    let [claude, looped] = both(
        "poster",
        "post",
        "mcp__chrome-devtools__click",
        |_| json!({"uid": "1_3"}),
    )
    .await;
    assert_eq!(claude, looped);
    assert_eq!(claude.len(), 1);
    assert!(claude[0].starts_with("card Change click"), "{claude:?}");
}

#[tokio::test]
async fn upload_asks_with_the_same_card() {
    let [claude, looped] = both(
        "poster",
        "upload",
        "mcp__chrome-devtools__upload_file",
        |w| json!({"uid": "1_2", "filePaths": [w.join("cv.pdf").display().to_string()]}),
    )
    .await;
    assert_eq!(claude, looped);
    assert!(claude[0].starts_with("card Upload"), "{claude:?}");
}

#[tokio::test]
async fn create_bot_asks_with_the_same_card() {
    let [claude, looped] = both(
        "assistant",
        "createbot",
        "mcp__oxbot__create_bot",
        |_| json!({"name": "Mail Checker", "instructions": "Check my mail."}),
    )
    .await;
    assert_eq!(claude, looped);
    assert!(claude[0].starts_with("card CreateBot"), "{claude:?}");
}

#[tokio::test]
async fn reading_and_listing_run_without_asking() {
    let [claude, looped] = both("assistant", "list", "mcp__oxbot__list_bots", |_| json!({})).await;
    assert_eq!(claude, looped);
    assert_eq!(claude, Vec::<String>::new());
}

/// The loop alone: refusals the fake `claude` can't make, checked
/// against the policy's own words.
async fn on_loop(call: &str, args: Value) -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub(
        tmp.path(),
        "poster",
        EngineKind::OpenRouter,
        "none",
        loop_model(call, args),
    );
    run(&hub, "poster").await
}

#[tokio::test]
async fn passwords_files_outside_and_local_pages_are_refused() {
    let password = on_loop(
        "mcp__chrome-devtools__fill",
        json!({"uid": "1_4", "value": "x"}),
    )
    .await;
    assert_eq!(password.len(), 1);
    assert!(
        password[0].starts_with("oxbot refused: oxbot never lets bots type passwords"),
        "{password:?}"
    );
    let outside = on_loop("Read", json!({"file_path": "/etc/hosts"})).await;
    assert_eq!(
        outside,
        [
            "oxbot refused: oxbot doesn't allow Read here: bots may only use files in their own folder."
        ]
    );
    let local = on_loop(
        "mcp__chrome-devtools__navigate_page",
        json!({"type": "url", "url": "file:///etc/hosts"}),
    )
    .await;
    assert_eq!(local.len(), 1);
    assert!(local[0].starts_with("oxbot refused: "), "{local:?}");
    // Inside its own folder a file tool runs without asking.
    let inside = on_loop("Write", json!({"file_path": "notes.md", "content": "x"})).await;
    assert_eq!(inside, Vec::<String>::new());
}
