//! Wire types shared by the oxbot server and its clients (the terminal client
//! and the Android app).
//!
//! Every message that crosses the network is defined here, once, so the
//! server and the apps can never disagree about its shape. Messages are JSON,
//! one per WebSocket text frame, tagged by a `"type"` field.

use std::collections::BTreeMap;

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
    /// with `seq > since`, then streams new ones. If more than [`MAX_PAGE`]
    /// were missed, it sends one [`ServerMsg::History`] of the latest page
    /// instead (a fresh window: drop what you have, and page back with
    /// [`ClientMsg::Older`]), and streams on from its end.
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
    /// Send the user's message to a bot, with any files uploaded for it
    /// (`POST /upload`; see [`Attachment`]). The text may be empty when
    /// there are files.
    Send {
        /// Bot id.
        bot: String,
        /// What the user typed.
        text: String,
        /// The upload ids ([`Attachment::id`]) of the files sent with it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<String>,
    },
    /// Forget a file uploaded for `bot` that won't be sent after all (its
    /// chip was removed): oxbot deletes it.
    DropUpload {
        /// Bot id.
        bot: String,
        /// [`Attachment::id`].
        id: String,
    },
    /// Farm only: open a file sent in a chat on this Mac (in Preview,
    /// Numbers and so on).
    OpenFile {
        /// Bot id.
        bot: String,
        /// The message it was sent with ([`BotEvent::seq`]).
        seq: u64,
        /// [`Attachment::id`].
        id: String,
    },
    /// Farm only: start or stop dictating with this Mac's microphone and
    /// its on-device speech recognition. The text comes back to this
    /// connection only, as [`ServerMsg::Dictation`].
    Dictate {
        /// Start or stop.
        on: bool,
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
        /// The engine it runs on; `None` for the server's default. Only the
        /// Farm's owner may pick Claude.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<EngineKind>,
    },
    /// Switch a bot to another engine. Its maker or the Farm; only the
    /// Farm's owner may switch one to Claude, and a bot others use only
    /// with `confirm_owner_only` (it then takes tasks only from the owner).
    /// Herders' engines are fixed.
    SetEngine {
        /// Bot id.
        bot: String,
        /// The engine.
        engine: EngineKind,
        /// Its model: a Claude alias, or an `OpenRouter` model id; empty for
        /// the default (Auto). `None` keeps the one it has.
        #[serde(default)]
        model: Option<String>,
        /// The person was told the bot becomes owner-only, and said yes.
        #[serde(default)]
        confirm_owner_only: bool,
    },
    /// Ask for Settings → Engines ([`ServerMsg::Engines`]).
    ListEngines,
    /// The Farm's owner has seen the one-time notice that Claude bots take
    /// tasks only from them. The Farm only.
    EnginesNoticeSeen,
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
    /// The "Always allowed" rules of a bot, answered with
    /// [`ServerMsg::AllowRules`]. Only for those who may manage the bot.
    ListAllowRules {
        /// Bot id.
        bot: String,
    },
    /// Removes one of a bot's "Always allowed" rules: the same click asks
    /// again. Only for those who may manage the bot.
    RemoveAllowRule {
        /// Bot id.
        bot: String,
        /// [`AllowRule::id`].
        id: i64,
    },
    /// Open one of a bot's research reports: answered with
    /// [`ServerMsg::Report`] (the page to show), for anyone who may open
    /// the bot.
    OpenReport {
        /// Bot id.
        bot: String,
        /// [`ReportInfo::id`].
        id: String,
        /// The viewer's time zone, in minutes east of UTC, for the dates
        /// in the report (UTC if absent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        utc_offset_min: Option<i32>,
    },
    /// A bot's research reports, answered with [`ServerMsg::Reports`], for
    /// anyone who may open the bot.
    ListReports {
        /// Bot id.
        bot: String,
    },
    /// Delete one of a bot's reports (its card stays in the chat and says
    /// it was deleted). Only for those who may manage the bot; the answer
    /// is a new [`ServerMsg::Reports`].
    DeleteReport {
        /// Bot id.
        bot: String,
        /// [`ReportInfo::id`].
        id: String,
    },
    /// Turn "Launch at login" on or off (desktop app only). Everyone gets
    /// the new [`ServerMsg::Settings`].
    SetLaunchAtLogin {
        /// On or off.
        on: bool,
    },
    /// Farm only: turn the 50% and 80% Claude usage alerts on or off. The
    /// asker gets the new [`ServerMsg::Settings`].
    SetUsageAlerts {
        /// On or off.
        on: bool,
    },
    /// Farm only: turn the Mac notification for a bot waiting for an OK or
    /// a sign-in while oxbot's window is hidden on or off (desktop app
    /// only). The asker gets the new [`ServerMsg::Settings`].
    SetNotifyWhenHidden {
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
    /// Make a sidebar section of your own, last in your list. Sections are
    /// each person's own view: nobody else sees them. Every connection of
    /// yours gets the new [`ServerMsg::Sections`].
    CreateSection {
        /// Its name, e.g. `Kitchen`.
        name: String,
        /// A bot to put in it straight away ("Move to → New section…").
        #[serde(default)]
        bot: Option<String>,
    },
    /// Rename one of your sections.
    RenameSection {
        /// [`SectionInfo::id`].
        id: String,
        /// The new name.
        name: String,
    },
    /// Delete one of your sections. Its bots go back to not being in a
    /// section; no bot is deleted.
    DeleteSection {
        /// [`SectionInfo::id`].
        id: String,
    },
    /// File a bot in one of your sections, or take it out of them. Any bot
    /// you're shown but Herder; it changes only your own view, never the bot.
    MoveBot {
        /// Bot id.
        bot: String,
        /// [`SectionInfo::id`], or `None` for not in a section.
        #[serde(default)]
        section: Option<String>,
    },
}

