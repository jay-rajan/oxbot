//! Runs the hub against the fake `claude` scripts from oxbot-claude's tests.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_browser::{Tools, ToolsConfig};
use oxbot_core::{Bot, BotId, Hub, HubConfig, HubError, Store, Viewer};
use oxbot_protocol::{
    Answer, Approval, ApprovalKind, EventKind, Outcome, ServerMsg, SignInOutcome,
};
use tokio::sync::broadcast::Receiver;

fn claude_tests() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../oxbot-claude/tests")
}

fn fake_hub(dir: &Path) -> Arc<Hub> {
    let bot = Bot::create(
        dir,
        BotId::new("assistant").unwrap(),
        "Assistant",
        "Be brief.",
    )
    .unwrap();
    let config = HubConfig {
        claude_program: claude_tests().join("fake-claude.sh"),
        extra_env: vec![(
            "FAKE_CLAUDE_FIXTURE".into(),
            claude_tests()
                .join("fixtures/tool-use.jsonl")
                .display()
                .to_string(),
        )],
        ..HubConfig::new(dir)
    };
    Hub::new(vec![bot], Store::open_in_memory().unwrap(), config)
}

/// A hub whose bot asks to upload a CV. Chrome is "missing", but the
/// browser tool looks installed, so the rules run as they would for real.
fn asking_hub(dir: &Path) -> Arc<Hub> {
    asking_hub_with(dir, "upload", &["poster"])
}

/// Like [`asking_hub`], with the fake's `FAKE_TOOL` and more bots.
fn asking_hub_with(dir: &Path, tool: &str, ids: &[&str]) -> Arc<Hub> {
    let mut tools = ToolsConfig::in_home(&dir.join("home"));
    tools.chrome = PathBuf::from("/no/chrome");
    let package = tools.tools_dir.join("node_modules/chrome-devtools-mcp");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("package.json"), r#"{"version":"1.10.1"}"#).unwrap();
    std::fs::create_dir_all(tools.tools_dir.join("node_modules/.bin")).unwrap();
    std::fs::write(tools.mcp_program(), "").unwrap();

    let bots = dir.join("bots");
    std::fs::create_dir_all(&bots).unwrap();
    let all = ids
        .iter()
        .map(|id| Bot::create(&bots, BotId::new(*id).unwrap(), id, "").unwrap())
        .collect();
    let config = HubConfig {
        claude_program: claude_tests().join("fake-claude-perm.sh"),
        browser: Some(Tools::new(tools)),
        approval_timeout: Duration::from_secs(5),
        sign_in_timeout: Duration::from_secs(5),
        extra_env: vec![("FAKE_TOOL".into(), tool.into())],
        ..HubConfig::new(&bots)
    };
    Hub::new(all, Store::open_in_memory().unwrap(), config)
}

async fn wait_for_turn_done(updates: &mut Receiver<ServerMsg>) {
    loop {
        if let ServerMsg::Event(e) = updates.recv().await.unwrap()
            && matches!(e.kind, EventKind::TurnDone { .. })
        {
            return;
        }
    }
}

#[tokio::test]
async fn a_message_becomes_a_stored_conversation() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = fake_hub(tmp.path());
    let mut updates = hub.subscribe();

    hub.send("assistant", "save a note", Some("Jay"))
        .await
        .unwrap();
    wait_for_turn_done(&mut updates).await;

    let kinds: Vec<_> = hub
        .history("assistant", 0)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(
        kinds,
        [
            EventKind::UserMessage {
                text: "save a note".into(),
                from: Some("Jay".into()),
            },
            EventKind::AssistantMessage {
                text: "I'll save that note.".into()
            },
            EventKind::ToolUse {
                name: "Write".into(),
                summary: "notes.md".into()
            },
            EventKind::AssistantMessage {
                text: "Saved to notes.md.".into()
            },
            EventKind::TurnDone {
                ok: true,
                duration_ms: 2300
            },
        ]
    );
    assert!(!hub.bots().await.unwrap()[0].busy);

    // A second turn reuses the same child and continues the numbering.
    hub.send("assistant", "again", Some("Anu")).await.unwrap();
    wait_for_turn_done(&mut updates).await;
    assert_eq!(hub.history("assistant", 5).unwrap()[0].seq, 6);

    // Each person keeps their own color.
    let people = hub.people().unwrap();
    let names: Vec<_> = people.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Jay", "Anu"]);
    assert_ne!(people[0].color, people[1].color);
    hub.shutdown().await;
}

