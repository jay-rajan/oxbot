//! A step's tool calls, in order: run through the session's tools
//! ([`crate::service`]: checked, asked of the gate, run) and saved. The
//! loop's own state is kept here, where the turn is: the to-do list and
//! progress notes, which results the model still sees, and the write-ahead
//! of calls that change something.

use std::collections::BTreeSet;

use oxbot_engine::EngineEvent;
use oxbot_llm::{Message, Part, Role, ToolCall};
use tokio_util::sync::CancellationToken;

use super::{End, ToSave, Turn};
use crate::context::state::{EntryKind, EntryMeta, WindowState, update_todo};
use crate::meter::now_ms;
use crate::runner::{Caller, RESTARTED_MID_CALL, SKIPPED, Stop};
use crate::service::{CallCtx, Dispatched, RepeatGuard, TurnHooks};
use crate::session::{Conv, Core};
use crate::store::{PendingCall, ProgressNote};
use crate::tools::ToolOutcome;
use crate::tools::control::{NOTE_PROGRESS, UPDATE_TODO};

/// Calls in a row the model got wrong before the turn stops.
const MAX_BAD_CALLS: u32 = 3;

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
                match self.one_call(core, conv, caller, &call, cancel).await {
                    Ok(Dispatched::Ran(outcome)) => {
                        *bad_calls = 0;
                        outcome
                    }
                    Ok(Dispatched::Invalid(outcome)) => {
                        *bad_calls += 1;
                        outcome
                    }
                    Ok(Dispatched::Refused(outcome)) => {
                        *bad_calls = 0;
                        refused = true;
                        outcome
                    }
                    Ok(Dispatched::Asked(question)) => {
                        *bad_calls = 0;
                        asked = Some(question);
                        ToolOutcome::note(crate::tools::control::ASKED, false)
                    }
                    Err(stop) => return Some(stop.into()),
                }
            };
            if outcome.error && outcome.meta.kind != EntryKind::Refusal {
                self.failures.count(&call, &mut outcome);
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

    /// One tool call, through the session's tools.
    async fn one_call(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        caller: &Caller<'_>,
        call: &ToolCall,
        cancel: &CancellationToken,
    ) -> Result<Dispatched, Stop> {
        let ctx = CallCtx {
            turn: self.number,
            step: self.step,
            budget: self.budget,
            person: caller.person,
            lane: caller.lane,
            cancel,
        };
        let mut seen = std::mem::take(&mut self.seen);
        let mut hooks = LoopHooks {
            turn: self,
            core,
            conv,
            seen: &mut seen,
        };
        let done = core.tools.dispatch(call, &ctx, &mut hooks).await;
        self.seen = seen;
        done
    }
}

/// What the loop keeps for the session's tools: its to-do list and
/// progress notes, the window (which results the model still sees), and
/// the write-ahead.
struct LoopHooks<'a> {
    turn: &'a Turn,
    core: &'a Core,
    conv: &'a mut Conv,
    seen: &'a mut RepeatGuard,
}

impl TurnHooks for LoopHooks<'_> {
    fn own_tool(&mut self, call: &ToolCall, turn: u32) -> ToolOutcome {
        let error = |text: String| {
            let mut o = ToolOutcome::note(format!("[tool error] {text}"), true);
            o.meta.tool = Some(call.name.clone());
            o
        };
        let named = |mut o: ToolOutcome| {
            o.meta.tool = Some(call.name.clone());
            o
        };
        let conv = &mut *self.conv;
        match call.name.as_str() {
            UPDATE_TODO => {
                let mut state = WindowState::from_value(&conv.context);
                match update_todo(&mut state.todo, &call.arguments) {
                    Ok(text) => {
                        state.last_todo_idx = last_assistant(conv);
                        state.recite_next = false;
                        conv.context = state.to_value();
                        conv.context_dirty = true;
                        named(ToolOutcome::note(text, false).kind(EntryKind::Todo))
                    }
                    Err(why) => error(why),
                }
            }
            NOTE_PROGRESS => match crate::tools::control::progress_note(&call.arguments) {
                Ok(note) => {
                    let tainted_by = WindowState::from_value(&conv.context).tainted_by;
                    let row = ProgressNote {
                        turn,
                        ms: now_ms(),
                        note,
                        tainted_by,
                    };
                    match self
                        .core
                        .shared
                        .setup
                        .store
                        .add_progress(&self.core.id, &row)
                    {
                        Ok(()) => named(ToolOutcome::note(
                            "Noted. It's shown again if the conversation starts over.",
                            false,
                        )),
                        Err(err) => error(format!("the note couldn't be saved: {err}")),
                    }
                }
                Err(why) => error(why),
            },
            other => error(format!("unknown tool {other}")),
        }
    }

    fn before_side_effect(&mut self, call: &ToolCall) -> Result<(), Stop> {
        // Written down first: after a restart it isn't run again.
        let ahead = PendingCall {
            id: call.id.clone(),
            name: call.name.clone(),
            args: call.raw.clone(),
            side_effect: true,
        };
        self.turn
            .save(self.core, self.conv, Vec::new(), Some(ahead))
    }

    fn visible_handles(&self) -> BTreeSet<String> {
        visible_handles(self.conv)
    }

    fn next_mark(&self) -> u32 {
        // Saved next, as the conversation's next message.
        self.conv.next_idx()
    }

    fn in_view(&self, mark: u32) -> bool {
        let state = WindowState::from_value(&self.conv.context);
        mark >= state.start_idx && !state.cleared.contains_key(&mark)
    }

    fn repeats(&mut self) -> &mut RepeatGuard {
        self.seen
    }
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
