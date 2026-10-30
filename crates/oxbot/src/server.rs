//! `oxbot serve`: a WebSocket server in front of the [`Hub`].
//!
//! Each connection gets a [`ServerMsg::Hello`], then speaks [`ClientMsg`] /
//! [`ServerMsg`] JSON, one message per text frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use oxbot_core::{Hub, Viewer};
use oxbot_protocol::{
    AppSettings, ClientMsg, LogLine, ServerHello, ServerMsg, SetupAction, SetupReport,
};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

use crate::logs::LogRelay;

/// Shared state for every connection.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Runs the bots.
    pub hub: Arc<Hub>,
    /// Name shown to clients, e.g. the Mac's name.
    pub server_name: String,
    /// When the server started (ms since the Unix epoch).
    pub started_ms: i64,
    /// The server's log, relayed to clients.
    pub logs: Option<Arc<LogRelay>>,
    /// Dev mode: serve the page from this folder on every request instead of
    /// the copy baked into the binary, so page edits show on refresh.
    pub ui_dir: Option<PathBuf>,
    /// oxbot's data folder, shown in Settings.
    pub data_dir: PathBuf,
    /// What the desktop app lets the page control (`None` for `oxbot serve`).
    pub app: Option<Arc<dyn AppControls>>,
    /// Who messages from this Mac come from (the Mac account's first name).
    /// Phones will send as the name they were paired with.
    pub local_person: String,
    /// The `PATH` oxbot's programs use (see [`crate::setup::search_path`]).
    pub search_path: String,
    /// The latest setup check.
    pub setup: Arc<std::sync::RwLock<SetupReport>>,
    /// Whether the browser tool (and the Node.js it needs) ships inside the
    /// app, so the setup check doesn't ask for Node.js.
    pub tool_built_in: bool,
    /// Set while a setup step (install, sign-in) runs, so only one runs.
    pub setup_running: Arc<std::sync::atomic::AtomicBool>,
    /// End-to-end test mode: a connection may say who it is with `?as=Name`
    /// (always a Gate person, never the Farm).
    pub e2e: bool,
}

/// Settings only the desktop app can change, such as "Launch at login".
pub trait AppControls: Send + Sync + std::fmt::Debug {
    /// Whether oxbot starts at login, if it can tell.
    fn launch_at_login(&self) -> Option<bool>;
    /// Turns starting at login on or off.
    ///
    /// # Errors
    /// Says why it couldn't.
    fn set_launch_at_login(&self, on: bool) -> Result<(), String>;
}

impl AppState {
    /// The app's settings right now, for the Farm's own person.
    #[must_use]
    pub fn settings(&self) -> AppSettings {
        self.settings_for(&self.farm_viewer())
    }

    /// The app's settings as `viewer` gets them.
    #[must_use]
    pub fn settings_for(&self, viewer: &Viewer) -> AppSettings {
        AppSettings {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            data_dir: self.data_dir.display().to_string(),
            launch_at_login: self.app.as_ref().and_then(|a| a.launch_at_login()),
            you: viewer.name.clone(),
            farm: viewer.farm,
        }
    }

    /// Who a connection from this Mac is: the Farm's own person. (Gates
    /// will connect as the person they were paired with.)
    #[must_use]
    pub fn farm_viewer(&self) -> Viewer {
        Viewer::farm(self.local_person.clone())
    }
}

impl AppState {
    /// Runs the setup check again and keeps the result.
    pub async fn refresh_setup(&self) -> SetupReport {
        let report = crate::setup::check(&self.search_path, self.tool_built_in).await;
        *self
            .setup
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = report.clone();
        if !report.ready() {
            let missing: Vec<&str> = report
                .checks
                .iter()
                .filter(|c| !c.ok)
                .map(|c| c.name.as_str())
                .collect();
            tracing::warn!("setup: missing {}", missing.join(", "));
        }
        report
    }

