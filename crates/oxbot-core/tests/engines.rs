//! Engines per bot, with every rule on: who may task a bot (anyone in the
//! household), the task chain's origin, Herders on Codex, switching
//! engines, and bots from before Claude left oxbot moving to Codex.
//! Bots run on oxbot's own loop with a stand-in model
//! (`oxbot_core::testing`), except where a test says Codex.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::engine::EngineKind;
use oxbot_core::hub::MOVED_FROM_CLAUDE;
use oxbot_core::testing::{self, FakeBots, On, Tool};
use oxbot_core::{Bot, BotId, Hub, HubConfig, HubError, LoopParts, Store, Viewer};
use oxbot_llm::{ScriptedModel, Step};
use oxbot_protocol::{EventKind, ServerMsg};
use tokio::sync::broadcast::Receiver;

/// A hub with bots `ids` in `dir/bots` on the loop, every rule on, history
/// in `dir/oxbot.db`; each bot's messages take `tool` (helpers answer
/// without tools).
fn hub_in(dir: &Path, ids: &[&str], tool: Tool) -> Arc<Hub> {
    let bots = dir.join("bots");
    let all = testing::bots(&bots, ids, &On::Loop);
    let fake = Arc::new(FakeBots {
        helper_tools: vec![Tool::None],
        no_page: true,
        ..FakeBots::new(&bots, tool)
    });
    let config = testing::config(&bots, &dir.join("home"), &fake, &On::Loop);
    let hub = Hub::new(all, Store::open(&dir.join("oxbot.db")).unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub
}

async fn turn_done(updates: &mut Receiver<ServerMsg>, bot: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let ServerMsg::Event(e) = updates.recv().await.unwrap()
                && e.bot == bot
                && matches!(e.kind, EventKind::TurnDone { .. })
            {
                return;
            }
        }
    })
    .await
    .unwrap();
}

fn user_messages(hub: &Hub, bot: &str) -> Vec<(u64, String)> {
    hub.history(bot, 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::UserMessage { text, .. } => Some((e.seq, text)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn each_turn_keeps_who_started_it_on_the_mac_or_a_phone() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["recipes"], Tool::None);
    let jay = hub.farm_person("Jay").unwrap();
    let mut updates = hub.subscribe();

    hub.send_as("recipes", "on the Mac", &hub.farm_viewer("Jay"))
        .await
        .unwrap();
    turn_done(&mut updates, "recipes").await;
    let phone = Viewer::person(jay.id.clone(), "Jay", false);
    hub.send_as("recipes", "on a phone", &phone).await.unwrap();
    turn_done(&mut updates, "recipes").await;

    let sent = user_messages(&hub, "recipes");
    assert_eq!(sent.len(), 2);
    let store = Store::open(&tmp.path().join("oxbot.db")).unwrap();
    let recipes = BotId::new("recipes").unwrap();
    let mac = store.turn_origin(&recipes, sent[0].0).unwrap().unwrap();
    assert!(mac.on_farm);
    assert_eq!(mac.entry, "farm");
    assert_eq!(mac.person.as_deref(), Some(jay.id.as_str()));
    assert_eq!(
        (mac.root_bot.as_str(), mac.root_seq),
        ("recipes", sent[0].0)
    );
    assert_eq!(mac.engine, "openrouter");
    let gate = store.turn_origin(&recipes, sent[1].0).unwrap().unwrap();
    assert!(!gate.on_farm);
    assert_eq!(gate.entry, "gate");
    hub.shutdown().await;
}

#[tokio::test]
async fn anyone_in_the_household_tasks_a_shared_bot() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["recipes"], Tool::None);
    hub.farm_person("Jay").unwrap();
    let mut updates = hub.subscribe();
    let sam = Viewer::gate("Sam");
    assert!(hub.may_send("recipes", &sam).is_ok());
    hub.send_as("recipes", "hello", &sam).await.unwrap();
    turn_done(&mut updates, "recipes").await;
    let sent = user_messages(&hub, "recipes");
    assert_eq!(sent, [(1, "hello".to_owned())]);
    let list = sam.redact(hub.bots().await.unwrap());
    assert!(list.iter().all(|b| b.can_task), "{list:?}");
    hub.shutdown().await;
}

