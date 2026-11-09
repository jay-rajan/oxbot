//! The Codex engine: [`CodexEngine`] starts sessions, [`CodexSession`]
//! runs their turns on a Codex thread.
//!
//! A turn: the bot's process is started if needed, the session's thread
//! is loaded (resumed after a restart, or started with oxbot's own
//! instructions and tools), the relay is told whose turn it is, and the
//! person's message goes in with `turn/start`. Then, until Codex says the
//! turn is complete:
//!
//! - text streams to the chat as it comes;
//! - each tool Codex calls (`item/tool/call`) runs through the session's
//!   [`ToolService`], the same one the loop uses: the gate and its cards,
//!   page snapshots stored and shown small, outside text marked; one call
//!   at a time, and after a refusal the rest of that step is skipped;
//! - every model request is metered by the relay and shown in the turn's
//!   footer;
//! - the limits Codex lacks are kept here: steps (model requests) and
//!   minutes per turn (time waiting on the person doesn't count), and the
//!   bot's daily requests (the relay stops the request); at a limit the
//!   turn is stopped (`turn/interrupt`) and the chat says why.
//!
//! A final answer after two or more tool calls is sent back once to be
//! checked, as the loop does (a follow-up turn with oxbot's note), and the
//! check is recorded. Anything Codex itself asks to approve (commands,
//! file changes, permissions, elicitations) is declined and logged: its
//! shell is off, so none should come.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use oxbot_engine::{
    Activity, CallRecord, Engine, EngineError, EngineEvent, EngineKind, GateMode, NotReady,
    Session, SessionSpec, Started, ToolUse, TurnInput,
};
use oxbot_llm::{Part, ToolCall};
use oxbot_loop::budget::TokenBudget;
use oxbot_loop::context::state::{EntryKind, TodoItem, update_todo};
use oxbot_loop::numbers::{self, Evidence};
use oxbot_loop::pacer::Lane;
use oxbot_loop::store::CheckRow;
use oxbot_loop::tools::ToolOutcome;
use oxbot_loop::tools::control::{ASKED, UPDATE_TODO};
use oxbot_loop::tools::page::ASK_PAGE;
use oxbot_loop::{
    CHECK_AFTER_CALLS, CallCtx, Dispatched, Failures, LoopEngine, MAX_NAMED, OwnTools, RepeatGuard,
    Stop, ToolService, TurnHooks, answer_after_check, check_note, wrap_up_note,
};
use oxbot_relay::{ClientInfo, Relay, TurnStart};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::binary::Found;
use crate::guard::RefuseProxy;
use crate::names::ToolNames;
use crate::rpc::{Incoming, RpcError};
use crate::store::{CodexStore, ThreadRow};
use crate::supervisor::{ProcessKey, Running, Supervisor, process_key};

/// Room for events the hub hasn't read yet.
const EVENT_ROOM: usize = 256;
/// How long a stopping session waits for its turn to wind down.
const GRACE: Duration = Duration::from_secs(5);
/// Calls in a row the model got wrong before the turn stops.
const MAX_BAD_CALLS: u32 = 3;
/// What the chat shows while the bot checks.
const CHECKING: &str = "Checking its answer";
/// What a skipped call tells the model.
const SKIPPED: &str = "[skipped] An earlier call in this step was refused, so this one didn't run.";
/// Longest wait for Codex to say a turn it was asked to stop has stopped.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// Most times a thread start drops a tool whose schema Codex refuses.
const MAX_DROPPED_TOOLS: usize = 8;

/// What the engine runs with. The host builds it once.
pub struct CodexSetup {
    /// What Codex bots share with oxbot (`oxbot-loop`): tools, gateway,
    /// prefs and checks.
    pub loop_engine: Arc<LoopEngine>,
    /// oxbot's model relay.
    pub relay: Arc<Relay>,
    /// Where threads are kept.
    pub store: Arc<dyn CodexStore>,
    /// The `codex` program, as found.
    pub found: Found,
    /// A process nothing used this long is stopped.
    pub idle_stop: Duration,
    /// Longest a turn may work (time waiting on the person aside).
    pub max_turn: Duration,
}

struct Shared {
    loop_engine: Arc<LoopEngine>,
    store: Arc<dyn CodexStore>,
    found: RwLock<Found>,
    supervisor: RwLock<Option<Arc<Supervisor>>>,
    relay: Arc<Relay>,
    idle_stop: Duration,
    max_turn: Duration,
    guard: RefuseProxy,
}

impl Shared {
    fn supervisor(&self) -> Option<Arc<Supervisor>> {
        self.supervisor
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn supervisor_for(&self, found: &Found) -> Option<Arc<Supervisor>> {
        found.binary().map(|b| {
            Arc::new(Supervisor::new(
                b.clone(),
                Arc::clone(&self.relay),
                self.guard.url().to_owned(),
                self.idle_stop,
            ))
        })
    }
}

/// The Codex engine. One per hub.
pub struct CodexEngine {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for CodexEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexEngine")
            .field("found", &read(&self.shared.found))
            .finish_non_exhaustive()
    }
}

