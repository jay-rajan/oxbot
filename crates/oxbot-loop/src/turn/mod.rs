//! One turn, one step at a time.
//!
//! Each step: pick a model, build the context for it (making room if
//! needed: [`windows`]), wait for the pacer, stream the answer, save it.
//! Then each tool call it asked for, in order: check it, ask the gate, run
//! it (a call that changes something is written down first), save its
//! result ([`tools`]). A turn ends when the model answers without a tool
//! call (after one check of that answer when the turn used tools:
//! [`check`]), asks the person something, fails, or is stopped. A restart
//! picks a turn up after its last saved step.

mod check;
pub use check::{CHECK_AFTER_CALLS, MAX_NAMED, note as check_note};
mod tools;
mod windows;

use std::sync::Arc;

use oxbot_engine::{Activity, CallKind, EngineEvent, GateMode, ToolUse, TurnInput};
use oxbot_llm::{ChatResponse, Message, Role, ToolCall};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::budget::{TokenBudget, est_message, est_tools};
use crate::context::LastCall;
use crate::context::normalize::normalize_message;
use crate::context::state::{EntryMeta, WindowState};
use crate::meter::now_ms;
use crate::pacer::Lane;
use crate::router::TurnRouting;
use crate::runner::{Caller, HelperCaller, Next, Stop};
use crate::session::{Conv, Core};
use crate::store::{CheckRow, PendingCall, StoredMessage, TurnRow, TurnState, TurnStatus};
use crate::{KnowledgeBlock, KnowledgeQuery};

/// What a turn's question is saved as, at most.
const MAX_QUESTION: usize = 2_000;
/// Times a request is asked again after a tool call that couldn't be used.
const MAX_REASKS: u32 = 2;

/// How a turn ends.
pub(crate) enum End {
    Done(String),
    Waiting(String),
    Failed(String),
    Stopped,
}

impl From<Stop> for End {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Cancelled => Self::Stopped,
            Stop::Fail(m) => Self::Failed(m),
        }
    }
}

/// One message to save: the message, whether it's outside text, its
/// stored handle, and the builder's notes.
pub(crate) struct ToSave {
    pub message: Message,
    pub untrusted: bool,
    pub handle: Option<String>,
    pub meta: EntryMeta,
}

impl ToSave {
    fn plain(message: Message) -> Self {
        Self {
            message,
            untrusted: false,
            handle: None,
            meta: EntryMeta::default(),
        }
    }
}

/// Runs one turn of `core`'s session.
pub(crate) async fn run_turn(core: Arc<Core>, input: TurnInput, cancel: CancellationToken) {
    let started = Instant::now();
    let mut conv = core.conv.lock().await;
    let mut turn = Turn::begin(&core, &mut conv, &input);
    let end = match &mut turn {
        Ok(t) => t.run(&core, &mut conv, &input, &cancel).await,
        Err(stop) => End::from(stop.clone()),
    };
    let number = turn.as_ref().ok().map(|t| t.number);
    if core.is_stopping() && matches!(end, End::Stopped) {
        // oxbot is stopping: the turn stays saved as running, to go on
        // with after the restart.
        return;
    }
    let (status, ok, text) = match end {
        End::Done(text) => (TurnStatus::Done, true, text),
        End::Waiting(question) => (TurnStatus::WaitingUser, true, question),
        End::Failed(message) => (TurnStatus::Failed, false, message),
        End::Stopped => (TurnStatus::Stopped, false, "Stopped.".to_owned()),
    };
    if let Some(number) = number
        && let Err(err) = core.shared.setup.store.end_turn(&core.id, number, status)
    {
        tracing::warn!(bot = %core.bot, %err, "couldn't save the end of a turn");
    }
    let _ = core
        .events
        .send(EngineEvent::TurnDone {
            ok,
            text,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            usage: None,
        })
        .await;
}

