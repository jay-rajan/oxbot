//! Messages sent while a bot works go into its running task (stand-in bots
//! on Codex, as in `hub.rs`; skipped, saying why, on a Mac without Codex
//! 0.148): a sign-in or an approval request it waits on ends, and the bot
//! reads the message; "stop" stops it; someone else's message waits and
//! starts as the next turn.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{Hub, Store};
use oxbot_protocol::{EventKind, Outcome, ServerMsg, SignInOutcome};
use tokio::sync::OnceCell;
use tokio::sync::broadcast::Receiver;

static CODEX: OnceCell<Option<PathBuf>> = OnceCell::const_new();

/// The `codex` program, if this Mac has Codex 0.148.
async fn codex() -> Option<&'static Path> {
    CODEX
        .get_or_init(|| async {
            let probe =
                std::env::temp_dir().join(format!("oxbot-steer-tests-{}", std::process::id()));
            testing::codex(&probe).await
        })
        .await
        .as_deref()
}

/// A hub whose `poster` takes `tools` for its messages in turn (then
/// answers `ok`), without reading the page first. Cards and sign-ins wait
/// long enough for a test to write meanwhile.
async fn hub(dir: &Path, codex: &Path, tools: Vec<Tool>) -> (Arc<Hub>, Arc<FakeBots>) {
    let bots = dir.join("bots");
    let all = testing::bots(&bots, &["poster"]);
    let fake = Arc::new(FakeBots {
        tools,
        no_page: true,
        ..FakeBots::new(&bots, Tool::None)
    });
    let mut config = testing::config(&bots, &dir.join("home"), &fake, Some(codex));
    config.approval_timeout = Duration::from_secs(60);
    config.sign_in_timeout = Duration::from_secs(60);
    let hub = Hub::new(all, Store::open_in_memory().unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    (hub, fake)
}

async fn next<T>(
    updates: &mut Receiver<ServerMsg>,
    mut pick: impl FnMut(ServerMsg) -> Option<T>,
) -> T {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), updates.recv())
            .await
            .unwrap()
            .unwrap();
        if let Some(found) = pick(msg) {
            return found;
        }
    }
}

async fn turn_done(updates: &mut Receiver<ServerMsg>) -> bool {
    next(updates, |m| match m {
        ServerMsg::Event(e) => match e.kind {
            EventKind::TurnDone { ok, .. } => Some(ok),
            _ => None,
        },
        _ => None,
    })
    .await
}

/// What the chat shows, in order: `you: …`, `bot: …`, `done`, and the
/// cards' and sign-ins' outcomes.
fn chat(hub: &Arc<Hub>) -> Vec<String> {
    hub.history("poster", 0)
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.kind {
            EventKind::UserMessage { text, from, .. } => {
                Some(format!("{}: {text}", from.unwrap_or_default()))
            }
            EventKind::AssistantMessage { text } => Some(format!("bot: {text}")),
            EventKind::TurnDone { ok, .. } => Some(format!("done ok={ok}")),
            EventKind::Approval { outcome, .. } => Some(format!("card {outcome:?}")),
            EventKind::SignIn { outcome, .. } => Some(format!("sign-in {outcome:?}")),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_message_during_a_sign_in_cancels_it_and_goes_into_the_task() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = codex().await else { return };
    let (hub, fake) = hub(tmp.path(), codex, vec![Tool::SignIn]).await;
    let mut updates = hub.subscribe();
    hub.send("poster", "read my mail", Some("Jay"))
        .await
        .unwrap();
    next(&mut updates, |m| match m {
        ServerMsg::SignInNeeded(r) => Some(r),
        _ => None,
    })
    .await;
    // No "still working": it goes in.
    hub.send("poster", "skip the mail, just say ok", Some("Jay"))
        .await
        .unwrap();
    assert!(turn_done(&mut updates).await);
    assert_eq!(hub.pending_sign_ins(), []);
    assert_eq!(
        chat(&hub),
        [
            "Jay: read my mail",
            "Jay: skip the mail, just say ok",
            &format!("sign-in {:?}", SignInOutcome::Cancelled),
            "bot: ok",
            "done ok=true",
        ]
    );
    assert_eq!(
        fake.told().unwrap(),
        // Names are replaced on the way to the model.
        "[[person 1]] skip the mail, just say ok",
        "the bot read it in the same task"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn a_message_during_an_approval_request_says_no_and_goes_into_the_task() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = codex().await else { return };
    let (hub, fake) = hub(tmp.path(), codex, vec![Tool::Upload]).await;
    let mut updates = hub.subscribe();
    hub.send("poster", "upload my CV", Some("Jay"))
        .await
        .unwrap();
    let card = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    hub.send("poster", "don't upload it", Some("Jay"))
        .await
        .unwrap();
    // The card goes away on every screen.
    next(&mut updates, |m| match m {
        ServerMsg::ApprovalResolved { id } if id == card.id => Some(()),
        _ => None,
    })
    .await;
    assert!(turn_done(&mut updates).await);
    assert_eq!(hub.pending_approvals(), []);
    let shown = chat(&hub);
    assert_eq!(
        shown,
        [
            "Jay: upload my CV",
            "Jay: don't upload it",
            &format!("card {:?}", Outcome::Denied),
            "bot: ok",
            "done ok=true",
        ]
    );
    assert_eq!(fake.told().unwrap(), "[[person 1]] don't upload it");
    // What the bot heard about the card: read the message, not "stop".
    let heard = fake
        .heard()
        .iter()
        .any(|r| r.contains("they sent a new message instead"));
    assert!(heard, "the no says why");
    hub.shutdown().await;
}

#[tokio::test]
async fn stop_alone_stops_the_task() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = codex().await else { return };
    let (hub, _) = hub(tmp.path(), codex, vec![Tool::Upload]).await;
    let mut updates = hub.subscribe();
    hub.send("poster", "upload my CV", Some("Jay"))
        .await
        .unwrap();
    next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    hub.send("poster", "  Stop! ", Some("Jay")).await.unwrap();
    assert!(!turn_done(&mut updates).await, "stopped");
    assert_eq!(hub.pending_approvals(), []);
    // Nothing more starts.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let shown = chat(&hub);
    assert_eq!(shown.iter().filter(|l| l.starts_with("done")).count(), 1);
    assert!(shown.contains(&"Jay:   Stop! ".to_owned()), "{shown:?}");
    hub.shutdown().await;
}

#[tokio::test]
async fn someone_elses_message_waits_and_runs_next() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(codex) = codex().await else { return };
    let (hub, fake) = hub(tmp.path(), codex, vec![Tool::Upload, Tool::None]).await;
    let mut updates = hub.subscribe();
    hub.send("poster", "upload my CV", Some("Jay"))
        .await
        .unwrap();
    let card = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    // Sam's message never answers Jay's card: it waits its turn.
    hub.send("poster", "what's the time?", Some("Sam"))
        .await
        .unwrap();
    assert_eq!(hub.pending_approvals().len(), 1);
    assert!(hub.answer(card.id, oxbot_protocol::Answer::Deny));
    assert!(turn_done(&mut updates).await);
    assert!(turn_done(&mut updates).await, "the waiting message ran");
    assert_eq!(
        chat(&hub),
        [
            "Jay: upload my CV",
            "Sam: what's the time?",
            &format!("card {:?}", Outcome::Denied),
            "bot: behavior=deny",
            "done ok=true",
            "bot: ok",
            "done ok=true",
        ]
    );
    assert_eq!(
        fake.told().unwrap().lines().next(),
        Some("[[person 2]] what's the time?")
    );
    hub.shutdown().await;
}
