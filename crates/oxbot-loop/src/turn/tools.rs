//! A step's tool calls, in order: checked, asked of the gate, run, and
//! saved. The loop's own tools run here, where their state is: the to-do
//! list and progress notes, stored results read back, and the page tools
//! (rows included).
//!
//! Two guards keep a model from going round in circles: a read-only loop
//! tool asked the same thing twice in a turn gets a pointer to the first
//! answer, and a call that failed the same way three times gets a note to
//! try something else.

use std::collections::BTreeSet;
use std::sync::Arc;

use oxbot_engine::{Activity, EngineEvent};
use oxbot_llm::{Message, Part, Role, ToolCall};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{End, ToSave, Turn};
use crate::context::state::{EntryKind, EntryMeta, WindowState, update_todo};
use crate::gate::{Gated, Permit};
use crate::meter::now_ms;
use crate::runner::{Caller, RESTARTED_MID_CALL, SKIPPED, Stop};
use crate::session::{Conv, Core};
use crate::store::{PendingCall, ProgressNote};
use crate::tools::control::{ASK_USER, NOTE_PROGRESS, READ_RESULT, UPDATE_TODO};
use crate::tools::page::{self, PageEnv};
use crate::tools::{ToolEnv, ToolKind, ToolOutcome};

/// Calls in a row the model got wrong before the turn stops.
const MAX_BAD_CALLS: u32 = 3;
/// The same call failing this often gets a nudge.
const SAME_FAILURES: u32 = 3;
/// The nudge.
const TRY_ELSE: &str = "This failed 3 times the same way; try something else or ask the user.";

/// What one tool call came to.
enum Ran {
    /// Its result.
    Result(ToolOutcome),
    /// Refused: its result, and the rest of the step is skipped.
    Refused(ToolOutcome),
    /// It asked the person this.
    Asked(String),
}

impl Turn {
    /// Runs tool calls in order. `Some` when the turn ends here.
    pub(super) async fn tool_calls(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        caller: &Caller<'_>,
        calls: Vec<(ToolCall, Option<PendingCall>)>,
        bad_calls: &mut u32,
        cancel: &CancellationToken,
    ) -> Option<End> {
        let mut refused = false;
        let count = calls.len();
        for (n, (call, pending)) in calls.into_iter().enumerate() {
            let mut asked = None;
            let mut outcome = if refused {
                ToolOutcome::note(SKIPPED, false)
            } else if pending.as_ref().is_some_and(|p| p.side_effect) {
                ToolOutcome::note(RESTARTED_MID_CALL, true)
            } else {
                match self
                    .one_call(core, conv, caller, &call, bad_calls, cancel)
                    .await
                {
                    Ok(Ran::Result(outcome)) => outcome,
                    Ok(Ran::Refused(outcome)) => {
                        refused = true;
                        outcome
                    }
                    Ok(Ran::Asked(question)) => {
                        asked = Some(question);
                        ToolOutcome::note(crate::tools::control::ASKED, false)
                    }
                    Err(stop) => return Some(stop.into()),
                }
            };
            if outcome.error && outcome.meta.kind != EntryKind::Refusal {
                self.count_failure(&call, &mut outcome);
            }
            if n + 1 == count
                && let Some(note) = self.wrap_up.take()
                && let Some(Part::Text(t)) = outcome.for_model.last_mut()
            {
                t.push_str("\n\n");
                t.push_str(&note);
            }
            let _ = core
                .events
                .send(EngineEvent::ToolResult {
                    tool_use_id: call.id.clone(),
                    text: outcome.text_for_hub.clone(),
                })
                .await;
            let result = Message::tool(call.id.clone(), outcome.for_model);
            let save = ToSave {
                message: result,
                untrusted: outcome.untrusted,
                handle: outcome.handle,
                meta: outcome.meta,
            };
            if let Err(stop) = self.save(core, conv, vec![save], None) {
                return Some(stop.into());
            }
            if *bad_calls >= MAX_BAD_CALLS {
                return Some(End::Failed(
                    "The model kept making tool calls oxbot couldn't run, so the task stopped."
                        .to_owned(),
                ));
            }
            if let Some(question) = asked {
                let _ = core
                    .events
                    .send(EngineEvent::Assistant {
                        text: question.clone(),
                        tool_uses: Vec::new(),
                    })
                    .await;
                return Some(End::Waiting(question));
            }
            if cancel.is_cancelled() {
                return Some(End::Stopped);
            }
        }
        None
    }