/// One turn as it runs.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Turn {
    pub number: u32,
    pub step: u32,
    model: Option<String>,
    /// Tool calls left over from before a restart: (call, its write-ahead).
    leftover: Vec<(ToolCall, Option<PendingCall>)>,
    knowledge: Option<KnowledgeBlock>,
    knowledge_asked: bool,
    started_model: bool,
    /// Picked up after a restart.
    resumed: bool,
    /// How long the session sat still before this turn, in ms.
    idle_ms: i64,
    /// The start-of-turn check on the window is done.
    window_checked: bool,
    /// The budget of the last request (tool results are cut to it).
    budget: TokenBudget,
    /// Read-only loop calls already made this turn.
    seen: crate::service::RepeatGuard,
    /// Calls that failed this turn.
    failures: crate::service::Failures,
    /// The step budget's wrap-up note was given.
    nudged: bool,
    /// The note to add to this step's last result, if it's due now.
    pub(super) wrap_up: Option<String>,
    /// The final answer was sent back once to be checked.
    bounced: bool,
    /// When.
    bounced_at: Option<Instant>,
    /// Values the first answer had that nothing read had.
    ungrounded_before: Option<Vec<String>>,
    /// Notes added to the end of the next request only (asking again
    /// after a tool call that couldn't be used), never saved.
    reask: Vec<Message>,
}

impl Turn {
    /// Starts a new turn, or picks up the one that was running.
    fn begin(core: &Core, conv: &mut Conv, input: &TurnInput) -> Result<Self, Stop> {
        let store = &core.shared.setup.store;
        let idle_ms = {
            let last = WindowState::from_value(&conv.context).last_ms;
            if last > 0 { now_ms() - last } else { 0 }
        };
        if let Some(row) = core.take_resume() {
            if row.user_seq == input.user_seq {
                return Ok(Self::resume(core, conv, &row, idle_ms));
            }
            // Not picked up: it ended when oxbot stopped.
            let _ = store.end_turn(&row.session, row.turn, TurnStatus::Stopped);
        }
        let number = conv.next_turn;
        conv.next_turn += 1;
        let question: String = input.text.chars().take(MAX_QUESTION).collect();
        let row = TurnRow {
            session: core.id.clone(),
            turn: number,
            bot: core.bot.clone(),
            user_seq: input.user_seq,
            origin: input.origin.clone(),
            question,
            status: TurnStatus::Running,
            step: 0,
            pending: None,
            model: None,
            resumes: 0,
            dry_run: core.gate_mode == GateMode::DryRun,
        };
        store
            .begin_turn(&row)
            .map_err(|e| Stop::Fail(format!("oxbot couldn't save the task: {e}")))?;
        let turn = Self::new(number, 0, None, Vec::new(), false, idle_ms);
        // A task passed on by a bot that had read outside text carries it.
        if let Some(why) = input.origin.get("tainted_by").and_then(Value::as_str) {
            let mut state = WindowState::from_value(&conv.context);
            state.taint(&format!("a task passed on after reading {why}"));
            conv.context = state.to_value();
            conv.context_dirty = true;
        }
        let user = Message::user(input.text.clone());
        turn.save(core, conv, vec![ToSave::plain(user)], None)?;
        Ok(turn)
    }

    fn new(
        number: u32,
        step: u32,
        model: Option<String>,
        leftover: Vec<(ToolCall, Option<PendingCall>)>,
        resumed: bool,
        idle_ms: i64,
    ) -> Self {
        Self {
            number,
            step,
            model,
            leftover,
            knowledge: None,
            knowledge_asked: false,
            started_model: false,
            resumed,
            idle_ms,
            window_checked: false,
            budget: TokenBudget::for_model(64_000, None),
            seen: crate::service::RepeatGuard::default(),
            failures: crate::service::Failures::default(),
            nudged: false,
            wrap_up: None,
            bounced: false,
            bounced_at: None,
            ungrounded_before: None,
            reask: Vec::new(),
        }
    }

    /// Picks up `row` after a restart.
    fn resume(core: &Core, conv: &Conv, row: &TurnRow, idle_ms: i64) -> Self {
        let _ = core.shared.setup.store.turn_resumed(&row.session, row.turn);
        let _ = core.events.try_send(EngineEvent::Activity(Activity::Other(
            "Picking up where it left off after a restart".to_owned(),
        )));
        // Calls of the last answer whose result wasn't saved.
        let mut leftover = Vec::new();
        let in_turn: Vec<&StoredMessage> = conv
            .messages
            .iter()
            .filter(|m| m.turn == row.turn)
            .collect();
        if let Some(pos) = in_turn
            .iter()
            .rposition(|m| m.message.role == Role::Assistant)
        {
            let answered: Vec<&str> = in_turn[pos + 1..]
                .iter()
                .filter_map(|m| m.message.tool_call_id.as_deref())
                .collect();
            for call in &in_turn[pos].message.tool_calls {
                if !answered.contains(&call.id.as_str()) {
                    let pending = row.pending.clone().filter(|p| p.id == call.id);
                    leftover.push((call.clone(), pending));
                }
            }
        }
        Self::new(
            row.turn,
            row.step,
            row.model.clone(),
            leftover,
            true,
            idle_ms,
        )
    }

