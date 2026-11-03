//! oxbot's own agent loop: the engine behind `OpenRouter` bots.
//!
//! A turn runs one step at a time ([`runner`]): build the context, pick a
//! model ([`router`]), wait for the household's request budget
//! ([`pacer`]), stream the answer, then run each tool call it asked for,
//! but only after oxbot's gate said yes ([`gate`]). Every step is saved
//! ([`store`]) so a turn picks up where it left off after a restart, and
//! every model request is metered ([`meter`]).
//!
//! The loop never decides what a tool may do: the hub does, through
//! [`oxbot_engine::EngineEvent::Permission`], with the same policy as for
//! Claude bots. Tools have the same names as on Claude, so the hub reads
//! both engines the same way.

// Product names (OpenRouter, Claude) read better in docs without backticks.
#![allow(clippy::doc_markdown)]

pub mod budget;
pub mod compact;
pub mod context;
pub mod gate;
pub mod meter;
pub mod pacer;
pub mod page;
pub mod router;
mod runner;
mod session;
pub mod store;
pub mod tools;

use std::fmt;
use std::sync::{Arc, RwLock};

use futures_util::future::BoxFuture;
use oxbot_engine::{
    CallKind, Engine, EngineError, EngineKind, ModelChoice, NotReady, Privacy, SessionSpec, Started,
};
use oxbot_llm::{ChatModel, KeyCell, Message};

pub use compact::{Compaction, Compactor, DropOldTurns, ModelCaller};
pub use context::{Built, ContextBuilder, LastCall, PlainContext, Pressure, TaskView, TokenBudget};
pub use meter::{GuardQuery, LimitHit, NoLimits, UsageGuard};
pub use pacer::{Account, Lane, Pacer};
pub use router::{CatalogCache, Router};
pub use store::{LoopStore, LoopStoreError, StoredMessage, TurnRow, TurnStatus};
pub use tools::browser::{BrowserFactory, BrowserOutput, BrowserTools};
pub use tools::mcp::McpBrowserFactory;
pub use tools::{ToolOutcome, ToolRegistry};

/// Household settings for the loop, changed by the owner.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopPrefs {
    /// Paid models may answer after every free one failed.
    pub paid_fallback: bool,
    /// The paid models the owner picked, in order.
    pub paid_models: Vec<String>,
    /// The model every person's Herder tries first (`None`: the router
    /// picks).
    pub herder_model: Option<String>,
    /// Ask for providers that don't keep data for every bot, not only
    /// private ones.
    pub deny_data_collection_all: bool,
    /// Longest answer to ask for, in tokens.
    pub max_tokens: u32,
    /// Most model requests in one turn.
    pub max_steps: u32,
}

impl Default for LoopPrefs {
    fn default() -> Self {
        Self {
            paid_fallback: false,
            paid_models: Vec::new(),
            herder_model: None,
            deny_data_collection_all: false,
            max_tokens: 4096,
            max_steps: 50,
        }
    }
}

/// What a knowledge source is asked at the start of a turn.
#[derive(Debug, Clone, Copy)]
pub struct KnowledgeQuery<'a> {
    /// The bot.
    pub bot: &'a str,
    /// The person the turn acts for.
    pub person: Option<&'a str>,
    /// The message.
    pub text: &'a str,
    /// Sites the session is on.
    pub sites: &'a [String],
    /// Most tokens the block may take.
    pub budget_tokens: u32,
}

/// What the bot knows that matters for a turn: one ready, escaped block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnowledgeBlock {
    /// The block, as the model gets it.
    pub text: String,
    /// The items in it.
    pub ids: Vec<String>,
    /// Whether any of it came from outside text.
    pub untrusted: bool,
}