    fn setup_report(&self) -> SetupReport {
        self.setup
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// State with no log relay and the built-in page.
    #[must_use]
    pub fn new(hub: Arc<Hub>, server_name: impl Into<String>) -> Self {
        Self {
            hub,
            server_name: server_name.into(),
            started_ms: 0,
            logs: None,
            ui_dir: None,
            data_dir: PathBuf::new(),
            app: None,
            local_person: "Me".to_owned(),
            search_path: std::env::var("PATH").unwrap_or_default(),
            setup: Arc::default(),
            tool_built_in: false,
            setup_running: Arc::default(),
            e2e: false,
        }
    }
}

/// The chat page, baked into the binary.
const INDEX_HTML: &str = include_str!("../ui/index.html");

/// The HTTP routes: the chat page at `/` and the WebSocket at `/ws`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
}

/// Serves until `shutdown` resolves.
///
/// # Errors
/// Fails if the server stops with an I/O error.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

async fn index(State(state): State<AppState>) -> Html<String> {
    if let Some(dir) = &state.ui_dir {
        match tokio::fs::read_to_string(dir.join("index.html")).await {
            Ok(page) => return Html(page),
            Err(err) => tracing::warn!(%err, "dev page not readable; serving the built-in one"),
        }
    }
    Html(INDEX_HTML.to_owned())
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Response {
    // Browsers send an Origin header and let any web page open WebSockets to
    // localhost. Only the oxbot page itself may connect; programs without an
    // Origin (the terminal client, the phone app) are let through.
    if let Some(origin) = headers.get(header::ORIGIN)
        && !is_local_origin(origin.to_str().unwrap_or(""))
    {
        tracing::warn!(?origin, "refused a WebSocket from another website");
        return (StatusCode::FORBIDDEN, "oxbot only accepts its own page").into_response();
    }
    let viewer = match query.get("as").map(|n| n.trim()) {
        Some(name) if state.e2e && !name.is_empty() => Viewer::gate(name),
        _ => state.farm_viewer(),
    };
    ws.on_upgrade(move |socket| connection(socket, state, viewer))
}

/// Whether a browser Origin is the oxbot page on this Mac.
fn is_local_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(':').next().unwrap_or("");
    matches!(host, "127.0.0.1" | "localhost") && !rest.contains('/')
}

/// What one connection may see and has subscribed to.
#[derive(Debug)]
struct Client {
    /// Who's connected.
    viewer: Viewer,
    /// The bots they may see, kept up to date from bot-list updates.
    visible: std::collections::HashSet<String>,
    /// Bot id → last `seq` sent.
    subs: HashMap<String, u64>,
}

impl Client {
    fn new(viewer: Viewer, bots: &[oxbot_protocol::BotInfo]) -> Self {
        let mut client = Self {
            viewer,
            visible: std::collections::HashSet::new(),
            subs: HashMap::new(),
        };
        client.see(bots);
        client
    }

    /// Takes a new bot list: remembers which bots this client may see, and
    /// drops subscriptions to the rest.
    fn see(&mut self, bots: &[oxbot_protocol::BotInfo]) {
        self.visible = bots
            .iter()
            .filter(|b| self.viewer.can_see(b.created_by.as_deref(), b.private))
            .map(|b| b.id.clone())
            .collect();
        let visible = &self.visible;
        self.subs.retain(|bot, _| visible.contains(bot));
    }

    fn sees(&self, bot: &str) -> bool {
        self.visible.contains(bot)
    }
}

