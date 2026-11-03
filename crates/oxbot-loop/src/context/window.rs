//! [`WindowedContext`]: the window's messages with the frozen header in
//! front, old tool results cleared to stubs when the context gets long
//! (lightest touch first), and the to-do list recited when it's due.
//!
//! Clearing (Anthropic's context editing): above 60% of the usable window,
//! the oldest tool results become one-line stubs until 40% is reached, but
//! only if that frees at least 10% (each clearing breaks the provider's
//! cache from that point). The last 3 results stay, unless a later
//! snapshot of the same tab replaced one. Errors, refusals and the to-do
//! list's own results are never cleared, nor are the model's own calls.
//! Every stub names the stored handle, so nothing is lost for good.

use std::collections::{BTreeSet, HashMap};

use oxbot_llm::{Message, Part, Role};

use super::header::{self, Exchange, HeaderParts};
use super::state::{EntryMeta, TaskState, WindowState, render_todo};
use super::{Built, ContextBuilder, Pressure, TaskView};
use crate::KnowledgeBlock;
use crate::budget::{TokenBudget, est, est_message, est_tools, scaled};
use crate::meter::utc_day;
use crate::store::{ProgressNote, StoredMessage};

/// Tool results kept whole however long the context gets.
pub const KEEP_LAST: usize = 3;
/// Steps without `update_todo` before the list is recited.
pub const RECITE_AFTER: usize = 5;
/// Most tokens of a recitation.
pub const RECITE_TOKENS: u64 = 300;

/// The window builder.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowedContext;

/// How a new window starts.
#[derive(Debug, Clone, Copy)]
pub struct NewWindow<'a> {
    /// Its first message (may not exist yet: a fresh window mid-turn).
    pub start_idx: u32,
    /// The task state to carry.
    pub task_state: Option<&'a TaskState>,
    /// Progress notes to show.
    pub progress: &'a [ProgressNote],
    /// Earlier chats to show.
    pub recent: &'a [Exchange],
    /// No earlier turns in it: add the start routine.
    pub fresh: bool,
    /// Now.
    pub now_ms: i64,
    /// What the bot knows.
    pub knowledge: Option<&'a KnowledgeBlock>,
    /// The conversation.
    pub messages: &'a [StoredMessage],
}

/// Starts the next window in `state`.
pub fn begin_window(state: &mut WindowState, new: &NewWindow<'_>) {
    let first = new.messages.iter().find(|m| m.idx == new.start_idx);
    let alone = first.is_none_or(|m| m.message.role != Role::User);
    state.v = 1;
    state.window += 1;
    state.start_idx = new.start_idx;
    state.started_ms = new.now_ms;
    state.header_alone = alone;
    state.task_state = new.task_state.cloned();
    if let Some(ts) = &state.task_state
        && ts.taint.untrusted
        && let Some(why) = ts.taint.why.first().cloned()
    {
        state.taint(&why);
    }
    state.header = header::render(&HeaderParts {
        window: state.window,
        date: &utc_day(new.now_ms),
        knowledge: new.knowledge,
        task_state: state.task_state.as_ref(),
        progress: new.progress,
        recent: new.recent,
        fresh: new.fresh,
        alone,
    });
    state.knowledge_ids = new.knowledge.map(|k| k.ids.clone()).unwrap_or_default();
    state.cleared.retain(|idx, _| *idx >= new.start_idx);
    state.recitations.clear();
    state.last_recite_idx = None;
    state.recite_next = true;
    state.fresh = new.fresh;
    state.pending_start = false;
    state.generation += 1;
}

/// The window's messages.
fn in_window<'a>(messages: &'a [StoredMessage], state: &WindowState) -> Vec<&'a StoredMessage> {
    messages
        .iter()
        .filter(|m| m.idx >= state.start_idx)
        .collect()
}

