//! Model requests another engine makes through oxbot (Codex, through
//! oxbot's relay): routed, limited, paced and metered exactly as the loop's
//! own. The engine brings the request; [`Gateway::request`] picks the
//! model (the bot's pin, then the free models, best first), checks the
//! host's limits, waits for the pacer, says which provider rules apply,
//! and lets the engine send it. A model that is busy or rate-limited cools
//! down and the next one is tried, so one model's 429 doesn't fail the
//! request. What answered is metered when the engine says the answer is in
//! ([`Ticket::done`]), so a streamed answer is counted once it's whole.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use oxbot_engine::{CallKind, CallRecord, ModelChoice, Privacy};
use oxbot_llm::{CallUsage, LlmError, ProviderPrefs};
use tokio_util::sync::CancellationToken;

use crate::Shared;
use crate::pacer::Lane;
use crate::router::{Pick, Route, TurnRouting};
use crate::runner::{Answered, Caller, Next, Sending, Stop};

/// The longest answer a request through the gateway may ask for (or the
/// model's own limit, if lower). Reasoning models spend output on thinking,
/// and an engine like Codex can't continue an answer that was cut off, so the
/// loop's 4,096 is too small here.
const GATEWAY_MAX_OUTPUT: u32 = 32_768;

/// Who a request is for, as the host knows it (never from the request).
#[derive(Debug, Clone)]
pub struct Asker {
    /// The bot.
    pub bot: String,
    /// The person its turn acts for.
    pub person: Option<String>,
    /// The bot's session.
    pub session: Option<String>,
    /// The turn in that session.
    pub turn: Option<u32>,
    /// The request's number in the turn.
    pub step: u32,
    /// Whether the bot is private (providers that keep no data).
    pub privacy: Privacy,
    /// The pacer queue.
    pub lane: Lane,
    /// The engine asking, for the meter (e.g. `codex`).
    pub engine: String,
    /// What it's for.
    pub kind: CallKind,
}

/// One try: the model and the rules to send it with.
#[derive(Debug, Clone, PartialEq)]
pub struct Attempt {
    /// The model asked first.
    pub model: String,
    /// Models the provider may try next on its side.
    pub fallbacks: Vec<String>,
    /// The longest answer to ask for.
    pub max_tokens: u32,
    /// Request parameters every one of them takes (`None`: not known, send
    /// what the request has).
    pub supported: Option<Vec<String>>,
    /// `OpenRouter`'s provider rules (privacy).
    pub provider: Option<ProviderPrefs>,
    /// A free model.
    pub free: bool,
}

/// Why no model answered.
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayStop {
    /// Stopped while waiting.
    Cancelled,
    /// What to tell the person, in oxbot's words (a limit, no model left).
    Fail(String),
    /// The provider refused the request itself (too long, say): its error,
    /// for the engine to handle.
    Upstream(LlmError),
}

/// What the engine said about the answer, for the meter.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnswerUsage {
    /// The model that answered (empty: not said).
    pub model: String,
    /// The provider's id for it.
    pub generation_id: Option<String>,
    /// Its tokens and cost.
    pub usage: CallUsage,
}

/// A request the provider took: meter it with [`Ticket::done`].
pub struct Ticket {
    shared: Arc<Shared>,
    asker: Asker,
    route: Route,
    sending: Sending,
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("bot", &self.asker.bot)
            .field("model", &self.route.primary.id)
            .finish_non_exhaustive()
    }
}

impl Ticket {
    /// The model it was sent to.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.route.primary.id
    }

    /// Meters it: answered with `result`, or how it failed after all (a
    /// stream that broke, or was stopped). Returns the meter's row.
    pub async fn done(self, result: Result<AnswerUsage, LlmError>) -> CallRecord {
        let cancel = CancellationToken::new();
        let caller = caller(&self.shared, &self.asker, TurnRouting::default(), &cancel);
        let answered = result.map(|a| Answered {
            model: a.model,
            generation_id: a.generation_id,
            usage: a.usage,
        });
        caller
            .record(
                &self.route,
                &self.sending,
                answered.as_ref().map_err(Clone::clone),
            )
            .await
    }
}