/// What a bot knows (#20 implements it).
pub trait KnowledgeSource: Send + Sync {
    /// The block for a turn.
    fn for_turn(&self, query: &KnowledgeQuery<'_>) -> KnowledgeBlock;
    /// The items a turn was given.
    fn used(&self, ids: &[String], session: &str, turn: u32);
}

/// Everything the loop engine runs with. The host builds it once.
pub struct LoopSetup {
    /// The chat model (OpenRouter's client; a scripted model in tests).
    pub model: Arc<dyn ChatModel>,
    /// A helper model for compaction and reflection, if the owner set one.
    pub helper: Option<Arc<dyn ChatModel>>,
    /// The model list.
    pub catalog: Arc<CatalogCache>,
    /// Where turns, messages and requests are saved.
    pub store: Arc<dyn LoopStore>,
    /// The host's limits.
    pub guard: Arc<dyn UsageGuard>,
    /// The household's request pacer.
    pub pacer: Arc<Pacer>,
    /// Household settings.
    pub prefs: Arc<RwLock<LoopPrefs>>,
    /// What bots know (#20).
    pub knowledge: Option<Arc<dyn KnowledgeSource>>,
    /// Makes each session's browser tool.
    pub browser: Option<Arc<dyn BrowserFactory>>,
    /// Browser tools models may use (their own names).
    pub browser_allow: Vec<String>,
    /// The OpenRouter key: the engine is ready while it's set. `None`:
    /// always ready (no key needed).
    pub key: Option<KeyCell>,
    /// Builds what the model is sent.
    pub context: Arc<dyn ContextBuilder>,
    /// Makes room when it's too long.
    pub compactor: Arc<dyn Compactor>,
}

impl LoopSetup {
    /// A setup with `model` and `store`, a fresh pacer, an empty model
    /// list, no limits, no browser, and the plain context.
    #[must_use]
    pub fn new(model: Arc<dyn ChatModel>, store: Arc<dyn LoopStore>) -> Self {
        Self {
            model,
            helper: None,
            catalog: Arc::new(CatalogCache::default()),
            store,
            guard: Arc::new(NoLimits),
            pacer: Pacer::new(None),
            prefs: Arc::new(RwLock::new(LoopPrefs::default())),
            knowledge: None,
            browser: None,
            browser_allow: Vec::new(),
            key: None,
            context: Arc::new(PlainContext),
            compactor: Arc::new(DropOldTurns),
        }
    }
}

impl fmt::Debug for LoopSetup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopSetup")
            .field("model", &self.model)
            .field("browser_allow", &self.browser_allow)
            .finish_non_exhaustive()
    }
}

/// What a bot says, and Settings → Engines shows, when the OpenRouter
/// account's own privacy settings rule out the free models (OpenRouter's
/// "No endpoints found matching your data policy"). oxbot never changes
/// those settings itself.
pub const DATA_POLICY_HELP: &str = "OpenRouter's privacy settings for this account rule out the \
    free models oxbot tried: their providers may train on what they're sent. To use them, the \
    owner can open openrouter.ai/settings/privacy and switch on \u{201c}Enable free endpoints \
    that may train on inputs\u{201d} (and switch off \u{201c}ZDR Endpoints only\u{201d} if it's \
    on), or switch on paid models in Settings \u{2192} Engines. Private bots never use those \
    providers either way.";

/// Whether the account's data policy ruled out free models lately (set
/// when it did, cleared when a free model answered again).
#[derive(Debug, Default)]
pub(crate) struct DataPolicyFlag(std::sync::atomic::AtomicBool);