/// `m` as sent: a stub if cleared, with the header in front if it starts
/// the window, with a recitation at the end if it has one.
fn shown(m: &StoredMessage, state: &WindowState) -> Message {
    let mut message = match state.cleared.get(&m.idx) {
        Some(stub) => Message {
            role: m.message.role,
            parts: vec![Part::Text(stub.clone())],
            tool_calls: m.message.tool_calls.clone(),
            tool_call_id: m.message.tool_call_id.clone(),
        },
        None => m.message.clone(),
    };
    if !state.header_alone && m.idx == state.start_idx && m.message.role == Role::User {
        prepend(&mut message, &format!("{}\n\n", state.header));
    }
    if let Some(recited) = state.recitations.get(&m.idx) {
        append(&mut message, &format!("\n\n{recited}"));
    }
    message
}

fn prepend(message: &mut Message, text: &str) {
    match message.parts.iter_mut().find_map(|p| match p {
        Part::Text(t) => Some(t),
        Part::Image { .. } => None,
    }) {
        Some(t) => t.insert_str(0, text),
        None => message
            .parts
            .insert(0, Part::Text(text.trim_end().to_owned())),
    }
}

fn append(message: &mut Message, text: &str) {
    match message.parts.iter_mut().rev().find_map(|p| match p {
        Part::Text(t) => Some(t),
        Part::Image { .. } => None,
    }) {
        Some(t) => t.push_str(text),
        None => message.parts.push(Part::Text(text.trim_start().to_owned())),
    }
}

/// `url` short: host and path, query cut at 40 characters.
#[must_use]
pub fn short_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split('#').next().unwrap_or(rest);
    match rest.split_once('?') {
        Some((path, query)) if query.chars().count() > 40 => {
            format!("{path}?{}…", query.chars().take(40).collect::<String>())
        }
        _ => rest.to_owned(),
    }
}

/// Tokens in words: `812 tokens`, `14.2k tokens`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn tokens_in_words(n: u64) -> String {
    if n < 1_000 {
        format!("{n} tokens")
    } else {
        format!("{:.1}k tokens", n as f64 / 1_000.0)
    }
}

/// The tool's own short name: `take_snapshot`, `Read`, `find_in_page`.
#[must_use]
pub fn short_tool(name: &str) -> &str {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__").map(|(_, t)| t))
        .unwrap_or(name)
}

/// The stub a cleared result becomes.
#[must_use]
pub fn stub(handle: &str, meta: &EntryMeta, tokens: u64, untrusted: bool) -> String {
    let tool = meta.tool.as_deref().map_or("tool", short_tool);
    let mut parts = vec![format!("[cleared {handle}: {tool}")];
    if let Some(url) = &meta.page_url {
        parts.push(short_url(url));
    }
    parts.push(tokens_in_words(meta.full_tokens.unwrap_or(tokens)));
    parts.push(
        match (&meta.trust_why, &meta.page_key) {
            (Some(_), Some(_)) => "web page",
            (Some(why), None) if why.starts_with("a web page") => "web text",
            (Some(_), None) => "outside text",
            // Saved before there were notes on it: the flag still knows.
            (None, _) if untrusted => "outside text",
            (None, _) => "oxbot text",
        }
        .to_owned(),
    );
    let page_tool = meta
        .tool
        .as_deref()
        .is_some_and(|t| crate::tools::page::PAGE_TOOLS.contains(&t));
    let again = if meta.page_key.is_some() || page_tool {
        format!("find_in_page(page: \"{handle}\", …) or read_section to look again]")
    } else {
        format!("read_result(\"{handle}\") to see it again]")
    };
    parts.push(again);
    parts.join(" · ")
}

