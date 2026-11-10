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
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::{SinkExt, StreamExt};
use oxbot_core::bot::{DEFAULT_BOT, is_personal_assistant};
use oxbot_core::engine::{EngineKind, from_wire};
use oxbot_core::hub::{answers, signs_in};
use oxbot_core::id::BrowserOwner;
use oxbot_core::store::Replay;
use oxbot_core::{Hub, Viewer};
use oxbot_protocol::{
    AppSettings, ClientMsg, DEFAULT_PAGE, EnginesView, KeyLimits, LogLine, MAX_PAGE, ModelRow,
    OpenRouterView, ServerHello, ServerMsg, SetupReport,
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
    /// The Mac account's first name: who the home computer's own person is the first
    /// time oxbot runs (then they're found by id; see
    /// [`Hub::owner_person`]). Devices send as the person they were paired for.
    pub local_person: String,
    /// The `PATH` oxbot's programs use (see [`crate::setup::search_path`]).
    pub search_path: String,
    /// The latest setup check.
    pub setup: Arc<std::sync::RwLock<SetupReport>>,
    /// Whether the browser tool (and the Node.js it needs) ships inside the
    /// app, so the setup check doesn't ask for Node.js.
    pub tool_built_in: bool,
    /// End-to-end test mode: a connection may say who it is with `?as=Name`
    /// (always a device person, never the home computer).
    pub e2e: bool,
    /// The device door phones and laptops connect through (`None` in tests
    /// that don't need it).
    pub door: Option<Arc<crate::door::Door>>,
    /// The home computer secret (see [`crate::secret`]): a connection from this Mac
    /// is the home computer only if it shows this.
    pub home_computer_key: String,
    /// Posts Mac notifications (see [`crate::notify`]); `None` posts none.
    pub notifier: Option<Arc<dyn crate::notify::Notifier>>,
    /// Dictation with this Mac's microphone (see [`crate::dictation`]).
    pub dictation: Arc<crate::dictation::Dictation>,
    /// Numbers connections, so dictation goes back to the one that asked.
    pub next_conn: Arc<std::sync::atomic::AtomicU64>,
    /// The port this server listens on (`OpenRouter`'s sign-in comes back
    /// to it); 0 if unknown.
    pub port: u16,
    /// The household's `OpenRouter` account (`None`: not in this server).
    pub openrouter: Option<Arc<crate::openrouter_auth::OpenRouter>>,
    /// Developer tools (comparing AI models): on only when the app started
    /// with `OXBOT_DEV_TOOLS=1`. Homes never see them.
    pub dev_tools: bool,
    /// Keeps the Mac awake while plugged in (see [`crate::awake`]); `None`
    /// where that doesn't apply (not a Mac, or tests that don't need it).
    pub awake: Option<Arc<crate::awake::KeepAwake>>,
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
    /// The app's settings right now, for the home computer's own person.
    #[must_use]
    pub fn settings(&self) -> AppSettings {
        self.settings_for(&self.home_computer_viewer())
    }

    /// The app's settings as `viewer` gets them.
    #[must_use]
    pub fn settings_for(&self, viewer: &Viewer) -> AppSettings {
        AppSettings {
            codex_ready: self.hub.engine_ready(EngineKind::Codex).is_ok(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            data_dir: self.data_dir.display().to_string(),
            launch_at_login: self.app.as_ref().and_then(|a| a.launch_at_login()),
            you: viewer.name.clone(),
            home_computer: viewer.home_computer,
            usage_alerts: self.hub.usage_alerts_on(),
            // The Mac's own microphone: the home computer's alone.
            dictation: viewer.home_computer && self.dictation.available(),
            owner: self.hub.is_owner_viewer(viewer),
            dev_tools: self.dev_tools,
            owner_name: self.hub.owner_display_name(),
            openrouter_ready: self.hub.openrouter_ready(),
            keychain_waiting: self
                .openrouter
                .as_ref()
                .is_some_and(|a| a.waiting_for_keychain()),
            // The Mac's own sleep: the home computer's alone.
            keep_awake: self
                .awake
                .as_ref()
                .filter(|_| viewer.home_computer)
                .map(|_| self.hub.keep_awake_on()),
        }
    }

    /// Settings → Engines as `viewer` gets it: everyone the notice and
    /// whether each engine is ready; the home computer also `OpenRouter`'s account
    /// and what was found of Codex.
    #[must_use]
    pub fn engines_for(&self, viewer: &Viewer) -> EnginesView {
        let home_computer = viewer.home_computer;
        let catalog = self.catalog();
        let models = catalog
            .as_ref()
            .map(|(c, _)| c.free_for_loop().map(model_row).collect())
            .unwrap_or_default();
        let paid_models = catalog
            .as_ref()
            .map(|(c, _)| c.paid_for_loop().into_iter().map(model_row).collect())
            .unwrap_or_default();
        let model_lists = catalog
            .as_ref()
            .map(|(c, _)| oxbot_core::model_lists::suggested(c))
            .unwrap_or_default();
        let openrouter = home_computer.then(|| self.openrouter_view(catalog.as_ref()));
        EnginesView {
            codex: home_computer.then(|| self.hub.codex_view()),
            codex_ready: self.hub.engine_ready(EngineKind::Codex).is_ok(),
            notice: oxbot_core::hub::ENGINES_NOTICE.to_owned(),
            owner: self.hub.owner_display_name(),
            openrouter_ready: self.hub.openrouter_ready(),
            models,
            paid_models,
            model_lists,
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

    /// Settings → Engines → `OpenRouter`, for the home computer.
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
            key_in_file: account.is_some_and(crate::openrouter_auth::OpenRouter::key_in_file),
            key,
            paid_models,
            filtered_out,
            models_fetched_ms,
            prefs: self.hub.engine_prefs(),
            usage: self.hub.openrouter_usage_today(),
            spent_this_month_usd: self.hub.paid_spent_this_month(),
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
        self.hub.announce(ServerMsg::Engines(
            self.engines_for(&self.home_computer_viewer()),
        ));
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

    /// Who the home computer's own window is: the home computer's own person. (devices connect
    /// through the device door as the person they were paired for.)
    #[must_use]
    pub fn home_computer_viewer(&self) -> Viewer {
        self.hub.home_computer_viewer(&self.local_person)
    }

    /// The hello each connection gets first. It says test mode only in a
    /// debug build started for tests, so the page gives the tests their
    /// names (`window.send`…) there and nowhere else.
    #[must_use]
    pub fn hello(&self) -> ServerHello {
        ServerHello {
            test: self.e2e && cfg!(debug_assertions),
            ..ServerHello::new(&self.server_name, self.started_ms)
        }
    }
}

impl AppState {
    /// Runs the setup check again and keeps the result.
    pub async fn refresh_setup(&self) -> SetupReport {
        self.dictation.check().await;
        let mut report = crate::setup::check(&self.search_path, self.tool_built_in);
        report
            .checks
            .insert(0, crate::setup::codex_check(&self.hub.codex_view()));
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
            e2e: false,
            door: None,
            home_computer_key: crate::secret::random_hex::<32>(),
            notifier: None,
            dictation: Arc::new(crate::dictation::Dictation::new(None)),
            next_conn: Arc::default(),
            port: 0,
            openrouter: None,
            dev_tools: false,
            awake: None,
        }
    }
}

/// The page (`crates/oxbot/ui-src`, built into `ui-dist/`), baked into the
/// binary.
mod page {
    pub const INDEX_HTML: &str = include_str!("../ui-dist/index.html");
    pub const APP_JS: &str = include_str!("../ui-dist/app.js");
    pub const APP_CSS: &str = include_str!("../ui-dist/app.css");
}

/// The page's Content-Security-Policy on this Mac (`port`: this server's):
/// its own script and stylesheet only, no inline scripts, connections back
/// to this server only (its WebSocket named outright, as older Safari
/// doesn't count `ws:` as `'self'`), and no other page may frame it or
/// take its forms. The report frame (`srcdoc`, which inherits this policy)
/// may use exactly the reports' own stylesheet, allowed by its hash;
/// nothing else inline.
fn page_csp(port: u16) -> String {
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self' {report}; \
         img-src 'self' data: blob:; \
         connect-src 'self' ws://127.0.0.1:{port} ws://localhost:{port}; \
         frame-src 'none'; frame-ancestors 'none'; form-action 'none'; \
         object-src 'none'; base-uri 'none'",
        report = report_style_hash()
    )
}

/// `'sha256-…'` of the reports' own stylesheet (see [`page_csp`]).
fn report_style_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| csp_hash(oxbot_core::report::STYLE))
}

/// `'sha256-…'` of `text`, as a CSP source.
fn csp_hash(text: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    format!("'sha256-{}'", base64(digest.as_ref()))
}

/// Standard base64 with padding, for CSP hashes.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The page's script and stylesheet, added to both routers so they're
/// reached (and checked) the same way on this Mac and through the device door.
fn with_page_files(router: Router<AppState>) -> Router<AppState> {
    router
        .route(
            "/app.js",
            get(|State(state): State<AppState>| async move {
                page_file(
                    &state,
                    "app.js",
                    "text/javascript; charset=utf-8",
                    page::APP_JS,
                )
                .await
            }),
        )
        .route(
            "/app.css",
            get(|State(state): State<AppState>| async move {
                page_file(&state, "app.css", "text/css; charset=utf-8", page::APP_CSS).await
            }),
        )
}

/// One of the page's files, with its type and the strict policy. In dev
/// mode (`ui_dir`) it's read from that folder on every request, so a
/// rebuilt page shows on refresh.
async fn page_file(
    state: &AppState,
    name: &str,
    content_type: &'static str,
    built_in: &'static str,
) -> Response {
    if let Some(dir) = &state.ui_dir {
        match tokio::fs::read_to_string(dir.join(name)).await {
            Ok(body) => return with_page_headers(content_type, state.port, body),
            Err(err) => {
                tracing::warn!(%err, name, "dev page file not readable; serving the built-in one");
            }
        }
    }
    with_page_headers(content_type, state.port, built_in)
}

/// A page file's response: its type and the strict policy for a server on
/// `port`.
fn with_page_headers(
    content_type: &'static str,
    port: u16,
    body: impl axum::response::IntoResponse,
) -> Response {
    let csp = HeaderValue::from_str(&page_csp(port))
        .unwrap_or_else(|_| HeaderValue::from_static("default-src 'none'"));
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CONTENT_SECURITY_POLICY, csp),
            (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

/// The HTTP routes: the page at `/`, the WebSocket at `/ws`, `POST /upload`
/// for files sent with messages, and `GET /hello` for oxbot's own programs
/// to check it's really oxbot (this Mac only: not on the device door).
/// Every one answers only on oxbot's own address ([`require_own_host`]).
pub fn router(state: AppState) -> Router {
    with_page_files(Router::new())
        .route(
            "/",
            get(|State(state): State<AppState>| async move {
                page_file(
                    &state,
                    "index.html",
                    "text/html; charset=utf-8",
                    page::INDEX_HTML,
                )
                .await
            }),
        )
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
        .route(crate::secret::HELLO_PATH, get(hello))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_own_host,
        ))
        .with_state(state)
}