/// Which engine runs a bot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    /// Claude Code on the Farm owner's own Claude plan: only the owner
    /// gives it tasks.
    #[default]
    Claude,
    /// oxbot's own agent loop on `OpenRouter`'s models: anyone in the
    /// household may give it tasks.
    #[serde(rename = "openrouter")]
    OpenRouter,
}

/// Settings → Engines: which engine does what, and who may use it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnginesView {
    /// The notice everyone sees: Claude bots run on the owner's own plan.
    pub notice: String,
    /// The Farm owner's name.
    pub owner: String,
    /// Whether `OpenRouter` bots can run on this Mac now.
    #[serde(default)]
    pub openrouter_ready: bool,
    /// The Farm only: show the one-time notice as a banner.
    #[serde(default)]
    pub banner: bool,
    /// The Farm only: Claude bots other people used lately, which now take
    /// tasks only from the owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub review: Vec<ReviewBot>,
}

/// A Claude bot other people used, for the owner to review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewBot {
    /// Bot id.
    pub bot: String,
    /// Its name.
    pub name: String,
    /// Who else sent it messages in the last 30 days.
    pub used_by: Vec<String>,
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
        /// Server only, never sent: the owner's person id, which decides
        /// who gets it (names are only shown).
        #[serde(skip)]
        owner_id: Option<String>,
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
    /// Your own sidebar sections and which bots are in them. Sent after
    /// [`ServerMsg::Hello`] (even with none) and when you change them; only
    /// your own connections get it.
    Sections {
        /// In order.
        sections: Vec<SectionInfo>,
        /// Bot id → the id of the section it's in. Bots not in a section
        /// aren't listed.
        placed: BTreeMap<String, String>,
        /// Server only, never sent: whose sections, which decides who gets
        /// them.
        #[serde(skip)]
        owner_id: Option<String>,
    },
    /// Something the user should know that isn't an error.
    Notice {
        /// The message, for the user.
        message: String,
        /// Whose it is, if it's about one person's own browser: only they
        /// (and the Farm) get it. `None` for everyone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        owner: Option<String>,
        /// Server only, never sent: the owner's person id, which decides
        /// who gets it (names are only shown).
        #[serde(skip)]
        owner_id: Option<String>,
    },
    /// A bot's "Always allowed" rules: the answer to
    /// [`ClientMsg::ListAllowRules`], and sent again whenever they change.
    /// Only to clients that may manage the bot.
    AllowRules {
        /// Bot id.
        bot: String,
        /// Newest first.
        rules: Vec<AllowRule>,
    },
    /// A research report to show: the answer to [`ClientMsg::OpenReport`],
    /// for the client that asked.
    Report(ReportView),
    /// A bot's research reports: the answer to [`ClientMsg::ListReports`]
    /// and [`ClientMsg::DeleteReport`].
    Reports {
        /// Bot id.
        bot: String,
        /// Newest first.
        reports: Vec<ReportInfo>,
    },
    /// Claude usage passed 50% or 80% of a plan window. Sent to everyone
    /// once per level per window, and on connect for a window's latest
    /// alert until the window resets. Never sent while alerts are off.
    UsageAlert(UsageAlert),
    /// What the Mac heard so far while dictating ([`ClientMsg::Dictate`]),
    /// for the connection that asked only. Each one replaces the last;
    /// `done` ends it.
    Dictation {
        /// Everything heard since dictation started.
        text: String,
        /// Dictation stopped.
        #[serde(default)]
        done: bool,
        /// Why it stopped early, for the user.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Settings → Engines. The Farm gets all of it; everyone else the
    /// notice and whether `OpenRouter` is ready. The answer to
    /// [`ClientMsg::ListEngines`], and sent again when it changes.
    Engines(EnginesView),
    /// Something the client asked for went wrong.
    Error {
        /// What went wrong, for the user.
        message: String,
    },
}

