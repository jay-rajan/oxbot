//! The step loop: one turn, one step at a time.
//!
//! Each step: pick a model, build the context (making room if needed),
//! wait for the pacer, stream the answer, save it. Then each tool call it
//! asked for, in order: check it, ask the gate, run it (a call that changes
//! something is written down first), save its result. A turn ends when the
//! model answers without a tool call, asks the person something, fails, or
//! is stopped. A restart picks a turn up after its last saved step.

use std::sync::Arc;
use std::time::Duration;

use oxbot_engine::{
    Activity, CallKind, CallRecord, EngineEvent, GateMode, ModelChoice, Privacy, ToolUse, TurnInput,
};
use oxbot_llm::{
    ChatDelta, ChatModel, ChatRequest, ChatResponse, ErrorKind, KEYCHAIN_WAITING, LlmError,
    Message, Provider, ProviderPrefs, RetryClass, Role, ToolCall, ToolSpec, complete,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::budget::est_message;
use crate::compact::ModelCaller;
use crate::context::{LastCall, Pressure, TaskView, TokenBudget};
use crate::gate::Gated;
use crate::meter::{GuardQuery, now_ms, outcome_of, utc_day};
use crate::pacer::{Lane, PacerStop};
use crate::router::{Pick, Route, TurnRouting};
use crate::session::{Conv, Core};
use crate::store::{PendingCall, StoredMessage, TurnRow, TurnState, TurnStatus};
use crate::tools::control::ASK_USER;
use crate::tools::{ToolEnv, ToolKind, ToolOutcome};
use crate::{DATA_POLICY_HELP, KnowledgeBlock, KnowledgeQuery, OneShot, Shared};

/// Waits before retrying the same model after a network or server error.
const RETRY_WAITS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(15),
];
/// Longest wait for a model to cool down within one step.
const MAX_COOL_WAIT: Duration = Duration::from_secs(90);
/// Calls in a row the model got wrong before the turn stops.
const MAX_BAD_CALLS: u32 = 3;
/// Tokens kept free beyond the answer.
const SAFETY_TOKENS: u64 = 512;
/// What a turn's question is saved as, at most.
const MAX_QUESTION: usize = 2_000;

const ZDR_DROPPED: &str = "No zero-retention provider was free; used one that doesn't keep data";
pub(crate) const RESTARTED_MID_CALL: &str = "oxbot restarted while this was running, so it may or may not \
    have happened. Look at the page before trying again.";
pub(crate) const SKIPPED: &str =
    "[skipped] An earlier call in this step was refused, so this one didn't run.";

/// Why a turn ends early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Stopped by the person (or the session is stopping).
    Cancelled,
    /// Failed: tell the person this.
    Fail(String),
}

/// How a turn ends.
enum End {
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

/// What to do after a failed request.
enum Next {
    Again,
    Compact,
    Stop(Stop),
}

/// Makes model requests for one turn (or one-off request): picks the
/// model, checks limits, paces, streams, meters, and sorts out errors.
pub(crate) struct Caller<'a> {
    shared: &'a Shared,
    model: Arc<dyn ChatModel>,
    bot: &'a str,
    person: Option<&'a str>,
    session: Option<&'a str>,
    turn: Option<u32>,
    step: u32,
    privacy: Privacy,
    lane: Lane,
    kind: CallKind,
    events: Option<&'a mpsc::Sender<EngineEvent>>,
    cancel: &'a CancellationToken,
    routing: TurnRouting,
    zdr_dropped: bool,
    same_retries: usize,
    cooled_for: Duration,
    /// What the service said about the last request that failed.
    last_error: Option<String>,
    /// A model was ruled out by the account's data policy (on a request
    /// that didn't ask for it).
    data_policy: bool,
}

