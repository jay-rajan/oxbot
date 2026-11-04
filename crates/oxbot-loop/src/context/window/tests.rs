use oxbot_llm::{Message, Part, Role, ToolCall, ToolSpec};
use serde_json::{Value, json};

use super::*;
use crate::KnowledgeBlock;
use crate::context::state::{EntryKind, EntryMeta, TodoItem, TodoState, WindowState};
use crate::context::{LastCall, TaskView};
use crate::store::StoredMessage;

const DAY: i64 = 86_400_000;
/// 2026-11-03.
const NOW: i64 = 1_793_664_000_000 + 3_600_000;

fn budget() -> TokenBudget {
    // U = 7,452: clear at 5,589 down to 4,098 (at least 745), compact at
    // 6,557.
    TokenBudget::for_model(10_000, Some(2_048))
}

fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec {
        name: "mcp__chrome-devtools__take_snapshot".into(),
        description: "Read the page.".into(),
        parameters: json!({"type": "object", "properties": {"b": {}, "a": {}}}),
    }]
}

fn user(idx: u32, turn: u32, text: &str) -> StoredMessage {
    StoredMessage {
        idx,
        turn,
        message: Message::user(text),
        tokens: 0,
        untrusted: false,
        handle: None,
        meta: json!({}),
    }
}

fn call(idx: u32, turn: u32, id: &str, text: &str) -> StoredMessage {
    StoredMessage {
        idx,
        turn,
        message: Message::assistant(
            text,
            vec![ToolCall::from_raw(
                id,
                "mcp__chrome-devtools__take_snapshot",
                "{}",
            )],
        ),
        tokens: 0,
        untrusted: false,
        handle: None,
        meta: json!({}),
    }
}

fn page(idx: u32, turn: u32, id: &str, handle: &str, url: &str, size: usize) -> StoredMessage {
    let meta = EntryMeta {
        tool: Some("mcp__chrome-devtools__take_snapshot".into()),
        trust_why: Some(format!("a web page ({url})")),
        page_key: Some(format!("1 {url}")),
        page_url: Some(url.into()),
        full_tokens: Some(14_200),
        ..EntryMeta::default()
    };
    StoredMessage {
        idx,
        turn,
        message: Message::tool(id, vec![Part::Text("x".repeat(size))]),
        tokens: 0,
        untrusted: true,
        handle: Some(handle.into()),
        meta: meta.to_value(),
    }
}

fn kept(idx: u32, turn: u32, id: &str, kind: EntryKind) -> StoredMessage {
    let meta = EntryMeta {
        kind,
        tool: Some("mcp__chrome-devtools__click".into()),
        ..EntryMeta::default()
    };
    StoredMessage {
        idx,
        turn,
        message: Message::tool(id, vec![Part::Text("e".repeat(3_000))]),
        tokens: 0,
        untrusted: false,
        handle: Some(format!("r9{idx}")),
        meta: meta.to_value(),
    }
}

/// A turn with `n` snapshot steps of `size` bytes each, on `n` pages.
fn browsing(n: u32, size: usize) -> Vec<StoredMessage> {
    let mut msgs = vec![user(0, 0, "[Jay] compare lamps")];
    for i in 0..n {
        let id = format!("c{i}");
        msgs.push(call(1 + 2 * i, 0, &id, ""));
        msgs.push(page(
            2 + 2 * i,
            0,
            &id,
            &format!("r{}", i + 1),
            &format!("https://shop.example.com/p/{i}"),
            size,
        ));
    }
    msgs
}

struct View {
    messages: Vec<StoredMessage>,
    context: Value,
    knowledge: Option<KnowledgeBlock>,
    last_call: Option<LastCall>,
    now_ms: i64,
    tools: Vec<ToolSpec>,
    answering: Vec<String>,
}

impl View {
    fn new(messages: Vec<StoredMessage>) -> Self {
        Self {
            messages,
            context: json!({}),
            knowledge: None,
            last_call: None,
            now_ms: NOW,
            tools: tools(),
            answering: vec!["m:free".to_owned()],
        }
    }

    fn build(&self) -> Built {
        let view = TaskView {
            system: "You are Scout.",
            tools: &self.tools,
            knowledge: self.knowledge.as_ref(),
            messages: &self.messages,
            context: &self.context,
            person: Some("p1"),
            now_ms: self.now_ms,
            last_call: self.last_call.as_ref(),
            turn: 0,
            ratio: 1.0,
            answering: &self.answering,
        };
        WindowedContext.build(&view, budget())
    }

    /// Builds and keeps the new state, as the runner does.
    fn step(&mut self) -> Built {
        let built = self.build();
        if let Some(c) = &built.context {
            self.context = c.clone();
        }
        built
    }