/// Claude usage passed a level of one of the plan's windows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageAlert {
    /// Claude's name for the window: `five_hour` or `seven_day`.
    pub limit: String,
    /// The level passed: 50 or 80.
    pub level: u8,
    /// How much was used when it passed it, from 0 to 1.
    pub used: f64,
    /// When the window resets (ms since the Unix epoch; 0 if unknown).
    pub resets_at_ms: i64,
    /// The models the bots use, as shown, e.g. `Sonnet 4.5`.
    pub models: Vec<String>,
    /// Ready to show, e.g. `Claude's 5-hour limit is at 52%. Resets in 2 h
    /// 10 min. Your bots use Sonnet 4.5.`
    pub message: String,
}

/// The app-wide settings shown in the Settings panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a wire message: each flag is its own JSON field"
)]
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
    /// Whether oxbot tells the user when Claude usage passes 50% and 80%.
    #[serde(default = "yes")]
    pub usage_alerts: bool,
    /// Whether oxbot posts a Mac notification when a bot waits for an OK
    /// or a sign-in while its window is hidden; `None` when this server
    /// can't tell (it isn't the desktop app).
    #[serde(default)]
    pub notify_when_hidden: Option<bool>,
    /// Whether this client may dictate with the Mac's microphone: the Farm
    /// only, and only where the Mac can recognize speech on-device.
    #[serde(default)]
    pub dictation: bool,
    /// Whether this client is the Farm's owner (the Farm, or the Farm's
    /// person on a Gate): the only one who may give Claude bots tasks.
    #[serde(default)]
    pub owner: bool,
    /// The Farm owner's name, e.g. `Jay`.
    #[serde(default)]
    pub owner_name: String,
    /// Whether `OpenRouter` bots can run on this Mac now.
    #[serde(default)]
    pub openrouter_ready: bool,
}

