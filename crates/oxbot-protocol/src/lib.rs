//! Wire types shared by the oxbot server and its clients (the terminal client
//! and the Android app).
//!
//! Every message that crosses the network is defined here, once, so the
//! server and the apps can never disagree about its shape. Messages are JSON,
//! one per WebSocket text frame, tagged by a `"type"` field.

use serde::{Deserialize, Serialize};

/// Version of the wire protocol. Bumped on any breaking change, so an old app
/// can tell the user to update instead of failing in odd ways.
pub const PROTOCOL_VERSION: u32 = 1;

/// The most events one [`ServerMsg::History`] page holds.
pub const MAX_PAGE: u32 = 500;
/// Events per [`ServerMsg::History`] page when the client doesn't say.
pub const DEFAULT_PAGE: u32 = 200;

/// The first message a server sends after a client connects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    /// Protocol version the server speaks.
    pub protocol_version: u32,
    /// Human-readable server name, e.g. the Mac's name.
    pub server_name: String,
    /// The oxbot version running on the server.
    pub server_version: String,
    /// When this server process started (ms since the Unix epoch). A client
    /// that sees it change knows the server restarted (the page reloads).
    #[serde(default)]
    pub started_ms: i64,
}

impl ServerHello {
    /// Builds a hello for this build of oxbot, for a server that started at
    /// `started_ms`.
    #[must_use]
    pub fn new(server_name: impl Into<String>, started_ms: i64) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            server_name: server_name.into(),
            server_version: env!("CARGO_PKG_VERSION").to_owned(),
            started_ms,
        }
    }

    /// Whether a client built for [`PROTOCOL_VERSION`] can talk to this server.
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
    }
}

