//! Windows within a turn: building each request, making room when it's
//! too long (clearing is the builder's own lightest touch; then a
//! compaction; then a fresh window), and starting a fresh window when a
//! turn should not carry its whole history:
//! - a new message more than 6 hours after the last, or one that arrives
//!   when the window is already past the clearing line: a fresh window
//!   with the task state and the last two chats, no old turns;
//! - a turn picked up after a restart more than an hour later, or whose
//!   window is past the clearing line: a fresh window from the task state;
//! - more than 60 steps in one window: the same;
//! - a compaction that couldn't get under the line: the same.
//!
//! A fresh window is a new window in the same session, so the session's
//! taint (and the window's) goes on.

use oxbot_engine::{Activity, EngineEvent};
use oxbot_llm::{ErrorKind, Role};

use super::Turn;
use crate::budget::TokenBudget;
use crate::compact::Compaction;
use crate::context::header::Exchange;
use crate::context::state::{Fact, TaskState, WindowState};
use crate::context::window::{NewWindow, begin_window};
use crate::context::{Built, Pressure, TaskView};
use crate::meter::now_ms;
use crate::runner::{Caller, Stop};
use crate::session::{Conv, Core};
use crate::store::{ProgressNote, StoredMessage};

/// A new message after this long starts a fresh window.
const CHAT_GAP_MS: i64 = 6 * 3_600_000;
/// A turn picked up after this long starts a fresh window.
const RESUME_GAP_MS: i64 = 3_600_000;
/// Steps in one window before a fresh one.
const MAX_WINDOW_STEPS: usize = 60;
/// Progress notes in a fresh window.
const PROGRESS_SHOWN: usize = 5;
/// Earlier chats in a fresh window between tasks.
const RECENT_CHATS: usize = 2;
/// What the chat shows while the bot makes room.
const TIDYING: &str = "Tidying up its notes";

/// Why a fresh window starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fresh {
    /// A new message: carry the earlier task as background.
    NewTask,
    /// The same task goes on.
    SameTask,
}

