//! `oxbot serve`: a WebSocket server in front of the [`Hub`].
//!
//! Each connection gets a [`ServerMsg::Hello`], then speaks [`ClientMsg`] /
//! [`ServerMsg`] JSON, one message per text frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::{SinkExt, StreamExt};
use oxbot_core::bot::{DEFAULT_BOT, is_personal_herder};
use oxbot_core::hub::answers;
use oxbot_core::id::BrowserOwner;
use oxbot_core::{Hub, Viewer};
use oxbot_protocol::{
    AppSettings, ClientMsg, DEFAULT_PAGE, LogLine, MAX_PAGE, ServerHello, ServerMsg, SetupAction,
    SetupReport,
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
    /// The Mac account's first name: who the Farm's own person is the first
    /// time oxbot runs (then they're found by id; see
    /// [`Hub::farm_person`]). Gates send as the person they were paired for.
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
    /// The Gate door phones and laptops connect through (`None` in tests
    /// that don't need it).
    pub door: Option<Arc<crate::door::Door>>,
    /// The Farm secret (see [`crate::secret`]): a connection from this Mac
    /// is the Farm only if it shows this.
    pub farm_key: String,
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

    /// Who the Farm's own window is: the Farm's own person. (Gates connect
    /// through the Gate door as the person they were paired for.)
    #[must_use]
    pub fn farm_viewer(&self) -> Viewer {
        self.hub.farm_viewer(&self.local_person)
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
            door: None,
            farm_key: crate::secret::random_hex::<32>(),
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

/// The Gate door's routes: `POST /pair` for anyone with the code, and the
/// page and WebSocket for paired Gates only (see [`crate::door`]).
pub fn door_router(state: AppState, door: &Arc<crate::door::Door>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(door),
            crate::door::http::require_key,
        ))
        .with_state(state)
        .merge(
            Router::new()
                .route("/pair", post(crate::door::http::pair))
                .with_state(Arc::clone(door)),
        )
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
    gate: Option<Extension<crate::door::http::GateAuth>>,
) -> Response {
    // Through the Gate door: the key was checked; always the Gate's person.
    if let Some(Extension(auth)) = gate {
        let viewer = auth.viewer;
        let id = auth.gate.id;
        return ws.on_upgrade(move |socket| connection(socket, state, viewer, Some(id)));
    }
    // Browsers send an Origin header and let any web page open WebSockets to
    // localhost. Only the oxbot page itself may connect.
    if let Some(origin) = headers.get(header::ORIGIN)
        && !is_local_origin(origin.to_str().unwrap_or(""))
    {
        tracing::warn!(?origin, "refused a WebSocket from another website");
        return (StatusCode::FORBIDDEN, "oxbot only accepts its own page").into_response();
    }
    // Being on this Mac isn't enough to be the Farm: it takes the Farm
    // secret, which only oxbot's own windows and tools have.
    let farm = query
        .get("k")
        .is_some_and(|k| crate::secret::same(k.trim(), &state.farm_key));
    let viewer = match query.get("as").map(|n| n.trim()) {
        _ if farm => state.farm_viewer(),
        // Test mode only: the page names its person, made on purpose here
        // (as pairing would) so the connection carries their id.
        Some(name) if state.e2e && !name.is_empty() => match state.hub.person_named(name) {
            Ok(person) => Viewer::person(person.id, person.name, false),
            Err(err) => {
                tracing::warn!(%err, "couldn't make a test connection's person");
                return (StatusCode::INTERNAL_SERVER_ERROR, "no such person").into_response();
            }
        },
        _ => {
            tracing::warn!("refused a local connection without the Farm secret");
            return (
                StatusCode::UNAUTHORIZED,
                "Open oxbot from its app, or with the link `oxbot serve` printed.",
            )
                .into_response();
        }
    };
    ws.on_upgrade(move |socket| connection(socket, state, viewer, None))
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
    /// Their own Herder's id. They see it as `assistant`, and see no other
    /// Herder (see [`Hub::herder_for`]).
    herder: String,
    /// The bots they may see, kept up to date from bot-list updates.
    visible: std::collections::HashSet<String>,
    /// Bot id → last `seq` sent.
    subs: HashMap<String, u64>,
}