fn yes() -> bool {
    true
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

/// One of a person's sidebar sections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionInfo {
    /// Its id, e.g. `s_0123456789abcdef`.
    pub id: String,
    /// Its name, e.g. `Kitchen`.
    pub name: String,
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
    /// Server only, never sent: the asker's person id, which decides who
    /// gets the card (the name is only shown).
    #[serde(skip)]
    pub asker_id: Option<String>,
    /// Whether this client is offered [`Answer::AlwaysAllow`]. Decided by
    /// the server for each client.
    #[serde(default, skip_serializing_if = "is_false")]
    pub always: bool,
    /// Server only, never sent: the element an "Always allow" rule would
    /// name (`button "Save"`), set only on cards that may offer one.
    #[serde(skip)]
    pub rule: Option<String>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// One "Always allowed" rule: a bot may do `action` on `label` on `site`
/// without asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowRule {
    /// Id to remove it with.
    pub id: i64,
    /// The exact host, e.g. `www.example.com`.
    pub site: String,
    /// e.g. `click`.
    pub action: String,
    /// The element, e.g. `button "Apply filters"`.
    pub label: String,
    /// Who set it, by name as it is now (`None`: the Farm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// When, in milliseconds since the Unix epoch.
    pub created_ms: i64,
}

/// A research report as a client shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportView {
    /// Bot id.
    pub bot: String,
    /// [`ReportInfo::id`].
    pub id: String,
    /// Its title.
    pub title: String,
    /// When it was saved, in milliseconds since the Unix epoch.
    pub created_ms: i64,
    /// The whole report as one HTML document, made safe by the server: no
    /// scripts, no images, links only to its sources. Shown in a sandboxed
    /// frame.
    pub html: String,
}

/// One research report in a bot's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportInfo {
    /// Its id, e.g. `r_3f…`.
    pub id: String,
    /// Its title.
    pub title: String,
    /// One line about it.
    pub summary: String,
    /// When it was saved, in milliseconds since the Unix epoch.
    pub created_ms: i64,
    /// How many sources it cites.
    pub sources: u32,
}

/// A file sent with a message. It's saved in the bot's folder under
/// `inbox/`, and Claude opens it with its own Read tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Its upload id (random hex).
    pub id: String,
    /// Its name as shown, e.g. `bill.pdf` (cleaned by the server).
    pub name: String,
    /// Its size in bytes, as uploaded.
    pub size: u64,
    /// What kind of file it is.
    pub kind: AttachmentKind,
    /// Where it's saved, relative to the bot's work folder, e.g.
    /// `inbox/2026-11-02/1-bill.pdf`.
    pub path: String,
    /// What Claude reads instead, relative to the work folder: a text copy
    /// of a Word file, a smaller copy of a photo, or a video's folder of
    /// still frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readable: Option<String>,
    /// How many still frames were taken (videos only).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub frames: u32,
    /// A few words about what oxbot did with it, for its chip, e.g. `as
    /// text` or `3 still frames`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What kind of file an [`Attachment`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// A photo or picture: PNG, JPEG, GIF, WebP or HEIC.
    Image,
    /// A PDF.
    Pdf,
    /// Plain text: notes, Markdown, CSV, JSON, code.
    Text,
    /// A Word file (`.docx`, `.doc`), RTF or `.odt` (read as text).
    Document,
    /// A spreadsheet: `.xlsx`, `.xls` or `.ods` (read as text).
    Sheet,
    /// Slides: a `.pptx` deck (read as text).
    Slides,
    /// A video (read as still frames).
    Video,
}

/// The user's answer to an [`Approval`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Do it, this once.
    AllowOnce,
    /// Do it, and from now on let this bot do the same thing on the same
    /// site without asking. Only on cards that offer it
    /// ([`Approval::always`]); the server refuses it on others.
    AlwaysAllow,
    /// Don't.
    Deny,
}