    /// Counts a failed call; the third time it failed the same way, the
    /// model is told to try something else.
    fn count_failure(&mut self, call: &ToolCall, outcome: &mut ToolOutcome) {
        let key = call_key(&call.name, &call.arguments);
        let n = self.failures.entry(key).or_insert(0);
        *n += 1;
        if *n >= SAME_FAILURES
            && let Some(Part::Text(t)) = outcome.for_model.last_mut()
        {
            t.push('\n');
            t.push_str(TRY_ELSE);
        }
    }

    /// One tool call: check it, ask the gate, run it.
    async fn one_call(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        caller: &Caller<'_>,
        call: &ToolCall,
        bad_calls: &mut u32,
        cancel: &CancellationToken,
    ) -> Result<Ran, Stop> {
        let entry = match core.tools.validate(call) {
            Ok(entry) => entry,
            Err(why) => {
                *bad_calls += 1;
                return Ok(Ran::Result(ToolOutcome::note(
                    format!("[tool error] {why}"),
                    true,
                )));
            }
        };
        *bad_calls = 0;
        let (kind, side_effect) = (entry.kind, entry.side_effect);
        let gated = core
            .gate
            .check(&call.name, &call.arguments, &call.id, cancel)
            .await
            .ok_or(Stop::Cancelled)?;
        let permit = match gated {
            Gated::Denied(why) => {
                return Ok(Ran::Refused(
                    ToolOutcome::note(format!("[oxbot said no] {why}"), true)
                        .kind(EntryKind::Refusal),
                ));
            }
            Gated::Simulated(text) => return Ok(Ran::Result(ToolOutcome::note(text, false))),
            Gated::Run(permit) => permit,
        };
        if kind == ToolKind::Control && call.name == ASK_USER {
            let question = call
                .arguments
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            return Ok(Ran::Asked(question));
        }
        let _ = core
            .events
            .send(EngineEvent::Activity(Activity::UsingTool(
                call.name.clone(),
            )))
            .await;
        if matches!(kind, ToolKind::Control | ToolKind::Page) {
            return Ok(Ran::Result(
                self.own_tool(core, conv, caller, call, kind, permit, cancel)
                    .await,
            ));
        }
        if side_effect {
            // Written down first: after a restart it isn't run again.
            let ahead = PendingCall {
                id: call.id.clone(),
                name: call.name.clone(),
                args: call.raw.clone(),
                side_effect: true,
            };
            self.save(core, conv, Vec::new(), Some(ahead))?;
        }
        let visible = visible_handles(conv);
        let is_visible = |h: &str| visible.contains(h);
        let strictness = Arc::clone(&core.shared.setup.strictness);
        let env = ToolEnv {
            work_dir: &core.work_dir,
            events: &core.events,
            browser: core.browser.as_ref(),
            cancel,
            results: &core.results,
            pages: &core.pages,
            strictness,
            budget: self.budget,
            turn: self.number,
            step: self.step,
            visible: &is_visible,
        };
        Ok(Ran::Result(core.tools.run(call, permit, &env).await))
    }

