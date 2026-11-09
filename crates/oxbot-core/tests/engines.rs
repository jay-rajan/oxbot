//! The engine every bot runs on, with every rule on: who may task a bot
//! (anyone in the household), the task chain's origin, setting a bot's AI
//! model, and bots from engines oxbot no longer has (Claude, oxbot's own
//! loop) moving to Codex. Bots run on Codex with a stand-in model
//! (`oxbot_core::testing`); a test skips, saying why, on a Mac without
//! Codex 0.148.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::engine::EngineKind;
use oxbot_core::hub::{MOVED_FROM_CLAUDE, MOVED_FROM_LOOP};
use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{Bot, BotId, Hub, HubConfig, HubError, Store, Viewer};
use oxbot_protocol::{Answer, EventKind, ServerMsg};
use tokio::sync::broadcast::Receiver;

/// A hub with bots `ids` in `dir/bots` on Codex (`codex`), every rule on,
/// history in `dir/oxbot.db`; each bot's messages take `tool` (helpers
/// answer without tools).
async fn hub_in(dir: &Path, ids: &[&str], tool: Tool, codex: &Path) -> Arc<Hub> {
    hub_with(dir, ids, tool, codex, |_| {}).await
}

/// Like [`hub_in`], with `tweak` applied to the settings.
async fn hub_with(
    dir: &Path,
    ids: &[&str],
    tool: Tool,
    codex: &Path,
    tweak: impl FnOnce(&mut HubConfig),
) -> Arc<Hub> {
    let bots = dir.join("bots");
    let all = testing::bots(&bots, ids);
    let fake = Arc::new(FakeBots {
        helper_tools: vec![Tool::None],
        no_page: true,
        ..FakeBots::new(&bots, tool)
    });
    let mut config = testing::config(&bots, &dir.join("home"), &fake, Some(codex));
    tweak(&mut config);
    let hub = Hub::new(all, Store::open(&dir.join("oxbot.db")).unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
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
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let hub = hub_in(tmp.path(), &["recipes"], Tool::None, &codex).await;
    let jay = hub.owner_person("Jay").unwrap();
    let mut updates = hub.subscribe();

    hub.send_as("recipes", "on the Mac", &hub.home_computer_viewer("Jay"))
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
    assert!(mac.on_home_computer);
    assert_eq!(mac.entry, "home_computer");
    assert_eq!(mac.person.as_deref(), Some(jay.id.as_str()));
    assert_eq!(
        (mac.root_bot.as_str(), mac.root_seq),
        ("recipes", sent[0].0)
    );
    assert_eq!(mac.engine, "codex");
    let device = store.turn_origin(&recipes, sent[1].0).unwrap().unwrap();
    assert!(!device.on_home_computer);
    assert_eq!(device.entry, "device");
    hub.shutdown().await;
}

#[tokio::test]
async fn anyone_in_the_household_tasks_a_shared_bot() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let hub = hub_in(tmp.path(), &["recipes"], Tool::None, &codex).await;
    hub.owner_person("Jay").unwrap();
    let mut updates = hub.subscribe();
    let sam = Viewer::device("Sam");
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
async fn the_owners_assistant_passes_a_task_on_in_the_owners_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let hub = hub_in(tmp.path(), &["assistant", "helper"], Tool::Task, &codex).await;
    let jay = hub.owner_person("Jay").unwrap();
    let mut updates = hub.subscribe();

    hub.send_as(
        "assistant",
        "ask the helper",
        &hub.home_computer_viewer("Jay"),
    )
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
    assert!(origin.on_home_computer, "still the owner's chain");
    assert_eq!(origin.root_bot, "assistant");
    hub.shutdown().await;
}

