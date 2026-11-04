//! Bots on oxbot's own loop, through the hub: the meter, limits, privacy
//! for private bots, and picking a turn up after a restart. A scripted
//! model stands in for `OpenRouter`.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::engine::EngineKind;
use oxbot_core::{Bot, BotId, Hub, HubConfig, LoopParts, Store, Viewer};
use oxbot_llm::{DataCollection, ScriptedModel, Step};
use oxbot_protocol::{EventKind, ServerMsg};
use serde_json::json;
use tokio::sync::broadcast::Receiver;

fn catalog() -> String {
    json!({"models": [{"id": "m:free", "name": "M", "context_length": 128_000,
        "max_completion": null, "prompt_usd_per_token": 0.0, "completion_usd_per_token": 0.0,
        "tools": true, "images_in": false, "text_io": true, "free": true}]})
    .to_string()
}

/// A hub in `dir` whose bots `ids` run on the loop with `model`.
fn hub(dir: &Path, ids: &[&str], model: &Arc<ScriptedModel>) -> Arc<Hub> {
    let bots = dir.join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let all = ids
        .iter()
        .map(|id| {
            let mut bot = Bot::load(&bots.join(id))
                .unwrap_or_else(|_| Bot::create(&bots, BotId::new(*id).unwrap(), id, "").unwrap());
            bot.engine = EngineKind::OpenRouter;
            bot.save().unwrap();
            bot
        })
        .collect();
    let config = HubConfig {
        openrouter: Some(LoopParts {
            free_per_minute: 1000,
            ..LoopParts::new(Arc::clone(model) as _, None, None)
        }),
        pre_engine_tests: false,
        ..HubConfig::for_app(&bots)
    };
    let hub = Hub::new(all, Store::open(&dir.join("oxbot.db")).unwrap(), config);
    hub.set_model_catalog(&catalog(), 1).unwrap();
    hub
}