    fn state(&self) -> WindowState {
        WindowState::from_value(&self.context)
    }
}

fn bytes(built: &Built) -> Vec<String> {
    built
        .messages
        .iter()
        .map(|m| serde_json::to_string(m).unwrap())
        .collect()
}

#[test]
fn build_is_byte_stable() {
    let mut v = View::new(browsing(2, 300));
    let first = v.step();
    // The same view again: the same bytes, and nothing new to save.
    let again = v.step();
    assert_eq!(bytes(&first), bytes(&again));
    assert_eq!(again.context, None);
    // The header is in front of the first message, the day only.
    assert_eq!(first.messages[0].role, Role::System);
    assert_eq!(
        first.messages[1].text(),
        "<window n=\"1\" date=\"2026-11-03\"/>\n\n[Jay] compare lamps"
    );
    // A new step changes only the tail.
    v.messages.push(call(5, 0, "c2", "Looking at a third."));
    let longer = v.step();
    let (a, b) = (bytes(&first), bytes(&longer));
    assert_eq!(b[..a.len()], a[..]);
    assert_eq!(b.len(), a.len() + 1);
    // What the bot knows changes mid-window: the header waits for the next
    // window.
    v.knowledge = Some(KnowledgeBlock {
        text: "<bot_knowledge>prefers brass</bot_knowledge>".into(),
        ids: vec!["k1".into()],
        untrusted: false,
    });
    assert_eq!(bytes(&v.step())[..b.len()], b[..]);
}

#[test]
fn prefix_has_no_clock() {
    let mut v = View::new(browsing(1, 300));
    let today = v.step();
    // The next day, another run, the same saved state: the same bytes.
    v.now_ms += DAY;
    assert_eq!(bytes(&v.build()), bytes(&today));
    let system = serde_json::to_string(&today.messages[0]).unwrap();
    assert!(!system.contains("2026"), "{system}");
    // Saved state has its keys sorted.
    let text = v.context.to_string();
    let keys: Vec<&str> = v
        .context
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted, "{text}");
    // A new window on another day shows that day.
    let mut next = v.state();
    begin_window(
        &mut next,
        &NewWindow {
            start_idx: 0,
            task_state: None,
            progress: &[],
            recent: &[],
            fresh: false,
            now_ms: v.now_ms,
            knowledge: None,
            messages: &v.messages,
        },
    );
    assert!(
        next.header
            .starts_with("<window n=\"2\" date=\"2026-11-04\"/>")
    );
}

#[test]
fn clearing_takes_the_oldest_and_keeps_the_last_three() {
    let mut v = View::new(browsing(8, 3_000));
    let built = v.step();
    let state = v.state();
    let cleared: Vec<u32> = state.cleared.keys().copied().collect();
    // Results at 2, 4, … 16; the last three (12, 14, 16) stay.
    assert_eq!(cleared, [2, 4, 6, 8, 10]);
    assert_eq!(
        state.cleared[&2],
        "[cleared r1: take_snapshot · shop.example.com/p/0 · 14.2k tokens · web page · \
         find_in_page(page: \"r1\", …) or read_section to look again]"
    );
    assert_eq!(built.messages[3].text(), state.cleared[&2]);
    assert_eq!(built.messages[3].tool_call_id.as_deref(), Some("c0"));
    // The model still sees what it called.
    assert_eq!(built.messages[2].tool_calls.len(), 1);
    assert!(built.cleared.contains(&2) && !built.cleared.contains(&12));
    assert_eq!(state.generation, 2, "the window, then one clearing");
    assert!(built.est_tokens < budget().clear_at());
    // Built again: stable.
    assert_eq!(bytes(&v.build()), bytes(&built));
}

#[test]
fn a_replaced_snapshot_goes_even_when_recent() {
    let mut msgs = browsing(5, 3_000);
    // Steps 4 and 5 look at the same page as step 3: step 3's is replaced.
    let url = "https://shop.example.com/p/2";
    for (i, idx) in [(3u32, 8u32), (4, 10)] {
        msgs[idx as usize] = page(idx, 0, &format!("c{i}"), &format!("r{}", i + 1), url, 3_000);
    }
    // The two oldest are errors (never cleared), so clearing reaches 8.
    for (i, idx) in [(0u32, 2u32), (1, 4)] {
        msgs[idx as usize] = kept(idx, 0, &format!("c{i}"), EntryKind::Error);
    }
    msgs.push(call(11, 0, "c5", ""));
    msgs.push(kept(12, 0, "c5", EntryKind::Normal));
    let mut v = View::new(msgs);
    v.step();
    let cleared: Vec<u32> = v.state().cleared.keys().copied().collect();
    // 8 is within the last three results (8, 10, 12) but replaced by 10.
    assert!(cleared.contains(&8), "{cleared:?}");
    assert!(!cleared.contains(&10) && !cleared.contains(&12));
}