#[tokio::test]
async fn without_an_openrouter_sign_in_every_bot_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let bots = tmp.path().join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let all = vec![
        Bot::create(&bots, BotId::new("assistant").unwrap(), "Assistant", "").unwrap(),
        Bot::create(&bots, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
    ];
    // No OpenRouter here at all: no bot can run.
    let hub = Hub::new(all, Store::open_in_memory().unwrap(), HubConfig::new(&bots));
    hub.owner_person("Jay").unwrap();
    let anns = hub.assistant_for("Ann").unwrap();
    for (bot, viewer) in [
        ("assistant", hub.home_computer_viewer("Jay")),
        (anns.as_str(), Viewer::device("Ann")),
        ("recipes", Viewer::device("Sam")),
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
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let recipes = BotId::new("recipes").unwrap();
    let run = |text: &'static str| {
        let dir = tmp.path().to_owned();
        let codex = codex.clone();
        async move {
            let hub = hub_in(&dir, &["recipes"], Tool::None, &codex).await;
            let mut updates = hub.subscribe();
            hub.send_as("recipes", text, &Viewer::home_computer("Jay"))
                .await
                .unwrap();
            turn_done(&mut updates, "recipes").await;
            hub.shutdown().await;
        }
    };
    let store = || Store::open(&tmp.path().join("oxbot.db")).unwrap();
    let engine = || store().session_engine(&recipes).unwrap();
    run("hi").await;
    let first = store().session(&recipes).unwrap().unwrap();
    assert_eq!(engine().as_deref(), Some("codex"));

    // Same engine: it goes on with the session.
    run("again").await;
    assert_eq!(store().session(&recipes).unwrap().unwrap().id, first.id);

    // Saved as an engine oxbot no longer has: a new one each time.
    let mut last = first.id;
    for old in ["openrouter", "claude"] {
        rusqlite::Connection::open(tmp.path().join("oxbot.db"))
            .unwrap()
            .execute("UPDATE sessions SET engine = ?1", [old])
            .unwrap();
        run("and again").await;
        let after = store().session(&recipes).unwrap().unwrap();
        assert_ne!(after.id, last, "no engine goes on with {old}'s session");
        assert_eq!(engine().as_deref(), Some("codex"));
        last = after.id;
    }
    let asked = store()
        .history(&recipes, 0)
        .unwrap()
        .into_iter()
        .filter(|e| matches!(e.kind, EventKind::UserMessage { .. }))
        .count();
    assert_eq!(asked, 4, "the chat is kept");
}

#[tokio::test]
async fn whoever_manages_a_bot_sets_its_model_but_never_a_assistants() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let hub = hub_in(tmp.path(), &["assistant"], Tool::None, &codex).await;
    hub.owner_person("Jay").unwrap();
    let owner = hub.home_computer_viewer("Jay");
    let notes = hub
        .create_bot_with("Notes", "", Some("Sam"), true)
        .await
        .unwrap();
    assert_eq!(notes.engine, oxbot_protocol::EngineKind::Codex);
    let sam = Viewer::device("Sam");
    let ann = Viewer::device("Ann");

    // Ann may not change Sam's bot; Sam may, with a model of his choice.
    assert!(matches!(
        hub.set_engine(
            "notes",
            EngineKind::Codex,
            Some(testing::MODEL.into()),
            None,
            &ann
        )
        .await,
        Err(HubError::UnknownBot(_) | HubError::NotAllowed(_))
    ));
    hub.set_engine(
        "notes",
        EngineKind::Codex,
        Some(testing::MODEL.into()),
        None,
        &sam,
    )
    .await
    .unwrap();
    let bot = hub.bot("notes").unwrap();
    assert_eq!(bot.loop_settings.model.as_deref(), Some(testing::MODEL));
    let file = std::fs::read_to_string(tmp.path().join("bots/notes/bot.toml")).unwrap();
    assert!(file.contains("engine = \"codex\""), "{file}");
    // Back to Auto.
    hub.set_engine(
        "notes",
        EngineKind::Codex,
        Some(String::new()),
        None,
        &owner,
    )
    .await
    .unwrap();
    assert_eq!(hub.bot("notes").unwrap().loop_settings.model, None);
    // An Assistant's is the owner's to set.
    let err = hub
        .set_engine("assistant", EngineKind::Codex, None, None, &owner)
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
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    // "busy" waits on its approval request, holding the only slot.
    let hub = hub_with(tmp.path(), &["busy", "scout"], Tool::Upload, &codex, |c| {
        c.max_concurrent_turns = 1;
        c.approval_timeout = Duration::from_secs(30);
    })
    .await;
    hub.owner_person("Jay").unwrap();
    let owner = hub.home_computer_viewer("Jay");
    let mut updates = hub.subscribe();
    hub.send_as("busy", "upload my CV", &owner).await.unwrap();
    let card = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let ServerMsg::ApprovalNeeded(a) = updates.recv().await.unwrap() {
                return a;
            }
        }
    })
    .await
    .unwrap();

    // Sam's message to Scout waits for the slot…
    let waiting = {
        let hub = Arc::clone(&hub);
        tokio::spawn(async move { hub.send_as("scout", "hello", &Viewer::device("Sam")).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!waiting.is_finished(), "it waits for the only slot");
    // …while the owner deletes Scout, then the slot frees up.
    hub.delete_bot("scout", &owner).await.unwrap();
    assert!(hub.answer(card.id, Answer::Deny));

    let err = tokio::time::timeout(Duration::from_secs(20), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, HubError::UnknownBot(_)), "{err}");
    hub.shutdown().await;
}