impl Client {
    #[cfg(test)]
    fn new(viewer: Viewer, bots: &[oxbot_protocol::BotInfo]) -> Self {
        Self::with_herder(viewer, bots, DEFAULT_BOT.to_owned())
    }

    #[cfg(test)]
    fn with_herder(viewer: Viewer, bots: &[oxbot_protocol::BotInfo], herder: String) -> Self {
        let mut client = Self::with_visible(viewer, std::collections::HashSet::new(), herder);
        client.see(bots);
        client
    }

    /// A client for `viewer`, who may see the bots in `visible`.
    fn with_visible(
        viewer: Viewer,
        visible: std::collections::HashSet<String>,
        herder: String,
    ) -> Self {
        Self {
            viewer,
            herder,
            visible,
            subs: HashMap::new(),
        }
    }

    /// Takes a new bot list: remembers which bots this client may see, and
    /// drops subscriptions to the rest.
    fn see(&mut self, bots: &[oxbot_protocol::BotInfo]) {
        self.visible = bots
            .iter()
            .filter(|b| self.viewer.can_see_info(b))
            .map(|b| b.id.clone())
            .collect();
        let visible = &self.visible;
        self.subs.retain(|bot, _| visible.contains(bot));
    }

    fn sees(&self, bot: &str) -> bool {
        self.visible.contains(bot)
    }

    /// A bot id as this client said it, as the Hub knows it: `assistant`
    /// is their own Herder.
    fn inner(&self, bot: String) -> String {
        if bot == DEFAULT_BOT {
            self.herder.clone()
        } else {
            bot
        }
    }

    /// A bot id as the Hub knows it, as this client sees it; `None` for
    /// someone else's Herder.
    fn outer(&self, bot: &str) -> Option<String> {
        if bot == self.herder {
            Some(DEFAULT_BOT.to_owned())
        } else if bot == DEFAULT_BOT || is_personal_herder(bot) {
            None
        } else {
            Some(bot.to_owned())
        }
    }

    /// The message with bot ids as this client says them.
    fn inbound(&self, msg: ClientMsg) -> ClientMsg {
        match msg {
            ClientMsg::Subscribe { bot, since } => ClientMsg::Subscribe {
                bot: self.inner(bot),
                since,
            },
            ClientMsg::OpenBot { bot, limit } => ClientMsg::OpenBot {
                bot: self.inner(bot),
                limit,
            },
            ClientMsg::Older { bot, before, limit } => ClientMsg::Older {
                bot: self.inner(bot),
                before,
                limit,
            },
            ClientMsg::Send { bot, text } => ClientMsg::Send {
                bot: self.inner(bot),
                text,
            },
            ClientMsg::Cancel { bot } => ClientMsg::Cancel {
                bot: self.inner(bot),
            },
            ClientMsg::SetPrivate { bot, private } => ClientMsg::SetPrivate {
                bot: self.inner(bot),
                private,
            },
            ClientMsg::RenameBot { bot, name } => ClientMsg::RenameBot {
                bot: self.inner(bot),
                name,
            },
            ClientMsg::DeleteBot { bot } => ClientMsg::DeleteBot {
                bot: self.inner(bot),
            },
            other => other,
        }
    }