#[tokio::test]
async fn unknown_bots_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = fake_hub(tmp.path());
    assert!(matches!(
        hub.send("nobody", "hi", None).await,
        Err(HubError::UnknownBot(_))
    ));
    assert!(matches!(
        hub.send("../x", "hi", None).await,
        Err(HubError::UnknownBot(_))
    ));
}

#[tokio::test]
async fn new_bots_get_ids_from_their_names_and_work_without_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = fake_hub(tmp.path());
    let mut updates = hub.subscribe();

    let info = hub
        .create_bot("Daily News", "Summarize the news.", Some("Anu"))
        .await
        .unwrap();
    assert_eq!(info.id, "daily-news");
    assert_eq!(
        info.created_by.as_deref(),
        Some("Anu"),
        "the creator is kept"
    );
    assert!(tmp.path().join("daily-news/instructions.md").exists());
    assert!(matches!(updates.recv().await.unwrap(), ServerMsg::Bots { bots } if bots.len() == 2));

    let again = hub.create_bot("Daily News", "", None).await.unwrap();
    assert_eq!(again.id, "daily-news-2");

    hub.send("daily-news-2", "anything new?", None)
        .await
        .unwrap();
    wait_for_turn_done(&mut updates).await;
    assert_eq!(hub.history("daily-news-2", 0).unwrap().len(), 5);
    hub.shutdown().await;
}

/// Sends a message and returns the approval card that comes up.
async fn card(hub: &Arc<Hub>, updates: &mut Receiver<ServerMsg>) -> Approval {
    hub.send("poster", "upload my CV", Some("Jay"))
        .await
        .unwrap();
    loop {
        if let ServerMsg::ApprovalNeeded(a) =
            tokio::time::timeout(Duration::from_secs(10), updates.recv())
                .await
                .unwrap()
                .unwrap()
        {
            return a;
        }
    }
}

/// The bot's final words and the red line, once the turn is done.
async fn ending(hub: &Arc<Hub>, updates: &mut Receiver<ServerMsg>) -> (String, Option<Outcome>) {
    wait_for_turn_done(updates).await;
    let history = hub.history("poster", 0).unwrap();
    let said = history
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::AssistantMessage { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    let outcome = history.iter().rev().find_map(|e| match &e.kind {
        EventKind::Approval { outcome, .. } => Some(*outcome),
        _ => None,
    });
    (said, outcome)
}

#[tokio::test]
async fn an_upload_waits_for_a_yes_and_no_means_no() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub(tmp.path());
    let mut updates = hub.subscribe();

    let approval = card(&hub, &mut updates).await;
    assert_eq!(approval.kind, ApprovalKind::Upload);
    assert_eq!(approval.target, "button \"Upload CV\"");
    assert_eq!(approval.site, "jobs.example.com");
    assert_eq!(approval.page.as_deref(), Some("Jobs"));
    assert!(approval.detail.unwrap().ends_with("/poster/work/cv.pdf"));
    assert_eq!(hub.pending_approvals().len(), 1);

    assert!(hub.answer(approval.id, Answer::Deny));
    let (said, outcome) = ending(&hub, &mut updates).await;
    assert_eq!(said, "behavior=deny");
    assert_eq!(outcome, Some(Outcome::Denied));
    assert_eq!(hub.pending_approvals(), []);

    let approval = card(&hub, &mut updates).await;
    hub.answer(approval.id, Answer::AllowOnce);
    let (said, outcome) = ending(&hub, &mut updates).await;
    assert_eq!(said, "behavior=allow");
    assert_eq!(outcome, Some(Outcome::Allowed));
    hub.shutdown().await;
}