impl Caller<'_> {
    fn activity(&self, activity: Activity) {
        if let Some(events) = self.events {
            let _ = events.try_send(EngineEvent::Activity(activity));
        }
    }

    fn paid_note(&self) -> &'static str {
        let prefs = self
            .shared
            .setup
            .prefs
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if prefs.paid_fallback {
            ""
        } else {
            ", or switch on paid models in Settings → Engines"
        }
    }

    fn private(&self) -> bool {
        self.privacy == Privacy::Private
            || self
                .shared
                .setup
                .prefs
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .deny_data_collection_all
    }

    /// OpenRouter's provider rules for this bot.
    fn provider_prefs(&self) -> Option<ProviderPrefs> {
        if self.model.provider() != Provider::OpenRouter {
            return None;
        }
        Some(if self.private() {
            let prefs = ProviderPrefs::private_bot();
            if self.zdr_dropped {
                prefs.without_zdr()
            } else {
                prefs
            }
        } else {
            ProviderPrefs::shared_bot()
        })
    }

    /// The model for the next request, after the host's limits allow it.
    async fn route(&mut self) -> Result<Route, Stop> {
        loop {
            if self.cancel.is_cancelled() {
                return Err(Stop::Cancelled);
            }
            let pacer = &self.shared.setup.pacer;
            let free_ok = !pacer.free_used_up();
            match self.shared.router.pick(&self.routing, free_ok) {
                Pick::Route(route) => {
                    let query = GuardQuery {
                        bot: self.bot,
                        person: self.person,
                        free: route.primary.free,
                    };
                    if let Err(hit) = self.shared.setup.guard.check(&query) {
                        return Err(Stop::Fail(hit.0));
                    }
                    return Ok(route);
                }
                Pick::AllCooling(until) => {
                    let wait = until.saturating_duration_since(Instant::now());
                    if self.cooled_for + wait > MAX_COOL_WAIT {
                        return Err(Stop::Fail(format!(
                            "The free models are busy right now. Try again in a few minutes{}.",
                            self.paid_note()
                        )));
                    }
                    self.activity(Activity::Other(
                        "All free models are busy; waiting".to_owned(),
                    ));
                    self.cooled_for += wait;
                    tokio::select! {
                        () = tokio::time::sleep_until(until) => {}
                        () = self.cancel.cancelled() => return Err(Stop::Cancelled),
                    }
                }
                Pick::None => return Err(self.nothing_left(free_ok)),
            }
        }
    }

    /// What the service last said, for the end of a failure message.
    fn said(&self) -> String {
        self.last_error
            .as_ref()
            .map(|m| format!(" (OpenRouter said: \u{201c}{m}\u{201d})"))
            .unwrap_or_default()
    }

    /// Why no model is left, or a second try without zero retention.
    fn nothing_left(&mut self, free_ok: bool) -> Stop {
        let private = self.private();
        if !self.routing.no_provider.is_empty() && private {
            if !self.zdr_dropped {
                // Never drops "deny": only the zero-retention wish.
                self.zdr_dropped = true;
                self.routing.no_provider.clear();
                self.activity(Activity::Other(ZDR_DROPPED.to_owned()));
                return Stop::Fail(String::new());
            }
            return Stop::Fail(format!(
                "No free model takes private work without keeping your data right now. You can \
                 try later{} or (soon) use a model on this Mac.{}",
                if self.paid_note().is_empty() {
                    ""
                } else {
                    ", switch on paid models"
                },
                self.said()
            ));
        }
        if self.data_policy {
            return Stop::Fail(format!("{DATA_POLICY_HELP}{}", self.said()));
        }
        if !free_ok {
            return Stop::Fail(
                "OpenRouter's free daily requests are used up; they reset at midnight UTC."
                    .to_owned(),
            );
        }
        if self.shared.router.catalog().models.is_empty()
            && matches!(self.routing.choice, ModelChoice::Auto)
        {
            return Stop::Fail(
                "oxbot hasn't read OpenRouter's model list yet. Check the connection and try \
                 again in a minute."
                    .to_owned(),
            );
        }
        Stop::Fail(format!(
            "No model can take this request right now. Try again in a few minutes{}.{}",
            self.paid_note(),
            self.said()
        ))
    }

    /// Picks a route, retrying once without zero retention when that's
    /// what stood in the way.
    async fn next_route(&mut self) -> Result<Route, Stop> {
        loop {
            match self.route().await {
                Err(Stop::Fail(m)) if m.is_empty() => {}
                other => return other,
            }
        }
    }

    /// Sends one request on `route`, metering it either way.
    async fn send(
        &mut self,
        route: &Route,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    ) -> Result<ChatResponse, LlmError> {
        if route.primary.free {
            match self
                .shared
                .setup
                .pacer
                .acquire(self.bot, self.lane, self.cancel)
                .await
            {
                Ok(()) => {}
                Err(PacerStop::Cancelled) => return Err(LlmError::cancelled()),
                Err(PacerStop::DailyUsedUp) => {
                    return Err(LlmError::new(
                        ErrorKind::FreeDailyLimit,
                        "OpenRouter's free daily requests are used up.",
                    ));
                }
            }
        }
        self.activity(Activity::Requesting);
        let provider = self.provider_prefs();
        let private = provider
            .as_ref()
            .is_some_and(ProviderPrefs::denies_data_collection);
        let req = ChatRequest {
            fallbacks: route.fallbacks.clone(),
            tools,
            max_tokens: Some(route.max_tokens),
            provider,
            supported: Some(route.supported.clone()),
            ..ChatRequest::new(route.primary.id.clone(), messages)
        };
        let ts_ms = now_ms();
        let started = Instant::now();
        let events = self.events;
        let mut writing = false;
        let result = complete(self.model.as_ref(), req, self.cancel.clone(), |delta| {
            if let (ChatDelta::Text(t), Some(events)) = (delta, events) {
                if !writing {
                    writing = true;
                    let _ = events.try_send(EngineEvent::Activity(Activity::Writing));
                }
                let _ = events.try_send(EngineEvent::TextDelta(t.clone()));
            }
        })
        .await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut record = CallRecord {
            ts_ms,
            day: utc_day(ts_ms),
            bot: self.bot.to_owned(),
            person: self.person.map(str::to_owned),
            session: self.session.map(str::to_owned),
            turn: self.turn,
            step: Some(self.step),
            provider: self.model.provider().as_str().to_owned(),
            requested: route.primary.id.clone(),
            free: route.primary.free,
            private,
            latency_ms,
            kind: self.kind,
            ..CallRecord::default()
        };
        match &result {
            Ok(resp) => {
                record.model.clone_from(&resp.model);
                record.generation_id.clone_from(&resp.generation_id);
                record.prompt_tokens = resp.usage.prompt_tokens;
                record.completion_tokens = resp.usage.completion_tokens;
                record.cached_tokens = resp.usage.cached_tokens;
                record.cost_usd = self.cost(route, resp);
                "ok".clone_into(&mut record.outcome);
            }
            Err(err) => {
                record.outcome = outcome_of(err);
                if err.kind != ErrorKind::Cancelled {
                    // The service's own short words: never the key or headers.
                    tracing::info!(
                        model = %route.primary.id,
                        status = err.status.unwrap_or(0),
                        why = %err.message,
                        "a model request failed"
                    );
                }
            }
        }
        if let Err(err) = self.shared.setup.store.record_call(&record) {
            tracing::warn!(%err, "couldn't save a model request");
        }
        if let Some(events) = self.events {
            let _ = events.send(EngineEvent::ModelCall(record)).await;
        }
        result
    }

    /// What a request cost: as the service says, else from the list's
    /// prices.
    #[allow(clippy::cast_precision_loss)]
    fn cost(&self, route: &Route, resp: &ChatResponse) -> f64 {
        if let Some(cost) = resp.usage.cost_usd {
            return cost;
        }
        let catalog = self.shared.router.catalog();
        let info = catalog
            .get(&resp.model)
            .or(route.primary.info.as_ref())
            .filter(|m| !m.free);
        info.map_or(0.0, |m| {
            m.prompt_usd_per_token * resp.usage.prompt_tokens as f64
                + m.completion_usd_per_token * resp.usage.completion_tokens as f64
        })
    }

    /// A request on `route` answered.
    fn answered(&mut self, route: &Route, resp: &ChatResponse) {
        self.shared.router.ok(&route.primary.id);
        if route.primary.free && !self.private() {
            // Free models answer requests without "deny" again.
            self.shared.data_policy.clear();
        }
        self.same_retries = 0;
        self.cooled_for = Duration::ZERO;
        let answered = if resp.model.is_empty() {
            route.primary.id.clone()
        } else {
            resp.model.clone()
        };
        let known = std::iter::once(&route.primary.id)
            .chain(&route.fallbacks)
            .any(|m| *m == answered);
        self.routing.sticky = Some(if known {
            answered
        } else {
            route.primary.id.clone()
        });
    }

    /// What to do after `err` on `route`.
    async fn after_error(&mut self, route: &Route, err: &LlmError) -> Next {
        let model = &route.primary.id;
        if self.routing.sticky.as_ref() == Some(model) {
            self.routing.sticky = None;
        }
        if err.kind != ErrorKind::Cancelled {
            self.last_error = Some(err.message.clone());
        }
        match err.retry_class() {
            RetryClass::RetrySame => {
                if let Some(wait) = RETRY_WAITS.get(self.same_retries) {
                    self.same_retries += 1;
                    self.routing.sticky = Some(model.clone());
                    self.activity(Activity::Retrying);
                    tokio::select! {
                        () = tokio::time::sleep(*wait) => {}
                        () = self.cancel.cancelled() => return Next::Stop(Stop::Cancelled),
                    }
                } else {
                    self.same_retries = 0;
                    self.shared.router.cool(model, None);
                }
                Next::Again
            }
            RetryClass::NextModel => {
                self.same_retries = 0;
                match err.kind {
                    ErrorKind::RateLimited | ErrorKind::Overloaded => {
                        self.shared.router.cool(model, err.retry_after);
                        if err.kind == ErrorKind::RateLimited {
                            self.shared.setup.pacer.refresh_soon();
                        }
                    }
                    ErrorKind::NoProvider | ErrorKind::DataPolicy => {
                        if err.kind == ErrorKind::DataPolicy && !self.private() {
                            // The account's own privacy settings, not oxbot's.
                            self.data_policy = true;
                            self.shared.data_policy.set();
                        }
                        self.routing.no_provider.insert(model.clone());
                        self.routing
                            .no_provider
                            .extend(route.fallbacks.iter().cloned());
                    }
                    _ => {
                        self.routing.excluded.insert(model.clone());
                    }
                }
                Next::Again
            }
            RetryClass::Compact => Next::Compact,
            RetryClass::StopFreeToday => {
                self.shared.setup.pacer.stop_free_today();
                Next::Again
            }
            RetryClass::SignInAgain => {
                if let Some(key) = &self.shared.setup.key {
                    if key.is_loading() {
                        // Not refused: the kept key isn't read yet.
                        return Next::Stop(Stop::Fail(KEYCHAIN_WAITING.to_owned()));
                    }
                    key.clear();
                }
                Next::Stop(Stop::Fail(
                    "OpenRouter refused oxbot's key. The owner needs to sign in with OpenRouter \
                     again (Settings → Engines)."
                        .to_owned(),
                ))
            }
            RetryClass::OutOfCredits => Next::Stop(Stop::Fail(
                "OpenRouter says this key is out of credits or over its limit.".to_owned(),
            )),
            RetryClass::Refused => Next::Stop(Stop::Fail(format!(
                "The model's provider refused this request ({}).",
                err.message
            ))),
            RetryClass::Cancelled => Next::Stop(Stop::Cancelled),
            RetryClass::Fail => Next::Stop(Stop::Fail(err.message.clone())),
        }
    }
}

