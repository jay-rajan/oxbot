//! Whole turns with the context tools: page maps and the page tools,
//! the to-do list, progress notes, stored results read back, the repeat
//! and failure guards, taint, and making room when a model says a request
//! is too long.

use oxbot_llm::CallUsage;

use super::*;
use crate::budget::est;
use crate::context::state::WindowState;
use crate::store::LoopStore as _;
use crate::tools::browser::BrowserOutput;
use crate::tools::tests::big_page;

fn with_page(model: ScriptedModel, page: String) -> Harness {
    Harness::with(model, |s| {
        s.browser = Some(Arc::new(SameBrowser(ScriptedBrowser::with_snapshot(page))));
    })
}

fn tool_texts(req: &oxbot_llm::ChatRequest) -> Vec<String> {
    req.messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(oxbot_llm::Message::text)
        .collect()
}

#[tokio::test]
async fn a_big_page_comes_as_a_map_and_the_page_tools_look_inside() {
    let h = with_page(
        ScriptedModel::new()
            .then(Step::tool("mcp__chrome-devtools__take_snapshot", json!({})))
            .then(Step::tool(
                "mcp__oxbot__find_in_page",
                json!({"query": "lamp 15", "role": "link"}),
            ))
            .then(Step::tool(
                "mcp__oxbot__read_section",
                json!({"uid": "1_70"}),
            ))
            // The same search again: pointed back to the first.
            .then(Step::tool(
                "mcp__oxbot__find_in_page",
                json!({"role": "link", "query": "lamp 15"}),
            ))
            .then(Step::text("Lamp 15 is $15.99.")),
        big_page(400, false),
    );
    let seen = h
        .turn(
            h.spec(new_session()),
            "[Jay] find lamp 15",
            HubScript::default(),
        )
        .await;
    assert_eq!(
        seen.last().unwrap(),
        &Seen::Done(true, "Lamp 15 is $15.99.".into())
    );
    // The gate saw every call, the page tools too.
    for tool in ["mcp__oxbot__find_in_page", "mcp__oxbot__read_section"] {
        assert!(seen.contains(&Seen::Permission(tool.into())), "{tool}");
    }
    // The hub got the whole page.
    assert!(seen.contains(&Seen::ToolResult(big_page(400, false))));
    let reqs = h.model.requests();
    let map = &tool_texts(&reqs[1])[0];
    assert!(map.contains("Page map of r1"), "{map}");
    // Before, the model got a 12,000-character preview of it.
    let raw = big_page(400, false);
    let preview: String = raw.chars().take(12_000).collect();
    assert!(
        est(map) * 4 < est(&preview),
        "{} vs {}",
        est(map),
        est(&preview)
    );
    let found = &tool_texts(&reqs[2])[1];
    assert!(
        found.starts_with("<web_page url=\"https://shop.example.com/search?q=lamp\" saved=\"r1\">"),
        "{found}"
    );
    assert!(
        found.contains("link \"Lamp 15 with a brass arm\""),
        "{found}"
    );
    let section = &tool_texts(&reqs[3])[2];
    assert!(section.contains("1_70"), "{section}");
    let again = &tool_texts(&reqs[4])[3];
    assert_eq!(
        again,
        "You already did this at step 2; the result is above."
    );
    // Page-tool results are web text, and point back to the page.
    let msgs = h.store.messages("s1").unwrap();
    let find = msgs
        .iter()
        .find(|m| m.message.text().contains("Lamp 15 with"))
        .unwrap();
    assert!(find.untrusted);
    assert_eq!(find.handle.as_deref(), Some("r1"));
    // Only the snapshot was stored.
    assert_eq!(h.store.last_result_number("s1").unwrap(), 1);
}

