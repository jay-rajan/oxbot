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
use oxbot_core::engine::EngineKind;
use oxbot_core::hub::answers;
use oxbot_core::id::BrowserOwner;
use oxbot_core::store::Replay;
use oxbot_core::{Hub, Viewer};
use oxbot_protocol::{
    AppSettings, ClientMsg, DEFAULT_PAGE, EnginesView, KeyLimits, LogLine, MAX_PAGE, ModelRow,
    OpenRouterView, ReviewBot, ServerHello, ServerMsg, SetupAction, SetupReport,
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
    /// Posts Mac notifications (see [`crate::notify`]); `None` posts none.
    pub notifier: Option<Arc<dyn crate::notify::Notifier>>,
    /// Dictation with this Mac's microphone (see [`crate::dictation`]).
    pub dictation: Arc<crate::dictation::Dictation>,
    /// Numbers connections, so dictation goes back to the one that asked.
    pub next_conn: Arc<std::sync::atomic::AtomicU64>,
    /// Claude bots other people used before they became owner-only, found
    /// at start (see [`AppState::review_engines`]).
    pub engine_review: Arc<std::sync::RwLock<Vec<ReviewBot>>>,
    /// The port this server listens on (`OpenRouter`'s sign-in comes back
    /// to it); 0 if unknown.
    pub port: u16,
    /// The household's `OpenRouter` account (`None`: not in this server).
    pub openrouter: Option<Arc<crate::openrouter_auth::OpenRouter>>,
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
    /// Whether oxbot's window is on screen; `None` if this can't tell.
    fn window_visible(&self) -> Option<bool> {
        None
    }
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
            usage_alerts: self.hub.usage_alerts_on(),
            notify_when_hidden: self
                .app
                .as_ref()
                .and_then(|a| a.window_visible())
                .map(|_| self.hub.notify_when_hidden()),
            // The Mac's own microphone: the Farm's alone.
            dictation: viewer.farm && self.dictation.available(),
            owner: self.hub.is_owner_viewer(viewer),
            owner_name: self.hub.owner_display_name(),
            openrouter_ready: self.hub.engine_ready(EngineKind::OpenRouter).is_ok(),
            keychain_waiting: self
                .openrouter
                .as_ref()
                .is_some_and(|a| a.waiting_for_keychain()),
        }
    }

    /// Settings → Engines as `viewer` gets it: everyone the notice and
    /// whether `OpenRouter` is ready; the Farm also the banner and the Claude
    /// bots others used (those still on Claude).
    #[must_use]
    pub fn engines_for(&self, viewer: &Viewer) -> EnginesView {
        let farm = viewer.farm;
        let review = if farm {
            self.engine_review
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .filter(|r| self.hub.engine_of_bot(&r.bot) == Some(EngineKind::Claude))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let catalog = self.catalog();
        let models = catalog
            .as_ref()
            .map(|(c, _)| c.free_for_loop().map(model_row).collect())
            .unwrap_or_default();
        let openrouter = farm.then(|| self.openrouter_view(catalog.as_ref()));
        EnginesView {
            notice: oxbot_core::hub::ENGINES_NOTICE.to_owned(),
            owner: self.hub.owner_display_name(),
            openrouter_ready: self.hub.engine_ready(EngineKind::OpenRouter).is_ok(),
            banner: farm && self.hub.engines_notice_due(),
            review,
            models,
            openrouter,
        }
    }

    /// `OpenRouter`'s model list as last read, and when.
    fn catalog(&self) -> Option<(oxbot_llm::Catalog, i64)> {
        let (json, ms) = self.hub.model_catalog()?;
        match serde_json::from_str(&json) {
            Ok(catalog) => Some((catalog, ms)),
            Err(err) => {
                tracing::warn!(%err, "the kept model list is unreadable");
                None
            }
        }
    }

    /// Settings → Engines → `OpenRouter`, for the Farm.
    fn openrouter_view(&self, catalog: Option<&(oxbot_llm::Catalog, i64)>) -> OpenRouterView {
        let account = self.openrouter.as_deref();
        let key = account
            .and_then(crate::openrouter_auth::OpenRouter::key_info)
            .map(|k| KeyLimits {
                limit_usd: k.limit_usd,
                remaining_usd: k.remaining_usd,
                usage_daily_usd: k.usage_daily_usd,
                is_free_tier: k.is_free_tier,
                free_used: k.free_daily.map(|q| q.used),
                free_limit: k.free_daily.map(|q| q.limit),
            });
        let (paid_models, filtered_out, models_fetched_ms) =
            catalog.map_or((Vec::new(), 0, 0), |(c, ms)| {
                let filtered = c.models.iter().filter(|m| m.free && !m.fits_loop()).count();
                (
                    c.paid_for_loop().into_iter().map(model_row).collect(),
                    u32::try_from(filtered).unwrap_or(u32::MAX),
                    *ms,
                )
            });
        OpenRouterView {
            signed_in: account.is_some_and(crate::openrouter_auth::OpenRouter::signed_in),
            key,
            paid_models,
            filtered_out,
            models_fetched_ms,
            prefs: self.hub.engine_prefs(),
            usage: self.hub.openrouter_usage_today(),
            data_policy: self
                .hub
                .openrouter_data_policy_blocked()
                .then(|| oxbot_core::hub::DATA_POLICY_HELP.to_owned()),
        }
    }

    /// Tells everyone `OpenRouter` changed (signed in or out, new models):
    /// each gets their own settings and engines view.
    pub fn openrouter_changed(&self) {
        self.hub.announce(ServerMsg::Settings(self.settings()));
        self.hub
            .announce(ServerMsg::Engines(self.engines_for(&self.farm_viewer())));
    }

    /// Reads `OpenRouter`'s model list and the key's limits again, keeps the
    /// list, and tells everyone.
    ///
    /// # Errors
    /// Why the list wasn't read or kept, in words.
    pub async fn refresh_models(&self) -> Result<(), String> {
        let Some(account) = self.openrouter.clone() else {
            return Err("OpenRouter isn't available in this oxbot.".to_owned());
        };
        account.refresh_key_info().await;
        let catalog = account.fetch_models().await;
        let result = match catalog {
            Ok(catalog) => match serde_json::to_string(&catalog) {
                Ok(json) => self
                    .hub
                    .set_model_catalog(&json, crate::runtime::now_ms())
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            },
            Err(err) => {
                tracing::warn!(kind = ?err.kind, "couldn't read OpenRouter's model list");
                Err(format!("OpenRouter's model list couldn't be read: {err}"))
            }
        };
        self.openrouter_changed();
        result
    }

    /// Looks again for Claude bots other people used in the last 30 days
    /// (once at start: they are owner-only now, and the owner reviews
    /// them in Settings → Engines).
    pub fn review_engines(&self) {
        const DAYS_30_MS: i64 = 30 * 24 * 60 * 60 * 1000;
        let since = crate::runtime::now_ms() - DAYS_30_MS;
        let found = self.hub.claude_bots_others_used(since);
        if !found.is_empty() {
            tracing::info!(
                bots = found.len(),
                "Claude bots other people used are owner-only now"
            );
        }
        *self
            .engine_review
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = found;
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
        self.dictation.check().await;
        let mut report = crate::setup::check(&self.search_path, self.tool_built_in).await;
        report.checks.push(crate::setup::mac_helper_check(
            self.dictation.helper().is_some(),
            self.dictation.available(),
            &self.dictation.why_not(),
        ));
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
            notifier: None,
            dictation: Arc::new(crate::dictation::Dictation::new(None)),
            next_conn: Arc::default(),
            engine_review: Arc::default(),
            port: 0,
            openrouter: None,
        }
    }
}

/// The chat page, baked into the binary.
const INDEX_HTML: &str = include_str!("../ui/index.html");

/// The HTTP routes: the chat page at `/`, the WebSocket at `/ws`, and
/// `POST /upload` for files sent with messages (this Mac only: not on the
/// Gate door).
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_upgrade))
        // "Sign in with OpenRouter" comes back here: this Mac only.
        .route(
            crate::openrouter_auth::CALLBACK_PATH,
            get(crate::openrouter_auth::callback),
        )
        .route(
            "/upload",
            // The upload streams to disk with its own limit.
            post(upload).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .with_state(state)
}

/// The most one WebSocket message may be: it carries only JSON (files go
/// over `POST /upload`).
const MAX_WS_MESSAGE: usize = 4 * 1024 * 1024;

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
    let ws = ws
        .max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE);
    // Through the Gate door: the key was checked; always the Gate's person.
    if let Some(Extension(auth)) = gate {
        let viewer = auth.viewer;
        let id = auth.gate.id;
        return ws.on_upgrade(move |socket| connection(socket, state, viewer, Some(id)));
    }
    let key = query.get("k").map(String::as_str);
    let viewer = match local_viewer(&state, &headers, key, query.get("as").map(String::as_str)) {
        Ok(viewer) => viewer,
        Err(refused) => return refused,
    };
    ws.on_upgrade(move |socket| connection(socket, state, viewer, None))
}