fn read<T: Clone>(lock: &RwLock<T>) -> T {
    lock.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CodexEngine {
    /// The engine with `setup`; its refusing proxy and idle sweep start.
    ///
    /// # Errors
    /// The refusing proxy couldn't start.
    pub async fn new(setup: CodexSetup) -> std::io::Result<Self> {
        let guard = RefuseProxy::start().await?;
        let shared = Arc::new(Shared {
            loop_engine: setup.loop_engine,
            store: setup.store,
            found: RwLock::new(Found::Unusable {
                path: None,
                version: None,
                why: String::new(),
            }),
            supervisor: RwLock::new(None),
            relay: setup.relay,
            idle_stop: setup.idle_stop,
            max_turn: setup.max_turn,
            guard,
        });
        set_found(&shared, setup.found);
        let weak = Arc::downgrade(&shared);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_mins(1)).await;
                let Some(shared) = weak.upgrade() else { return };
                if let Some(s) = shared.supervisor() {
                    s.stop_idle().await;
                }
            }
        });
        Ok(Self { shared })
    }

    /// What was found of Codex.
    #[must_use]
    pub fn found(&self) -> Found {
        read(&self.shared.found)
    }

    /// Connections Codex tried to anything but the relay (all refused).
    #[must_use]
    pub fn refused_connections(&self) -> u64 {
        self.shared.guard.refused()
    }

    /// Uses what `found` says from now on (Codex was looked for again):
    /// new sessions start on it; running ones finish on what they had.
    pub fn use_found(&self, found: Found) {
        set_found(&self.shared, found);
    }

    /// Stops every Codex process.
    pub async fn shutdown(&self) {
        if let Some(s) = self.shared.supervisor() {
            s.stop_all().await;
        }
    }
}

fn set_found(shared: &Shared, found: Found) {
    let supervisor = shared.supervisor_for(&found);
    *shared
        .supervisor
        .write()
        .unwrap_or_else(PoisonError::into_inner) = supervisor;
    *shared.found.write().unwrap_or_else(PoisonError::into_inner) = found;
}

impl Engine for CodexEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Codex
    }

    fn ready(&self) -> Result<(), NotReady> {
        if let Some(why) = read(&self.shared.found).problem() {
            return Err(NotReady(why.to_owned()));
        }
        // Codex's requests go to OpenRouter on the household's key.
        self.shared.loop_engine.ready()
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<Started, EngineError>> {
        Box::pin(async move {
            self.ready()?;
            CodexSession::start(Arc::clone(&self.shared), spec).await
        })
    }
}

/// The session's thread, as loaded in a process.
#[derive(Debug, Default)]
struct ThreadState {
    id: Option<String>,
    /// The process start it's loaded in.
    loaded: Option<u64>,
    model: String,
    window: u64,
    budget: Option<TokenBudget>,
}

/// What a session's turns share.
struct Core {
    shared: Arc<Shared>,
    /// The supervisor this session's process belongs to.
    supervisor: Arc<Supervisor>,
    spec: SessionSpec,
    key: ProcessKey,
    tools: ToolService,
    names: ToolNames,
    events: mpsc::Sender<EngineEvent>,
    thread: tokio::sync::Mutex<ThreadState>,
    /// The model's to-do list (`update_todo`).
    todo: Mutex<Vec<TodoItem>>,
}

/// A running Codex session.
pub struct CodexSession {
    core: Arc<Core>,
    turn: Option<(CancellationToken, JoinHandle<()>)>,
}

impl CodexSession {
    async fn start(shared: Arc<Shared>, spec: SessionSpec) -> Result<Started, EngineError> {
        let Some(supervisor) = shared.supervisor() else {
            return Err(EngineError::NotReady(NotReady(
                "The AI helper isn't installed on the home computer.".to_owned(),
            )));
        };
        let (events, events_rx) = mpsc::channel(EVENT_ROOM);
        let tools = shared
            .loop_engine
            .open_tools(&spec, events.clone(), OwnTools::NoProgressNotes)
            .await;
        let names = ToolNames::new(&tools.specs());
        let key = process_key(&spec);
        supervisor.hold(&key).await;
        let core = Arc::new(Core {
            supervisor,
            shared: Arc::clone(&shared),
            key,
            names,
            tools,
            events,
            thread: tokio::sync::Mutex::new(ThreadState::default()),
            todo: Mutex::new(Vec::new()),
            spec,
        });
        Ok(Started {
            session: Box::new(Self { core, turn: None }),
            events: events_rx,
        })
    }
}

impl Session for CodexSession {
    fn id(&self) -> &str {
        self.core.spec.resume.id()
    }

    fn send(&mut self, turn: TurnInput) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            if self
                .turn
                .as_ref()
                .is_some_and(|(_, task)| !task.is_finished())
            {
                return Err(EngineError::Other(
                    "The bot is still working on the last message.".to_owned(),
                ));
            }
            let cancel = CancellationToken::new();
            let task = tokio::spawn(run_turn(Arc::clone(&self.core), turn, cancel.clone()));
            self.turn = Some((cancel, task));
            Ok(())
        })
    }

    fn interrupt(&mut self) -> BoxFuture<'_, Result<(), EngineError>> {
        if let Some((cancel, _)) = &self.turn {
            cancel.cancel();
        }
        Box::pin(async { Ok(()) })
    }

    fn shutdown(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        let turn = self.turn.take();
        let core = Arc::clone(&self.core);
        Box::pin(async move {
            if let Some((cancel, task)) = turn {
                cancel.cancel();
                if tokio::time::timeout(GRACE, task).await.is_err() {
                    tracing::warn!(bot = %core.spec.bot, "a Codex turn didn't stop in time");
                }
            }
            core.tools.shutdown().await;
            core.supervisor.release(&core.key).await;
        })
    }
}