/// What the provider took: the engine's value (an open stream, say) and
/// its ticket.
#[derive(Debug)]
pub struct Opened<T> {
    /// What `send` returned.
    pub value: T,
    /// For the meter.
    pub ticket: Ticket,
}

/// The loop's routing, limits, pacer and meter, for other engines.
#[derive(Clone)]
pub struct Gateway {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gateway")
    }
}

fn caller<'a>(
    shared: &'a Shared,
    asker: &'a Asker,
    routing: TurnRouting,
    cancel: &'a CancellationToken,
) -> Caller<'a> {
    Caller {
        shared,
        model: Arc::clone(&shared.setup.model),
        bot: &asker.bot,
        person: asker.person.as_deref(),
        session: asker.session.as_deref(),
        turn: asker.turn,
        step: asker.step,
        privacy: asker.privacy,
        lane: asker.lane,
        kind: asker.kind,
        engine: &asker.engine,
        events: None,
        meter: None,
        cancel,
        routing,
        zdr_dropped: false,
        same_retries: 0,
        cooled_for: Duration::ZERO,
        last_error: None,
        data_policy: false,
    }
}

impl Gateway {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// The model the router would ask first now for `choice`, the window a
    /// session may count on (the smallest of it and its fallbacks) and the
    /// longest answer. `None` when no model can be picked.
    #[must_use]
    pub fn window_for(&self, choice: &ModelChoice, central: bool) -> Option<(String, u64, u32)> {
        let routing = TurnRouting {
            choice: choice.clone(),
            central,
            ..TurnRouting::default()
        };
        let free_ok = !self.shared.setup.pacer.free_used_up();
        match self.shared.router.pick(&routing, free_ok) {
            Pick::Route(route) => Some((route.primary.id, route.window, route.max_tokens)),
            _ => None,
        }
    }

