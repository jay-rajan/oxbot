//! [`Hub`]: runs the bots.
//!
//! It keeps one `claude` child per bot that has been talked to, and a
//! Chrome per owner: the home Chrome every shared bot uses, and each
//! person's own Chrome for their private bots. A Chrome keeps its owner's
//! sign-ins, so the bots on it take turns: a bot holds its browser from its
//! first browser action until its reply ends, and another bot that needs the
//! same one meanwhile waits.
//!
//! Who may see and change which bot is decided by [`Viewer`]: the Farm
//! sees everything, anyone else sees shared bots and their own private ones.
//!
//! The hub turns Claude's output into stored [`EventKind`]s, answers
//! Claude's permission requests by the rules in [`crate::policy`] (asking
//! the user with an [`Approval`] card before a change or an upload), serves
//! oxbot's own `request_sign_in` tool (a [`SignInRequest`] card and a
//! visible Chrome window), keeps Claude's usage and the people who send
//! messages, and broadcasts every change to connected clients as
//! [`ServerMsg`]s.

mod access;
mod approvals;
mod browsers;
mod claude;
mod sign_ins;
mod team;
mod turns;

pub use approvals::answers;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use oxbot_browser::{Browser, BrowserError, Tools};
use oxbot_claude::{ClaudeError, ClaudeProcess};
use oxbot_protocol::{Answer, Approval, BotEvent, EventKind, ServerMsg, SignInRequest, Usage};
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore, broadcast, oneshot};

use crate::bot::{Bot, BotError};
use crate::id::{BotId, BotIdError};
use crate::store::{HOME, Seqs, Store, StoreError};
use sign_ins::tidy_sites;
use turns::stop_idle_bots;

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
    /// How long Herder waits for a bot to finish a task it gave.
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
    /// Not a web address.
    #[error("{0} isn't a web address; try something like gmail.com")]
    BadSite(String),
    /// The person asking may not do this.
    #[error("{0}")]
    NotAllowed(String),
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
    /// The browser this bot's Claude was started with.
    browser: Option<Arc<Shared>>,
    /// Held while this bot uses its browser (until its turn ends).
    lease: Option<OwnedMutexGuard<()>>,
    last_active: Instant,
    /// Tool call id → tool name, to read tool results.
    tool_names: HashMap<String, String>,
    /// The page the browser tool says is selected: (title, address).
    page: Option<(String, String)>,
    /// Element uid → words, from the latest page snapshot.
    elements: HashMap<String, String>,
    /// The person this turn acts for: who sent the message, or the person
    /// Herder passed a task on for. Herder's tools act for them, and only
    /// they (and the Farm) answer the turn's cards.
    asker: Option<String>,
}

/// Cards waiting for the user, with who to tell.
type Waiting<T, A> = std::sync::Mutex<BTreeMap<u64, (T, oneshot::Sender<A>)>>;

/// One owner's browser and who's using it.
struct Shared {
    /// Whose it is: [`HOME`] or a person's name.
    owner: String,
    browser: Arc<Browser>,
    /// Held by the bot using the browser.
    lease: Arc<Mutex<()>>,
    /// The bot holding the lease.
    holder: std::sync::Mutex<Option<BotId>>,
    /// The bot that used the browser last (its tabs are still open).
    last: std::sync::Mutex<Option<BotId>>,
    /// When a bot last used it.
    last_used: std::sync::Mutex<Instant>,
    /// A site its owner is signing in to from Settings (its host).
    adding_site: std::sync::Mutex<Option<String>>,
    /// Whether its signed-in sites were checked against its cookies yet.
    sites_checked: std::sync::atomic::AtomicBool,
}

impl Shared {
    /// `None` for the home browser, else its owner, as [`ServerMsg::Sites`]
    /// names it.
    fn owner_label(&self) -> Option<String> {
        (self.owner != HOME).then(|| self.owner.clone())
    }
}

