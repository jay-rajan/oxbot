//! Model requests: [`Caller`] picks the model, checks the host's limits,
//! waits for the pacer, streams, meters and sorts out errors, for the
//! relay's requests ([`crate::gateway`]) and for helper requests (page
//! questions) with the same privacy and routing.

use std::sync::Arc;
use std::time::Duration;

use oxbot_engine::{Activity, CallKind, CallRecord, EngineEvent, ModelChoice, Privacy};
use oxbot_llm::{
    ChatDelta, ChatModel, ChatRequest, ChatResponse, ErrorKind, KEYCHAIN_WAITING, LlmError,
    Message, Provider, ProviderPrefs, RetryClass, ToolSpec, complete,
};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::meter::{GuardQuery, now_ms, outcome_of, utc_day};
use crate::pacer::{Lane, PacerStop};
use crate::router::{Pick, Route, TurnRouting};
use crate::{DATA_POLICY_HELP, Shared};

/// Makes model requests on a session's behalf (a page question).
pub trait ModelCaller: Send + Sync {
    /// Sends `messages` (no tools) and returns the answer's text.
    fn call(
        &self,
        kind: CallKind,
        messages: Vec<Message>,
        max_tokens: u32,
    ) -> futures_util::future::BoxFuture<'_, Result<String, LlmError>>;
}

/// Waits before retrying the same model after a network or server error.
const RETRY_WAITS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(15),
];
/// Longest wait for a model to cool down within one step.
const MAX_COOL_WAIT: Duration = Duration::from_secs(90);

const ZDR_DROPPED: &str = "No zero-retention provider was free; used one that doesn't keep data";

/// The engine helper requests are metered for: Codex, the only one.
const ENGINE: &str = "codex";

/// A request on its way: its provider rules and when it went out.
#[derive(Debug, Clone)]
pub(crate) struct Sending {
    pub provider: Option<ProviderPrefs>,
    pub private: bool,
    pub ts_ms: i64,
    pub started: Instant,
}

/// What answered a request, for the meter.
#[derive(Debug, Clone, Default)]
pub(crate) struct Answered {
    pub model: String,
    pub generation_id: Option<String>,
    pub usage: oxbot_llm::CallUsage,
}

/// Why a turn ends early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// Stopped by the person (or the session is stopping).
    Cancelled,
    /// Failed: tell the person this.
    Fail(String),
}

/// What to do after a failed request.
pub(crate) enum Next {
    Again,
    Compact,
    Stop(Stop),
}

