//! Engines per bot and the owner-only rule for Claude bots, with every
//! rule on (`pre_engine_tests: false`), against the fake `claude` scripts.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::engine::EngineKind;
use oxbot_core::{Bot, BotId, Hub, HubConfig, HubError, Store, Viewer};
use oxbot_protocol::{EventKind, ServerMsg};
use tokio::sync::broadcast::Receiver;

fn claude_tests() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../oxbot-claude/tests")
}

/// A hub with bots `ids` in `dir/bots`, every rule on, history in
/// `dir/oxbot.db`. The fake records its arguments to `dir/args.txt`.
fn hub_in(dir: &Path, ids: &[&str], perm_tool: Option<&str>) -> Arc<Hub> {
    let bots = dir.join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let all = ids
        .iter()
        .map(|id| match Bot::load(&bots.join(id)) {
            Ok(bot) => bot,
            Err(_) => Bot::create(&bots, BotId::new(*id).unwrap(), id, "").unwrap(),
        })
        .collect();
    let mut env = vec![(
        "FAKE_CLAUDE_ARGS".to_owned(),
        dir.join("args.txt").display().to_string(),
    )];
    let program = if let Some(tool) = perm_tool {
        env.push(("FAKE_TOOL".to_owned(), tool.to_owned()));
        env.push(("FAKE_HELPER_TOOLS".to_owned(), "none".to_owned()));
        env.push(("FAKE_NO_PAGE".to_owned(), "1".to_owned()));
        claude_tests().join("fake-claude-perm.sh")
    } else {
        env.push((
            "FAKE_CLAUDE_FIXTURE".to_owned(),
            claude_tests()
                .join("fixtures/tool-use.jsonl")
                .display()
                .to_string(),
        ));
        claude_tests().join("fake-claude.sh")
    };
    let config = HubConfig {
        claude_program: program,
        extra_env: env,
        approval_timeout: Duration::from_secs(5),
        pre_engine_tests: false,
        ..HubConfig::for_app(&bots)
    };
    Hub::new(all, Store::open(&dir.join("oxbot.db")).unwrap(), config)
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
async fn the_owner_tasks_a_claude_bot_from_the_mac_or_a_phone() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["recipes"], None);
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
    assert_eq!(mac.engine, "claude");
    let gate = store.turn_origin(&recipes, sent[1].0).unwrap().unwrap();
    assert!(!gate.on_farm);
    assert_eq!(gate.entry, "gate");
    hub.shutdown().await;
}

#[tokio::test]
async fn someone_else_cant_task_a_claude_bot() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["recipes"], None);
    hub.farm_person("Jay").unwrap();
    let sam = Viewer::gate("Sam");

    let err = hub.send_as("recipes", "hello", &sam).await.unwrap_err();
    assert!(matches!(err, HubError::OwnerOnly { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "Only Jay can give recipes tasks: it runs on Jay's own Claude plan."
    );
    // Nothing recorded, no Claude started.
    assert_eq!(hub.history("recipes", 0).unwrap(), []);
    assert!(!tmp.path().join("args.txt").exists());
    // Files for it are refused before they're taken.
    let err = hub
        .send_files_as("recipes", "", &["u1".to_owned()], &sam)
        .await
        .unwrap_err();
    assert!(matches!(err, HubError::OwnerOnly { .. }), "{err}");
    assert!(matches!(
        hub.may_send("recipes", &sam),
        Err(HubError::OwnerOnly { .. })
    ));
    assert!(hub.may_send("recipes", &hub.farm_viewer("Jay")).is_ok());
    // A bot on another engine isn't owner-only; it just isn't set up yet.
    hub.set_engine(
        "recipes",
        EngineKind::OpenRouter,
        None,
        &hub.farm_viewer("Jay"),
        false,
    )
    .await
    .unwrap_err();
}

#[tokio::test]
async fn with_no_farm_person_only_the_farm_connection_tasks_claude_bots() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["recipes"], None);
    let mut updates = hub.subscribe();
    // "Jay" on a Gate is just someone called Jay until the Farm's person is chosen.
    let err = hub
        .send_as("recipes", "hi", &Viewer::gate("Jay"))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Only the Farm's owner can give recipes tasks: it runs on the Farm's owner's own Claude \
         plan."
    );
    hub.send_as("recipes", "hi", &Viewer::farm("Jay"))
        .await
        .unwrap();
    turn_done(&mut updates, "recipes").await;
    hub.shutdown().await;
}