#[tokio::test]
async fn the_owners_herder_passes_a_task_on_in_the_owners_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["assistant", "helper"], Tool::Task);
    let jay = hub.farm_person("Jay").unwrap();
    let mut updates = hub.subscribe();

    hub.send_as("assistant", "ask the helper", &hub.farm_viewer("Jay"))
        .await
        .unwrap();
    turn_done(&mut updates, "assistant").await;
    let sent = user_messages(&hub, "helper");
    assert_eq!(sent.len(), 1, "the task reached the helper");
    let store = Store::open(&tmp.path().join("oxbot.db")).unwrap();
    let origin = store
        .turn_origin(&BotId::new("helper").unwrap(), sent[0].0)
        .unwrap()
        .unwrap();
    assert_eq!(origin.via, ["assistant"]);
    assert_eq!(origin.person.as_deref(), Some(jay.id.as_str()));
    assert!(origin.on_farm, "still the owner's chain");
    assert_eq!(origin.root_bot, "assistant");
    hub.shutdown().await;
}

#[tokio::test]
async fn without_an_openrouter_sign_in_every_bot_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let bots = tmp.path().join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let all = vec![
        Bot::create(&bots, BotId::new("assistant").unwrap(), "Herder", "").unwrap(),
        Bot::create(&bots, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
    ];
    // No OpenRouter here at all: neither engine can run.
    let hub = Hub::new(all, Store::open_in_memory().unwrap(), HubConfig::new(&bots));
    hub.farm_person("Jay").unwrap();
    let anns = hub.herder_for("Ann").unwrap();
    for (bot, viewer) in [
        ("assistant", hub.farm_viewer("Jay")),
        (anns.as_str(), Viewer::gate("Ann")),
        ("recipes", Viewer::gate("Sam")),
    ] {
        assert_eq!(hub.engine_of_bot(bot), Some(EngineKind::Codex), "{bot}");
        let err = hub.send_as(bot, "hi", &viewer).await.unwrap_err();
        assert!(matches!(err, HubError::EngineNotReady(_)), "{err}");
        assert_eq!(
            err.to_string(),
            "The home's AI account (OpenRouter) isn't connected yet; ask Jay."
        );
        assert_eq!(hub.history(bot, 0).unwrap(), [], "{bot}: nothing recorded");
    }
}

#[tokio::test]
async fn a_session_of_another_engine_starts_new() {
    let tmp = tempfile::tempdir().unwrap();
    let recipes = BotId::new("recipes").unwrap();
    let run = |text: &'static str| {
        let dir = tmp.path().to_owned();
        async move {
            let hub = hub_in(&dir, &["recipes"], Tool::None);
            let mut updates = hub.subscribe();
            hub.send_as("recipes", text, &Viewer::farm("Jay"))
                .await
                .unwrap();
            turn_done(&mut updates, "recipes").await;
            hub.shutdown().await;
        }
    };
    let store = || Store::open(&tmp.path().join("oxbot.db")).unwrap();
    run("hi").await;
    let first = store().session(&recipes).unwrap().unwrap();
    assert_eq!(
        store().session_engine(&recipes).unwrap().as_deref(),
        Some("openrouter")
    );

    // Same engine: it goes on with the session.
    run("again").await;
    assert_eq!(store().session(&recipes).unwrap().unwrap().id, first.id);

    // Saved as Claude's (from before Claude left oxbot): a new one.
    rusqlite::Connection::open(tmp.path().join("oxbot.db"))
        .unwrap()
        .execute("UPDATE sessions SET engine = 'claude'", [])
        .unwrap();
    run("and again").await;
    let after = store().session(&recipes).unwrap().unwrap();
    assert_ne!(
        after.id, first.id,
        "no engine goes on with Claude's session"
    );
    assert_eq!(
        store().session_engine(&recipes).unwrap().as_deref(),
        Some("openrouter")
    );
    assert_eq!(
        store().history(&recipes, 0).unwrap().len(),
        3 * 3,
        "the chat is kept"
    );
}