impl Drop for CodexSession {
    fn drop(&mut self) {
        if let Some((cancel, _)) = self.turn.take() {
            cancel.cancel();
        }
    }
}

/// How a turn ends.
enum End {
    Done(String),
    Waiting(String),
    Failed(String),
    Stopped,
}

/// Runs one turn and tells the hub how it ended.
async fn run_turn(core: Arc<Core>, input: TurnInput, cancel: CancellationToken) {
    let started = Instant::now();
    let end = Turn::run(&core, &input, &cancel).await;
    let (ok, text) = match end {
        End::Done(text) | End::Waiting(text) => (true, text),
        End::Failed(message) => (false, message),
        End::Stopped => (false, "Stopped.".to_owned()),
    };
    let _ = core
        .events
        .send(EngineEvent::TurnDone {
            ok,
            text,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        })
        .await;
}

/// What the turn's tool calls share (behind one lock: one call at a
/// time).
#[derive(Default)]
struct CallState {
    repeats: RepeatGuard,
    failures: Failures,
    /// The step a refusal happened in: its other calls are skipped.
    refused_step: Option<u32>,
    /// Tool calls that ran this turn.
    ran: usize,
    /// oxbot or the person said no this turn.
    refused: bool,
    /// What the turn read (for the answer check).
    read: Vec<String>,
    /// Stored results it read, by handle.
    handles: BTreeSet<String>,
    bad_calls: u32,
    nudged: bool,
}

/// What the turn's loop hears from a finished call.
enum CallDone {
    Ran,
    /// Calls in a row the model got wrong.
    Invalid(u32),
    Asked(String),
    Stopped,
}

struct Turn {
    number: u32,
    thread: String,
    codex_id: String,
    max_steps: u32,
    budget: TokenBudget,
    calls: Arc<tokio::sync::Mutex<CallState>>,
    /// Bumped when Codex compacts: earlier results are out of view.
    epoch: Arc<AtomicU32>,
    /// Agent text since the last tool call.
    pending: Vec<String>,
    /// Calls the chat was told about.
    announced: HashSet<String>,
    /// Calls being run now (the clock stops while any runs).
    in_calls: usize,
    /// Why oxbot stopped the turn, if it did.
    stopped_for: Option<End>,
    /// The person stopped it: tool calls stop too.
    cancel: CancellationToken,
    /// When oxbot asked Codex to stop the turn.
    stopping_since: Option<Instant>,
    bounced: bool,
    ungrounded_before: Option<Vec<String>>,
    /// The answer given before the check, kept if the checked one is only a report.
    first_answer: Option<String>,
    bounced_at: Option<Instant>,
    writing: bool,
}

impl Turn {
    async fn run(core: &Arc<Core>, input: &TurnInput, cancel: &CancellationToken) -> End {
        let supervisor = &core.supervisor;
        let info = ClientInfo {
            bot: core.spec.bot.clone(),
            context_dir: core.spec.context_dir.clone(),
            privacy: core.spec.privacy,
            choice: core.spec.model.clone(),
            central: core.spec.host_tools.central,
        };
        let running = match supervisor.get(&core.key, info).await {
            Ok(r) => r,
            Err(why) => return End::Failed(why),
        };
        let (thread, model, budget) = match load_thread(core, &running).await {
            Ok(t) => t,
            Err(why) => return End::Failed(why),
        };
        let number = match next_turn(core, &thread) {
            Ok(n) => n,
            Err(why) => return End::Failed(why),
        };
        let max_steps = core
            .shared
            .loop_engine
            .prefs()
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .max_steps;
        let (meter_tx, mut meter_rx) = mpsc::unbounded_channel::<CallRecord>();
        running.relay.begin_turn(TurnStart {
            session: core.spec.resume.id().to_owned(),
            turn: number,
            person: input.person.clone(),
            lane: if core.spec.gate_mode == GateMode::DryRun {
                Lane::Trial
            } else {
                Lane::Live
            },
            min_window: Some(budget.window),
            max_requests: max_steps,
            meter: Some(meter_tx),
        });
        let (tx, mut rx) = mpsc::unbounded_channel();
        running.process.watch(&thread, tx);
        let _ = core.events.send(EngineEvent::Started { model }).await;
        let mut turn = Self {
            number,
            thread: thread.clone(),
            codex_id: String::new(),
            max_steps,
            budget,
            calls: Arc::new(tokio::sync::Mutex::new(CallState::default())),
            epoch: Arc::new(AtomicU32::new(0)),
            pending: Vec::new(),
            announced: HashSet::new(),
            in_calls: 0,
            stopped_for: None,
            cancel: cancel.clone(),
            stopping_since: None,
            bounced: false,
            ungrounded_before: None,
            first_answer: None,
            bounced_at: None,
            writing: false,
        };
        let first = format!("{}\n\n{}", input.text, date_line(&chrono::Local::now()));
        let end = match turn.start_codex(&running, &first).await {
            Ok(()) => {
                turn.drive(core, &running, input, cancel, &mut rx, &mut meter_rx)
                    .await
            }
            Err(why) => End::Failed(why),
        };
        running.process.unwatch(&thread);
        let summary = running.relay.end_turn();
        // The last requests' meter rows.
        while let Ok(record) = meter_rx.try_recv() {
            let _ = core.events.send(EngineEvent::ModelCall(record)).await;
        }
        supervisor.touch(&core.key).await;
        match end {
            End::Failed(why) => End::Failed(summary.stop.unwrap_or(why)),
            other => other,
        }
    }

