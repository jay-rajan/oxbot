//! Comparing engines and models (#21): a finished turn becomes a test case,
//! and replays of it on Claude (the fake `claude` that asks oxbot first)
//! and on oxbot's own loop (a scripted model and browser) record their
//! steps instead of doing them, raise no card, leave the chat alone, and
//! show up on the scoreboard. Everything is the owner's only.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_browser::{Tools, ToolsConfig};
use oxbot_core::engine::EngineKind;
use oxbot_core::{Bot, BotId, Hub, HubConfig, LoopParts, Store, Viewer};
use oxbot_llm::{ScriptedModel, Step};
use oxbot_loop::tools::browser::{SameBrowser, ScriptedBrowser};
use oxbot_protocol::{
    Answer, EngineKind as Wire, EventKind, RunVerdict, ServerMsg, Target, TrialNetwork,
};
use serde_json::json;

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

/// The loop's model: read the page, click Post, answer; once per run in
/// `checked`, with the answer given twice when the run's answer is
/// checked (a practice run's click ran; a refused one ends as said).
fn poster(checked: &[bool]) -> Arc<ScriptedModel> {
    let mut model = ScriptedModel::new();
    for &check in checked {
        model = model
            .then(Step::tool("mcp__chrome-devtools__take_snapshot", json!({})))
            .then(Step::tool(
                "mcp__chrome-devtools__click",
                json!({"uid": "1_3"}),
            ))
            .then(Step::text("Posted."));
        if check {
            model = model.then(Step::text("Posted."));
        }
    }
    Arc::new(model)
}

/// A hub with bot `jobs` on `engine` (Claude: the fake asks to click
/// Post), and the loop's scripted model clicking Post too.
fn hub(dir: &Path, engine: EngineKind) -> Arc<Hub> {
    let bots = dir.join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let mut bot = Bot::create(&bots, BotId::new("jobs").unwrap(), "Jobs", "Post jobs.").unwrap();
    bot.engine = engine;
    bot.save().unwrap();
    let config = HubConfig {
        claude_program: claude_tests().join("fake-claude-perm.sh"),
        browser: Some(tools(dir)),
        approval_timeout: Duration::from_secs(5),
        extra_env: vec![("FAKE_TOOL".to_owned(), "post".to_owned())],
        openrouter: Some(LoopParts {
            browser: Some(Arc::new(SameBrowser(ScriptedBrowser::with_snapshot(
                SNAPSHOT,
            )))),
            free_per_minute: 1000,
            // On the loop, the live turn is refused (Sam says no); every
            // practice run's answer is checked.
            ..LoopParts::new(
                poster(if engine == EngineKind::OpenRouter {
                    &[false, true, true]
                } else {
                    &[true, true, true]
                }),
                None,
                None,
            )
        }),
        pre_engine_tests: false,
        trials_without_guard: true,
        data_dir: dir.join("data"),
        ..HubConfig::for_app(&bots)
    };
    let hub = Hub::new(vec![bot], Store::open_in_memory().unwrap(), config);
    let catalog = json!({"models": [{"id": "m:free", "name": "M", "context_length": 128_000,
        "max_completion": null, "prompt_usd_per_token": 0.0, "completion_usd_per_token": 0.0,
        "tools": true, "images_in": false, "text_io": true, "free": true}]});
    hub.set_model_catalog(&catalog.to_string(), 1).unwrap();
    hub
}