/// A message from a client (terminal or phone) to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Ask for the list of bots.
    ListBots,
    /// Start receiving a bot's events. The server first replays stored events
    /// with `seq > since`, then streams new ones. Use `since: 0` for all.
    Subscribe {
        /// Bot id.
        bot: String,
        /// Last event number the client already has.
        since: u64,
    },
    /// Open a bot: the server answers with one [`ServerMsg::History`] of
    /// its latest `limit` events, then streams new ones as
    /// [`ServerMsg::Event`]s (like [`ClientMsg::Subscribe`] from the end of
    /// that page).
    OpenBot {
        /// Bot id.
        bot: String,
        /// Most events to send; the server's default (200) if absent, and
        /// never more than [`MAX_PAGE`].
        #[serde(default)]
        limit: Option<u32>,
    },
    /// An earlier page of a bot's history, answered with a
    /// [`ServerMsg::History`] of the events just before `before`.
    Older {
        /// Bot id.
        bot: String,
        /// The oldest event number the client has
        /// ([`ServerMsg::History::first_seq`]).
        before: u64,
        /// Most events to send, as for [`ClientMsg::OpenBot`].
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Send the user's message to a bot.
    Send {
        /// Bot id.
        bot: String,
        /// What the user typed.
        text: String,
    },
    /// Stop the bot's current turn.
    Cancel {
        /// Bot id.
        bot: String,
    },
    /// Create a new bot. The server picks its id from the name, replies with
    /// [`ServerMsg::BotCreated`] and sends every client an updated
    /// [`ServerMsg::Bots`].
    CreateBot {
        /// Display name, e.g. `Daily News`.
        name: String,
        /// What the bot should do, in the user's words.
        instructions: String,
        /// Only its creator (and the Farm) can see and use it.
        #[serde(default)]
        private: bool,
    },
    /// Make a bot private (only its creator and the Farm see it, and it
    /// uses its creator's own browser) or shared. Its creator or the Farm
    /// only.
    SetPrivate {
        /// Bot id.
        bot: String,
        /// Private or shared.
        private: bool,
    },
    /// Give a bot a new name. Its creator or the Farm only.
    RenameBot {
        /// Bot id.
        bot: String,
        /// The new name.
        name: String,
    },
    /// Delete a bot and its history (its folder goes to oxbot's trash).
    /// Its creator or the Farm only; Herder can't be deleted.
    DeleteBot {
        /// Bot id.
        bot: String,
    },
    /// The user's answer to an [`Approval`].
    Answer {
        /// The approval's id.
        id: u64,
        /// What they chose.
        answer: Answer,
    },
    /// Turn "Launch at login" on or off (desktop app only). Everyone gets
    /// the new [`ServerMsg::Settings`].
    SetLaunchAtLogin {
        /// On or off.
        on: bool,
    },
    /// Open the sign-in window for a [`SignInRequest`].
    OpenSignIn {
        /// The request's id.
        id: u64,
    },
    /// The user has signed in for a [`SignInRequest`].
    SignInDone {
        /// The request's id.
        id: u64,
    },
    /// The user gave up on a [`SignInRequest`].
    CancelSignIn {
        /// The request's id.
        id: u64,
    },
    /// Sign a browser out of every site (deletes its profile).
    SignOutAll {
        /// Your own browser instead of the home one.
        #[serde(default)]
        mine: bool,
    },
    /// Run the setup check again; the answer is a [`ServerMsg::Setup`].
    CheckSetup,
    /// Show a signed-in site in the visible Chrome window.
    OpenSite {
        /// The site, as [`SiteInfo::site`] names it.
        site: String,
        /// In your own browser instead of the home one.
        #[serde(default)]
        mine: bool,
    },
    /// Open a sign-in window on `url` so the user signs in to a site
    /// before any bot needs it. Closing the window adds it to the list.
    AddSignIn {
        /// The site's address, e.g. `gmail.com` or `https://www.amazon.ca`.
        url: String,
        /// In your own browser (for your private bots) instead of the home one.
        #[serde(default)]
        mine: bool,
    },
    /// Sign a browser out of one site; the others stay signed in.
    SignOutSite {
        /// The site, as [`SiteInfo::site`] names it.
        site: String,
        /// Your own browser instead of the home one.
        #[serde(default)]
        mine: bool,
    },
    /// Do a setup step for the user (install Claude Code, sign in to
    /// Claude). Progress comes as [`ServerMsg::SetupProgress`], then a new
    /// [`ServerMsg::Setup`].
    RunSetup {
        /// Which step.
        action: SetupAction,
    },
    /// Farm only: show a pairing code (and QR code) for a new Gate that
    /// will belong to `person`. Replaces any code still showing. The answer
    /// is [`ServerMsg::Pairing`], then [`ServerMsg::Gates`] once it pairs.
    StartPairing {
        /// Who the Gate is for, e.g. `Sam`.
        person: String,
    },
    /// Farm only: stop showing the pairing code; it stops working.
    CancelPairing,
    /// Farm only: unpair a Gate. Its key stops working and it's
    /// disconnected.
    RemoveGate {
        /// [`GateInfo::id`].
        id: String,
    },
    /// Farm only: let phones use the network this Mac is on now. The
    /// answer is [`ServerMsg::Door`].
    AddDoorNetwork,
    /// Farm only: stop letting phones use a network.
    RemoveDoorNetwork {
        /// [`DoorNetwork::id`].
        id: String,
    },
    /// Farm only: the name phones see for this Farm on the network.
    SetDoorName {
        /// e.g. `Jay's Mac mini`; empty goes back to the Mac's name.
        name: String,
    },
}