#[tokio::test]
async fn ask_page_asks_a_helper_with_the_bots_privacy() {
    let h = with_page(
        ScriptedModel::new()
            .then(Step::tool("mcp__chrome-devtools__take_snapshot", json!({})))
            .then(Step::tool(
                "mcp__oxbot__ask_page",
                json!({"question": "Which lamp is cheapest?"}),
            ))
            .then(Step::text("Lamp 0, $0.99 (uid=1_11)."))
            .then(Step::tool(
                "mcp__oxbot__ask_page",
                json!({"question": "Which has five stars?"}),
            ))
            .then(Step::error(ErrorKind::Refused, "flagged"))
            .then(Step::text("Lamp 0.")),
        big_page(400, false),
    );
    let mut spec = h.spec(new_session());
    spec.privacy = Privacy::Private;
    let seen = h
        .turn(spec, "[Jay] cheapest lamp", HubScript::default())
        .await;
    assert_eq!(seen.last().unwrap(), &Seen::Done(true, "Lamp 0.".into()));
    let reqs = h.model.requests();
    // The helper: the fixed prompt, the page and the question, no tools,
    // a short answer, and the private bot's provider rules.
    let helper = &reqs[2];
    assert_eq!(helper.messages[0].text(), crate::ASK_PAGE_V1);
    let input = helper.messages[1].text();
    assert!(
        input.starts_with("<web_page url=\"https://shop.example.com/search?q=lamp\" saved=\"r1\">")
    );
    assert!(input.ends_with("\n\nQuestion: Which lamp is cheapest?"));
    // Every item, not folded.
    assert!(input.contains("Lamp 399 with a brass arm"));
    assert_eq!(helper.tools, Vec::new());
    assert_eq!(helper.max_tokens, Some(800));
    assert!(helper.provider.as_ref().unwrap().denies_data_collection());
    let answer = &tool_texts(&reqs[3])[1];
    assert_eq!(
        answer,
        "<web_page url=\"https://shop.example.com/search?q=lamp\" saved=\"r1\">Lamp 0, $0.99 (uid=1_11).</web_page>"
    );
    // A helper that fails is a short tool error.
    let failed = &tool_texts(&reqs[5])[2];
    assert!(
        failed.starts_with("[tool error] ask_page couldn't get an answer"),
        "{failed}"
    );
    assert!(failed.contains("Try find_in_page instead."));
    let kinds: Vec<_> = h.store.calls().iter().map(|c| c.kind).collect();
    assert_eq!(kinds[2], oxbot_engine::CallKind::PageQuestion);
    let msgs = h.store.messages("s1").unwrap();
    assert!(
        msgs.iter()
            .filter(|m| m.message.text().contains("Lamp 0, $0.99"))
            .all(|m| m.untrusted)
    );
}

#[tokio::test]
async fn the_todo_list_notes_and_stored_results() {
    let notes: String = (0..2_000).fold(String::new(), |mut s, i| {
        s.push_str("note line ");
        s.push_str(&i.to_string());
        s.push('\n');
        s
    });
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::tool(
                "mcp__oxbot__update_todo",
                json!({"items": [{"id": "t1", "text": "Read the notes", "state": "doing"}]}),
            ))
            .then(Step::tool("Read", json!({"file_path": "notes.md"})))
            .then(Step::tool(
                "mcp__oxbot__read_result",
                json!({"handle": "r1", "find": "line 1999"}),
            ))
            .then(Step::tool(
                "mcp__oxbot__update_todo",
                json!({"items": [{"id": "t1", "text": "Something else", "state": "done"}]}),
            ))
            .then(Step::tool(
                "mcp__oxbot__note_progress",
                json!({"text": "Read the notes; the last line is 1999."}),
            ))
            .then(Step::text("Done.")),
    );
    std::fs::write(h.work.join("notes.md"), &notes).unwrap();
    let seen = h
        .turn(
            h.spec(new_session()),
            "[Jay] read my notes",
            HubScript::default(),
        )
        .await;
    assert_eq!(seen.last().unwrap(), &Seen::Done(true, "Done.".into()));
    let reqs = h.model.requests();
    let texts = tool_texts(&reqs[5]);
    assert_eq!(texts[0], "<todo>\n[>] t1 Read the notes\n</todo>");
    assert!(texts[1].starts_with("[stored as r1: Read,"), "{}", texts[1]);
    assert!(texts[2].starts_with("<stored_result handle=\"r1\">1 matches for \"line 1999\""));
    // Read's own line numbers, not a second set.
    assert!(
        texts[2].contains("\n  2000\tnote line 1999</stored_result>"),
        "{}",
        texts[2]
    );
    assert!(
        texts[3].starts_with("[tool error] t1 can't be reworded"),
        "{}",
        texts[3]
    );
    assert_eq!(
        texts[4],
        "Noted. It's shown again if the conversation starts over."
    );
    // Read back, never stored again; it keeps the file's trust.
    assert_eq!(h.store.last_result_number("s1").unwrap(), 1);
    let msgs = h.store.messages("s1").unwrap();
    let back = msgs
        .iter()
        .find(|m| m.message.text().starts_with("<stored_result"))
        .unwrap();
    assert!(back.untrusted);
    assert_eq!(back.handle.as_deref(), Some("r1"));
    // The list is the session's, and the note outlives the window.
    let state = WindowState::from_value(&h.store.context("s1").unwrap());
    assert_eq!(state.todo[0].state, crate::context::state::TodoState::Doing);
    assert_eq!(state.last_todo_idx, Some(1));
    let progress = h.store.progress("s1", 5).unwrap();
    assert_eq!(progress[0].note, "Read the notes; the last line is 1999.");
    assert!(
        progress[0].tainted_by.is_some(),
        "written after reading a file"
    );
}

