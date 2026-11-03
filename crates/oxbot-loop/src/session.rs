//! One bot's loop session: its conversation, its tools, and the task that
//! runs its current turn.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use oxbot_engine::{
    EngineError, EngineEvent, GateMode, ModelChoice, Privacy, ResumedTurn, Session, SessionSpec,
    Started, TurnInput,
};
use oxbot_llm::ToolSpec;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::context::LastCall;
use crate::context::state::WindowState;
use crate::gate::GateCheck;
use crate::results::ResultStore;
use crate::store::{StoredMessage, TurnRow};
use crate::tools::ToolRegistry;
use crate::tools::browser::{BrowserTools, model_specs};
use crate::tools::pages::PageTracker;
use crate::{Shared, prefix_hash, turn};

/// Room for events the hub hasn't read yet.
const EVENT_ROOM: usize = 256;
/// How long a stopping session waits for its turn to wind down.
const GRACE: Duration = Duration::from_secs(5);

/// A session's conversation, held by the turn that runs.
pub(crate) struct Conv {
    pub messages: Vec<StoredMessage>,
    pub context: Value,
    /// The context changed and isn't saved yet.
    pub context_dirty: bool,
    pub next_turn: u32,
    pub last_call: Option<LastCall>,
    /// Models that refused this session's requests as invalid.
    pub excluded: HashSet<String>,
}

impl Conv {
    /// The next message number.
    pub fn next_idx(&self) -> u32 {
        self.messages.last().map_or(0, |m| m.idx + 1)
    }
}

/// What a session's turns share.
pub(crate) struct Core {
    pub shared: Arc<Shared>,
    pub bot: String,
    pub id: String,
    pub work_dir: PathBuf,
    pub system: String,
    pub tools: ToolRegistry,
    pub specs: Vec<ToolSpec>,
    pub privacy: Privacy,
    pub gate_mode: GateMode,
    pub choice: ModelChoice,
    pub central: bool,
    pub events: mpsc::Sender<EngineEvent>,
    pub gate: GateCheck,
    pub browser: Option<Arc<dyn BrowserTools>>,
    /// The browser's tabs, as the session last saw them.
    pub pages: Mutex<PageTracker>,
    /// Stored tool results.
    pub results: ResultStore,
    pub conv: tokio::sync::Mutex<Conv>,
    /// A turn that was running when oxbot stopped, to go on with.
    pub resume: Mutex<Option<TurnRow>>,
    /// The session is stopping: a turn cut short stays saved as running.
    pub stopping: AtomicBool,
}

impl Core {
    pub fn take_resume(&self) -> Option<TurnRow> {
        self.resume
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Relaxed)
    }

    /// The selected tab's address, if known.
    pub fn current_url(&self) -> Option<String> {
        self.pages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .current()
            .map(|p| p.url)
            .filter(|u| !u.is_empty())
    }
}