/// A message from the server to a client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Sent once, right after the connection opens.
    Hello(ServerHello),
    /// The app's settings. Sent after [`ServerMsg::Hello`] and whenever they
    /// change.
    Settings(AppSettings),
    /// How much Claude has been used: the plan's limits and each bot's
    /// tokens. Sent after [`ServerMsg::Hello`] and after every turn.
    Usage(Usage),
    /// Reply to [`ClientMsg::ListBots`], and sent to everyone when the list
    /// changes.
    Bots {
        /// All bots on this server.
        bots: Vec<BotInfo>,
    },
    /// Reply to [`ClientMsg::CreateBot`], for the requester only.
    BotCreated(BotInfo),
    /// A stored, numbered event. Clients remember the highest `seq` they saw
    /// so they can catch up after a reconnect.
    Event(BotEvent),
    /// A page of a bot's stored events, oldest first: the answer to
    /// [`ClientMsg::OpenBot`] (its latest events) or [`ClientMsg::Older`].
    History {
        /// Bot id.
        bot: String,
        /// The events, oldest first.
        events: Vec<BotEvent>,
        /// Number of the oldest event in this page (0 if it's empty): ask
        /// for [`ClientMsg::Older`] events before it.
        first_seq: u64,
        /// Whether there are events before this page.
        has_older: bool,
    },
    /// A live piece of the reply as it's being written. Not stored: the full
    /// text arrives later as an [`EventKind::AssistantMessage`].
    Delta {
        /// Bot id.
        bot: String,
        /// The new text.
        text: String,
    },
    /// What a bot is doing right now, like the terminal's "Clauding…"
    /// spinner. Not stored; cleared by the turn's [`EventKind::TurnDone`].
    Activity {
        /// Bot id.
        bot: String,
        /// Short label, e.g. `Thinking…` or `Using browser: navigate_page…`.
        text: String,
    },
    /// A line from the server's log, so the app sees what the terminal
    /// shows. Sent live, plus the recent ones right after [`ServerMsg::Hello`].
    Log(LogLine),
    /// A bot's browser changed state.
    Browser {
        /// Bot id.
        bot: String,
        /// What its browser is doing.
        state: BrowserState,
    },
    /// A bot wants to do something that needs the user's yes. Sent to every
    /// client when it happens, and for each open one on connect.
    ApprovalNeeded(Approval),
    /// An approval was answered or timed out; clients drop the card.
    ApprovalResolved {
        /// The approval's id.
        id: u64,
    },
    /// A bot needs the user to sign in to a site. Sent to every client when
    /// it happens or changes (the window opened), and for each open one on
    /// connect.
    SignInNeeded(SignInRequest),
    /// A sign-in request was answered or timed out; clients drop it.
    SignInResolved {
        /// The request's id.
        id: u64,
    },
    /// What's installed and what's missing (Claude Code, Chrome, Node.js).
    /// Sent after [`ServerMsg::Hello`] and after [`ClientMsg::CheckSetup`].
    Setup(SetupReport),
    /// The sites a browser is signed in to: the home browser (shared bots)
    /// or one person's own (their private bots). Sent after
    /// [`ServerMsg::Hello`] (home, then yours) and whenever a list changes.
    Sites {
        /// Newest sign-in first.
        sites: Vec<SiteInfo>,
        /// Whose browser: `None` for home, else the person's name. A client
        /// only gets home and its own person's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
    },
    /// Farm only: a pairing code is ready to scan or type. Good once,
    /// until `expires_ms`.
    Pairing(PairingCode),
    /// Farm only: the pairing code was used, cancelled or ran out.
    PairingClosed,
    /// Farm only: the paired Gates. Sent after [`ServerMsg::Hello`] and
    /// whenever one pairs, connects, leaves or is removed.
    Gates {
        /// Oldest first.
        gates: Vec<GateInfo>,
    },
    /// Farm only: whether the Gate door is open, which networks phones may
    /// use, and the name it shows. Sent with [`ServerMsg::Gates`] and when
    /// any of it changes.
    Door(DoorState),
    /// A setup step oxbot is doing for the user, as it goes.
    SetupProgress {
        /// Which step.
        action: SetupAction,
        /// The latest line of output, in plain words.
        text: String,
        /// Whether it's still going.
        running: bool,
    },
    /// The people who have sent messages, each with their own color. Sent
    /// after [`ServerMsg::Hello`] and when someone new appears.
    People {
        /// Everyone, in the order oxbot first saw them.
        people: Vec<Person>,
    },
    /// Something the user should know that isn't an error.
    Notice {
        /// The message, for the user.
        message: String,
        /// Whose it is, if it's about one person's own browser: only they
        /// (and the Farm) get it. `None` for everyone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
    },
    /// Something the client asked for went wrong.
    Error {
        /// What went wrong, for the user.
        message: String,
    },
}

/// The app-wide settings shown in the Settings panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// The oxbot version running.
    pub version: String,
    /// Where oxbot keeps its bots and history.
    pub data_dir: String,
    /// Whether oxbot starts when the user logs in to the Mac; `None` when
    /// this server can't change it (it isn't the desktop app).
    pub launch_at_login: Option<bool>,
    /// Who this client's messages are sent as, e.g. `Jayadev`.
    #[serde(default)]
    pub you: String,
    /// Whether this client is on the Farm itself, which can see, change and
    /// delete every bot, private ones included.
    #[serde(default)]
    pub farm: bool,
}

/// A bot asking the user to sign in to a site in the shared browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignInRequest {
    /// Id to answer it with.
    pub id: u64,
    /// Bot id.
    pub bot: String,
    /// The sign-in page.
    pub url: String,
    /// Its site, e.g. `mail.google.com`.
    pub site: String,
    /// Why the bot needs it, in its words.
    pub why: String,
    /// Whether the sign-in window is open now.
    pub window_open: bool,
}