#[tokio::test]
async fn the_same_failure_three_times_gets_a_nudge() {
    let click = || Step::tool("mcp__chrome-devtools__click", json!({"uid": "9_9"}));
    let h = Harness::with(
        ScriptedModel::new()
            .then(click())
            .then(click())
            .then(click())
            .then(Step::text("I couldn't.")),
        |s| {
            let browser = ScriptedBrowser::new(|_, _| BrowserOutput {
                text: "No element with uid 9_9.".into(),
                is_error: true,
                ..BrowserOutput::default()
            });
            s.browser = Some(Arc::new(SameBrowser(browser)));
        },
    );
    h.turn(
        h.spec(new_session()),
        "[Jay] click it",
        HubScript::default(),
    )
    .await;
    let texts = tool_texts(&h.model.requests()[3]);
    assert_eq!(texts[0], "[tool error] No element with uid 9_9.");
    assert_eq!(texts[1], "[tool error] No element with uid 9_9.");
    assert_eq!(
        texts[2],
        "[tool error] No element with uid 9_9.\nThis failed 3 times the same way; try something else or ask the user."
    );
}

#[tokio::test]
async fn a_task_passed_on_after_outside_text_starts_tainted() {
    let h = Harness::new(ScriptedModel::new().then(Step::text("Ok.")));
    let started = h.engine.start(h.spec(new_session())).await.unwrap();
    let mut session = started.session;
    session
        .send(TurnInput {
            text: "[Herder] check the news".into(),
            user_seq: 3,
            person: Some("p1".into()),
            origin: json!({"person": "p1", "tainted_by": "a web page (https://news.example.com/)"}),
        })
        .await
        .unwrap();
    pump(started.events, HubScript::default()).await;
    session.shutdown().await;
    let state = WindowState::from_value(&h.store.context("s1").unwrap());
    assert_eq!(
        state.tainted_by.as_deref(),
        Some("a task passed on after reading a web page (https://news.example.com/)")
    );
}

#[tokio::test]
async fn too_long_for_the_model_makes_room_and_counts_higher() {
    let summary = json!({"progress_note": "Answer it."}).to_string();
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::error(
                ErrorKind::ContextLength,
                "maximum context length is 1000",
            ))
            .then(Step::text(summary))
            .then(Step::text("Short answer.").with_usage(CallUsage {
                prompt_tokens: 900,
                completion_tokens: 3,
                ..CallUsage::default()
            })),
    );
    let seen = h
        .turn(
            h.spec(new_session()),
            "[Jay] hello there",
            HubScript::default(),
        )
        .await;
    assert_eq!(
        seen.last().unwrap(),
        &Seen::Done(true, "Short answer.".into())
    );
    let kinds: Vec<_> = h
        .store
        .calls()
        .iter()
        .map(|c| (c.kind, c.outcome.clone()))
        .collect();
    assert_eq!(
        kinds[0],
        (
            oxbot_engine::CallKind::Chat,
            "error:context_length".to_owned()
        )
    );
    assert_eq!(kinds[1].0, oxbot_engine::CallKind::Compaction);
    assert_eq!(kinds[2], (oxbot_engine::CallKind::Chat, "ok".to_owned()));
    // Both models that may answer now count a fifth higher, and the
    // provider's count teaches the one that answered.
    let saved = h.store.calibrations().unwrap();
    let b = saved.iter().find(|c| c.model == "b:free").unwrap();
    assert!((b.ratio - 1.2).abs() < 1e-9, "{saved:?}");
    let a = saved.iter().find(|c| c.model == "a:free").unwrap();
    assert_eq!(a.samples, 1);
}

