//! Comparing models (#21): a finished turn becomes a test case, and
//! replays of it on Codex (the real `codex` program; skipped, saying why,
//! on a Mac without Codex 0.148) record their steps instead of doing
//! them, raise no card, leave the chat alone, and show up on the
//! scoreboard. Everything is the owner's only. The bots are stand-ins
//! (`oxbot_core::testing`) that read the page and click Post.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{Bot, BotId, Hub, HubConfig, Store, Viewer};
use oxbot_protocol::{
    Answer, EngineKind as Wire, EventKind, RunVerdict, ServerMsg, Target, TrialNetwork,
};

/// A hub on Codex (`codex`) with bot `jobs`.
async fn hub(dir: &Path, codex: &Path) -> Arc<Hub> {
    let bots = dir.join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let bot = Bot::create(&bots, BotId::new("jobs").unwrap(), "Jobs", "Post jobs.").unwrap();
    let fake = Arc::new(FakeBots::new(&bots, Tool::Post));
    let config = HubConfig {
        trials_without_guard: true,
        ..testing::config(&bots, &dir.join("home"), &fake, Some(codex))
    };
    let hub = Hub::new(vec![bot], Store::open_in_memory().unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
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
    tokio::time::timeout(Duration::from_secs(90), async {
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

fn target(engine: Wire) -> Target {
    Target {
        engine,
        model: testing::MODEL.into(),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one story, start to scoreboard")]
async fn a_replay_records_the_post_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let hub = hub(tmp.path(), &codex).await;
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

    let chat_before = hub.history("jobs", 0).unwrap().len();
    let targets = vec![target(Wire::Codex)];
    let seen = run_batch(&hub, &owner, &case.id, targets.clone()).await;
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
    assert_eq!(replays.len(), targets.len(), "{runs:#?}");
    let run = replays[0];
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
    assert_eq!(run.provider, "openrouter");
    // The recorded click reads as done to the bot.
    assert_eq!(run.answer, "behavior=allow", "{run:#?}");
    assert!(run.requests >= 3, "{run:#?}");
    // The runs' own folders and keys are gone.
    assert_eq!(
        std::fs::read_dir(tmp.path().join("home/data/trials")).map_or(0, Iterator::count),
        0
    );
    assert!(
        hub.bots()
            .await
            .unwrap()
            .iter()
            .all(|b| !b.id.starts_with("t--"))
    );

    // The owner marks it; the scoreboard has one row for the model, with
    // the original (on the same model) and the replay.
    hub.set_run_verdict(&run.id, Some(RunVerdict::Pass), &owner)
        .unwrap();
    let rows = hub.scoreboard_for("jobs", &owner).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].engine, Wire::Codex);
    assert_eq!(
        rows[0].model,
        testing::MODEL,
        "the original's model, from the meter"
    );
    assert_eq!((rows[0].n, rows[0].pass_rate), (2, Some(1.0)));
}

#[tokio::test]
async fn only_the_owner_compares_and_a_replay_repeats_a_no() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let hub = hub(tmp.path(), &codex).await;
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
    // Sam said no to the post: a replay that posts repeats a "no".
    let seen = run_batch(&hub, &owner, &case.id, vec![target(Wire::Codex)]).await;
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

    // Thumbs: the owner's, by model.
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