/// A [`ModelCaller`] for helpers inside a turn (compaction): its own
/// routing, the turn's bot, person, privacy and stop.
struct HelperCaller<'a> {
    shared: &'a Shared,
    bot: &'a str,
    person: Option<&'a str>,
    session: Option<&'a str>,
    turn: Option<u32>,
    privacy: Privacy,
    lane: Lane,
    choice: ModelChoice,
    cancel: &'a CancellationToken,
}

impl ModelCaller for HelperCaller<'_> {
    fn call(
        &self,
        kind: CallKind,
        messages: Vec<Message>,
        max_tokens: u32,
    ) -> futures_util::future::BoxFuture<'_, Result<String, LlmError>> {
        Box::pin(async move {
            let mut caller = Caller {
                shared: self.shared,
                model: helper_model(self.shared, kind),
                bot: self.bot,
                person: self.person,
                session: self.session,
                turn: self.turn,
                step: 0,
                privacy: self.privacy,
                lane: self.lane,
                kind,
                events: None,
                cancel: self.cancel,
                routing: TurnRouting {
                    choice: self.choice.clone(),
                    ..TurnRouting::default()
                },
                zdr_dropped: false,
                same_retries: 0,
                cooled_for: Duration::ZERO,
                last_error: None,
                data_policy: false,
            };
            plain_call(&mut caller, messages, max_tokens).await
        })
    }
}