    /// One checkpoint: these messages, the turn's state, and the context
    /// (with the time the session last moved).
    pub(crate) fn save(
        &self,
        core: &Core,
        conv: &mut Conv,
        add: Vec<ToSave>,
        pending: Option<PendingCall>,
    ) -> Result<(), Stop> {
        let mut idx = conv.next_idx();
        let stored: Vec<StoredMessage> = add
            .into_iter()
            .map(|m| {
                // Cleaned once, so resending never changes a byte.
                let message = normalize_message(m.message);
                let tokens = u32::try_from(est_message(&message)).unwrap_or(u32::MAX);
                let s = StoredMessage {
                    idx,
                    turn: self.number,
                    message,
                    tokens,
                    untrusted: m.untrusted,
                    handle: m.handle,
                    meta: m.meta.to_value(),
                };
                idx += 1;
                s
            })
            .collect();
        let mut state = WindowState::from_value(&conv.context);
        state.last_ms = now_ms();
        conv.context = state.to_value();
        let state_now = TurnState {
            step: self.step,
            model: self.model.clone(),
            pending,
        };
        core.shared
            .setup
            .store
            .checkpoint(
                &core.id,
                self.number,
                &stored,
                &state_now,
                Some(&conv.context),
            )
            .map_err(|e| Stop::Fail(format!("oxbot couldn't save the task's progress: {e}")))?;
        conv.context_dirty = false;
        conv.messages.extend(stored);
        Ok(())
    }

    async fn run(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        input: &TurnInput,
        cancel: &CancellationToken,
    ) -> End {
        let lane = if core.gate_mode == GateMode::DryRun {
            Lane::Trial
        } else {
            Lane::Live
        };
        let person = input.person.as_deref();
        let mut caller = self.caller(core, conv, person, lane, cancel);
        let mut bad_calls = 0;
        let mut reasks = 0;
        if !self.leftover.is_empty() {
            let calls = std::mem::take(&mut self.leftover);
            if let Some(end) = self
                .tool_calls(core, conv, &caller, calls, &mut bad_calls, cancel)
                .await
            {
                return end;
            }
        }
        let max_steps = core
            .shared
            .setup
            .prefs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .max_steps;
        loop {
            if cancel.is_cancelled() {
                return End::Stopped;
            }
            if self.step >= max_steps {
                return self.step_limit(core, conv, max_steps);
            }
            caller.step = self.step;
            let resp = match self
                .call(core, conv, &mut caller, &input.text, cancel)
                .await
            {
                Ok(r) => r,
                Err(stop) => {
                    conv.excluded.clone_from(&caller.routing.excluded);
                    return stop.into();
                }
            };
            conv.excluded.clone_from(&caller.routing.excluded);
            // A call that can't be used: the same request again with a
            // note, up to twice; the broken answer isn't kept (so it isn't
            // copied). Then it goes on as a tool error.
            if reasks < MAX_REASKS
                && let Some(note) = core.tools.registry().reask_note(&resp.message.tool_calls)
            {
                reasks += 1;
                self.reask = vec![Message::user(note)];
                continue;
            }
            reasks = 0;
            self.reask.clear();
            self.step += 1;
            self.model.clone_from(&caller.routing.sticky);
            let message = resp.message;
            let text = message.text();
            let calls = message.tool_calls.clone();
            // A final answer reaches the chat only once it's accepted.
            if !calls.is_empty() {
                send_assistant(core, &text, &calls).await;
            }
            if let Err(stop) = self.save(core, conv, vec![ToSave::plain(message)], None) {
                return stop.into();
            }
            if calls.is_empty() {
                match self.answer_check(core, conv, &input.text, &text, max_steps) {
                    check::Checked::Bounce(note) => match self.bounce(core, conv, note) {
                        Ok(()) => continue,
                        Err(stop) => return stop.into(),
                    },
                    check::Checked::Accept(row) => return self.accept(core, text, row).await,
                }
            }
            let calls = calls.into_iter().map(|c| (c, None)).collect();
            self.wrap_up = wrap_up_note(self.step, max_steps, &mut self.nudged);
            if let Some(end) = self
                .tool_calls(core, conv, &caller, calls, &mut bad_calls, cancel)
                .await
            {
                return end;
            }
        }
    }