impl WindowedContext {
    /// Clears old results in `state` if it's over the line. Returns
    /// whether anything changed.
    #[allow(clippy::too_many_arguments)]
    fn clear(
        state: &mut WindowState,
        window: &[&StoredMessage],
        sizes: &HashMap<u32, u64>,
        total: u64,
        budget: TokenBudget,
        ratio: f64,
    ) -> bool {
        if total <= budget.clear_at() {
            return false;
        }
        let tools: Vec<&StoredMessage> = window
            .iter()
            .copied()
            .filter(|m| m.message.role == Role::Tool)
            .collect();
        let last: BTreeSet<u32> = tools.iter().rev().take(KEEP_LAST).map(|m| m.idx).collect();
        let metas: HashMap<u32, EntryMeta> = tools
            .iter()
            .map(|m| (m.idx, EntryMeta::from_value(&m.meta)))
            .collect();
        // Snapshots a later full look at the same tab replaced (a diff
        // doesn't: the model needs the page it compares with).
        let mut superseded: BTreeSet<u32> = BTreeSet::new();
        let mut later: BTreeSet<&str> = BTreeSet::new();
        for m in tools.iter().rev() {
            if let Some(key) = metas.get(&m.idx).and_then(|meta| meta.page_key.as_deref()) {
                if later.contains(key) {
                    superseded.insert(m.idx);
                }
                if !metas.get(&m.idx).is_some_and(|meta| meta.diff) {
                    later.insert(key);
                }
            }
        }
        let newest_recital = state.recitations.keys().max().copied();
        let mut plan: Vec<(u32, Option<String>)> = Vec::new();
        let mut freed = 0u64;
        let target = total.saturating_sub(budget.clear_to());
        for m in &tools {
            if freed >= target {
                break;
            }
            let meta = metas.get(&m.idx).cloned().unwrap_or_default();
            // Only a real handle (results saved by an older version named a
            // file path instead): every stub must read back.
            let eligible = !state.cleared.contains_key(&m.idx)
                && m.handle
                    .as_deref()
                    .is_some_and(crate::results::valid_handle)
                && !meta.kept()
                && (!last.contains(&m.idx) || superseded.contains(&m.idx));
            if eligible && let Some(handle) = &m.handle {
                let size = sizes.get(&m.idx).copied().unwrap_or(0);
                let text = stub(handle, &meta, est_message(&m.message), m.untrusted);
                let after = scaled(est(&text) + crate::budget::MESSAGE_OVERHEAD, ratio);
                if size > after {
                    freed += size - after;
                    plan.push((m.idx, Some(text)));
                }
            } else if Some(m.idx) != newest_recital
                && let Some(recited) = state.recitations.get(&m.idx)
            {
                // An old recitation goes even where the result stays.
                freed += scaled(est(recited), ratio);
                plan.push((m.idx, None));
            }
        }
        if freed < budget.clear_at_least() {
            return false;
        }
        for (idx, stub) in plan {
            match stub {
                Some(text) => {
                    state.cleared.insert(idx, text);
                    state.recitations.remove(&idx);
                }
                None => {
                    state.recitations.remove(&idx);
                }
            }
        }
        state.generation += 1;
        state.recite_next = true;
        true
    }

    /// Recites the to-do list on the newest tool result when it's due.
    fn recite(state: &mut WindowState, window: &[&StoredMessage]) -> bool {
        let Some(newest) = window.last() else {
            return false;
        };
        if newest.message.role != Role::Tool
            || state.open_todo() == 0
            || state.recitations.contains_key(&newest.idx)
            || state.cleared.contains_key(&newest.idx)
        {
            return false;
        }
        let since = state.last_todo_idx.max(state.last_recite_idx);
        let steps = window
            .iter()
            .filter(|m| m.message.role == Role::Assistant && since.is_none_or(|s| m.idx > s))
            .count();
        if !state.recite_next && steps < RECITE_AFTER {
            return false;
        }
        state
            .recitations
            .insert(newest.idx, render_todo(&state.todo, RECITE_TOKENS));
        state.last_recite_idx = Some(newest.idx);
        state.recite_next = false;
        true
    }
}