#[tokio::test]
async fn an_unanswered_card_is_a_no() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub(tmp.path());
    let mut updates = hub.subscribe();
    let _ = card(&hub, &mut updates).await;
    let (said, outcome) = tokio::time::timeout(Duration::from_secs(15), ending(&hub, &mut updates))
        .await
        .unwrap();
    assert_eq!(said, "behavior=deny");
    assert_eq!(outcome, Some(Outcome::TimedOut));
    hub.shutdown().await;
}

#[tokio::test]
async fn usage_is_kept_per_bot_with_the_plan_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub(tmp.path());
    let mut updates = hub.subscribe();
    let approval = card(&hub, &mut updates).await;
    hub.answer(approval.id, Answer::AllowOnce);
    let _ = ending(&hub, &mut updates).await;

    let usage = hub.usage().unwrap();
    assert_eq!(usage.bots.len(), 1);
    assert_eq!(
        (usage.bots[0].bot.as_str(), usage.bots[0].turns),
        ("poster", 1)
    );
    assert_eq!(usage.bots[0].output_tokens, 7);
    let limits: Vec<_> = usage
        .limits
        .iter()
        .map(|l| (l.name.as_str(), l.used))
        .collect();
    assert_eq!(limits, [("five_hour", 0.25), ("seven_day", 0.5)]);
    hub.shutdown().await;
}

async fn next<T>(
    updates: &mut Receiver<ServerMsg>,
    mut pick: impl FnMut(ServerMsg) -> Option<T>,
) -> T {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), updates.recv())
            .await
            .unwrap()
            .unwrap();
        if let Some(found) = pick(msg) {
            return found;
        }
    }
}

#[tokio::test]
async fn clicking_post_asks_with_a_red_card() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "post", &["poster"]);
    let mut updates = hub.subscribe();
    hub.send("poster", "post my update", Some("Jay"))
        .await
        .unwrap();
    let approval = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    assert_eq!(approval.kind, ApprovalKind::Change);
    assert_eq!(
        (approval.action.as_str(), approval.target.as_str()),
        ("click", "button \"Post\"")
    );
    hub.answer(approval.id, Answer::Deny);
    let (said, outcome) = ending(&hub, &mut updates).await;
    assert_eq!(said, "behavior=deny");
    assert_eq!(outcome, Some(Outcome::Denied));
    hub.shutdown().await;
}

#[tokio::test]
async fn a_sign_in_request_waits_for_the_user() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "signin", &["poster"]);
    let mut updates = hub.subscribe();

    hub.send("poster", "read my mail", Some("Jay"))
        .await
        .unwrap();
    let request = next(&mut updates, |m| match m {
        ServerMsg::SignInNeeded(r) => Some(r),
        _ => None,
    })
    .await;
    assert_eq!(request.site, "mail.example.com");
    assert_eq!(request.why, "to read your mail");
    // Chrome is missing here, so the window couldn't open: the card offers the button.
    assert!(!request.window_open);
    assert_eq!(hub.pending_sign_ins().len(), 1);
    assert!(hub.resolve_sign_in(request.id, true));
    let (said, _) = ending(&hub, &mut updates).await;
    assert_eq!(said, "signed_in=true");
    let signed = hub.history("poster", 0).unwrap().into_iter().any(|e| {
        matches!(
            e.kind,
            EventKind::SignIn {
                outcome: SignInOutcome::Done,
                ..
            }
        )
    });
    assert!(signed, "the chat shows the sign-in");

    // Cancelling tells the bot it isn't signed in.
    hub.send("poster", "again", Some("Jay")).await.unwrap();
    let request = next(&mut updates, |m| match m {
        ServerMsg::SignInNeeded(r) => Some(r),
        _ => None,
    })
    .await;
    hub.resolve_sign_in(request.id, false);
    let (said, _) = ending(&hub, &mut updates).await;
    assert_eq!(said, "signed_in=false");
    assert_eq!(hub.pending_sign_ins(), []);
    hub.shutdown().await;
}