/// Makes model requests for one turn (or one-off request): picks the
/// model, checks limits, paces, streams, meters, and sorts out errors.
pub(crate) struct Caller<'a> {
    pub shared: &'a Shared,
    pub model: Arc<dyn ChatModel>,
    pub bot: &'a str,
    pub person: Option<&'a str>,
    pub session: Option<&'a str>,
    pub turn: Option<u32>,
    pub step: u32,
    pub privacy: Privacy,
    pub lane: Lane,
    pub kind: CallKind,
    /// The engine that asked, for the meter (`openrouter`: the loop).
    pub engine: &'a str,
    /// Live text and status go here (a turn's own requests only).
    pub events: Option<&'a mpsc::Sender<EngineEvent>>,
    /// Each request's meter record goes here too (a turn's requests and
    /// its helpers'), so the chat's count and practice runs include them.
    pub meter: Option<&'a mpsc::Sender<EngineEvent>>,
    pub cancel: &'a CancellationToken,
    pub routing: TurnRouting,
    pub zdr_dropped: bool,
    pub same_retries: usize,
    pub cooled_for: Duration,
    /// What the service said about the last request that failed.
    pub last_error: Option<String>,
    /// A model was ruled out by the account's data policy (on a request
    /// that didn't ask for it).
    pub data_policy: bool,
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
            ", or switch on paid models in Settings → AI models"
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
                // A private bot every free model's providers refused (some
                // also busy): that's the reason, not "busy".
                Pick::AllCooling(_) if self.private() && !self.routing.no_provider.is_empty() => {
                    return Err(self.nothing_left(free_ok));
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
                "This bot is private, so it may only use model providers that don't keep its \
                 data, and none of the free models has one right now. You can make the bot \
                 shared{}, or try again later.{}",
                if self.paid_note().is_empty() {
                    ""
                } else {
                    ", switch on paid models in Settings → AI models"
                },
                self.said()
            ));
        }
        if self.data_policy {
            return Stop::Fail(format!("{DATA_POLICY_HELP}{}", self.said()));
        }
        if !free_ok {
            return Stop::Fail(
                "Today's free AI questions are used up; they reset at midnight UTC.".to_owned(),
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
    pub(crate) async fn next_route(&mut self) -> Result<Route, Stop> {
        loop {
            match self.route().await {
                Err(Stop::Fail(m)) if m.is_empty() => {}
                other => return other,
            }
        }
    }

    /// Waits for the pacer (free models) and says a request is going out:
    /// what's needed to send on `route` and meter it after.
    ///
    /// # Errors
    /// Stopped, or the free daily requests are used up.
    pub(crate) async fn begin(&mut self, route: &Route) -> Result<Sending, LlmError> {
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
                        "Today's free AI questions are used up.",
                    ));
                }
            }
        }
        self.activity(Activity::Requesting);
        let provider = self.provider_prefs();
        let private = provider
            .as_ref()
            .is_some_and(ProviderPrefs::denies_data_collection);
        Ok(Sending {
            provider,
            private,
            ts_ms: now_ms(),
            started: Instant::now(),
        })
    }

    /// Sends one request on `route`, metering it either way.
    pub(crate) async fn send(
        &mut self,
        route: &Route,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    ) -> Result<ChatResponse, LlmError> {
        let sending = self.begin(route).await?;
        let Sending {
            provider,
            private,
            ts_ms,
            started,
        } = sending;
        let req = ChatRequest {
            fallbacks: route.fallbacks.clone(),
            tools,
            max_tokens: Some(route.max_tokens),
            provider,
            supported: Some(route.supported.clone()),
            ..ChatRequest::new(route.primary.id.clone(), messages)
        };
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
        let sending = Sending {
            provider: None,
            private,
            ts_ms,
            started,
        };
        let answered = result.as_ref().map(|resp| Answered {
            model: resp.model.clone(),
            generation_id: resp.generation_id.clone(),
            usage: resp.usage,
        });
        self.record(route, &sending, answered.as_ref().map_err(|e| (*e).clone()))
            .await;
        result
    }

    /// Meters one request that `sending` began on `route`: what answered,
    /// or how it failed.
    pub(crate) async fn record(
        &self,
        route: &Route,
        sending: &Sending,
        result: Result<&Answered, LlmError>,
    ) -> CallRecord {
        let latency_ms = u64::try_from(sending.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut record = CallRecord {
            ts_ms: sending.ts_ms,
            day: utc_day(sending.ts_ms),
            bot: self.bot.to_owned(),
            person: self.person.map(str::to_owned),
            session: self.session.map(str::to_owned),
            turn: self.turn,
            step: Some(self.step),
            provider: self.model.provider().as_str().to_owned(),
            requested: route.primary.id.clone(),
            free: route.primary.free,
            private: sending.private,
            latency_ms,
            kind: self.kind,
            engine: self.engine.to_owned(),
            ..CallRecord::default()
        };
        match &result {
            Ok(answered) => {
                record.model.clone_from(&answered.model);
                record.generation_id.clone_from(&answered.generation_id);
                record.prompt_tokens = answered.usage.prompt_tokens;
                record.completion_tokens = answered.usage.completion_tokens;
                record.cached_tokens = answered.usage.cached_tokens;
                record.cost_usd = self.cost(route, answered);
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
        if let Some(meter) = self.meter {
            let _ = meter.send(EngineEvent::ModelCall(record.clone())).await;
        }
        record
    }

    /// What a request cost: as the service says, else from the list's
    /// prices.
    #[allow(clippy::cast_precision_loss)]
    fn cost(&self, route: &Route, answered: &Answered) -> f64 {
        if let Some(cost) = answered.usage.cost_usd {
            return cost;
        }
        let catalog = self.shared.router.catalog();
        let info = catalog
            .get(&answered.model)
            .or(route.primary.info.as_ref())
            .filter(|m| !m.free);
        info.map_or(0.0, |m| {
            m.prompt_usd_per_token * answered.usage.prompt_tokens as f64
                + m.completion_usd_per_token * answered.usage.completion_tokens as f64
        })
    }

    /// A request on `route` answered.
    pub(crate) fn answered(&mut self, route: &Route, resp: &ChatResponse) {
        self.answered_by(route, &resp.model);
    }

    /// A request on `route` answered, by `model` (empty: unknown).
    pub(crate) fn answered_by(&mut self, route: &Route, model: &str) {
        self.shared.router.ok(&route.primary.id);
        if route.primary.free && !self.private() {
            // Free models answer requests without "deny" again.
            self.shared.data_policy.clear();
        }
        self.same_retries = 0;
        self.cooled_for = Duration::ZERO;
        let answered = if model.is_empty() {
            route.primary.id.clone()
        } else {
            model.to_owned()
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
    pub(crate) async fn after_error(&mut self, route: &Route, err: &LlmError) -> Next {
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
                     again (Settings → AI models)."
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

/// A [`ModelCaller`] for helpers inside a turn (page questions): its own
/// routing, the turn's bot, person, privacy and stop.
pub(crate) struct HelperCaller<'a> {
    pub shared: &'a Shared,
    pub bot: &'a str,
    pub person: Option<&'a str>,
    pub session: Option<&'a str>,
    pub turn: Option<u32>,
    pub step: u32,
    pub privacy: Privacy,
    pub lane: Lane,
    pub choice: ModelChoice,
    pub cancel: &'a CancellationToken,
    /// The session's events, for the meter only.
    pub meter: Option<&'a mpsc::Sender<EngineEvent>>,
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
                step: self.step,
                privacy: self.privacy,
                lane: self.lane,
                kind,
                engine: ENGINE,
                events: None,
                meter: self.meter,
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

/// The model for `kind`: the helper for compaction, reflection and page
/// questions when there is one.
fn helper_model(shared: &Shared, kind: CallKind) -> Arc<dyn ChatModel> {
    match (&shared.setup.helper, kind) {
        (Some(helper), CallKind::Compaction | CallKind::Reflection | CallKind::PageQuestion) => {
            Arc::clone(helper)
        }
        _ => Arc::clone(&shared.setup.model),
    }
}

/// Tries on another model after an empty answer (e.g. a reasoning model
/// that spent its whole answer on thinking).
const EMPTY_RETRIES: usize = 1;

/// One request without tools, with retries; the answer's text, never
/// empty (an empty answer moves to the next model once, then fails).
async fn plain_call(
    caller: &mut Caller<'_>,
    messages: Vec<Message>,
    max_tokens: u32,
) -> Result<String, LlmError> {
    let mut empty = 0;
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
                let text = resp.message.text();
                if !text.trim().is_empty() {
                    return Ok(text);
                }
                tracing::info!(model = %route.primary.id, "a helper request came back empty");
                if empty >= EMPTY_RETRIES {
                    return Err(LlmError::new(
                        ErrorKind::Protocol,
                        "the model gave an empty answer",
                    ));
                }
                empty += 1;
                caller.routing.excluded.insert(route.primary.id.clone());
                if let Some(answered) = caller.routing.sticky.take() {
                    caller.routing.excluded.insert(answered);
                }
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
