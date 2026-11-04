//! A bot shared with picked people, with stand-in bots on Codex
//! (`oxbot_core::testing`; skipped, saying why, on a Mac without Codex
//! 0.148): its people give it tasks, from its chat or their own
//! Assistant, and answer its approval requests; nobody else does.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::bot::Sharing;
use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{Hub, Store, Viewer};
use oxbot_protocol::{Answer, Approval, EventKind, ServerMsg};
use tokio::sync::OnceCell;
use tokio::sync::broadcast::Receiver;

static CODEX: OnceCell<Option<PathBuf>> = OnceCell::const_new();

async fn codex() -> Option<&'static Path> {
    CODEX
        .get_or_init(|| async {
            let probe =
                std::env::temp_dir().join(format!("oxbot-sharing-tests-{}", std::process::id()));
            testing::codex(&probe).await
        })
        .await
        .as_deref()
}

/// Jay's bot `helper`, shared with Sam; every message to an Assistant
/// passes a task to it, and it clicks Post.
async fn hub(dir: &Path) -> (Arc<Hub>, Viewer, Viewer, Viewer) {
    let bots = dir.join("bots");
    let all = testing::bots(&bots, &["assistant"]);
    let fake = Arc::new(FakeBots {
        tools: vec![Tool::Task],
        helper_tools: vec![Tool::Post, Tool::Post],
        // Only the first message reads the page (so the click is named):
        // the Assistants after it have read nothing untrusted.
        page_once: Some(dir.join("page-read")),
        ..FakeBots::new(&bots, Tool::Task)
    });
    let config = testing::config(&bots, &dir.join("home"), &fake, codex().await);
    let hub = Hub::new(all, Store::open_in_memory().unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    let jay = hub.farm_person("Jay").unwrap();
    let jay = Viewer::person(jay.id, "Jay", false);
    let sam = hub.person_named("Sam").unwrap();
    let sam = Viewer::person(sam.id, "Sam", false);
    let ann = hub.person_named("Ann").unwrap();
    let ann = Viewer::person(ann.id, "Ann", false);
    // Fake bots are named by their ids; the one called helper posts.
    hub.create_bot_as("helper", "", Some(&jay), false)
        .await
        .unwrap();
    hub.set_sharing("helper", Sharing::People, &["Sam".into()], &jay)
        .await
        .unwrap();
    (hub, jay, sam, ann)
}

async fn next_card(updates: &mut Receiver<ServerMsg>) -> Approval {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if let ServerMsg::ApprovalNeeded(a) = updates.recv().await.unwrap() {
                return a;
            }
        }
    })
    .await
    .unwrap()
}

async fn turn_done(updates: &mut Receiver<ServerMsg>, bot: &str) {
    tokio::time::timeout(Duration::from_secs(60), async {
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

fn last_reply(hub: &Hub, bot: &str) -> String {
    hub.history(bot, 0)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|e| match e.kind {
            EventKind::AssistantMessage { text } => Some(text),
            _ => None,
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn its_people_task_it_and_answer_its_cards_and_nobody_else_does() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (hub, jay, sam, ann) = hub(tmp.path()).await;
    let mut updates = hub.subscribe();

    // Sam asks it in its own chat; its card goes to Sam and to Jay (its
    // maker), never Ann.
    hub.send_as("helper", "post it", &sam).await.unwrap();
    let card = next_card(&mut updates).await;
    assert!(hub.approval_visible(card.id, &sam));
    assert!(hub.approval_visible(card.id, &jay));
    assert!(!hub.approval_visible(card.id, &ann));
    assert!(!hub.answer_as(card.id, Answer::AllowOnce, &ann).unwrap());
    assert!(hub.answer_as(card.id, Answer::AllowOnce, &jay).unwrap());
    turn_done(&mut updates, "helper").await;
    assert_eq!(last_reply(&hub, "helper"), "behavior=allow");

    // Ann can't send it anything.
    assert!(hub.send_as("helper", "post it", &ann).await.is_err());

    // Sam's own Assistant passes it a task (it read nothing untrusted:
    // no card first).
    let sams = hub.herder_of(&sam).unwrap();
    hub.send_as(sams.as_str(), "ask helper", &sam)
        .await
        .unwrap();
    let card = next_card(&mut updates).await;
    assert_eq!(card.bot, "helper", "its own card, for the task it got");
    hub.answer_as(card.id, Answer::Deny, &sam).unwrap();
    turn_done(&mut updates, sams.as_str()).await;
    assert!(
        last_reply(&hub, sams.as_str()).starts_with("tool_ok=true helper replied"),
        "{}",
        last_reply(&hub, sams.as_str())
    );

    // Ann's Assistant can't.
    let anns = hub.herder_of(&ann).unwrap();
    hub.send_as(anns.as_str(), "ask helper", &ann)
        .await
        .unwrap();
    turn_done(&mut updates, anns.as_str()).await;
    let said = last_reply(&hub, anns.as_str());
    assert!(
        said.starts_with("tool_ok=false helper is private to Jay"),
        "{said}"
    );
    hub.shutdown().await;
}