/// Each window message's tokens, the estimated total, and the total from
/// the provider's own count of the last request plus what came after it
/// (when nothing sent then has changed).
fn measure(
    view: &TaskView<'_>,
    state: &WindowState,
    window: &[&StoredMessage],
) -> (HashMap<u32, u64>, u64, Option<u64>) {
    let ratio = view.ratio;
    let fixed = scaled(est(view.system) + est_tools(view.tools), ratio)
        + if state.header_alone {
            scaled(est(&state.header) + crate::budget::MESSAGE_OVERHEAD, ratio)
        } else {
            0
        };
    let sizes: HashMap<u32, u64> = window
        .iter()
        .map(|m| (m.idx, scaled(est_message(&shown(m, state)), ratio)))
        .collect();
    let estimated = fixed + sizes.values().sum::<u64>();
    let anchored = view.last_call.and_then(|last| {
        let li = last.last_idx?;
        (last.generation == state.generation
            && li >= state.start_idx
            && last.prompt_tokens > 0
            && view.answering.contains(&last.model))
        .then(|| {
            let mut after = window.iter().filter(|m| m.idx > li).peekable();
            // The model's own answer is what it counted as completion.
            if after
                .peek()
                .is_some_and(|m| m.message.role == Role::Assistant)
            {
                after.next();
            }
            let rest: u64 = after.map(|m| sizes.get(&m.idx).copied().unwrap_or(0)).sum();
            last.prompt_tokens + last.completion_tokens + rest
        })
    });
    (sizes, estimated, anchored)
}

impl ContextBuilder for WindowedContext {
    fn build(&self, view: &TaskView<'_>, budget: TokenBudget) -> Built {
        let mut state = WindowState::from_value(view.context);
        let mut changed = false;
        if state.window == 0 || state.pending_start {
            let start = view
                .messages
                .iter()
                .map(|m| m.idx)
                .find(|i| *i >= state.start_idx)
                .unwrap_or(state.start_idx);
            let first = state.window == 0;
            let task = state.task_state.clone();
            begin_window(
                &mut state,
                &NewWindow {
                    start_idx: start,
                    task_state: task.as_ref(),
                    progress: &[],
                    recent: &[],
                    fresh: false,
                    now_ms: view.now_ms,
                    knowledge: view.knowledge,
                    messages: view.messages,
                },
            );
            state.pending_start = false;
            // A first window has no list to recite yet.
            if first {
                state.recite_next = false;
            }
            changed = true;
        }
        let window = in_window(view.messages, &state);
        for m in &window {
            if m.untrusted && state.tainted_by.is_none() {
                let why = EntryMeta::from_value(&m.meta)
                    .trust_why
                    .unwrap_or_else(|| "outside text".to_owned());
                state.taint(&why);
                changed = true;
            }
        }
        let ratio = view.ratio;
        let (sizes, estimated, anchored) = measure(view, &state, &window);
        let total = anchored.unwrap_or(estimated);
        if Self::clear(&mut state, &window, &sizes, total, budget, ratio) {
            changed = true;
        }
        if Self::recite(&mut state, &window) {
            changed = true;
        }
        let mut messages = vec![Message::system(view.system)];
        if state.header_alone {
            messages.push(Message::user(state.header.clone()));
        }
        messages.extend(window.iter().map(|m| shown(m, &state)));
        // Exact while nothing sent before changed; else the estimate.
        let (_, estimated, anchored) = measure(view, &state, &window);
        let est_tokens = anchored.unwrap_or(estimated);
        let cleared = view
            .messages
            .iter()
            .filter(|m| m.idx < state.start_idx || state.cleared.contains_key(&m.idx))
            .map(|m| m.idx)
            .collect();
        Built {
            messages,
            est_tokens,
            pressure: if est_tokens > budget.compact_at() {
                Pressure::Compact
            } else {
                Pressure::Ok
            },
            context: changed.then(|| state.to_value()),
            cleared,
            last_idx: window.last().map(|m| m.idx),
            generation: state.generation,
        }
    }
}

#[cfg(test)]
mod tests;