/// Sends `text` to `jobs` as `viewer`, answers its card `answer`, and
/// returns the turn's `TurnDone` event number.
async fn live_turn(hub: &Arc<Hub>, viewer: &Viewer, answer: Answer) -> u64 {
    let mut updates = hub.subscribe();
    hub.send_as("jobs", "post it", viewer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match updates.recv().await.unwrap() {
                ServerMsg::ApprovalNeeded(a) => {
                    assert!(hub.answer_as(a.id, answer, viewer).unwrap());
                }
                ServerMsg::Event(e) if e.bot == "jobs" => {
                    if let EventKind::TurnDone { .. } = e.kind {
                        return e.seq;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}

/// Runs a batch and returns what the owner was told meanwhile (no card may
/// show), once it's done.
async fn run_batch(
    hub: &Arc<Hub>,
    owner: &Viewer,
    case: &str,
    targets: Vec<Target>,
) -> Vec<ServerMsg> {
    let mut updates = hub.subscribe();
    hub.start_batch("jobs", &[case.to_owned()], &targets, 1, owner)
        .unwrap();
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let msg = updates.recv().await.unwrap();
            let done = matches!(&msg, ServerMsg::BatchProgress { status, .. } if status == "done");
            seen.push(msg);
            if done {
                return;
            }
        }
    })
    .await
    .unwrap();
    seen
}

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one story, start to scoreboard")]
async fn a_replay_on_both_engines_records_the_post_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub(tmp.path(), EngineKind::Claude);
    let owner = hub.farm_viewer("Jay");
    // "Always allow": a replay records the post all the same.
    let done = live_turn(&hub, &owner, Answer::AlwaysAllow).await;

    let draft = hub.draft_test_case("jobs", done, &owner).unwrap();
    assert_eq!(draft.title, "post it");
    assert_eq!(draft.kind, "acts");
    assert_eq!(
        draft.rubric,
        "Would click button \"Post\" on jobs.example.com"
    );
    let case = hub
        .mark_test_case(
            "jobs",
            done,
            &draft.title,
            &draft.rubric,
            TrialNetwork::Strict,
            None,
            &owner,
        )
        .await
        .unwrap();
    assert!(case.owner_started);

    let chat_before = hub.history("jobs", 0).unwrap().len();
    let seen = run_batch(
        &hub,
        &owner,
        &case.id,
        vec![
            Target {
                engine: Wire::Claude,
                model: "sonnet".into(),
            },
            Target {
                engine: Wire::OpenRouter,
                model: "m:free".into(),
            },
        ],
    )
    .await;
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMsg::ApprovalNeeded(_))),
        "a replay never shows a card"
    );
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMsg::Event(e) if e.bot == "jobs")),
        "nothing reaches the chat"
    );
    assert_eq!(hub.history("jobs", 0).unwrap().len(), chat_before);
    assert!(
        seen.iter()
            .any(|m| matches!(m, ServerMsg::TrialEvent { event, .. }
        if matches!(&event.kind, EventKind::DryRun { target, .. } if target == "button \"Post\"")))
    );

    let runs = hub.runs_for("jobs", Some(&case.id), &owner).unwrap();
    let replays: Vec<_> = runs.iter().filter(|r| r.kind == "replay").collect();
    assert_eq!(replays.len(), 2, "{runs:#?}");
    for run in &replays {
        assert_eq!(run.status, "done", "{run:#?}");
        assert_eq!(run.intents, 1, "{run:#?}");
        assert_eq!(run.intent_list[0].target, "button \"Post\"");
        assert!(
            run.intent_list[0].by_rule,
            "a rule would have let it run live"
        );
        let m = run.intent_match.as_ref().unwrap();
        assert_eq!(m.matched, ["click button \"Post\" on jobs.example.com"]);
        assert!(m.missing.is_empty() && m.should_not.is_empty());
        assert!(run.steps >= 2);
    }
    let on_loop = replays
        .iter()
        .find(|r| r.engine == Wire::OpenRouter)
        .unwrap();
    assert_eq!(
        on_loop.requests, 4,
        "three model requests and the answer check: {on_loop:#?}"
    );
    assert_eq!(on_loop.answer, "Posted.");
    let on_claude = replays.iter().find(|r| r.engine == Wire::Claude).unwrap();
    assert!(on_claude.input_tokens > 0, "Claude's usage goes to the run");
    // The run's own folder and key are gone.
    assert_eq!(
        std::fs::read_dir(tmp.path().join("data/trials")).map_or(0, Iterator::count),
        0
    );
    let usage = hub.usage().unwrap();
    assert!(
        usage.bots.iter().all(|b| !b.bot.starts_with("t--")),
        "{usage:?}"
    );

    // The owner marks one; the scoreboard shows both engines.
    hub.set_run_verdict(&on_loop.id, Some(RunVerdict::Pass), &owner)
        .unwrap();
    let rows = hub.scoreboard_for("jobs", &owner).unwrap();
    assert_eq!(rows[0].engine, Wire::Claude, "Claude first");
    let loop_row = rows.iter().find(|r| r.model == "m:free").unwrap();
    assert_eq!((loop_row.n, loop_row.pass_rate), (1, Some(1.0)));
    assert!(rows.iter().any(|r| r.model == "sonnet" && r.n == 1));
    assert!(
        rows.iter().any(|r| r.model == "default" && r.n == 1),
        "the original"
    );
}