    /// The message with bot ids as this client sees them, or `None` if it's
    /// about someone else's Herder.
    fn outbound(&self, msg: ServerMsg) -> Option<ServerMsg> {
        Some(match msg {
            ServerMsg::Event(mut e) => {
                e.bot = self.outer(&e.bot)?;
                ServerMsg::Event(e)
            }
            ServerMsg::History {
                bot,
                mut events,
                first_seq,
                has_older,
            } => {
                let bot = self.outer(&bot)?;
                for e in &mut events {
                    e.bot.clone_from(&bot);
                }
                ServerMsg::History {
                    bot,
                    events,
                    first_seq,
                    has_older,
                }
            }
            ServerMsg::Delta { bot, text } => ServerMsg::Delta {
                bot: self.outer(&bot)?,
                text,
            },
            ServerMsg::Activity { bot, text } => ServerMsg::Activity {
                bot: self.outer(&bot)?,
                text,
            },
            ServerMsg::Browser { bot, state } => ServerMsg::Browser {
                bot: self.outer(&bot)?,
                state,
            },
            ServerMsg::BotCreated(mut b) => {
                b.id = self.outer(&b.id)?;
                ServerMsg::BotCreated(b)
            }
            ServerMsg::ApprovalNeeded(mut a) => {
                a.bot = self.outer(&a.bot)?;
                ServerMsg::ApprovalNeeded(a)
            }
            ServerMsg::SignInNeeded(mut r) => {
                r.bot = self.outer(&r.bot)?;
                ServerMsg::SignInNeeded(r)
            }
            ServerMsg::Bots { bots } => ServerMsg::Bots {
                bots: bots
                    .into_iter()
                    .filter_map(|mut b| {
                        b.id = self.outer(&b.id)?;
                        Some(b)
                    })
                    .collect(),
            },
            ServerMsg::Usage(mut usage) => {
                usage.bots = usage
                    .bots
                    .into_iter()
                    .filter_map(|mut b| {
                        b.bot = self.outer(&b.bot)?;
                        Some(b)
                    })
                    .collect();
                ServerMsg::Usage(usage)
            }
            other => other,
        })
    }
}

/// Which Herder `viewer` talks to: the Farm and the Farm's own person (by
/// id, on the Mac or a phone) use the Farm's; anyone else gets their own.
fn herder_for(state: &AppState, viewer: &Viewer) -> String {
    match state.hub.herder_of(viewer) {
        Ok(id) => id.to_string(),
        Err(err) => {
            // No Herder at all rather than someone else's.
            let person = viewer.id.as_ref().map(ToString::to_string);
            tracing::warn!(%err, person, "couldn't make a Herder for a person");
            String::from("-")
        }
    }
}

/// One client's connection. `gate` is the Gate's id when it came through
/// the Gate door: it shows as online meanwhile, and is cut off if removed.
async fn connection(socket: WebSocket, state: AppState, viewer: Viewer, gate: Option<String>) {
    let door = state.door.clone().filter(|_| gate.is_some());
    let mut removals = door.as_ref().map(|d| d.removals());
    // Online until this connection ends, however it ends.
    let _online = match (&door, &gate) {
        (Some(door), Some(id)) => Some(door.online(id)),
        _ => None,
    };
    serve_client(socket, &state, viewer, gate.as_deref(), removals.as_mut()).await;
}

