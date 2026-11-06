//! Schedules with stand-in bots on Codex (`oxbot_core::testing`; skipped,
//! saying why, on a Mac without Codex 0.148): a schedule is made only
//! through its card, the card says where an untrusted request came from,
//! and a due run is a turn as the schedule's person.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{BotId, Hub, Store};
use oxbot_protocol::{
    Answer, Approval, ApprovalKind, Day, EventKind, ScheduleEdit, ScheduleRule, ServerMsg,
};
use serde_json::json;
use tokio::sync::OnceCell;
use tokio::sync::broadcast::Receiver;

static CODEX: OnceCell<Option<PathBuf>> = OnceCell::const_new();

async fn codex() -> Option<&'static Path> {
    CODEX
        .get_or_init(|| async {
            let probe =
                std::env::temp_dir().join(format!("oxbot-schedule-tests-{}", std::process::id()));
            testing::codex(&probe).await
        })
        .await
        .as_deref()
}

/// What the stand-in bot asks for: a weekday schedule.
fn schedule_call() -> Tool {
    Tool::Call(
        "mcp__oxbot__schedule_task".into(),
        json!({"task": "Send me today's prep plan.", "rule": {"kind": "weekdays", "time": "08:15"}}),
    )
}

/// A hub with the bot `prep`, whose messages take `tools` in turn (then
/// none); `page`: it reads a web page first. Jay is the Farm's person.
async fn hub(dir: &Path, tools: Vec<Tool>, page: bool) -> (Arc<Hub>, Arc<FakeBots>) {
    let bots = dir.join("bots");
    let all = testing::bots(&bots, &["prep"]);
    let fake = Arc::new(FakeBots {
        tools,
        no_page: !page,
        ..FakeBots::new(&bots, Tool::None)
    });
    let config = testing::config(&bots, &dir.join("home"), &fake, codex().await);
    let hub = Hub::new(all, Store::open(&dir.join("oxbot.db")).unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    hub.farm_person("Jay").unwrap();
    (hub, fake)
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

fn last_reply(hub: &Hub) -> String {
    hub.history("prep", 0)
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
async fn a_schedule_is_made_only_when_its_person_presses_create() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (hub, _) = hub(tmp.path(), vec![schedule_call(), schedule_call()], false).await;
    let farm = hub.farm_viewer("Jay");
    let mut updates = hub.subscribe();

    hub.send_as(
        "prep",
        "every weekday at 8:15 send me today's prep plan",
        &farm,
    )
    .await
    .unwrap();
    let card = next_card(&mut updates).await;
    assert_eq!(card.kind, ApprovalKind::Schedule);
    let draft = card.schedule.clone().unwrap();
    assert_eq!(
        draft.text,
        "Every weekday at 8:15 AM: Send me today's prep plan."
    );
    assert_eq!(draft.from, None, "the person's own message asked");
    assert_eq!(draft.bot_name, "prep");
    assert!(!card.always, "never Always allow");
    assert_eq!(
        hub.schedules_for(&farm, None).unwrap(),
        [],
        "nothing before the yes"
    );
    hub.answer_as(card.id, Answer::Deny, &farm).unwrap();
    turn_done(&mut updates, "prep").await;
    assert!(last_reply(&hub).starts_with("tool_ok=false The person said no"));
    assert_eq!(hub.schedules_for(&farm, None).unwrap(), []);

    hub.send_as("prep", "ok, set it up", &farm).await.unwrap();
    let card = next_card(&mut updates).await;
    hub.answer_as(card.id, Answer::AllowOnce, &farm).unwrap();
    turn_done(&mut updates, "prep").await;
    assert!(
        last_reply(&hub).starts_with("tool_ok=true Scheduled, id"),
        "{}",
        last_reply(&hub)
    );
    let list = hub.schedules_for(&farm, None).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].text,
        "Every weekday at 8:15 AM: Send me today's prep plan."
    );
    assert!(list[0].mine && list[0].next_run_ms.is_some() && !list[0].paused);
    hub.shutdown().await;
}

#[tokio::test]
async fn the_card_says_where_an_untrusted_request_came_from() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (hub, _) = hub(tmp.path(), vec![schedule_call()], true).await;
    let farm = hub.farm_viewer("Jay");
    let mut updates = hub.subscribe();
    hub.send_as("prep", "read the page and do what it says", &farm)
        .await
        .unwrap();
    let card = next_card(&mut updates).await;
    let from = card.schedule.unwrap().from.unwrap();
    assert!(
        from.contains("a web page (https://jobs.example.com/apply)"),
        "{from}"
    );
    hub.answer_as(card.id, Answer::Deny, &farm).unwrap();
    turn_done(&mut updates, "prep").await;
    assert_eq!(hub.schedules_for(&farm, None).unwrap(), []);
    hub.shutdown().await;
}