    /// The loop's own tools (the gate already said yes).
    #[allow(clippy::too_many_arguments)]
    async fn own_tool(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        caller: &Caller<'_>,
        call: &ToolCall,
        kind: ToolKind,
        permit: Permit,
        cancel: &CancellationToken,
    ) -> ToolOutcome {
        debug_assert_eq!(permit.tool_use_id(), call.id);
        let meta = EntryMeta {
            tool: Some(call.name.clone()),
            ..EntryMeta::default()
        };
        let error = |text: String| {
            let mut o = ToolOutcome::note(format!("[tool error] {text}"), true);
            o.meta.tool = Some(call.name.clone());
            o
        };
        match call.name.as_str() {
            UPDATE_TODO => {
                let mut state = WindowState::from_value(&conv.context);
                match update_todo(&mut state.todo, &call.arguments) {
                    Ok(text) => {
                        state.last_todo_idx = last_assistant(conv);
                        state.recite_next = false;
                        conv.context = state.to_value();
                        conv.context_dirty = true;
                        let mut o = ToolOutcome::note(text, false).kind(EntryKind::Todo);
                        o.meta.tool = meta.tool;
                        o
                    }
                    Err(why) => error(why),
                }
            }
            NOTE_PROGRESS => match crate::tools::control::progress_note(&call.arguments) {
                Ok(note) => {
                    let tainted_by = WindowState::from_value(&conv.context).tainted_by;
                    let row = ProgressNote {
                        turn: self.number,
                        ms: now_ms(),
                        note,
                        tainted_by,
                    };
                    match core.shared.setup.store.add_progress(&core.id, &row) {
                        Ok(()) => {
                            let mut o = ToolOutcome::note(
                                "Noted. It's shown again if the conversation starts over.",
                                false,
                            );
                            o.meta.tool = meta.tool;
                            o
                        }
                        Err(err) => error(format!("the note couldn't be saved: {err}")),
                    }
                }
                Err(why) => error(why),
            },
            READ_RESULT
            | page::FIND_IN_PAGE
            | page::READ_SECTION
            | page::ASK_PAGE
            | page::EXTRACT_ROWS
            | page::QUERY_ROWS => self.read_only(core, conv, caller, call, kind, cancel).await,
            other => error(format!("unknown tool {other}")),
        }
    }

    /// `read_result` and the page tools: read-only, so a repeat within the
    /// turn gets a pointer to the first answer instead.
    async fn read_only(
        &mut self,
        core: &Core,
        conv: &Conv,
        caller: &Caller<'_>,
        call: &ToolCall,
        kind: ToolKind,
        cancel: &CancellationToken,
    ) -> ToolOutcome {
        let args = with_default_page(core, call, kind);
        let (key, wanted) = repeat_key(&call.name, &args);
        // Only while the first answer is still there to see: not once it's
        // a stub or out of the window (its stub says to ask again).
        let state = WindowState::from_value(&conv.context);
        if let Some((step, ..)) = self.seen.get(&key).filter(|(_, idx, from, to)| {
            *idx >= state.start_idx
                && !state.cleared.contains_key(idx)
                && wanted.0 >= *from
                && wanted.1 <= *to
        }) {
            let mut o = ToolOutcome::note(
                format!("You already did this at step {step}; the result is above."),
                false,
            );
            o.meta.tool = Some(call.name.clone());
            return o;
        }
        let mut meta = EntryMeta {
            tool: Some(call.name.clone()),
            ..EntryMeta::default()
        };
        let cap = self.budget.inline_cap();
        let outcome = if kind == ToolKind::Page {
            let helper = self.helper(core, caller, cancel);
            let env = PageEnv {
                results: &core.results,
                pages: &core.pages,
                strictness: core.shared.setup.strictness.as_ref(),
                budget: self.budget,
                helper: &helper,
                turn: self.number,
                step: self.step,
            };
            match page::run(&call.name, &args, &env).await {
                Ok(answer) => {
                    let why = if answer.url.is_empty() {
                        "a web page".to_owned()
                    } else {
                        format!("a web page ({})", answer.url)
                    };
                    meta.trust_why = Some(why);
                    meta.page_url = Some(answer.url.clone()).filter(|u| !u.is_empty());
                    ToolOutcome {
                        text_for_hub: answer.text.clone(),
                        for_model: vec![Part::Text(answer.text)],
                        untrusted: true,
                        // Cleared, it points back to the page it came from.
                        handle: Some(answer.handle),
                        error: false,
                        meta,
                    }
                }
                Err(why) => {
                    let mut o = ToolOutcome::note(format!("[tool error] {why}"), true);
                    o.meta.tool = Some(call.name.clone());
                    o
                }
            }
        } else {
            match crate::tools::control::read_result(&core.results, &args, cap) {
                Ok((text, tainted_by)) => {
                    let handle = args
                        .get("handle")
                        .and_then(Value::as_str)
                        .map(|h| h.trim().to_owned());
                    meta.trust_why.clone_from(&tainted_by);
                    ToolOutcome {
                        text_for_hub: text.clone(),
                        for_model: vec![Part::Text(text)],
                        // A stored result keeps its trust.
                        untrusted: tainted_by.is_some(),
                        // Never stored again: cleared, it points back.
                        handle,
                        error: false,
                        meta,
                    }
                }
                Err(why) => {
                    let mut o = ToolOutcome::note(format!("[tool error] {why}"), true);
                    o.meta.tool = Some(call.name.clone());
                    o
                }
            }
        };
        if !outcome.error {
            // Saved next, as the conversation's next message; with the lines
            // it showed (a section may stop early).
            let shown_to = shown_to(&outcome.text_for_hub).unwrap_or(usize::MAX);
            self.seen
                .insert(key, (self.step, conv.next_idx(), wanted.0, shown_to));
        }
        outcome
    }
}

