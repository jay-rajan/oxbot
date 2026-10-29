//! `oxbot serve`: a WebSocket server in front of the [`Hub`].
//!
//! Each connection gets a [`ServerMsg::Hello`], then speaks [`ClientMsg`] /
//! [`ServerMsg`] JSON, one message per text frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use oxbot_core::Hub;
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
    /// The app's settings right now.
    #[must_use]
    pub fn settings(&self) -> AppSettings {
        AppSettings {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            data_dir: self.data_dir.display().to_string(),
            launch_at_login: self.app.as_ref().and_then(|a| a.launch_at_login()),
            you: self.local_person.clone(),
        }
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
    ws.on_upgrade(move |socket| connection(socket, state))
}

/// Whether a browser Origin is the oxbot page on this Mac.
fn is_local_origin(origin: &str) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(':').next().unwrap_or("");
    matches!(host, "127.0.0.1" | "localhost") && !rest.contains('/')
}

/// What a connection has subscribed to: bot id → last `seq` sent.
type Subscriptions = HashMap<String, u64>;

async fn connection(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    // Subscribe before replaying history, so nothing falls in the gap.
    let mut updates = state.hub.subscribe();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(64);
    let mut subs = Subscriptions::new();

    let mut logs = state.logs.as_ref().map(|l| l.subscribe());
    let mut first = vec![
        ServerMsg::Hello(ServerHello::new(&state.server_name, state.started_ms)),
        ServerMsg::Settings(state.settings()),
    ];
    first.push(ServerMsg::Setup(state.setup_report()));
    if let Ok(usage) = state.hub.usage() {
        first.push(ServerMsg::Usage(usage));
    }
    if let Ok(people) = state.hub.people() {
        first.push(ServerMsg::People { people });
    }
    first.push(ServerMsg::Sites {
        sites: state.hub.sites(),
    });
    if let Some(relay) = &state.logs {
        first.extend(relay.recent().into_iter().map(ServerMsg::Log));
    }
    first.extend(
        state
            .hub
            .pending_approvals()
            .into_iter()
            .map(ServerMsg::ApprovalNeeded),
    );
    first.extend(
        state
            .hub
            .pending_sign_ins()
            .into_iter()
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
                    Ok(msg) => handle(msg, &state, &mut subs, &out_tx).await,
                    Err(err) => vec![ServerMsg::Error { message: format!("bad message: {err}") }],
                },
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => continue, // pings, binary: ignore
            },
            update = updates.recv() => match update {
                Ok(msg) => forward(msg, &mut subs).into_iter().collect(),
                Err(broadcast::error::RecvError::Lagged(n)) => vec![ServerMsg::Error {
                    message: format!("missed {n} updates; subscribe again to catch up"),
                }],
                Err(broadcast::error::RecvError::Closed) => break,
            },
            Some(msg) = out_rx.recv() => vec![msg],
            Some(line) = next_log(logs.as_mut()) => vec![ServerMsg::Log(line)],
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
    subs: &mut Subscriptions,
    out: &mpsc::Sender<ServerMsg>,
) -> Vec<ServerMsg> {
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    match msg {
        ClientMsg::ListBots => match state.hub.bots().await {
            Ok(bots) => vec![ServerMsg::Bots { bots }],
            Err(e) => error(&e),
        },
        ClientMsg::Subscribe { bot, since } => match state.hub.history(&bot, since) {
            Ok(events) => {
                let last = events.last().map_or(since, |e| e.seq);
                subs.insert(bot, last);
                events.into_iter().map(ServerMsg::Event).collect()
            }
            Err(e) => error(&e),
        },
        ClientMsg::Send { bot, text } => {
            // Sending may wait for a free turn slot; don't block this connection.
            let hub = Arc::clone(&state.hub);
            let out = out.clone();
            let from = state.local_person.clone();
            tokio::spawn(async move {
                if let Err(e) = hub.send(&bot, &text, Some(&from)).await {
                    let _ = out
                        .send(ServerMsg::Error {
                            message: e.to_string(),
                        })
                        .await;
                }
            });
            Vec::new()
        }
        ClientMsg::Cancel { bot } => match state.hub.cancel(&bot).await {
            Ok(()) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::CreateBot { name, instructions } => {
            match state.hub.create_bot(&name, &instructions).await {
                Ok(info) => vec![ServerMsg::BotCreated(info)],
                Err(e) => error(&e),
            }
        }
        ClientMsg::Answer { id, answer } => {
            state.hub.answer(id, answer);
            Vec::new()
        }
        ClientMsg::SetLaunchAtLogin { on } => {
            let Some(app) = &state.app else {
                return error(&"Launch at login is only in the oxbot app.");
            };
            match app.set_launch_at_login(on) {
                Ok(()) => vec![ServerMsg::Settings(state.settings())],
                Err(e) => error(&e),
            }
        }
        ClientMsg::OpenSignIn { id } => match state.hub.open_sign_in(id).await {
            Ok(()) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::SignInDone { id } => {
            state.hub.resolve_sign_in(id, true);
            Vec::new()
        }
        ClientMsg::CancelSignIn { id } => {
            state.hub.resolve_sign_in(id, false);
            Vec::new()
        }
        ClientMsg::SignOutAll => match state.hub.sign_out_all().await {
            Ok(()) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::CheckSetup => vec![ServerMsg::Setup(state.refresh_setup().await)],
        ClientMsg::OpenSite { site } => match state.hub.open_site(&site).await {
            Ok(()) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::SignOutSite { site } => match state.hub.sign_out_site(&site).await {
            Ok(()) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::RunSetup { action } => {
            if state
                .setup_running
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return vec![ServerMsg::Notice {
                    message: "A setup step is already running.".to_owned(),
                }];
            }
            tokio::spawn(run_setup(state.clone(), action));
            Vec::new()
        }
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

/// Passes a live update on if the client subscribed to that bot and hasn't
/// already had it from the history replay.
fn forward(msg: ServerMsg, subs: &mut Subscriptions) -> Option<ServerMsg> {
    match &msg {
        ServerMsg::Event(e) => {
            let last = subs.get_mut(&e.bot)?;
            if e.seq <= *last {
                return None;
            }
            *last = e.seq;
            Some(msg)
        }
        ServerMsg::Delta { bot, .. } => subs.contains_key(bot).then_some(msg),
        _ => Some(msg),
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

    #[test]
    fn forward_skips_other_bots_and_replayed_events() {
        let mut subs = Subscriptions::from([("a".to_owned(), 2)]);
        assert!(
            forward(event("b", 1), &mut subs).is_none(),
            "not subscribed"
        );
        assert!(
            forward(event("a", 2), &mut subs).is_none(),
            "already replayed"
        );
        assert!(forward(event("a", 3), &mut subs).is_some());
        assert_eq!(subs["a"], 3);
    }
}
