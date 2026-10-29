//! [`Hub`]: runs the bots.
//!
//! It keeps one `claude` child per bot that has been talked to, and one
//! Chrome that every bot shares. The shared Chrome keeps the user's
//! sign-ins, so bots take turns on it: a bot holds the browser from its
//! first browser action until its reply ends, and another bot that needs it
//! meanwhile waits.
//!
//! The hub turns Claude's output into stored [`EventKind`]s, answers
//! Claude's permission requests by the rules in [`crate::policy`] (asking
//! the user with an [`Approval`] card before a change or an upload), serves
//! oxbot's own `request_sign_in` tool (a [`SignInRequest`] card and a
//! visible Chrome window), keeps Claude's usage and the people who send
//! messages, and broadcasts every change to connected clients as
//! [`ServerMsg`]s.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use oxbot_browser::{
    Browser, BrowserError, BrowserSpec, BrowserStatus, MCP_SERVER_NAME, Tools, site_of, web_host,
};
use oxbot_claude::{
    Activity, ClaudeConfig, ClaudeError, ClaudeEvent, ClaudeProcess, Decision, PermissionRequest,
    Session,
};
use oxbot_protocol::{
    Answer, Approval, ApprovalKind, BotEvent, BotInfo, BrowserState, EventKind, Outcome, Person,
    ServerMsg, SignInOutcome, SignInRequest, SiteInfo, Usage,
};
use serde_json::{Value, json};
use tokio::sync::{
    Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot,
};

use crate::bot::{Bot, BotError};
use crate::id::{BotId, BotIdError};
use crate::policy::{self, PageFacts, Verdict};
use crate::prompt::{Capabilities, system_prompt};
use crate::store::{SavedSession, Store, StoreError};
use crate::tools::{self, SIGN_IN_TOOL, TOOL_SERVER_NAME};
use std::fmt::Write as _;

/// Settings for the [`Hub`].
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// The `claude` program.
    pub claude_program: PathBuf,
    /// Where bot folders live; new bots are created here.
    pub bots_dir: PathBuf,
    /// Most turns that may run at the same time, across all bots.
    pub max_concurrent_turns: usize,
    /// Chrome and the browser tool, if bots may browse.
    pub browser: Option<Arc<Tools>>,
    /// How long a bot waits for the user to answer an approval card.
    pub approval_timeout: Duration,
    /// How long a bot waits for the user to sign in.
    pub sign_in_timeout: Duration,
    /// How long the Assistant waits for a bot to finish a task it gave.
    pub task_timeout: Duration,
    /// Bots idle this long are stopped (and the browser, once nobody has
    /// used it this long); they resume on the next message.
    pub idle_stop: Duration,
    /// Extra environment for every `claude` child (tests use this; the app
    /// passes a `PATH` that finds Node.js).
    pub extra_env: Vec<(String, String)>,
}

impl HubConfig {
    /// Defaults: `claude` from `PATH`, 3 turns at once, no browser, 10
    /// minutes to answer a card or sign in, idle bots stop after 15 minutes.
    #[must_use]
    pub fn new(bots_dir: impl Into<PathBuf>) -> Self {
        Self {
            claude_program: PathBuf::from("claude"),
            bots_dir: bots_dir.into(),
            max_concurrent_turns: 3,
            browser: None,
            approval_timeout: Duration::from_mins(10),
            sign_in_timeout: Duration::from_mins(10),
            task_timeout: Duration::from_mins(10),
            idle_stop: Duration::from_mins(15),
            extra_env: Vec::new(),
        }
    }
}

