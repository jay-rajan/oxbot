//! What Codex bots share with oxbot: the tools, the gate and the model
//! requests, from what was oxbot's own agent loop (retired as an engine).
//!
//! - [`service::ToolService`]: a Codex session's tools: the browser, page
//!   tools ([`page`]: maps, find, sections, rows, `ask_page`), files, and
//!   stored results ([`results`]), each asked of oxbot's gate first
//!   ([`gate`]).
//! - [`gateway::Gateway`]: every model request the relay sends: routing
//!   ([`router`]), the household's request budget ([`pacer`]), retries,
//!   limits and the meter ([`meter`]).
//! - [`check`]: the answer check and the wrap-up nudge; [`numbers`]: what
//!   an answer says that nothing read had.
//! - [`store`]: where results, requests and checks are saved.
//!
//! The tools never decide what they may do: the hub does, through
//! [`oxbot_engine::EngineEvent::Permission`].

// Product names (OpenRouter, Codex) read better in docs without backticks.
#![allow(clippy::doc_markdown)]

pub mod budget;
pub mod check;
pub mod context;
pub mod gate;
pub mod gateway;
pub mod meter;
pub mod numbers;
pub mod pacer;
pub mod page;
pub mod results;
pub mod router;
pub mod rows;
mod runner;
pub mod service;
pub mod store;
pub mod tools;

use std::fmt;
use std::sync::{Arc, RwLock};

use oxbot_engine::{NotReady, SessionSpec};
use oxbot_llm::{ChatModel, KeyCell};

pub use budget::TokenBudget;
pub use check::{
    CHECK_AFTER_CALLS, MAX_NAMED, answer_after_check, note as check_note, wrap_up_note,
};
pub use meter::{GuardQuery, LimitHit, NoLimits, UsageGuard};
pub use pacer::{Account, Lane, Pacer};
pub use router::{CatalogCache, Router};
pub use runner::{ModelCaller, Stop};
pub use service::{CallCtx, Dispatched, Failures, OwnTools, RepeatGuard, ToolService, TurnHooks};
pub use store::{LoopStore, LoopStoreError};
pub use tools::browser::{BrowserFactory, BrowserOutput, BrowserTools};
pub use tools::mcp::McpBrowserFactory;
pub use tools::page::ASK_PAGE_V1;
pub use tools::{ToolOutcome, ToolRegistry};

/// Household settings for bots' model requests, changed by the owner.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopPrefs {
    /// Paid models may answer after every free one failed (within the
    /// monthly limit).
    pub paid_fallback: bool,
    /// The paid models the owner picked, in order.
    pub paid_models: Vec<String>,
    /// The model every person's Assistant tries first (`None`: the router
    /// picks).
    pub assistant_model: Option<String>,
    /// The list a bot with none of its own (and the Assistant) tries first,
    /// strongest first, so Auto never means the smallest model that's up.
    pub default_models: Vec<String>,
    /// Longest answer to ask for, in tokens.
    pub max_tokens: u32,
    /// Most model requests in one turn.
    pub max_steps: u32,
}

impl Default for LoopPrefs {
    fn default() -> Self {
        Self {
            // The host sets it from the home's settings (on there).
            paid_fallback: false,
            paid_models: Vec::new(),
            assistant_model: None,
            default_models: Vec::new(),
            max_tokens: 4096,
            max_steps: 50,
        }
    }
}

/// Everything Codex bots' tools and requests run with. The host builds
/// it once.
pub struct LoopSetup {
    /// The chat model (OpenRouter's client; a scripted model in tests).
    pub model: Arc<dyn ChatModel>,
    /// A helper model for page questions, if the owner set one.
    pub helper: Option<Arc<dyn ChatModel>>,
    /// The model list.
    pub catalog: Arc<CatalogCache>,
    /// Where results, requests and checks are saved.
    pub store: Arc<dyn LoopStore>,
    /// The host's limits.
    pub guard: Arc<dyn UsageGuard>,
    /// The household's request pacer.
    pub pacer: Arc<Pacer>,
    /// Household settings.
    pub prefs: Arc<RwLock<LoopPrefs>>,
    /// Makes each session's browser tool.
    pub browser: Option<Arc<dyn BrowserFactory>>,
    /// Browser tools models may use (their own names).
    pub browser_allow: Vec<String>,
    /// The OpenRouter key: the engine is ready while it's set. `None`:
    /// always ready (no key needed).
    pub key: Option<KeyCell>,
    /// How strict an element label is (higher: more care before a click),
    /// the gate's own measure. A page snapshot naming one uid twice keeps
    /// the strictest label.
    pub strictness: Arc<Strictness>,
}

/// How strict an element label (`button "Pay now"`) is.
pub type Strictness = dyn Fn(&str) -> u8 + Send + Sync;

impl LoopSetup {
    /// A setup with `model` and `store`, a fresh pacer, an empty model
    /// list, no limits and no browser.
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
            browser: None,
            browser_allow: Vec::new(),
            key: None,
            strictness: Arc::new(|_: &str| 0),
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
    on), or switch on paid models in Settings \u{2192} General.";

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

/// What Codex sessions share.
pub(crate) struct Shared {
    pub setup: LoopSetup,
    pub router: Router,
    pub data_policy: DataPolicyFlag,
}

/// What every Codex bot runs with: the tools, gate, routing, pacer and
/// meter. One per hub. (Its name is from when it was oxbot's own engine.)
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

impl LoopEngine {
    /// The host for `setup`.
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
                data_policy: DataPolicyFlag::default(),
            }),
        }
    }

    /// The routing, limits, pacer and meter for Codex's requests (through
    /// oxbot's relay).
    #[must_use]
    pub fn gateway(&self) -> gateway::Gateway {
        gateway::Gateway::new(Arc::clone(&self.shared))
    }

    /// A Codex session's tools: oxbot's tools, gate, stored results and
    /// page tools, asking the gate and sending results through `events`.
    pub async fn open_tools(
        &self,
        spec: &SessionSpec,
        events: tokio::sync::mpsc::Sender<oxbot_engine::EngineEvent>,
        own: OwnTools,
    ) -> ToolService {
        ToolService::open(Arc::clone(&self.shared), spec, events, own).await
    }

    /// Records how a turn's final answer was checked (`loop_checks`).
    ///
    /// # Errors
    /// The store failed.
    pub fn record_check(&self, row: &store::CheckRow) -> Result<(), LoopStoreError> {
        self.shared.setup.store.record_check(row)
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

    /// Whether bots can use models now: the OpenRouter key is set (and
    /// not still being read from the Keychain).
    ///
    /// # Errors
    /// [`NotReady`] with what's missing, in words for the person.
    pub fn ready(&self) -> Result<(), NotReady> {
        match &self.shared.setup.key {
            Some(key) if key.is_loading() => Err(NotReady(oxbot_llm::KEYCHAIN_WAITING.to_owned())),
            Some(key) if !key.is_set() => Err(NotReady(
                "The home's AI account (OpenRouter) isn't connected yet. The owner can connect it \
                 under Settings → General."
                    .to_owned(),
            )),
            _ => Ok(()),
        }
    }
}