#[test]
fn a_diff_doesnt_replace_the_page_it_compares_with() {
    let mut msgs = browsing(5, 3_000);
    let url = "https://shop.example.com/p/2";
    for (i, idx) in [(3u32, 8u32), (4, 10)] {
        msgs[idx as usize] = page(idx, 0, &format!("c{i}"), &format!("r{}", i + 1), url, 3_000);
    }
    // The later look was shown as what changed since 8.
    let mut meta = EntryMeta::from_value(&msgs[10].meta);
    meta.diff = true;
    msgs[10].meta = meta.to_value();
    msgs.push(call(11, 0, "c5", ""));
    msgs.push(kept(12, 0, "c5", EntryKind::Normal));
    let mut v = View::new(msgs);
    v.step();
    assert!(
        !v.state().cleared.contains_key(&8),
        "{:?}",
        v.state().cleared
    );
}

#[test]
fn errors_refusals_and_the_todo_list_are_never_cleared() {
    let mut msgs = vec![user(0, 0, "[Jay] go")];
    let kinds = [EntryKind::Error, EntryKind::Refusal, EntryKind::Todo];
    for i in 0..9u32 {
        let id = format!("c{i}");
        msgs.push(call(1 + 2 * i, 0, &id, ""));
        msgs.push(kept(2 + 2 * i, 0, &id, kinds[(i % 3) as usize]));
    }
    let mut v = View::new(msgs);
    let built = v.step();
    assert!(v.state().cleared.is_empty());
    // Still too long: compaction's turn.
    assert_eq!(built.pressure, Pressure::Compact);
}

#[test]
fn nothing_is_cleared_when_it_would_free_too_little() {
    // Over the line, but the only old result is small.
    let mut msgs = vec![user(0, 0, &"y".repeat(17_000))];
    msgs.push(call(1, 0, "c0", ""));
    let mut small = page(2, 0, "c0", "r1", "https://shop.example.com/", 900);
    small.message = Message::tool("c0", vec![Part::Text("s".repeat(900))]);
    msgs.push(small);
    for i in 1..4u32 {
        let id = format!("c{i}");
        msgs.push(call(1 + 2 * i, 0, &id, ""));
        msgs.push(page(
            2 + 2 * i,
            0,
            &id,
            &format!("r{}", i + 1),
            "https://shop.example.com/x",
            30,
        ));
    }
    let mut v = View::new(msgs);
    let built = v.step();
    assert!(built.est_tokens > budget().clear_at());
    assert!(v.state().cleared.is_empty());
}

#[test]
fn rows_and_the_latest_reading_of_each_page_are_never_cleared() {
    let mut msgs = browsing(8, 3_000);
    let reading = |idx: usize, tool: &str, handle: &str| {
        let mut meta = EntryMeta::from_value(&msgs[idx].meta);
        meta.tool = Some(tool.into());
        meta.page_key = None;
        (meta, handle.to_owned())
    };
    // 2: rows; 4 and 6: two sections of page r9 (6 is the latest); 8: a
    // question about page r8; 10: an older section of r8.
    let marks = [
        (2usize, reading(2, crate::tools::page::QUERY_ROWS, "d1")),
        (4, reading(4, crate::tools::page::READ_SECTION, "r9")),
        (6, reading(6, crate::tools::page::READ_SECTION, "r9")),
        (8, reading(8, crate::tools::page::READ_SECTION, "r8")),
        (10, reading(10, crate::tools::page::ASK_PAGE, "r8")),
    ];
    for (idx, (meta, handle)) in marks {
        msgs[idx].meta = meta.to_value();
        msgs[idx].handle = Some(handle);
    }
    let mut v = View::new(msgs);
    v.step();
    let cleared: Vec<u32> = v.state().cleared.keys().copied().collect();
    assert!(!cleared.contains(&2), "rows stay: {cleared:?}");
    assert!(cleared.contains(&4), "an older section goes: {cleared:?}");
    assert!(!cleared.contains(&6), "the latest section of r9 stays");
    assert!(cleared.contains(&8), "r8's section was read again later");
    assert!(!cleared.contains(&10), "the latest reading of r8 stays");
}

#[test]
fn results_without_a_handle_stay() {
    let mut msgs = browsing(8, 3_000);
    for m in &mut msgs {
        m.handle = None;
    }
    let mut v = View::new(msgs);
    v.step();
    assert!(v.state().cleared.is_empty(), "every stub needs a handle");
}