#[tokio::test]
async fn the_owners_herder_passes_a_task_to_a_claude_bot() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["assistant", "helper"], Some("task"));
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
async fn other_peoples_herders_run_on_openrouter() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["assistant"], None);
    hub.farm_person("Jay").unwrap();
    let anns = hub.herder_for("Ann").unwrap();
    let err = hub
        .send_as(anns.as_str(), "hi", &Viewer::gate("Ann"))
        .await
        .unwrap_err();
    assert!(matches!(err, HubError::EngineNotReady(_)), "{err}");
    assert_eq!(
        err.to_string(),
        "OpenRouter isn't set up on this Mac yet; ask Jay."
    );
    assert_eq!(hub.history(anns.as_str(), 0).unwrap(), []);
}

#[tokio::test]
async fn a_session_of_another_engine_starts_new() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = hub_in(tmp.path(), &["recipes"], None);
    let mut updates = hub.subscribe();
    hub.send_as("recipes", "hi", &Viewer::farm("Jay"))
        .await
        .unwrap();
    turn_done(&mut updates, "recipes").await;
    hub.shutdown().await;
    drop(hub);
    let args = || std::fs::read_to_string(tmp.path().join("args.txt")).unwrap();
    assert!(args().contains("--session-id"));

    // Same engine: it goes on with the session.
    let hub = hub_in(tmp.path(), &["recipes"], None);
    let mut updates = hub.subscribe();
    hub.send_as("recipes", "again", &Viewer::farm("Jay"))
        .await
        .unwrap();
    turn_done(&mut updates, "recipes").await;
    hub.shutdown().await;
    drop(hub);
    assert!(args().contains("--resume"));

    // Saved as another engine's: a new one.
    let store = Store::open(&tmp.path().join("oxbot.db")).unwrap();
    let recipes = BotId::new("recipes").unwrap();
    let saved = store.session(&recipes).unwrap().unwrap();
    store
        .save_session(&recipes, Some(&saved), EngineKind::OpenRouter)
        .unwrap();
    let hub = hub_in(tmp.path(), &["recipes"], None);
    let mut updates = hub.subscribe();
    hub.send_as("recipes", "and again", &Viewer::farm("Jay"))
        .await
        .unwrap();
    turn_done(&mut updates, "recipes").await;
    hub.shutdown().await;
    assert!(args().contains("--session-id"));
    assert_eq!(
        store.session_engine(&recipes).unwrap().as_deref(),
        Some("claude")
    );
}

#[tokio::test]
async fn only_the_owner_switches_a_bot_to_claude() {
    let tmp = tempfile::tempdir().unwrap();
    let bots = tmp.path().join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let mut scout = Bot::create(&bots, BotId::new("scout").unwrap(), "Scout", "").unwrap();
    scout.engine = EngineKind::OpenRouter;
    scout.save().unwrap();
    let hub = hub_in(tmp.path(), &["assistant", "scout"], None);
    hub.farm_person("Jay").unwrap();
    let owner = hub.farm_viewer("Jay");
    hub.create_bot_with("Notes", "", Some("Sam"), true)
        .await
        .unwrap();
    let sam = Viewer::gate("Sam");

    // Sam can't, even for his own bot.
    let err = hub
        .set_engine("notes", EngineKind::Claude, None, &sam, true)
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("Only Jay can switch"), "{err}");
    // A shared bot needs the owner's confirmation first.
    let err = hub
        .set_engine("scout", EngineKind::Claude, None, &owner, false)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("makes it owner-only"), "{err}");
    hub.set_engine(
        "scout",
        EngineKind::Claude,
        Some("haiku".into()),
        &owner,
        true,
    )
    .await
    .unwrap();
    let scout = hub.bot("scout").unwrap();
    assert_eq!(scout.engine, EngineKind::Claude);
    assert_eq!(scout.model.as_deref(), Some("haiku"));
    let file = std::fs::read_to_string(bots.join("scout/bot.toml")).unwrap();
    assert!(!file.contains("engine"), "claude is the default: {file}");
    // Now owner-only.
    assert!(matches!(
        hub.may_send("scout", &sam),
        Err(HubError::OwnerOnly { .. })
    ));
    // Herders' engines are fixed.
    let err = hub
        .set_engine("assistant", EngineKind::OpenRouter, None, &owner, true)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Herder's engine is fixed.");
}