    async fn start_codex(&mut self, running: &Running, text: &str) -> Result<(), String> {
        let answer = running
            .process
            .request(
                "turn/start",
                json!({
                    "threadId": self.thread,
                    "input": [{"type": "text", "text": text, "text_elements": []}],
                }),
            )
            .await
            .map_err(|e| format!("The AI helper didn't take the message: {e}"))?;
        answer["turn"]["id"]
            .as_str()
            .unwrap_or_default()
            .clone_into(&mut self.codex_id);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn drive(
        &mut self,
        core: &Arc<Core>,
        running: &Running,
        input: &TurnInput,
        cancel: &CancellationToken,
        rx: &mut mpsc::UnboundedReceiver<Incoming>,
        meter_rx: &mut mpsc::UnboundedReceiver<CallRecord>,
    ) -> End {
        let (done_tx, mut done_rx) = mpsc::unbounded_channel::<CallDone>();
        let mut worked = Duration::ZERO;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let _ = core
            .events
            .send(EngineEvent::Activity(Activity::Requesting))
            .await;
        loop {
            tokio::select! {
                () = cancel.cancelled(), if self.stopped_for.is_none() => {
                    self.stop(running, End::Stopped).await;
                }
                Some(record) = meter_rx.recv() => {
                    let _ = core.events.send(EngineEvent::ModelCall(record)).await;
                }
                Some(done) = done_rx.recv() => {
                    self.in_calls = self.in_calls.saturating_sub(1);
                    match done {
                        CallDone::Ran | CallDone::Stopped => {}
                        CallDone::Invalid(bad_calls) => {
                            if bad_calls >= MAX_BAD_CALLS {
                                self.stop(running, End::Failed(
                                    "The model kept making tool calls oxbot couldn't run, so the \
                                     task stopped.".to_owned(),
                                )).await;
                            }
                        }
                        CallDone::Asked(question) => {
                            self.stop(running, End::Waiting(question)).await;
                        }
                    }
                }
                _ = tick.tick() => {
                    // Codex didn't say the turn stopped: it's over anyway.
                    if self.stopping_since.is_some_and(|at| at.elapsed() >= STOP_WAIT)
                        && let Some(end) = self.stopped_for.take()
                    {
                        return end;
                    }
                    if self.in_calls == 0 && self.stopped_for.is_none() {
                        worked += Duration::from_secs(1);
                        if worked >= core.shared.max_turn {
                            let minutes = core.shared.max_turn.as_secs() / 60;
                            self.stop(running, End::Failed(format!(
                                "This task ran for {minutes} minutes without finishing, so it \
                                 stopped. Send what's left as a new, smaller request."
                            ))).await;
                        }
                    }
                }
                incoming = rx.recv() => {
                    let Some(incoming) = incoming else {
                        return End::Failed("The bot's AI helper stopped unexpectedly. Send the message again.".to_owned());
                    };
                    match incoming {
                        Incoming::Gone => {
                            core.thread.lock().await.loaded = None;
                            return self.stopped_for.take().unwrap_or_else(|| End::Failed(
                                "The bot's AI helper stopped unexpectedly. Send the message again.".to_owned(),
                            ));
                        }
                        Incoming::Request { id, method, params } => {
                            self.request(core, running, input, &done_tx, id, &method, params).await;
                        }
                        Incoming::Note { method, params } => {
                            if params["turnId"].as_str().is_some_and(|t| t != self.codex_id)
                                && method != "turn/completed"
                            {
                                continue;
                            }
                            if let Some(end) = self.note(core, running, input, &method, &params).await {
                                return end;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Stops the Codex turn, to end as `why` once Codex says it stopped.
    async fn stop(&mut self, running: &Running, why: End) {
        if self.stopped_for.is_some() {
            return;
        }
        self.stopped_for = Some(why);
        self.stopping_since = Some(Instant::now());
        if let Err(err) = running
            .process
            .request(
                "turn/interrupt",
                json!({"threadId": self.thread, "turnId": self.codex_id}),
            )
            .await
        {
            tracing::debug!(%err, "Codex didn't take the interrupt");
        }
    }

    /// A notification about the turn. `Some` when the turn is over.
    async fn note(
        &mut self,
        core: &Arc<Core>,
        running: &Running,
        input: &TurnInput,
        method: &str,
        params: &Value,
    ) -> Option<End> {
        match method {
            "item/agentMessage/delta" => {
                if let Some(delta) = params["delta"].as_str() {
                    if !self.writing {
                        self.writing = true;
                        let _ = core
                            .events
                            .send(EngineEvent::Activity(Activity::Writing))
                            .await;
                    }
                    let _ = core
                        .events
                        .send(EngineEvent::TextDelta(delta.to_owned()))
                        .await;
                }
                None
            }
            "item/started" if params["item"]["type"] == "dynamicToolCall" => {
                let item = &params["item"];
                let call = Self::tool_call(core, item["id"].as_str(), item);
                if let Some(call) = call {
                    self.announce(core, &call).await;
                }
                None
            }
            "item/completed" => {
                match params["item"]["type"].as_str() {
                    Some("agentMessage") => {
                        if let Some(text) = params["item"]["text"].as_str()
                            && !text.trim().is_empty()
                        {
                            self.pending.push(text.to_owned());
                        }
                    }
                    Some("contextCompaction") => {
                        self.epoch.fetch_add(1, Ordering::Relaxed);
                    }
                    _ => {}
                }
                None
            }
            "thread/compacted" => {
                self.epoch.fetch_add(1, Ordering::Relaxed);
                None
            }
            "error" if params["willRetry"] == false => {
                tracing::info!(bot = %core.spec.bot, error = %params["error"]["message"], "Codex reported an error");
                None
            }
            "turn/completed" => {
                if params["turn"]["id"].as_str() != Some(self.codex_id.as_str()) {
                    return None;
                }
                self.completed(core, running, input, &params["turn"]).await
            }
            _ => None,
        }
    }

    /// Codex says the turn ended. `Some` when oxbot's turn ends too.
    async fn completed(
        &mut self,
        core: &Arc<Core>,
        running: &Running,
        input: &TurnInput,
        turn: &Value,
    ) -> Option<End> {
        if let Some(end) = self.stopped_for.take() {
            if let End::Waiting(question) = &end {
                let _ = core
                    .events
                    .send(EngineEvent::Assistant {
                        text: question.clone(),
                        tool_uses: Vec::new(),
                    })
                    .await;
            }
            return Some(end);
        }
        match turn["status"].as_str() {
            Some("completed") => {}
            Some("interrupted") => return Some(End::Stopped),
            _ => {
                let raw = turn["error"]["message"]
                    .as_str()
                    .unwrap_or("The AI helper failed.");
                return Some(End::Failed(plain_error(raw)));
            }
        }
        let text = std::mem::take(&mut self.pending).join("\n\n");
        let calls = self.calls.lock().await;
        let check = calls.ran >= CHECK_AFTER_CALLS && !calls.refused;
        let missing = if check {
            ungrounded(&calls, core, &input.text, &text)
        } else {
            Vec::new()
        };
        drop(calls);
        if check && !self.bounced {
            let steps = running.relay.summary().requests;
            if steps < self.max_steps {
                self.bounced = true;
                self.bounced_at = Some(Instant::now());
                let note = check_note(&input.text, &missing, core.tools.has_browser());
                self.ungrounded_before = Some(missing);
                self.first_answer = Some(text);
                let _ = core
                    .events
                    .send(EngineEvent::Activity(Activity::Other(CHECKING.to_owned())))
                    .await;
                self.writing = false;
                if let Err(why) = self.start_codex(running, &note).await {
                    return Some(End::Failed(why));
                }
                return None;
            }
            self.record_check(core, false, missing.clone(), missing);
        } else if self.bounced {
            let before = self.ungrounded_before.take().unwrap_or_default();
            self.record_check(core, true, before, missing);
        }
        let text = match self.first_answer.take() {
            Some(first) => answer_after_check(&first, &text),
            None => text,
        };
        let _ = core
            .events
            .send(EngineEvent::Assistant {
                text: text.clone(),
                tool_uses: Vec::new(),
            })
            .await;
        Some(End::Done(text))
    }

    fn record_check(&self, core: &Core, bounced: bool, before: Vec<String>, after: Vec<String>) {
        let ms = self.bounced_at.map_or(0, |at| {
            i64::try_from(at.elapsed().as_millis()).unwrap_or(i64::MAX)
        });
        let row = CheckRow {
            session: core.spec.resume.id().to_owned(),
            turn: self.number,
            bounced,
            ungrounded_before: before,
            ungrounded_after: after,
            ms,
        };
        if let Err(err) = core.shared.loop_engine.record_check(&row) {
            tracing::warn!(%err, "couldn't save a turn's answer check");
        }
    }

    /// The tool call an item or request names, under oxbot's own name.
    fn tool_call(core: &Core, call_id: Option<&str>, item: &Value) -> Option<ToolCall> {
        let id = call_id?;
        let codex_name = item["tool"].as_str()?;
        let name = core.names.oxbot(codex_name).unwrap_or(codex_name);
        Some(ToolCall::new(id, name, item["arguments"].clone()))
    }

    /// Tells the chat about a call (once), with the text said before it.
    async fn announce(&mut self, core: &Core, call: &ToolCall) {
        if !self.announced.insert(call.id.clone()) {
            return;
        }
        self.writing = false;
        let text = std::mem::take(&mut self.pending).join("\n\n");
        let _ = core
            .events
            .send(EngineEvent::Assistant {
                text,
                tool_uses: vec![ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: call.arguments.clone(),
                }],
            })
            .await;
    }

    /// A request from Codex.
    #[allow(clippy::too_many_arguments)]
    async fn request(
        &mut self,
        core: &Arc<Core>,
        running: &Running,
        input: &TurnInput,
        done: &mpsc::UnboundedSender<CallDone>,
        id: Value,
        method: &str,
        params: Value,
    ) {
        let process = &running.process;
        match method {
            "item/tool/call" => {
                let Some(call) = Self::tool_call(core, params["callId"].as_str(), &params) else {
                    process
                        .respond_error(&id, "a tool call without a tool")
                        .await;
                    return;
                };
                self.announce(core, &call).await;
                self.in_calls += 1;
                let step = running.relay.summary().requests;
                let job = CallJob {
                    core: Arc::clone(core),
                    process: Arc::clone(process),
                    relay_steps: Arc::clone(&running.relay),
                    calls: Arc::clone(&self.calls),
                    epoch: self.epoch.load(Ordering::Relaxed),
                    call,
                    id,
                    step,
                    turn: self.number,
                    max_steps: self.max_steps,
                    budget: self.budget,
                    person: input.person.clone(),
                    stopped: self.stopped_for.is_some(),
                    files_asked: core.spec.keeps_files
                        || oxbot_loop::tools::files::asks_for_file(&input.text),
                    cancel: self.cancel.child_token(),
                };
                let done = done.clone();
                tokio::spawn(async move {
                    let result = job.run().await;
                    let _ = done.send(result);
                });
            }
            "currentTime/read" => {
                let secs = oxbot_loop::meter::now_ms() / 1000;
                process.respond(&id, json!({"currentTimeAt": secs})).await;
            }
            _ => decline(core, process, &id, method).await,
        }
    }
}

/// The date line at the end of each turn's message (after the person's
/// words, like oxbot's other notes), in the Mac's own time
/// zone, so a task's "Nov 14" means this year. Codex's own environment block
/// stays off: it would also send the bot's folder path.
fn date_line<Tz: chrono::TimeZone>(now: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    format!("[oxbot] Today is {}.", now.format("%A, %B %-d, %Y"))
}

/// Declines what Codex asks to approve: oxbot never lets it run commands,
/// change files or widen its permissions, and never answers for the
/// person.
async fn decline(core: &Core, process: &crate::rpc::Process, id: &Value, method: &str) {
    let answer = match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            Some(json!({"decision": "decline"}))
        }
        "item/permissions/requestApproval" => Some(json!({"permissions": {}, "scope": "turn"})),
        "mcpServer/elicitation/request" => {
            Some(json!({"action": "decline", "content": null, "_meta": null}))
        }
        "item/tool/requestUserInput" => Some(json!({"answers": {}})),
        "applyPatchApproval" | "execCommandApproval" => {
            Some(json!({"decision": {"denied": {"rejection": "oxbot doesn't allow this"}}}))
        }
        _ => None,
    };
    tracing::warn!(bot = %core.spec.bot, method, "Codex asked for something oxbot never allows; declined");
    match answer {
        Some(answer) => process.respond(id, answer).await,
        None => process.respond_error(id, "oxbot doesn't do that").await,
    }
}

/// Codex's error text, in plain words: the message inside a JSON error.
fn plain_error(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| raw.to_owned())
}

/// The values in `text` that nothing the turn read had.
fn ungrounded(calls: &CallState, core: &Core, goal: &str, text: &str) -> Vec<String> {
    let mut said = Evidence::new();
    said.add(goal);
    let claims: Vec<String> = numbers::claims(text)
        .into_iter()
        .filter(|c| !said.has(c))
        .collect();
    if claims.is_empty() {
        return Vec::new();
    }
    let mut read = Evidence::new();
    for t in &calls.read {
        read.add(t);
    }
    for handle in &calls.handles {
        if let Ok((_, text)) = core.tools.results().get(handle) {
            read.add(&text);
        }
    }
    read.missing(&claims, MAX_NAMED)
}

/// One tool call from Codex, run through the session's tools.
struct CallJob {
    core: Arc<Core>,
    process: Arc<crate::rpc::Process>,
    relay_steps: Arc<oxbot_relay::RelayClient>,
    calls: Arc<tokio::sync::Mutex<CallState>>,
    /// Codex's compactions so far.
    epoch: u32,
    call: ToolCall,
    id: Value,
    step: u32,
    turn: u32,
    max_steps: u32,
    budget: TokenBudget,
    person: Option<String>,
    stopped: bool,
    /// The person asked for a file (or the bot keeps files): Write and Edit may run.
    files_asked: bool,
    /// Fires when the person stops the turn.
    cancel: CancellationToken,
}

impl CallJob {
    async fn run(self) -> CallDone {
        let cancel = self.cancel.clone();
        // A call Codex gave up on (the turn was stopped) stops too.
        let gone = self.process.clone();
        let watch = cancel.clone();
        let watcher = tokio::spawn(async move {
            gone.gone().await;
            watch.cancel();
        });
        let mut calls = self.calls.lock().await;
        let (mut outcome, done) = if self.stopped {
            (ToolOutcome::note("Stopped.", true), CallDone::Stopped)
        } else if calls.refused_step == Some(self.step) {
            (ToolOutcome::note(SKIPPED, false), CallDone::Ran)
        } else if !self.files_asked && oxbot_loop::tools::files::changes_files(&self.call.name) {
            (
                ToolOutcome::note(oxbot_loop::tools::files::NO_FILE_ASKED, false),
                CallDone::Ran,
            )
        } else {
            self.dispatch(&mut calls, &cancel).await
        };
        watcher.abort();
        if outcome.error && outcome.meta.kind != EntryKind::Refusal {
            calls.failures.count(&self.call, &mut outcome);
        }
        let steps = self.relay_steps.summary().requests;
        if let Some(note) = wrap_up_note(steps, self.max_steps, &mut calls.nudged)
            && let Some(Part::Text(t)) = outcome.for_model.last_mut()
        {
            t.push_str("\n\n");
            t.push_str(&note);
        }
        drop(calls);
        let _ = self
            .core
            .events
            .send(EngineEvent::ToolResult {
                tool_use_id: self.call.id.clone(),
                text: outcome.text_for_hub.clone(),
            })
            .await;
        let text: Vec<Value> = outcome
            .for_model
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(json!({"type": "inputText", "text": t})),
                Part::Image { .. } => None,
            })
            .collect();
        self.process
            .respond(
                &self.id,
                json!({"contentItems": text, "success": !outcome.error}),
            )
            .await;
        done
    }

    async fn dispatch(
        &self,
        calls: &mut CallState,
        cancel: &CancellationToken,
    ) -> (ToolOutcome, CallDone) {
        let ctx = CallCtx {
            turn: self.turn,
            step: self.step,
            budget: self.budget,
            person: self.person.as_deref(),
            lane: if self.core.spec.gate_mode == GateMode::DryRun {
                Lane::Trial
            } else {
                Lane::Live
            },
            cancel,
        };
        let mut hooks = CodexHooks {
            core: &self.core,
            calls: &mut *calls,
            epoch: self.epoch,
        };
        let dispatched = self.core.tools.dispatch(&self.call, &ctx, &mut hooks).await;
        match dispatched {
            Ok(Dispatched::Ran(outcome)) => {
                calls.bad_calls = 0;
                calls.ran += 1;
                if !outcome.error {
                    if self.call.name != ASK_PAGE {
                        calls.read.push(outcome.text_for_hub.clone());
                    }
                    if let Some(h) = outcome.handle.clone() {
                        calls.handles.insert(h);
                    }
                }
                (outcome, CallDone::Ran)
            }
            Ok(Dispatched::Invalid(outcome)) => {
                calls.bad_calls += 1;
                (outcome, CallDone::Invalid(calls.bad_calls))
            }
            Ok(Dispatched::Refused(outcome)) => {
                calls.bad_calls = 0;
                calls.refused = true;
                calls.refused_step = Some(self.step);
                (outcome, CallDone::Ran)
            }
            Ok(Dispatched::Asked(question)) => {
                calls.bad_calls = 0;
                (ToolOutcome::note(ASKED, false), CallDone::Asked(question))
            }
            Err(Stop::Cancelled) => (ToolOutcome::note("Stopped.", true), CallDone::Stopped),
            Err(Stop::Fail(why)) => (
                ToolOutcome::note(format!("[tool error] {why}"), true),
                CallDone::Ran,
            ),
        }
    }
}

/// What the Codex engine keeps for the session's tools: the to-do list,
/// and which results Codex still has (all of them, until it compacts).
struct CodexHooks<'a> {
    core: &'a Core,
    calls: &'a mut CallState,
    epoch: u32,
}

impl TurnHooks for CodexHooks<'_> {
    fn own_tool(&mut self, call: &ToolCall, _turn: u32) -> ToolOutcome {
        let mut outcome = if call.name == UPDATE_TODO {
            let mut todo = lock(&self.core.todo);
            match update_todo(&mut todo, &call.arguments) {
                Ok(text) => ToolOutcome::note(text, false).kind(EntryKind::Todo),
                Err(why) => ToolOutcome::note(format!("[tool error] {why}"), true),
            }
        } else {
            ToolOutcome::note(format!("[tool error] unknown tool {}", call.name), true)
        };
        outcome.meta.tool = Some(call.name.clone());
        outcome
    }

