//! Answers taken from the web link their pages (stand-in bots on Codex, as
//! in `hub.rs`; skipped, saying why, on a Mac without Codex 0.148): one
//! without links goes back once with the pages read, and if it still has
//! none, oxbot lists them itself.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{Hub, Store};
use oxbot_protocol::{EventKind, ServerMsg};
use tokio::sync::OnceCell;

static CODEX: OnceCell<Option<PathBuf>> = OnceCell::const_new();

async fn codex() -> Option<&'static Path> {
    CODEX
        .get_or_init(|| async {
            let probe =
                std::env::temp_dir().join(format!("oxbot-sources-tests-{}", std::process::id()));
            testing::codex(&probe).await
        })
        .await
        .as_deref()
}

/// The stand-in's brief: five items, no links.
const BRIEF: &str =
    "This week's jobs:\n- Nurse, days\n- Nurse, nights\n- Porter\n- Cook\n- Cleaner";

/// A hub whose `scout` reads the jobs page (the stand-in browser's
/// snapshot, `https://jobs.example.com/apply`) and answers `answer`; after
/// a check, `checked` if given.
async fn scout(dir: &Path, codex: &Path, checked: Option<&str>) -> (Arc<Hub>, Arc<FakeBots>) {
    let bots = dir.join("bots");
    let all = testing::bots(&bots, &["scout"]);
    let fake = Arc::new(FakeBots {
        answer: Some(BRIEF.to_owned()),
        checked: checked.map(str::to_owned),
        ..FakeBots::new(&bots, Tool::None)
    });
    let config = testing::config(&bots, &dir.join("home"), &fake, Some(codex));
    let hub = Hub::new(all, Store::open_in_memory().unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    (hub, fake)
}

/// Sends `text` and returns the bot's answer once the turn is done.
async fn answer(hub: &Arc<Hub>, text: &str) -> String {
    let mut updates = hub.subscribe();
    hub.send("scout", text, Some("Jay")).await.unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), updates.recv())
            .await
            .unwrap()
            .unwrap();
        if let ServerMsg::Event(e) = msg
            && matches!(e.kind, EventKind::TurnDone { .. })
        {
            break;
        }
    }
    hub.history("scout", 0)
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|e| match e.kind {
            EventKind::AssistantMessage { text } => Some(text),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn an_answer_from_the_web_without_links_goes_back_then_gets_its_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = codex().await else { return };
    let (hub, fake) = scout(tmp.path(), codex, None).await;
    let said = answer(&hub, "list this week's jobs").await;
    // Sent back once, naming the page it read.
    let notes = fake.noted();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains(
            "Add a link to the page for each item you took from the web, as [title](url). Pages \
             you read: https://jobs.example.com/apply."
        ),
        "{}",
        notes[0]
    );
    // Still no link: oxbot lists the page itself.
    assert_eq!(
        said,
        format!(
            "{BRIEF}\n\n**Sources**\n- [jobs.example.com — Jobs](https://jobs.example.com/apply)"
        )
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn an_answer_that_adds_its_links_after_the_check_is_kept_as_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = codex().await else { return };
    let linked = "This week's [jobs](https://jobs.example.com/apply):\n- Nurse, days\n- Porter";
    let (hub, fake) = scout(tmp.path(), codex, Some(linked)).await;
    let said = answer(&hub, "list this week's jobs").await;
    assert_eq!(fake.noted().len(), 1);
    assert_eq!(said, linked);
    hub.shutdown().await;
}