#[tokio::test]
async fn whoever_manages_a_bot_switches_its_engine_but_never_a_herders() {
    let tmp = tempfile::tempdir().unwrap();
    let bots = tmp.path().join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let fake = Arc::new(FakeBots::new(&bots, Tool::None));
    let config = HubConfig {
        // Each bot on its own engine, as in the app.
        engines_on: None,
        ..testing::config(&bots, &tmp.path().join("home"), &fake, &On::Loop)
    };
    let herder = Bot::create(&bots, BotId::new("assistant").unwrap(), "Herder", "").unwrap();
    let hub = Hub::new(vec![herder], Store::open_in_memory().unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.farm_person("Jay").unwrap();
    let owner = hub.farm_viewer("Jay");
    let notes = hub
        .create_bot_with("Notes", "", Some("Sam"), true)
        .await
        .unwrap();
    assert_eq!(notes.engine, oxbot_protocol::EngineKind::Codex, "new bots");
    let sam = Viewer::gate("Sam");
    let ann = Viewer::gate("Ann");

    // Ann may not change Sam's bot; Sam may, with a model of his choice.
    assert!(matches!(
        hub.set_engine("notes", EngineKind::OpenRouter, None, &ann)
            .await,
        Err(HubError::UnknownBot(_) | HubError::NotAllowed(_))
    ));
    hub.set_engine(
        "notes",
        EngineKind::OpenRouter,
        Some(testing::MODEL.into()),
        &sam,
    )
    .await
    .unwrap();
    let bot = hub.bot("notes").unwrap();
    assert_eq!(bot.engine, EngineKind::OpenRouter);
    assert_eq!(bot.loop_settings.model.as_deref(), Some(testing::MODEL));
    let file = std::fs::read_to_string(bots.join("notes/bot.toml")).unwrap();
    assert!(file.contains("engine = \"openrouter\""), "{file}");
    // Back to Codex: refused while Codex isn't here, and the model stays.
    let err = hub
        .set_engine("notes", EngineKind::Codex, None, &owner)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "The AI helper isn't set up on the home computer."
    );
    assert_eq!(hub.bot("notes").unwrap().engine, EngineKind::OpenRouter);
    // Herders' engines are fixed.
    let err = hub
        .set_engine("assistant", EngineKind::OpenRouter, None, &owner)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "The Assistant's AI model is set by the owner."
    );
}

#[tokio::test]
async fn a_message_waiting_for_a_slot_is_checked_again() {
    let tmp = tempfile::tempdir().unwrap();
    let bots = tmp.path().join("bots");
    let all = testing::bots(&bots, &["busy", "scout"], &On::Loop);
    // The first request never answers, so "busy" holds the only slot.
    let model = Arc::new(ScriptedModel::new().then(Step::Hang));
    let config = HubConfig {
        openrouter: Some(LoopParts {
            free_per_minute: 1000,
            ..LoopParts::new(Arc::clone(&model) as _, None, None)
        }),
        max_concurrent_turns: 1,
        ..HubConfig::new(&bots)
    };
    let hub = Hub::new(
        all,
        Store::open(&tmp.path().join("oxbot.db")).unwrap(),
        config,
    );
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.farm_person("Jay").unwrap();
    let owner = hub.farm_viewer("Jay");
    hub.send_as("busy", "take your time", &owner).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while model.requests().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    // Sam's message to Scout waits for the slot…
    let waiting = {
        let hub = Arc::clone(&hub);
        tokio::spawn(async move { hub.send_as("scout", "hello", &Viewer::gate("Sam")).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished(), "it waits for the only slot");
    // …while the owner deletes Scout, then the slot frees up.
    hub.delete_bot("scout", &owner).await.unwrap();
    hub.cancel("busy").await.unwrap();

    let err = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, HubError::UnknownBot(_)), "{err}");
    assert_eq!(
        model.requests().len(),
        1,
        "no model request for Sam's message"
    );
    hub.shutdown().await;
}