/// What happened to an approval, as stored in the chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The user allowed it.
    Allowed,
    /// The user allowed it, and the same click from now on.
    AllowedAlways,
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
    /// The engine it runs on.
    #[serde(default)]
    pub engine: EngineKind,
    /// Its model as shown, if it has one set (a Claude alias, or a pinned
    /// `OpenRouter` model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Whether the client's person may give it tasks. Decided by the
    /// server for each client (Claude bots: only the Farm's owner).
    #[serde(default = "yes")]
    pub can_task: bool,
    /// An `OpenRouter` bot's own cap on model requests a day, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_requests: Option<u32>,
    /// Who alone may give it tasks: the Farm's person's id for a Claude
    /// bot (empty: only the Farm connection), `None` for anyone who sees
    /// it. For the server's checks. Never sent.
    #[serde(skip)]
    pub task_owner: Option<String>,
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
        /// What the user typed (may be empty when files were sent).
        text: String,
        /// Who sent it, e.g. `Jayadev` (missing in old history).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        /// The files sent with it (missing in old history).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<Attachment>,
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
    /// A card wasn't needed: one of the user's "Always allowed" rules
    /// let it through (shown as a quiet line).
    AllowedByRule {
        /// The action, e.g. `click`.
        action: String,
        /// What it acted on, e.g. `button "Apply filters"`.
        target: String,
        /// The site.
        site: String,
    },
    /// The bot saved a research report: a card in the chat that opens it
    /// (see [`ClientMsg::OpenReport`]). The event's time is the report's.
    Report {
        /// [`ReportInfo::id`].
        id: String,
        /// Its title.
        title: String,
        /// One line about it.
        summary: String,
        /// How many sources it cites.
        sources: u32,
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
    fn engines_ride_along_and_who_may_task_stays_on_the_server() {
        let old: BotInfo = serde_json::from_str(
            r#"{"id":"a","name":"A","busy":false,"last_seq":0,"browser_state":"off"}"#,
        )
        .unwrap();
        assert!(old.can_task, "an older server let everyone task");
        assert_eq!(old.engine, EngineKind::Claude);
        let info = BotInfo {
            id: "a".into(),
            engine: EngineKind::OpenRouter,
            task_owner: Some("p_0123456789abcdef".into()),
            ..BotInfo::default()
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""engine":"openrouter""#), "{json}");
        assert!(
            !json.contains("p_0123") && !json.contains("task_owner"),
            "{json}"
        );

        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"set_engine","bot":"a","engine":"claude"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::SetEngine {
                bot: "a".into(),
                engine: EngineKind::Claude,
                model: None,
                confirm_owner_only: false,
            }
        );
        let msg: ClientMsg = serde_json::from_str(
            r#"{"type":"create_bot","name":"X","instructions":"","engine":"openrouter"}"#,
        )
        .unwrap();
        assert!(matches!(
            msg,
            ClientMsg::CreateBot {
                engine: Some(EngineKind::OpenRouter),
                ..
            }
        ));
        for (msg, tag) in [
            (ClientMsg::ListEngines, "list_engines"),
            (ClientMsg::EnginesNoticeSeen, "engines_notice_seen"),
        ] {
            assert_eq!(
                serde_json::to_string(&msg).unwrap(),
                format!(r#"{{"type":"{tag}"}}"#)
            );
        }
        let view = ServerMsg::Engines(EnginesView {
            notice: "n".into(),
            owner: "Jay".into(),
            openrouter_ready: false,
            banner: false,
            review: Vec::new(),
        });
        let json = serde_json::to_string(&view).unwrap();
        assert!(json.starts_with(r#"{"type":"engines","#), "{json}");
        assert!(!json.contains("review"), "{json}");
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
                engine: None,
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
            owner_id: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"sites","sites":[]}"#);
    }

    #[test]
    fn person_ids_never_leave_the_server() {
        let json = serde_json::to_string(&ServerMsg::Sites {
            sites: vec![],
            owner: Some("Sam".into()),
            owner_id: Some("p_0123456789abcdef".into()),
        })
        .unwrap();
        assert_eq!(json, r#"{"type":"sites","sites":[],"owner":"Sam"}"#);
        let json = serde_json::to_string(&ServerMsg::Notice {
            message: "Hi".into(),
            owner: None,
            owner_id: Some("p_0123456789abcdef".into()),
        })
        .unwrap();
        assert!(!json.contains("p_0123"), "{json}");
        let card = Approval {
            id: 1,
            bot: "b".into(),
            kind: ApprovalKind::Change,
            action: "click".into(),
            target: "Buy".into(),
            site: "shop.example".into(),
            page: None,
            detail: None,
            screenshot: None,
            asker: Some("Sam".into()),
            asker_id: Some("p_0123456789abcdef".into()),
            always: false,
            rule: Some("button \"Buy\"".into()),
        };
        let json = serde_json::to_string(&card).unwrap();
        assert!(
            !json.contains("p_0123") && json.contains("\"asker\":\"Sam\""),
            "{json}"
        );
        assert!(
            !json.contains("always") && !json.contains("rule"),
            "the rule stays on the server and a card that doesn't offer Always says nothing: {json}"
        );
        let back: Approval = serde_json::from_str(&json).unwrap();
        assert_eq!((back.asker_id, back.rule, back.always), (None, None, false));
        let offered = serde_json::to_string(&Approval {
            always: true,
            ..card
        })
        .unwrap();
        assert!(offered.contains(r#""always":true"#), "{offered}");
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
        let always: ClientMsg =
            serde_json::from_str(r#"{"type":"answer","id":4,"answer":"always_allow"}"#).unwrap();
        assert_eq!(
            always,
            ClientMsg::Answer {
                id: 4,
                answer: Answer::AlwaysAllow
            }
        );
        assert_eq!(
            serde_json::to_string(&Outcome::AllowedAlways).unwrap(),
            r#""allowed_always""#
        );
    }

    #[test]
    fn allow_rules_go_back_and_forth() {
        let list: ClientMsg =
            serde_json::from_str(r#"{"type":"list_allow_rules","bot":"shop"}"#).unwrap();
        assert_eq!(list, ClientMsg::ListAllowRules { bot: "shop".into() });
        let remove: ClientMsg =
            serde_json::from_str(r#"{"type":"remove_allow_rule","bot":"shop","id":3}"#).unwrap();
        assert_eq!(
            remove,
            ClientMsg::RemoveAllowRule {
                bot: "shop".into(),
                id: 3
            }
        );
        let json = serde_json::to_string(&ServerMsg::AllowRules {
            bot: "shop".into(),
            rules: vec![AllowRule {
                id: 3,
                site: "www.example.com".into(),
                action: "click".into(),
                label: "button \"Save\"".into(),
                by: None,
                created_ms: 5,
            }],
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"type":"allow_rules","bot":"shop","rules":[{"id":3,"site":"www.example.com","action":"click","label":"button \"Save\"","created_ms":5}]}"#
        );
        let line = serde_json::to_string(&EventKind::AllowedByRule {
            action: "click".into(),
            target: "button \"Save\"".into(),
            site: "www.example.com".into(),
        })
        .unwrap();
        assert!(line.contains(r#""kind":"allowed_by_rule""#), "{line}");
    }

    #[test]
    fn reports_go_back_and_forth() {
        let open: ClientMsg =
            serde_json::from_str(r#"{"type":"open_report","bot":"cafes","id":"r_1"}"#).unwrap();
        assert_eq!(
            open,
            ClientMsg::OpenReport {
                bot: "cafes".into(),
                id: "r_1".into(),
                utc_offset_min: None
            }
        );
        let local: ClientMsg = serde_json::from_str(
            r#"{"type":"open_report","bot":"cafes","id":"r_1","utc_offset_min":-300}"#,
        )
        .unwrap();
        assert!(matches!(
            local,
            ClientMsg::OpenReport {
                utc_offset_min: Some(-300),
                ..
            }
        ));
        let list: ClientMsg =
            serde_json::from_str(r#"{"type":"list_reports","bot":"cafes"}"#).unwrap();
        assert_eq!(
            list,
            ClientMsg::ListReports {
                bot: "cafes".into()
            }
        );
        let delete: ClientMsg =
            serde_json::from_str(r#"{"type":"delete_report","bot":"cafes","id":"r_1"}"#).unwrap();
        assert_eq!(
            delete,
            ClientMsg::DeleteReport {
                bot: "cafes".into(),
                id: "r_1".into()
            }
        );
        let view = ServerMsg::Report(ReportView {
            bot: "cafes".into(),
            id: "r_1".into(),
            title: "Coffee".into(),
            created_ms: 5,
            html: "<p>x</p>".into(),
        });
        let json = serde_json::to_string(&view).unwrap();
        assert_eq!(
            json,
            r#"{"type":"report","bot":"cafes","id":"r_1","title":"Coffee","created_ms":5,"html":"<p>x</p>"}"#
        );
        assert_eq!(serde_json::from_str::<ServerMsg>(&json).unwrap(), view);
        let event = serde_json::to_string(&BotEvent {
            bot: "cafes".into(),
            seq: 4,
            ts_ms: 5,
            kind: EventKind::Report {
                id: "r_1".into(),
                title: "Coffee".into(),
                summary: "Cafe A wins.".into(),
                sources: 2,
            },
        })
        .unwrap();
        assert_eq!(
            event,
            r#"{"bot":"cafes","seq":4,"ts_ms":5,"kind":"report","id":"r_1","title":"Coffee","summary":"Cafe A wins.","sources":2}"#
        );
    }

    #[test]
    fn settings_say_when_launch_at_login_is_unavailable() {
        let json = serde_json::to_string(&ServerMsg::Settings(AppSettings {
            version: "0.1.0".into(),
            data_dir: "/d".into(),
            launch_at_login: None,
            you: "Jay".into(),
            farm: true,
            usage_alerts: true,
            notify_when_hidden: None,
            dictation: false,
            owner: true,
            owner_name: "Jay".into(),
            openrouter_ready: false,
        }))
        .unwrap();
        assert!(json.contains(r#""launch_at_login":null"#));
        assert!(json.contains(r#""notify_when_hidden":null"#));
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        let ServerMsg::Settings(back) = back else {
            panic!("settings")
        };
        assert_eq!(back.notify_when_hidden, None);
    }

    #[test]
    fn settings_from_an_older_server_have_usage_alerts_on() {
        let settings: AppSettings = serde_json::from_str(
            r#"{"version":"0.1.0","data_dir":"/d","launch_at_login":null,"you":"Jay","farm":true}"#,
        )
        .unwrap();
        assert!(settings.usage_alerts);
        assert_eq!(settings.notify_when_hidden, None);
    }

    #[test]
    fn a_usage_alert_is_tagged_and_flat() {
        let json = serde_json::to_string(&ServerMsg::UsageAlert(UsageAlert {
            limit: "five_hour".into(),
            level: 50,
            used: 0.52,
            resets_at_ms: 1000,
            models: vec!["Sonnet 4.5".into()],
            message: "Claude's 5-hour limit is at 52%.".into(),
        }))
        .unwrap();
        assert_eq!(
            json,
            r#"{"type":"usage_alert","limit":"five_hour","level":50,"used":0.52,"resets_at_ms":1000,"models":["Sonnet 4.5"],"message":"Claude's 5-hour limit is at 52%."}"#
        );
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"set_usage_alerts","on":false}"#).unwrap();
        assert_eq!(msg, ClientMsg::SetUsageAlerts { on: false });
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"set_notify_when_hidden","on":true}"#).unwrap();
        assert_eq!(msg, ClientMsg::SetNotifyWhenHidden { on: true });
    }

    #[test]
    fn old_user_messages_have_no_sender() {
        let kind: EventKind =
            serde_json::from_str(r#"{"kind":"user_message","text":"hi"}"#).unwrap();
        assert_eq!(
            kind,
            EventKind::UserMessage {
                text: "hi".into(),
                from: None,
                files: vec![]
            }
        );
        let json = serde_json::to_string(&EventKind::UserMessage {
            text: "hi".into(),
            from: Some("Jay".into()),
            files: vec![],
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"user_message","text":"hi","from":"Jay"}"#);
    }

    #[test]
    fn files_ride_along_with_a_message_and_old_clients_still_fit() {
        // An old client's send, and old history, have no files.
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"send","bot":"a","text":"hi"}"#).unwrap();
        assert!(matches!(msg, ClientMsg::Send { ref files, .. } if files.is_empty()));
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"send","bot":"a","text":"","files":["f1"]}"#).unwrap();
        assert!(matches!(msg, ClientMsg::Send { ref files, .. } if files == &["f1"]));

        let sent = EventKind::UserMessage {
            text: "what's due?".into(),
            from: Some("Jay".into()),
            files: vec![Attachment {
                id: "0a1b".into(),
                name: "bill.pdf".into(),
                size: 1234,
                kind: AttachmentKind::Pdf,
                path: "inbox/2026-11-02/1-bill.pdf".into(),
                readable: None,
                frames: 0,
                note: None,
            }],
        };
        let json = serde_json::to_string(&sent).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"user_message","text":"what's due?","from":"Jay","files":[{"id":"0a1b","name":"bill.pdf","size":1234,"kind":"pdf","path":"inbox/2026-11-02/1-bill.pdf"}]}"#
        );
        assert_eq!(serde_json::from_str::<EventKind>(&json).unwrap(), sent);
        let video = Attachment {
            kind: AttachmentKind::Video,
            readable: Some("inbox/2026-11-02/2-clip.mov.frames".into()),
            frames: 3,
            note: Some("3 still frames".into()),
            ..match sent {
                EventKind::UserMessage { mut files, .. } => files.remove(0),
                _ => unreachable!(),
            }
        };
        let json = serde_json::to_string(&video).unwrap();
        assert!(
            json.contains(r#""frames":3"#) && json.contains(r#""kind":"video""#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Attachment>(&json).unwrap(), video);
    }

    #[test]
    fn dictation_and_opening_files_are_snake_case() {
        let msg: ClientMsg = serde_json::from_str(r#"{"type":"dictate","on":true}"#).unwrap();
        assert_eq!(msg, ClientMsg::Dictate { on: true });
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"open_file","bot":"a","seq":4,"id":"f1"}"#).unwrap();
        assert!(matches!(msg, ClientMsg::OpenFile { seq: 4, .. }));
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"drop_upload","bot":"a","id":"f1"}"#).unwrap();
        assert!(matches!(msg, ClientMsg::DropUpload { .. }));
        let json = serde_json::to_string(&ServerMsg::Dictation {
            text: "call the plumber".into(),
            done: false,
            error: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"type":"dictation","text":"call the plumber","done":false}"#
        );
        let old: AppSettings =
            serde_json::from_str(r#"{"version":"1","data_dir":"/d","launch_at_login":null}"#)
                .unwrap();
        assert!(!old.dictation, "no mic unless the server says so");
    }

    #[test]
    fn sections_go_without_whose_they_are() {
        let msg = ServerMsg::Sections {
            sections: vec![SectionInfo {
                id: "s_1".into(),
                name: "Kitchen".into(),
            }],
            placed: [("recipes".to_owned(), "s_1".to_owned())].into(),
            owner_id: Some("p_0123456789abcdef".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"sections","sections":[{"id":"s_1","name":"Kitchen"}],"placed":{"recipes":"s_1"}}"#
        );
        let back: ServerMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ServerMsg::Sections { owner_id: None, .. }));
    }

    #[test]
    fn section_messages_are_snake_case_with_optional_parts() {
        let out = ClientMsg::MoveBot {
            bot: "recipes".into(),
            section: None,
        };
        assert_eq!(
            serde_json::to_string(&out).unwrap(),
            r#"{"type":"move_bot","bot":"recipes","section":null}"#
        );
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"move_bot","bot":"recipes"}"#).unwrap();
        assert_eq!(msg, out, "no section means out of sections");
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"create_section","name":"Kitchen"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::CreateSection {
                name: "Kitchen".into(),
                bot: None
            }
        );
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"rename_section","id":"s_1","name":"Home"}"#).unwrap();
        assert!(matches!(msg, ClientMsg::RenameSection { .. }));
        let msg: ClientMsg =
            serde_json::from_str(r#"{"type":"delete_section","id":"s_1"}"#).unwrap();
        assert_eq!(msg, ClientMsg::DeleteSection { id: "s_1".into() });
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
            files: vec![],
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