/// The model for `kind`: the helper for compaction and reflection when
/// there is one.
fn helper_model(shared: &Shared, kind: CallKind) -> Arc<dyn ChatModel> {
    match (&shared.setup.helper, kind) {
        (Some(helper), CallKind::Compaction | CallKind::Reflection) => Arc::clone(helper),
        _ => Arc::clone(&shared.setup.model),
    }
}

/// One request without tools, with retries; the answer's text.
async fn plain_call(
    caller: &mut Caller<'_>,
    messages: Vec<Message>,
    max_tokens: u32,
) -> Result<String, LlmError> {
    loop {
        let mut route = match caller.next_route().await {
            Ok(r) => r,
            Err(Stop::Cancelled) => return Err(LlmError::cancelled()),
            Err(Stop::Fail(m)) => return Err(LlmError::new(ErrorKind::Config, m)),
        };
        route.max_tokens = route.max_tokens.min(max_tokens.max(1));
        match caller.send(&route, messages.clone(), Vec::new()).await {
            Ok(resp) => {
                caller.answered(&route, &resp);
                return Ok(resp.message.text());
            }
            Err(err) => match caller.after_error(&route, &err).await {
                Next::Again => {}
                Next::Compact => return Err(err),
                Next::Stop(Stop::Cancelled) => return Err(LlmError::cancelled()),
                Next::Stop(Stop::Fail(m)) => {
                    return Err(LlmError::new(err.kind, m));
                }
            },
        }
    }
}