/// How a sign-in request ended, as stored in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInOutcome {
    /// The user signed in.
    Done,
    /// The user gave up.
    Cancelled,
    /// Nobody answered in time.
    TimedOut,
}

/// The result of oxbot's setup check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupReport {
    /// Every check, in order.
    pub checks: Vec<SetupCheck>,
}

impl SetupReport {
    /// Whether everything oxbot needs is there (optional checks don't count).
    #[must_use]
    pub fn ready(&self) -> bool {
        self.checks.iter().all(|c| c.ok || c.optional)
    }
}

/// A site the user signed in to through oxbot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteInfo {
    /// Its main domain, e.g. `google.com` (for a sign-in at
    /// accounts.google.com) or `substack.com`.
    pub site: String,
    /// When the user last signed in (ms since the Unix epoch).
    pub signed_in_ms: i64,
    /// The site's icon, as Chrome saved it (a `data:image/png` URL), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// A setup step oxbot can do for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupAction {
    /// Install Claude Code with Anthropic's installer.
    InstallClaude,
    /// Sign in to Claude: `claude auth login` opens the sign-in page.
    SignInClaude,
}

/// One thing oxbot needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupCheck {
    /// Stable id, e.g. `claude`, `claude_login`, `chrome`, `node`.
    pub id: String,
    /// What it is, e.g. `Claude Code`.
    pub name: String,
    /// Whether it's there.
    pub ok: bool,
    /// What oxbot found, or what's wrong, in plain words.
    pub detail: String,
    /// What to do about it, if it's missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Nice to have, not needed (e.g. Codex, before oxbot can use it).
    #[serde(default)]
    pub optional: bool,
    /// A step oxbot can do for the user, offered as a button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<SetupAction>,
}

/// A code that pairs one Gate with the Farm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    /// Who the Gate will belong to.
    pub person: String,
    /// What to type on the Gate, e.g. `K7QM-3XRP`.
    pub code: String,
    /// Everything the Gate needs (addresses, the Farm's certificate
    /// fingerprint, the code), as an `oxbot://pair?...` link. The QR code
    /// holds this.
    pub link: String,
    /// The QR code of `link`, as an SVG drawing.
    pub qr_svg: String,
    /// When the code stops working (ms since the Unix epoch).
    pub expires_ms: i64,
}

/// A phone or laptop paired with the Farm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateInfo {
    /// Its id.
    pub id: String,
    /// Who it belongs to; they act as this person.
    pub person: String,
    /// The device's name, e.g. `Pixel 10 Pro XL`.
    pub name: String,
    /// When it paired (ms since the Unix epoch).
    pub paired_ms: i64,
    /// When it last connected (ms since the Unix epoch).
    pub last_seen_ms: i64,
    /// Connected right now.
    pub online: bool,
}

/// A network phones may use to reach the Farm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorNetwork {
    /// The router's hardware address, e.g. `a4:2b:b0:12:34:56`. A café's
    /// Wi-Fi can copy a network's name, not this.
    pub id: String,
    /// What to call it: the Wi-Fi name if the Mac can read it, else its
    /// address range, e.g. `192.168.1.x`.
    pub label: String,
    /// The Mac is on it now.
    #[serde(default)]
    pub here: bool,
}

/// The Gate door, as Settings → Gates shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorState {
    /// Phones can connect right now.
    pub open: bool,
    /// Why it's closed, in plain words (empty when open).
    #[serde(default)]
    pub why: String,
    /// Networks phones may use.
    pub networks: Vec<DoorNetwork>,
    /// The network the Mac is on now, if it can tell (listed or not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<DoorNetwork>,
    /// The name phones see.
    pub name: String,
}

/// What a Gate sends to pair: `POST /pair` on the Gate door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    /// The code from the Farm's screen.
    pub code: String,
    /// The device's name, e.g. `Pixel 10 Pro XL`.
    pub device: String,
}

/// The Farm's answer to a good [`PairRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairReply {
    /// The new Gate's id.
    pub id: String,
    /// Who it belongs to.
    pub person: String,
    /// The Farm's name, e.g. `Jay's Mac mini`.
    pub farm: String,
    /// The Gate's key: sent once, kept by the Gate, shown on every
    /// connection as `Authorization: Bearer <key>`.
    pub key: String,
}