async fn serve_client(
    socket: WebSocket,
    state: &AppState,
    viewer: Viewer,
    gate: Option<&str>,
    mut removals: Option<&mut broadcast::Receiver<String>>,
) {
    let state = state.clone();
    let (mut sink, mut stream) = socket.split();
    // Subscribe before replaying history, so nothing falls in the gap.
    let mut updates = state.hub.subscribe();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(64);
    let herder = herder_for(&state, &viewer);
    let visible = state.hub.visible_ids(&viewer);
    let mut client = Client::with_visible(viewer, visible, herder);

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
    // Only once there are Gates, so clients that don't know Gates (an older
    // `oxbot chat`) never see the message on a Farm that has none.
    if let Some(door) = state.door.as_ref().filter(|_| client.viewer.farm) {
        let gates = door.gates();
        if !gates.is_empty() {
            first.push(ServerMsg::Gates { gates });
        }
        first.push(ServerMsg::Door(door.state()));
    }
    // The log names every bot, site and person: the Farm's only.
    if let Some(relay) = state.logs.as_ref().filter(|_| client.viewer.farm) {
        first.extend(relay.recent().into_iter().map(ServerMsg::Log));
    }
    first.extend(
        state
            .hub
            .pending_approvals()
            .into_iter()
            .filter(|a| client.sees(&a.bot) && answers(a, &client.viewer))
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
    for msg in first {
        let Some(msg) = client.outbound(msg) else {
            continue;
        };
        if send(&mut sink, &msg).await.is_err() {
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
            removed = next_removal(removals.as_deref_mut()) => {
                if gate.is_some() && (gate == Some(removed.as_str()) || removed == crate::door::EVERY_GATE) {
                    tracing::info!(gate = removed, "closing a removed Gate's connection");
                    let _ = sink.send(Message::Close(None)).await;
                    return;
                }
                continue;
            }
        };
        for msg in outgoing {
            let Some(msg) = client.outbound(msg) else {
                continue;
            };
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

/// The next removed Gate's id; never resolves without a door.
async fn next_removal(rx: Option<&mut broadcast::Receiver<String>>) -> String {
    let Some(rx) = rx else {
        return std::future::pending().await;
    };
    loop {
        match rx.recv().await {
            Ok(id) => return id,
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
    let msg = client.inbound(msg);
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    if !viewer.farm && farm_only(&msg) {
        return error(&"Only the Farm can do that.");
    }
    let msg = match handle_farm(msg, state).await {
        Ok(reply) => return reply,
        Err(other) => other,
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
        ClientMsg::Subscribe { .. } | ClientMsg::OpenBot { .. } | ClientMsg::Older { .. } => {
            handle_history(msg, state, client)
        }
        ClientMsg::Send { bot, text } => {
            if let Err(e) = state.hub.bot_for(&bot, &viewer) {
                return error(&e);
            }
            // Sending may wait for a free turn slot; don't block this connection.
            tokio::spawn(send_as(
                Arc::clone(&state.hub),
                bot,
                text,
                viewer.clone(),
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
                .create_bot_as(&name, &instructions, Some(&viewer), private)
                .await
            {
                Ok(info) => {
                    client.visible.insert(info.id.clone());
                    viewer
                        .redact(vec![info])
                        .into_iter()
                        .map(ServerMsg::BotCreated)
                        .collect()
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
        other => handle_sign_ins(other, state, &viewer).await,
    }
}

/// Sends a bot's history: replayed events ([`ClientMsg::Subscribe`]) or a
/// page ([`ClientMsg::OpenBot`], [`ClientMsg::Older`]). Subscribing and
/// opening also stream the bot's new events from then on.
fn handle_history(msg: ClientMsg, state: &AppState, client: &mut Client) -> Vec<ServerMsg> {
    let viewer = &client.viewer;
    let result = match msg {
        ClientMsg::Subscribe { bot, since } => {
            state.hub.history_for(&bot, since, viewer).map(|events| {
                let last = events.last().map_or(since, |e| e.seq);
                client.visible.insert(bot.clone());
                client.subs.insert(bot, last);
                events.into_iter().map(ServerMsg::Event).collect()
            })
        }
        ClientMsg::OpenBot { bot, limit } => state
            .hub
            .history_page_for(&bot, None, page_size(limit), viewer)
            .map(|page| {
                // Live events follow on from the end of the page.
                client.visible.insert(bot.clone());
                client.subs.insert(bot.clone(), page.last_seq);
                vec![history(bot, page)]
            }),
        ClientMsg::Older { bot, before, limit } => state
            .hub
            .history_page_for(&bot, Some(before), page_size(limit), viewer)
            .map(|page| vec![history(bot, page)]),
        _ => Ok(Vec::new()),
    };
    result.unwrap_or_else(|e| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    })
}

/// How many events a page holds: what the client asked, within reason.
fn page_size(limit: Option<u32>) -> usize {
    let n = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    usize::try_from(n).unwrap_or(usize::MAX)
}

fn history(bot: String, page: oxbot_core::store::HistoryPage) -> ServerMsg {
    ServerMsg::History {
        bot,
        events: page.events,
        first_seq: page.first_seq,
        has_older: page.has_older,
    }
}

/// The Farm's own jobs: pairing and removing Gates, the Mac's setup
/// (Farm-only ones are checked by the caller). Gives other messages back.
async fn handle_farm(msg: ClientMsg, state: &AppState) -> Result<Vec<ServerMsg>, ClientMsg> {
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    Ok(match msg {
        ClientMsg::StartPairing { .. }
        | ClientMsg::CancelPairing
        | ClientMsg::RemoveGate { .. }
        | ClientMsg::AddDoorNetwork
        | ClientMsg::RemoveDoorNetwork { .. }
        | ClientMsg::SetDoorName { .. } => {
            let Some(door) = &state.door else {
                return Ok(error(&"This oxbot has no Gate door."));
            };
            match handle_door(msg, door).await {
                Ok(reply) => reply,
                Err(e) => error(&format!("{e:#}")),
            }
        }
        ClientMsg::SetLaunchAtLogin { on } => {
            let Some(app) = &state.app else {
                return Ok(error(&"Launch at login is only in the oxbot app."));
            };
            match app.set_launch_at_login(on) {
                Ok(()) => vec![ServerMsg::Settings(state.settings())],
                Err(e) => error(&e),
            }
        }
        ClientMsg::CheckSetup => vec![ServerMsg::Setup(state.refresh_setup().await)],
        ClientMsg::RunSetup { action } => {
            if state
                .setup_running
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(vec![ServerMsg::Notice {
                    message: "A setup step is already running.".to_owned(),
                    owner: None,
                    owner_id: None,
                }]);
            }
            tokio::spawn(run_setup(state.clone(), action));
            Vec::new()
        }
        other => return Err(other),
    })
}

/// The door's jobs: pairing, removing Gates, and which networks phones may
/// use.
async fn handle_door(msg: ClientMsg, door: &crate::door::Door) -> anyhow::Result<Vec<ServerMsg>> {
    Ok(match msg {
        ClientMsg::StartPairing { person } => {
            vec![ServerMsg::Pairing(door.start_pairing(&person).await?)]
        }
        ClientMsg::CancelPairing => {
            door.cancel_pairing().await;
            vec![ServerMsg::PairingClosed]
        }
        ClientMsg::RemoveGate { id } => {
            door.remove(&id).await?;
            Vec::new()
        }
        ClientMsg::AddDoorNetwork => vec![ServerMsg::Door(door.add_network().await?)],
        ClientMsg::RemoveDoorNetwork { id } => {
            vec![ServerMsg::Door(door.remove_network(&id).await?)]
        }
        ClientMsg::SetDoorName { name } => vec![ServerMsg::Door(door.set_name(&name).await?)],
        _ => Vec::new(),
    })
}

/// What only the Farm itself may do: pair and remove Gates, pick the
/// networks phones use, change the Mac's setup.
fn farm_only(msg: &ClientMsg) -> bool {
    matches!(
        msg,
        ClientMsg::StartPairing { .. }
            | ClientMsg::CancelPairing
            | ClientMsg::RemoveGate { .. }
            | ClientMsg::AddDoorNetwork
            | ClientMsg::RemoveDoorNetwork { .. }
            | ClientMsg::SetDoorName { .. }
            | ClientMsg::RunSetup { .. }
            | ClientMsg::SetLaunchAtLogin { .. }
    )
}

/// Sends `text` to `bot` from `from`, reporting a failure to the client.
async fn send_as(
    hub: Arc<Hub>,
    bot: String,
    text: String,
    from: Viewer,
    out: mpsc::Sender<ServerMsg>,
) {
    if let Err(e) = hub.send_as(&bot, &text, &from).await {
        let _ = out
            .send(ServerMsg::Error {
                message: e.to_string(),
            })
            .await;
    }
}

/// Handles sign-in cards and the sign-in lists. A card for a bot the
/// viewer can't see is ignored; "mine" means the viewer's own browser
/// (never home's, even for someone oxbot can't place), else the home one.
async fn handle_sign_ins(msg: ClientMsg, state: &AppState, viewer: &Viewer) -> Vec<ServerMsg> {
    let viewer = &*state.hub.resolve(viewer);
    let owner = |mine: bool| -> Result<BrowserOwner, oxbot_core::HubError> {
        match (mine, &viewer.id) {
            (false, _) => Ok(BrowserOwner::Home),
            (true, Some(id)) => Ok(BrowserOwner::Person(id.clone())),
            (true, None) => Err(oxbot_core::HubError::NotAllowed(
                "oxbot doesn't know whose browser that is.".to_owned(),
            )),
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
        ClientMsg::SignOutAll { mine } => match owner(mine) {
            Ok(owner) => reply(state.hub.sign_out_all(owner).await),
            Err(e) => reply(Err(e)),
        },
        ClientMsg::OpenSite { site, mine } => match owner(mine) {
            Ok(owner) => reply(state.hub.open_site(&site, owner).await),
            Err(e) => reply(Err(e)),
        },
        ClientMsg::AddSignIn { url, mine } => match owner(mine) {
            Ok(owner) => reply(state.hub.add_sign_in(&url, owner).await),
            Err(e) => reply(Err(e)),
        },
        ClientMsg::SignOutSite { site, mine } => match owner(mine) {
            Ok(owner) => reply(state.hub.sign_out_site(&site, owner).await),
            Err(e) => reply(Err(e)),
        },
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
        ServerMsg::History { ref bot, .. } => client.sees(bot).then_some(msg),
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
        // A card goes to the person whose request raised it, and the Farm.
        ServerMsg::ApprovalNeeded(ref a) => {
            (client.sees(&a.bot) && answers(a, &client.viewer)).then_some(msg)
        }
        ServerMsg::SignInNeeded(ref r) => client.sees(&r.bot).then_some(msg),
        // A person's own sign-ins go to them alone, by id.
        ServerMsg::Sites {
            ref owner,
            ref owner_id,
            ..
        } => (owner.is_none()
            || client
                .viewer
                .is_person(owner_id.as_deref(), owner.as_deref()))
        .then_some(msg),
        ServerMsg::Usage(mut usage) => {
            usage.bots.retain(|b| client.sees(&b.bot));
            Some(ServerMsg::Usage(usage))
        }
        // The log names every bot, site and person; pairing is the Farm's
        // job: the Farm's only.
        ServerMsg::Log(_)
        | ServerMsg::Pairing(_)
        | ServerMsg::PairingClosed
        | ServerMsg::Gates { .. }
        | ServerMsg::Door(_) => client.viewer.farm.then_some(msg),
        ServerMsg::Notice {
            ref owner,
            ref owner_id,
            ..
        } => (owner.is_none()
            || client.viewer.farm
            || client
                .viewer
                .is_person(owner_id.as_deref(), owner.as_deref()))
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
            ..Default::default()
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
            owner_id: None,
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
            owner_id: None,
        };
        assert!(
            forward(sams.clone(), &mut ann).is_none(),
            "someone else's sign-ins"
        );
        assert!(forward(sams, &mut Client::new(Viewer::gate("Sam"), &bots)).is_some());
        let home = ServerMsg::Sites {
            sites: vec![],
            owner: None,
            owner_id: None,
        };
        assert!(forward(home, &mut ann).is_some());

        // A bot made private later drops out of the list and the subscriptions.
        ann.subs.insert("shared".into(), 0);
        ann.see(&[info("shared", Some("Sam"), true)]);
        assert!(ann.subs.is_empty() && !ann.sees("shared"));
    }

    fn card(bot: &str, asker: Option<&str>) -> ServerMsg {
        ServerMsg::ApprovalNeeded(oxbot_protocol::Approval {
            id: 1,
            bot: bot.into(),
            kind: oxbot_protocol::ApprovalKind::Change,
            action: "click".into(),
            target: "Buy".into(),
            site: "shop.example".into(),
            page: None,
            detail: None,
            screenshot: None,
            asker: asker.map(Into::into),
            asker_id: None,
        })
    }

    #[test]
    fn a_card_goes_to_who_asked_and_the_farm() {
        let bots = [info("shared", None, false)];
        let mut jay = Client::new(Viewer::gate("Jay"), &bots);
        let mut sam = Client::new(Viewer::gate("Sam"), &bots);
        let mut farm = Client::new(Viewer::farm("Jay"), &bots);
        assert!(forward(card("shared", Some("Jay")), &mut jay).is_some());
        assert!(forward(card("shared", Some("Jay")), &mut farm).is_some());
        assert!(
            forward(card("shared", Some("Jay")), &mut sam).is_none(),
            "Sam can't answer Jay's card"
        );
        assert!(
            forward(card("shared", None), &mut sam).is_none(),
            "nobody asked: the Farm's"
        );
        assert!(forward(card("shared", None), &mut farm).is_some());
    }

    #[test]
    fn a_persons_sites_notices_and_cards_go_to_them_by_id() {
        let sam = oxbot_core::id::PersonId::random();
        let mut real = Client::new(Viewer::person(sam.clone(), "Samuel", false), &[]);
        let mut lookalike = Client::new(
            Viewer::person(oxbot_core::id::PersonId::random(), "Sam", false),
            &[],
        );
        let mut by_name = Client::new(Viewer::gate("Sam"), &[]);
        let sites = ServerMsg::Sites {
            sites: vec![],
            owner: Some("Sam".into()),
            owner_id: Some(sam.to_string()),
        };
        let notice = ServerMsg::Notice {
            message: "Signed out.".into(),
            owner: Some("Sam".into()),
            owner_id: Some(sam.to_string()),
        };
        for msg in [sites, notice] {
            assert!(forward(msg.clone(), &mut real).is_some(), "theirs, renamed");
            assert!(forward(msg.clone(), &mut lookalike).is_none(), "same name");
            assert!(forward(msg, &mut by_name).is_none(), "a name alone");
        }
        let bots = [info("shared", None, false)];
        let mut real = Client::new(Viewer::person(sam.clone(), "Samuel", false), &bots);
        let mut lookalike = Client::new(
            Viewer::person(oxbot_core::id::PersonId::random(), "Sam", false),
            &bots,
        );
        let ServerMsg::ApprovalNeeded(mut theirs) = card("shared", Some("Sam")) else {
            panic!("a card")
        };
        theirs.asker_id = Some(sam.to_string());
        let theirs = ServerMsg::ApprovalNeeded(theirs);
        assert!(forward(theirs.clone(), &mut real).is_some());
        assert!(forward(theirs, &mut lookalike).is_none());
    }

    #[tokio::test]
    async fn a_test_connection_names_its_person_on_purpose() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = Hub::new(
            vec![],
            oxbot_core::Store::open_in_memory().unwrap(),
            oxbot_core::HubConfig {
                enrol_named_viewers: false,
                ..oxbot_core::HubConfig::new(tmp.path())
            },
        );
        let state = AppState {
            e2e: true,
            ..AppState::new(hub, "test-mac")
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, state.clone(), std::future::pending()));
        let (_ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?as=Ann"))
            .await
            .unwrap();
        let ann = state.hub.resolve(&Viewer::gate("Ann")).into_owned();
        assert!(ann.id.is_some(), "made when the page connected");
    }

    fn state(dir: &std::path::Path) -> AppState {
        let hub = Hub::new(
            vec![],
            oxbot_core::Store::open_in_memory().unwrap(),
            oxbot_core::HubConfig::new(dir),
        );
        AppState {
            local_person: "Jay".to_owned(),
            ..AppState::new(hub, "test-mac")
        }
    }

    #[tokio::test]
    async fn a_gate_named_like_the_farms_person_is_not_the_farm() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let farm = state.farm_viewer();
        assert!(farm.farm && farm.id.is_some());
        assert_eq!(herder_for(&state, &farm), DEFAULT_BOT);

        // Another person's Gate that calls itself "Jay".
        let sam = state.hub.resolve(&Viewer::gate("Sam")).into_owned();
        let lookalike = Viewer {
            name: "Jay".into(),
            ..sam.clone()
        };
        let herder = herder_for(&state, &lookalike);
        assert_ne!(herder, DEFAULT_BOT, "not the Farm's Herder");
        assert_eq!(herder, herder_for(&state, &sam), "Sam's own");
        assert!(!state.settings_for(&lookalike).farm, "no Farm rights");
        // The Farm's person's own phone (same id, no secret) isn't the Farm either.
        let phone = Viewer {
            farm: false,
            ..farm.clone()
        };
        assert_eq!(herder_for(&state, &phone), DEFAULT_BOT);
        assert!(!state.settings_for(&phone).farm);
    }

    #[tokio::test]
    async fn my_own_browser_is_never_home_for_someone_oxbot_cant_place() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        for msg in [
            ClientMsg::SignOutAll { mine: true },
            ClientMsg::AddSignIn {
                url: "bank.example".into(),
                mine: true,
            },
        ] {
            let replies = handle_sign_ins(msg, &state, &Viewer::gate("")).await;
            assert!(
                matches!(&replies[..], [ServerMsg::Error { message }] if message.contains("whose browser")),
                "{replies:?}"
            );
        }
    }

    #[test]
    fn each_person_sees_only_their_own_herder_as_assistant() {
        let anns = oxbot_core::bot::personal_herder_id("Ann").to_string();
        let sams = oxbot_core::bot::personal_herder_id("Sam").to_string();
        let mut herders = [
            info(DEFAULT_BOT, None, false),
            info(&anns, Some("Ann"), true),
            info(&sams, Some("Sam"), true),
        ];
        for h in &mut herders {
            h.central = true;
        }
        let bots = [herders.to_vec(), vec![info("recipes", None, false)]].concat();
        let ann = Client::with_herder(Viewer::gate("Ann"), &bots, anns.clone());
        let farm = Client::with_herder(Viewer::farm("Jay"), &bots, DEFAULT_BOT.to_owned());

        let Some(ServerMsg::Bots { bots: seen }) =
            ann.outbound(ServerMsg::Bots { bots: bots.clone() })
        else {
            panic!("a list")
        };
        let ids: Vec<&str> = seen.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, [DEFAULT_BOT, "recipes"]);
        let Some(ServerMsg::Bots { bots: seen }) = farm.outbound(ServerMsg::Bots { bots }) else {
            panic!("a list")
        };
        assert_eq!(seen.iter().filter(|b| b.central).count(), 1);

        // What Ann says to `assistant` goes to her own Herder, and back.
        let ClientMsg::Send { bot, .. } = ann.inbound(ClientMsg::Send {
            bot: DEFAULT_BOT.into(),
            text: "hi".into(),
        }) else {
            panic!("a send")
        };
        assert_eq!(bot, anns);
        let Some(ServerMsg::Event(e)) = ann.outbound(event(&anns, 1)) else {
            panic!("her Herder's event")
        };
        assert_eq!(e.bot, DEFAULT_BOT);
        assert!(ann.outbound(event(&sams, 1)).is_none(), "not Sam's");
        assert!(
            ann.outbound(event(DEFAULT_BOT, 1)).is_none(),
            "not the Farm's"
        );
        assert!(farm.outbound(event(&anns, 1)).is_none());
    }
}