#[tokio::test]
async fn bots_take_turns_on_the_shared_browser() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "upload", &["first", "second"]);
    let mut updates = hub.subscribe();

    hub.send("first", "upload", Some("Jay")).await.unwrap();
    let first = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    assert_eq!(first.bot, "first");

    // The second bot waits for the browser while the first holds it.
    hub.send("second", "upload", Some("Anu")).await.unwrap();
    let waiting = next(&mut updates, |m| match m {
        ServerMsg::Activity { bot, text }
            if bot == "second" && text.starts_with("Waiting for the browser") =>
        {
            Some(text)
        }
        _ => None,
    })
    .await;
    assert_eq!(waiting, "Waiting for the browser (first is using it)…");
    assert_eq!(
        hub.pending_approvals().len(),
        1,
        "no card for the second bot yet"
    );

    // When the first bot's turn ends, the second gets the browser.
    hub.answer(first.id, Answer::AllowOnce);
    let second = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    assert_eq!(second.bot, "second");
    hub.answer(second.id, Answer::Deny);
    hub.shutdown().await;
}

#[tokio::test]
async fn signing_out_is_refused_while_a_bot_uses_the_browser() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub(tmp.path());
    let mut updates = hub.subscribe();
    let approval = card(&hub, &mut updates).await;
    assert!(matches!(
        hub.sign_out_all(oxbot_core::store::HOME).await,
        Err(HubError::BrowserBusy(_))
    ));
    hub.answer(approval.id, Answer::Deny);
    let _ = ending(&hub, &mut updates).await;
    hub.sign_out_all(oxbot_core::store::HOME).await.unwrap();
    hub.shutdown().await;
}

/// Waits for `bot`'s turn to end and returns its last reply.
async fn reply_of(hub: &Arc<Hub>, updates: &mut Receiver<ServerMsg>, bot: &str) -> String {
    next(updates, |m| match m {
        ServerMsg::Event(e) if e.bot == bot && matches!(e.kind, EventKind::TurnDone { .. }) => {
            Some(())
        }
        _ => None,
    })
    .await;
    hub.history(bot, 0)
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
async fn the_assistant_creates_a_bot_only_after_the_users_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "createbot", &["assistant"]);
    let mut updates = hub.subscribe();

    hub.send("assistant", "make me a mail bot", Some("Jay"))
        .await
        .unwrap();
    let card = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    assert_eq!(card.kind, ApprovalKind::CreateBot);
    assert_eq!(card.target, "Mail Checker");
    assert_eq!(card.detail.as_deref(), Some("Check my mail."));
    hub.answer(card.id, Answer::Deny);
    assert_eq!(
        reply_of(&hub, &mut updates, "assistant").await,
        "behavior=deny"
    );
    assert_eq!(hub.bots().await.unwrap().len(), 1, "no means no bot");

    hub.send("assistant", "please, make it", Some("Jay"))
        .await
        .unwrap();
    let card = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    hub.answer(card.id, Answer::AllowOnce);
    let said = reply_of(&hub, &mut updates, "assistant").await;
    assert!(said.starts_with("tool_ok=true Created"), "{said}");
    let bots = hub.bots().await.unwrap();
    let made = bots
        .iter()
        .find(|b| b.name == "Mail Checker")
        .expect("created");
    assert!(!made.central);
    assert!(bots.iter().any(|b| b.id == "assistant" && b.central));
    hub.shutdown().await;
}