    fn before_side_effect(&mut self, _call: &ToolCall) -> Result<(), Stop> {
        // A Codex turn isn't picked up after a restart: nothing to write
        // down first.
        Ok(())
    }

    fn visible_handles(&self) -> BTreeSet<String> {
        // Codex keeps every result until it compacts.
        self.calls.handles.clone()
    }

    fn next_mark(&self) -> u32 {
        self.epoch
    }

    fn in_view(&self, mark: u32) -> bool {
        mark == self.epoch
    }

    fn repeats(&mut self) -> &mut RepeatGuard {
        &mut self.calls.repeats
    }
}

/// The next turn's number in the session (kept with its thread).
fn next_turn(core: &Core, thread: &str) -> Result<u32, String> {
    let store = &core.shared.store;
    let bot = &core.spec.bot;
    let session = core.spec.resume.id();
    let mut row = store
        .thread(bot, session)?
        .filter(|r| r.thread_id == thread)
        .ok_or_else(|| "the session's AI helper conversation wasn't saved".to_owned())?;
    let number = row.turns;
    row.turns += 1;
    row.updated_ms = oxbot_loop::meter::now_ms();
    store.save_thread(&row)?;
    Ok(number)
}

/// The session's thread, loaded in `running`: resumed if it was saved,
/// else started. Returns it, its model and the window's budget.
async fn load_thread(
    core: &Core,
    running: &Running,
) -> Result<(String, String, TokenBudget), String> {
    let mut ts = core.thread.lock().await;
    if let (Some(id), Some(budget)) = (&ts.id, ts.budget)
        && ts.loaded == Some(running.generation)
    {
        return Ok((id.clone(), ts.model.clone(), budget));
    }
    let gateway = core.shared.loop_engine.gateway();
    let Some((model, window, max_tokens)) =
        gateway.window_for(&core.spec.model, core.spec.host_tools.central)
    else {
        return Err(
            "No model can take this request right now. Try again in a few minutes.".to_owned(),
        );
    };
    let budget = TokenBudget::for_model(window, Some(max_tokens));
    let config = json!({
        "model_context_window": window,
        "model_auto_compact_token_limit": budget.compact_at(),
        "tool_output_token_limit": budget.snapshot_cap() + budget.inline_cap(),
    });
    let store = &core.shared.store;
    let bot = &core.spec.bot;
    let session = core.spec.resume.id();
    let saved = store.thread(bot, session)?;
    let now = oxbot_loop::meter::now_ms();
    let mut row = None;
    if let Some(saved) = saved {
        let resumed = running
            .process
            .request(
                "thread/resume",
                json!({
                    "threadId": saved.thread_id,
                    "cwd": core.spec.work_dir,
                    "approvalPolicy": "never",
                    "sandbox": "read-only",
                    "config": config,
                    "excludeTurns": true,
                }),
            )
            .await;
        match resumed {
            Ok(_) => {
                row = Some(ThreadRow {
                    model: model.clone(),
                    window,
                    updated_ms: now,
                    ..saved
                });
            }
            Err(err) => {
                tracing::info!(%bot, %err, "couldn't go on with the saved Codex thread; starting a new one");
            }
        }
    }
    let row = if let Some(row) = row {
        row
    } else {
        let id = start_thread(core, running, &model, &config).await?;
        ThreadRow {
            bot: bot.clone(),
            session: session.to_owned(),
            thread_id: id,
            model: model.clone(),
            window,
            turns: store.thread(bot, session)?.map_or(0, |r| r.turns),
            created_ms: now,
            updated_ms: now,
        }
    };
    store.save_thread(&row)?;
    ts.id = Some(row.thread_id.clone());
    ts.loaded = Some(running.generation);
    ts.model.clone_from(&model);
    ts.window = window;
    ts.budget = Some(budget);
    Ok((row.thread_id, model, budget))
}