/// Someone who sends messages, with the color they keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// Their name, e.g. `Jayadev`.
    pub name: String,
    /// Their color, e.g. `#2F6FEB`.
    pub color: String,
}

/// How much Claude has been used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// The plan's limits as Claude last reported them (empty until the
    /// first reply), e.g. the 5-hour and weekly windows.
    pub limits: Vec<PlanLimit>,
    /// When Claude reported them (ms since the Unix epoch; 0 if never).
    pub limits_seen_ms: i64,
    /// Each bot's totals, most used first.
    pub bots: Vec<BotUsage>,
}

/// One of the plan's limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanLimit {
    /// Claude's name for it, e.g. `five_hour` or `seven_day`.
    pub name: String,
    /// How much is used, from 0 to 1.
    pub used: f64,
    /// When it resets (ms since the Unix epoch; 0 if unknown).
    pub resets_at_ms: i64,
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
}

/// One bot's Claude usage since it was created.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotUsage {
    /// Bot id.
    pub bot: String,
    /// Replies finished.
    pub turns: u64,
    /// New input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write_tokens: u64,
    /// Claude's cost estimate in US dollars (API prices; a subscription
    /// counts against its limits instead).
    pub cost_usd: f64,
}

/// One line from the server's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// `info`, `warn` or `error`.
    pub level: String,
    /// Where it came from, e.g. `oxbot_browser` or `claude_stderr`.
    pub target: String,
    /// The message, with any fields appended as `key=value`.
    pub message: String,
    /// When, in milliseconds since the Unix epoch.
    pub ts_ms: i64,
}

/// What the shared browser is doing. Every bot uses the same Chrome, which
/// keeps the user's sign-ins.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserState {
    /// This server gives bots no browser.
    Disabled,
    /// Not running; starts when a bot needs it.
    #[default]
    Off,
    /// Running, hidden.
    Ready,
    /// The sign-in window is open for the user.
    SigningIn,
}

/// What kind of thing an [`Approval`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    /// Uploading a file from the Mac to a site.
    Upload,
    /// A change on a site, e.g. clicking Send or Post (shown in red).
    Change,
    /// Herder (the central bot) wants to create a new bot.
    CreateBot,
    /// Herder wants to pass a task to someone's private bot, after reading
    /// web pages or other bots' replies (which may have asked it to).
    GiveTask,
}

/// Something a bot wants to do that needs the user's yes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// Id to answer it with.
    pub id: u64,
    /// Bot id.
    pub bot: String,
    /// What kind of request this is.
    pub kind: ApprovalKind,
    /// The action in a few words, e.g. `upload a file to`.
    pub action: String,
    /// What it acts on, e.g. `button "Choose file"`.
    pub target: String,
    /// The site, e.g. `example.com`.
    pub site: String,
    /// The page it's on, if known: its title or address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    /// More about it: the files it will upload, or the text it typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A picture of the page right now, as a `data:image/...` URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    /// The person whose request led to this card. Only they and the Farm
    /// see and answer it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<String>,
}

/// The user's answer to an [`Approval`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Do it, this once.
    AllowOnce,
    /// Don't.
    Deny,
}

/// What happened to an approval, as stored in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The user allowed it.
    Allowed,
    /// The user said no.
    Denied,
    /// Nobody answered in time.
    TimedOut,
}

/// What a client needs to show a bot in a list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a wire message: each flag is its own JSON field"
)]
pub struct BotInfo {
    /// Bot id, e.g. `assistant`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Whether the bot is in the middle of a turn.
    pub busy: bool,
    /// Number of the bot's latest stored event (0 if none).
    pub last_seq: u64,
    /// Number of the bot's latest reply (0 if none), so a client can mark
    /// bots with replies the user hasn't seen.
    #[serde(default)]
    pub last_reply_seq: u64,
    /// What its browser is doing.
    pub browser_state: BrowserState,
    /// Whether this is the central bot (Herder), which can create
    /// bots and give them tasks.
    #[serde(default)]
    pub central: bool,
    /// Who it belongs to (a person's name, as it is now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Only its creator (and the Farm) can see and use it.
    #[serde(default)]
    pub private: bool,
    /// Whether the client's person may rename it, delete it or change who
    /// sees it. Decided by the server for each client.
    #[serde(default)]
    pub can_manage: bool,
    /// Whether it's someone else's private bot, which won't open for the
    /// client's person. Decided by the server for each client.
    #[serde(default)]
    pub locked: bool,
    /// The owner's person id, for the server's checks. Never sent.
    #[serde(skip)]
    pub owner: Option<String>,
}