async fn connection(socket: WebSocket, state: AppState, viewer: Viewer) {
    let (mut sink, mut stream) = socket.split();
    // Subscribe before replaying history, so nothing falls in the gap.
    let mut updates = state.hub.subscribe();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(64);
    let mut client = Client::new(viewer, &state.hub.bots().await.unwrap_or_default());

    let mut logs = state.logs.as_ref().map(|l| l.subscribe());
    let mut first = vec![
        ServerMsg::Hello(ServerHello::new(&state.server_name, state.started_ms)),
        ServerMsg::Settings(state.settings_for(&client.viewer)),
    ];
    first.push(ServerMsg::Setup(state.setup_report()));
    if let Ok(usage) = state.hub.usage() {
        first.extend(forward(ServerMsg::Usage(usage), &mut client));
    }
    if let Ok(people) = state.hub.people() {
        first.push(ServerMsg::People { people });
    }
    first.extend(state.hub.sites_for(&client.viewer));
    // The log names every bot, site and person: the Farm's only.
    if let Some(relay) = state.logs.as_ref().filter(|_| client.viewer.farm) {
        first.extend(relay.recent().into_iter().map(ServerMsg::Log));
    }
    first.extend(
        state
            .hub
            .pending_approvals()
            .into_iter()
            .filter(|a| client.sees(&a.bot))
            .map(ServerMsg::ApprovalNeeded),
    );
    first.extend(
        state
            .hub
            .pending_sign_ins()
            .into_iter()
            .filter(|r| client.sees(&r.bot))
            .map(ServerMsg::SignInNeeded),
    );
    for msg in &first {
        if send(&mut sink, msg).await.is_err() {
            return;
        }
    }

    loop {
        let outgoing = tokio::select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(msg) => handle(msg, &state, &mut client, &out_tx).await,
                    Err(err) => vec![ServerMsg::Error { message: format!("bad message: {err}") }],
                },
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => continue, // pings, binary: ignore
            },
            update = updates.recv() => match update {
                Ok(msg) => forward(msg, &mut client).into_iter().collect(),
                Err(broadcast::error::RecvError::Lagged(n)) => vec![ServerMsg::Error {
                    message: format!("missed {n} updates; subscribe again to catch up"),
                }],
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Some(msg) = out_rx.recv() => vec![msg],
            Some(line) = next_log(logs.as_mut()) => forward(ServerMsg::Log(line), &mut client).into_iter().collect(),
        };
        for msg in outgoing {
            if send(&mut sink, &msg).await.is_err() {
                return;
            }
        }
    }
}

/// The next relayed log line; never resolves without a relay. A client that
/// falls behind skips the lines it missed.
async fn next_log(rx: Option<&mut broadcast::Receiver<LogLine>>) -> Option<LogLine> {
    let Some(rx) = rx else {
        return std::future::pending().await;
    };
    loop {
        match rx.recv().await {
            Ok(line) => return Some(line),
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => return std::future::pending().await,
        }
    }
}

/// Handles one client message; returns what to send back right away.
async fn handle(
    msg: ClientMsg,
    state: &AppState,
    client: &mut Client,
    out: &mpsc::Sender<ServerMsg>,
) -> Vec<ServerMsg> {
    let viewer = client.viewer.clone();
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    match msg {
        ClientMsg::ListBots => match state.hub.bots().await {
            Ok(bots) => {
                client.see(&bots);
                forward(ServerMsg::Bots { bots }, client)
                    .into_iter()
                    .collect()
            }
            Err(e) => error(&e),
        },
        ClientMsg::Subscribe { bot, since } => match state.hub.history_for(&bot, since, &viewer) {
            Ok(events) => {
                let last = events.last().map_or(since, |e| e.seq);
                client.visible.insert(bot.clone());
                client.subs.insert(bot, last);
                events.into_iter().map(ServerMsg::Event).collect()
            }
            Err(e) => error(&e),
        },
        ClientMsg::Send { bot, text } => {
            if let Err(e) = state.hub.bot_for(&bot, &viewer) {
                return error(&e);
            }
            // Sending may wait for a free turn slot; don't block this connection.
            tokio::spawn(send_as(
                Arc::clone(&state.hub),
                bot,
                text,
                viewer.name.clone(),
                out.clone(),
            ));
            Vec::new()
        }
        ClientMsg::Cancel { bot } => match state.hub.bot_for(&bot, &viewer) {
            Ok(_) => reply(state.hub.cancel(&bot).await),
            Err(e) => error(&e),
        },
        ClientMsg::CreateBot {
            name,
            instructions,
            private,
        } => {
            match state
                .hub
                .create_bot_with(&name, &instructions, Some(&viewer.name), private)
                .await
            {
                Ok(info) => {
                    client.visible.insert(info.id.clone());
                    vec![ServerMsg::BotCreated(info)]
                }
                Err(e) => error(&e),
            }
        }
        ClientMsg::SetPrivate { bot, private } => {
            reply(state.hub.set_private(&bot, private, &viewer).await)
        }
        ClientMsg::RenameBot { bot, name } => {
            reply(state.hub.rename_bot(&bot, &name, &viewer).await)
        }
        ClientMsg::DeleteBot { bot } => reply(state.hub.delete_bot(&bot, &viewer).await),
        ClientMsg::Answer { id, answer } => {
            if state.hub.approval_visible(id, &viewer) {
                state.hub.answer(id, answer);
            }
            Vec::new()
        }
        ClientMsg::SetLaunchAtLogin { on } => {
            let Some(app) = &state.app else {
                return error(&"Launch at login is only in the oxbot app.");
            };
            match app.set_launch_at_login(on) {
                Ok(()) => vec![ServerMsg::Settings(state.settings_for(&viewer))],
                Err(e) => error(&e),
            }
        }
        ClientMsg::CheckSetup => vec![ServerMsg::Setup(state.refresh_setup().await)],
        ClientMsg::RunSetup { action } => {
            if state
                .setup_running
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return vec![ServerMsg::Notice {
                    message: "A setup step is already running.".to_owned(),
                    owner: None,
                }];
            }
            tokio::spawn(run_setup(state.clone(), action));
            Vec::new()
        }
        other => handle_sign_ins(other, state, &viewer).await,
    }
}