#[tokio::test]
async fn the_assistant_gives_a_task_and_other_bots_cant() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "task", &["assistant", "helper"]);
    let mut updates = hub.subscribe();

    hub.send("assistant", "ask the helper to say hi", Some("Jay"))
        .await
        .unwrap();
    let said = reply_of(&hub, &mut updates, "assistant").await;
    // The helper (the same fake) tried to give a task too, and was refused.
    assert!(said.starts_with("tool_ok=true helper replied:"), "{said}");
    assert!(said.contains("tool_ok=false"), "{said}");
    let from_assistant = hub.history("helper", 0).unwrap().into_iter().any(|e| {
        matches!(e.kind, EventKind::UserMessage { ref text, ref from }
            if text == "say hi" && from.as_deref() == Some("assistant"))
    });
    assert!(
        from_assistant,
        "the task shows in the helper's chat, from Herder"
    );
    hub.shutdown().await;
}

fn ids(bots: &[oxbot_protocol::BotInfo]) -> Vec<&str> {
    bots.iter().map(|b| b.id.as_str()).collect()
}

#[tokio::test]
async fn a_private_bot_is_seen_only_by_its_maker_and_the_farm() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "upload", &["assistant"]);
    let bank = hub
        .create_bot_with("Bank", "Check my bank.", Some("Sam"), true)
        .await
        .unwrap();
    assert!(bank.private);
    hub.create_bot_with("Recipes", "", Some("Sam"), false)
        .await
        .unwrap();
    let (sam, ann, farm) = (
        Viewer::gate("Sam"),
        Viewer::gate("Ann"),
        Viewer::farm("Jay"),
    );

    // Ann sees its name and lock, but not what it does, and can't open it.
    assert_eq!(ids(&hub.bots_for(&ann).await.unwrap()).len(), 3);
    assert_eq!(ids(&hub.bots_for(&sam).await.unwrap()).len(), 3);
    assert_eq!(ids(&hub.bots_for(&farm).await.unwrap()).len(), 3);
    // To Ann it's as if it didn't exist.
    assert!(matches!(
        hub.bot_for("bank", &ann),
        Err(HubError::UnknownBot(_))
    ));
    assert!(hub.history_for("bank", 0, &ann).is_err());
    assert!(hub.history_for("bank", 0, &sam).is_ok());
    assert!(hub.history_for("bank", 0, &farm).is_ok());
    hub.shutdown().await;
}

#[tokio::test]
async fn only_the_maker_or_the_farm_can_change_or_delete_a_bot() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "upload", &["assistant"]);
    hub.create_bot_with("Recipes", "", Some("Sam"), false)
        .await
        .unwrap();
    let (sam, ann, farm) = (
        Viewer::gate("Sam"),
        Viewer::gate("Ann"),
        Viewer::farm("Jay"),
    );

    assert!(matches!(
        hub.set_private("recipes", true, &ann).await,
        Err(HubError::NotAllowed(_))
    ));
    assert!(matches!(
        hub.delete_bot("recipes", &ann).await,
        Err(HubError::NotAllowed(_))
    ));
    hub.set_private("recipes", true, &sam).await.unwrap();
    assert!(hub.bot("recipes").unwrap().private);
    assert!(hub.bot_for("recipes", &ann).is_err(), "locked to Ann");
    hub.rename_bot("recipes", "Sam's Recipes", &sam)
        .await
        .unwrap();
    assert_eq!(hub.bot("recipes").unwrap().name, "Sam's Recipes");
    // One line, not too long, not taken.
    for bad in ["two\nlines", &"x".repeat(61), "herder"] {
        assert!(
            matches!(
                hub.rename_bot("recipes", bad, &sam).await,
                Err(HubError::NotAllowed(_))
            ),
            "{bad:?}"
        );
    }

    // Herder belongs to everyone.
    assert!(hub.set_private("assistant", true, &farm).await.is_err());
    assert!(hub.delete_bot("assistant", &farm).await.is_err());
    assert!(hub.rename_bot("assistant", "Boss", &farm).await.is_err());

    // The Farm can delete anyone's bot, private or not; its files go to the trash.
    let mut updates = hub.subscribe();
    hub.send("recipes", "hi", Some("Sam")).await.unwrap();
    let card = next(&mut updates, |m| match m {
        ServerMsg::ApprovalNeeded(a) => Some(a),
        _ => None,
    })
    .await;
    assert!(
        matches!(
            hub.delete_bot("recipes", &farm).await,
            Err(HubError::Busy(_))
        ),
        "not while it's working"
    );
    hub.answer(card.id, Answer::Deny);
    let _ = reply_of(&hub, &mut updates, "recipes").await;
    hub.delete_bot("recipes", &farm).await.unwrap();
    assert!(hub.bot("recipes").is_err());
    assert!(hub.history("recipes", 0).is_err());
    let trash = tmp.path().join("trash");
    let moved: Vec<_> = std::fs::read_dir(&trash).unwrap().flatten().collect();
    assert_eq!(moved.len(), 1, "the folder is in the trash");
    let kept = std::fs::read_to_string(moved[0].path().join("history.jsonl")).unwrap();
    assert!(kept.contains("\"hi\""), "its history went with it: {kept}");
    // A new bot with the same name starts with an empty history.
    hub.create_bot("Recipes", "", Some("Ann")).await.unwrap();
    assert_eq!(hub.history("recipes", 0).unwrap().len(), 0);
    hub.shutdown().await;
}