/// [`crate::LoopEngine::one_shot`].
pub(crate) async fn one_shot(shared: &Shared, req: OneShot) -> Result<String, String> {
    let cancel = CancellationToken::new();
    let mut caller = Caller {
        shared,
        model: helper_model(shared, req.kind),
        bot: &req.bot,
        person: req.person.as_deref(),
        session: None,
        turn: None,
        step: 0,
        privacy: req.privacy,
        lane: Lane::Live,
        kind: req.kind,
        events: None,
        cancel: &cancel,
        routing: TurnRouting {
            choice: req.model.clone(),
            ..TurnRouting::default()
        },
        zdr_dropped: false,
        same_retries: 0,
        cooled_for: Duration::ZERO,
        last_error: None,
        data_policy: false,
    };
    plain_call(&mut caller, req.messages, req.max_tokens)
        .await
        .map_err(|e| e.message)
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
struct Turn {
    number: u32,
    step: u32,
    model: Option<String>,
    /// Tool calls left over from before a restart: (call, its write-ahead).
    leftover: Vec<(ToolCall, Option<PendingCall>)>,
    knowledge: Option<KnowledgeBlock>,
    started_model: bool,
}

impl Turn {
    /// Starts a new turn, or picks up the one that was running.
    fn begin(core: &Core, conv: &mut Conv, input: &TurnInput) -> Result<Self, Stop> {
        let store = &core.shared.setup.store;
        if let Some(row) = core.take_resume() {
            if row.user_seq == input.user_seq {
                return Ok(Self::resume(core, conv, &row));
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
        let turn = Self {
            number,
            step: 0,
            model: None,
            leftover: Vec::new(),
            knowledge: None,
            started_model: false,
        };
        let user = Message::user(input.text.clone());
        turn.save(core, conv, vec![(user, false, None)], None)?;
        Ok(turn)
    }

    /// Picks up `row` after a restart.
    fn resume(core: &Core, conv: &Conv, row: &TurnRow) -> Self {
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
        Self {
            number: row.turn,
            step: row.step,
            model: row.model.clone(),
            leftover,
            knowledge: None,
            started_model: false,
        }
    }

    /// One checkpoint: these messages (message, untrusted, handle), the
    /// turn's state, and the context if it changed.
    fn save(
        &self,
        core: &Core,
        conv: &mut Conv,
        add: Vec<(Message, bool, Option<String>)>,
        pending: Option<PendingCall>,
    ) -> Result<(), Stop> {
        let mut idx = conv.next_idx();
        let stored: Vec<StoredMessage> = add
            .into_iter()
            .map(|(message, untrusted, handle)| {
                let tokens = u32::try_from(est_message(&message)).unwrap_or(u32::MAX);
                let m = StoredMessage {
                    idx,
                    turn: self.number,
                    message,
                    tokens,
                    untrusted,
                    handle,
                    meta: json!({}),
                };
                idx += 1;
                m
            })
            .collect();
        let context = conv.context_dirty.then(|| conv.context.clone());
        let state = TurnState {
            step: self.step,
            model: self.model.clone(),
            pending,
        };
        core.shared
            .setup
            .store
            .checkpoint(&core.id, self.number, &stored, &state, context.as_ref())
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
        self.knowledge = knowledge(core, person, &input.text, self.number);
        let mut caller = self.caller(core, conv, person, lane, cancel);
        let mut bad_calls = 0;
        if !self.leftover.is_empty() {
            let calls = std::mem::take(&mut self.leftover);
            if let Some(end) = self
                .tool_calls(core, conv, calls, &mut bad_calls, cancel)
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
                return End::Failed(format!(
                    "This task took {max_steps} steps without finishing, so it stopped. Send \
                     what's left as a new, smaller request."
                ));
            }
            caller.step = self.step;
            let resp = match self.call(core, conv, &mut caller, cancel).await {
                Ok(r) => r,
                Err(stop) => {
                    conv.excluded.clone_from(&caller.routing.excluded);
                    return stop.into();
                }
            };
            conv.excluded.clone_from(&caller.routing.excluded);
            self.step += 1;
            self.model.clone_from(&caller.routing.sticky);
            let message = resp.message;
            let text = message.text();
            let calls = message.tool_calls.clone();
            let _ = core
                .events
                .send(EngineEvent::Assistant {
                    text: text.clone(),
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
            if let Err(stop) = self.save(core, conv, vec![(message, false, None)], None) {
                return stop.into();
            }
            if calls.is_empty() {
                return End::Done(text);
            }
            let calls = calls.into_iter().map(|c| (c, None)).collect();
            if let Some(end) = self
                .tool_calls(core, conv, calls, &mut bad_calls, cancel)
                .await
            {
                return end;
            }
        }
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
            events: Some(&core.events),
            cancel,
            routing: TurnRouting {
                choice: core.choice.clone(),
                central: core.central,
                sticky: self.model.clone(),
                excluded: conv.excluded.clone(),
                no_provider: std::collections::HashSet::new(),
            },
            zdr_dropped: false,
            same_retries: 0,
            cooled_for: Duration::ZERO,
            last_error: None,
            data_policy: false,
        }
    }

    /// One model request: pick, build, make room, send, sort out errors.
    async fn call(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        caller: &mut Caller<'_>,
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
            let window = to_estimate(core, &route.primary.id, route.window);
            let budget = TokenBudget {
                window: if squeezed { window / 2 } else { window },
                reserve: u64::from(route.max_tokens) + SAFETY_TOKENS,
            };
            let built = self
                .build(core, conv, budget, caller, cancel, squeezed)
                .await?;
            let est_tokens = built.0;
            match caller.send(&route, built.1, core.specs.clone()).await {
                Ok(resp) => {
                    core.shared.calibration.observe(
                        &resp.model,
                        est_tokens,
                        resp.usage.prompt_tokens,
                    );
                    conv.last_call = Some(LastCall {
                        model: resp.model.clone(),
                        prompt_tokens: resp.usage.prompt_tokens,
                        messages_sent: built.2,
                    });
                    caller.answered(&route, &resp);
                    return Ok(resp);
                }
                Err(err) => match caller.after_error(&route, &err).await {
                    Next::Again => {}
                    Next::Compact if !squeezed => squeezed = true,
                    Next::Compact => {
                        return Err(Stop::Fail(format!(
                            "This conversation is too long for {}. Send a shorter request.",
                            route.primary.id
                        )));
                    }
                    Next::Stop(stop) => return Err(stop),
                },
            }
        }
    }

    /// The messages for a request within `budget`, compacting first when
    /// they don't fit (or when `force`d after the model said too long).
    /// Returns (estimated tokens, messages, how many).
    async fn build(
        &self,
        core: &Core,
        conv: &mut Conv,
        budget: TokenBudget,
        caller: &Caller<'_>,
        cancel: &CancellationToken,
        force: bool,
    ) -> Result<(u64, Vec<Message>, usize), Stop> {
        let setup = &core.shared.setup;
        let mut compacted = false;
        loop {
            let view = TaskView {
                system: &core.system,
                tools: &core.specs,
                knowledge: self.knowledge.as_ref(),
                messages: &conv.messages,
                context: &conv.context,
                person: caller.person,
                now_ms: now_ms(),
                last_call: conv.last_call.as_ref(),
                turn: self.number,
            };
            let built = setup.context.build(&view, budget);
            let fits = built.pressure == Pressure::Ok && (!force || compacted);
            if fits || compacted {
                if built.pressure == Pressure::Compact {
                    return Err(Stop::Fail(
                        "This conversation is too long for the model's window. Send a shorter \
                         request."
                            .to_owned(),
                    ));
                }
                if let Some(context) = built.context {
                    conv.context = context;
                    conv.context_dirty = true;
                }
                let n = built.messages.len();
                return Ok((built.est_tokens, built.messages, n));
            }
            let _ = core.events.try_send(EngineEvent::Activity(Activity::Other(
                "Making room in the conversation".to_owned(),
            )));
            let helper = HelperCaller {
                shared: &core.shared,
                bot: &core.bot,
                person: caller.person,
                session: Some(&core.id),
                turn: Some(self.number),
                privacy: core.privacy,
                lane: caller.lane,
                choice: core.choice.clone(),
                cancel,
            };
            let compaction = setup
                .compactor
                .compact(&view, budget, &helper)
                .await
                .map_err(|e| match e.kind {
                    ErrorKind::Cancelled => Stop::Cancelled,
                    _ => Stop::Fail(format!("oxbot couldn't shorten the conversation: {e}")),
                })?;
            conv.context = compaction.context;
            conv.context_dirty = true;
            self.save(core, conv, Vec::new(), None)?;
            compacted = true;
        }
    }

    /// Runs tool calls in order. `Some` when the turn ends here.
    async fn tool_calls(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        calls: Vec<(ToolCall, Option<PendingCall>)>,
        bad_calls: &mut u32,
        cancel: &CancellationToken,
    ) -> Option<End> {
        let mut refused = false;
        for (n, (call, pending)) in calls.into_iter().enumerate() {
            let mut asked = None;
            let outcome = if refused {
                ToolOutcome::note(SKIPPED, false)
            } else if pending.as_ref().is_some_and(|p| p.side_effect) {
                ToolOutcome::note(RESTARTED_MID_CALL, true)
            } else {
                match self.one_call(core, conv, &call, n, bad_calls, cancel).await {
                    Ok(Ran::Result(outcome)) => outcome,
                    Ok(Ran::Refused(outcome)) => {
                        refused = true;
                        outcome
                    }
                    Ok(Ran::Asked(question)) => {
                        asked = Some(question);
                        ToolOutcome::note("Asked the person; waiting for them.", false)
                    }
                    Err(stop) => return Some(stop.into()),
                }
            };
            let _ = core
                .events
                .send(EngineEvent::ToolResult {
                    tool_use_id: call.id.clone(),
                    text: outcome.text_for_hub.clone(),
                })
                .await;
            let result = Message::tool(call.id.clone(), outcome.for_model);
            if let Err(stop) = self.save(
                core,
                conv,
                vec![(result, outcome.untrusted, outcome.handle)],
                None,
            ) {
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

    /// One tool call: check it, ask the gate, run it.
    async fn one_call(
        &self,
        core: &Core,
        conv: &mut Conv,
        call: &ToolCall,
        n: usize,
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
                return Ok(Ran::Refused(ToolOutcome::note(
                    format!("[oxbot said no] {why}"),
                    true,
                )));
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
        let _ = core
            .events
            .send(EngineEvent::Activity(Activity::UsingTool(
                call.name.clone(),
            )))
            .await;
        let env = ToolEnv {
            work_dir: &core.work_dir,
            events: &core.events,
            browser: core.browser.as_ref(),
            cancel,
            result_name: format!("{}-{}-{n}", self.number, self.step),
            page: &core.page,
        };
        Ok(Ran::Result(core.tools.run(call, permit, &env).await))
    }
}

/// What one tool call came to.
enum Ran {
    /// Its result.
    Result(ToolOutcome),
    /// Refused: its result, and the rest of the step is skipped.
    Refused(ToolOutcome),
    /// It asked the person this.
    Asked(String),
}

/// What the bot knows for this turn, if there's a knowledge source.
fn knowledge(core: &Core, person: Option<&str>, text: &str, turn: u32) -> Option<KnowledgeBlock> {
    let source = core.shared.setup.knowledge.as_ref()?;
    let sites: Vec<String> = core
        .page
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .into_iter()
        .collect();
    let block = source.for_turn(&KnowledgeQuery {
        bot: &core.bot,
        person,
        text,
        sites: &sites,
        budget_tokens: 2_000,
    });
    if !block.ids.is_empty() {
        source.used(&block.ids, &core.id, turn);
    }
    Some(block)
}

/// A window in tokens as the estimator counts them for `model` (so a
/// model that counts more tokens than estimated gets less room).
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn to_estimate(core: &Core, model: &str, window: u64) -> u64 {
    let probe = 100_000;
    let counted = core.shared.calibration.adjust(model, probe).max(1);
    (window as f64 * probe as f64 / counted as f64) as u64
}