/// Sends `text` to `bot` from `from`, reporting a failure to the client.
async fn send_as(
    hub: Arc<Hub>,
    bot: String,
    text: String,
    from: String,
    out: mpsc::Sender<ServerMsg>,
) {
    if let Err(e) = hub.send(&bot, &text, Some(&from)).await {
        let _ = out
            .send(ServerMsg::Error {
                message: e.to_string(),
            })
            .await;
    }
}

/// Handles sign-in cards and the sign-in lists. A card for a bot the
/// viewer can't see is ignored; "mine" means the viewer's own browser,
/// else the home one.
async fn handle_sign_ins(msg: ClientMsg, state: &AppState, viewer: &Viewer) -> Vec<ServerMsg> {
    let owner = |mine: bool| {
        if mine {
            viewer.name.clone()
        } else {
            oxbot_core::store::HOME.to_owned()
        }
    };
    // The home browser holds everyone's shared sign-ins: only the Farm
    // changes it or opens its sites. Anyone may use their own.
    let home_only_farm = |mine: bool| {
        (!mine && !viewer.farm).then(|| ServerMsg::Error {
            message: "Only the Farm can change the home browser's sign-ins.".to_owned(),
        })
    };
    match msg {
        ClientMsg::SignOutAll { mine }
        | ClientMsg::OpenSite { mine, .. }
        | ClientMsg::AddSignIn { mine, .. }
        | ClientMsg::SignOutSite { mine, .. }
            if home_only_farm(mine).is_some() =>
        {
            home_only_farm(mine).into_iter().collect()
        }
        ClientMsg::OpenSignIn { id } if state.hub.sign_in_visible(id, viewer) => {
            reply(state.hub.open_sign_in(id).await)
        }
        ClientMsg::SignInDone { id } if state.hub.sign_in_visible(id, viewer) => {
            state.hub.resolve_sign_in(id, true);
            Vec::new()
        }
        ClientMsg::CancelSignIn { id } if state.hub.sign_in_visible(id, viewer) => {
            state.hub.resolve_sign_in(id, false);
            Vec::new()
        }
        ClientMsg::SignOutAll { mine } => reply(state.hub.sign_out_all(&owner(mine)).await),
        ClientMsg::OpenSite { site, mine } => reply(state.hub.open_site(&site, &owner(mine)).await),
        ClientMsg::AddSignIn { url, mine } => {
            reply(state.hub.add_sign_in(&url, &owner(mine)).await)
        }
        ClientMsg::SignOutSite { site, mine } => {
            reply(state.hub.sign_out_site(&site, &owner(mine)).await)
        }
        _ => Vec::new(),
    }
}

/// Nothing to send back if it worked, else the error.
fn reply(result: Result<(), oxbot_core::HubError>) -> Vec<ServerMsg> {
    match result {
        Ok(()) => Vec::new(),
        Err(e) => vec![ServerMsg::Error {
            message: e.to_string(),
        }],
    }
}