/// Waits for `bot`'s turn to end; returns (its reply or error, the meter
/// message if one came).
async fn done(updates: &mut Receiver<ServerMsg>, bot: &str) -> (String, Option<(u32, String)>) {
    let mut last = String::new();
    let mut meter = None;
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match updates.recv().await.unwrap() {
                ServerMsg::Event(e) if e.bot == bot => match e.kind {
                    EventKind::AssistantMessage { text } | EventKind::Error { message: text } => {
                        last = text;
                    }
                    EventKind::TurnDone { .. } => {
                        // The meter comes right after.
                        if let Ok(Ok(ServerMsg::Meter {
                            requests, model, ..
                        })) = tokio::time::timeout(Duration::from_secs(1), updates.recv()).await
                        {
                            meter = Some((requests, model));
                        }
                        return;
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    (last, meter)
}

#[tokio::test]
async fn a_loop_turn_is_metered() {
    let tmp = tempfile::tempdir().unwrap();
    let model = Arc::new(
        ScriptedModel::new()
            .then(Step::tool(
                "Write",
                json!({"file_path": "a.md", "content": "hi"}),
            ))
            .then(Step::text("Saved it.")),
    );
    let hub = hub(tmp.path(), &["scout"], &model);
    let mut updates = hub.subscribe();
    hub.send_as("scout", "save a note", &Viewer::farm("Jay"))
        .await
        .unwrap();
    let (reply, meter) = done(&mut updates, "scout").await;
    assert_eq!(reply, "Saved it.");
    assert_eq!(meter, Some((2, "m:free".to_owned())));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("bots/scout/work/a.md")).unwrap(),
        "hi"
    );
    let usage = hub.openrouter_usage_today();
    let scout = usage
        .iter()
        .find(|u| u.bot.as_deref() == Some("scout"))
        .unwrap();
    assert_eq!(scout.requests, 2);
    // The prompt says how the loop works.
    let req = &model.requests()[0];
    assert!(req.messages[0].text().contains("How you work here:"));
}

#[tokio::test]
async fn a_bots_daily_limit_stops_it_with_an_alert() {
    let tmp = tempfile::tempdir().unwrap();
    let model = Arc::new(ScriptedModel::new().with_responder(|_| Step::text("ok")));
    let bots = tmp.path().join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let mut bot = Bot::create(&bots, BotId::new("scout").unwrap(), "Scout", "").unwrap();
    bot.loop_settings.daily_requests = Some(2);
    bot.save().unwrap();
    let hub = hub(tmp.path(), &["scout"], &model);
    let mut updates = hub.subscribe();
    let mut replies = Vec::new();
    let mut alerts = Vec::new();
    for _ in 0..3 {
        hub.send_as("scout", "hi", &Viewer::farm("Jay"))
            .await
            .unwrap();
        replies.push(done(&mut updates, "scout").await.0);
    }
    while let Ok(msg) = updates.try_recv() {
        if let ServerMsg::UsageAlert(a) = msg {
            alerts.push(a.level);
        }
    }
    assert_eq!(replies[0], "ok");
    assert_eq!(replies[1], "ok");
    assert_eq!(
        replies[2],
        "Scout has used its 2 requests for today. It can go on tomorrow, or the owner can raise \
         the limit (Settings → Engines)."
    );
    assert_eq!(model.requests().len(), 2, "nothing went out the third time");
    let all: Vec<u8> = hub.live_usage_alerts().iter().map(|a| a.level).collect();
    assert!(all.contains(&100), "{all:?} {alerts:?}");
}

#[tokio::test]
async fn private_bots_ask_providers_not_to_keep_data() {
    let tmp = tempfile::tempdir().unwrap();
    let model = Arc::new(ScriptedModel::new().with_responder(|_| Step::text("ok")));
    let hub = hub(tmp.path(), &[], &model);
    let sam = hub.person_named("Sam").unwrap();
    let sam = Viewer::person(sam.id, "Sam", false);
    let shared = hub
        .create_bot_as("Shared", "", Some(&sam), false)
        .await
        .unwrap();
    let private = hub
        .create_bot_as("Mine", "", Some(&sam), true)
        .await
        .unwrap();
    let mut updates = hub.subscribe();
    for bot in [&shared.id, &private.id] {
        hub.send_as(bot, "hi", &sam).await.unwrap();
        done(&mut updates, bot).await;
    }
    let reqs = model.requests();
    let prefs = |i: usize| reqs[i].provider.clone().unwrap();
    assert_eq!(
        prefs(0).data_collection,
        None,
        "shared: OpenRouter's default"
    );
    assert_eq!(prefs(1).data_collection, Some(DataCollection::Deny));
    assert_eq!(prefs(1).zdr, Some(true));
    assert!(private.private);
    // New bots run on OpenRouter, so Sam could task them.
    assert_eq!(hub.engine_of_bot(&shared.id), Some(EngineKind::OpenRouter));
}

#[tokio::test]
async fn a_turn_goes_on_after_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let first = Arc::new(
        ScriptedModel::new()
            .then(Step::tool(
                "Write",
                json!({"file_path": "a.md", "content": "1"}),
            ))
            .then(Step::Hang),
    );
    let hub1 = hub(tmp.path(), &["scout"], &first);
    let mut updates = hub1.subscribe();
    hub1.send_as("scout", "write two notes", &Viewer::farm("Jay"))
        .await
        .unwrap();
    // Wait until the first step's result is saved and the model is asked again.
    tokio::time::timeout(Duration::from_secs(10), async {
        while first.requests().len() < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let _ = updates.try_recv();
    hub1.shutdown().await;
    drop(hub1);

    let second = Arc::new(ScriptedModel::new().then(Step::text("Wrote both.")));
    let hub2 = hub(tmp.path(), &["scout"], &second);
    let mut updates = hub2.subscribe();
    hub2.resume_loop_turns().await;
    let (reply, _) = done(&mut updates, "scout").await;
    assert_eq!(reply, "Wrote both.");
    // It went on from the saved step: the model saw the write and its result.
    let req = &second.requests()[0];
    let roles: Vec<&str> = req.messages.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool"]);
    // Nothing left to pick up.
    let hub3 = hub(tmp.path(), &["scout"], &second);
    hub3.resume_loop_turns().await;
    assert_eq!(second.requests().len(), 1);
}

#[tokio::test]
async fn someone_elses_claude_bot_turn_isnt_picked_up() {
    let tmp = tempfile::tempdir().unwrap();
    let first = Arc::new(ScriptedModel::new().then(Step::Hang));
    let hub1 = hub(tmp.path(), &["scout"], &first);
    hub1.send_as("scout", "hi", &Viewer::gate("Sam"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while first.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    hub1.shutdown().await;
    drop(hub1);
    // Switched to Claude while oxbot was stopped.
    let mut bot = Bot::load(&tmp.path().join("bots/scout")).unwrap();
    bot.engine = EngineKind::Claude;
    bot.save().unwrap();
    let bots = tmp.path().join("bots");
    let config = HubConfig {
        openrouter: Some(LoopParts::new(
            Arc::new(ScriptedModel::new()) as _,
            None,
            None,
        )),
        pre_engine_tests: false,
        ..HubConfig::for_app(&bots)
    };
    let hub2 = Hub::new(
        vec![Bot::load(&bots.join("scout")).unwrap()],
        Store::open(&tmp.path().join("oxbot.db")).unwrap(),
        config,
    );
    hub2.resume_loop_turns().await;
    let last = hub2.history("scout", 0).unwrap();
    let errors: Vec<String> = last
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::Error { message } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        errors,
        [
            "oxbot restarted and couldn't pick this task up again (the bot runs on Claude now). Send \
          it again if you still need it."
        ]
    );
}

#[tokio::test]
async fn what_a_loop_bot_reads_taints_its_session_as_on_claude() {
    let tmp = tempfile::tempdir().unwrap();
    let model = Arc::new(
        ScriptedModel::new()
            .then(Step::tool("Read", json!({"file_path": "notes.md"})))
            .then(Step::tool(
                "mcp__oxbot__update_todo",
                json!({"items": [
                {"id": "t1", "text": "Read the notes", "state": "done"}]}),
            ))
            .then(Step::text("Read them."))
            // After the answer check.
            .then(Step::text("Read them.")),
    );
    let hub = hub(tmp.path(), &["scout"], &model);
    std::fs::write(tmp.path().join("bots/scout/work/notes.md"), "Buy a lamp.").unwrap();
    let mut updates = hub.subscribe();
    hub.send_as("scout", "read my notes", &Viewer::farm("Jay"))
        .await
        .unwrap();
    assert_eq!(done(&mut updates, "scout").await.0, "Read them.");
    // The hub marks the session from the same tool names as on Claude: the
    // file taints it, the loop's to-do list doesn't change that.
    let store = Store::open(&tmp.path().join("oxbot.db")).unwrap();
    let taint = store.session_taint(&BotId::new("scout").unwrap()).unwrap();
    assert_eq!(taint.as_deref(), Some("a file or tool result (Read)"));
    // The loop ran its own tool after the gate said yes.
    let req = &model.requests()[2];
    let todo = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == oxbot_llm::Role::Tool)
        .unwrap();
    // The newest message ends with the loop's rules (never saved).
    assert!(
        todo.text()
            .starts_with("<todo>\n[x] t1 Read the notes\n</todo>\n\n[oxbot] Answer only"),
        "{}",
        todo.text()
    );
}