/// Every request on this Mac must name oxbot's own address in `Host`
/// (`127.0.0.1:{port}` or `localhost:{port}`). A web page that points a
/// name of its own at `127.0.0.1` (DNS rebinding) sends that name instead,
/// and is refused before anything is read.
async fn require_own_host(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !crate::openrouter_auth::own_host(host, state.port) {
        tracing::warn!("refused a request for another address");
        return (StatusCode::FORBIDDEN, "This isn't oxbot's own address.").into_response();
    }
    next.run(request).await
}

/// `GET /hello?n=<nonce>`: proves this is the oxbot with this data
/// folder's home computer secret, without showing it (see
/// [`crate::secret::hello_proof`]). oxbot's own programs ask before they
/// send the secret, so a stranger on oxbot's port never gets it.
async fn hello(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(nonce) = query.get("n").filter(|n| crate::secret::hello_nonce_ok(n)) else {
        return (StatusCode::BAD_REQUEST, "Say hello with a nonce.").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        crate::secret::hello_proof(&state.home_computer_key, nonce),
    )
        .into_response()
}

/// The most one WebSocket message may be: it carries only JSON (files go
/// over `POST /upload`).
const MAX_WS_MESSAGE: usize = 4 * 1024 * 1024;

/// The device door's routes: `POST /pair` for anyone with the code, and the
/// page and WebSocket for paired devices only (see [`crate::door`]).
pub fn door_router(state: AppState, door: &Arc<crate::door::Door>) -> Router {
    with_page_files(Router::new())
        // The phone app fetches `/` once and puts it in its own window, so
        // it gets the page with its script and styles inside (and their
        // hashes in its own policy: see `inline_into`).
        .route("/", get(|| async { Html(inline_page()) }))
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
    let mut state = state;
    // The address checks need the port: the listener's, if not given.
    if state.port == 0 {
        state.port = listener.local_addr()?.port();
    }
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown)
        .await
}

/// The page as one file, its script and stylesheet inside, for the
/// phone app (which loads the page through its own connection, not by
/// address). Built once.
fn inline_page() -> &'static str {
    static PAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE.get_or_init(|| inline_into(page::INDEX_HTML, page::APP_JS, page::APP_CSS))
}

/// `html` with its `app.js` script and `app.css` link replaced by their
/// contents. A `</script` inside the script is escaped so it can't end it.
/// Right after `<head>` comes the page's own policy ([`inline_csp`]),
/// before anything it covers.
fn inline_into(html: &str, js: &str, css: &str) -> String {
    let script = js.replace("</script", "<\\/script");
    let style = css.replace("</style", "<\\/style");
    let csp = inline_csp(&script, &style);
    let mut page = String::with_capacity(html.len() + js.len() + css.len() + csp.len());
    for line in html.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("<head") {
            page.push_str(line);
            page.push_str("<meta http-equiv=\"Content-Security-Policy\" content=\"");
            page.push_str(&csp);
            page.push_str("\">\n");
        } else if t.starts_with("<script") && t.contains("app.js") {
            page.push_str("<script type=\"module\">");
            page.push_str(&script);
            page.push_str("</script>\n");
        } else if t.starts_with("<link") && t.contains("app.css") {
            page.push_str("<style>");
            page.push_str(&style);
            page.push_str("</style>\n");
        } else {
            page.push_str(line);
        }
    }
    page
}

/// The policy of the page the phone app shows (in a `<meta>`: it isn't
/// fetched by address, so no header reaches it): exactly the inlined
/// `script` and `style` (by their hashes) and the reports' stylesheet,
/// pictures from the page itself, and connections only to the phone app's
/// own bridge (Tauri's `ipc:`, `http://ipc.localhost` on Android).
fn inline_csp(script: &str, style: &str) -> String {
    format!(
        "default-src 'none'; script-src {script}; style-src {style} {report}; \
         img-src 'self' data: blob:; connect-src 'self' ipc: http://ipc.localhost; \
         frame-src 'none'; form-action 'none'; object-src 'none'; base-uri 'none'",
        script = csp_hash(script),
        style = csp_hash(style),
        report = report_style_hash(),
    )
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<AppState>,
    device: Option<Extension<crate::door::http::DeviceAuth>>,
    conn: Option<Extension<axum::extract::ConnectInfo<crate::door::listener::ConnBytes>>>,
) -> Response {
    let ws = ws
        .max_message_size(MAX_WS_MESSAGE)
        .max_frame_size(MAX_WS_MESSAGE);
    // Through the device door: the key was checked; always the device's person.
    if let Some(Extension(auth)) = device {
        let viewer = auth.viewer;
        let id = auth.device.id;
        let bytes = conn.map(|Extension(c)| c.0);
        return ws.on_upgrade(move |socket| connection(socket, state, viewer, Some(id), bytes));
    }
    let key = query.get("k").map(String::as_str);
    let viewer = match local_viewer(&state, &headers, key, query.get("as").map(String::as_str)) {
        Ok(viewer) => viewer,
        Err(refused) => return refused,
    };
    ws.on_upgrade(move |socket| connection(socket, state, viewer, None, None))
}