/// Runs a setup step for the user, telling every client how it goes, then
/// checks setup again.
async fn run_setup(state: AppState, action: SetupAction) {
    let tell = |text: String, running: bool| {
        state.hub.announce(ServerMsg::SetupProgress {
            action,
            text,
            running,
        });
    };
    tracing::info!(?action, "setup step started");
    tell("Starting…".to_owned(), true);
    let result = crate::setup::run(action, &state.search_path, |line| tell(line, true)).await;
    match &result {
        Ok(()) => tell("Done.".to_owned(), false),
        Err(why) => {
            tracing::warn!(?action, why, "setup step failed");
            tell(why.clone(), false);
        }
    }
    let report = state.refresh_setup().await;
    state.hub.announce(ServerMsg::Setup(report));
    state
        .setup_running
        .store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Passes a live update on if this client may see it: never anything about
/// a bot hidden from them, nor another person's sign-ins. Events and reply
/// pieces go only to clients subscribed to that bot (and not twice).
fn forward(msg: ServerMsg, client: &mut Client) -> Option<ServerMsg> {
    match msg {
        ServerMsg::Event(e) => {
            let last = client.subs.get_mut(&e.bot)?;
            if e.seq <= *last {
                return None;
            }
            *last = e.seq;
            Some(ServerMsg::Event(e))
        }
        ServerMsg::Delta { ref bot, .. } => client.subs.contains_key(bot).then_some(msg),
        ServerMsg::Bots { bots } => {
            client.see(&bots);
            Some(ServerMsg::Bots {
                bots: client.viewer.redact(bots),
            })
        }
        ServerMsg::BotCreated(ref b) if !client.sees(&b.id) => None,
        ServerMsg::Activity { ref bot, .. } | ServerMsg::Browser { ref bot, .. } => {
            client.sees(bot).then_some(msg)
        }
        ServerMsg::ApprovalNeeded(ref a) => client.sees(&a.bot).then_some(msg),
        ServerMsg::SignInNeeded(ref r) => client.sees(&r.bot).then_some(msg),
        ServerMsg::Sites { ref owner, .. } => (owner.is_none()
            || owner.as_deref() == Some(client.viewer.name.as_str()))
        .then_some(msg),
        ServerMsg::Usage(mut usage) => {
            usage.bots.retain(|b| client.sees(&b.bot));
            Some(ServerMsg::Usage(usage))
        }
        // The log names every bot, site and person: the Farm's only.
        ServerMsg::Log(_) => client.viewer.farm.then_some(msg),
        ServerMsg::Notice { ref owner, .. } => (owner.is_none()
            || client.viewer.farm
            || owner.as_deref() == Some(client.viewer.name.as_str()))
        .then_some(msg),
        // Nothing about any one bot or person. Every variant is listed, so a
        // new one has to decide who gets it.
        ServerMsg::Hello(_)
        | ServerMsg::Settings(_)
        | ServerMsg::BotCreated(_)
        | ServerMsg::ApprovalResolved { .. }
        | ServerMsg::SignInResolved { .. }
        | ServerMsg::Setup(_)
        | ServerMsg::SetupProgress { .. }
        | ServerMsg::People { .. }
        | ServerMsg::Error { .. } => Some(msg),
    }
}

async fn send<S>(sink: &mut S, msg: &ServerMsg) -> Result<(), S::Error>
where
    S: SinkExt<Message> + Unpin,
{
    // Our own types always serialize.
    let text = serde_json::to_string(msg).unwrap_or_default();
    sink.send(Message::Text(text.into())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_protocol::{BotEvent, EventKind};

    fn event(bot: &str, seq: u64) -> ServerMsg {
        ServerMsg::Event(BotEvent {
            bot: bot.into(),
            seq,
            ts_ms: 0,
            kind: EventKind::UserMessage {
                text: String::new(),
                from: None,
            },
        })
    }

    #[test]
    fn only_the_local_page_may_open_a_websocket() {
        assert!(is_local_origin("http://127.0.0.1:7878"));
        assert!(is_local_origin("http://localhost:7878"));
        assert!(!is_local_origin("https://evil.example"));
        assert!(!is_local_origin("http://127.0.0.1.evil.example"));
        assert!(!is_local_origin("http://localhost.evil.example:80"));
        assert!(!is_local_origin("null"));
    }

    fn info(id: &str, created_by: Option<&str>, private: bool) -> oxbot_protocol::BotInfo {
        oxbot_protocol::BotInfo {
            id: id.into(),
            name: id.into(),
            busy: false,
            last_seq: 0,
            last_reply_seq: 0,
            browser_state: oxbot_protocol::BrowserState::Off,
            central: false,
            created_by: created_by.map(Into::into),
            private,
        }
    }

    #[test]
    fn forward_skips_other_bots_and_replayed_events() {
        let mut client = Client::new(Viewer::farm("Jay"), &[]);
        client.subs.insert("a".to_owned(), 2);
        assert!(
            forward(event("b", 1), &mut client).is_none(),
            "not subscribed"
        );
        assert!(
            forward(event("a", 2), &mut client).is_none(),
            "already replayed"
        );
        assert!(forward(event("a", 3), &mut client).is_some());
        assert_eq!(client.subs["a"], 3);
    }

    #[test]
    fn the_log_is_the_farms_and_a_persons_notice_is_theirs() {
        let log = || {
            ServerMsg::Log(oxbot_protocol::LogLine {
                level: "info".into(),
                target: "oxbot_core".into(),
                message: "asking the user to sign in site=bank.example browser=Sam".into(),
                ts_ms: 0,
            })
        };
        let notice = |owner: Option<&str>| ServerMsg::Notice {
            message: "Signed out of bank.example.".into(),
            owner: owner.map(str::to_owned),
        };
        let mut farm = Client::new(Viewer::farm("Jay"), &[]);
        let mut sam = Client::new(Viewer::gate("Sam"), &[]);
        let mut ann = Client::new(Viewer::gate("Ann"), &[]);
        assert!(forward(log(), &mut farm).is_some());
        assert!(forward(log(), &mut ann).is_none());
        assert!(forward(notice(Some("Sam")), &mut sam).is_some());
        assert!(forward(notice(Some("Sam")), &mut farm).is_some());
        assert!(forward(notice(Some("Sam")), &mut ann).is_none());
        assert!(forward(notice(None), &mut ann).is_some());
    }

    #[test]
    fn nothing_about_a_private_bot_reaches_anyone_else() {
        let bots = [
            info("mine", Some("Sam"), true),
            info("shared", Some("Sam"), false),
        ];
        let mut ann = Client::new(Viewer::gate("Ann"), &bots);
        let mut farm = Client::new(Viewer::farm("Jay"), &bots);
        let list = ServerMsg::Bots {
            bots: bots.to_vec(),
        };
        let mut busy = list.clone();
        if let ServerMsg::Bots { bots } = &mut busy {
            bots[0].busy = true;
            bots[0].last_reply_seq = 9;
        }
        let Some(ServerMsg::Bots { bots: seen }) = forward(busy, &mut ann) else {
            panic!("a list")
        };
        assert_eq!(seen.len(), 2, "a private bot is listed by name");
        assert!(
            seen[0].private && !seen[0].busy && seen[0].last_reply_seq == 0,
            "but shows nothing of what it does"
        );
        assert!(!ann.sees("mine"));
        let Some(ServerMsg::Bots { bots: seen }) = forward(list, &mut farm) else {
            panic!("a list")
        };
        assert_eq!(seen.len(), 2, "the Farm sees every bot");

        let activity = ServerMsg::Activity {
            bot: "mine".into(),
            text: "Thinking…".into(),
        };
        assert!(forward(activity.clone(), &mut ann).is_none());
        assert!(forward(activity, &mut farm).is_some());

        let sams = ServerMsg::Sites {
            sites: vec![],
            owner: Some("Sam".into()),
        };
        assert!(
            forward(sams.clone(), &mut ann).is_none(),
            "someone else's sign-ins"
        );
        assert!(forward(sams, &mut Client::new(Viewer::gate("Sam"), &bots)).is_some());
        let home = ServerMsg::Sites {
            sites: vec![],
            owner: None,
        };
        assert!(forward(home, &mut ann).is_some());

        // A bot made private later drops out of the list and the subscriptions.
        ann.subs.insert("shared".into(), 0);
        ann.see(&[info("shared", Some("Sam"), true)]);
        assert!(ann.subs.is_empty() && !ann.sees("shared"));
    }
}