/// Starts a thread with oxbot's instructions and tools. A tool whose
/// schema Codex can't take is left out (and logged), not the session.
async fn start_thread(
    core: &Core,
    running: &Running,
    model: &str,
    config: &Value,
) -> Result<String, String> {
    let mut dropped: Vec<String> = Vec::new();
    loop {
        let tools = core.names.dynamic_tools(&core.tools.specs(), &dropped);
        let answer = running
            .process
            .request(
                "thread/start",
                json!({
                    "model": model,
                    "cwd": core.spec.work_dir,
                    "approvalPolicy": "never",
                    "sandbox": "read-only",
                    "baseInstructions": core.spec.system_prompt,
                    "dynamicTools": tools,
                    "config": config,
                    "serviceName": "oxbot",
                    "ephemeral": core.spec.gate_mode == GateMode::DryRun,
                }),
            )
            .await;
        match answer {
            Ok(answer) => {
                return answer["thread"]["id"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        "The AI helper started a conversation without an id".to_owned()
                    });
            }
            Err(RpcError::Codex(why)) if dropped.len() < MAX_DROPPED_TOOLS => {
                let Some(tool) = why
                    .strip_prefix("dynamic tool input schema is not supported for ")
                    .and_then(|rest| rest.split(':').next())
                else {
                    return Err(format!(
                        "The AI helper couldn't start the conversation: {why}"
                    ));
                };
                tracing::warn!(bot = %core.spec.bot, tool, why, "Codex can't take this tool; leaving it out");
                dropped.push(tool.to_owned());
            }
            Err(err) => {
                return Err(format!(
                    "The AI helper couldn't start the conversation: {err}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_turn_starts_with_the_local_date() {
        use chrono::TimeZone;
        let toronto = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        // 9 PM in Toronto is already the next day in UTC; the bot gets the local day.
        let now = toronto.with_ymd_and_hms(2026, 11, 4, 21, 0, 0).unwrap();
        assert_eq!(
            date_line(&now),
            "[oxbot] Today is Wednesday, November 4, 2026."
        );
    }

    #[test]
    fn codex_s_errors_read_in_plain_words() {
        assert_eq!(
            plain_error(
                r#"{"error": {"message": "Scout has used its 200 requests for today.", "code": "oxbot"}}"#
            ),
            "Scout has used its 200 requests for today."
        );
        assert_eq!(plain_error("stream disconnected"), "stream disconnected");
    }

    #[test]
    fn what_a_turn_read_grounds_its_answer() {
        let calls = CallState {
            read: vec!["Toronto: 2,794,356 people (2021)".to_owned()],
            ..CallState::default()
        };
        let mut said = Evidence::new();
        said.add("[Sam] Toronto's population in 2021");
        let claims: Vec<String> = numbers::claims("It had 2,794,356 people in 2021, up 42%.")
            .into_iter()
            .filter(|c| !said.has(c))
            .collect();
        let mut read = Evidence::new();
        for t in &calls.read {
            read.add(t);
        }
        assert_eq!(read.missing(&claims, MAX_NAMED), ["42"]);
    }
}