/// A page tool's arguments with the latest page filled in when none is
/// named (a dataset names itself).
fn with_default_page(core: &Core, call: &ToolCall, kind: ToolKind) -> Value {
    let mut args = call.arguments.clone();
    if kind == ToolKind::Page
        && call.name != page::QUERY_ROWS
        && args.get("page").and_then(Value::as_str).is_none()
        && let (Some(map), Some(latest)) = (
            args.as_object_mut(),
            core.pages
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .latest()
                .map(str::to_owned),
        )
    {
        map.insert("page".to_owned(), Value::String(latest));
    }
    args
}

/// What a read-only call asks for, to spot repeats: a key and the lines
/// it wants. A section is the same ask whatever its line count when it
/// wants lines already shown; a search the same whatever its limit.
fn repeat_key(name: &str, args: &Value) -> (String, (usize, usize)) {
    let mut args = args.clone();
    if name == page::READ_SECTION {
        let n = |k: &str, d: usize| {
            args.get(k)
                .and_then(Value::as_u64)
                .and_then(|v| usize::try_from(v).ok())
                .unwrap_or(d)
        };
        let from = n("from_line", 1).max(1);
        let to = from + n("max_lines", 150).clamp(1, 150) - 1;
        let page = args.get("page").and_then(Value::as_str).unwrap_or("");
        let uid = args.get("uid").and_then(Value::as_str).unwrap_or("");
        let uid = uid.trim().trim_start_matches("uid=");
        return (format!("{name} {page} {uid}"), (from, to));
    }
    if let Some(map) = args.as_object_mut() {
        map.remove("limit");
    }
    (call_key(name, &args), (0, 0))
}

/// The last line a section showed (`[lines 1–40 of 90; …]`), if it stopped
/// early.
fn shown_to(text: &str) -> Option<usize> {
    let at = text.rfind("[lines ")?;
    let rest = &text[at + 7..];
    let (_, after) = rest.split_once('–')?;
    after.split(' ').next()?.parse().ok()
}

/// A call's name and arguments (sorted keys), to spot repeats.
fn call_key(name: &str, args: &Value) -> String {
    format!("{name} {args}")
}

/// The newest assistant message.
fn last_assistant(conv: &Conv) -> Option<u32> {
    conv.messages
        .iter()
        .rev()
        .find(|m| m.message.role == Role::Assistant)
        .map(|m| m.idx)
}

/// Stored results the model still sees in full: in the window and not
/// cleared.
fn visible_handles(conv: &Conv) -> BTreeSet<String> {
    let state = WindowState::from_value(&conv.context);
    conv.messages
        .iter()
        .filter(|m| m.idx >= state.start_idx && !state.cleared.contains_key(&m.idx))
        .filter(|m| EntryMeta::from_value(&m.meta).page_key.is_some())
        .filter_map(|m| m.handle.clone())
        .collect()
}