/// A data folder as an oxbot from before Claude left it leaves one: a
/// shared bot on Claude (no engine in its file, a Claude model), one that
/// says `claude`, a bot on `OpenRouter`, the Farm's Herder and a person's
/// own Herder (whose files name no engine), Claude sessions saved for
/// them, a chat each, and an alert for Claude's 5-hour window.
fn pre_upgrade_folder(dir: &Path) -> std::path::PathBuf {
    let bots = dir.join("bots");
    for (id, file) in [
        ("assistant", "name = \"Herder\"\n"),
        (
            "recipes",
            "name = \"Recipes\"\nmodel = \"sonnet\"\ncreated_by = \"Jay\"\n",
        ),
        (
            "journal",
            "name = \"Journal\"\nengine = \"claude\"\ncreated_by = \"Sam\"\nprivate = true\n",
        ),
        (
            "scout",
            "name = \"Scout\"\nengine = \"openrouter\"\ncreated_by = \"Jay\"\n",
        ),
        (
            "assistant--ann-1a2b3c",
            "name = \"Herder\"\ncreated_by = \"Ann\"\nprivate = true\n",
        ),
    ] {
        std::fs::create_dir_all(bots.join(id).join("work")).unwrap();
        std::fs::write(bots.join(id).join("bot.toml"), file).unwrap();
    }
    let db = dir.join("oxbot.db");
    Store::open_at_version(&db, 21).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    for (i, id) in ["assistant", "recipes", "journal"].iter().enumerate() {
        conn.execute(
            "INSERT INTO sessions (bot, claude_session, prompt, updated_ms, engine)
             VALUES (?1, ?2, 'old prompt', 1, 'claude')",
            rusqlite::params![id, format!("claude-session-{i}")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (bot, seq, ts_ms, body) VALUES (?1, 1, 1, ?2)",
            rusqlite::params![
                id,
                r#"{"kind":"assistant_message","text":"an answer from Claude"}"#
            ],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO limit_alerts (name, resets_at_ms, level, used, alerted_ms)
         VALUES ('five_hour', 9999999999999, 80, 0.85, 1);",
    )
    .unwrap();
    bots
}

#[tokio::test]
async fn bots_from_before_claude_left_move_to_codex_once_and_keep_their_chats() {
    let tmp = tempfile::tempdir().unwrap();
    let bots = pre_upgrade_folder(tmp.path());
    // As the app starts: the Assistant's file is brought up to date (its
    // name) before the hub loads the bots, and must still be moved then.
    let start = || {
        oxbot_core::bot::ensure_default(&bots).unwrap();
        Hub::new(
            oxbot_core::bot::load_all(&bots).unwrap(),
            Store::open(&tmp.path().join("oxbot.db")).unwrap(),
            HubConfig::new(&bots),
        )
    };
    let hub = start();
    hub.farm_person("Jay").unwrap();
    let file = |id: &str| std::fs::read_to_string(bots.join(id).join("bot.toml")).unwrap();

    // Their files say Codex now, and lose Claude's model; the rest stays
    // (an owner is added, as for any bot from before owners).
    let recipes = file("recipes");
    assert!(
        recipes.starts_with("name = \"Recipes\"\nengine = \"codex\"\n")
            && recipes.contains("created_by = \"Jay\"")
            && !recipes.contains("sonnet"),
        "{recipes}"
    );
    assert!(
        file("journal").contains("engine = \"codex\"")
            && file("journal").contains("private = true")
    );
    assert!(file("assistant").contains("engine = \"codex\""));
    assert!(
        file("assistant").contains("name = \"Assistant\""),
        "Herder is the Assistant now"
    );
    assert!(file("assistant--ann-1a2b3c").contains("engine = \"codex\""));
    assert!(
        file("scout").contains("engine = \"openrouter\""),
        "untouched"
    );

    // Each chat that ran on Claude keeps its history and gets one line.
    let note = |hub: &Hub, id: &str| {
        hub.history(id, 0)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(&e.kind, EventKind::Note { text } if text == MOVED_FROM_CLAUDE))
            .count()
    };
    for id in ["assistant", "recipes", "journal"] {
        let history = hub.history(id, 0).unwrap();
        assert!(
            matches!(&history[0].kind, EventKind::AssistantMessage { text } if text == "an answer from Claude"),
            "{id}: {history:?}"
        );
        assert_eq!(note(&hub, id), 1, "{id}");
    }
    assert_eq!(note(&hub, "scout"), 0, "it never ran on Claude");
    assert_eq!(note(&hub, "assistant--ann-1a2b3c"), 0, "it ran on the loop");
    assert_eq!(
        MOVED_FROM_CLAUDE,
        "This bot now uses free and paid AI models through your OpenRouter account. Its old \
         Claude sessions can't be continued."
    );
    for id in ["assistant", "recipes", "journal", "assistant--ann-1a2b3c"] {
        assert_eq!(hub.engine_of_bot(id), Some(EngineKind::Codex), "{id}");
    }
    let list = hub.bots().await.unwrap();
    let recipes = list.iter().find(|b| b.id == "recipes").unwrap();
    assert_eq!(recipes.engine, oxbot_protocol::EngineKind::Codex);
    assert_eq!(
        recipes.model, None,
        "Claude's model isn't an OpenRouter model"
    );

    // With no OpenRouter sign-in yet, they say so rather than fail later.
    let err = hub
        .send_as("recipes", "hi", &hub.farm_viewer("Jay"))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "The home's AI account (OpenRouter) isn't connected yet; ask Jay."
    );
    // Claude's window alert never shows again.
    assert_eq!(hub.live_usage_alerts(), []);
    hub.shutdown().await;
    drop(hub);

    // Started again: no second line, and the files stay as they are.
    let before = file("recipes");
    let hub = start();
    for id in ["assistant", "recipes", "journal"] {
        assert_eq!(note(&hub, id), 1, "{id}: told once");
    }
    assert_eq!(file("recipes"), before);
    hub.shutdown().await;
}