    /// Ends the turn at its step limit, saying so in the conversation too,
    /// so the next message reads as a new one, not as more steps of this
    /// task.
    fn step_limit(&self, core: &Core, conv: &mut Conv, max_steps: u32) -> End {
        let stop = Message::assistant(
            format!(
                "(I stopped here: this task reached its limit of {max_steps} steps before I \
                 finished it.)"
            ),
            Vec::new(),
        );
        if let Err(stop) = self.save(core, conv, vec![ToSave::plain(stop)], None) {
            return stop.into();
        }
        End::Failed(format!(
            "This task took {max_steps} steps without finishing, so it stopped. Send what's \
             left as a new, smaller request."
        ))
    }

    /// Accepts the final answer `text`: records its check and tells the
    /// hub.
    async fn accept(&self, core: &Core, text: String, row: Option<CheckRow>) -> End {
        if let Some(row) = row
            && let Err(err) = core.shared.setup.store.record_check(&row)
        {
            tracing::warn!(%err, "couldn't save a turn's answer check");
        }
        send_assistant(core, &text, &[]).await;
        End::Done(text)
    }

    /// The turn's model caller.
    fn caller<'a>(
        &self,
        core: &'a Core,
        conv: &Conv,
        person: Option<&'a str>,
        lane: Lane,
        cancel: &'a CancellationToken,
    ) -> Caller<'a> {
        Caller {
            shared: &core.shared,
            model: Arc::clone(&core.shared.setup.model),
            bot: &core.bot,
            person,
            session: Some(&core.id),
            turn: Some(self.number),
            step: self.step,
            privacy: core.privacy,
            lane,
            kind: CallKind::Chat,
            engine: "openrouter",
            events: Some(&core.events),
            meter: Some(&core.events),
            cancel,
            routing: TurnRouting {
                choice: core.choice.clone(),
                central: core.central,
                sticky: self.model.clone(),
                excluded: conv.excluded.clone(),
                no_provider: std::collections::HashSet::new(),
                min_window: None,
            },
            zdr_dropped: false,
            same_retries: 0,
            cooled_for: std::time::Duration::ZERO,
            last_error: None,
            data_policy: false,
        }
    }

    /// A caller for helper requests within the turn (compaction, ask_page):
    /// the turn's bot, person, privacy, lane and stop.
    fn helper<'a>(
        &self,
        core: &'a Core,
        caller: &Caller<'a>,
        cancel: &'a CancellationToken,
    ) -> HelperCaller<'a> {
        HelperCaller {
            shared: &core.shared,
            bot: &core.bot,
            person: caller.person,
            session: Some(&core.id),
            turn: Some(self.number),
            step: self.step,
            privacy: core.privacy,
            lane: caller.lane,
            choice: core.choice.clone(),
            cancel,
            meter: Some(&core.events),
        }
    }

    /// One model request: pick, budget, build, make room, send, sort out
    /// errors.
    async fn call(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        caller: &mut Caller<'_>,
        text: &str,
        cancel: &CancellationToken,
    ) -> Result<ChatResponse, Stop> {
        let mut squeezed = false;
        loop {
            let route = caller.next_route().await?;
            if !self.started_model {
                self.started_model = true;
                let _ = core
                    .events
                    .send(EngineEvent::Started {
                        model: route.primary.id.clone(),
                    })
                    .await;
            }
            // The budget of the smallest window that may answer.
            let budget = TokenBudget::for_model(route.window, Some(route.max_tokens));
            self.budget = budget;
            if !self.knowledge_asked {
                self.knowledge_asked = true;
                self.knowledge = knowledge(core, caller.person, text, self.number, budget);
            }
            let answering: Vec<String> = std::iter::once(route.primary.id.clone())
                .chain(route.fallbacks.iter().cloned())
                .collect();
            let ratio = core.shared.calibration.ratio_of_any(&answering);
            let built = self
                .build(
                    core, conv, budget, ratio, &answering, caller, cancel, squeezed,
                )
                .await?;
            let mut messages = built.messages.clone();
            messages.extend(self.reask.iter().cloned());
            with_tail(&mut messages, core.tools.has_browser());
            let estimated: u64 =
                messages.iter().map(est_message).sum::<u64>() + est_tools(&core.specs);
            match caller.send(&route, messages, core.specs.clone()).await {
                Ok(resp) => {
                    let answered = if resp.model.is_empty() {
                        route.primary.id.clone()
                    } else {
                        resp.model.clone()
                    };
                    if let Some(row) = core.shared.calibration.observe(
                        &answered,
                        estimated,
                        resp.usage.prompt_tokens,
                    ) && let Err(err) = core.shared.setup.store.save_calibration(&row)
                    {
                        tracing::warn!(%err, "couldn't save the token calibration");
                    }
                    conv.last_call = Some(LastCall {
                        model: answered,
                        prompt_tokens: resp.usage.prompt_tokens,
                        completion_tokens: resp.usage.completion_tokens,
                        messages_sent: built.messages.len(),
                        last_idx: built.last_idx,
                        generation: built.generation,
                    });
                    caller.answered(&route, &resp);
                    return Ok(resp);
                }
                Err(err) => match caller.after_error(&route, &err).await {
                    Next::Again => {}
                    Next::Compact if !squeezed => {
                        // Too long for the model after all: count higher for
                        // it from now on and make room before trying again.
                        squeezed = true;
                        for model in &answering {
                            if let Some(row) = core.shared.calibration.overflowed(model)
                                && let Err(err) = core.shared.setup.store.save_calibration(&row)
                            {
                                tracing::warn!(%err, "couldn't save the token calibration");
                            }
                        }
                    }
                    Next::Compact => {
                        return Err(Stop::Fail(format!(
                            "This conversation is too long for {}, even after tidying it up. \
                             Send a shorter request.",
                            route.primary.id
                        )));
                    }
                    Next::Stop(stop) => return Err(stop),
                },
            }
        }
    }
}