impl Turn {
    /// The messages for one request within `budget`, making room when they
    /// don't fit (or when `force`d after the model said too long).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn build(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        budget: TokenBudget,
        ratio: f64,
        answering: &[String],
        caller: &Caller<'_>,
        cancel: &tokio_util::sync::CancellationToken,
        mut force: bool,
    ) -> Result<Built, Stop> {
        if let Some(kind) = self.fresh_due(core, conv, budget, ratio, answering, caller) {
            self.fresh_window(core, conv, budget, ratio, answering, caller, cancel, kind)
                .await?;
        }
        let mut stage = 0;
        loop {
            let built = self.view_build(core, conv, budget, ratio, answering, caller.person);
            if let Some(context) = built.context.clone() {
                conv.context = context;
                conv.context_dirty = true;
            }
            let fits = built.pressure == Pressure::Ok;
            match stage {
                0 if fits && !force => return Ok(built),
                1 if fits => return Ok(built),
                2 if built.est_tokens <= budget.usable() => return Ok(built),
                2 => {
                    return Err(Stop::Fail(
                        "This conversation is too long for the model's window, even after \
                         tidying it up. Send a shorter request."
                            .to_owned(),
                    ));
                }
                0 => {
                    self.compact(core, conv, budget, ratio, answering, caller, cancel)
                        .await?;
                    force = false;
                    stage = 1;
                }
                _ => {
                    // The summary and the last exchange still don't fit:
                    // a fresh window with the summary alone.
                    let task = WindowState::from_value(&conv.context).task_state;
                    self.start_fresh(core, conv, task.as_ref(), Fresh::SameTask, &[])?;
                    stage = 2;
                }
            }
        }
    }

    fn view_build(
        &self,
        core: &Core,
        conv: &Conv,
        budget: TokenBudget,
        ratio: f64,
        answering: &[String],
        person: Option<&str>,
    ) -> Built {
        let view = TaskView {
            system: &core.system,
            tools: &core.specs,
            knowledge: self.knowledge.as_ref(),
            messages: &conv.messages,
            context: &conv.context,
            person,
            now_ms: now_ms(),
            last_call: conv.last_call.as_ref(),
            turn: self.number,
            ratio,
            answering,
        };
        core.shared.setup.context.build(&view, budget)
    }

    /// Whether this step should start a fresh window, and why.
    fn fresh_due(
        &mut self,
        core: &Core,
        conv: &Conv,
        budget: TokenBudget,
        ratio: f64,
        answering: &[String],
        caller: &Caller<'_>,
    ) -> Option<Fresh> {
        let state = WindowState::from_value(&conv.context);
        if state.window == 0 {
            return None;
        }
        let in_window = |m: &&StoredMessage| m.idx >= state.start_idx;
        let first_check = !self.window_checked;
        self.window_checked = true;
        if first_check {
            let earlier = conv
                .messages
                .iter()
                .filter(in_window)
                .any(|m| m.turn < self.number);
            if !earlier {
                return None;
            }
            let long = || {
                self.view_build(core, conv, budget, ratio, answering, caller.person)
                    .est_tokens
                    > budget.clear_at()
            };
            if self.resumed {
                if self.idle_ms > RESUME_GAP_MS || long() {
                    return Some(Fresh::SameTask);
                }
            } else if self.idle_ms > CHAT_GAP_MS || long() {
                return Some(Fresh::NewTask);
            }
        }
        let steps = conv
            .messages
            .iter()
            .filter(in_window)
            .filter(|m| m.message.role == Role::Assistant)
            .count();
        (steps > MAX_WINDOW_STEPS).then_some(Fresh::SameTask)
    }

    /// Summarizes the window (keeping its last exchange) and starts the
    /// next window with it.
    #[allow(clippy::too_many_arguments)]
    async fn compact(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        budget: TokenBudget,
        ratio: f64,
        answering: &[String],
        caller: &Caller<'_>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<(), Stop> {
        let compaction = self
            .summarize(core, conv, budget, ratio, answering, caller, cancel, None)
            .await?;
        conv.context = compaction.context;
        conv.context_dirty = true;
        self.note(core, conv, compaction.progress_note, compaction.untrusted);
        self.save(core, conv, Vec::new(), None)
    }

    /// The compactor's run over the window (or the messages before
    /// `before`).
    #[allow(clippy::too_many_arguments)]
    async fn summarize(
        &self,
        core: &Core,
        conv: &Conv,
        budget: TokenBudget,
        ratio: f64,
        answering: &[String],
        caller: &Caller<'_>,
        cancel: &tokio_util::sync::CancellationToken,
        before: Option<u32>,
    ) -> Result<Compaction, Stop> {
        let _ = core
            .events
            .try_send(EngineEvent::Activity(Activity::Other(TIDYING.to_owned())));
        let helper = self.helper(core, caller, cancel);
        let messages: Vec<StoredMessage> = match before {
            Some(idx) => conv
                .messages
                .iter()
                .filter(|m| m.idx < idx)
                .cloned()
                .collect(),
            None => conv.messages.clone(),
        };
        let view = TaskView {
            system: &core.system,
            tools: &core.specs,
            knowledge: self.knowledge.as_ref(),
            messages: &messages,
            context: &conv.context,
            person: caller.person,
            now_ms: now_ms(),
            last_call: None,
            turn: self.number,
            ratio,
            answering,
        };
        core.shared
            .setup
            .compactor
            .compact(&view, budget, &helper)
            .await
            .map_err(|e| match e.kind {
                ErrorKind::Cancelled => Stop::Cancelled,
                _ => Stop::Fail(format!("oxbot couldn't tidy up the conversation: {e}")),
            })
    }

    /// Starts a fresh window: summarizes first, then a window with the task
    /// state, progress notes (and for a new message, the last chats).
    #[allow(clippy::too_many_arguments)]
    async fn fresh_window(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        budget: TokenBudget,
        ratio: f64,
        answering: &[String],
        caller: &Caller<'_>,
        cancel: &tokio_util::sync::CancellationToken,
        kind: Fresh,
    ) -> Result<(), Stop> {
        // A new message isn't part of what's summarized.
        let this_turn = conv
            .messages
            .iter()
            .find(|m| m.turn == self.number)
            .map(|m| m.idx);
        let before = if kind == Fresh::NewTask {
            this_turn
        } else {
            None
        };
        let compaction = self
            .summarize(core, conv, budget, ratio, answering, caller, cancel, before)
            .await?;
        let task = WindowState::from_value(&compaction.context).task_state;
        self.note(core, conv, compaction.progress_note, compaction.untrusted);
        let recent = if kind == Fresh::NewTask {
            recent_chats(&conv.messages, self.number)
        } else {
            Vec::new()
        };
        let task = match kind {
            Fresh::NewTask => task.map(as_background),
            Fresh::SameTask => task,
        };
        self.start_fresh(core, conv, task.as_ref(), kind, &recent)
    }

    /// Begins the fresh window and saves it.
    fn start_fresh(
        &self,
        core: &Core,
        conv: &mut Conv,
        task: Option<&TaskState>,
        kind: Fresh,
        recent: &[Exchange],
    ) -> Result<(), Stop> {
        let progress = core
            .shared
            .setup
            .store
            .progress(&core.id, PROGRESS_SHOWN)
            .unwrap_or_else(|err| {
                tracing::warn!(%err, "couldn't read the progress notes");
                Vec::new()
            });
        let start_idx = match kind {
            // The new message starts it.
            Fresh::NewTask => conv
                .messages
                .iter()
                .find(|m| m.turn == self.number)
                .map_or_else(|| conv.next_idx(), |m| m.idx),
            Fresh::SameTask => conv.next_idx(),
        };
        let mut state = WindowState::from_value(&conv.context);
        if kind == Fresh::NewTask {
            // The earlier task's list is in its task state now.
            state.todo.clear();
            state.last_todo_idx = None;
        }
        begin_window(
            &mut state,
            &NewWindow {
                start_idx,
                task_state: task,
                progress: &progress,
                recent,
                fresh: kind == Fresh::SameTask,
                now_ms: now_ms(),
                knowledge: self.knowledge.as_ref(),
                messages: &conv.messages,
            },
        );
        conv.context = state.to_value();
        conv.context_dirty = true;
        self.save(core, conv, Vec::new(), None)
    }

    /// Keeps a compaction's note for later windows.
    fn note(&self, core: &Core, conv: &Conv, note: Option<String>, untrusted: bool) {
        let Some(note) = note else { return };
        let state = WindowState::from_value(&conv.context);
        let tainted_by = state
            .tainted_by
            .clone()
            .or_else(|| untrusted.then(|| "outside text".to_owned()));
        let row = ProgressNote {
            turn: self.number,
            ms: now_ms(),
            note: note.chars().take(crate::tools::control::MAX_NOTE).collect(),
            tainted_by,
        };
        if let Err(err) = core.shared.setup.store.add_progress(&core.id, &row) {
            tracing::warn!(%err, "couldn't save a progress note");
        }
    }
}