/// One stored event in a bot's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotEvent {
    /// Bot id.
    pub bot: String,
    /// Event number, counting from 1 per bot, with no gaps.
    pub seq: u64,
    /// When it happened, in milliseconds since the Unix epoch.
    pub ts_ms: i64,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// What happened in a [`BotEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// The user's message.
    UserMessage {
        /// What the user typed.
        text: String,
        /// Who sent it, e.g. `Jayadev` (missing in old history).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
    },
    /// A complete piece of the bot's reply.
    AssistantMessage {
        /// The text.
        text: String,
    },
    /// The bot used a tool, e.g. wrote a file.
    ToolUse {
        /// Tool name, e.g. `Write`.
        name: String,
        /// One line about what it did, e.g. `notes.md`.
        summary: String,
    },
    /// An approval was answered (shown as a red line).
    Approval {
        /// The action, e.g. `upload a file to`.
        action: String,
        /// What it acted on.
        target: String,
        /// The site.
        site: String,
        /// What happened.
        outcome: Outcome,
    },
    /// A sign-in request ended.
    SignIn {
        /// The site, e.g. `mail.google.com`.
        site: String,
        /// How it ended.
        outcome: SignInOutcome,
    },
    /// oxbot refused something the bot tried (shown as a red line).
    Blocked {
        /// What was refused and why, for the user.
        message: String,
    },
    /// The turn finished.
    TurnDone {
        /// Whether it finished without error.
        ok: bool,
        /// How long it took.
        duration_ms: u64,
    },
    /// The turn failed or the bot stopped.
    Error {
        /// What went wrong, for the user.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_flags_default_to_off_and_the_owner_is_never_sent() {
        let old: BotInfo = serde_json::from_str(
            r#"{"id":"a","name":"A","busy":false,"last_seq":0,"browser_state":"off"}"#,
        )
        .unwrap();
        assert!(!old.can_manage && !old.locked && old.owner.is_none());
        let info = BotInfo {
            id: "a".into(),
            owner: Some("p_0123456789abcdef".into()),
            locked: true,
            ..BotInfo::default()
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""locked":true"#), "{json}");
        assert!(!json.contains("p_0123"), "{json}");
    }

    #[test]
    fn hello_round_trips_through_json() {
        let hello = ServerHello::new("jays-mac", 1);
        let json = serde_json::to_string(&hello).unwrap();
        let back: ServerHello = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hello);
        assert!(back.is_compatible());
    }

    #[test]
    fn other_protocol_version_is_incompatible() {
        let hello = ServerHello {
            protocol_version: PROTOCOL_VERSION + 1,
            ..ServerHello::new("x", 1)
        };
        assert!(!hello.is_compatible());
    }

    #[test]
    fn opening_a_bot_needs_only_its_id() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"open_bot","bot":"a"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::OpenBot {
                bot: "a".into(),
                limit: None
            }
        );
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"older","bot":"a","before":201,"limit":50}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::Older {
                bot: "a".into(),
                before: 201,
                limit: Some(50)
            }
        );
        let page = ServerMsg::History {
            bot: "a".into(),
            events: vec![],
            first_seq: 0,
            has_older: false,
        };
        let json = serde_json::to_string(&page).unwrap();
        assert_eq!(
            json,
            r#"{"type":"history","bot":"a","events":[],"first_seq":0,"has_older":false}"#
        );
        assert_eq!(serde_json::from_str::<ServerMsg>(&json).unwrap(), page);
    }

    #[test]
    fn create_bot_needs_only_a_name_and_instructions() {
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"create_bot","name":"X","instructions":""}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::CreateBot {
                name: "X".into(),
                instructions: String::new(),
                private: false,
            }
        );
    }

    #[test]
    fn sign_in_actions_mean_the_home_browser_unless_mine() {
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"add_sign_in","url":"gmail.com"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::AddSignIn {
                url: "gmail.com".into(),
                mine: false
            }
        );
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"sign_out_site","site":"x.com","mine":true}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::SignOutSite {
                site: "x.com".into(),
                mine: true
            }
        );
        let json = serde_json::to_string(&ServerMsg::Sites {
            sites: vec![],
            owner: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"sites","sites":[]}"#);
    }

    #[test]
    fn browser_state_is_snake_case_and_per_bot() {
        let json = serde_json::to_string(&ServerMsg::Browser {
            bot: "news".into(),
            state: BrowserState::Ready,
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"browser","bot":"news","state":"ready"}"#);
    }

    #[test]
    fn answers_and_outcomes_are_snake_case() {
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"answer","id":4,"answer":"allow_once"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::Answer {
                id: 4,
                answer: Answer::AllowOnce
            }
        );
        let json = serde_json::to_string(&EventKind::Approval {
            action: "upload a file to".into(),
            target: "button".into(),
            site: "example.com".into(),
            outcome: Outcome::Denied,
        })
        .unwrap();
        assert!(json.contains(r#""kind":"approval""#) && json.contains(r#""outcome":"denied""#));
    }

    #[test]
    fn settings_say_when_launch_at_login_is_unavailable() {
        let json = serde_json::to_string(&ServerMsg::Settings(AppSettings {
            version: "0.1.0".into(),
            data_dir: "/d".into(),
            launch_at_login: None,
            you: "Jay".into(),
            farm: true,
        }))
        .unwrap();
        assert!(json.contains(r#""launch_at_login":null"#));
    }

    #[test]
    fn old_user_messages_have_no_sender() {
        let kind: EventKind =
            serde_json::from_str(r#"{"kind":"user_message","text":"hi"}"#).unwrap();
        assert_eq!(
            kind,
            EventKind::UserMessage {
                text: "hi".into(),
                from: None
            }
        );
        let json = serde_json::to_string(&EventKind::UserMessage {
            text: "hi".into(),
            from: Some("Jay".into()),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"user_message","text":"hi","from":"Jay"}"#);
    }

    #[test]
    fn pairing_messages_are_snake_case() {
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"start_pairing","person":"Sam"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::StartPairing {
                person: "Sam".into()
            }
        );
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"remove_gate","id":"g1"}"#).unwrap();
        assert_eq!(msg, ClientMsg::RemoveGate { id: "g1".into() });
        let json = serde_json::to_string(&ServerMsg::PairingClosed).unwrap();
        assert_eq!(json, r#"{"type":"pairing_closed"}"#);
    }

    #[test]
    fn sign_in_messages_are_snake_case() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"sign_in_done","id":2}"#).unwrap();
        assert_eq!(msg, ClientMsg::SignInDone { id: 2 });
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"sign_out_all"}"#).unwrap();
        assert_eq!(msg, ClientMsg::SignOutAll { mine: false });
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"run_setup","action":"install_claude"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::RunSetup {
                action: SetupAction::InstallClaude
            }
        );
        let json = serde_json::to_string(&ServerMsg::Browser {
            bot: "x".into(),
            state: BrowserState::SigningIn,
        })
        .unwrap();
        assert!(json.contains(r#""state":"signing_in""#));
    }

    #[test]
    fn setup_is_ready_only_when_every_check_passes() {
        let check = |ok| SetupCheck {
            id: "chrome".into(),
            name: "Google Chrome".into(),
            ok,
            detail: String::new(),
            fix: None,
            optional: false,
            action: None,
        };
        assert!(
            SetupReport {
                checks: vec![check(true)]
            }
            .ready()
        );
        assert!(
            !SetupReport {
                checks: vec![check(true), check(false)]
            }
            .ready()
        );
    }

    #[test]
    fn client_messages_are_tagged_by_type() {
        let msg = ClientMsg::Send {
            bot: "assistant".into(),
            text: "hi".into(),
        };
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"type":"send","bot":"assistant","text":"hi"}"#
        );
    }

    #[test]
    fn events_flatten_their_kind() {
        let msg = ServerMsg::Event(BotEvent {
            bot: "assistant".into(),
            seq: 3,
            ts_ms: 1,
            kind: EventKind::ToolUse {
                name: "Write".into(),
                summary: "notes.md".into(),
            },
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"event","bot":"assistant","seq":3,"ts_ms":1,"kind":"tool_use","name":"Write","summary":"notes.md"}"#
        );
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }
}