#[tokio::test]
async fn only_the_owner_compares_and_claude_runs_only_the_owners_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub(tmp.path(), EngineKind::OpenRouter);
    let owner = hub.farm_viewer("Jay");
    let sam = Viewer::gate("Sam");
    let done = live_turn(&hub, &sam, Answer::Deny).await;

    assert!(hub.draft_test_case("jobs", done, &sam).is_err());
    assert!(hub.test_cases_for("jobs", &sam).is_err());
    assert!(hub.scoreboard_for("jobs", &sam).is_err());
    assert!(hub.rate_turn("jobs", done, Some(true), &sam).is_err());

    let case = hub
        .mark_test_case("jobs", done, "", "", TrialNetwork::Strict, None, &owner)
        .await
        .unwrap();
    assert!(!case.owner_started);
    let estimate = hub
        .estimate_batch(
            "jobs",
            std::slice::from_ref(&case.id),
            &[Target {
                engine: Wire::Claude,
                model: "sonnet".into(),
            }],
            1,
            &owner,
        )
        .unwrap();
    assert_eq!(estimate.runs, 0);
    assert!(
        estimate.refused[0].contains("someone else started it"),
        "{estimate:?}"
    );
    assert!(
        hub.start_batch(
            "jobs",
            std::slice::from_ref(&case.id),
            &[Target {
                engine: Wire::Claude,
                model: "sonnet".into()
            }],
            1,
            &owner
        )
        .is_err()
    );

    // Sam said no to the post: a replay that posts repeats a "no".
    let seen = run_batch(
        &hub,
        &owner,
        &case.id,
        vec![Target {
            engine: Wire::OpenRouter,
            model: "m:free".into(),
        }],
    )
    .await;
    assert!(
        !seen
            .iter()
            .any(|m| matches!(m, ServerMsg::ApprovalNeeded(_)))
    );
    let run = hub
        .runs_for("jobs", Some(&case.id), &owner)
        .unwrap()
        .into_iter()
        .find(|r| r.kind == "replay")
        .unwrap();
    let m = run.intent_match.unwrap();
    assert_eq!(m.should_not, ["click button \"Post\" on jobs.example.com"]);
    assert!(
        hub.judge_runs(std::slice::from_ref(&run.id), false, &owner)
            .is_err(),
        "not on Claude"
    );

    // Thumbs: the owner's, by engine and model.
    hub.rate_turn("jobs", done, Some(false), &owner).unwrap();
    let rows = hub.scoreboard_for("jobs", &owner).unwrap();
    assert!(rows.iter().any(|r| r.thumbs_down == 1), "{rows:?}");
}

#[test]
fn a_bot_folder_shaped_like_a_practice_run_is_never_loaded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("t--0123456789abcdef");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bot.toml"), "name = \"Sneaky\"\n").unwrap();
    assert!(Bot::load(&dir).is_err());
    assert_eq!(oxbot_core::bot::load_all(tmp.path()).unwrap().len(), 0);
}