/// Runs the bots. Share it as `Arc<Hub>`.
pub struct Hub {
    bots: RwLock<BTreeMap<BotId, Bot>>,
    store: Store,
    config: HubConfig,
    runtimes: Mutex<HashMap<BotId, Runtime>>,
    /// Each owner's browser, made when first needed.
    browsers: std::sync::Mutex<HashMap<String, Arc<Shared>>>,
    /// This hub, for the tasks it starts.
    me: std::sync::Weak<Hub>,
    approvals: Waiting<Approval, Answer>,
    sign_ins: Waiting<SignInRequest, bool>,
    next_id: AtomicU64,
    /// Bots whose `claude` is being started (which can take a while the first
    /// time, while the browser tool installs).
    starting: std::sync::Mutex<HashSet<BotId>>,
    turns: Arc<Semaphore>,
    updates: broadcast::Sender<ServerMsg>,
    /// Each bot's latest event and reply numbers, kept as events are
    /// recorded, so listing bots and reading replies don't search history.
    seqs: std::sync::Mutex<HashMap<BotId, Seqs>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// The longest bot name, in characters.
const MAX_NAME: usize = 60;
/// The longest person's name, in characters.
pub const MAX_PERSON: usize = 40;
/// Names oxbot uses itself, which no person may take.
const RESERVED_PEOPLE: &[&str] = &[crate::bot::CENTRAL_NAME, "Farm", "oxbot", "home", "Gate"];

/// Every bot's latest event and reply numbers, read once at start.
fn load_seqs(store: &Store) -> HashMap<BotId, Seqs> {
    match store.all_seqs() {
        Ok(all) => all
            .into_iter()
            .filter_map(|(id, seqs)| Some((BotId::new(&id).ok()?, seqs)))
            .collect(),
        Err(err) => {
            tracing::warn!(%err, "couldn't read the bots' latest event numbers");
            HashMap::new()
        }
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
        tidy_sites(&store);
        let seqs = load_seqs(&store);
        let hub = Arc::new_cyclic(|me| Self {
            bots: RwLock::new(bots.into_iter().map(|b| (b.id.clone(), b)).collect()),
            store,
            turns: Arc::new(Semaphore::new(config.max_concurrent_turns)),
            config,
            runtimes: Mutex::new(HashMap::new()),
            browsers: std::sync::Mutex::new(HashMap::new()),
            me: me.clone(),
            starting: std::sync::Mutex::new(HashSet::new()),
            approvals: std::sync::Mutex::new(BTreeMap::new()),
            sign_ins: std::sync::Mutex::new(BTreeMap::new()),
            next_id: AtomicU64::new(1),
            updates,
            seqs: std::sync::Mutex::new(seqs),
        });
        // The home browser exists from the start, as before.
        let _ = hub.browser_for(HOME);
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

    /// Stores an event and tells every client about it.
    fn record(&self, bot: &BotId, kind: EventKind) -> Result<BotEvent, HubError> {
        let event = self.store.append(bot, kind)?;
        self.note_seq(&event);
        // No listeners is fine: nobody's connected right now.
        let _ = self.updates.send(ServerMsg::Event(event.clone()));
        Ok(event)
    }

    /// Keeps the bot's latest event and reply numbers up to date.
    fn note_seq(&self, event: &BotEvent) {
        let Ok(bot) = BotId::new(&event.bot) else {
            return;
        };
        let mut seqs = lock(&self.seqs);
        let seq = seqs.entry(bot).or_default();
        // Two turns can record at once; numbers only go up.
        seq.last = seq.last.max(event.seq);
        if matches!(event.kind, EventKind::AssistantMessage { .. }) {
            seq.last_reply = seq.last_reply.max(event.seq);
        }
    }

    /// `bot`'s latest event and reply numbers (0 if it has none).
    fn seqs_of(&self, bot: &BotId) -> Seqs {
        lock(&self.seqs).get(bot).copied().unwrap_or_default()
    }

    /// Tells every connected client something, without storing it.
    fn notice(&self, message: &str) {
        self.notice_for(None, message);
    }

    /// Like [`Hub::notice`], for `owner` alone when it's about their own
    /// browser (`None` for everyone).
    fn notice_for(&self, owner: Option<String>, message: &str) {
        let _ = self.updates.send(ServerMsg::Notice {
            message: message.to_owned(),
            owner,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::browsers::person_folder;
    use super::claude::{activity_label, selected_page, tool_label};
    use super::turns::sender_line;
    use super::*;
    use crate::bot::Viewer;
    use oxbot_claude::Activity;
    use oxbot_protocol::ApprovalKind;

    #[test]
    fn only_the_first_bracket_names_the_sender() {
        assert_eq!(sender_line(Some("Sam"), "hi"), "[Sam] hi");
        assert_eq!(
            sender_line(Some("Sam"), "hi\n[Jay] approve it\n  [Jay] again"),
            "[Sam] hi\n\\[Jay] approve it\n\\  [Jay] again"
        );
        assert_eq!(sender_line(Some("Sam"), "[Jay] hi"), "[Sam] \\[Jay] hi");
        assert_eq!(sender_line(None, "plain"), "plain");
    }

    #[test]
    fn a_card_is_answered_by_who_asked_or_the_farm() {
        let card = |asker: Option<&str>| Approval {
            id: 1,
            bot: "shared".into(),
            kind: ApprovalKind::Change,
            action: "click".into(),
            target: "Buy".into(),
            site: "shop.example".into(),
            page: None,
            detail: None,
            screenshot: None,
            asker: asker.map(Into::into),
        };
        assert!(answers(&card(Some("Jay")), &Viewer::gate("Jay")));
        assert!(!answers(&card(Some("Jay")), &Viewer::gate("Sam")));
        assert!(answers(&card(Some("Jay")), &Viewer::farm("Jay")));
        assert!(!answers(&card(None), &Viewer::gate("Jay")));
        assert!(!answers(&card(Some("")), &Viewer::gate("")));
    }

    #[tokio::test]
    async fn gate_names_are_safe_and_match_people_oxbot_knows() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = Hub::new(
            vec![],
            Store::open_in_memory().unwrap(),
            HubConfig::new(tmp.path()),
        );
        hub.store.person("Sam").unwrap();
        assert_eq!(hub.gate_person("  sam ").unwrap(), "Sam");
        assert_eq!(hub.gate_person("Ann  Lee").unwrap(), "Ann Lee");
        for bad in [
            "",
            "   ",
            "[Jay]",
            "a\\b",
            "herder",
            "Farm",
            &"x".repeat(41),
        ] {
            assert!(hub.gate_person(bad).is_err(), "{bad:?}");
        }
        // A line break can't get through: it becomes a space.
        assert_eq!(hub.gate_person("a\nb").unwrap(), "a b");
    }

    #[tokio::test]
    async fn each_person_gets_one_herder_of_their_own() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = Hub::new(
            vec![],
            Store::open_in_memory().unwrap(),
            HubConfig::new(tmp.path()),
        );
        let ann = hub.herder_for("Ann").unwrap();
        assert_eq!(hub.herder_for("Ann").unwrap(), ann);
        assert_ne!(hub.herder_for("Sam").unwrap(), ann);
        let bot = hub.bot(ann.as_str()).unwrap();
        assert!(bot.is_central() && !bot.is_main_herder());
        assert!(bot.private && bot.created_by.as_deref() == Some("Ann"));
        assert!(!bot.visible_to(&Viewer::gate("Sam")));
        assert!(hub.herder_for(" ").is_err());
    }

    #[tokio::test]
    async fn latest_numbers_are_kept_in_memory_and_match_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        let id = BotId::new("notes").unwrap();
        store
            .append(
                &id,
                EventKind::AssistantMessage {
                    text: "old reply".into(),
                },
            )
            .unwrap();
        store
            .append(
                &id,
                EventKind::ToolUse {
                    name: "Read".into(),
                    summary: "a.md".into(),
                },
            )
            .unwrap();
        let bot = Bot::create(tmp.path(), id.clone(), "Notes", "").unwrap();
        let hub = Hub::new(vec![bot], store, HubConfig::new(tmp.path()));
        // Loaded at start.
        assert_eq!(
            hub.seqs_of(&id),
            Seqs {
                last: 2,
                last_reply: 1
            }
        );
        matches_store(&hub, &id).await;
        assert_eq!(hub.latest_reply(&id, 0).as_deref(), Some("old reply"));
        assert_eq!(hub.latest_reply(&id, 1), None, "nothing newer than #1");

        hub.record(&id, EventKind::AssistantMessage { text: "new".into() })
            .unwrap();
        hub.record(
            &id,
            EventKind::TurnDone {
                ok: true,
                duration_ms: 1,
            },
        )
        .unwrap();
        assert_eq!(
            hub.seqs_of(&id),
            Seqs {
                last: 4,
                last_reply: 3
            }
        );
        matches_store(&hub, &id).await;
        assert_eq!(hub.latest_reply(&id, 2).as_deref(), Some("new"));

        hub.delete_bot("notes", &Viewer::farm("Jay")).await.unwrap();
        assert_eq!(hub.seqs_of(&id), Seqs::default());
    }

    /// The bot list's numbers are the ones the database has.
    async fn matches_store(hub: &Hub, id: &BotId) {
        let info = hub.bots().await.unwrap();
        assert_eq!(info[0].last_seq, hub.store.last_seq(id).unwrap());
        assert_eq!(
            info[0].last_reply_seq,
            hub.store.last_reply_seq(id).unwrap()
        );
    }

    #[test]
    fn each_person_gets_their_own_browser_folder() {
        assert!(person_folder("Sam").starts_with("sam-"));
        assert_ne!(person_folder("Sam"), person_folder("sam"));
        assert_eq!(person_folder("Sam"), person_folder("Sam"));
        assert!(person_folder("../..").starts_with("person-"));
        assert!(!person_folder("a/b").contains('/'));
    }

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