/// Starts a session for `spec`.
pub(crate) async fn start(shared: Arc<Shared>, spec: SessionSpec) -> Result<Started, EngineError> {
    let setup = &shared.setup;
    let (browser, browser_specs) = open_browser(&shared, &spec).await;
    let host = spec
        .host_tool_specs
        .iter()
        .map(|t| ToolSpec {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.parameters.clone(),
        })
        .collect();
    let tools = ToolRegistry::new(host, browser_specs);
    let specs = tools.specs();
    let hash = prefix_hash(&spec.system_prompt, &specs);
    let store = &setup.store;
    let id = spec.resume.id().to_owned();
    let err =
        |e: crate::LoopStoreError| EngineError::Start(format!("couldn't open the session: {e}"));
    let row = store
        .open_session(&spec.bot, &id, &hash, spec.resume.is_resume())
        .map_err(err)?;
    let messages = if row.existed {
        store.messages(&id).map_err(err)?
    } else {
        Vec::new()
    };
    let running: Vec<TurnRow> = store
        .running_turns()
        .map_err(err)?
        .into_iter()
        .filter(|t| t.session == id)
        .collect();
    let mut context = row.context.clone();
    if !context.is_object() {
        context = json!({});
    }
    let mut context_dirty = false;
    let last_idx = messages.last().map(|m| m.idx);
    if row.prefix_changed
        && let Some(last) = last_idx
    {
        // The rules or tools changed: a new window after what it had (the
        // history stays saved; the task state, list and taint go on).
        let mut state = WindowState::from_value(&context);
        // A turn picked up after the restart starts the new window with its
        // own message, so its calls and results keep their pairs.
        let resumed_from = running
            .iter()
            .max_by_key(|t| t.turn)
            .and_then(|t| messages.iter().find(|m| m.turn == t.turn))
            .map(|m| m.idx);
        state.start_idx = resumed_from.unwrap_or(last + 1);
        state.pending_start = true;
        context = state.to_value();
        context_dirty = true;
    }
    let next_turn = messages
        .iter()
        .map(|m| m.turn + 1)
        .chain(running.iter().map(|t| t.turn + 1))
        .max()
        .unwrap_or(0);
    let resume = running.into_iter().max_by_key(|t| t.turn);
    let resumed_turn = resume.as_ref().map(|t| ResumedTurn {
        user_seq: t.user_seq,
        origin: t.origin.clone(),
        question: t.question.clone(),
    });

    let (events, events_rx) = mpsc::channel(EVENT_ROOM);
    let core = Arc::new(Core {
        bot: spec.bot.clone(),
        id: id.clone(),
        work_dir: spec.work_dir.clone(),
        system: spec.system_prompt.clone(),
        tools,
        specs,
        privacy: spec.privacy,
        gate_mode: spec.gate_mode,
        choice: spec.model.clone(),
        central: spec.host_tools.central,
        gate: GateCheck::new(events.clone()),
        events,
        browser,
        pages: Mutex::new(PageTracker::default()),
        results: ResultStore::new(Arc::clone(store), &spec.bot, &id, &spec.context_dir),
        conv: tokio::sync::Mutex::new(Conv {
            messages,
            context,
            context_dirty,
            next_turn,
            last_call: None,
            excluded: HashSet::new(),
        }),
        resume: Mutex::new(resume),
        stopping: AtomicBool::new(false),
        shared,
    });
    Ok(Started {
        session: Box::new(LoopSession { core, turn: None }),
        events: events_rx,
        resumed_turn,
    })
}

/// The session's browser tool and its tools as the model sees them.
async fn open_browser(
    shared: &Shared,
    spec: &SessionSpec,
) -> (Option<Arc<dyn BrowserTools>>, Vec<ToolSpec>) {
    let setup = &shared.setup;
    let browser = match (&spec.browser, &setup.browser) {
        (Some(b), Some(factory)) => Some(factory.open(b, &spec.env)),
        _ => None,
    };
    let specs = match &browser {
        Some(tools) => match tools.specs().await {
            Ok(list) => model_specs(list, &setup.browser_allow),
            Err(err) => {
                tracing::warn!(bot = %spec.bot, %err, "the browser tool couldn't list its tools");
                Vec::new()
            }
        },
        None => Vec::new(),
    };
    (browser, specs)
}

/// A running loop session.
pub(crate) struct LoopSession {
    core: Arc<Core>,
    turn: Option<(CancellationToken, JoinHandle<()>)>,
}

impl Session for LoopSession {
    fn id(&self) -> &str {
        &self.core.id
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
            let task = tokio::spawn(turn::run_turn(Arc::clone(&self.core), turn, cancel.clone()));
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
        self.core.stopping.store(true, Ordering::Relaxed);
        let turn = self.turn.take();
        let core = Arc::clone(&self.core);
        Box::pin(async move {
            if let Some((cancel, task)) = turn {
                cancel.cancel();
                if tokio::time::timeout(GRACE, task).await.is_err() {
                    tracing::warn!(bot = %core.bot, "a loop turn didn't stop in time");
                }
            }
            if let Some(browser) = &core.browser {
                browser.shutdown().await;
            }
        })
    }
}

impl Drop for LoopSession {
    fn drop(&mut self) {
        let was_stopping = self.core.stopping.swap(true, Ordering::Relaxed);
        if let Some((cancel, _)) = self.turn.take() {
            cancel.cancel();
        }
        // Dropped without `shutdown`: stop the browser tool too.
        if !was_stopping
            && let Some(browser) = self.core.browser.clone()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            rt.spawn(async move { browser.shutdown().await });
        }
    }
}