/// Who a request from this Mac (not the Gate door) is: the Farm if it
/// shows the Farm secret `key`; in test mode, the person it names with
/// `as`. Refused from another website's page, or without either.
#[allow(
    clippy::result_large_err,
    reason = "the refusal is the response to send"
)]
fn local_viewer(
    state: &AppState,
    headers: &HeaderMap,
    key: Option<&str>,
    as_name: Option<&str>,
) -> Result<Viewer, Response> {
    // Browsers send an Origin header and let any web page reach localhost.
    // Only the oxbot page itself may connect.
    if let Some(origin) = headers.get(header::ORIGIN)
        && !is_local_origin(origin.to_str().unwrap_or(""))
    {
        tracing::warn!(?origin, "refused a request from another website");
        return Err((StatusCode::FORBIDDEN, "oxbot only accepts its own page").into_response());
    }
    // Being on this Mac isn't enough to be the Farm: it takes the Farm
    // secret, which only oxbot's own windows and tools have.
    let farm = key.is_some_and(|k| crate::secret::same(k.trim(), &state.farm_key));
    match as_name.map(str::trim) {
        _ if farm => Ok(state.farm_viewer()),
        // Test mode only: the page names its person, made on purpose here
        // (as pairing would) so the connection carries their id.
        Some(name) if state.e2e && !name.is_empty() => match state.hub.person_named(name) {
            Ok(person) => Ok(Viewer::person(person.id, person.name, false)),
            Err(err) => {
                tracing::warn!(%err, "couldn't make a test connection's person");
                Err((StatusCode::INTERNAL_SERVER_ERROR, "no such person").into_response())
            }
        },
        _ => {
            tracing::warn!("refused a local connection without the Farm secret");
            Err((
                StatusCode::UNAUTHORIZED,
                "Open oxbot from its app, or with the link `oxbot serve` printed.",
            )
                .into_response())
        }
    }
}

/// How long an upload may go without sending anything before it's dropped.
const UPLOAD_IDLE: std::time::Duration = std::time::Duration::from_secs(60);

/// `POST /upload?bot=<id>&name=<file name>`, with the file as the body and
/// the Farm secret in `X-Oxbot-Key`: saves a file to send with the next
/// message to that bot. The answer is the file as an [`Attachment`] (JSON),
/// or `{"error": "…"}` in plain words.
///
/// [`Attachment`]: oxbot_protocol::Attachment
async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: axum::body::Body,
) -> Response {
    use oxbot_core::attach;
    let key = headers.get("x-oxbot-key").and_then(|v| v.to_str().ok());
    let viewer = match local_viewer(&state, &headers, key, query.get("as").map(String::as_str)) {
        Ok(viewer) => viewer,
        Err(refused) => return refused,
    };
    let Some(bot) = query.get("bot").filter(|b| !b.is_empty()) else {
        return upload_error(StatusCode::BAD_REQUEST, "Say which bot the file is for.");
    };
    let bot = if bot == DEFAULT_BOT {
        herder_for(&state, &viewer)
    } else {
        bot.clone()
    };
    let name = attach::shown_name(query.get("name").map_or("file", String::as_str));
    let (bot_id, inbox) = match state.hub.upload_inbox(&bot, &viewer) {
        Ok(found) => found,
        Err(err @ oxbot_core::HubError::UnknownBot(_)) => {
            return upload_error(StatusCode::NOT_FOUND, &err.to_string());
        }
        Err(err) => return upload_error(StatusCode::FORBIDDEN, &err.to_string()),
    };
    // Files only for a bot they may give tasks (Claude bots: the owner's),
    // refused before anything is read.
    if let Err(err) = state.hub.may_send(bot_id.as_str(), &viewer) {
        return upload_error(StatusCode::FORBIDDEN, &err.to_string());
    }
    // Too big to take, by what it says it is.
    let length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let video = std::path::Path::new(&name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["mov", "mp4", "m4v"].contains(&e.to_ascii_lowercase().as_str()));
    let (cap, what) = if video {
        (attach::MAX_VIDEO, "videos")
    } else {
        (attach::MAX_FILE, "files")
    };
    if let Some(length) = length.filter(|l| *l > cap) {
        return upload_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &attach::too_big(&name, length, cap, what),
        );
    }
    let id = attach::new_upload_id();
    let (path, file) = match inbox.incoming(&id) {
        Ok(made) => made,
        Err(err) => {
            tracing::warn!(%err, "couldn't save an upload");
            return upload_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "oxbot couldn't save the file.",
            );
        }
    };
    // However this ends (even if the page goes away), the partial file goes.
    let _partial = RemoveOnDrop(path.clone());
    if let Err(refused) = receive(body, file, &name, cap.max(attach::MAX_VIDEO)).await {
        return refused;
    }
    // Checked and converted on its own, so it's never left half done.
    let hub = Arc::clone(&state.hub);
    let job = tokio::spawn(async move {
        hub.accept_upload(&bot_id, &inbox, &viewer, &path, &id, &name)
            .await
    });
    match job.await {
        Ok(Ok(attachment)) => axum::Json(attachment).into_response(),
        Ok(Err(err)) => {
            let status = match err {
                attach::AttachError::TooBig(_) => StatusCode::PAYLOAD_TOO_LARGE,
                attach::AttachError::Unsupported(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
                attach::AttachError::Unreadable(_) => StatusCode::UNPROCESSABLE_ENTITY,
                attach::AttachError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            upload_error(status, &err.to_string())
        }
        Err(_) => upload_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oxbot couldn't take the file.",
        ),
    }
}

/// Writes an upload's body to `file`, at most `cap` bytes.
#[allow(
    clippy::result_large_err,
    reason = "the refusal is the response to send"
)]
async fn receive(
    body: axum::body::Body,
    file: std::fs::File,
    name: &str,
    cap: u64,
) -> Result<(), Response> {
    use tokio::io::AsyncWriteExt as _;
    let mut file = tokio::fs::File::from_std(file);
    let mut stream = body.into_data_stream();
    let mut written: u64 = 0;
    let failed = |_| {
        upload_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oxbot couldn't save the file.",
        )
    };
    loop {
        let chunk = match tokio::time::timeout(UPLOAD_IDLE, stream.next()).await {
            Err(_) => {
                return Err(upload_error(
                    StatusCode::REQUEST_TIMEOUT,
                    "The upload stopped coming in.",
                ));
            }
            Ok(None) => break,
            Ok(Some(Err(_))) => {
                return Err(upload_error(
                    StatusCode::BAD_REQUEST,
                    "The upload was cut off.",
                ));
            }
            Ok(Some(Ok(chunk))) => chunk,
        };
        written += chunk.len() as u64;
        if written > cap {
            return Err(upload_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                &oxbot_core::attach::too_big(name, written, cap, "videos"),
            ));
        }
        file.write_all(&chunk).await.map_err(failed)?;
    }
    file.flush().await.map_err(failed)?;
    Ok(())
}