#[tokio::test]
async fn a_bot_moved_from_claude_starts_a_new_session_on_its_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let bots = pre_upgrade_folder(tmp.path());
    let fake = Arc::new(FakeBots {
        no_page: true,
        ..FakeBots::new(&bots, Tool::None)
    });
    // Its file says Codex now; here every bot runs on the loop (no codex).
    let config = testing::config(&bots, &tmp.path().join("home"), &fake, &On::Loop);
    let hub = Hub::new(
        oxbot_core::bot::load_all(&bots).unwrap(),
        Store::open(&tmp.path().join("oxbot.db")).unwrap(),
        config,
    );
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.farm_person("Jay").unwrap();
    let mut updates = hub.subscribe();
    hub.send_as("recipes", "hi", &hub.farm_viewer("Jay"))
        .await
        .unwrap();
    turn_done(&mut updates, "recipes").await;
    let store = Store::open(&tmp.path().join("oxbot.db")).unwrap();
    let recipes = BotId::new("recipes").unwrap();
    let session = store.session(&recipes).unwrap().unwrap();
    assert_ne!(
        session.id, "claude-session-1",
        "Claude's session isn't resumed"
    );
    let history = hub.history("recipes", 0).unwrap();
    assert!(matches!(&history[1].kind, EventKind::Note { .. }));
    assert!(matches!(&history[2].kind, EventKind::UserMessage { text, .. } if text == "hi"));
    hub.shutdown().await;
}