#[tokio::test]
async fn herder_never_relays_a_private_bot() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "task", &["assistant", "helper"]);
    hub.set_private("helper", true, &Viewer::farm("Sam"))
        .await
        .unwrap();
    assert_eq!(
        hub.bot("helper").unwrap().created_by.as_deref(),
        Some("Sam")
    );
    let mut updates = hub.subscribe();

    hub.send("assistant", "ask the helper to say hi", Some("Ann"))
        .await
        .unwrap();
    let said = reply_of(&hub, &mut updates, "assistant").await;
    assert!(
        said.starts_with("tool_ok=false helper is private to Sam"),
        "{said}"
    );
    assert!(
        hub.history("helper", 0).unwrap().is_empty(),
        "nothing reached it"
    );

    // Not even for its maker: the reply would land in Herder's chat, which
    // everyone can read.
    hub.send("assistant", "ask the helper to say hi", Some("Sam"))
        .await
        .unwrap();
    let said = reply_of(&hub, &mut updates, "assistant").await;
    assert!(
        said.starts_with("tool_ok=false helper is private. Herder doesn't pass"),
        "{said}"
    );
    assert!(
        hub.history("helper", 0).unwrap().is_empty(),
        "nothing reached it"
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn herder_lists_only_the_bots_the_asker_may_see() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "list", &["assistant"]);
    hub.create_bot_with("Bank", "", Some("Sam"), true)
        .await
        .unwrap();
    hub.create_bot_with("Recipes", "", Some("Sam"), false)
        .await
        .unwrap();
    let mut updates = hub.subscribe();

    hub.send("assistant", "what bots are there?", Some("Ann"))
        .await
        .unwrap();
    let said = reply_of(&hub, &mut updates, "assistant").await;
    assert!(said.contains("Recipes (id recipes), made by Sam"), "{said}");
    assert!(
        said.contains("Bank (id bank): private to Sam, locked"),
        "{said}"
    );

    hub.send("assistant", "what bots are there?", Some("Sam"))
        .await
        .unwrap();
    let said = reply_of(&hub, &mut updates, "assistant").await;
    assert!(said.contains("Bank (id bank), private to Sam"), "{said}");
    hub.shutdown().await;
}

#[tokio::test]
async fn only_a_listed_site_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = asking_hub_with(tmp.path(), "upload", &["assistant"]);
    assert!(matches!(
        hub.open_site("evil.example/phish", oxbot_core::store::HOME)
            .await,
        Err(HubError::BadSite(_))
    ));
    hub.shutdown().await;
}