/// Deletes a file when dropped.
struct RemoveOnDrop(std::path::PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn upload_error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": message }))).into_response()
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
    /// This connection's number (for dictation).
    conn: u64,
    /// The Farm owner (the Farm, or the Farm's person on their Gate): gets
    /// comparisons of engines and models.
    owner: bool,
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
            conn: 0,
            owner: false,
        }
    }

    /// Takes a new bot list: remembers which bots this client may see, and
    /// drops subscriptions to the rest.
    fn see(&mut self, bots: &[oxbot_protocol::BotInfo]) {
        self.visible = bots
            .iter()
            .filter(|b| self.viewer.can_see_info(b))
            // The Farm's Herder is only for those whose Herder it is (as the
            // hub's visible_ids says).
            .filter(|b| b.id != DEFAULT_BOT || self.herder == DEFAULT_BOT)
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
            ClientMsg::Send { bot, text, files } => ClientMsg::Send {
                bot: self.inner(bot),
                text,
                files,
            },
            ClientMsg::DropUpload { bot, id } => ClientMsg::DropUpload {
                bot: self.inner(bot),
                id,
            },
            ClientMsg::OpenFile { bot, seq, id } => ClientMsg::OpenFile {
                bot: self.inner(bot),
                seq,
                id,
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
            ClientMsg::ListAllowRules { bot } => ClientMsg::ListAllowRules {
                bot: self.inner(bot),
            },
            ClientMsg::RemoveAllowRule { bot, id } => ClientMsg::RemoveAllowRule {
                bot: self.inner(bot),
                id,
            },
            ClientMsg::MoveBot { bot, section } => ClientMsg::MoveBot {
                bot: self.inner(bot),
                section,
            },
            ClientMsg::CreateSection { name, bot } => ClientMsg::CreateSection {
                name,
                bot: bot.map(|b| self.inner(b)),
            },
            ClientMsg::OpenReport {
                bot,
                id,
                utc_offset_min,
            } => ClientMsg::OpenReport {
                bot: self.inner(bot),
                id,
                utc_offset_min,
            },
            ClientMsg::ListReports { bot } => ClientMsg::ListReports {
                bot: self.inner(bot),
            },
            ClientMsg::DeleteReport { bot, id } => ClientMsg::DeleteReport {
                bot: self.inner(bot),
                id,
            },
            other => other,
        }
    }

    /// The engines view with bot ids as this client sees them.
    fn outer_engines(&self, mut view: EnginesView) -> EnginesView {
        if let Some(or) = &mut view.openrouter {
            or.usage.retain_mut(|u| match &u.bot {
                Some(bot) => self.outer(bot).map(|b| u.bot = Some(b)).is_some(),
                None => true,
            });
        }
        view.review = view
            .review
            .into_iter()
            .filter_map(|mut r| {
                r.bot = self.outer(&r.bot)?;
                Some(r)
            })
            .collect();
        view
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
            ServerMsg::AllowRules { bot, rules } => ServerMsg::AllowRules {
                bot: self.outer(&bot)?,
                rules,
            },
            ServerMsg::Report(mut r) => {
                r.bot = self.outer(&r.bot)?;
                ServerMsg::Report(r)
            }
            ServerMsg::Reports { bot, reports } => ServerMsg::Reports {
                bot: self.outer(&bot)?,
                reports,
            },
            ServerMsg::Bots { bots } => ServerMsg::Bots {
                bots: bots
                    .into_iter()
                    .filter_map(|mut b| {
                        b.id = self.outer(&b.id)?;
                        Some(b)
                    })
                    .collect(),
            },
            ServerMsg::Sections {
                sections,
                placed,
                owner_id,
            } => ServerMsg::Sections {
                sections,
                placed: placed
                    .into_iter()
                    .filter_map(|(bot, section)| Some((self.outer(&bot)?, section)))
                    .collect(),
                owner_id,
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
            other => return self.outbound_engines(other),
        })
    }

    /// [`Client::outbound`] for the engines view and turn meters.
    fn outbound_engines(&self, msg: ServerMsg) -> Option<ServerMsg> {
        Some(match msg {
            ServerMsg::Engines(view) => ServerMsg::Engines(self.outer_engines(view)),
            ServerMsg::Meter {
                bot,
                model,
                requests,
                cost_usd,
            } => ServerMsg::Meter {
                bot: self.outer(&bot)?,
                model,
                requests,
                cost_usd,
            },
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
    let conn = state
        .next_conn
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    // Dictation stops with the connection that started it.
    let _dictation = StopDictation(Arc::clone(&state.dictation), conn);
    serve_client(
        socket,
        &state,
        viewer,
        conn,
        gate.as_deref(),
        removals.as_mut(),
    )
    .await;
}

async fn serve_client(
    socket: WebSocket,
    state: &AppState,
    viewer: Viewer,
    conn: u64,
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
    client.conn = conn;
    client.owner = state.hub.is_owner_viewer(&client.viewer);

    let mut logs = state.logs.as_ref().map(|l| l.subscribe());
    let mut first = vec![
        ServerMsg::Hello(ServerHello::new(&state.server_name, state.started_ms)),
        ServerMsg::Settings(state.settings_for(&client.viewer)),
    ];
    first.push(ServerMsg::Setup(state.setup_report()));
    first.extend(usage_now(&state, &mut client));
    if let Ok(people) = state.hub.people() {
        first.push(ServerMsg::People { people });
    }
    first.extend(state.hub.sites_for(&client.viewer));
    // Always (even with none), so the page knows where it stands.
    first.extend(state.hub.sections_for(&client.viewer));
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
        let Some(msg) = for_viewer(msg, &state, &client.viewer).and_then(|m| client.outbound(m))
        else {
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
            let Some(msg) =
                for_viewer(msg, &state, &client.viewer).and_then(|m| client.outbound(m))
            else {
                continue;
            };
            if send(&mut sink, &msg).await.is_err() {
                return;
            }
        }
    }
}

/// Usage as `client` may see it, then the usage alerts still live (so the
/// banner survives a reload).
fn usage_now(state: &AppState, client: &mut Client) -> Vec<ServerMsg> {
    let usage = state.hub.usage().ok().map(ServerMsg::Usage);
    usage
        .and_then(|u| forward(u, client))
        .into_iter()
        .chain(
            state
                .hub
                .live_usage_alerts()
                .into_iter()
                .map(ServerMsg::UsageAlert),
        )
        .collect()
}

/// Stops a connection's dictation when it ends.
struct StopDictation(Arc<crate::dictation::Dictation>, u64);

impl Drop for StopDictation {
    fn drop(&mut self) {
        self.0.stop(self.1);
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
    // Comparing engines and models: the owner's only (the hub checks).
    let msg = match crate::compare::handle(msg, &state.hub, &viewer).await {
        Ok(reply) => return reply,
        Err(other) => other,
    };
    let msg = match handle_farm(msg, state).await {
        Ok(reply) => return reply,
        Err(other) => other,
    };
    let msg = match handle_messages(msg, state, client, out).await {
        Ok(reply) => return reply,
        Err(other) => other,
    };
    let msg = match handle_engines(msg, state, &viewer).await {
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
        ClientMsg::Cancel { bot } => match state.hub.bot_for(&bot, &viewer) {
            Ok(_) => reply(state.hub.cancel(&bot).await),
            Err(e) => error(&e),
        },
        ClientMsg::CreateBot {
            name,
            instructions,
            private,
            engine,
        } => create_bot(state, client, &name, &instructions, private, engine).await,
        ClientMsg::SetPrivate { bot, private } => {
            reply(state.hub.set_private(&bot, private, &viewer).await)
        }
        ClientMsg::RenameBot { bot, name } => {
            reply(state.hub.rename_bot(&bot, &name, &viewer).await)
        }
        ClientMsg::DeleteBot { bot } => reply(state.hub.delete_bot(&bot, &viewer).await),
        ClientMsg::Answer { id, answer } => match state.hub.answer_as(id, answer, &viewer) {
            Ok(_) => Vec::new(),
            Err(e) => error(&e),
        },
        ClientMsg::ListAllowRules { bot } => match state.hub.allow_rules_for(&bot, &viewer) {
            Ok(rules) => vec![ServerMsg::AllowRules { bot, rules }],
            Err(e) => error(&e),
        },
        ClientMsg::RemoveAllowRule { bot, id } => {
            reply(state.hub.remove_allow_rule(&bot, id, &viewer))
        }
        // Sections are the viewer's own view: anyone oxbot can place.
        ClientMsg::CreateSection { name, bot } => {
            reply(state.hub.create_section(&name, bot.as_deref(), &viewer))
        }
        ClientMsg::RenameSection { id, name } => {
            reply(state.hub.rename_section(&id, &name, &viewer))
        }
        ClientMsg::DeleteSection { id } => reply(state.hub.delete_section(&id, &viewer)),
        ClientMsg::MoveBot { bot, section } => {
            reply(state.hub.move_bot(&bot, section.as_deref(), &viewer))
        }
        ClientMsg::OpenReport { .. }
        | ClientMsg::ListReports { .. }
        | ClientMsg::DeleteReport { .. } => handle_reports(msg, state, client),
        other => handle_sign_ins(other, state, &viewer).await,
    }
}

/// Switching a bot's engine and its own daily limit (the hub decides who
/// may). Gives other messages back.
async fn handle_engines(
    msg: ClientMsg,
    state: &AppState,
    viewer: &Viewer,
) -> Result<Vec<ServerMsg>, ClientMsg> {
    Ok(match msg {
        ClientMsg::SetBotDailyLimit {
            bot,
            daily_requests,
        } => reply(
            state
                .hub
                .set_bot_daily_limit(&bot, daily_requests, viewer)
                .await,
        ),
        ClientMsg::SetEngine {
            bot,
            engine,
            model,
            confirm_owner_only,
        } => {
            let done = state
                .hub
                .set_engine(&bot, engine_kind(engine), model, viewer, confirm_owner_only)
                .await;
            if done.is_ok() {
                // The owner's review list changes with it.
                state
                    .hub
                    .announce(ServerMsg::Engines(state.engines_for(viewer)));
            }
            reply(done)
        }
        other => return Err(other),
    })
}

/// Sending messages (with files), the Mac's dictation, and the setup
/// check. Gives other messages back.
async fn handle_messages(
    msg: ClientMsg,
    state: &AppState,
    client: &Client,
    out: &mpsc::Sender<ServerMsg>,
) -> Result<Vec<ServerMsg>, ClientMsg> {
    let viewer = &client.viewer;
    Ok(match msg {
        ClientMsg::Send { bot, text, files } => {
            if let Err(e) = state.hub.bot_for(&bot, viewer) {
                return Ok(reply(Err(e)));
            }
            // Sending may wait for a free turn slot; don't block this connection.
            tokio::spawn(send_as(
                Arc::clone(&state.hub),
                Outgoing { bot, text, files },
                viewer.clone(),
                out.clone(),
            ));
            Vec::new()
        }
        ClientMsg::DropUpload { bot, id } => reply(state.hub.drop_upload(&bot, &id, viewer)),
        ClientMsg::OpenFile { bot, seq, id } => open_file(state, &bot, seq, &id, viewer),
        ClientMsg::Dictate { on } => {
            if on {
                state.dictation.start(client.conn, out.clone());
            } else {
                state.dictation.stop(client.conn);
            }
            Vec::new()
        }
        ClientMsg::ListEngines => vec![ServerMsg::Engines(state.engines_for(viewer))],
        ClientMsg::CheckSetup => vec![
            ServerMsg::Setup(state.refresh_setup().await),
            // Whether the mic shows may have changed.
            ServerMsg::Settings(state.settings_for(viewer)),
        ],
        other => return Err(other),
    })
}

/// Opens, lists and deletes a bot's research reports. A report goes only
/// to someone who may open its bot (as its chat does); nothing of it is
/// read before that's known.
fn handle_reports(msg: ClientMsg, state: &AppState, client: &Client) -> Vec<ServerMsg> {
    let viewer = &client.viewer;
    let hidden = |bot: &str| {
        vec![ServerMsg::Error {
            message: oxbot_core::HubError::UnknownBot(bot.to_owned()).to_string(),
        }]
    };
    let result = match msg {
        ClientMsg::OpenReport {
            bot,
            id,
            utc_offset_min,
        } => {
            if !client.sees(&bot) {
                return hidden(&bot);
            }
            state
                .hub
                .report_for(&bot, &id, viewer, utc_offset_min.unwrap_or(0))
                .map(|view| vec![ServerMsg::Report(view)])
        }
        ClientMsg::ListReports { bot } => {
            if !client.sees(&bot) {
                return hidden(&bot);
            }
            state
                .hub
                .reports_for(&bot, viewer)
                .map(|reports| vec![ServerMsg::Reports { bot, reports }])
        }
        ClientMsg::DeleteReport { bot, id } => state
            .hub
            .delete_report(&bot, &id, viewer)
            .map(|reports| vec![ServerMsg::Reports { bot, reports }]),
        _ => Ok(Vec::new()),
    };
    result.unwrap_or_else(|e| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    })
}

/// Sends a bot's history: replayed events ([`ClientMsg::Subscribe`]) or a
/// page ([`ClientMsg::OpenBot`], [`ClientMsg::Older`]). Subscribing and
/// opening also stream the bot's new events from then on. A subscriber that
/// missed more than a page gets the latest page instead of every event (it
/// pages back as it needs to), so catching up costs the same however long
/// it was away.
fn handle_history(msg: ClientMsg, state: &AppState, client: &mut Client) -> Vec<ServerMsg> {
    let viewer = &client.viewer;
    let result = match msg {
        ClientMsg::Subscribe { bot, since } => state
            .hub
            .replay_for(&bot, since, page_size(Some(MAX_PAGE)), viewer)
            .map(|replay| {
                client.visible.insert(bot.clone());
                match replay {
                    Replay::Events(events) => {
                        let last = events.last().map_or(since, |e| e.seq);
                        client.subs.insert(bot, last);
                        events.into_iter().map(ServerMsg::Event).collect()
                    }
                    Replay::Page(page) => {
                        // Live events follow on from the end of the page.
                        client.subs.insert(bot.clone(), page.last_seq);
                        vec![history(bot, page)]
                    }
                }
            }),
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
        ClientMsg::SetUsageAlerts { on } => match state.hub.set_usage_alerts(on) {
            Ok(()) => vec![ServerMsg::Settings(state.settings())],
            Err(e) => error(&e),
        },
        ClientMsg::SetNotifyWhenHidden { on } => match state.hub.set_notify_when_hidden(on) {
            Ok(()) => vec![ServerMsg::Settings(state.settings())],
            Err(e) => error(&e),
        },
        ClientMsg::EnginesNoticeSeen => match state.hub.set_engines_notice_seen() {
            Ok(()) => vec![ServerMsg::Engines(state.engines_for(&state.farm_viewer()))],
            Err(e) => error(&e),
        },
        ClientMsg::OpenRouterSignIn
        | ClientMsg::OpenRouterSignOut
        | ClientMsg::RefreshModels
        | ClientMsg::SetEnginePrefs { .. } => handle_openrouter(msg, state).await,
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

/// The household's `OpenRouter` account: sign in and out, the model list,
/// the settings. The Farm only (checked by the caller).
async fn handle_openrouter(msg: ClientMsg, state: &AppState) -> Vec<ServerMsg> {
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    let account = state.openrouter.clone();
    match msg {
        ClientMsg::OpenRouterSignIn => {
            let Some(account) = account else {
                return error(&"OpenRouter isn't available in this oxbot.");
            };
            match account.start_sign_in(state.port) {
                Ok(url) => vec![ServerMsg::OpenRouterAuth { url }],
                Err(e) => error(&e),
            }
        }
        ClientMsg::OpenRouterSignOut => {
            let Some(account) = account else {
                return Vec::new();
            };
            let done = account.sign_out().await;
            state.hub.stop_engine_sessions(EngineKind::OpenRouter).await;
            state.openrouter_changed();
            match done {
                Ok(()) => vec![ServerMsg::Notice {
                    message: format!(
                        "Signed out of OpenRouter. To revoke the key itself, delete \"oxbot on {}\" at openrouter.ai/keys.",
                        state.server_name
                    ),
                    owner: None,
                    owner_id: None,
                }],
                Err(e) => error(&e),
            }
        }
        ClientMsg::RefreshModels => {
            // Reading the list can take a while: don't hold the connection.
            let state = state.clone();
            tokio::spawn(async move {
                if let Err(why) = state.refresh_models().await {
                    tracing::warn!(why, "model list not refreshed");
                }
            });
            Vec::new()
        }
        ClientMsg::SetEnginePrefs { prefs } => match state.hub.set_engine_prefs(prefs) {
            Ok(_) => {
                state.openrouter_changed();
                Vec::new()
            }
            Err(e) => error(&e),
        },
        _ => Vec::new(),
    }
}

/// A model as listed in Settings → Engines.
fn model_row(m: &oxbot_llm::ModelInfo) -> ModelRow {
    ModelRow {
        id: m.id.clone(),
        name: m.name.clone(),
        context_length: m.context_length,
        usd_per_mtok_in: m.prompt_usd_per_token * 1e6,
        usd_per_mtok_out: m.completion_usd_per_token * 1e6,
    }
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
/// networks phones use, change the Mac's setup and its notifications.
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
            | ClientMsg::SetUsageAlerts { .. }
            | ClientMsg::SetNotifyWhenHidden { .. }
            | ClientMsg::EnginesNoticeSeen
            // The household's OpenRouter account and settings.
            | ClientMsg::OpenRouterSignIn
            | ClientMsg::OpenRouterSignOut
            | ClientMsg::RefreshModels
            | ClientMsg::SetEnginePrefs { .. }
            // The Mac's own microphone, and opening files on its screen.
            | ClientMsg::Dictate { .. }
            | ClientMsg::OpenFile { .. }
    )
}

/// Opens a file sent in a chat on this Mac's screen, in the app macOS
/// picks for it. Found from the stored message, never from a path the
/// page sends.
fn open_file(state: &AppState, bot: &str, seq: u64, id: &str, viewer: &Viewer) -> Vec<ServerMsg> {
    match state.hub.attachment_file(bot, seq, id, viewer) {
        Ok((file, path)) => {
            // Tests never open windows on the Mac they run on.
            if !state.e2e {
                // Text (whatever its name says) opens in the text editor, and a
                // web archive only shows in Finder: a file someone sent never
                // runs as a script or a page.
                let how = match file.kind {
                    oxbot_protocol::AttachmentKind::Text => Some("-t"),
                    _ if path.extension().is_some_and(|e| e == "webarchive") => Some("-R"),
                    _ => None,
                };
                tokio::spawn(async move {
                    let status = tokio::process::Command::new("/usr/bin/open")
                        .args(how)
                        .arg(&path)
                        .stdin(std::process::Stdio::null())
                        .status()
                        .await;
                    if !status.as_ref().is_ok_and(std::process::ExitStatus::success) {
                        tracing::warn!(?status, "couldn't open a file sent in a chat");
                    }
                });
            }
            vec![ServerMsg::Notice {
                message: format!("Opening {} on this Mac.", file.name),
                owner: None,
                owner_id: None,
            }]
        }
        Err(e) => vec![ServerMsg::Error {
            message: e.to_string(),
        }],
    }
}

/// Makes a bot for the client on `engine` (the server's default if
/// `None`). Claude only for the Farm's owner; `OpenRouter` only once it's
/// set up. Checked before anything is made.
async fn create_bot(
    state: &AppState,
    client: &mut Client,
    name: &str,
    instructions: &str,
    private: bool,
    engine: Option<oxbot_protocol::EngineKind>,
) -> Vec<ServerMsg> {
    let viewer = client.viewer.clone();
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    let wanted = engine.map(engine_kind);
    match wanted {
        Some(EngineKind::Claude) if !state.hub.is_owner_viewer(&viewer) => {
            let owner = state.hub.owner_display_name();
            return error(&format!(
                "Only {owner} can make Claude bots: they run on {owner}'s own Claude plan."
            ));
        }
        Some(EngineKind::OpenRouter) => {
            if let Err(why) = state.hub.engine_ready(EngineKind::OpenRouter) {
                return error(&why);
            }
        }
        _ => {}
    }
    let info = match state
        .hub
        .create_bot_as(name, instructions, Some(&viewer), private)
        .await
    {
        Ok(info) => info,
        Err(e) => return error(&e),
    };
    client.visible.insert(info.id.clone());
    let mut out = Vec::new();
    if let Some(wanted) = wanted.filter(|w| *w != engine_kind(info.engine)) {
        // The person picked it in the dialog, which said what Claude means.
        if let Err(e) = state
            .hub
            .set_engine(&info.id, wanted, None, &viewer, true)
            .await
        {
            out.push(ServerMsg::Error {
                message: format!("Made {}, but couldn't switch its engine: {e}", info.name),
            });
        }
    }
    let info = match state.hub.bots().await {
        Ok(list) => list.into_iter().find(|b| b.id == info.id).unwrap_or(info),
        Err(_) => info,
    };
    out.splice(
        0..0,
        viewer
            .redact(vec![info])
            .into_iter()
            .map(ServerMsg::BotCreated),
    );
    out
}

/// The engine as the hub names it.
fn engine_kind(engine: oxbot_protocol::EngineKind) -> EngineKind {
    match engine {
        oxbot_protocol::EngineKind::Claude => EngineKind::Claude,
        oxbot_protocol::EngineKind::OpenRouter => EngineKind::OpenRouter,
    }
}

/// A message on its way: to `bot`, `text` with the uploaded `files`.
struct Outgoing {
    bot: String,
    text: String,
    files: Vec<String>,
}

/// Sends a message from `from`, reporting a failure to the client.
async fn send_as(hub: Arc<Hub>, msg: Outgoing, from: Viewer, out: mpsc::Sender<ServerMsg>) {
    if let Err(e) = hub
        .send_files_as(&msg.bot, &msg.text, &msg.files, &from)
        .await
    {
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

/// What only the hub can decide for one viewer, just before sending: on a
/// card, whether they're offered "Always allow"; a bot's rules go only to
/// those who may manage it.
fn for_viewer(msg: ServerMsg, state: &AppState, viewer: &Viewer) -> Option<ServerMsg> {
    match msg {
        ServerMsg::ApprovalNeeded(mut a) => {
            a.always = state.hub.can_always(&a, viewer);
            Some(ServerMsg::ApprovalNeeded(a))
        }
        ServerMsg::AllowRules { ref bot, .. } => state.hub.may_manage(bot, viewer).then_some(msg),
        // Settings and the engines view are made for each viewer: one sent
        // to everyone is a cue to send them their own.
        ServerMsg::Settings(_) => Some(ServerMsg::Settings(state.settings_for(viewer))),
        ServerMsg::Engines(_) => Some(ServerMsg::Engines(state.engines_for(viewer))),
        other => Some(other),
    }
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
        // A bot's rules also go only to those who may manage it: see
        // [`for_viewer`].
        ServerMsg::AllowRules { ref bot, .. } if client.sees(bot) => Some(msg),
        ServerMsg::AllowRules { .. } => None,
        ServerMsg::Report(ref r) => client.sees(&r.bot).then_some(msg),
        ServerMsg::Bots { bots } => {
            client.see(&bots);
            Some(ServerMsg::Bots {
                bots: client.viewer.redact(bots),
            })
        }
        ServerMsg::BotCreated(ref b) if !client.sees(&b.id) => None,
        // History and report lists are direct answers, listed so one never
        // reaches a client that doesn't see its bot.
        ServerMsg::History { ref bot, .. }
        | ServerMsg::Meter { ref bot, .. }
        | ServerMsg::Activity { ref bot, .. }
        | ServerMsg::Browser { ref bot, .. }
        | ServerMsg::Reports { ref bot, .. } => client.sees(bot).then_some(msg),
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
        // A person's sections are their own view: only their connections
        // get them, by id. Not the Farm: it manages bots, not people's views.
        ServerMsg::Sections { ref owner_id, .. } => client
            .viewer
            .is_person(owner_id.as_deref(), None)
            .then_some(msg),
        // The log names every bot, site and person; pairing is the Farm's
        // job: the Farm's only.
        // Dictation goes straight to the connection that asked; if it were
        // ever broadcast, only the Farm could have asked.
        ServerMsg::Log(_)
        | ServerMsg::OpenRouterAuth { .. }
        | ServerMsg::Dictation { .. }
        | ServerMsg::Pairing(_)
        | ServerMsg::PairingClosed
        | ServerMsg::Gates { .. }
        | ServerMsg::Door(_) => client.viewer.farm.then_some(msg),
        // Comparisons of engines and models: the owner's alone. Practice
        // runs are never a chat's events.
        ServerMsg::Ratings { .. }
        | ServerMsg::CaseDraft(_)
        | ServerMsg::TestCases { .. }
        | ServerMsg::BatchEstimate(_)
        | ServerMsg::BatchProgress { .. }
        | ServerMsg::TrialRun(_)
        | ServerMsg::TrialEvent { .. }
        | ServerMsg::Runs { .. }
        | ServerMsg::RunDetail { .. }
        | ServerMsg::Scoreboard { .. }
        | ServerMsg::ShadowPair { .. }
        | ServerMsg::ComparePrefs(_) => client.owner.then_some(msg),
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
        // Plan limits already go to everyone in `Usage`; an alert names no
        // bot, site or person.
        | ServerMsg::UsageAlert(_)
        // Made for each viewer in [`for_viewer`].
        | ServerMsg::Engines(_)
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
                files: vec![],
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
    fn practice_runs_reach_the_owner_alone_and_never_a_chat() {
        let trial = || ServerMsg::TrialEvent {
            run: "r_1".into(),
            event: BotEvent {
                bot: "r_1".into(),
                seq: 1,
                ts_ms: 0,
                kind: EventKind::DryRun {
                    action: "click".into(),
                    target: "button \"Post\"".into(),
                    site: "jobs.example.test".into(),
                    click: None,
                },
            },
        };
        let mut sam = Client::new(Viewer::gate("Sam"), &[]);
        assert!(forward(trial(), &mut sam).is_none());
        // Even the Farm connection gets them only as the owner.
        let mut farm = Client::new(Viewer::farm("Jay"), &[]);
        assert!(forward(trial(), &mut farm).is_none());
        farm.owner = true;
        assert!(forward(trial(), &mut farm).is_some());
        // A practice run's key is never a bot anyone is subscribed to.
        farm.subs.insert("jobs".to_owned(), 0);
        assert!(forward(event("t--0123456789abcdef", 1), &mut farm).is_none());
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

        // A report never reaches someone who doesn't see its bot.
        let report = ServerMsg::Report(oxbot_protocol::ReportView {
            bot: "mine".into(),
            id: "r_1".into(),
            title: "SECRET".into(),
            created_ms: 0,
            html: "SECRET".into(),
        });
        assert!(forward(report.clone(), &mut ann).is_none());
        assert!(forward(report, &mut farm).is_some());
        let list = ServerMsg::Reports {
            bot: "mine".into(),
            reports: vec![],
        };
        assert!(forward(list.clone(), &mut ann).is_none());
        assert!(forward(list, &mut farm).is_some());

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
            always: false,
            rule: None,
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
            files: vec![],
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

    /// A server with Sam's shared bot `shop`; Jay is the Farm's person.
    fn shop_state(dir: &std::path::Path) -> AppState {
        let store = oxbot_core::Store::open_in_memory().unwrap();
        let sam = store.find_or_create_person("Sam").unwrap().0.id;
        store.find_or_create_person("Jay").unwrap();
        let mut shop =
            oxbot_core::Bot::create(dir, oxbot_core::BotId::new("shop").unwrap(), "Shop", "")
                .unwrap();
        shop.owner = Some(sam);
        shop.created_by = Some("Sam".into());
        shop.save().unwrap();
        let hub = Hub::new(vec![shop], store, oxbot_core::HubConfig::new(dir));
        AppState::new(hub, "test-mac")
    }

    fn save_card(asker: &str) -> oxbot_protocol::Approval {
        oxbot_protocol::Approval {
            asker: Some(asker.into()),
            rule: Some("button \"Save\"".into()),
            ..match card("shop", None) {
                ServerMsg::ApprovalNeeded(a) => a,
                _ => unreachable!(),
            }
        }
    }

    fn offered(state: &AppState, card: oxbot_protocol::Approval, viewer: &Viewer) -> bool {
        match for_viewer(ServerMsg::ApprovalNeeded(card), state, viewer) {
            Some(ServerMsg::ApprovalNeeded(a)) => a.always,
            other => panic!("a card, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn always_is_offered_only_to_those_who_manage_the_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let state = shop_state(tmp.path());
        let sam = state.hub.resolve(&Viewer::gate("Sam")).into_owned();
        let jay = state.hub.resolve(&Viewer::gate("Jay")).into_owned();
        let with_id = |asker: &Viewer| oxbot_protocol::Approval {
            asker_id: asker.id.as_ref().map(ToString::to_string),
            ..save_card(&asker.name)
        };
        // Sam's own card on Sam's bot: offered to Sam and the Farm.
        assert!(offered(&state, with_id(&sam), &Viewer::gate("Sam")));
        assert!(offered(&state, with_id(&sam), &Viewer::farm("Jay")));
        // Jay's card on Sam's bot: Jay may only allow it once.
        assert!(!offered(&state, with_id(&jay), &Viewer::gate("Jay")));
        // A card that names no rule (pay, Enter, upload) is never offered.
        let plain = oxbot_protocol::Approval {
            rule: None,
            ..with_id(&sam)
        };
        assert!(!offered(&state, plain, &Viewer::farm("Jay")));
        // The rule itself is never sent.
        let json = serde_json::to_string(&with_id(&sam)).unwrap();
        assert!(!json.contains("Save"), "{json}");

        // Rule lists go only to those who manage the bot.
        let rules = || ServerMsg::AllowRules {
            bot: "shop".into(),
            rules: Vec::new(),
        };
        assert!(for_viewer(rules(), &state, &Viewer::gate("Sam")).is_some());
        assert!(for_viewer(rules(), &state, &Viewer::farm("Jay")).is_some());
        assert!(for_viewer(rules(), &state, &Viewer::gate("Jay")).is_none());
    }

    #[tokio::test]
    async fn rules_are_listed_and_removed_only_by_those_who_manage_the_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let state = shop_state(tmp.path());
        let (out, _rx) = mpsc::channel(4);
        let bots = [info("shop", Some("Sam"), false)];
        let mut jay = Client::new(Viewer::gate("Jay"), &bots);
        let mut sam = Client::new(Viewer::gate("Sam"), &bots);
        let list = || ClientMsg::ListAllowRules { bot: "shop".into() };
        let refused = handle(list(), &state, &mut jay, &out).await;
        assert!(
            matches!(&refused[..], [ServerMsg::Error { .. }]),
            "{refused:?}"
        );
        let remove = ClientMsg::RemoveAllowRule {
            bot: "shop".into(),
            id: 1,
        };
        let refused = handle(remove.clone(), &state, &mut jay, &out).await;
        assert!(
            matches!(&refused[..], [ServerMsg::Error { .. }]),
            "{refused:?}"
        );
        let listed = handle(list(), &state, &mut sam, &out).await;
        assert!(
            matches!(&listed[..], [ServerMsg::AllowRules { bot, rules }] if bot == "shop" && rules.is_empty()),
            "{listed:?}"
        );
        assert_eq!(handle(remove, &state, &mut sam, &out).await, []);
        // Always on a card that isn't open (or isn't theirs) does nothing.
        let answer = ClientMsg::Answer {
            id: 99,
            answer: oxbot_protocol::Answer::AlwaysAllow,
        };
        assert_eq!(handle(answer, &state, &mut sam, &out).await, []);
    }

    #[test]
    fn rule_requests_for_assistant_go_to_your_own_herder() {
        let ann = Client::with_herder(Viewer::gate("Ann"), &[], "assistant--ann-1".into());
        let ClientMsg::ListAllowRules { bot } = ann.inbound(ClientMsg::ListAllowRules {
            bot: DEFAULT_BOT.into(),
        }) else {
            panic!("a list request")
        };
        assert_eq!(bot, "assistant--ann-1");
        let ClientMsg::RemoveAllowRule { bot, id } = ann.inbound(ClientMsg::RemoveAllowRule {
            bot: DEFAULT_BOT.into(),
            id: 4,
        }) else {
            panic!("a remove request")
        };
        assert_eq!((bot.as_str(), id), ("assistant--ann-1", 4));
        let Some(ServerMsg::AllowRules { bot, .. }) = ann.outbound(ServerMsg::AllowRules {
            bot: "assistant--ann-1".into(),
            rules: Vec::new(),
        }) else {
            panic!("her Herder's rules")
        };
        assert_eq!(bot, DEFAULT_BOT);
    }

    #[test]
    fn a_persons_sections_go_to_them_alone_with_herder_ids_as_they_see_them() {
        let sam = oxbot_core::id::PersonId::random();
        let anns = oxbot_core::bot::personal_herder_id("Ann").to_string();
        let sams = oxbot_core::bot::personal_herder_id("Sam").to_string();
        let sections = ServerMsg::Sections {
            sections: vec![oxbot_protocol::SectionInfo {
                id: "s_1".into(),
                name: "Kitchen".into(),
            }],
            placed: [
                ("recipes".to_owned(), "s_1".to_owned()),
                (sams.clone(), "s_1".to_owned()),
                (anns.clone(), "s_1".to_owned()),
                (DEFAULT_BOT.to_owned(), "s_1".to_owned()),
            ]
            .into(),
            owner_id: Some(sam.to_string()),
        };
        let mut real =
            Client::with_herder(Viewer::person(sam.clone(), "Sam", false), &[], sams.clone());
        let mut farm = Client::new(
            Viewer::person(oxbot_core::id::PersonId::random(), "Jay", true),
            &[],
        );
        let mut by_name = Client::new(Viewer::gate("Sam"), &[]);
        let mut nobody = Client::new(Viewer::farm(""), &[]);
        assert!(
            forward(sections.clone(), &mut farm).is_none(),
            "not even the Farm"
        );
        assert!(forward(sections.clone(), &mut by_name).is_none());
        assert!(forward(sections.clone(), &mut nobody).is_none());
        let msg = forward(sections, &mut real).unwrap();
        let Some(ServerMsg::Sections { placed, .. }) = real.outbound(msg) else {
            panic!("sections")
        };
        let bots: Vec<&str> = placed.keys().map(String::as_str).collect();
        assert_eq!(
            bots,
            [DEFAULT_BOT, "recipes"],
            "their own Herder as assistant; no other Herder"
        );

        // `assistant` in a message means their own Herder.
        let ClientMsg::MoveBot { bot, .. } = real.inbound(ClientMsg::MoveBot {
            bot: DEFAULT_BOT.into(),
            section: None,
        }) else {
            panic!("a move")
        };
        assert_eq!(bot, sams);
        let ClientMsg::CreateSection { bot, .. } = real.inbound(ClientMsg::CreateSection {
            name: "K".into(),
            bot: Some(DEFAULT_BOT.into()),
        }) else {
            panic!("a new section")
        };
        assert_eq!(bot.as_deref(), Some(sams.as_str()));
    }

    #[tokio::test]
    async fn sections_need_no_farm_rights() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let ann = state.hub.resolve(&Viewer::gate("Ann")).into_owned();
        for msg in [
            ClientMsg::CreateSection {
                name: "Kitchen".into(),
                bot: None,
            },
            ClientMsg::MoveBot {
                bot: "nobody".into(),
                section: None,
            },
        ] {
            assert!(!farm_only(&msg));
        }
        let (out, _rx) = mpsc::channel(4);
        let herder = herder_for(&state, &ann);
        let mut client =
            Client::with_visible(ann.clone(), std::collections::HashSet::new(), herder);
        let replies = handle(
            ClientMsg::CreateSection {
                name: "Kitchen".into(),
                bot: None,
            },
            &state,
            &mut client,
            &out,
        )
        .await;
        assert!(replies.is_empty(), "{replies:?}");
        let Some(ServerMsg::Sections { sections, .. }) = state.hub.sections_for(&ann) else {
            panic!("sections")
        };
        assert_eq!(sections.len(), 1);
        let replies = handle(
            ClientMsg::MoveBot {
                bot: DEFAULT_BOT.into(),
                section: None,
            },
            &state,
            &mut client,
            &out,
        )
        .await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { message }] if message.contains("Herder")),
            "{replies:?}"
        );
    }

    const BANK_REPORT: &str = "r_0123456789abcdef0123456789abcdef";
    const HERDER_REPORT: &str = "r_fedcba9876543210fedcba9876543210";

    fn a_report(id: &str, title: &str) -> oxbot_core::report::Report {
        oxbot_core::report::Report {
            id: id.into(),
            created_ms: 0,
            asked_by: None,
            question: "research my bank".into(),
            title: title.into(),
            summary: "One line.".into(),
            markdown: "Fees are low [1].".into(),
            sources: vec![oxbot_core::report::Source {
                n: 1,
                url: "https://bank.example.com/fees".into(),
                title: "Fees".into(),
                site: "example.com".into(),
                read_ms: 0,
            }],
            links: vec![],
        }
    }

    /// A server with Sam's private bot `bank` and a report in it, and a
    /// report in Ann's own Herder.
    fn bank_state(dir: &std::path::Path) -> AppState {
        let store = oxbot_core::Store::open_in_memory().unwrap();
        let sam = store.find_or_create_person("Sam").unwrap().0.id;
        store.find_or_create_person("Jay").unwrap();
        let (ann, _) = store.find_or_create_person("Ann").unwrap();
        let mut bank =
            oxbot_core::Bot::create(dir, oxbot_core::BotId::new("bank").unwrap(), "Bank", "")
                .unwrap();
        bank.owner = Some(sam);
        bank.created_by = Some("Sam".into());
        bank.private = true;
        bank.save().unwrap();
        store
            .add_report(&bank.id, &mut a_report(BANK_REPORT, "SECRET-REPORT"))
            .unwrap();
        store
            .add_report(&ann.herder, &mut a_report(HERDER_REPORT, "Ann's research"))
            .unwrap();
        let central = oxbot_core::Bot::create(
            dir,
            oxbot_core::BotId::new(DEFAULT_BOT).unwrap(),
            "Herder",
            "",
        )
        .unwrap();
        let hub = Hub::new(vec![central, bank], store, oxbot_core::HubConfig::new(dir));
        AppState::new(hub, "test-mac")
    }

    fn client_for(state: &AppState, viewer: &Viewer) -> Client {
        let viewer = state.hub.resolve(viewer).into_owned();
        let herder = herder_for(state, &viewer);
        let visible = state.hub.visible_ids(&viewer);
        Client::with_visible(viewer, visible, herder)
    }

    #[tokio::test]
    async fn a_report_opens_only_for_those_who_see_its_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let state = bank_state(tmp.path());
        let (out, _rx) = mpsc::channel(4);
        let open = |bot: &str, id: &str| ClientMsg::OpenReport {
            bot: bot.into(),
            id: id.into(),
            utc_offset_min: Some(60),
        };
        for viewer in [Viewer::farm("Jay"), Viewer::gate("Sam")] {
            let mut client = client_for(&state, &viewer);
            let got = handle(open("bank", BANK_REPORT), &state, &mut client, &out).await;
            let [ServerMsg::Report(view)] = &got[..] else {
                panic!("a report: {got:?}")
            };
            assert_eq!(view.bot, "bank");
            assert!(view.html.contains("SECRET-REPORT"));
            assert!(view.html.contains("href=\"https://bank.example.com/fees\""));
            let listed = handle(
                ClientMsg::ListReports { bot: "bank".into() },
                &state,
                &mut client,
                &out,
            )
            .await;
            assert!(
                matches!(&listed[..], [ServerMsg::Reports { reports, .. }] if reports.len() == 1),
                "{listed:?}"
            );
        }
        let mut ann = client_for(&state, &Viewer::gate("Ann"));
        for msg in [
            open("bank", BANK_REPORT),
            ClientMsg::ListReports { bot: "bank".into() },
            ClientMsg::DeleteReport {
                bot: "bank".into(),
                id: BANK_REPORT.into(),
            },
        ] {
            let got = handle(msg, &state, &mut ann, &out).await;
            assert!(matches!(&got[..], [ServerMsg::Error { .. }]), "{got:?}");
            assert!(!format!("{got:?}").contains("SECRET"), "{got:?}");
        }
        // An id from another bot finds nothing.
        let got = handle(open(DEFAULT_BOT, BANK_REPORT), &state, &mut ann, &out).await;
        assert!(
            matches!(&got[..], [ServerMsg::Error { message }] if message.contains("deleted")),
            "{got:?}"
        );
        // `assistant` is Ann's own Herder, there and back.
        let got = handle(open(DEFAULT_BOT, HERDER_REPORT), &state, &mut ann, &out).await;
        let [ServerMsg::Report(view)] = &got[..] else {
            panic!("her Herder's report: {got:?}")
        };
        let Some(ServerMsg::Report(view)) = ann.outbound(ServerMsg::Report(view.clone())) else {
            panic!("passed on")
        };
        assert_eq!(view.bot, DEFAULT_BOT);
        assert!(view.html.contains("By Herder"));
        // The Farm's page doesn't see Ann's Herder at all.
        let mut farm = client_for(&state, &Viewer::farm("Jay"));
        let anns = herder_for(&state, &ann.viewer);
        let got = handle(open(&anns, HERDER_REPORT), &state, &mut farm, &out).await;
        assert!(farm.outbound(got[0].clone()).is_none(), "{got:?}");

        // Deleting is for those who manage the bot.
        let mut sam = client_for(&state, &Viewer::gate("Sam"));
        let got = handle(
            ClientMsg::DeleteReport {
                bot: "bank".into(),
                id: BANK_REPORT.into(),
            },
            &state,
            &mut sam,
            &out,
        )
        .await;
        assert!(
            matches!(&got[..], [ServerMsg::Reports { reports, .. }] if reports.is_empty()),
            "{got:?}"
        );
        let got = handle(open("bank", BANK_REPORT), &state, &mut sam, &out).await;
        assert!(
            matches!(&got[..], [ServerMsg::Error { message }] if message == "That report was deleted."),
            "{got:?}"
        );
    }

    /// A server with every engine rule on: Recipes (shared, Claude) and
    /// Sam's own Notes (`OpenRouter`); Jay is the Farm's person.
    fn engines_state(dir: &std::path::Path) -> (AppState, Viewer) {
        engines_state_with(dir, None)
    }

    /// [`engines_state`] with the loop engine on `openrouter`.
    fn engines_state_with(
        dir: &std::path::Path,
        openrouter: Option<oxbot_core::LoopParts>,
    ) -> (AppState, Viewer) {
        use oxbot_core::{Bot, BotId};
        let store = oxbot_core::Store::open(&dir.join("oxbot.db")).unwrap();
        let (sam, _) = store.find_or_create_person("Sam").unwrap();
        let mut notes = Bot::create_owned(
            dir,
            BotId::new("notes").unwrap(),
            "Notes",
            "",
            Some(&sam.id),
            Some("Sam"),
        )
        .unwrap();
        notes.engine = EngineKind::OpenRouter;
        notes.save().unwrap();
        let bots = vec![
            Bot::create(dir, BotId::new("assistant").unwrap(), "Herder", "").unwrap(),
            Bot::create(dir, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
            notes,
        ];
        let config = oxbot_core::HubConfig {
            pre_engine_tests: false,
            openrouter,
            ..oxbot_core::HubConfig::for_app(dir)
        };
        let hub = Hub::new(bots, store, config);
        let state = AppState {
            local_person: "Jay".to_owned(),
            ..AppState::new(hub, "test-mac")
        };
        // Chooses Jay as the Farm's person.
        let _ = state.farm_viewer();
        let sam = Viewer::person(sam.id, "Sam", false);
        (state, sam)
    }

    #[tokio::test]
    async fn only_the_owner_may_task_claude_bots_whoever_sees_the_list() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, sam) = engines_state(tmp.path());
        let farm = state.farm_viewer();
        let jay_phone = Viewer {
            farm: false,
            ..farm.clone()
        };
        let bots = state.hub.bots().await.unwrap();
        let can_task = |viewer: &Viewer| {
            let mut client = Client::new(viewer.clone(), &bots);
            let Some(ServerMsg::Bots { bots }) =
                forward(ServerMsg::Bots { bots: bots.clone() }, &mut client)
            else {
                panic!("bots")
            };
            let json = serde_json::to_string(&bots).unwrap();
            assert!(!json.contains("task_owner"), "{json}");
            bots.into_iter()
                .filter(|b| b.id != "assistant")
                .map(|b| (b.id, b.can_task))
                .collect::<Vec<_>>()
        };
        let all = vec![("notes".to_owned(), true), ("recipes".to_owned(), true)];
        assert_eq!(can_task(&farm), all);
        assert_eq!(can_task(&jay_phone), all);
        assert_eq!(
            can_task(&sam),
            [("notes".to_owned(), true), ("recipes".to_owned(), false)]
        );
        // The engine is listed for everyone.
        assert!(
            bots.iter()
                .any(|b| b.id == "notes" && b.engine == oxbot_protocol::EngineKind::OpenRouter)
        );

        let settings = state.settings_for(&sam);
        assert!(!settings.owner && settings.owner_name == "Jay");
        assert!(!settings.openrouter_ready);
        assert!(state.settings_for(&jay_phone).owner);
        assert!(state.settings_for(&farm).owner);
    }

    #[tokio::test]
    async fn only_the_owner_switches_bots_to_claude_or_makes_claude_bots() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, sam) = engines_state(tmp.path());
        let (out, _rx) = mpsc::channel(4);
        let mut client = Client::with_visible(
            sam.clone(),
            state.hub.visible_ids(&sam),
            herder_for(&state, &sam),
        );
        let to_claude = ClientMsg::SetEngine {
            bot: "notes".into(),
            engine: oxbot_protocol::EngineKind::Claude,
            model: None,
            confirm_owner_only: true,
        };
        assert!(!farm_only(&to_claude));
        let replies = handle(to_claude, &state, &mut client, &out).await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { message }] if message.starts_with("Only Jay can switch a bot to Claude")),
            "{replies:?}"
        );
        assert_eq!(
            state.hub.engine_of_bot("notes"),
            Some(EngineKind::OpenRouter)
        );

        let make = ClientMsg::CreateBot {
            name: "Diary".into(),
            instructions: String::new(),
            private: true,
            engine: Some(oxbot_protocol::EngineKind::Claude),
        };
        let replies = handle(make, &state, &mut client, &out).await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { message }] if message.starts_with("Only Jay can make Claude bots")),
            "{replies:?}"
        );
        assert!(state.hub.bot("diary").is_err(), "nothing made");

        // The notice is the Farm's to dismiss.
        assert!(farm_only(&ClientMsg::EnginesNoticeSeen));
        let replies = handle(ClientMsg::EnginesNoticeSeen, &state, &mut client, &out).await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { .. }]),
            "{replies:?}"
        );
        assert!(state.hub.engines_notice_due());

        // Everyone gets the notice; only the Farm the banner and the review.
        let view = state.engines_for(&sam);
        assert_eq!(view.notice, oxbot_core::hub::ENGINES_NOTICE);
        assert!(!view.banner && view.review.is_empty());
        assert!(state.engines_for(&state.farm_viewer()).banner);
    }

    #[tokio::test]
    async fn the_owner_reviews_claude_bots_others_used_until_switched() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _sam) = engines_state(tmp.path());
        // Sam's message to Recipes from before engines.
        oxbot_core::Store::open(&tmp.path().join("oxbot.db"))
            .unwrap()
            .append(
                &oxbot_core::BotId::new("recipes").unwrap(),
                oxbot_protocol::EventKind::UserMessage {
                    text: "dinner?".into(),
                    from: Some("Sam".into()),
                    files: Vec::new(),
                },
            )
            .unwrap();
        state.review_engines();
        let farm = state.farm_viewer();
        let review = state.engines_for(&farm).review;
        assert_eq!(review.len(), 1);
        assert_eq!(
            (review[0].bot.as_str(), &review[0].used_by[..]),
            ("recipes", &["Sam".to_owned()][..])
        );
    }

    const FAKE_KEY: &str = "sk-or-v1-test-feedfacecafe0123";

    /// [`engines_state`] signed in to a stand-in `OpenRouter` nobody
    /// answers on, listening on a real port.
    async fn signed_in_state(
        dir: &std::path::Path,
    ) -> (AppState, Viewer, Arc<oxbot_llm::MemorySecrets>) {
        let (mut state, sam) = engines_state(dir);
        let secrets = Arc::new(oxbot_llm::MemorySecrets::new());
        oxbot_llm::SecretStore::save(
            &*secrets,
            &oxbot_llm::ApiKey::new(FAKE_KEY.to_owned()).unwrap(),
        )
        .unwrap();
        let stand_in = oxbot_llm::Endpoint::loopback(
            "http://127.0.0.1:9/api/v1",
            oxbot_llm::Provider::OpenRouter,
        )
        .unwrap();
        state.openrouter = Some(Arc::new(
            crate::openrouter_auth::OpenRouter::new(
                stand_in,
                secrets.clone(),
                "oxbot on test-mac".into(),
                false,
            )
            .unwrap(),
        ));
        if let Some(account) = &state.openrouter {
            account.load_kept_key().await;
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        state.port = listener.local_addr().unwrap().port();
        tokio::spawn(serve(listener, state.clone(), std::future::pending()));
        (state, sam, secrets)
    }

    #[tokio::test]
    async fn a_slow_keychain_holds_up_neither_the_page_nor_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let (secrets, answer) = crate::openrouter_auth::tests::SlowSecrets::new();
        let stand_in = oxbot_llm::Endpoint::loopback(
            "http://127.0.0.1:9/api/v1",
            oxbot_llm::Provider::OpenRouter,
        )
        .unwrap();
        let account = Arc::new(
            crate::openrouter_auth::OpenRouter::new(
                stand_in,
                secrets,
                "oxbot on test-mac".into(),
                false,
            )
            .unwrap(),
        );
        let parts = oxbot_core::LoopParts::openrouter(account.client(), account.key_cell());
        let (mut state, _) = engines_state_with(tmp.path(), Some(parts));
        state.openrouter = Some(Arc::clone(&account));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        state.port = listener.local_addr().unwrap().port();
        tokio::spawn(serve(listener, state.clone(), std::future::pending()));
        let loading = tokio::spawn({
            let account = Arc::clone(&account);
            async move { account.load_kept_key().await }
        });

        // The Keychain's dialog is up: the page is served all the same,
        // and OpenRouter bots say what they wait for.
        let own = format!("localhost:{}", state.port);
        let (status, _) = get_page(state.port, "/", &own).await;
        assert_eq!(status, 200);
        let settings = state.settings_for(&state.farm_viewer());
        assert!(settings.keychain_waiting && !settings.openrouter_ready);
        assert_eq!(
            state.hub.engine_ready(EngineKind::OpenRouter),
            Err(oxbot_llm::KEYCHAIN_WAITING.to_owned())
        );
        assert!(!loading.is_finished());

        // Allowed: everything goes on.
        answer.send(()).unwrap();
        loading.await.unwrap();
        let settings = state.settings_for(&state.farm_viewer());
        assert!(!settings.keychain_waiting && settings.openrouter_ready);
        assert!(state.hub.engine_ready(EngineKind::OpenRouter).is_ok());
    }

    /// A raw GET to the server: (status, body).
    async fn get_page(port: u16, path: &str, host: &str) -> (u16, String) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tcp.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut reply = String::new();
        tcp.read_to_string(&mut reply).await.unwrap();
        let status = reply[9..12].parse().unwrap();
        (status, reply)
    }

    #[tokio::test]
    async fn only_the_farm_signs_in_and_the_key_never_reaches_a_page() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, sam, _) = signed_in_state(tmp.path()).await;
        let (out, _rx) = mpsc::channel(4);
        let farm = state.farm_viewer();
        for msg in [
            ClientMsg::OpenRouterSignIn,
            ClientMsg::OpenRouterSignOut,
            ClientMsg::RefreshModels,
            ClientMsg::SetEnginePrefs {
                prefs: oxbot_protocol::EnginePrefs::default(),
            },
        ] {
            assert!(farm_only(&msg));
            let mut client = Client::with_visible(
                sam.clone(),
                state.hub.visible_ids(&sam),
                herder_for(&state, &sam),
            );
            let replies = handle(msg, &state, &mut client, &out).await;
            assert!(
                matches!(&replies[..], [ServerMsg::Error { message }] if message == "Only the Farm can do that."),
                "{replies:?}"
            );
        }
        // Sam gets no account details; the Farm does, but never the key.
        assert!(state.engines_for(&sam).openrouter.is_none());
        let view = state.engines_for(&farm);
        assert!(view.openrouter.as_ref().unwrap().signed_in);
        for json in [
            serde_json::to_string(&ServerMsg::Engines(view)).unwrap(),
            serde_json::to_string(&ServerMsg::Settings(state.settings_for(&farm))).unwrap(),
            format!("{state:?}"),
        ] {
            assert!(!json.contains("feedfacecafe"), "{json}");
        }

        let mut client = Client::with_visible(
            farm.clone(),
            state.hub.visible_ids(&farm),
            herder_for(&state, &farm),
        );
        let replies = handle(ClientMsg::OpenRouterSignIn, &state, &mut client, &out).await;
        let [ServerMsg::OpenRouterAuth { url }] = &replies[..] else {
            panic!("{replies:?}")
        };
        let callback = format!("localhost%3A{}%2Fopenrouter%2Fcallback", state.port);
        assert!(url.contains(&callback), "{url}");
        assert!(url.contains("key_label=oxbot+on+test-mac"), "{url}");
    }

    #[tokio::test]
    async fn the_sign_in_callback_takes_only_oxbots_own_address_and_a_live_state() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _, _) = signed_in_state(tmp.path()).await;
        let own = format!("localhost:{}", state.port);
        let (status, _) = get_page(
            state.port,
            "/openrouter/callback?code=c&state=s",
            "evil.example",
        )
        .await;
        assert_eq!(status, 403, "another address (DNS rebinding)");
        let (status, page) =
            get_page(state.port, "/openrouter/callback?code=c&state=s", &own).await;
        assert_eq!(status, 400);
        assert!(
            page.contains("This sign-in link isn't valid any more."),
            "{page}"
        );
        assert!(
            page.to_ascii_lowercase()
                .contains("cache-control: no-store"),
            "{page}"
        );
        assert!(
            page.to_ascii_lowercase()
                .contains("referrer-policy: no-referrer"),
            "{page}"
        );
        // A real sign-in's state, but used once already by a wrong guess.
        let url = state
            .openrouter
            .as_ref()
            .unwrap()
            .start_sign_in(state.port)
            .unwrap();
        let live = url
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let (status, _) =
            get_page(state.port, "/openrouter/callback?code=c&state=wrong", &own).await;
        assert_eq!(status, 400);
        let (status, page) = get_page(
            state.port,
            &format!("/openrouter/callback?code=c&state={live}"),
            &own,
        )
        .await;
        assert_eq!(status, 400, "single use: {page}");
    }

    #[tokio::test]
    async fn signing_out_forgets_the_key_and_prefs_are_the_farms() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _, secrets) = signed_in_state(tmp.path()).await;
        let farm = state.farm_viewer();
        let (out, _rx) = mpsc::channel(4);
        let mut client = Client::with_visible(
            farm.clone(),
            state.hub.visible_ids(&farm),
            herder_for(&state, &farm),
        );
        let prefs = oxbot_protocol::EnginePrefs {
            no_data_collection: true,
            ..oxbot_protocol::EnginePrefs::default()
        };
        let replies = handle(
            ClientMsg::SetEnginePrefs {
                prefs: prefs.clone(),
            },
            &state,
            &mut client,
            &out,
        )
        .await;
        assert!(replies.is_empty(), "{replies:?}");
        assert_eq!(state.hub.engine_prefs(), prefs);
        let replies = handle(ClientMsg::OpenRouterSignOut, &state, &mut client, &out).await;
        assert!(
            matches!(&replies[..], [ServerMsg::Notice { message, .. }] if message.contains("openrouter.ai/keys")),
            "{replies:?}"
        );
        assert!(oxbot_llm::SecretStore::load(&*secrets).unwrap().is_none());
        assert!(!state.engines_for(&farm).openrouter.unwrap().signed_in);
    }
}