/// Who a request from this Mac (not the device door) is: the home computer if it
/// shows the home computer secret `key`; in test mode, the person it names with
/// `as` (never the home computer's own person). Refused from another website's
/// page, or without either.
///
/// A browser always sends `Origin`, and it must be oxbot's own page at this
/// server's port. A request without one isn't from a page: a program on this
/// Mac (`oxbot chat`), which must show the key (or, in test mode, name a
/// test person).
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
        && !is_local_origin(origin.to_str().unwrap_or(""), state.port)
    {
        tracing::warn!(?origin, "refused a request from another website");
        return Err((StatusCode::FORBIDDEN, "oxbot only accepts its own page").into_response());
    }
    // Being on this Mac isn't enough to be the home computer: it takes the home computer
    // secret, which only oxbot's own windows and tools have.
    let home_computer =
        key.is_some_and(|k| crate::secret::same(k.trim(), &state.home_computer_key));
    match as_name.map(str::trim) {
        _ if home_computer => Ok(state.home_computer_viewer()),
        // Test mode only (debug builds): the page names its person, made on
        // purpose here (as pairing would) so the connection carries their
        // id. Never the home computer's own person: that takes the key.
        Some(name) if state.e2e && !name.is_empty() => match state.hub.person_named(name) {
            Ok(person) if state.hub.is_owner_person(person.id.as_str()) => {
                tracing::warn!("refused a test connection naming the home computer's person");
                Err((
                    StatusCode::FORBIDDEN,
                    "That person needs the home computer secret.",
                )
                    .into_response())
            }
            Ok(person) => Ok(Viewer::person(person.id, person.name, false)),
            Err(err) => {
                tracing::warn!(%err, "couldn't make a test connection's person");
                Err((StatusCode::INTERNAL_SERVER_ERROR, "no such person").into_response())
            }
        },
        _ => {
            tracing::warn!("refused a local connection without the home computer secret");
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
/// the home computer secret in `X-Oxbot-Key`: saves a file to send with the next
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
        assistant_for(&state, &viewer)
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
    // Files only for a bot they may give tasks, refused before anything is
    // read.
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

/// Whether a browser Origin is the oxbot page on this Mac at `port`:
/// exactly `http://127.0.0.1:{port}` or `http://localhost:{port}`.
fn is_local_origin(origin: &str, port: u16) -> bool {
    origin
        .strip_prefix("http://")
        .is_some_and(|host| crate::openrouter_auth::own_host(host, port))
}

/// What one connection may see and has subscribed to.
#[derive(Debug)]
struct Client {
    /// Who's connected.
    viewer: Viewer,
    /// Their own Assistant's id. They see it as `assistant`, and see no other
    /// the Assistant (see [`Hub::assistant_for`]).
    assistant: String,
    /// The bots they may see, kept up to date from bot-list updates.
    visible: std::collections::HashSet<String>,
    /// Bot id → last `seq` sent.
    subs: HashMap<String, u64>,
    /// This connection's number (for dictation).
    conn: u64,
    /// The home computer owner (the home computer, or the home computer's person on their device): gets
    /// comparisons of engines and models.
    owner: bool,
    /// When this connection last ran the setup check (see
    /// [`SETUP_CHECK_EVERY`]).
    setup_checked: Option<std::time::Instant>,
}

/// How often a device's connection may run the Mac's setup check (it
/// starts programs on the Mac); in between it gets the last result. The
/// home computer may check whenever it likes.
const SETUP_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

impl Client {
    #[cfg(test)]
    fn new(viewer: Viewer, bots: &[oxbot_protocol::BotInfo]) -> Self {
        Self::with_assistant(viewer, bots, DEFAULT_BOT.to_owned())
    }

    #[cfg(test)]
    fn with_assistant(viewer: Viewer, bots: &[oxbot_protocol::BotInfo], assistant: String) -> Self {
        let mut client = Self::with_visible(viewer, std::collections::HashSet::new(), assistant);
        client.see(bots);
        client
    }

    /// A client for `viewer`, who may see the bots in `visible`.
    fn with_visible(
        viewer: Viewer,
        visible: std::collections::HashSet<String>,
        assistant: String,
    ) -> Self {
        Self {
            viewer,
            assistant,
            visible,
            subs: HashMap::new(),
            conn: 0,
            owner: false,
            setup_checked: None,
        }
    }

    /// Takes a new bot list: remembers which bots this client may see, and
    /// drops subscriptions to the rest.
    fn see(&mut self, bots: &[oxbot_protocol::BotInfo]) {
        self.visible = bots
            .iter()
            .filter(|b| self.viewer.can_see_info(b))
            // The home computer's Assistant is only for those whose Assistant it is (as the
            // hub's visible_ids says).
            .filter(|b| b.id != DEFAULT_BOT || self.assistant == DEFAULT_BOT)
            .map(|b| b.id.clone())
            .collect();
        let visible = &self.visible;
        self.subs.retain(|bot, _| visible.contains(bot));
    }

    fn sees(&self, bot: &str) -> bool {
        self.visible.contains(bot)
    }

    /// A bot id as this client said it, as the Hub knows it: `assistant`
    /// is their own Assistant.
    fn inner(&self, bot: String) -> String {
        if bot == DEFAULT_BOT {
            self.assistant.clone()
        } else {
            bot
        }
    }

    /// A bot id as the Hub knows it, as this client sees it; `None` for
    /// someone else's Assistant.
    fn outer(&self, bot: &str) -> Option<String> {
        if bot == self.assistant {
            Some(DEFAULT_BOT.to_owned())
        } else if bot == DEFAULT_BOT || is_personal_assistant(bot) {
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
            ClientMsg::SetSharing {
                bot,
                sharing,
                people,
            } => ClientMsg::SetSharing {
                bot: self.inner(bot),
                sharing,
                people,
            },
            ClientMsg::RenameBot { bot, name } => ClientMsg::RenameBot {
                bot: self.inner(bot),
                name,
            },
            ClientMsg::SetBotInstructions { bot, instructions } => ClientMsg::SetBotInstructions {
                bot: self.inner(bot),
                instructions,
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
            ClientMsg::ListSchedules { bot } => ClientMsg::ListSchedules {
                bot: bot.map(|b| self.inner(b)),
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
        view
    }

    /// The message with bot ids as this client sees them, or `None` if it's
    /// about someone else's Assistant.
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
            other => return self.outbound_engines(other),
        })
    }

    /// [`Client::outbound`] for the engines view, turn meters, schedules
    /// and scheduled runs' lines.
    fn outbound_engines(&self, msg: ServerMsg) -> Option<ServerMsg> {
        Some(match msg {
            ServerMsg::Engines(view) => ServerMsg::Engines(self.outer_engines(view)),
            ServerMsg::Notify {
                bot,
                text,
                waiting,
                person_id,
            } => ServerMsg::Notify {
                bot: self.outer(&bot)?,
                text,
                waiting,
                person_id,
            },
            ServerMsg::Schedules { bot, schedules } => ServerMsg::Schedules {
                bot: bot.and_then(|b| self.outer(&b)),
                // The owner sees schedules in others' Assistants too, under
                // their own ids.
                schedules: schedules
                    .into_iter()
                    .map(|mut s| {
                        if let Some(id) = self.outer(&s.bot) {
                            s.bot = id;
                        }
                        s
                    })
                    .collect(),
            },
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

/// Which Assistant `viewer` talks to: the home computer and the home computer's own person (by
/// id, on the Mac or a phone) use the home computer's; anyone else gets their own.
fn assistant_for(state: &AppState, viewer: &Viewer) -> String {
    match state.hub.assistant_of(viewer) {
        Ok(id) => id.to_string(),
        Err(err) => {
            // No Assistant at all rather than someone else's.
            let person = viewer.id.as_ref().map(ToString::to_string);
            tracing::warn!(%err, person, "couldn't make an Assistant for a person");
            String::from("-")
        }
    }
}

/// One client's connection. `device` is the device's id when it came through
/// the device door: it shows as online meanwhile, and is cut off if removed;
/// `bytes` counts what its connection carried (Settings → Activity).
async fn connection(
    socket: WebSocket,
    state: AppState,
    viewer: Viewer,
    device: Option<String>,
    bytes: Option<crate::door::listener::ConnBytes>,
) {
    let door = state.door.clone().filter(|_| device.is_some());
    let mut removals = door.as_ref().map(|d| d.removals());
    // Online until this connection ends, however it ends.
    let _online = match (&door, &device) {
        (Some(door), Some(id)) => Some(door.online(id, viewer.id.as_ref(), bytes)),
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
        device.as_deref(),
        removals.as_mut(),
    )
    .await;
}

async fn serve_client(
    socket: WebSocket,
    state: &AppState,
    viewer: Viewer,
    conn: u64,
    device: Option<&str>,
    mut removals: Option<&mut broadcast::Receiver<String>>,
) {
    let state = state.clone();
    let (mut sink, mut stream) = socket.split();
    // Subscribe before replaying history, so nothing falls in the gap.
    let mut updates = state.hub.subscribe();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(64);
    let assistant = assistant_for(&state, &viewer);
    let visible = state.hub.visible_ids(&viewer);
    let mut client = Client::with_visible(viewer, visible, assistant);
    client.conn = conn;
    client.owner = state.hub.is_owner_viewer(&client.viewer);

    let mut logs = state.logs.as_ref().map(|l| l.subscribe());
    let mut first = vec![
        ServerMsg::Hello(state.hello()),
        ServerMsg::Settings(state.settings_for(&client.viewer)),
    ];
    first.push(ServerMsg::Setup(state.setup_report()));
    first.extend(usage_now(&state));
    if let Ok(people) = state.hub.people() {
        first.push(ServerMsg::People { people });
    }
    // Their own profile (the welcome screen shows if it's due).
    if let Ok(Some(view)) = state.hub.profile_for(&client.viewer) {
        first.push(ServerMsg::Profile(view));
    }
    first.extend(state.hub.sites_for(&client.viewer));
    // Always (even with none), so the page knows where it stands.
    first.extend(state.hub.sections_for(&client.viewer));
    // Only once there are devices, so clients that don't know devices (an older
    // `oxbot chat`) never see the message on a home computer that has none.
    if let Some(door) = state.door.as_ref().filter(|_| client.viewer.home_computer) {
        let devices = door.devices();
        if !devices.is_empty() {
            first.push(ServerMsg::Devices { devices });
        }
        first.push(ServerMsg::Door(door.state()));
    }
    // The log names every bot, site and person: the home computer's only.
    if let Some(relay) = state.logs.as_ref().filter(|_| client.viewer.home_computer) {
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
            .filter(|r| client.sees(&r.bot) && signs_in(r, &client.viewer))
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
                if device.is_some() && (device == Some(removed.as_str()) || removed == crate::door::EVERY_DEVICE) {
                    tracing::info!(device = removed, "closing a removed device's connection");
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

/// The usage alerts still live (so the banner survives a reload).
fn usage_now(state: &AppState) -> Vec<ServerMsg> {
    state
        .hub
        .live_usage_alerts()
        .into_iter()
        .map(ServerMsg::UsageAlert)
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

/// The next removed device's id; never resolves without a door.
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

/// Bot settings: who it's shared with, its name, what it's for, and
/// deleting it. The hub checks that `viewer` may manage it.
async fn handle_bot_settings(msg: ClientMsg, state: &AppState, viewer: &Viewer) -> Vec<ServerMsg> {
    let hub = &state.hub;
    reply(match msg {
        ClientMsg::SetPrivate { bot, private } => hub.set_private(&bot, private, viewer).await,
        ClientMsg::SetSharing {
            bot,
            sharing,
            people,
        } => hub.set_sharing(&bot, sharing, &people, viewer).await,
        ClientMsg::RenameBot { bot, name } => hub.rename_bot(&bot, &name, viewer).await,
        ClientMsg::SetBotInstructions { bot, instructions } => {
            hub.set_instructions(&bot, &instructions, viewer).await
        }
        ClientMsg::DeleteBot { bot } => hub.delete_bot(&bot, viewer).await,
        _ => Ok(()),
    })
}

/// The viewer's own sidebar sections.
fn handle_sections(msg: ClientMsg, state: &AppState, viewer: &Viewer) -> Vec<ServerMsg> {
    match msg {
        ClientMsg::CreateSection { name, bot } => {
            reply(state.hub.create_section(&name, bot.as_deref(), viewer))
        }
        ClientMsg::RenameSection { id, name } => {
            reply(state.hub.rename_section(&id, &name, viewer))
        }
        ClientMsg::DeleteSection { id } => reply(state.hub.delete_section(&id, viewer)),
        ClientMsg::MoveBot { bot, section } => {
            reply(state.hub.move_bot(&bot, section.as_deref(), viewer))
        }
        _ => Vec::new(),
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
    if !viewer.home_computer && home_computer_only(&msg) {
        return error(&"That can only be done on the home computer.");
    }
    // Comparing engines and models: the owner's only (the hub checks).
    let msg = match crate::compare::handle(msg, &state.hub, &viewer, state.dev_tools).await {
        Ok(reply) => return reply,
        Err(other) => other,
    };
    let msg = match handle_home_computer(msg, state).await {
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
        msg @ ClientMsg::CreateBot { .. } => create_bot(state, client, out, msg).await,
        ClientMsg::SetPrivate { .. }
        | ClientMsg::SetSharing { .. }
        | ClientMsg::RenameBot { .. }
        | ClientMsg::SetBotInstructions { .. }
        | ClientMsg::DeleteBot { .. } => handle_bot_settings(msg, state, &viewer).await,
        ClientMsg::Answer {
            id,
            answer,
            schedule,
        } => match state.hub.answer_with(id, answer, schedule, &viewer) {
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
        // The hub checks it's the home computer too.
        ClientMsg::SetFineGrainedSite { site, on } => {
            reply(state.hub.set_fine_grained_site(&site, on, &viewer))
        }
        // Sections are the viewer's own view: anyone oxbot can place.
        ClientMsg::CreateSection { .. }
        | ClientMsg::RenameSection { .. }
        | ClientMsg::DeleteSection { .. }
        | ClientMsg::MoveBot { .. } => handle_sections(msg, state, &viewer),
        ClientMsg::OpenReport { .. }
        | ClientMsg::ListReports { .. }
        | ClientMsg::DeleteReport { .. } => handle_reports(msg, state, client),
        ClientMsg::ListSchedules { .. }
        | ClientMsg::PauseSchedule { .. }
        | ClientMsg::DeleteSchedule { .. }
        | ClientMsg::E2eAdvanceClock { .. }
        | ClientMsg::SetProfile { .. }
        | ClientMsg::GetProfile => handle_own(msg, state, &viewer).await,
        other => handle_sign_ins(other, state, &viewer).await,
    }
}

/// One's own profile, and schedules: lists, pauses and deletes them (the
/// hub decides who may), and in test mode moves the scheduler's clock.
/// Nobody makes a schedule here: only a bot's schedule card does.
async fn handle_own(msg: ClientMsg, state: &AppState, viewer: &Viewer) -> Vec<ServerMsg> {
    match msg {
        ClientMsg::SetProfile { profile } => match state.hub.set_profile(viewer, &profile) {
            Ok(view) => vec![ServerMsg::Profile(view)],
            Err(e) => reply(Err(e)),
        },
        ClientMsg::GetProfile => match state.hub.profile_for(viewer) {
            Ok(Some(view)) => vec![ServerMsg::Profile(view)],
            Ok(None) => Vec::new(),
            Err(e) => reply(Err(e)),
        },
        ClientMsg::ListSchedules { bot } => match state.hub.schedules_for(viewer, bot.as_deref()) {
            Ok(schedules) => vec![ServerMsg::Schedules { bot, schedules }],
            Err(e) => reply(Err(e)),
        },
        ClientMsg::PauseSchedule { id, paused } => {
            reply(state.hub.pause_schedule_as(id, paused, viewer))
        }
        ClientMsg::DeleteSchedule { id } => reply(state.hub.delete_schedule_as(id, viewer)),
        ClientMsg::E2eAdvanceClock { minutes } if state.e2e => {
            reply(state.hub.advance_test_clock(minutes).await)
        }
        ClientMsg::E2eAdvanceClock { .. } => vec![ServerMsg::Error {
            message: "That's only for tests.".to_owned(),
        }],
        _ => Vec::new(),
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
            models,
        } => reply(
            state
                .hub
                .set_engine(&bot, from_wire(engine), model, models, viewer)
                .await,
        ),
        other => return handle_net_log(other, state, viewer),
    })
}

/// Settings → Activity: the rows `viewer` may see (the hub decides: the
/// owner all, others their own bots' and devices', kids none). Gives other
/// messages back.
fn handle_net_log(
    msg: ClientMsg,
    state: &AppState,
    viewer: &Viewer,
) -> Result<Vec<ServerMsg>, ClientMsg> {
    let ClientMsg::ListNetLog {
        kind,
        before_ms,
        limit,
    } = msg
    else {
        return Err(msg);
    };
    Ok(
        match state.hub.net_log_for(viewer, kind, before_ms, limit) {
            Ok(log) => vec![log],
            Err(e) => reply(Err(e)),
        },
    )
}

/// Sending messages (with files), the Mac's dictation, and the setup
/// check. Gives other messages back.
async fn handle_messages(
    msg: ClientMsg,
    state: &AppState,
    client: &mut Client,
    out: &mpsc::Sender<ServerMsg>,
) -> Result<Vec<ServerMsg>, ClientMsg> {
    if matches!(msg, ClientMsg::CheckSetup) {
        let report = if client.viewer.home_computer
            || client
                .setup_checked
                .is_none_or(|at| at.elapsed() >= SETUP_CHECK_EVERY)
        {
            client.setup_checked = Some(std::time::Instant::now());
            state.refresh_setup().await
        } else {
            state.setup_report()
        };
        return Ok(vec![
            ServerMsg::Setup(report),
            // Whether the mic shows may have changed.
            ServerMsg::Settings(state.settings_for(&client.viewer)),
        ]);
    }
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

/// The home computer's own jobs: pairing and removing devices, the Mac's setup
/// (Home-computer-only ones are checked by the caller). Gives other messages back.
async fn handle_home_computer(
    msg: ClientMsg,
    state: &AppState,
) -> Result<Vec<ServerMsg>, ClientMsg> {
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    Ok(match msg {
        ClientMsg::StartPairing { .. }
        | ClientMsg::CancelPairing
        | ClientMsg::RemoveDevice { .. }
        | ClientMsg::AddDoorNetwork
        | ClientMsg::RemoveDoorNetwork { .. }
        | ClientMsg::SetDoorName { .. } => {
            let Some(door) = &state.door else {
                return Ok(error(&"This oxbot can't connect devices."));
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
        ClientMsg::SetKeepAwake { on } => {
            let Some(awake) = &state.awake else {
                return Ok(error(
                    &"Keeping the Mac awake is only in the oxbot app on a Mac.",
                ));
            };
            match state.hub.set_keep_awake(on) {
                Ok(()) => {
                    awake.set(on);
                    vec![ServerMsg::Settings(state.settings())]
                }
                Err(e) => error(&e),
            }
        }
        ClientMsg::OpenRouterSignIn
        | ClientMsg::OpenRouterSignOut
        | ClientMsg::RefreshModels
        | ClientMsg::SetEnginePrefs { .. } => handle_openrouter(msg, state).await,
        ClientMsg::FindCodex { path } => match state.hub.find_codex(&path).await {
            Ok(()) => {
                state.openrouter_changed();
                Vec::new()
            }
            Err(e) => error(&e),
        },
        other => return Err(other),
    })
}

/// The household's `OpenRouter` account: sign in and out, the model list,
/// the settings. The home computer only (checked by the caller).
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
            // Codex bots reach OpenRouter on this key.
            state.hub.stop_engine_sessions(EngineKind::Codex).await;
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

/// The door's jobs: pairing, removing devices, and which networks phones may
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
        ClientMsg::RemoveDevice { id } => {
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

/// What only the home computer itself may do: pair and remove devices, pick the
/// networks phones use, change the Mac's setup and its notifications.
fn home_computer_only(msg: &ClientMsg) -> bool {
    matches!(
        msg,
        ClientMsg::StartPairing { .. }
            | ClientMsg::CancelPairing
            | ClientMsg::RemoveDevice { .. }
            | ClientMsg::AddDoorNetwork
            | ClientMsg::RemoveDoorNetwork { .. }
            | ClientMsg::SetDoorName { .. }
            | ClientMsg::SetLaunchAtLogin { .. }
            | ClientMsg::SetUsageAlerts { .. }
            // The Mac's own sleep.
            | ClientMsg::SetKeepAwake { .. }
            // The household's OpenRouter account and settings.
            | ClientMsg::OpenRouterSignIn
            | ClientMsg::OpenRouterSignOut
            | ClientMsg::RefreshModels
            | ClientMsg::SetEnginePrefs { .. }
            // Which program runs Codex bots on this Mac.
            | ClientMsg::FindCodex { .. }
            // Websites with fine-grained control (one step at a time).
            | ClientMsg::SetFineGrainedSite { .. }
            // The Mac's own microphone, and opening files on its screen.
            | ClientMsg::Dictate { .. }
            | ClientMsg::OpenFile { .. }
    )
}

/// How `/usr/bin/open` shows a file sent in a chat (`kind`, saved at
/// `path`), so it never runs as a program, a script or a page: text in the
/// text editor (`-t`, whatever its name says); a picture, PDF, document,
/// spreadsheet, slides or video whose name ends as its kind's do, in the
/// app macOS picks; anything else only shown in Finder (`-R`).
fn open_how(kind: oxbot_protocol::AttachmentKind, path: &std::path::Path) -> Option<&'static str> {
    use oxbot_protocol::AttachmentKind as Kind;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let known: &[&str] = match kind {
        Kind::Text => return Some("-t"),
        Kind::Image => &[
            "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "avif", "tif", "tiff", "bmp",
        ],
        Kind::Pdf => &["pdf"],
        Kind::Document => &["docx", "doc", "rtf", "odt"],
        Kind::Sheet => &["xlsx", "xls", "ods"],
        Kind::Slides => &["pptx", "ppt"],
        Kind::Video => &["mov", "mp4", "m4v"],
    };
    if known.contains(&ext.as_str()) {
        None
    } else {
        Some("-R")
    }
}

/// Opens a file sent in a chat on this Mac's screen, in the app macOS
/// picks for it. Found from the stored message, never from a path the
/// page sends.
fn open_file(state: &AppState, bot: &str, seq: u64, id: &str, viewer: &Viewer) -> Vec<ServerMsg> {
    match state.hub.attachment_file(bot, seq, id, viewer) {
        Ok((file, path)) => {
            // Tests never open windows on the Mac they run on.
            if !state.e2e {
                let how = open_how(file.kind, &path);
                tokio::spawn(async move {
                    let status = tokio::process::Command::new("/usr/bin/open")
                        .args(how)
                        .arg("--")
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
                message: format!("Opening {} on the home computer.", file.name),
                owner: None,
                owner_id: None,
            }]
        }
        Err(e) => vec![ServerMsg::Error {
            message: e.to_string(),
        }],
    }
}

/// The New bot form: makes a bot for the client on `engine` (Codex if
/// `None`), once that engine can run (checked before anything is made), and
/// starts it if it was given a description.
async fn create_bot(
    state: &AppState,
    client: &mut Client,
    out_tx: &mpsc::Sender<ServerMsg>,
    msg: ClientMsg,
) -> Vec<ServerMsg> {
    let ClientMsg::CreateBot {
        name,
        instructions,
        private,
        engine,
        models,
    } = msg
    else {
        return Vec::new();
    };
    let (name, instructions) = (name.as_str(), instructions.as_str());
    let viewer = client.viewer.clone();
    let error = |e: &dyn std::fmt::Display| {
        vec![ServerMsg::Error {
            message: e.to_string(),
        }]
    };
    // A list that can't be kept stops it before anything is made.
    let mut models = match oxbot_core::hub::checked_models(models) {
        Ok(models) => models,
        Err(e) => return error(&e),
    };
    // No list given: one picked from what the bot is for (Auto if nothing
    // fits).
    if models.is_empty()
        && let Some((catalog, _)) = state.catalog()
    {
        models = oxbot_core::model_lists::models_for_new_bot(&catalog, name, instructions);
    }
    let wanted = engine.map(from_wire);
    if let Some(kind) = wanted
        && let Err(why) = state.hub.engine_ready(kind)
    {
        return error(&why);
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
    if let Some(wanted) = wanted.filter(|w| *w != from_wire(info.engine))
        && let Err(e) = state
            .hub
            .set_engine(&info.id, wanted, None, None, &viewer)
            .await
    {
        out.push(ServerMsg::Error {
            message: format!("Made {}, but couldn't switch its AI model: {e}", info.name),
        });
    }
    if !models.is_empty()
        && let Err(e) = state.hub.set_bot_models(&info.id, models, &viewer).await
    {
        out.push(ServerMsg::Error {
            message: format!("Made {}, but couldn't give it its models: {e}", info.name),
        });
    }
    let info = match state.hub.bots().await {
        Ok(list) => list.into_iter().find(|b| b.id == info.id).unwrap_or(info),
        Err(_) => info,
    };
    // A bot made with a description starts right away, as its maker.
    if !instructions.trim().is_empty() {
        tokio::spawn(send_as(
            Arc::clone(&state.hub),
            Outgoing {
                bot: info.id.clone(),
                text: FIRST_TASK.to_owned(),
                files: Vec::new(),
            },
            viewer.clone(),
            out_tx.clone(),
        ));
    }
    out.splice(
        0..0,
        viewer
            .redact(vec![info])
            .into_iter()
            .map(ServerMsg::BotCreated),
    );
    out
}

/// The first message a new bot gets from its maker, so it starts working
/// instead of waiting to be asked.
const FIRST_TASK: &str = "Get started: do your job once now if you can. If you need something \
from me first, ask. If it should repeat (like every morning), offer a schedule.";

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
/// (the owner only: the hub refuses anyone else; never home's, even for
/// someone oxbot can't place), else the home one.
async fn handle_sign_ins(msg: ClientMsg, state: &AppState, viewer: &Viewer) -> Vec<ServerMsg> {
    let viewer = &*state.hub.resolve(viewer);
    // Only the owner has a browser of their own: the hub refuses anyone else.
    let owner = |mine: bool| -> Result<BrowserOwner, oxbot_core::HubError> {
        if mine {
            state.hub.own_browser_of(viewer)
        } else {
            Ok(BrowserOwner::Home)
        }
    };
    // The home browser holds everyone's shared sign-ins: only the home computer
    // changes it or opens its sites. Anyone may use their own.
    let home_only_home_computer = |mine: bool| {
        (!mine && !viewer.home_computer).then(|| ServerMsg::Error {
            message: "The websites everyone's bots are signed in to can only be changed on the home computer.".to_owned(),
        })
    };
    match msg {
        ClientMsg::SignOutAll { mine }
        | ClientMsg::OpenSite { mine, .. }
        | ClientMsg::SignOutSite { mine, .. }
            if home_only_home_computer(mine).is_some() =>
        {
            home_only_home_computer(mine).into_iter().collect()
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
        ClientMsg::AddSignIn { url } => match home_only_home_computer(false) {
            Some(refused) => vec![refused],
            None => reply(state.hub.add_sign_in(&url, BrowserOwner::Home).await),
        },
        // Their own sign-in, into the home browser: the hub checks it's
        // theirs and that they aren't a kid.
        ClientMsg::ShareSite { site } => reply(state.hub.share_site(&site, viewer).await),
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
        // A schedule list (or Settings → Activity) is a direct answer, made
        // for its viewer: never passed on to anyone else.
        ServerMsg::AllowRules { .. }
        | ServerMsg::Schedules { .. }
        | ServerMsg::Profile(_)
        | ServerMsg::NetLog { .. } => None,
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
        // A card goes to the person whose request raised it, and the home computer.
        ServerMsg::ApprovalNeeded(ref a) => {
            (client.sees(&a.bot) && answers(a, &client.viewer)).then_some(msg)
        }
        // A person's own browser is signed in by them alone (a bot shared
        // with picked people acts with its maker's sign-ins).
        ServerMsg::SignInNeeded(ref r) => {
            (client.sees(&r.bot) && signs_in(r, &client.viewer)).then_some(msg)
        }
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
        // A person's sections are their own view: only their connections
        // get them, by id. Not the home computer: it manages bots, not people's views.
        ServerMsg::Sections { ref owner_id, .. } => client
            .viewer
            .is_person(owner_id.as_deref(), None)
            .then_some(msg),
        // The log names every bot, site and person; pairing is the home computer's
        // job: the home computer's only.
        // Dictation goes straight to the connection that asked; if it were
        // ever broadcast, only the home computer could have asked.
        ServerMsg::Log(_)
        | ServerMsg::OpenRouterAuth { .. }
        | ServerMsg::Dictation { .. }
        | ServerMsg::Pairing(_)
        | ServerMsg::PairingClosed
        | ServerMsg::Devices { .. }
        | ServerMsg::Door(_) => client.viewer.home_computer.then_some(msg),
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
        | ServerMsg::ComparePrefs(_) => client.owner.then_some(msg),
        ServerMsg::Notice {
            ref owner,
            ref owner_id,
            ..
        } => (owner.is_none()
            || client.viewer.home_computer
            || client
                .viewer
                .is_person(owner_id.as_deref(), owner.as_deref()))
        .then_some(msg),
        // A scheduled run's line goes to its person's own connections
        // alone, by id (not the home computer's: it isn't theirs), for a bot they see.
        ServerMsg::Notify {
            ref bot,
            ref person_id,
            ..
        } => (client.sees(bot) && client.viewer.is_person(Some(person_id), None)).then_some(msg),
        // Nothing about any one bot or person. Every variant is listed, so a
        // new one has to decide who gets it.
        ServerMsg::Hello(_)
        | ServerMsg::Settings(_)
        | ServerMsg::BotCreated(_)
        | ServerMsg::ApprovalResolved { .. }
        | ServerMsg::SignInResolved { .. }
        // Only cues to ask again; they say nothing about which.
        | ServerMsg::SchedulesChanged
        | ServerMsg::ProfilesChanged
        | ServerMsg::Setup(_)
        | ServerMsg::People { .. }
        // Which websites have fine-grained control: no person's.
        | ServerMsg::FineGrainedSites { .. }
        // The household's limits: an alert names at most a bot, by the
        // name everyone sees in the list.
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

    #[test]
    fn a_sent_file_opens_as_a_document_or_only_shows_in_finder() {
        use oxbot_protocol::AttachmentKind as Kind;
        let p = std::path::Path::new;
        assert_eq!(open_how(Kind::Pdf, p("/w/inbox/d/1-bill.pdf")), None);
        assert_eq!(open_how(Kind::Image, p("/w/inbox/d/1-photo.JPG")), None);
        assert_eq!(open_how(Kind::Document, p("/w/inbox/d/1-memo.docx")), None);
        assert_eq!(
            open_how(Kind::Text, p("/w/inbox/d/1-run.command")),
            Some("-t")
        );
        // Anything else is only shown: never run.
        for (kind, path) in [
            (Kind::Pdf, "/w/inbox/d/1-bill.app"),
            (Kind::Image, "/w/inbox/d/1-photo.command"),
            (Kind::Video, "/w/inbox/d/1-clip"),
            (Kind::Sheet, "/w/inbox/d/1-a.webarchive"),
            (Kind::Slides, "/w/inbox/d/1-a.terminal"),
        ] {
            assert_eq!(open_how(kind, p(path)), Some("-R"), "{path}");
        }
    }

    #[test]
    fn base64_matches_the_standard_examples() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
        // The page's policy allows the reports' stylesheet by its hash.
        assert!(super::page_csp(7878).contains("style-src 'self' 'sha256-"));
    }

    #[test]
    fn the_page_connects_only_home_and_is_never_framed() {
        let csp = super::page_csp(7878);
        assert!(
            csp.contains("connect-src 'self' ws://127.0.0.1:7878 ws://localhost:7878;"),
            "{csp}"
        );
        assert!(!csp.contains("ws: ") && !csp.contains("wss:"), "{csp}");
        for part in [
            "frame-ancestors 'none'",
            "form-action 'none'",
            "object-src 'none'",
        ] {
            assert!(csp.contains(part), "{csp}");
        }
    }

    /// The `'sha256-…'` sources in `csp`'s `directive`.
    fn hashes_in<'a>(csp: &'a str, directive: &str) -> Vec<&'a str> {
        csp.split(';')
            .map(str::trim)
            .find_map(|d| d.strip_prefix(directive))
            .unwrap_or("")
            .split_whitespace()
            .filter(|s| s.starts_with("'sha256-"))
            .collect()
    }

    #[test]
    fn the_phones_page_allows_exactly_its_own_script_and_styles() {
        let page = super::inline_page();
        let start = page
            .find("<meta http-equiv=\"Content-Security-Policy\" content=\"")
            .expect("the policy is in the page");
        let csp = &page[start..].split('"').nth(3).unwrap();
        // Before anything it covers.
        assert!(start < page.find("<script").unwrap() && start < page.find("<style").unwrap());
        let between = |open: &str, close: &str| {
            let from = page.find(open).unwrap() + open.len();
            &page[from..from + page[from..].find(close).unwrap()]
        };
        let script = between("<script type=\"module\">", "</script>");
        let style = between("<style>", "</style>");
        // HTML turns \r\n into \n, which would change what the phone hashes.
        assert!(!script.contains('\r') && !style.contains('\r'));
        assert_eq!(hashes_in(csp, "script-src"), [csp_hash(script)]);
        assert_eq!(
            hashes_in(csp, "style-src"),
            [csp_hash(style).as_str(), report_style_hash()]
        );
        assert!(csp.starts_with("default-src 'none';"), "{csp}");
        assert!(
            csp.contains("connect-src 'self' ipc: http://ipc.localhost;"),
            "{csp}"
        );
        assert!(csp.contains("img-src 'self' data: blob:;"), "{csp}");
        assert!(!csp.contains("unsafe"), "{csp}");
    }

    #[test]
    fn the_phone_gets_the_page_with_its_script_and_styles_inside() {
        let html = "<head>\n<script type=\"module\" crossorigin src=\"./app.js\"></script>\n<link rel=\"stylesheet\" crossorigin href=\"./app.css\">\n</head><div id=\"root\"></div>\n";
        let page = super::inline_into(html, "let s = '</script>';", "a{color:red}");
        assert!(
            page.contains("<script type=\"module\">let s = '<\\/script>';</script>"),
            "{page}"
        );
        assert!(page.contains("<style>a{color:red}</style>"), "{page}");
        assert!(
            !page.contains("app.js") && !page.contains("app.css"),
            "{page}"
        );
        assert!(page.contains("<div id=\"root\">"));
        // The built page is one file too.
        assert!(!super::inline_page().contains("src=\"./app.js\""));
    }

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
        assert!(is_local_origin("http://127.0.0.1:7878", 7878));
        assert!(is_local_origin("http://localhost:7878", 7878));
        assert!(!is_local_origin("https://evil.example", 7878));
        assert!(!is_local_origin("http://127.0.0.1.evil.example", 7878));
        assert!(!is_local_origin("http://localhost.evil.example:80", 7878));
        assert!(!is_local_origin("null", 7878));
        // Another program's page on this Mac: another port.
        assert!(!is_local_origin("http://127.0.0.1:8080", 7878));
        assert!(!is_local_origin("http://localhost:7879", 7878));
        assert!(!is_local_origin("http://127.0.0.1", 7878));
        assert!(!is_local_origin("http://127.0.0.1:7878/", 7878));
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
        let mut sam = Client::new(Viewer::device("Sam"), &[]);
        assert!(forward(trial(), &mut sam).is_none());
        // Even the home computer connection gets them only as the owner.
        let mut home_computer = Client::new(Viewer::home_computer("Jay"), &[]);
        assert!(forward(trial(), &mut home_computer).is_none());
        home_computer.owner = true;
        assert!(forward(trial(), &mut home_computer).is_some());
        // A practice run's key is never a bot anyone is subscribed to.
        home_computer.subs.insert("jobs".to_owned(), 0);
        assert!(forward(event("t--0123456789abcdef", 1), &mut home_computer).is_none());
    }

    #[test]
    fn forward_skips_other_bots_and_replayed_events() {
        let mut client = Client::new(Viewer::home_computer("Jay"), &[]);
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
    fn the_log_is_the_home_computers_and_a_persons_notice_is_theirs() {
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
        let mut home_computer = Client::new(Viewer::home_computer("Jay"), &[]);
        let mut sam = Client::new(Viewer::device("Sam"), &[]);
        let mut ann = Client::new(Viewer::device("Ann"), &[]);
        assert!(forward(log(), &mut home_computer).is_some());
        assert!(forward(log(), &mut ann).is_none());
        assert!(forward(notice(Some("Sam")), &mut sam).is_some());
        assert!(forward(notice(Some("Sam")), &mut home_computer).is_some());
        assert!(forward(notice(Some("Sam")), &mut ann).is_none());
        assert!(forward(notice(None), &mut ann).is_some());
    }

    #[test]
    fn nothing_about_a_private_bot_reaches_anyone_else() {
        let bots = [
            info("mine", Some("Sam"), true),
            info("shared", Some("Sam"), false),
        ];
        let mut ann = Client::new(Viewer::device("Ann"), &bots);
        let mut home_computer = Client::new(Viewer::home_computer("Jay"), &bots);
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
        let Some(ServerMsg::Bots { bots: seen }) = forward(list, &mut home_computer) else {
            panic!("a list")
        };
        assert_eq!(seen.len(), 2, "the home computer sees every bot");

        let activity = ServerMsg::Activity {
            bot: "mine".into(),
            text: "Thinking…".into(),
        };
        assert!(forward(activity.clone(), &mut ann).is_none());
        assert!(forward(activity, &mut home_computer).is_some());

        let sams = ServerMsg::Sites {
            sites: vec![],
            owner: Some("Sam".into()),
            owner_id: None,
        };
        assert!(
            forward(sams.clone(), &mut ann).is_none(),
            "someone else's sign-ins"
        );
        assert!(forward(sams, &mut Client::new(Viewer::device("Sam"), &bots)).is_some());
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
        assert!(forward(report, &mut home_computer).is_some());
        let list = ServerMsg::Reports {
            bot: "mine".into(),
            reports: vec![],
        };
        assert!(forward(list.clone(), &mut ann).is_none());
        assert!(forward(list, &mut home_computer).is_some());

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
            schedule: None,
            also: Vec::new(),
        })
    }

    #[test]
    fn a_bot_shared_with_picked_people_reaches_them_and_its_sign_ins_its_maker() {
        let (jay, sam, ann) = (
            oxbot_core::id::PersonId::random(),
            oxbot_core::id::PersonId::random(),
            oxbot_core::id::PersonId::random(),
        );
        let garden = oxbot_protocol::BotInfo {
            owner: Some(jay.to_string()),
            sharing: oxbot_protocol::Sharing::People,
            members: vec!["Sam".into()],
            member_ids: vec![sam.to_string()],
            ..info("garden", Some("Jay"), true)
        };
        let bots = [garden];
        let client = |id: &oxbot_core::id::PersonId, name: &str| {
            Client::new(Viewer::person(id.clone(), name, false), &bots)
        };
        let (mut j, mut s, mut a) = (
            client(&jay, "Jay"),
            client(&sam, "Sam"),
            client(&ann, "Ann"),
        );
        assert!(j.sees("garden") && s.sees("garden") && !a.sees("garden"));
        // The list as each gets it: Sam may use it, not change it; Ann
        // sees a lock and nobody's names.
        let Some(ServerMsg::Bots { bots: for_sam }) = forward(
            ServerMsg::Bots {
                bots: bots.to_vec(),
            },
            &mut s,
        ) else {
            panic!("a list")
        };
        assert!(!for_sam[0].locked && for_sam[0].can_task && !for_sam[0].can_manage);
        let Some(ServerMsg::Bots { bots: for_ann }) = forward(
            ServerMsg::Bots {
                bots: bots.to_vec(),
            },
            &mut a,
        ) else {
            panic!("a list")
        };
        assert!(for_ann[0].locked && for_ann[0].members.is_empty());
        let json = serde_json::to_string(&for_sam).unwrap();
        assert!(
            !json.contains(sam.as_str()) && !json.contains(jay.as_str()),
            "no ids are sent"
        );
        // Its cards go to its people.
        let ServerMsg::ApprovalNeeded(mut card) = card("garden", Some("Ann")) else {
            panic!("a card")
        };
        card.asker_id = Some(ann.to_string());
        card.also = vec![jay.to_string(), sam.to_string()];
        let card = ServerMsg::ApprovalNeeded(card);
        assert!(forward(card.clone(), &mut s).is_some() && forward(card.clone(), &mut j).is_some());
        assert!(forward(card, &mut a).is_none(), "Ann can't open it");
        // Its browser is Jay's: only Jay signs it in.
        let sign_in = ServerMsg::SignInNeeded(oxbot_protocol::SignInRequest {
            id: 1,
            bot: "garden".into(),
            url: "https://x.example/".into(),
            site: "x.example".into(),
            why: String::new(),
            window_open: false,
            before: None,
            only: Some(jay.to_string()),
        });
        assert!(forward(sign_in.clone(), &mut j).is_some());
        assert!(forward(sign_in, &mut s).is_none());
    }

    #[test]
    fn a_scheduled_runs_line_goes_to_its_person_alone_and_lists_go_nowhere() {
        let sam = oxbot_core::id::PersonId::random();
        let bots = [info("shared", None, false), info("bank", Some("Ann"), true)];
        let mut real = Client::new(Viewer::person(sam.clone(), "Sam", false), &bots);
        let mut home_computer = Client::new(Viewer::home_computer("Jay"), &bots);
        let mut by_name = Client::new(Viewer::device("Sam"), &bots);
        let line = |bot: &str| ServerMsg::Notify {
            bot: bot.into(),
            text: "Shared: done".into(),
            waiting: false,
            person_id: sam.to_string(),
        };
        let sent = forward(line("shared"), &mut real).unwrap();
        let json = serde_json::to_string(&sent).unwrap();
        assert!(
            !json.contains(sam.as_str()),
            "the person's id is never sent"
        );
        assert!(
            forward(line("shared"), &mut home_computer).is_none(),
            "not the home computer's"
        );
        assert!(
            forward(line("shared"), &mut by_name).is_none(),
            "a name alone"
        );
        assert!(
            forward(line("bank"), &mut real).is_none(),
            "a bot they can't open"
        );
        let list = ServerMsg::Schedules {
            bot: None,
            schedules: vec![],
        };
        assert!(
            forward(list, &mut home_computer).is_none(),
            "made for one viewer"
        );
        assert!(forward(ServerMsg::SchedulesChanged, &mut real).is_some());
    }

    #[tokio::test]
    async fn a_profile_is_its_persons_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let (out, _rx) = mpsc::channel(8);
        let sam = state.hub.person_named("Sam").unwrap();
        let mut sam = Client::new(Viewer::person(sam.id, "Sam", false), &[]);
        let edit = oxbot_protocol::ProfileEdit {
            name: "Sam".into(),
            color: oxbot_core::store::PEOPLE_COLORS[2].into(),
            food: "vegan".into(),
            food_use: true,
            ..oxbot_protocol::ProfileEdit::default()
        };
        let replies = handle(
            ClientMsg::SetProfile {
                profile: Box::new(edit),
            },
            &state,
            &mut sam,
            &out,
        )
        .await;
        let [ServerMsg::Profile(view)] = &replies[..] else {
            panic!("{replies:?}")
        };
        assert_eq!(view.food, "vegan");
        // Never passed on to anyone else's connection.
        let mut home_computer = Client::new(Viewer::home_computer("Jay"), &[]);
        assert!(forward(ServerMsg::Profile(view.clone()), &mut home_computer).is_none());
        assert!(forward(ServerMsg::ProfilesChanged, &mut home_computer).is_some());
        let replies = handle(ClientMsg::GetProfile, &state, &mut sam, &out).await;
        assert!(matches!(&replies[..], [ServerMsg::Profile(v)] if v.food == "vegan"));
    }

    #[tokio::test]
    async fn the_clock_moves_only_in_test_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let (out, _rx) = mpsc::channel(8);
        let mut home_computer = Client::new(state.home_computer_viewer(), &[]);
        let replies = handle(
            ClientMsg::E2eAdvanceClock { minutes: Some(60) },
            &state,
            &mut home_computer,
            &out,
        )
        .await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { message }] if message.contains("only for tests"))
        );
        let replies = handle(
            ClientMsg::ListSchedules { bot: None },
            &state,
            &mut home_computer,
            &out,
        )
        .await;
        assert!(
            matches!(&replies[..], [ServerMsg::Schedules { schedules, .. }] if schedules.is_empty())
        );
    }

    #[test]
    fn a_card_goes_to_who_asked_and_the_home_computer() {
        let bots = [info("shared", None, false)];
        let mut jay = Client::new(Viewer::device("Jay"), &bots);
        let mut sam = Client::new(Viewer::device("Sam"), &bots);
        let mut home_computer = Client::new(Viewer::home_computer("Jay"), &bots);
        assert!(forward(card("shared", Some("Jay")), &mut jay).is_some());
        assert!(forward(card("shared", Some("Jay")), &mut home_computer).is_some());
        assert!(
            forward(card("shared", Some("Jay")), &mut sam).is_none(),
            "Sam can't answer Jay's card"
        );
        assert!(
            forward(card("shared", None), &mut sam).is_none(),
            "nobody asked: the home computer's"
        );
        assert!(forward(card("shared", None), &mut home_computer).is_some());
    }

    #[test]
    fn a_persons_sites_notices_and_cards_go_to_them_by_id() {
        let sam = oxbot_core::id::PersonId::random();
        let mut real = Client::new(Viewer::person(sam.clone(), "Samuel", false), &[]);
        let mut lookalike = Client::new(
            Viewer::person(oxbot_core::id::PersonId::random(), "Sam", false),
            &[],
        );
        let mut by_name = Client::new(Viewer::device("Sam"), &[]);
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

    /// A server like [`a_test_connection_names_its_person_on_purpose`]'s,
    /// in test mode; its state and address.
    async fn test_mode_server(dir: &std::path::Path) -> (AppState, std::net::SocketAddr) {
        let hub = Hub::new(
            vec![],
            oxbot_core::Store::open_in_memory().unwrap(),
            oxbot_core::HubConfig {
                enrol_named_viewers: false,
                ..oxbot_core::HubConfig::new(dir)
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = AppState {
            e2e: true,
            port: addr.port(),
            ..AppState::new(hub, "test-mac")
        };
        state.hub.owner_person_from("Jay", true).unwrap();
        tokio::spawn(serve(listener, state.clone(), std::future::pending()));
        (state, addr)
    }

    /// A raw request (`head` lines after the request line); the reply.
    async fn raw(addr: std::net::SocketAddr, line: &str, head: &str) -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        tcp.write_all(format!("{line} HTTP/1.1\r\n{head}Connection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reply = String::new();
        tcp.read_to_string(&mut reply).await.unwrap();
        reply
    }

    #[tokio::test]
    async fn only_test_mode_gives_the_page_its_test_names() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _) = test_mode_server(tmp.path()).await;
        assert!(state.hello().test, "a debug build in test mode");
        let normal = AppState {
            e2e: false,
            ..state
        };
        assert!(!normal.hello().test);
    }

    #[tokio::test]
    async fn a_test_connection_never_names_the_home_computers_person() {
        let tmp = tempfile::tempdir().unwrap();
        let (_state, addr) = test_mode_server(tmp.path()).await;
        assert!(
            tokio_tungstenite::connect_async(format!("ws://{addr}/ws?as=Jay"))
                .await
                .is_err(),
            "the owner takes the key"
        );
        assert!(
            tokio_tungstenite::connect_async(format!("ws://{addr}/ws?as=Ann"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn only_oxbots_own_address_and_page_get_an_answer() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
        let tmp = tempfile::tempdir().unwrap();
        let (state, addr) = test_mode_server(tmp.path()).await;
        let port = addr.port();
        let key = state.home_computer_key.clone();
        let upgrade = "Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n";
        // Another name for 127.0.0.1 (DNS rebinding), or the right name on
        // another port: refused on every route, before anything is read.
        for host in [
            "evil.example",
            "evil.example:{port}",
            "localhost:1",
            "127.0.0.1",
        ] {
            let host = host.replace("{port}", &port.to_string());
            for line in [
                "GET /".to_owned(),
                "GET /app.js".to_owned(),
                format!("GET /ws?k={key}"),
                format!("GET /hello?n={}", "a".repeat(32)),
                "POST /upload?bot=assistant&name=a.txt".to_owned(),
            ] {
                let reply = raw(addr, &line, &format!("Host: {host}\r\n{upgrade}")).await;
                assert!(reply.starts_with("HTTP/1.1 403"), "{host} {line}: {reply}");
            }
        }
        for host in [format!("127.0.0.1:{port}"), format!("localhost:{port}")] {
            let reply = raw(addr, "GET /", &format!("Host: {host}\r\n")).await;
            assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
            let headers = reply.to_ascii_lowercase();
            assert!(headers.contains("x-frame-options: deny"), "{reply}");
            assert!(headers.contains("frame-ancestors 'none'"), "{reply}");
        }

        // The page at another port (another program's page) can't connect,
        // even with the key; its own can.
        let with_origin = |origin: &str| {
            let mut req = format!("ws://{addr}/ws?k={key}")
                .into_client_request()
                .unwrap();
            req.headers_mut().insert("Origin", origin.parse().unwrap());
            req
        };
        for origin in [
            format!("http://127.0.0.1:{}", port + 1),
            format!("http://localhost:{}", port + 1),
            "http://127.0.0.1".to_owned(),
            format!("https://127.0.0.1:{port}"),
            format!("http://127.0.0.1.evil.example:{port}"),
            "null".to_owned(),
        ] {
            assert!(
                tokio_tungstenite::connect_async(with_origin(&origin))
                    .await
                    .is_err(),
                "{origin}"
            );
        }
        for origin in [
            format!("http://127.0.0.1:{port}"),
            format!("http://localhost:{port}"),
        ] {
            assert!(
                tokio_tungstenite::connect_async(with_origin(&origin))
                    .await
                    .is_ok(),
                "{origin}"
            );
        }
        // No Origin (a program, not a page): only with the key.
        assert!(
            tokio_tungstenite::connect_async(format!("ws://{addr}/ws?k={key}"))
                .await
                .is_ok()
        );
        assert!(
            tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
                .await
                .is_err()
        );
        // An upload from the wrong page is refused too.
        let reply = raw(
            addr,
            "POST /upload?bot=assistant&name=a.txt",
            &format!(
                "Host: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{}\r\nX-Oxbot-Key: {key}\r\nContent-Length: 1\r\n",
                port + 1
            ),
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
    }

    #[tokio::test]
    async fn hello_proves_this_is_the_oxbot_with_the_key() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, addr) = test_mode_server(tmp.path()).await;
        let host = format!("Host: 127.0.0.1:{}\r\n", addr.port());
        let nonce = crate::secret::random_hex::<16>();
        let reply = raw(addr, &format!("GET /hello?n={nonce}"), &host).await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let body = reply.split("\r\n\r\n").nth(1).unwrap();
        assert!(crate::secret::hello_proof_ok(
            &state.home_computer_key,
            &nonce,
            body
        ));
        assert!(!body.contains(&state.home_computer_key));
        for bad in [
            "/hello",
            "/hello?n=short",
            "/hello?n=%2F%2F%2F%2F%2F%2F%2F%2F%2F%2F%2F%2F%2F%2F%2F%2F",
        ] {
            let reply = raw(addr, &format!("GET {bad}"), &host).await;
            assert!(reply.starts_with("HTTP/1.1 400"), "{bad}: {reply}");
        }
    }

    #[tokio::test]
    async fn a_device_runs_the_setup_check_at_most_every_ten_seconds() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let (out, _rx) = mpsc::channel(4);
        let sam = Viewer::device("Sam");
        let mut client = client_for(&state, &sam);
        let first = handle(ClientMsg::CheckSetup, &state, &mut client, &out).await;
        assert!(matches!(
            &first[..],
            [ServerMsg::Setup(_), ServerMsg::Settings(_)]
        ));
        let checked = client.setup_checked.unwrap();
        let again = handle(ClientMsg::CheckSetup, &state, &mut client, &out).await;
        assert!(matches!(
            &again[..],
            [ServerMsg::Setup(_), ServerMsg::Settings(_)]
        ));
        assert_eq!(
            client.setup_checked,
            Some(checked),
            "the last result, not a new check"
        );
        // The home computer checks whenever it asks.
        let home = state.home_computer_viewer();
        let mut client = client_for(&state, &home);
        handle(ClientMsg::CheckSetup, &state, &mut client, &out).await;
        let first = client.setup_checked.unwrap();
        handle(ClientMsg::CheckSetup, &state, &mut client, &out).await;
        assert!(client.setup_checked.unwrap() > first);
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
        let ann = state.hub.resolve(&Viewer::device("Ann")).into_owned();
        assert!(ann.id.is_some(), "made when the page connected");
    }

    #[tokio::test]
    async fn only_the_home_computer_keeps_the_mac_awake_and_turns_it_off() {
        let tmp = tempfile::tempdir().unwrap();
        let base = state(tmp.path());
        let awake = Arc::new(crate::awake::KeepAwake::start(
            crate::awake::Holder::Pretend,
            base.hub.keep_awake_on(),
            std::time::Duration::from_millis(10),
        ));
        let state = AppState {
            awake: Some(Arc::clone(&awake)),
            ..base
        };
        // On by default, and only the home computer sees it.
        assert_eq!(state.settings().keep_awake, Some(true));
        assert!(awake.is_on());
        let sam = Viewer::device("Sam");
        assert_eq!(state.settings_for(&sam).keep_awake, None);
        assert!(home_computer_only(&ClientMsg::SetKeepAwake { on: false }));
        let (out, _rx) = mpsc::channel(4);
        let mut phone = Client::new(sam, &[]);
        let replies = handle(
            ClientMsg::SetKeepAwake { on: false },
            &state,
            &mut phone,
            &out,
        )
        .await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { message }] if message.contains("home computer"))
        );
        assert!(state.hub.keep_awake_on());
        // The home computer turns it off: kept, and the program stops.
        let replies = handle_home_computer(ClientMsg::SetKeepAwake { on: false }, &state)
            .await
            .unwrap();
        assert!(matches!(&replies[..], [ServerMsg::Settings(s)] if s.keep_awake == Some(false)));
        assert!(!state.hub.keep_awake_on());
        assert!(!awake.is_on());
        // Not a Mac (no keep-awake here): it doesn't apply.
        let none = AppState {
            awake: None,
            ..state.clone()
        };
        assert_eq!(none.settings().keep_awake, None);
        assert!(
            handle_home_computer(ClientMsg::SetKeepAwake { on: true }, &none)
                .await
                .is_ok_and(|r| matches!(&r[..], [ServerMsg::Error { .. }]))
        );
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
    async fn a_device_named_like_the_home_computers_person_is_not_the_home_computer() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let home_computer = state.home_computer_viewer();
        assert!(home_computer.home_computer && home_computer.id.is_some());
        assert_eq!(assistant_for(&state, &home_computer), DEFAULT_BOT);

        // Another person's device that calls itself "Jay".
        let sam = state.hub.resolve(&Viewer::device("Sam")).into_owned();
        let lookalike = Viewer {
            name: "Jay".into(),
            ..sam.clone()
        };
        let assistant = assistant_for(&state, &lookalike);
        assert_ne!(assistant, DEFAULT_BOT, "not the home computer's Assistant");
        assert_eq!(assistant, assistant_for(&state, &sam), "Sam's own");
        assert!(
            !state.settings_for(&lookalike).home_computer,
            "no home computer rights"
        );
        // The home computer's person's own phone (same id, no secret) isn't the home computer either.
        let phone = Viewer {
            home_computer: false,
            ..home_computer.clone()
        };
        assert_eq!(assistant_for(&state, &phone), DEFAULT_BOT);
        assert!(!state.settings_for(&phone).home_computer);
    }

    #[tokio::test]
    async fn my_own_browser_is_never_home_for_someone_oxbot_cant_place() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        for msg in [
            ClientMsg::SignOutAll { mine: true },
            ClientMsg::OpenSite {
                site: "bank.example".into(),
                mine: true,
            },
        ] {
            let replies = handle_sign_ins(msg, &state, &Viewer::device("")).await;
            assert!(
                matches!(&replies[..], [ServerMsg::Error { message }] if message.contains("whose browser")),
                "{replies:?}"
            );
        }
    }

    #[test]
    fn each_person_sees_only_their_own_assistant_as_assistant() {
        let anns = oxbot_core::bot::personal_assistant_id("Ann").to_string();
        let sams = oxbot_core::bot::personal_assistant_id("Sam").to_string();
        let mut assistants = [
            info(DEFAULT_BOT, None, false),
            info(&anns, Some("Ann"), true),
            info(&sams, Some("Sam"), true),
        ];
        for h in &mut assistants {
            h.central = true;
        }
        let bots = [assistants.to_vec(), vec![info("recipes", None, false)]].concat();
        let ann = Client::with_assistant(Viewer::device("Ann"), &bots, anns.clone());
        let home_computer =
            Client::with_assistant(Viewer::home_computer("Jay"), &bots, DEFAULT_BOT.to_owned());

        let Some(ServerMsg::Bots { bots: seen }) =
            ann.outbound(ServerMsg::Bots { bots: bots.clone() })
        else {
            panic!("a list")
        };
        let ids: Vec<&str> = seen.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, [DEFAULT_BOT, "recipes"]);
        let Some(ServerMsg::Bots { bots: seen }) = home_computer.outbound(ServerMsg::Bots { bots })
        else {
            panic!("a list")
        };
        assert_eq!(seen.iter().filter(|b| b.central).count(), 1);

        // What Ann says to `assistant` goes to her own Assistant, and back.
        let ClientMsg::Send { bot, .. } = ann.inbound(ClientMsg::Send {
            bot: DEFAULT_BOT.into(),
            text: "hi".into(),
            files: vec![],
        }) else {
            panic!("a send")
        };
        assert_eq!(bot, anns);
        let Some(ServerMsg::Event(e)) = ann.outbound(event(&anns, 1)) else {
            panic!("her Assistant's event")
        };
        assert_eq!(e.bot, DEFAULT_BOT);
        assert!(ann.outbound(event(&sams, 1)).is_none(), "not Sam's");
        assert!(
            ann.outbound(event(DEFAULT_BOT, 1)).is_none(),
            "not the home computer's"
        );
        assert!(home_computer.outbound(event(&anns, 1)).is_none());
    }

    /// A server with Sam's shared bot `shop`; Jay is the home computer's person.
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
        let sam = state.hub.resolve(&Viewer::device("Sam")).into_owned();
        let jay = state.hub.resolve(&Viewer::device("Jay")).into_owned();
        let with_id = |asker: &Viewer| oxbot_protocol::Approval {
            asker_id: asker.id.as_ref().map(ToString::to_string),
            ..save_card(&asker.name)
        };
        // Sam's own card on Sam's bot: offered to Sam and the home computer.
        assert!(offered(&state, with_id(&sam), &Viewer::device("Sam")));
        assert!(offered(
            &state,
            with_id(&sam),
            &Viewer::home_computer("Jay")
        ));
        // Jay's card on Sam's bot: Jay may only allow it once.
        assert!(!offered(&state, with_id(&jay), &Viewer::device("Jay")));
        // A card that names no rule (pay, Enter, upload) is never offered.
        let plain = oxbot_protocol::Approval {
            rule: None,
            ..with_id(&sam)
        };
        assert!(!offered(&state, plain, &Viewer::home_computer("Jay")));
        // The rule itself is never sent.
        let json = serde_json::to_string(&with_id(&sam)).unwrap();
        assert!(!json.contains("Save"), "{json}");

        // Rule lists go only to those who manage the bot.
        let rules = || ServerMsg::AllowRules {
            bot: "shop".into(),
            rules: Vec::new(),
        };
        assert!(for_viewer(rules(), &state, &Viewer::device("Sam")).is_some());
        assert!(for_viewer(rules(), &state, &Viewer::home_computer("Jay")).is_some());
        assert!(for_viewer(rules(), &state, &Viewer::device("Jay")).is_none());
    }

    #[tokio::test]
    async fn rules_are_listed_and_removed_only_by_those_who_manage_the_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let state = shop_state(tmp.path());
        let (out, _rx) = mpsc::channel(4);
        let bots = [info("shop", Some("Sam"), false)];
        let mut jay = Client::new(Viewer::device("Jay"), &bots);
        let mut sam = Client::new(Viewer::device("Sam"), &bots);
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
            schedule: None,
        };
        assert_eq!(handle(answer, &state, &mut sam, &out).await, []);
    }

    #[test]
    fn rule_requests_for_assistant_go_to_your_own_assistant() {
        let ann = Client::with_assistant(Viewer::device("Ann"), &[], "assistant--ann-1".into());
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
            panic!("her Assistant's rules")
        };
        assert_eq!(bot, DEFAULT_BOT);
    }

    #[test]
    fn a_persons_sections_go_to_them_alone_with_assistant_ids_as_they_see_them() {
        let sam = oxbot_core::id::PersonId::random();
        let anns = oxbot_core::bot::personal_assistant_id("Ann").to_string();
        let sams = oxbot_core::bot::personal_assistant_id("Sam").to_string();
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
            Client::with_assistant(Viewer::person(sam.clone(), "Sam", false), &[], sams.clone());
        let mut home_computer = Client::new(
            Viewer::person(oxbot_core::id::PersonId::random(), "Jay", true),
            &[],
        );
        let mut by_name = Client::new(Viewer::device("Sam"), &[]);
        let mut nobody = Client::new(Viewer::home_computer(""), &[]);
        assert!(
            forward(sections.clone(), &mut home_computer).is_none(),
            "not even the home computer"
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
            "their own Assistant as assistant; no other the Assistant"
        );

        // `assistant` in a message means their own Assistant.
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
    async fn sections_need_no_home_computer_rights() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state(tmp.path());
        let ann = state.hub.resolve(&Viewer::device("Ann")).into_owned();
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
            assert!(!home_computer_only(&msg));
        }
        let (out, _rx) = mpsc::channel(4);
        let assistant = assistant_for(&state, &ann);
        let mut client =
            Client::with_visible(ann.clone(), std::collections::HashSet::new(), assistant);
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
            matches!(&replies[..], [ServerMsg::Error { message }] if message.contains("Assistant")),
            "{replies:?}"
        );
    }

    const BANK_REPORT: &str = "r_0123456789abcdef0123456789abcdef";
    const ASSISTANT_REPORT: &str = "r_fedcba9876543210fedcba9876543210";

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
    /// report in Ann's own Assistant.
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
            .add_report(
                &ann.assistant,
                &mut a_report(ASSISTANT_REPORT, "Ann's research"),
            )
            .unwrap();
        let central = oxbot_core::Bot::create(
            dir,
            oxbot_core::BotId::new(DEFAULT_BOT).unwrap(),
            "Assistant",
            "",
        )
        .unwrap();
        let hub = Hub::new(vec![central, bank], store, oxbot_core::HubConfig::new(dir));
        AppState::new(hub, "test-mac")
    }

    fn client_for(state: &AppState, viewer: &Viewer) -> Client {
        let viewer = state.hub.resolve(viewer).into_owned();
        let assistant = assistant_for(state, &viewer);
        let visible = state.hub.visible_ids(&viewer);
        Client::with_visible(viewer, visible, assistant)
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
        for viewer in [Viewer::home_computer("Jay"), Viewer::device("Sam")] {
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
        let mut ann = client_for(&state, &Viewer::device("Ann"));
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
        // `assistant` is Ann's own Assistant, there and back.
        let got = handle(open(DEFAULT_BOT, ASSISTANT_REPORT), &state, &mut ann, &out).await;
        let [ServerMsg::Report(view)] = &got[..] else {
            panic!("her Assistant's report: {got:?}")
        };
        let Some(ServerMsg::Report(view)) = ann.outbound(ServerMsg::Report(view.clone())) else {
            panic!("passed on")
        };
        assert_eq!(view.bot, DEFAULT_BOT);
        assert!(view.html.contains("By Assistant"));
        // The home computer's page doesn't see Ann's Assistant at all.
        let mut home_computer = client_for(&state, &Viewer::home_computer("Jay"));
        let anns = assistant_for(&state, &ann.viewer);
        let got = handle(
            open(&anns, ASSISTANT_REPORT),
            &state,
            &mut home_computer,
            &out,
        )
        .await;
        assert!(home_computer.outbound(got[0].clone()).is_none(), "{got:?}");

        // Deleting is for those who manage the bot.
        let mut sam = client_for(&state, &Viewer::device("Sam"));
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

    /// A server with every engine rule on: Recipes (shared, Codex) and
    /// Sam's own Notes (`OpenRouter`); Jay is the home computer's person.
    fn engines_state(dir: &std::path::Path) -> (AppState, Viewer) {
        engines_state_with(dir, None)
    }

    /// [`engines_state`] with the bots' model requests on `openrouter`.
    fn engines_state_with(
        dir: &std::path::Path,
        openrouter: Option<oxbot_core::LoopParts>,
    ) -> (AppState, Viewer) {
        use oxbot_core::{Bot, BotId};
        let store = oxbot_core::Store::open(&dir.join("oxbot.db")).unwrap();
        let (sam, _) = store.find_or_create_person("Sam").unwrap();
        let notes = Bot::create_owned(
            dir,
            BotId::new("notes").unwrap(),
            "Notes",
            "",
            Some(&sam.id),
            Some("Sam"),
        )
        .unwrap();
        notes.save().unwrap();
        let bots = vec![
            Bot::create(dir, BotId::new("assistant").unwrap(), "Assistant", "").unwrap(),
            Bot::create(dir, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
            notes,
        ];
        let config = oxbot_core::HubConfig {
            openrouter,
            ..oxbot_core::HubConfig::new(dir)
        };
        let hub = Hub::new(bots, store, config);
        let state = AppState {
            local_person: "Jay".to_owned(),
            ..AppState::new(hub, "test-mac")
        };
        // Chooses Jay as the home computer's person.
        let _ = state.home_computer_viewer();
        let sam = Viewer::person(sam.id, "Sam", false);
        (state, sam)
    }

    #[tokio::test]
    async fn anyone_may_task_the_bots_they_see_and_engines_are_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, sam) = engines_state(tmp.path());
        let home_computer = state.home_computer_viewer();
        let jay_phone = Viewer {
            home_computer: false,
            ..home_computer.clone()
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
        assert_eq!(can_task(&home_computer), all);
        assert_eq!(can_task(&jay_phone), all);
        assert_eq!(can_task(&sam), all);
        // The engine is listed for everyone.
        assert!(
            bots.iter()
                .all(|b| b.engine == oxbot_protocol::EngineKind::Codex)
        );

        let settings = state.settings_for(&sam);
        assert!(!settings.owner && settings.owner_name == "Jay");
        assert!(!settings.openrouter_ready);
        assert!(state.settings_for(&jay_phone).owner);
        assert!(state.settings_for(&home_computer).owner);
    }

    #[tokio::test]
    async fn bots_are_made_and_switched_only_on_an_engine_that_can_run() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, sam) = engines_state(tmp.path());
        let (out, _rx) = mpsc::channel(4);
        let mut client = Client::with_visible(
            sam.clone(),
            state.hub.visible_ids(&sam),
            assistant_for(&state, &sam),
        );
        // No OpenRouter here: bots can't run, and nothing changes.
        let to_codex = ClientMsg::SetEngine {
            bot: "notes".into(),
            engine: oxbot_protocol::EngineKind::Codex,
            model: None,
            models: None,
        };
        assert!(!home_computer_only(&to_codex));
        let replies = handle(to_codex, &state, &mut client, &out).await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { message }] if message.starts_with("The home's AI account (OpenRouter) isn't connected")),
            "{replies:?}"
        );
        assert_eq!(state.hub.engine_of_bot("notes"), Some(EngineKind::Codex));
        let make = ClientMsg::CreateBot {
            name: "Diary".into(),
            instructions: String::new(),
            private: true,
            engine: Some(oxbot_protocol::EngineKind::Codex),
            models: Vec::new(),
        };
        let replies = handle(make, &state, &mut client, &out).await;
        assert!(
            matches!(&replies[..], [ServerMsg::Error { message }] if message.starts_with("The home's AI account (OpenRouter) isn't connected")),
            "{replies:?}"
        );
        assert!(state.hub.bot("diary").is_err(), "nothing made");

        // Everyone gets the notice: where the bots' model calls go.
        let view = state.engines_for(&sam);
        assert_eq!(view.notice, oxbot_core::hub::ENGINES_NOTICE);
        assert!(
            view.notice
                .contains("goes only to OpenRouter (or a model on this computer)")
        );
        assert!(
            view.codex.is_none() && view.openrouter.is_none(),
            "the home computer's only"
        );
        let json = serde_json::to_string(&ServerMsg::Engines(view)).unwrap();
        assert!(!json.to_lowercase().contains("claude"), "{json}");
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
        // and bots say what they wait for.
        let own = format!("localhost:{}", state.port);
        let (status, _) = get_page(state.port, "/", &own).await;
        assert_eq!(status, 200);
        let settings = state.settings_for(&state.home_computer_viewer());
        assert!(settings.keychain_waiting && !settings.openrouter_ready);
        assert_eq!(
            state.hub.engine_ready(EngineKind::Codex),
            Err(oxbot_llm::KEYCHAIN_WAITING.to_owned())
        );
        assert!(!loading.is_finished());

        // Allowed: everything goes on.
        answer.send(()).unwrap();
        loading.await.unwrap();
        let settings = state.settings_for(&state.home_computer_viewer());
        assert!(!settings.keychain_waiting && settings.openrouter_ready);
        assert!(state.hub.openrouter_ready());
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
    async fn only_the_home_computer_signs_in_and_the_key_never_reaches_a_page() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, sam, _) = signed_in_state(tmp.path()).await;
        let (out, _rx) = mpsc::channel(4);
        let home_computer = state.home_computer_viewer();
        for msg in [
            ClientMsg::OpenRouterSignIn,
            ClientMsg::OpenRouterSignOut,
            ClientMsg::RefreshModels,
            ClientMsg::SetEnginePrefs {
                prefs: oxbot_protocol::EnginePrefs::default(),
            },
            ClientMsg::FindCodex {
                path: "/tmp/not-codex".into(),
            },
        ] {
            assert!(home_computer_only(&msg));
            let mut client = Client::with_visible(
                sam.clone(),
                state.hub.visible_ids(&sam),
                assistant_for(&state, &sam),
            );
            let replies = handle(msg, &state, &mut client, &out).await;
            assert!(
                matches!(&replies[..], [ServerMsg::Error { message }] if message == "That can only be done on the home computer."),
                "{replies:?}"
            );
        }
        // Sam gets no account details; the home computer does, but never the key.
        assert!(state.engines_for(&sam).openrouter.is_none());
        let view = state.engines_for(&home_computer);
        assert!(view.openrouter.as_ref().unwrap().signed_in);
        // A memory store is no key file: the page keeps the Keychain words.
        assert!(!view.openrouter.as_ref().unwrap().key_in_file);
        for json in [
            serde_json::to_string(&ServerMsg::Engines(view)).unwrap(),
            serde_json::to_string(&ServerMsg::Settings(state.settings_for(&home_computer)))
                .unwrap(),
            format!("{state:?}"),
        ] {
            assert!(!json.contains("feedfacecafe"), "{json}");
        }

        let mut client = Client::with_visible(
            home_computer.clone(),
            state.hub.visible_ids(&home_computer),
            assistant_for(&state, &home_computer),
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
        // A real sign-in: a wrong guess leaves it waiting; its own state
        // reaches the exchange (the stand-in refuses) and is used up.
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
        let (status, page) =
            get_page(state.port, "/openrouter/callback?code=c&state=wrong", &own).await;
        assert_eq!(status, 400);
        assert!(page.contains("isn't valid any more"), "{page}");
        let (status, page) = get_page(
            state.port,
            &format!("/openrouter/callback?code=c&state={live}"),
            &own,
        )
        .await;
        assert_eq!(status, 400);
        assert!(page.contains("didn't accept this sign-in"), "{page}");
        let (status, page) = get_page(
            state.port,
            &format!("/openrouter/callback?code=c&state={live}"),
            &own,
        )
        .await;
        assert_eq!(status, 400, "single use: {page}");
    }

    #[tokio::test]
    async fn signing_out_forgets_the_key_and_prefs_are_the_home_computers() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _, secrets) = signed_in_state(tmp.path()).await;
        let home_computer = state.home_computer_viewer();
        let (out, _rx) = mpsc::channel(4);
        let mut client = Client::with_visible(
            home_computer.clone(),
            state.hub.visible_ids(&home_computer),
            assistant_for(&state, &home_computer),
        );
        let prefs = oxbot_protocol::EnginePrefs {
            paid_fallback: false,
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
        assert!(
            !state
                .engines_for(&home_computer)
                .openrouter
                .unwrap()
                .signed_in
        );
    }
}