    /// Sends one request for `asker`: `send` gets each try's model and
    /// rules and sends it, until one is taken. `routing` is the turn's
    /// routing state (kept between its requests: the model that answered
    /// last goes first).
    ///
    /// # Errors
    /// Why no model took it.
    pub async fn request<T, F, Fut>(
        &self,
        asker: &Asker,
        routing: &mut TurnRouting,
        cancel: &CancellationToken,
        mut send: F,
    ) -> Result<Opened<T>, GatewayStop>
    where
        F: FnMut(Attempt) -> Fut,
        Fut: Future<Output = Result<T, LlmError>>,
    {
        let mut caller = caller(&self.shared, asker, routing.clone(), cancel);
        let result = loop {
            let route = match caller.next_route().await {
                Ok(route) => route,
                Err(Stop::Cancelled) => break Err(GatewayStop::Cancelled),
                Err(Stop::Fail(m)) => break Err(GatewayStop::Fail(m)),
            };
            let tried = match caller.begin(&route).await {
                Ok(sending) => {
                    let attempt = Attempt {
                        model: route.primary.id.clone(),
                        fallbacks: route.fallbacks.clone(),
                        max_tokens: route
                            .primary
                            .info
                            .as_ref()
                            .map_or(GATEWAY_MAX_OUTPUT, |m| m.max_tokens(GATEWAY_MAX_OUTPUT)),
                        supported: route.primary.info.as_ref().map(|_| route.supported.clone()),
                        provider: sending.provider.clone(),
                        free: route.primary.free,
                    };
                    match send(attempt).await {
                        Ok(value) => Ok((value, sending)),
                        Err(err) => {
                            caller.record(&route, &sending, Err(err.clone())).await;
                            Err(err)
                        }
                    }
                }
                Err(err) => Err(err),
            };
            match tried {
                Ok((value, sending)) => {
                    caller.answered_by(&route, "");
                    break Ok(Opened {
                        value,
                        ticket: Ticket {
                            shared: Arc::clone(&self.shared),
                            asker: asker.clone(),
                            route,
                            sending,
                        },
                    });
                }
                Err(err) => match caller.after_error(&route, &err).await {
                    Next::Again => {}
                    Next::Compact => break Err(GatewayStop::Upstream(err)),
                    Next::Stop(Stop::Cancelled) => break Err(GatewayStop::Cancelled),
                    Next::Stop(Stop::Fail(m)) => break Err(GatewayStop::Fail(m)),
                },
            }
        };
        *routing = caller.routing.clone();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryLoopStore;
    use crate::{LoopEngine, LoopSetup};
    use oxbot_llm::{Catalog, ErrorKind, ModelInfo, ScriptedModel};

    fn model(id: &str, ctx: u32) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: id.into(),
            context_length: ctx,
            max_completion: Some(8000),
            prompt_usd_per_token: 0.0,
            completion_usd_per_token: 0.0,
            tools: true,
            images_in: false,
            text_io: true,
            free: true,
            supported_parameters: ["tools", "max_tokens"].map(str::to_owned).to_vec(),
        }
    }

    fn engine(store: Arc<MemoryLoopStore>) -> LoopEngine {
        let mut setup = LoopSetup::new(Arc::new(ScriptedModel::new()), store);
        setup.catalog = Arc::new(crate::CatalogCache::new(Catalog {
            models: vec![model("a/big:free", 200_000), model("b/small:free", 70_000)],
        }));
        LoopEngine::new(setup)
    }

    fn asker() -> Asker {
        Asker {
            bot: "scout".into(),
            person: Some("p1".into()),
            session: Some("s1".into()),
            turn: Some(0),
            step: 0,
            privacy: Privacy::Private,
            lane: Lane::Live,
            engine: "codex".into(),
            kind: CallKind::Chat,
        }
    }

    #[tokio::test]
    async fn a_rate_limited_model_hands_over_to_the_next_and_both_are_metered() {
        let store = Arc::new(MemoryLoopStore::new());
        let gateway = engine(Arc::clone(&store)).gateway();
        let mut routing = TurnRouting::default();
        let cancel = CancellationToken::new();
        let mut tried = Vec::new();
        let opened = gateway
            .request(&asker(), &mut routing, &cancel, |attempt| {
                tried.push(attempt.clone());
                let n = tried.len();
                async move {
                    if n == 1 {
                        Err(LlmError::new(ErrorKind::RateLimited, "busy"))
                    } else {
                        Ok(attempt.model)
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(tried.len(), 2);
        assert_ne!(tried[0].model, tried[1].model);
        // A private bot's rules: no data collection.
        assert!(tried[0].provider.as_ref().unwrap().denies_data_collection());
        let record = opened
            .ticket
            .done(Ok(AnswerUsage {
                model: opened.value.clone(),
                usage: CallUsage {
                    prompt_tokens: 10,
                    completion_tokens: 2,
                    ..CallUsage::default()
                },
                ..AnswerUsage::default()
            }))
            .await;
        assert_eq!(record.engine, "codex");
        assert_eq!(record.outcome, "ok");
        let calls = store.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].outcome, "rate_limited");
        assert_eq!(calls[1].prompt_tokens, 10);
        assert_eq!(calls[1].person.as_deref(), Some("p1"));
        assert_eq!(routing.sticky.as_deref(), Some(opened.value.as_str()));
    }

    #[tokio::test]
    async fn an_engine_may_ask_for_the_models_whole_answer_length() {
        let store = Arc::new(MemoryLoopStore::new());
        let gateway = engine(store).gateway();
        let mut routing = TurnRouting::default();
        let cancel = CancellationToken::new();
        let opened = gateway
            .request(&asker(), &mut routing, &cancel, |a| async move {
                Ok::<_, LlmError>(a)
            })
            .await
            .unwrap();
        // The model's own limit (8000), not the loop's 4096.
        assert_eq!(opened.value.max_tokens, 8000);
    }

    #[tokio::test]
    async fn a_window_floor_leaves_out_smaller_models() {
        let store = Arc::new(MemoryLoopStore::new());
        let gateway = engine(store).gateway();
        let mut routing = TurnRouting {
            min_window: Some(100_000),
            ..TurnRouting::default()
        };
        let cancel = CancellationToken::new();
        let opened = gateway
            .request(&asker(), &mut routing, &cancel, |a| async move {
                Ok::<_, LlmError>(a)
            })
            .await
            .unwrap();
        assert_eq!(opened.value.model, "a/big:free");
        assert_eq!(opened.value.fallbacks, Vec::<String>::new());
        assert_eq!(
            gateway.window_for(&ModelChoice::Auto, false).map(|w| w.1),
            Some(70_000)
        );
    }
}