impl DataPolicyFlag {
    pub fn set(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn clear(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn get(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// What the engine shares between its sessions.
pub(crate) struct Shared {
    pub setup: LoopSetup,
    pub router: Router,
    pub calibration: budget::Calibration,
    pub data_policy: DataPolicyFlag,
}

/// The loop engine. One per hub.
pub struct LoopEngine {
    shared: Arc<Shared>,
}

impl fmt::Debug for LoopEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopEngine")
            .field("setup", &self.shared.setup)
            .finish()
    }
}

/// One model request outside any session (reflection, judging).
#[derive(Debug, Clone)]
pub struct OneShot {
    /// The bot it's for (its limits and meter).
    pub bot: String,
    /// The person it acts for.
    pub person: Option<String>,
    /// The bot's privacy (provider rules).
    pub privacy: Privacy,
    /// What it's for.
    pub kind: CallKind,
    /// Which model.
    pub model: ModelChoice,
    /// The messages (system first).
    pub messages: Vec<Message>,
    /// Longest answer.
    pub max_tokens: u32,
}

impl LoopEngine {
    /// An engine running with `setup`.
    #[must_use]
    pub fn new(setup: LoopSetup) -> Self {
        let router = Router::new(
            Arc::clone(&setup.catalog),
            Arc::clone(&setup.store),
            Arc::clone(&setup.prefs),
        );
        Self {
            shared: Arc::new(Shared {
                setup,
                router,
                calibration: budget::Calibration::default(),
                data_policy: DataPolicyFlag::default(),
            }),
        }
    }

    /// The household settings (to change them).
    #[must_use]
    pub fn prefs(&self) -> Arc<RwLock<LoopPrefs>> {
        Arc::clone(&self.shared.setup.prefs)
    }

    /// The model list (to refresh it).
    #[must_use]
    pub fn catalog(&self) -> Arc<CatalogCache> {
        Arc::clone(&self.shared.setup.catalog)
    }

    /// Whether the OpenRouter account's privacy settings ruled out the
    /// free models lately (see [`DATA_POLICY_HELP`]).
    #[must_use]
    pub fn data_policy_blocked(&self) -> bool {
        self.shared.data_policy.get()
    }

    /// Whether the OpenRouter key is still being read from the Keychain.
    #[must_use]
    pub fn waiting_for_keychain(&self) -> bool {
        self.shared
            .setup
            .key
            .as_ref()
            .is_some_and(KeyCell::is_loading)
    }

    /// The pacer (for key info).
    #[must_use]
    pub fn pacer(&self) -> Arc<Pacer> {
        Arc::clone(&self.shared.setup.pacer)
    }

    /// Turns still marked running, as the store has them.
    ///
    /// # Errors
    /// The store failed.
    pub fn running_turns(&self) -> Result<Vec<TurnRow>, LoopStoreError> {
        self.shared.setup.store.running_turns()
    }

    /// Ends a turn that won't be picked up again.
    ///
    /// # Errors
    /// The store failed.
    pub fn end_turn(
        &self,
        session: &str,
        turn: u32,
        status: TurnStatus,
    ) -> Result<(), LoopStoreError> {
        self.shared.setup.store.end_turn(session, turn, status)
    }

    /// Sends one request outside any session, routed, paced and metered
    /// like every other, with `req.privacy`'s provider rules. Returns the
    /// answer's text.
    pub fn one_shot(&self, req: OneShot) -> BoxFuture<'_, Result<String, EngineError>> {
        Box::pin(async move {
            self.ready()?;
            runner::one_shot(&self.shared, req)
                .await
                .map_err(EngineError::Other)
        })
    }
}

impl Engine for LoopEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::OpenRouter
    }

    fn ready(&self) -> Result<(), NotReady> {
        match &self.shared.setup.key {
            Some(key) if key.is_loading() => Err(NotReady(oxbot_llm::KEYCHAIN_WAITING.to_owned())),
            Some(key) if !key.is_set() => Err(NotReady(
                "OpenRouter isn't signed in on this Mac yet. The owner can sign in under \
                 Settings → Engines."
                    .to_owned(),
            )),
            _ => Ok(()),
        }
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<Started, EngineError>> {
        Box::pin(async move {
            self.ready()?;
            session::start(Arc::clone(&self.shared), spec).await
        })
    }
}

/// SHA-256 (hex) of the stable prefix: the system prompt and the tools in
/// their order.
#[must_use]
pub fn prefix_hash(system: &str, tools: &[oxbot_llm::ToolSpec]) -> String {
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(system.as_bytes());
    ctx.update(b"\n");
    ctx.update(serde_json::to_string(tools).unwrap_or_default().as_bytes());
    ctx.finish()
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests;