/// Errors from the [`Hub`].
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    /// No bot has that id.
    #[error("no bot called {0:?}")]
    UnknownBot(String),
    /// The bot is still answering the previous message.
    #[error("{0} is still working on the last message")]
    Busy(BotId),
    /// A bot is using the browser right now.
    #[error("{0} is using the browser right now; try again when it's done")]
    BrowserBusy(String),
    /// This server gives bots no browser.
    #[error("this oxbot has no browser")]
    NoBrowser,
    /// A new bot's id isn't valid.
    #[error("bad bot id: {0}")]
    BadId(#[from] BotIdError),
    /// A bot couldn't be created or saved.
    #[error(transparent)]
    Bot(#[from] BotError),
    /// The browser failed.
    #[error(transparent)]
    Browser(#[from] BrowserError),
    /// The history database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Claude couldn't be started or stopped responding.
    #[error(transparent)]
    Claude(#[from] ClaudeError),
}

/// A bot's running `claude` child, and what oxbot knows about its turn.
struct Runtime {
    process: ClaudeProcess,
    busy: bool,
    /// Held while a turn runs, so at most `max_concurrent_turns` run at once.
    permit: Option<OwnedSemaphorePermit>,
    /// Held while this bot uses the shared browser (until its turn ends).
    lease: Option<OwnedMutexGuard<()>>,
    last_active: Instant,
    /// Tool call id → tool name, to read tool results.
    tool_names: HashMap<String, String>,
    /// The page the browser tool says is selected: (title, address).
    page: Option<(String, String)>,
    /// Element uid → words, from the latest page snapshot.
    elements: HashMap<String, String>,
}

/// Cards waiting for the user, with who to tell.
type Waiting<T, A> = std::sync::Mutex<BTreeMap<u64, (T, oneshot::Sender<A>)>>;

/// The shared browser and who's using it.
struct Shared {
    browser: Arc<Browser>,
    /// Held by the bot using the browser.
    lease: Arc<Mutex<()>>,
    /// The bot holding the lease.
    holder: std::sync::Mutex<Option<BotId>>,
    /// The bot that used the browser last (its tabs are still open).
    last: std::sync::Mutex<Option<BotId>>,
    /// When a bot last used it.
    last_used: std::sync::Mutex<Instant>,
}

/// Runs the bots. Share it as `Arc<Hub>`.
pub struct Hub {
    bots: RwLock<BTreeMap<BotId, Bot>>,
    store: Store,
    config: HubConfig,
    runtimes: Mutex<HashMap<BotId, Runtime>>,
    shared: Option<Shared>,
    approvals: Waiting<Approval, Answer>,
    sign_ins: Waiting<SignInRequest, bool>,
    next_id: AtomicU64,
    /// Bots whose `claude` is being started (which can take a while the first
    /// time, while the browser tool installs).
    starting: std::sync::Mutex<HashSet<BotId>>,
    turns: Arc<Semaphore>,
    updates: broadcast::Sender<ServerMsg>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Hub {
    /// A hub for `bots`, storing history in `store`. Must be called inside a
    /// Tokio runtime (it starts the idle-stop timer).
    #[must_use]
    pub fn new(bots: Vec<Bot>, store: Store, config: HubConfig) -> Arc<Self> {
        let (updates, _) = broadcast::channel(1024);
        let shared = config.browser.clone().and_then(|tools| {
            let spec = BrowserSpec {
                label: "shared".to_owned(),
                profile_dir: tools.config().profile_dir.clone(),
                downloads: config.bots_dir.clone(),
            };
            match Browser::new(tools, spec) {
                Ok(browser) => Some(Shared {
                    browser,
                    lease: Arc::new(Mutex::new(())),
                    holder: std::sync::Mutex::new(None),
                    last: std::sync::Mutex::new(None),
                    last_used: std::sync::Mutex::new(Instant::now()),
                }),
                Err(err) => {
                    tracing::error!(%err, "no local port for Chrome; bots get no browser");
                    None
                }
            }
        });
        let hub = Arc::new(Self {
            bots: RwLock::new(bots.into_iter().map(|b| (b.id.clone(), b)).collect()),
            store,
            turns: Arc::new(Semaphore::new(config.max_concurrent_turns)),
            config,
            runtimes: Mutex::new(HashMap::new()),
            shared,
            starting: std::sync::Mutex::new(HashSet::new()),
            approvals: std::sync::Mutex::new(BTreeMap::new()),
            sign_ins: std::sync::Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            updates,
        });
        if let Some(shared) = &hub.shared {
            tokio::spawn(watch_browser(
                Arc::downgrade(&hub),
                Arc::clone(&shared.browser),
            ));
        }
        tokio::spawn(stop_idle_bots(Arc::downgrade(&hub)));
        hub
    }

    /// Live updates: every stored event, reply delta, card, browser change,
    /// usage change, new person and bot-list change, for all bots.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<ServerMsg> {
        self.updates.subscribe()
    }

    /// Sends `msg` to every connected client, e.g. setup progress.
    pub fn announce(&self, msg: ServerMsg) {
        let _ = self.updates.send(msg);
    }

    /// Looks up a bot by id.
    ///
    /// # Errors
    /// [`HubError::UnknownBot`] if there's no such bot.
    pub fn bot(&self, id: &str) -> Result<Bot, HubError> {
        let bots = self.bots.read().unwrap_or_else(PoisonError::into_inner);
        BotId::new(id)
            .ok()
            .and_then(|id| bots.get(&id).cloned())
            .ok_or_else(|| HubError::UnknownBot(id.to_owned()))
    }

    fn bot_name(&self, id: &BotId) -> String {
        self.bot(id.as_str())
            .map_or_else(|_| id.to_string(), |b| b.name)
    }

    /// Claude's usage: the plan's limits and every bot's totals.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub fn usage(&self) -> Result<Usage, HubError> {
        Ok(self.store.usage()?)
    }

    fn announce_usage(&self) {
        match self.usage() {
            Ok(usage) => {
                let _ = self.updates.send(ServerMsg::Usage(usage));
            }
            Err(err) => tracing::warn!(%err, "couldn't read usage"),
        }
    }

    /// Everyone who has sent a message, each with their own color.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub fn people(&self) -> Result<Vec<Person>, HubError> {
        Ok(self.store.people()?)
    }

    /// What the shared browser is doing.
    #[must_use]
    pub fn browser_state(&self) -> BrowserState {
        match &self.shared {
            None => BrowserState::Disabled,
            Some(s) => browser_state(*s.browser.status().borrow()),
        }
    }

    /// Every bot, with whether it's busy, its latest event number and the
    /// browser's state.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub async fn bots(&self) -> Result<Vec<BotInfo>, HubError> {
        let list: Vec<Bot> = self
            .bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        let busy: HashSet<BotId> = self
            .runtimes
            .lock()
            .await
            .iter()
            .filter(|(_, r)| r.busy)
            .map(|(id, _)| id.clone())
            .collect();
        let browser_state = self.browser_state();
        list.into_iter()
            .map(|b| {
                Ok(BotInfo {
                    busy: busy.contains(&b.id),
                    last_seq: self.store.last_seq(&b.id)?,
                    last_reply_seq: self.store.last_reply_seq(&b.id)?,
                    central: b.is_central(),
                    id: b.id.to_string(),
                    name: b.name,
                    browser_state,
                })
            })
            .collect()
    }

    async fn announce_bots(&self) {
        if let Ok(bots) = self.bots().await {
            let _ = self.updates.send(ServerMsg::Bots { bots });
        }
    }

    /// A bot's stored events after `since`.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist or the database fails.
    pub fn history(&self, bot: &str, since: u64) -> Result<Vec<BotEvent>, HubError> {
        let bot = self.bot(bot)?;
        Ok(self.store.history(&bot.id, since)?)
    }

    /// Creates a bot, picking its id from `name`, and tells every client.
    ///
    /// # Errors
    /// Fails if its files can't be written.
    pub async fn create_bot(&self, name: &str, instructions: &str) -> Result<BotInfo, HubError> {
        let name = if name.trim().is_empty() {
            "New bot".to_owned()
        } else {
            name.trim().to_owned()
        };
        let id = {
            let bots = self.bots.read().unwrap_or_else(PoisonError::into_inner);
            BotId::from_name(&name, |id| {
                bots.contains_key(id) || self.config.bots_dir.join(id.as_str()).exists()
            })
        };
        let bot = Bot::create(
            &self.config.bots_dir,
            id.clone(),
            &name,
            instructions.trim(),
        )?;
        self.bots
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(bot.id.clone(), bot);
        self.announce_bots().await;
        self.bots()
            .await?
            .into_iter()
            .find(|b| b.id == id.as_str())
            .ok_or_else(|| HubError::UnknownBot(id.to_string()))
    }

    // ------------------------------------------------------------ the shared browser

    /// Gives `bot` the shared browser for the rest of its turn, waiting if
    /// another bot has it. Returns `false` if the bot's turn ended while it
    /// waited.
    async fn take_browser(&self, bot: &BotId) -> Result<bool, HubError> {
        let Some(shared) = &self.shared else {
            return Ok(true);
        };
        if self
            .runtimes
            .lock()
            .await
            .get(bot)
            .is_some_and(|r| r.lease.is_some())
        {
            *lock(&shared.last_used) = Instant::now();
            return self.browser_running(shared).await.map(|()| true);
        }
        let guard = if let Ok(guard) = Arc::clone(&shared.lease).try_lock_owned() {
            guard
        } else {
            let holder = lock(&shared.holder)
                .clone()
                .map_or_else(|| "another bot".to_owned(), |id| self.bot_name(&id));
            let _ = self.updates.send(ServerMsg::Activity {
                bot: bot.to_string(),
                text: format!("Waiting for the browser ({holder} is using it)…"),
            });
            Arc::clone(&shared.lease).lock_owned().await
        };
        {
            let mut runtimes = self.runtimes.lock().await;
            let Some(runtime) = runtimes.get_mut(bot).filter(|r| r.busy) else {
                return Ok(false);
            };
            runtime.lease = Some(guard);
            let previous = lock(&shared.last).replace(bot.clone());
            // Another bot used the browser since: what this bot read is stale.
            if previous.as_ref().is_some_and(|p| p != bot) {
                runtime.page = None;
                runtime.elements.clear();
            }
            *lock(&shared.holder) = Some(bot.clone());
            *lock(&shared.last_used) = Instant::now();
            if previous.as_ref() == Some(bot) {
                drop(runtimes);
                self.browser_running(shared).await?;
                return self.downloads_for(shared, bot).await.map(|()| true);
            }
        }
        self.browser_running(shared).await?;
        shared.browser.reset_tabs().await;
        self.downloads_for(shared, bot).await?;
        tracing::info!(%bot, "has the browser");
        Ok(true)
    }

    /// Starts Chrome if it isn't running. A failure is only logged: the
    /// browser tool then tells the bot it can't connect.
    async fn browser_running(&self, shared: &Shared) -> Result<(), HubError> {
        if *shared.browser.status().borrow() == BrowserStatus::Off
            && let Err(err) = shared.browser.ensure_running().await
        {
            tracing::warn!(%err, "couldn't start Chrome");
        }
        Ok(())
    }

    async fn downloads_for(&self, shared: &Shared, bot: &BotId) -> Result<(), HubError> {
        let dir = self.bot(bot.as_str())?.work_dir().join("downloads");
        shared.browser.set_downloads(&dir).await;
        Ok(())
    }

    /// Lets go of the browser if `runtime` holds it.
    fn release_browser(&self, bot: &BotId, runtime: &mut Runtime) {
        if runtime.lease.take().is_some()
            && let Some(shared) = &self.shared
        {
            let mut holder = lock(&shared.holder);
            if holder.as_ref() == Some(bot) {
                *holder = None;
            }
            *lock(&shared.last_used) = Instant::now();
        }
    }

    /// Signs the shared browser out of every site by deleting its profile.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] if a bot is using it, or the profile can't
    /// be deleted.
    pub async fn sign_out_all(&self) -> Result<(), HubError> {
        let shared = self.shared.as_ref().ok_or(HubError::NoBrowser)?;
        let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned() else {
            let holder = lock(&shared.holder)
                .clone()
                .map_or_else(|| "A bot".to_owned(), |id| self.bot_name(&id));
            return Err(HubError::BrowserBusy(holder));
        };
        shared.browser.sign_out().await?;
        lock(&shared.last).take();
        self.notice(
            "Signed out of every site. Bots will ask you to sign in again where they need to.",
        );
        Ok(())
    }

    // ------------------------------------------------------------ sites

    /// The sites the user signed in to through oxbot, newest first.
    #[must_use]
    pub fn sites(&self) -> Vec<SiteInfo> {
        self.store.sites().unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read the signed-in sites");
            Vec::new()
        })
    }

    /// Checks the list against the browser's cookies, drops sites it's no
    /// longer signed in to (say, the window closed before signing in), and
    /// tells every client.
    async fn refresh_sites(&self) {
        if let Some(shared) = &self.shared {
            match shared.browser.cookie_sites().await {
                Ok(held) => {
                    for site in self.sites() {
                        if !held.contains(&site.site) {
                            tracing::info!(
                                site = site.site,
                                "no sign-in cookies left; dropping it"
                            );
                            let _ = self.store.remove_site(&site.site);
                        }
                    }
                }
                Err(err) => tracing::debug!(%err, "couldn't check the browser's cookies"),
            }
        }
        self.announce(ServerMsg::Sites {
            sites: self.sites(),
        });
    }

    /// Shows `site` in the visible Chrome window, e.g. to check an account.
    /// Closing the window goes back to hidden Chrome.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] while a bot is using the browser.
    pub async fn open_site(&self, site: &str) -> Result<(), HubError> {
        let shared = self.shared.as_ref().ok_or(HubError::NoBrowser)?;
        if let Some(holder) = lock(&shared.holder).clone() {
            return Err(HubError::BrowserBusy(self.bot_name(&holder)));
        }
        shared
            .browser
            .open_window(&format!("https://{site}"))
            .await?;
        Ok(())
    }

    /// Signs the shared browser out of `site` only.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] while a bot is using the browser, or the
    /// browser's error.
    pub async fn sign_out_site(&self, site: &str) -> Result<(), HubError> {
        let shared = self.shared.as_ref().ok_or(HubError::NoBrowser)?;
        let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned() else {
            let holder = lock(&shared.holder)
                .clone()
                .map_or_else(|| "A bot".to_owned(), |id| self.bot_name(&id));
            return Err(HubError::BrowserBusy(holder));
        };
        shared.browser.forget_site(site).await?;
        if let Err(err) = self.store.remove_site(site) {
            tracing::warn!(%err, "couldn't forget the site");
        }
        lock(&shared.last).take();
        self.announce(ServerMsg::Sites {
            sites: self.sites(),
        });
        self.notice(&format!(
            "Signed out of {site}. Bots will ask you to sign in again if they need it."
        ));
        Ok(())
    }

    // ------------------------------------------------------------ sign-in

    /// A bot asks the user to sign in at `url`. Shows every client a card and
    /// waits until the user is done, gives up, or doesn't answer in time.
    /// Returns whether they signed in, and what to tell the bot.
    pub async fn request_sign_in(&self, bot: &BotId, url: &str, why: &str) -> (bool, String) {
        if self.shared.is_none() {
            return (false, "This oxbot has no browser.".to_owned());
        }
        let Some(site) = web_host(url) else {
            return (
                false,
                "request_sign_in needs a web address (https://…).".to_owned(),
            );
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = SignInRequest {
            id,
            bot: bot.to_string(),
            url: url.to_owned(),
            site: site.clone(),
            why: why.trim().to_owned(),
            window_open: false,
        };
        let (tx, rx) = oneshot::channel();
        lock(&self.sign_ins).insert(id, (request.clone(), tx));
        let _ = self.updates.send(ServerMsg::SignInNeeded(request));
        let _ = self.updates.send(ServerMsg::Activity {
            bot: bot.to_string(),
            text: format!("Waiting for you to sign in to {site}…"),
        });
        tracing::info!(%bot, site, "asking the user to sign in");
        // Open the sign-in window right away; the card's button is the fallback.
        if let Err(err) = self.open_sign_in(id).await {
            tracing::warn!(%bot, site, %err, "couldn't open the sign-in window");
        }

        let minutes = self.config.sign_in_timeout.as_secs() / 60;
        let answer = tokio::time::timeout(self.config.sign_in_timeout, rx).await;
        let (outcome, result) = match answer {
            Ok(Ok(true)) => (
                SignInOutcome::Done,
                (
                    true,
                    format!(
                        "The user finished with the sign-in window for {site} (they pressed I'm \
                         signed in, or closed the window). Chrome restarted, so list or open pages \
                         again, then check you're really signed in. If you're still signed out, say \
                         so plainly and don't ask to sign in again until the user asks you to."
                    ),
                ),
            ),
            Ok(Ok(false) | Err(_)) => (
                SignInOutcome::Cancelled,
                (
                    false,
                    "The sign-in was cancelled: the user pressed Cancel, or oxbot restarted. Tell \
                     them you stopped because the site needs signing in, and that they can ask you \
                     to try again."
                        .to_owned(),
                ),
            ),
            Err(_) => {
                self.resolve_sign_in(id, false);
                (
                    SignInOutcome::TimedOut,
                    (
                        false,
                        format!(
                            "The user didn't sign in within {minutes} minutes. Tell them you stopped."
                        ),
                    ),
                )
            }
        };
        if let Some(shared) = &self.shared {
            shared.browser.close_window().await;
        }
        if outcome == SignInOutcome::Done {
            if let Err(err) = self.store.add_site(&site_of(&site)) {
                tracing::warn!(%err, "couldn't remember the site");
            }
            self.refresh_sites().await;
        }
        if let Err(err) = self.record(bot, EventKind::SignIn { site, outcome }) {
            tracing::warn!(%err, "couldn't record the sign-in");
        }
        result
    }

    /// The user opened the sign-in window for request `id`.
    ///
    /// # Errors
    /// Fails if there's no such request or Chrome can't start.
    pub async fn open_sign_in(&self, id: u64) -> Result<(), HubError> {
        let shared = self.shared.as_ref().ok_or(HubError::NoBrowser)?;
        let request = lock(&self.sign_ins).get(&id).map(|(r, _)| r.clone());
        let Some(mut request) = request else {
            return Err(HubError::UnknownBot(format!("sign-in {id}")));
        };
        shared.browser.open_window(&request.url).await?;
        request.window_open = true;
        if let Some(entry) = lock(&self.sign_ins).get_mut(&id) {
            entry.0.window_open = true;
        }
        let _ = self.updates.send(ServerMsg::SignInNeeded(request));
        Ok(())
    }

    /// The user finished (`true`) or gave up on (`false`) a sign-in. Returns
    /// whether it was still open.
    pub fn resolve_sign_in(&self, id: u64, signed_in: bool) -> bool {
        let entry = lock(&self.sign_ins).remove(&id);
        let Some((_, tx)) = entry else { return false };
        let _ = tx.send(signed_in);
        let _ = self.updates.send(ServerMsg::SignInResolved { id });
        true
    }

    /// Sign-in requests still waiting for the user.
    #[must_use]
    pub fn pending_sign_ins(&self) -> Vec<SignInRequest> {
        lock(&self.sign_ins)
            .values()
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// The sign-in window closed: requests whose window was open count as
    /// done (the user closes it when they've signed in).
    fn sign_in_window_closed(&self) {
        let ids: Vec<u64> = lock(&self.sign_ins)
            .iter()
            .filter(|(_, (r, _))| r.window_open)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.resolve_sign_in(id, true);
        }
    }

    fn cancel_sign_ins(&self, bot: &BotId) {
        let ids: Vec<u64> = lock(&self.sign_ins)
            .iter()
            .filter(|(_, (r, _))| r.bot == bot.as_str())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.resolve_sign_in(id, false);
        }
    }

    /// Answers a message for oxbot's own tool server.
    async fn tool_message(self: Arc<Self>, bot: BotId, request_id: String, message: Value) {
        let central = self.bot(bot.as_str()).is_ok_and(|b| b.is_central());
        let response = tools::handle(&message, central, |tool, args| {
            let hub = Arc::clone(&self);
            let bot = bot.clone();
            hub.run_tool(bot, tool, args)
        })
        .await;
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(&bot)
            && let Err(err) = runtime.process.answer_mcp(&request_id, response).await
        {
            tracing::warn!(%bot, %err, "couldn't answer Claude's tool call");
        }
    }

    /// Runs one of oxbot's tools for `bot` (`tools::handle` has already
    /// checked the bot may use it). Returns whether it worked and what to
    /// tell the bot.
    ///
    /// Boxed with a named type: giving a task can start another bot, whose
    /// tool calls come back here, and the compiler needs that loop broken.
    fn run_tool(
        self: Arc<Self>,
        bot: BotId,
        tool: String,
        args: Value,
    ) -> std::pin::Pin<Box<dyn Future<Output = (bool, String)> + Send>> {
        Box::pin(async move { self.run_tool_inner(&bot, &tool, &args).await })
    }

    async fn run_tool_inner(
        self: &Arc<Self>,
        bot: &BotId,
        tool: &str,
        args: &Value,
    ) -> (bool, String) {
        let text = |key: &str| tools::text_arg(args, key).trim().to_owned();
        match tool {
            tools::SIGN_IN_TOOL => self.request_sign_in(bot, &text("url"), &text("why")).await,
            tools::LIST_BOTS_TOOL => (true, self.team().await),
            tools::CREATE_BOT_TOOL => {
                // The user already said yes on the card (see `policy`).
                match self.create_bot(&text("name"), &text("instructions")).await {
                    Ok(info) => (
                        true,
                        format!(
                            "Created \"{}\" (id {}). It's in the user's sidebar. Give it tasks with give_task.",
                            info.name, info.id
                        ),
                    ),
                    Err(err) => (false, format!("Couldn't create the bot: {err}")),
                }
            }
            tools::GIVE_TASK_TOOL => {
                let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(true);
                self.give_task(bot, &text("bot"), &text("message"), wait)
                    .await
            }
            tools::READ_REPLY_TOOL => self.read_reply(&text("bot")).await,
            _ => (false, "unknown tool".to_owned()),
        }
    }

    // ------------------------------------------------------------ the team

    /// Every bot, for the Assistant: id, name, what it's for, and status.
    async fn team(&self) -> String {
        let busy: HashSet<BotId> = self
            .runtimes
            .lock()
            .await
            .iter()
            .filter(|(_, r)| r.busy)
            .map(|(id, _)| id.clone())
            .collect();
        let mut bots: Vec<Bot> = self
            .bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        bots.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = String::new();
        for b in bots {
            let purpose: String = b
                .instructions
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(160)
                .collect();
            let status = if busy.contains(&b.id) {
                "working"
            } else {
                "free"
            };
            let you = if b.is_central() { " (you)" } else { "" };
            let _ = writeln!(
                out,
                "- {} (id {}){you}: {status}. {}",
                b.name,
                b.id,
                if purpose.is_empty() {
                    "No instructions."
                } else {
                    &purpose
                }
            );
        }
        out
    }

    /// Sends `target` a task from the Assistant (`from`) and, if `wait`,
    /// waits for its reply.
    async fn give_task(
        self: &Arc<Self>,
        from: &BotId,
        target: &str,
        message: &str,
        wait: bool,
    ) -> (bool, String) {
        let Ok(to) = self.bot(target) else {
            return (
                false,
                format!("There's no bot with id {target}. Use list_bots."),
            );
        };
        if &to.id == from {
            return (false, "That's you. Do the task yourself.".to_owned());
        }
        // Give up the browser first, so the other bot can use it.
        if let Some(runtime) = self.runtimes.lock().await.get_mut(from) {
            self.release_browser(from, runtime);
        }
        let mut updates = self.subscribe();
        let start = self.store.last_seq(&to.id).unwrap_or(0);
        let sender = self.bot_name(from);
        match self.send(to.id.as_str(), message, Some(&sender)).await {
            Ok(()) => {}
            Err(HubError::Busy(_)) => {
                return (
                    false,
                    format!(
                        "{} is busy with something else. Try again later, or read_reply.",
                        to.name
                    ),
                );
            }
            Err(err) => return (false, format!("Couldn't send the task: {err}")),
        }
        tracing::info!(from = %from, to = %to.id, "gave a task");
        if !wait {
            return (
                true,
                format!("Sent to {}. Use read_reply for its answer.", to.name),
            );
        }
        let finished = tokio::time::timeout(self.config.task_timeout, async {
            loop {
                match updates.recv().await {
                    Ok(ServerMsg::Event(e)) if e.bot == to.id.as_str() => {
                        if let EventKind::TurnDone { ok, .. } = e.kind {
                            return ok;
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return false,
                }
            }
        })
        .await;
        let reply = self.latest_reply(&to.id, start);
        match finished {
            Err(_) => (
                true,
                format!(
                    "{} is still working after {} minutes. Use read_reply later.",
                    to.name,
                    self.config.task_timeout.as_secs() / 60
                ),
            ),
            Ok(ok) => match reply {
                Some(text) => (true, format!("{} replied:\n{text}", to.name)),
                None if ok => (true, format!("{} finished without a reply.", to.name)),
                None => (false, format!("{} stopped without finishing.", to.name)),
            },
        }
    }

    /// A bot's latest reply and whether it's still working.
    async fn read_reply(&self, target: &str) -> (bool, String) {
        let Ok(bot) = self.bot(target) else {
            return (
                false,
                format!("There's no bot with id {target}. Use list_bots."),
            );
        };
        let busy = self
            .runtimes
            .lock()
            .await
            .get(&bot.id)
            .is_some_and(|r| r.busy);
        let status = if busy {
            "still working"
        } else {
            "not working right now"
        };
        match self.latest_reply(&bot.id, 0) {
            Some(text) => (
                true,
                format!("{} ({status}). Latest reply:\n{text}", bot.name),
            ),
            None => (true, format!("{} ({status}) hasn't replied yet.", bot.name)),
        }
    }

    /// The text of `bot`'s latest reply after event `since`.
    fn latest_reply(&self, bot: &BotId, since: u64) -> Option<String> {
        self.store
            .history(bot, since)
            .ok()?
            .into_iter()
            .rev()
            .find_map(|e| match e.kind {
                EventKind::AssistantMessage { text } => Some(text),
                _ => None,
            })
    }

    // ------------------------------------------------------------ approvals

    /// The user answered an approval card. Returns whether it was still open.
    pub fn answer(&self, id: u64, answer: Answer) -> bool {
        let entry = lock(&self.approvals).remove(&id);
        let Some((_, tx)) = entry else { return false };
        let _ = tx.send(answer);
        let _ = self.updates.send(ServerMsg::ApprovalResolved { id });
        true
    }

    /// Approval cards still waiting for the user.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<Approval> {
        lock(&self.approvals)
            .values()
            .map(|(a, _)| a.clone())
            .collect()
    }

    /// Denies every open card for `bot` (its turn ended or was cancelled).
    fn deny_open_approvals(&self, bot: &BotId) {
        let ids: Vec<u64> = lock(&self.approvals)
            .iter()
            .filter(|(_, (a, _))| a.bot == bot.as_str())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.answer(id, Answer::Deny);
        }
    }

    /// Claude asks whether `bot` may run a tool: decide, maybe ask the user,
    /// then answer Claude.
    async fn permission(self: Arc<Self>, bot: BotId, request: PermissionRequest) {
        let decision = match self.decide_permission(&bot, &request).await {
            Ok(decision) => decision,
            Err(err) => Decision::Deny(format!("oxbot couldn't check this: {err}")),
        };
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(&bot)
            && let Err(err) = runtime
                .process
                .answer(&request.request_id, &request.input, &decision)
                .await
        {
            tracing::warn!(%bot, %err, "couldn't answer Claude's permission request");
        }
    }

    async fn decide_permission(
        self: &Arc<Self>,
        bot_id: &BotId,
        request: &PermissionRequest,
    ) -> Result<Decision, HubError> {
        let bot = self.bot(bot_id.as_str())?;
        let uses_browser = request
            .tool
            .starts_with(&format!("mcp__{MCP_SERVER_NAME}__"))
            || request.tool == format!("mcp__{TOOL_SERVER_NAME}__{SIGN_IN_TOOL}");
        if uses_browser && !self.take_browser(bot_id).await? {
            return Ok(Decision::Deny(
                "Your turn ended while you waited for the browser.".to_owned(),
            ));
        }

        let (page, elements) = {
            let runtimes = self.runtimes.lock().await;
            let r = runtimes.get(bot_id);
            (
                r.and_then(|r| r.page.clone()),
                r.map(|r| r.elements.clone()).unwrap_or_default(),
            )
        };
        let page_url = page.as_ref().map(|(_, url)| url.as_str());
        let facts = match &self.shared {
            Some(shared) if policy::needs_facts(&request.tool) => shared
                .browser
                .inspect(page_url, &policy::facts_script(&request.input))
                .await
                .map(|v| PageFacts::from_value(&v)),
            _ => None,
        };
        let ctx = policy::Context {
            work_dir: &bot.work_dir(),
            page_url,
            facts: facts.as_ref(),
        };
        let verdict = policy::decide(&request.tool, &request.input, ctx, |uid| {
            elements.get(uid).cloned()
        });
        match verdict {
            Verdict::Allow => Ok(Decision::Allow),
            Verdict::Deny(message) => {
                self.record(
                    bot_id,
                    EventKind::Blocked {
                        message: format!("oxbot refused: {message}"),
                    },
                )?;
                Ok(Decision::Deny(message))
            }
            Verdict::Ask(ask) => self.ask_user(&bot, ask, page).await,
        }
    }

    /// Shows the user a card for `ask` and turns their answer into Claude's.
    async fn ask_user(
        &self,
        bot: &Bot,
        ask: policy::Ask,
        page: Option<(String, String)>,
    ) -> Result<Decision, HubError> {
        let bot_id = &bot.id;
        let screenshot = match &self.shared {
            // A new bot has nothing to do with the page on screen.
            _ if ask.kind == ApprovalKind::CreateBot => None,
            Some(s) => {
                s.browser
                    .screenshot(page.as_ref().map(|(_, u)| u.as_str()))
                    .await
            }
            None => None,
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let approval = Approval {
            id,
            bot: bot_id.to_string(),
            kind: ask.kind,
            action: ask.action.clone(),
            target: ask.target.clone(),
            site: ask.site.clone(),
            page: page.map(|(title, url)| if title.is_empty() { url } else { title }),
            detail: ask.detail.clone(),
            screenshot,
        };
        let (tx, rx) = oneshot::channel();
        lock(&self.approvals).insert(id, (approval.clone(), tx));
        let _ = self.updates.send(ServerMsg::ApprovalNeeded(approval));
        let _ = self.updates.send(ServerMsg::Activity {
            bot: bot_id.to_string(),
            text: "Waiting for your OK…".to_owned(),
        });
        tracing::info!(bot = %bot_id, action = ask.action, site = ask.site, "asking the user");

        let answer = match tokio::time::timeout(self.config.approval_timeout, rx).await {
            Ok(Ok(answer)) => Some(answer),
            Ok(Err(_)) => Some(Answer::Deny),
            Err(_) => {
                self.answer(id, Answer::Deny);
                None
            }
        };
        let (outcome, decision) = match answer {
            None => (
                Outcome::TimedOut,
                Decision::Deny(format!(
                    "The user didn't answer within {} minutes, so oxbot said no. Tell them what you \
                     wanted to do, and stop.",
                    self.config.approval_timeout.as_secs() / 60
                )),
            ),
            Some(Answer::Deny) => (
                Outcome::Denied,
                Decision::Deny(format!(
                    "The user said no to: {} {}. Don't try another way; tell them what you wanted to \
                     do, and stop.",
                    ask.action, ask.target
                )),
            ),
            Some(Answer::AllowOnce) => (Outcome::Allowed, Decision::Allow),
        };
        self.record(
            bot_id,
            EventKind::Approval {
                action: ask.action,
                target: ask.target,
                site: ask.site,
                outcome,
            },
        )?;
        Ok(decision)
    }

    // ------------------------------------------------------------ turns

    /// Sends a message to a bot from `from` (a person's name, if known).
    /// Returns once Claude has it; the reply arrives through
    /// [`Hub::subscribe`]. Waits if `max_concurrent_turns` turns are already
    /// running.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist, is busy, or Claude can't be started.
    pub async fn send(
        self: &Arc<Self>,
        bot: &str,
        text: &str,
        from: Option<&str>,
    ) -> Result<(), HubError> {
        let bot = self.bot(bot)?;
        let from = from.map(str::trim).filter(|f| !f.is_empty());
        if let Some(name) = from {
            let (_, new) = self.store.person(name)?;
            if new {
                let _ = self.updates.send(ServerMsg::People {
                    people: self.people()?,
                });
            }
        }
        let permit = Arc::clone(&self.turns)
            .acquire_owned()
            .await
            .map_err(|_| HubError::Busy(bot.id.clone()))?;

        // Claim the bot, so a second message can't slip in while Claude starts.
        {
            let runtimes = self.runtimes.lock().await;
            let mut starting = lock(&self.starting);
            if runtimes.get(&bot.id).is_some_and(|r| r.busy) || starting.contains(&bot.id) {
                return Err(HubError::Busy(bot.id));
            }
            starting.insert(bot.id.clone());
        }
        let result = self.deliver(&bot, text, from, permit).await;
        lock(&self.starting).remove(&bot.id);
        result
    }

    async fn deliver(
        self: &Arc<Self>,
        bot: &Bot,
        text: &str,
        from: Option<&str>,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), HubError> {
        self.record(
            &bot.id,
            EventKind::UserMessage {
                text: text.to_owned(),
                from: from.map(str::to_owned),
            },
        )?;

        let running = self.runtimes.lock().await.contains_key(&bot.id);
        if !running {
            // Started outside the lock: the first start can take a while.
            let runtime = match self.start_claude(bot).await {
                Ok(r) => r,
                Err(err) => {
                    self.turn_failed(&bot.id, &err.to_string())?;
                    return Err(err);
                }
            };
            self.runtimes.lock().await.insert(bot.id.clone(), runtime);
        }

        let message = match from {
            Some(name) => format!("[{name}] {text}"),
            None => text.to_owned(),
        };
        let mut runtimes = self.runtimes.lock().await;
        let Some(runtime) = runtimes.get_mut(&bot.id) else {
            unreachable!("inserted above, and only this call may touch the bot meanwhile");
        };
        if let Err(err) = runtime.process.send(&message).await {
            runtimes.remove(&bot.id);
            drop(runtimes);
            self.turn_failed(&bot.id, &err.to_string())?;
            return Err(err.into());
        }
        runtime.busy = true;
        runtime.permit = Some(permit);
        runtime.last_active = Instant::now();
        drop(runtimes);
        self.announce_bots().await;
        Ok(())
    }

    fn turn_failed(&self, bot: &BotId, message: &str) -> Result<(), HubError> {
        self.record(
            bot,
            EventKind::Error {
                message: message.to_owned(),
            },
        )?;
        self.record(
            bot,
            EventKind::TurnDone {
                ok: false,
                duration_ms: 0,
            },
        )?;
        Ok(())
    }

    /// Asks a busy bot to stop its current turn, and says no to any card or
    /// sign-in it's waiting on. Does nothing if it's idle.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist or Claude has stopped.
    pub async fn cancel(&self, bot: &str) -> Result<(), HubError> {
        let id = self.bot(bot)?.id;
        self.deny_open_approvals(&id);
        self.cancel_sign_ins(&id);
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(&id).filter(|r| r.busy) {
            runtime.process.interrupt().await?;
        }
        Ok(())
    }

    /// Stops every `claude` child and the browser (sign-ins stay).
    pub async fn shutdown(&self) {
        let runtimes: Vec<_> = self.runtimes.lock().await.drain().collect();
        for (_, runtime) in runtimes {
            runtime.process.shutdown().await;
        }
        if let Some(shared) = &self.shared {
            shared.browser.shutdown().await;
        }
    }

    async fn stop_claude(&self, bot: &BotId) {
        let runtime = self.runtimes.lock().await.remove(bot);
        if let Some(r) = runtime {
            r.process.shutdown().await;
        }
    }

    /// Stops bots that have been idle for `idle_stop`, and the browser once
    /// nobody has used it for that long.
    async fn stop_idle(&self) {
        let waiting: HashSet<String> = self
            .pending_approvals()
            .into_iter()
            .map(|a| a.bot)
            .chain(self.pending_sign_ins().into_iter().map(|r| r.bot))
            .collect();
        let idle: Vec<BotId> = self
            .runtimes
            .lock()
            .await
            .iter()
            .filter(|(id, r)| {
                !r.busy
                    && r.last_active.elapsed() >= self.config.idle_stop
                    && !waiting.contains(id.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in idle {
            tracing::info!(bot = %id, "idle; stopping Claude until the next message");
            self.stop_claude(&id).await;
        }
        if let Some(shared) = &self.shared
            && waiting.is_empty()
            && lock(&shared.last_used).elapsed() >= self.config.idle_stop
            && *shared.browser.status().borrow() == BrowserStatus::Ready
            && let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned()
        {
            tracing::info!("browser idle; stopping Chrome until a bot needs it (sign-ins stay)");
            shared.browser.shutdown().await;
            lock(&shared.last).take();
        }
    }

    /// Starts `bot`'s Claude and a task that turns its output into events.
    /// Resumes the saved session unless the system prompt has changed.
    async fn start_claude(self: &Arc<Self>, bot: &Bot) -> Result<Runtime, HubError> {
        let caps = Capabilities {
            browser: self.shared.is_some(),
        };
        let prompt = system_prompt(bot, caps);
        let session = match self.store.session(&bot.id)? {
            Some(saved) if saved.prompt == prompt => Session::Resume(saved.id),
            saved => {
                if saved.is_some() {
                    tracing::info!(bot = %bot.id, "system prompt changed; starting a new Claude session");
                }
                Session::New(uuid::Uuid::new_v4().to_string())
            }
        };
        let mut config = ClaudeConfig::new(bot.work_dir(), session.clone());
        config.program.clone_from(&self.config.claude_program);
        config.model.clone_from(&bot.model);
        config.extra_env.clone_from(&self.config.extra_env);
        config.instructions = Some(prompt.clone());
        // Every tool that isn't pre-allowed is checked by oxbot.
        config.ask_oxbot = true;

        let mut servers = serde_json::Map::new();
        if let (Some(tools), Some(shared)) = (self.config.browser.clone(), &self.shared) {
            if !tools.is_installed() {
                self.notice("Setting up the browser for bots (first time only)…");
            }
            let started = async {
                tools.ensure_installed().await?;
                shared.browser.ensure_running().await
            };
            match started.await {
                Ok(()) => {
                    servers.insert(
                        MCP_SERVER_NAME.to_owned(),
                        tools.mcp_server(shared.browser.port(), &bot.work_dir()),
                    );
                    servers.insert(TOOL_SERVER_NAME.to_owned(), tools::mcp_entry());
                }
                Err(err) => {
                    tracing::warn!(%err, "starting without the browser");
                    self.notice(&format!(
                        "{} can't use the browser right now: {err}",
                        bot.name
                    ));
                }
            }
        }
        // The Assistant always gets its team tools, browser or not.
        if bot.is_central() {
            servers.insert(TOOL_SERVER_NAME.to_owned(), tools::mcp_entry());
        }
        if !servers.is_empty() {
            config.mcp_config = Some(json!({ "mcpServers": servers }).to_string());
            // A sign-in or a task can take minutes: let a tool call wait that long.
            let wait =
                self.config.sign_in_timeout.max(self.config.task_timeout) + Duration::from_mins(1);
            config
                .extra_env
                .push(("MCP_TOOL_TIMEOUT".to_owned(), wait.as_millis().to_string()));
        }

        let (process, events) = ClaudeProcess::spawn(&config)?;
        self.store.set_session(
            &bot.id,
            Some(&SavedSession {
                id: session.id().to_owned(),
                prompt,
            }),
        )?;
        tokio::spawn(Arc::clone(self).pump(bot.id.clone(), events));
        Ok(Runtime {
            process,
            busy: false,
            permit: None,
            lease: None,
            last_active: Instant::now(),
            tool_names: HashMap::new(),
            page: None,
            elements: HashMap::new(),
        })
    }

    /// Turns one bot's Claude output into stored events until the child exits.
    async fn pump(self: Arc<Self>, bot: BotId, mut events: mpsc::Receiver<ClaudeEvent>) {
        let mut started = false;
        while let Some(event) = events.recv().await {
            if let Err(err) = self.handle(&bot, event, &mut started).await {
                tracing::error!(%bot, %err, "couldn't record claude output");
            }
        }

        // The child exited.
        self.deny_open_approvals(&bot);
        self.cancel_sign_ins(&bot);
        let removed = self.runtimes.lock().await.remove(&bot);
        let was_busy = removed.is_some_and(|mut r| {
            self.release_browser(&bot, &mut r);
            r.busy
        });
        if !started && was_busy {
            // It never got going (e.g. its saved session is gone): start fresh next time.
            tracing::warn!(%bot, "claude exited before starting; forgetting its session");
            let _ = self.store.set_session(&bot, None);
        }
        if was_busy {
            let _ = self.turn_failed(&bot, "Claude stopped unexpectedly.");
        }
    }

    async fn handle(
        self: &Arc<Self>,
        bot: &BotId,
        event: ClaudeEvent,
        started: &mut bool,
    ) -> Result<(), HubError> {
        match event {
            ClaudeEvent::Init { .. } => *started = true,
            ClaudeEvent::TextDelta(text) => {
                let _ = self.updates.send(ServerMsg::Delta {
                    bot: bot.to_string(),
                    text,
                });
            }
            ClaudeEvent::Assistant { text, tool_uses } => {
                if !text.trim().is_empty() {
                    self.record(bot, EventKind::AssistantMessage { text })?;
                }
                if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
                    for tool in &tool_uses {
                        r.tool_names.insert(tool.id.clone(), tool.name.clone());
                    }
                }
                for tool in tool_uses {
                    let summary = tool.summary();
                    let name = tool_label(&tool.name);
                    self.record(bot, EventKind::ToolUse { name, summary })?;
                }
            }
            ClaudeEvent::ToolResult { tool_use_id, text } => {
                if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
                    let is_browser = r
                        .tool_names
                        .remove(&tool_use_id)
                        .is_some_and(|n| n.starts_with(&format!("mcp__{MCP_SERVER_NAME}__")));
                    if is_browser {
                        learn_from_result(r, &text);
                    }
                    r.last_active = Instant::now();
                }
                let _ = self.updates.send(ServerMsg::Activity {
                    bot: bot.to_string(),
                    text: activity_label(&Activity::Requesting),
                });
            }
            ClaudeEvent::Permission(request) => {
                tokio::spawn(Arc::clone(self).permission(bot.clone(), request));
            }
            ClaudeEvent::McpMessage {
                request_id,
                server,
                message,
            } => {
                if server == TOOL_SERVER_NAME {
                    tokio::spawn(Arc::clone(self).tool_message(bot.clone(), request_id, message));
                } else {
                    tracing::warn!(%bot, server, "message for a tool server oxbot doesn't host");
                }
            }
            ClaudeEvent::Limits(windows) => {
                self.store.set_limits(&windows)?;
                self.announce_usage();
            }
            ClaudeEvent::TurnResult {
                ok,
                text,
                duration_ms,
                usage,
            } => {
                self.deny_open_approvals(bot);
                self.cancel_sign_ins(bot);
                self.store.add_usage(bot, &usage)?;
                if !ok {
                    self.record(bot, EventKind::Error { message: text })?;
                }
                self.record(bot, EventKind::TurnDone { ok, duration_ms })?;
                if let Some(runtime) = self.runtimes.lock().await.get_mut(bot) {
                    runtime.busy = false;
                    runtime.permit = None;
                    runtime.last_active = Instant::now();
                    self.release_browser(bot, runtime);
                }
                self.announce_bots().await;
                self.announce_usage();
            }
            ClaudeEvent::Activity(activity) => {
                let _ = self.updates.send(ServerMsg::Activity {
                    bot: bot.to_string(),
                    text: activity_label(&activity),
                });
            }
            ClaudeEvent::ControlAck { .. } => {}
        }
        Ok(())
    }

    /// Stores an event and tells every client about it.
    fn record(&self, bot: &BotId, kind: EventKind) -> Result<BotEvent, HubError> {
        let event = self.store.append(bot, kind)?;
        // No listeners is fine: nobody's connected right now.
        let _ = self.updates.send(ServerMsg::Event(event.clone()));
        Ok(event)
    }

    /// Tells every connected client something, without storing it.
    fn notice(&self, message: &str) {
        let _ = self.updates.send(ServerMsg::Notice {
            message: message.to_owned(),
        });
    }
}

fn browser_state(status: BrowserStatus) -> BrowserState {
    match status {
        BrowserStatus::Off => BrowserState::Off,
        BrowserStatus::Ready => BrowserState::Ready,
        BrowserStatus::SigningIn => BrowserState::SigningIn,
    }
}

/// Tells clients when the browser changes state, and counts a closed
/// sign-in window as "signed in".
async fn watch_browser(hub: std::sync::Weak<Hub>, browser: Arc<Browser>) {
    let mut status = browser.status();
    drop(browser);
    let mut before = *status.borrow();
    while status.changed().await.is_ok() {
        let now = *status.borrow_and_update();
        let Some(hub) = hub.upgrade() else { return };
        if before == BrowserStatus::SigningIn && now != BrowserStatus::SigningIn {
            hub.sign_in_window_closed();
        }
        before = now;
        hub.announce_bots().await;
    }
}

/// Every minute, stops bots that have been idle for a while.
async fn stop_idle_bots(hub: std::sync::Weak<Hub>) {
    loop {
        tokio::time::sleep(Duration::from_mins(1)).await;
        let Some(hub) = hub.upgrade() else { return };
        hub.stop_idle().await;
    }
}

/// Reads what the browser tool reported: the selected page and the element
/// names from a page snapshot.
fn learn_from_result(runtime: &mut Runtime, text: &str) {
    let mut elements = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("uid=") {
            let (uid, words) = rest.split_once(' ').unwrap_or((rest, ""));
            elements.insert(uid.to_owned(), shorten(words, 80));
        } else if let Some(page) = selected_page(line) {
            runtime.page = Some(page);
        }
    }
    if !elements.is_empty() {
        runtime.elements = elements;
    }
}