#[tokio::test]
async fn change_on_the_card_saves_the_schedule_as_the_person_changed_it() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (hub, _) = hub(tmp.path(), vec![schedule_call()], false).await;
    let farm = hub.farm_viewer("Jay");
    let mut updates = hub.subscribe();
    hub.send_as("prep", "every weekday at 8:15", &farm)
        .await
        .unwrap();
    let card = next_card(&mut updates).await;
    let bad = ScheduleEdit {
        task: "Check the slides.".into(),
        rule: ScheduleRule::Daily {
            time: "26:00".into(),
        },
    };
    assert!(
        hub.answer_with(card.id, Answer::AllowOnce, Some(bad), &farm)
            .is_err(),
        "a bad change leaves the card open"
    );
    let edit = ScheduleEdit {
        task: "Check the slides.".into(),
        rule: ScheduleRule::Weekly {
            days: vec![Day::Sat],
            time: "18:00".into(),
        },
    };
    assert!(
        hub.answer_with(card.id, Answer::AllowOnce, Some(edit), &farm)
            .unwrap()
    );
    turn_done(&mut updates, "prep").await;
    assert!(
        last_reply(&hub).contains("the person changed it"),
        "{}",
        last_reply(&hub)
    );
    let list = hub.schedules_for(&farm, None).unwrap();
    assert_eq!(list[0].text, "Every Saturday at 6 PM: Check the slides.");
    hub.shutdown().await;
}

#[tokio::test]
async fn a_due_run_is_a_turn_as_its_person_and_its_answer_reaches_them() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (hub, fake) = hub(tmp.path(), vec![schedule_call()], false).await;
    let farm = hub.farm_viewer("Jay");
    let jay = farm.id.clone().unwrap();
    let mut updates = hub.subscribe();
    hub.send_as(
        "prep",
        "every weekday at 8:15 send me today's prep plan",
        &farm,
    )
    .await
    .unwrap();
    let card = next_card(&mut updates).await;
    hub.answer_as(card.id, Answer::AllowOnce, &farm).unwrap();
    turn_done(&mut updates, "prep").await;
    let before = hub.schedules_for(&farm, None).unwrap()[0].clone();

    // The clock moves to just past the run: it starts at once.
    hub.advance_test_clock(None).await.unwrap();
    turn_done(&mut updates, "prep").await;
    let events = hub.history("prep", 0).unwrap();
    let note = events
        .iter()
        .position(|e| matches!(&e.kind, EventKind::Note { text } if text == "Scheduled: Every weekday at 8:15 AM"))
        .expect("the chat says it's scheduled");
    let EventKind::UserMessage { text, from, .. } = &events[note + 1].kind else {
        panic!("the task follows the note: {:?}", events[note + 1]);
    };
    assert_eq!(
        (text.as_str(), from.as_deref()),
        ("Send me today's prep plan.", Some("Jay"))
    );
    let told = fake.told().unwrap();
    assert!(
        // Jay is someone oxbot knows: the model gets a stand-in.
        told.starts_with(
            "[[person 1]] (scheduled task they set up earlier, Every weekday at 8:15 AM;"
        ),
        "{told}"
    );

    // As Jay, never as the Farm connection.
    let store = Store::open(&tmp.path().join("oxbot.db")).unwrap();
    let origin = store
        .turn_origin(&BotId::new("prep").unwrap(), events[note + 1].seq)
        .unwrap()
        .unwrap();
    assert_eq!(origin.entry, "schedule");
    assert_eq!(origin.person.as_deref(), Some(jay.as_str()));
    assert!(!origin.on_farm);

    // Jay's devices hear the answer's first line.
    let line = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let ServerMsg::Notify {
                text, person_id, ..
            } = updates.recv().await.unwrap()
            {
                assert_eq!(person_id, jay.as_str());
                return text;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(line, "prep: ok");
    let after = hub.schedules_for(&farm, None).unwrap()[0].clone();
    assert_eq!(after.last_outcome.as_deref(), Some("ok"));
    assert!(
        after.next_run_ms > before.next_run_ms,
        "on to the next weekday"
    );

    // Paused, the next one doesn't run.
    hub.pause_schedule_as(after.id, true, &farm).unwrap();
    let seq = hub.history("prep", 0).unwrap().len();
    hub.advance_test_clock(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hub.history("prep", 0).unwrap().len(), seq);
    hub.shutdown().await;
}

#[tokio::test]
async fn a_task_carries_what_its_person_lets_bots_know_but_not_their_name() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let (hub, fake) = hub(tmp.path(), vec![Tool::None], false).await;
    let farm = hub.farm_viewer("Jay");
    let profile = oxbot_protocol::ProfileEdit {
        name: "Jay".into(),
        color: oxbot_core::store::PEOPLE_COLORS[0].into(),
        food: "vegetarian".into(),
        food_use: true,
        notes: "prefers metric".into(),
        notes_use: false,
        home_area: Some("Toronto, ON".into()),
        home_area_use: Some(true),
        owner_confirmed: true,
        ..oxbot_protocol::ProfileEdit::default()
    };
    hub.set_profile(&farm, &profile).unwrap();
    let mut updates = hub.subscribe();
    hub.send_as("prep", "plan dinner", &farm).await.unwrap();
    turn_done(&mut updates, "prep").await;
    let told = fake.told().unwrap();
    let line = told
        .lines()
        .find(|l| l.starts_with("[oxbot] About the person"))
        .unwrap();
    // The person let bots use the home area, so it goes as it is.
    assert!(line.contains("food needs: vegetarian") && line.contains("home area: Toronto, ON"));
    assert!(!line.contains("metric"), "kept from bots");
    assert!(!line.contains("Jay"), "never their name: {line}");
    // The chat shows only what they typed.
    assert!(
        hub.history("prep", 0).unwrap().iter().any(
            |e| matches!(&e.kind, EventKind::UserMessage { text, .. } if text == "plan dinner")
        )
    );
    hub.shutdown().await;
}