/// Runs one turn on `session` (already started) and waits for its end.
async fn one_turn(
    session: &mut Box<dyn oxbot_engine::Session>,
    events: &mut mpsc::Receiver<EngineEvent>,
    text: &str,
    seq: u64,
) {
    session
        .send(TurnInput {
            text: text.into(),
            user_seq: seq,
            person: Some("p1".into()),
            origin: json!({"person": "p1"}),
        })
        .await
        .unwrap();
    loop {
        match events.recv().await.unwrap() {
            EngineEvent::TurnDone { ok, text, .. } => {
                assert!(ok, "{text}");
                break;
            }
            EngineEvent::Permission(r) => r.reply.send(Decision::Allow),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_message_hours_later_starts_a_fresh_window_with_the_last_chats() {
    let h = Harness::new(
        ScriptedModel::new()
            .then(Step::text("Lamp A is best."))
            .then(Step::text(
                json!({"facts": [{"text": "Lamp A is best", "source": "bot"}]}).to_string(),
            ))
            .then(Step::text("Hello again.")),
    );
    let started = h.engine.start(h.spec(new_session())).await.unwrap();
    let (mut session, mut events) = (started.session, started.events);
    one_turn(&mut session, &mut events, "[Jay] which lamp?", 1).await;
    session.shutdown().await;
    // Seven hours pass.
    let mut state = WindowState::from_value(&h.store.context("s1").unwrap());
    state.last_ms -= 7 * 3_600_000;
    h.store.set_context("s1", state.to_value());
    let started = h
        .engine
        .start(h.spec(Resume::Resume("s1".into())))
        .await
        .unwrap();
    let (mut session, mut events) = (started.session, started.events);
    one_turn(&mut session, &mut events, "[Jay] hi", 2).await;
    session.shutdown().await;
    let reqs = h.model.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[1].messages[0].text(), crate::COMPACT_V1);
    let last = &reqs[2];
    assert_eq!(last.messages.len(), 2, "no old turns");
    let text = last.messages[1].text();
    assert!(text.starts_with("<window n=\"2\""), "{text}");
    assert!(
        text.contains(
            "<earlier_chat>[Jay] which lamp?\nYou replied: Lamp A is best.</earlier_chat>"
        ),
        "{text}"
    );
    assert!(text.contains("Earlier task: [Jay] which lamp?"));
    assert!(
        !text.contains(crate::context::header::START_ROUTINE),
        "a new message, not a pick-up"
    );
    assert!(text.ends_with("\n\n[Jay] hi"));
}

#[tokio::test]
async fn sixty_steps_in_one_window_start_a_fresh_one() {
    let mut model = ScriptedModel::new();
    for _ in 0..35 {
        model = model.then(Step::tool("Glob", json!({"pattern": "*.md"})));
    }
    model = model.then(Step::text("Half way."));
    for _ in 0..25 {
        model = model.then(Step::tool("Glob", json!({"pattern": "*.md"})));
    }
    model = model
        .then(Step::text(
            json!({"open": ["finish"], "progress_note": "25 more globs done."}).to_string(),
        ))
        .then(Step::text("All done."));
    // No pacing: 63 requests would take three minutes at the free rate.
    let h = Harness::with(model, |s| s.pacer = crate::Pacer::with_rate(10_000, None));
    let started = h.engine.start(h.spec(new_session())).await.unwrap();
    let (mut session, mut events) = (started.session, started.events);
    one_turn(&mut session, &mut events, "[Jay] glob a lot", 1).await;
    one_turn(&mut session, &mut events, "[Jay] go on", 2).await;
    session.shutdown().await;
    let reqs = h.model.requests();
    let compaction = reqs
        .iter()
        .position(|r| r.messages[0].text() == crate::COMPACT_V1)
        .unwrap();
    // After 61 steps in the window (36 + 25).
    assert_eq!(compaction, 61);
    let fresh = &reqs[compaction + 1];
    assert_eq!(fresh.messages.len(), 2, "the header alone");
    let header = fresh.messages[1].text();
    assert!(
        header.contains(crate::context::header::START_ROUTINE),
        "{header}"
    );
    assert!(
        header.contains("<progress untrusted=\"yes\">- 25 more globs done.</progress>"),
        "{header}"
    );
    assert!(header.ends_with("The task, in the person's own words:\n[Jay] go on\nTheir other messages, oldest first:\n[Jay] glob a lot"), "{header}");
}