/// A data folder as an oxbot from before Codex was the only engine leaves
/// one: a shared bot on Claude (no engine in its file, a Claude model),
/// one that says `claude`, a bot on oxbot's own loop (`openrouter`, with
/// a loop session), the home computer's Assistant and a person's own Assistant (whose
/// files name no engine), Claude sessions saved for them, a chat each,
/// and an alert for Claude's 5-hour window.
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
            "name = \"Scout\"\nengine = \"openrouter\"\ncreated_by = \"Jay\"\n\n[openrouter]\nmodel = \"m:free\"\n",
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
    for (i, (id, engine)) in [
        ("assistant", "claude"),
        ("recipes", "claude"),
        ("journal", "claude"),
        ("scout", "openrouter"),
    ]
    .iter()
    .enumerate()
    {
        conn.execute(
            "INSERT INTO sessions (bot, claude_session, prompt, updated_ms, engine)
             VALUES (?1, ?2, 'old prompt', 1, ?3)",
            rusqlite::params![id, format!("old-session-{i}"), engine],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO events (bot, seq, ts_ms, body) VALUES (?1, 1, 1, ?2)",
            rusqlite::params![
                id,
                r#"{"kind":"assistant_message","text":"an earlier answer"}"#
            ],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO limit_alerts (name, resets_at_ms, level, used, alerted_ms)
         VALUES ('five_hour', 9999999999999, 80, 0.85, 1);
         INSERT INTO loop_sessions (id, bot, prompt_hash, created_ms) VALUES ('old-session-3', 'scout', 'h', 1);
         INSERT INTO loop_turns (session, turn, bot, user_seq, origin, question, status, started_ms, updated_ms)
         VALUES ('old-session-3', 0, 'scout', 1, '{}', 'compare the lamps', 'running', 1, 1);",
    )
    .unwrap();
    bots
}

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one upgrade, as a person sees it")]
async fn bots_from_retired_engines_move_to_codex_once_and_keep_their_chats() {
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
    hub.owner_person("Jay").unwrap();
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
    let scout = file("scout");
    assert!(
        scout.contains("engine = \"codex\"") && scout.contains("model = \"m:free\""),
        "the loop's bot keeps its model: {scout}"
    );

    // Each chat keeps its history and gets one line: Claude's or the loop's.
    let note = |hub: &Hub, id: &str, line: &str| {
        hub.history(id, 0)
            .unwrap()
            .into_iter()
            .filter(|e| matches!(&e.kind, EventKind::Note { text } if text == line))
            .count()
    };
    for (id, line) in [
        ("assistant", MOVED_FROM_CLAUDE),
        ("recipes", MOVED_FROM_CLAUDE),
        ("journal", MOVED_FROM_CLAUDE),
        ("scout", MOVED_FROM_LOOP),
    ] {
        let history = hub.history(id, 0).unwrap();
        assert!(
            matches!(&history[0].kind, EventKind::AssistantMessage { text } if text == "an earlier answer"),
            "{id}: {history:?}"
        );
        assert_eq!(note(&hub, id, line), 1, "{id}");
        assert_eq!(history.len(), 2, "{id}: one line, nothing else");
    }
    assert_eq!(
        note(&hub, "assistant--ann-1a2b3c", MOVED_FROM_LOOP),
        1,
        "a person's own Assistant ran on the loop"
    );
    assert_eq!(
        MOVED_FROM_CLAUDE,
        "This bot now uses free and paid AI models through your OpenRouter account. Its old \
         Claude sessions can't be continued."
    );
    assert_eq!(
        MOVED_FROM_LOOP,
        "This bot now uses free and paid AI models through your OpenRouter account, in a new way. \
         It starts a new conversation: a task it was in the middle of can't be continued, so \
         send it again if you still need it."
    );
    for line in [MOVED_FROM_CLAUDE, MOVED_FROM_LOOP] {
        assert!(
            !line.contains("Codex") && !line.contains("loop"),
            "plain words"
        );
    }
    for id in [
        "assistant",
        "recipes",
        "journal",
        "scout",
        "assistant--ann-1a2b3c",
    ] {
        assert_eq!(hub.engine_of_bot(id), Some(EngineKind::Codex), "{id}");
    }
    let list = hub.bots().await.unwrap();
    let recipes = list.iter().find(|b| b.id == "recipes").unwrap();
    assert_eq!(recipes.engine, oxbot_protocol::EngineKind::Codex);
    assert_eq!(
        recipes.model, None,
        "Claude's model isn't an OpenRouter model"
    );
    // The loop's turn that was running never goes on.
    let status: String = rusqlite::Connection::open(tmp.path().join("oxbot.db"))
        .unwrap()
        .query_row("SELECT status FROM loop_turns", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "stopped");

    // With no OpenRouter sign-in yet, they say so rather than fail later.
    let err = hub
        .send_as("recipes", "hi", &hub.home_computer_viewer("Jay"))
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
    let (before, scout_before) = (file("recipes"), file("scout"));
    let hub = start();
    for (id, line) in [("recipes", MOVED_FROM_CLAUDE), ("scout", MOVED_FROM_LOOP)] {
        assert_eq!(note(&hub, id, line), 1, "{id}: told once");
    }
    assert_eq!((file("recipes"), file("scout")), (before, scout_before));
    hub.shutdown().await;
}