/// The earlier task's state as background for a new one: its goal and
/// the person's later words become facts, and the new message is the goal
/// from now on.
fn as_background(mut task: TaskState) -> TaskState {
    let mut facts = Vec::new();
    if !task.goal.is_empty() {
        facts.push(Fact {
            text: format!("Earlier task: {}", clip(&task.goal, 380)),
            source: "user".to_owned(),
        });
    }
    for c in task.constraints.drain(..) {
        facts.push(Fact {
            text: format!("Earlier message: {}", clip(&c, 380)),
            source: "user".to_owned(),
        });
    }
    facts.append(&mut task.facts);
    let skip = facts.len().saturating_sub(crate::context::state::MAX_ITEMS);
    task.facts = facts.split_off(skip);
    task.goal.clear();
    task
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The last chats before turn `now`: what was asked and the final reply.
fn recent_chats(messages: &[StoredMessage], now: u32) -> Vec<Exchange> {
    let mut turns: Vec<u32> = messages
        .iter()
        .map(|m| m.turn)
        .filter(|t| *t < now)
        .collect();
    turns.dedup();
    let skip = turns.len().saturating_sub(RECENT_CHATS);
    turns
        .into_iter()
        .skip(skip)
        .filter_map(|turn| {
            let of_turn: Vec<&StoredMessage> = messages.iter().filter(|m| m.turn == turn).collect();
            let asked = of_turn
                .iter()
                .find(|m| m.message.role == Role::User)
                .map(|m| clip(&m.message.text(), 1_000))?;
            let replied = of_turn
                .iter()
                .rev()
                .find(|m| m.message.role == Role::Assistant && !m.message.text().trim().is_empty())
                .map(|m| clip(&m.message.text(), 1_000))
                .unwrap_or_default();
            Some(Exchange { asked, replied })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_llm::Message;
    use serde_json::json;

    fn m(idx: u32, turn: u32, message: Message) -> StoredMessage {
        StoredMessage {
            idx,
            turn,
            message,
            tokens: 0,
            untrusted: false,
            handle: None,
            meta: json!({}),
        }
    }

    #[test]
    fn recent_chats_are_the_last_two_asks_and_answers() {
        let msgs = [
            m(0, 0, Message::user("[Jay] one")),
            m(1, 0, Message::assistant("first", Vec::new())),
            m(2, 1, Message::user("[Jay] two")),
            m(3, 1, Message::assistant("", Vec::new())),
            m(4, 1, Message::assistant("second", Vec::new())),
            m(5, 2, Message::user("[Jay] three")),
            m(6, 2, Message::assistant("third", Vec::new())),
            m(7, 3, Message::user("[Jay] now")),
        ];
        let r = recent_chats(&msgs, 3);
        assert_eq!(
            r,
            [
                Exchange {
                    asked: "[Jay] two".into(),
                    replied: "second".into()
                },
                Exchange {
                    asked: "[Jay] three".into(),
                    replied: "third".into()
                }
            ]
        );
    }

    #[test]
    fn an_earlier_task_becomes_background() {
        let t = as_background(TaskState {
            goal: "[Jay] find a lamp".into(),
            constraints: vec!["[Jay] brass".into()],
            facts: vec![Fact {
                text: "Lamp A is $24".into(),
                source: "r3".into(),
            }],
            ..TaskState::default()
        });
        assert!(t.goal.is_empty() && t.constraints.is_empty());
        assert_eq!(t.facts[0].text, "Earlier task: [Jay] find a lamp");
        assert_eq!(t.facts[1].text, "Earlier message: [Jay] brass");
        assert_eq!(t.facts[2].source, "r3");
    }
}