/// The rules again at the very end of every request (models follow what
/// they read last), after everything a provider can cache, and never
/// saved: the same bytes every time.
pub(crate) const TAIL: &str = "[oxbot] Answer only from what you read. Use query_rows for \
    counting. Check every part of the task before your final answer.";
/// The same for a bot without a browser (no query_rows).
pub(crate) const TAIL_NO_BROWSER: &str = "[oxbot] Answer only from what you read. Check every part of the task before your final answer.";

/// Adds the tail to the end of the last message of a request.
fn with_tail(messages: &mut [Message], browser: bool) {
    let tail = if browser { TAIL } else { TAIL_NO_BROWSER };
    let Some(last) = messages.last_mut() else {
        return;
    };
    match last.parts.iter_mut().rev().find_map(|p| match p {
        oxbot_llm::Part::Text(t) => Some(t),
        oxbot_llm::Part::Image { .. } => None,
    }) {
        Some(text) => {
            text.push_str("\n\n");
            text.push_str(tail);
        }
        None => last.parts.push(oxbot_llm::Part::Text(tail.to_owned())),
    }
}

/// Tells the hub what the model said and called.
async fn send_assistant(core: &Core, text: &str, calls: &[ToolCall]) {
    let _ = core
        .events
        .send(EngineEvent::Assistant {
            text: text.to_owned(),
            tool_uses: calls
                .iter()
                .map(|c| ToolUse {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    input: c.arguments.clone(),
                })
                .collect(),
        })
        .await;
}

/// Share of the step budget after which the model is told to wrap up.
const WRAP_UP_PERCENT: u32 = 70;

/// The wrap-up note once `step` reaches 70% of `max_steps` (once:
/// `nudged` says it was given).
pub fn wrap_up_note(step: u32, max_steps: u32, nudged: &mut bool) -> Option<String> {
    let at = (max_steps * WRAP_UP_PERCENT).div_ceil(100);
    if *nudged || step < at || step >= max_steps {
        return None;
    }
    *nudged = true;
    let left = max_steps - step;
    Some(format!(
        "[oxbot] You have {left} steps left: finish with what you have, and say what's missing."
    ))
}

/// What the bot knows for this turn, if there's a knowledge source.
fn knowledge(
    core: &Core,
    person: Option<&str>,
    text: &str,
    turn: u32,
    budget: TokenBudget,
) -> Option<KnowledgeBlock> {
    let source = core.shared.setup.knowledge.as_ref()?;
    let sites: Vec<String> = core.current_url().into_iter().collect();
    let block = source.for_turn(&KnowledgeQuery {
        bot: &core.bot,
        person,
        text,
        sites: &sites,
        budget_tokens: u32::try_from(budget.knowledge()).unwrap_or(u32::MAX),
    });
    if !block.ids.is_empty() {
        source.used(&block.ids, &core.id, turn);
    }
    Some(block)
}