/// `3: Inbox (https://mail.google.com/…) [selected]` → (`Inbox`, address).
fn selected_page(line: &str) -> Option<(String, String)> {
    let label = line.strip_suffix(" [selected]")?;
    let (number, label) = label.split_once(": ")?;
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if let Some(inner) = label.strip_suffix(')')
        && let Some((title, url)) = inner.rsplit_once(" (")
    {
        return Some((title.to_owned(), url.to_owned()));
    }
    Some((String::new(), label.to_owned()))
}

fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

/// How a tool call shows in the chat: `browser navigate_page`,
/// `sign-in request`.
fn tool_label(name: &str) -> String {
    if name == format!("mcp__{TOOL_SERVER_NAME}__{SIGN_IN_TOOL}") {
        return "sign-in request".to_owned();
    }
    if let Some(label) = team_label(name) {
        return label.to_owned();
    }
    match name.strip_prefix(&format!("mcp__{MCP_SERVER_NAME}__")) {
        Some(action) => format!("browser {action}"),
        None => name.to_owned(),
    }
}

/// Plain names for the Assistant's team tools.
fn team_label(name: &str) -> Option<&'static str> {
    let tool = name.strip_prefix(&format!("mcp__{TOOL_SERVER_NAME}__"))?;
    Some(match tool {
        tools::LIST_BOTS_TOOL => "looking at your bots",
        tools::CREATE_BOT_TOOL => "creating a bot",
        tools::GIVE_TASK_TOOL => "giving a bot a task",
        tools::READ_REPLY_TOOL => "reading a bot's reply",
        _ => return None,
    })
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The status line for an activity, in the terminal's spirit.
fn activity_label(activity: &Activity) -> String {
    match activity {
        Activity::Requesting => "Clauding…".to_owned(),
        Activity::Thinking => "Thinking…".to_owned(),
        Activity::Writing => "Writing…".to_owned(),
        Activity::Retrying => "Waiting to retry…".to_owned(),
        Activity::UsingTool(name)
            if name == &format!("mcp__{TOOL_SERVER_NAME}__{SIGN_IN_TOOL}") =>
        {
            "Asking you to sign in…".to_owned()
        }
        Activity::UsingTool(name) if team_label(name).is_some() => {
            format!("{}…", capitalized(team_label(name).unwrap_or_default()))
        }
        Activity::UsingTool(name) => {
            match name.strip_prefix(&format!("mcp__{MCP_SERVER_NAME}__")) {
                Some(action) => format!("Using browser: {action}…"),
                None => format!("Using {name}…"),
            }
        }
        Activity::Other(status) => format!("{status}…"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_calls_get_readable_names() {
        assert_eq!(
            tool_label("mcp__chrome-devtools__navigate_page"),
            "browser navigate_page"
        );
        assert_eq!(tool_label("mcp__oxbot__request_sign_in"), "sign-in request");
        assert_eq!(tool_label("mcp__oxbot__give_task"), "giving a bot a task");
        assert_eq!(
            activity_label(&Activity::UsingTool("mcp__oxbot__create_bot".into())),
            "Creating a bot…"
        );
        assert_eq!(tool_label("Write"), "Write");
    }

    #[test]
    fn activity_labels_read_like_the_terminal() {
        assert_eq!(activity_label(&Activity::Requesting), "Clauding…");
        assert_eq!(
            activity_label(&Activity::UsingTool(
                "mcp__chrome-devtools__navigate_page".into()
            )),
            "Using browser: navigate_page…"
        );
        assert_eq!(
            activity_label(&Activity::UsingTool("mcp__oxbot__request_sign_in".into())),
            "Asking you to sign in…"
        );
        assert_eq!(
            activity_label(&Activity::UsingTool("Write".into())),
            "Using Write…"
        );
    }

    #[test]
    fn the_selected_page_is_read_from_the_page_list() {
        assert_eq!(
            selected_page(
                "2: Inbox (3) - Gmail (https://mail.google.com/mail/u/0/#inbox) [selected]"
            ),
            Some((
                "Inbox (3) - Gmail".to_owned(),
                "https://mail.google.com/mail/u/0/#inbox".to_owned()
            ))
        );
        assert_eq!(
            selected_page("1: about:blank [selected]"),
            Some((String::new(), "about:blank".to_owned()))
        );
        assert_eq!(selected_page("1: https://x.com (https://x.com)"), None);
        assert_eq!(selected_page("Note: x [selected]"), None);
    }
}