#[tokio::test]
async fn a_moved_bot_starts_a_new_session_on_codex() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = testing::codex(tmp.path()).await else {
        return;
    };
    let bots = pre_upgrade_folder(tmp.path());
    let fake = Arc::new(FakeBots {
        no_page: true,
        ..FakeBots::new(&bots, Tool::None)
    });
    let config = testing::config(&bots, &tmp.path().join("home"), &fake, Some(&codex));
    let hub = Hub::new(
        oxbot_core::bot::load_all(&bots).unwrap(),
        Store::open(&tmp.path().join("oxbot.db")).unwrap(),
        config,
    );
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    hub.owner_person("Jay").unwrap();
    let mut updates = hub.subscribe();
    let store = Store::open(&tmp.path().join("oxbot.db")).unwrap();
    for (id, old) in [("recipes", "old-session-1"), ("scout", "old-session-3")] {
        hub.send_as(id, "hi", &hub.home_computer_viewer("Jay"))
            .await
            .unwrap();
        turn_done(&mut updates, id).await;
        let bot = BotId::new(id).unwrap();
        let session = store.session(&bot).unwrap().unwrap();
        assert_ne!(session.id, old, "{id}: the old session isn't resumed");
        assert_eq!(
            store.session_engine(&bot).unwrap().as_deref(),
            Some("codex")
        );
        let history = hub.history(id, 0).unwrap();
        assert!(matches!(&history[1].kind, EventKind::Note { .. }), "{id}");
        assert!(
            matches!(&history[2].kind, EventKind::UserMessage { text, .. } if text == "hi"),
            "{id}"
        );
        assert!(
            history
                .iter()
                .any(|e| matches!(&e.kind, EventKind::AssistantMessage { text } if text == "ok")),
            "{id}: it answers: {history:?}"
        );
    }
    hub.shutdown().await;
}