#[test]
fn an_old_spill_path_is_no_handle() {
    let mut msgs = browsing(8, 3_000);
    for m in &mut msgs {
        if m.handle.is_some() {
            m.handle = Some(".oxbot/results/0-1-0.txt".into());
        }
    }
    let mut v = View::new(msgs);
    v.step();
    assert!(v.state().cleared.is_empty());
}

#[test]
fn the_todo_list_is_recited_after_five_quiet_steps() {
    let mut msgs = browsing(2, 30);
    let mut v = View::new(msgs.clone());
    v.step();
    let mut state = v.state();
    state.todo = vec![TodoItem {
        id: "t1".into(),
        text: "Compare three lamps".into(),
        state: TodoState::Doing,
        why: None,
    }];
    // update_todo was the first call.
    state.last_todo_idx = Some(1);
    v.context = state.to_value();
    // Steps 2…5 since: not yet.
    for i in 2..5u32 {
        let id = format!("c{i}");
        msgs.push(call(1 + 2 * i, 0, &id, ""));
        msgs.push(page(
            2 + 2 * i,
            0,
            &id,
            &format!("r{}", i + 1),
            "https://shop.example.com/",
            30,
        ));
    }
    v.messages = msgs.clone();
    v.step();
    assert!(v.state().recitations.is_empty());
    // The fifth step since: recited at the end of the newest result.
    msgs.push(call(11, 0, "c5", ""));
    msgs.push(page(12, 0, "c5", "r6", "https://shop.example.com/", 30));
    v.messages = msgs.clone();
    let built = v.step();
    let last = built.messages.last().unwrap().text();
    assert!(
        last.ends_with("\n\n<todo>\n[>] t1 Compare three lamps\n</todo>"),
        "{last}"
    );
    assert_eq!(v.state().last_recite_idx, Some(12));
    // And it stays as it was sent.
    msgs.push(call(13, 0, "c6", ""));
    v.messages = msgs;
    let next = v.step();
    assert_eq!(next.messages[next.messages.len() - 2].text(), last);
}

#[test]
fn a_header_alone_when_the_window_starts_mid_turn() {
    let msgs = browsing(3, 30);
    let mut state = WindowState::default();
    begin_window(
        &mut state,
        &NewWindow {
            start_idx: 5,
            task_state: None,
            progress: &[],
            recent: &[],
            fresh: false,
            now_ms: NOW,
            knowledge: None,
            messages: &msgs,
        },
    );
    let mut v = View::new(msgs);
    v.context = state.to_value();
    let built = v.step();
    assert_eq!(built.messages[1].role, Role::User);
    assert_eq!(
        built.messages[1].text(),
        "<window n=\"1\" date=\"2026-11-03\"/>"
    );
    assert_eq!(built.messages[2].role, Role::Assistant);
    assert_eq!(built.messages.len(), 4);
    assert_eq!(built.cleared, [0, 1, 2, 3, 4]);
}

#[test]
fn the_providers_own_count_is_used_while_nothing_sent_changed() {
    let mut v = View::new(browsing(2, 300));
    let first = v.step();
    v.messages.push(call(5, 0, "c2", ""));
    v.messages
        .push(page(6, 0, "c2", "r3", "https://shop.example.com/", 3_000));
    // The provider counted 900 in and 20 out last time.
    v.last_call = Some(LastCall {
        model: "m:free".into(),
        prompt_tokens: 900,
        completion_tokens: 20,
        messages_sent: first.messages.len(),
        last_idx: first.last_idx,
        generation: first.generation,
    });
    let built = v.step();
    // 900 + 20 + the new result (3,000 bytes / 3 + framing).
    assert_eq!(built.est_tokens, 900 + 20 + 1_000 + 4);
    // Another model may answer: back to the estimate.
    v.answering = vec!["other:free".into()];
    assert_ne!(v.build().est_tokens, built.est_tokens);
}

#[test]
fn outside_text_taints_the_window() {
    let mut v = View::new(browsing(1, 30));
    v.step();
    assert_eq!(
        v.state().tainted_by.as_deref(),
        Some("a web page (https://shop.example.com/p/0)")
    );
}

#[test]
fn short_forms() {
    assert_eq!(
        short_url("https://shop.example.com/s?q=lamp#top"),
        "shop.example.com/s?q=lamp"
    );
    assert_eq!(
        short_url(&format!("https://a.example/s?{}", "q".repeat(50))),
        format!("a.example/s?{}…", "q".repeat(40))
    );
    assert_eq!(tokens_in_words(812), "812 tokens");
    assert_eq!(tokens_in_words(14_200), "14.2k tokens");
    assert_eq!(short_tool("mcp__oxbot__read_result"), "read_result");
    assert_eq!(short_tool("Read"), "Read");
}
