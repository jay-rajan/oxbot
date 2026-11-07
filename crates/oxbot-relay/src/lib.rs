//! oxbot's model relay: an HTTP server on 127.0.0.1 that is the only place
//! a Codex process may send model requests.
//!
//! - One route, `POST /v1/responses` (OpenAI's Responses API, streamed or
//!   not); everything else is a 404.
//! - Each Codex process gets its own random token ([`Relay::register`]),
//!   passed in its environment; a request without a registered token is
//!   refused. Which bot a request is for comes only from that token and
//!   from oxbot's own record of the bot's current turn
//!   ([`RelayClient::begin_turn`]), never from anything Codex sends.
//! - The body is rebuilt from an allow list ([`shape`]): nothing that
//!   identifies a person, a device or a session leaves, and no header of
//!   Codex's is copied.
//! - The model, its fallbacks, the provider's privacy rules and the limits
//!   come from the loop's gateway ([`oxbot_loop::gateway`]): a busy or
//!   rate-limited model hands over to the next, a limit stops the request
//!   with oxbot's own words, and every request is metered
//!   (`model_calls`, engine `codex`).
//! - Personal details stay home ([`personal`]): unless the owner turned
//!   it off, every personal detail in the conversation is swapped for a
//!   stand-in before it leaves (`[email 1]`, `[person 2]`), and swapped
//!   back in the answer as it streams to Codex. A chat's stand-ins are
//!   kept beside what was sent, so a resumed chat keeps them.
//! - What was sent is kept ([`RelayLog`]): the outgoing body (stand-ins,
//!   not details: what left the Mac) in a file under the bot's context
//!   folder, readable only by this Mac's user; and for Settings →
//!   Activity, one [`AiExchange`] per request: its sizes, cost and how
//!   many personal details of each kind were swapped (never the details).

// Product names (OpenRouter, Codex) read better in docs without backticks.
#![allow(clippy::doc_markdown)]

pub mod personal;
pub mod shape;
pub mod tap;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use oxbot_engine::{CallKind, CallRecord, ModelChoice, Privacy};
use oxbot_llm::{ErrorKind, LlmError, OpenAiClient, RawAnswer};
use oxbot_loop::gateway::{Asker, Gateway, GatewayStop, Ticket};
use oxbot_loop::meter::now_ms;
use oxbot_loop::pacer::Lane;
use oxbot_loop::router::TurnRouting;
use personal::{Known, Redactor, SseRestorer, StandIns};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

/// Biggest request body taken (a long conversation with pages in it).
const MAX_BODY: usize = 32 * 1024 * 1024;
/// The engine name the meter records.
const ENGINE: &str = "codex";

/// Where a relayed request goes: OpenRouter's Responses API.
pub trait Upstream: Send + Sync + fmt::Debug {
    /// Sends `body`; the answer as it streams, or the service's error.
    fn send(
        &self,
        body: Vec<u8>,
        stream: bool,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RawAnswer, LlmError>>;
}

impl Upstream for OpenAiClient {
    fn send(
        &self,
        body: Vec<u8>,
        stream: bool,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RawAnswer, LlmError>> {
        let client = self.clone();
        Box::pin(async move { client.post_responses(body, stream, &cancel).await })
    }
}

/// One request as the relay sent it (its body is in `body_path`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayLogRow {
    /// When it went out, ms since the Unix epoch.
    pub ts_ms: i64,
    /// The bot.
    pub bot: String,
    /// Its session.
    pub session: Option<String>,
    /// The turn.
    pub turn: Option<u32>,
    /// The model asked.
    pub model: String,
    /// Bytes sent.
    pub bytes: u64,
    /// The file holding what was sent.
    pub body_path: PathBuf,
}

/// One request to the AI as Settings → Activity shows it: sizes, cost,
/// tokens and what was swapped. Never what was said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiExchange {
    /// When it went out, ms since the Unix epoch.
    pub ts_ms: i64,
    /// The bot.
    pub bot: String,
    /// The person the turn acts for, by id.
    pub person: Option<String>,
    /// The model that answered (else the one asked last).
    pub model: String,
    /// What it cost, in US dollars.
    pub cost_usd: f64,
    /// Input tokens.
    pub tokens_in: u64,
    /// Output tokens.
    pub tokens_out: u64,
    /// How long it took, from the request to the end of the answer.
    pub ms: u64,
    /// The meter's outcome (`ok`, `rate_limited`, `error:<kind>`,
    /// `cancelled`…), or oxbot's own words when no model took it.
    pub outcome: String,
    /// Bytes sent: every try's body as it left (after the stand-ins).
    pub bytes_up: u64,
    /// Bytes of the answer received.
    pub bytes_down: u64,
    /// Personal details swapped for stand-ins, by kind → how many.
    pub swapped: BTreeMap<String, u32>,
}

/// Where the relay notes what it sent.
pub trait RelayLog: Send + Sync {
    /// Notes one request.
    ///
    /// # Errors
    /// It couldn't be saved (the request still goes).
    fn add(&self, row: &RelayLogRow) -> Result<(), String>;

    /// Notes how one request went, once its answer ended (or no model
    /// took it after something was sent). Nothing by default.
    fn exchange(&self, _row: &AiExchange) {}
}

/// Keeps no log (tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLog;

impl RelayLog for NoLog {
    fn add(&self, _: &RelayLogRow) -> Result<(), String> {
        Ok(())
    }
}

/// The names oxbot knows (people, the home area), for swapping them for
/// stand-ins. Read for every request, so a new person counts at once.
pub trait KnownNames: Send + Sync {
    /// Everyone and every place oxbot was told about.
    fn known_names(&self) -> Vec<Known>;
}

/// Knows no names (tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoNames;

impl KnownNames for NoNames {
    fn known_names(&self) -> Vec<Known> {
        Vec::new()
    }
}

/// What the relay runs with.
pub struct RelaySetup {
    /// The loop's routing, limits, pacer and meter.
    pub gateway: Gateway,
    /// OpenRouter (a stand-in in tests).
    pub upstream: Arc<dyn Upstream>,
    /// Where what was sent is noted.
    pub log: Arc<dyn RelayLog>,
    /// The names to swap for stand-ins.
    pub names: Arc<dyn KnownNames>,
}

/// One Codex process's bot, as oxbot knows it.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// The bot.
    pub bot: String,
    /// The bot's context folder (the relay keeps what it sent under
    /// `relay/`).
    pub context_dir: PathBuf,
    /// Private bots ask for providers that keep no data.
    pub privacy: Privacy,
    /// The bot's model.
    pub choice: ModelChoice,
    /// A Herder (its own model setting).
    pub central: bool,
}

/// The bot's current turn, as oxbot records it.
#[derive(Debug, Clone)]
pub struct TurnStart {
    /// The bot's session.
    pub session: String,
    /// The turn's number in it.
    pub turn: u32,
    /// The person the turn acts for (per-person limits, the meter).
    pub person: Option<String>,
    /// The pacer queue.
    pub lane: Lane,
    /// The smallest window a model may have (the one Codex was told).
    pub min_window: Option<u64>,
    /// Most model requests the turn may make.
    pub max_requests: u32,
    /// Each request's meter row goes here too (the turn's footer).
    pub meter: Option<mpsc::UnboundedSender<CallRecord>>,
}

/// How a turn's requests went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnSummary {
    /// Requests sent.
    pub requests: u32,
    /// Why the relay stopped a request, in oxbot's words (a limit, no
    /// model left), if it did.
    pub stop: Option<String>,
}

struct Turn {
    start: TurnStart,
    requests: u32,
    routing: TurnRouting,
    stop: Option<String>,
}

/// A chat's stand-ins, and where they're kept.
struct Personal {
    session: String,
    redactor: Redactor,
    path: PathBuf,
}

/// One registered Codex process.
struct Client {
    info: Mutex<ClientInfo>,
    turn: Mutex<Option<Turn>>,
    personal: Mutex<Option<Personal>>,
}

impl Client {
    /// Swaps the personal details in `request` for the chat's stand-ins
    /// (kept on disk when new ones were made); the stand-ins, for the
    /// answer.
    fn redact(
        &self,
        request: &mut serde_json::Map<String, Value>,
        session: &str,
        context_dir: &Path,
        known: Vec<Known>,
    ) -> (Arc<StandIns>, BTreeMap<String, u32>) {
        let mut personal = self.personal.lock().unwrap_or_else(PoisonError::into_inner);
        if personal.as_ref().is_none_or(|p| p.session != session) {
            let path = stand_ins_path(context_dir, session);
            let stand_ins = std::fs::read_to_string(&path)
                .ok()
                .and_then(|json| StandIns::from_json(&json).ok())
                .unwrap_or_default();
            *personal = Some(Personal {
                session: session.to_owned(),
                redactor: Redactor::resume(Vec::new(), stand_ins),
                path,
            });
        }
        let Some(p) = personal.as_mut() else {
            return (Arc::default(), BTreeMap::new());
        };
        p.redactor.set_known(known);
        let before = p.redactor.stand_ins().len();
        p.redactor.redact_request(request);
        let stand_ins = p.redactor.stand_ins();
        if stand_ins.len() != before
            && let Some(dir) = p.path.parent()
            && let Err(err) = write_private(dir, &p.path, stand_ins.to_json().as_bytes())
        {
            tracing::warn!(%err, "couldn't keep a chat's stand-ins");
        }
        (Arc::new(stand_ins.clone()), p.redactor.swapped())
    }

    fn turn(&self) -> std::sync::MutexGuard<'_, Option<Turn>> {
        self.turn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn info(&self) -> ClientInfo {
        self.info
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A Codex process's place at the relay. Dropping it takes its token back.
pub struct RelayClient {
    client: Arc<Client>,
    key: String,
    relay: Weak<Inner>,
}

impl fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayClient")
            .field("bot", &self.client.info().bot)
            .finish_non_exhaustive()
    }
}

impl RelayClient {
    /// The bot's turn started: requests until [`Self::end_turn`] are its.
    pub fn begin_turn(&self, start: TurnStart) {
        let info = self.client.info();
        let routing = TurnRouting {
            choice: info.choice,
            central: info.central,
            min_window: start.min_window,
            ..TurnRouting::default()
        };
        *self.client.turn() = Some(Turn {
            start,
            requests: 0,
            routing,
            stop: None,
        });
    }

    /// The turn ended: no request is taken until the next one begins.
    #[must_use = "the summary says why the relay stopped a request, if it did"]
    pub fn end_turn(&self) -> TurnSummary {
        self.client
            .turn()
            .take()
            .map(|t| TurnSummary {
                requests: t.requests,
                stop: t.stop,
            })
            .unwrap_or_default()
    }

    /// How the current turn's requests are going.
    #[must_use]
    pub fn summary(&self) -> TurnSummary {
        self.client
            .turn()
            .as_ref()
            .map(|t| TurnSummary {
                requests: t.requests,
                stop: t.stop.clone(),
            })
            .unwrap_or_default()
    }

    /// The bot's model or privacy changed (its next turn uses them).
    pub fn update(&self, choice: ModelChoice, privacy: Privacy) {
        let mut info = self
            .client
            .info
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        info.choice = choice;
        info.privacy = privacy;
    }
}

impl Drop for RelayClient {
    fn drop(&mut self) {
        if let Some(relay) = self.relay.upgrade() {
            relay.clients().remove(&self.key);
        }
    }
}

struct Inner {
    setup: RelaySetup,
    /// SHA-256 of each token → its client.
    clients: Mutex<HashMap<String, Arc<Client>>>,
    /// Requests the relay took (any outcome), for tests and Settings.
    seen: AtomicU64,
    /// Numbers the files of what was sent.
    serial: AtomicU64,
}

impl Inner {
    fn clients(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Client>>> {
        self.clients.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Personal details become stand-ins before anything leaves (unless
    /// the owner turned it off), and the answer gets them back: the
    /// stand-ins, and `net` counts what was swapped.
    fn redact(
        &self,
        client: &Client,
        info: &ClientInfo,
        asker: &mut Asker,
        request: &mut serde_json::Map<String, Value>,
        net: &mut NetNote,
    ) -> Option<Arc<StandIns>> {
        if !self.setup.gateway.removes_personal_details() {
            return None;
        }
        asker.filtered = true;
        let (stand_ins, swapped) = client.redact(
            request,
            asker.session.as_deref().unwrap_or_default(),
            &info.context_dir,
            self.setup.names.known_names(),
        );
        net.row.swapped = swapped;
        Some(stand_ins)
    }

    fn client_for(&self, headers: &HeaderMap) -> Option<Arc<Client>> {
        let token = headers
            .get(header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        self.clients().get(&key_of(token)).cloned()
    }
}

/// The relay, running. Stops when dropped.
pub struct Relay {
    inner: Arc<Inner>,
    addr: SocketAddr,
    stop: CancellationToken,
}

impl fmt::Debug for Relay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Relay")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Relay {
    /// Starts the relay on 127.0.0.1 on a free port.
    ///
    /// # Errors
    /// No port could be had.
    pub async fn start(setup: RelaySetup) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let inner = Arc::new(Inner {
            setup,
            clients: Mutex::new(HashMap::new()),
            seen: AtomicU64::new(0),
            serial: AtomicU64::new(0),
        });
        let app = Router::new()
            .route("/v1/responses", post(responses))
            .fallback(|| async { error(StatusCode::NOT_FOUND, "Not found") })
            .layer(DefaultBodyLimit::max(MAX_BODY))
            .with_state(Arc::clone(&inner));
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        tokio::spawn(async move {
            let serve = axum::serve(listener, app)
                .with_graceful_shutdown(async move { stopped.cancelled().await });
            if let Err(err) = serve.await {
                tracing::warn!(%err, "the model relay stopped");
            }
        });
        tracing::info!(%addr, "the model relay is running");
        Ok(Self { inner, addr, stop })
    }

    /// Its address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The base URL a Codex process is given: `http://127.0.0.1:<port>/v1`.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Requests taken so far (any outcome).
    #[must_use]
    pub fn requests_seen(&self) -> u64 {
        self.inner.seen.load(Ordering::Relaxed)
    }

    /// Registers one Codex process for `info`'s bot: its place here, and
    /// the token to put in its environment (never on a command line,
    /// never logged).
    ///
    /// # Errors
    /// No randomness for the token.
    pub fn register(&self, info: ClientInfo) -> Result<(RelayClient, Zeroizing<String>), String> {
        let token = Zeroizing::new(new_token()?);
        let key = key_of(&token);
        let client = Arc::new(Client {
            info: Mutex::new(info),
            turn: Mutex::new(None),
            personal: Mutex::new(None),
        });
        self.inner
            .clients()
            .insert(key.clone(), Arc::clone(&client));
        Ok((
            RelayClient {
                client,
                key,
                relay: Arc::downgrade(&self.inner),
            },
            token,
        ))
    }
}

/// A token: 32 random bytes in hex.
fn new_token() -> Result<String, String> {
    use ring::rand::SecureRandom as _;
    let mut buf = [0u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| "no randomness for the relay token".to_owned())?;
    Ok(hex(&buf))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// What a token is kept as: its SHA-256 (so lookups don't compare the
/// secret itself, and it never sits in the map).
fn key_of(token: &str) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, token.as_bytes()).as_ref())
}

/// An error in the Responses API's shape, which Codex shows as it is.
fn error(status: StatusCode, message: &str) -> Response {
    (
        status,
        axum::Json(json!({"error": {"message": message, "code": "oxbot"}})),
    )
        .into_response()
}

/// A streamed answer that only says the conversation is too long, so
/// Codex compacts it.
fn too_long(message: &str) -> Response {
    let event = json!({
        "type": "response.failed",
        "response": {"status": "failed", "error": {"code": "context_length_exceeded", "message": message}}
    });
    let body = format!("event: response.failed\ndata: {event}\n\n");
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from(body),
    )
        .into_response()
}

/// What a request is for, from oxbot's record of the bot's turn.
fn asker_of(info: &ClientInfo, turn: &Turn) -> Asker {
    Asker {
        bot: info.bot.clone(),
        person: turn.start.person.clone(),
        session: Some(turn.start.session.clone()),
        turn: Some(turn.start.turn),
        step: turn.requests,
        privacy: info.privacy,
        filtered: false,
        lane: turn.start.lane,
        engine: ENGINE.to_owned(),
        kind: CallKind::Chat,
    }
}

async fn responses(State(relay): State<Arc<Inner>>, headers: HeaderMap, body: Bytes) -> Response {
    relay.seen.fetch_add(1, Ordering::Relaxed);
    let Some(client) = relay.client_for(&headers) else {
        return error(
            StatusCode::UNAUTHORIZED,
            "Not a Codex process of this oxbot.",
        );
    };
    let Ok(Value::Object(mut request)) = serde_json::from_slice::<Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "Not a Responses API request.");
    };
    let stream = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let info = client.info();
    // The turn as oxbot recorded it: who it's for, and what it may still do.
    let (mut asker, mut routing, meter) = {
        let mut turn = client.turn();
        let Some(t) = turn.as_mut() else {
            return error(
                StatusCode::BAD_REQUEST,
                "No task is running for this bot right now.",
            );
        };
        if t.requests >= t.start.max_requests {
            let why = format!(
                "This task took {} steps without finishing, so it stopped. Send what's left as a \
                 new, smaller request.",
                t.start.max_requests
            );
            t.stop = Some(why.clone());
            return error(StatusCode::BAD_REQUEST, &why);
        }
        let asker = asker_of(&info, t);
        t.requests += 1;
        (asker, t.routing.clone(), t.start.meter.clone())
    };
    let mut net = NetNote::new(&relay, &info, &asker);
    let stand_ins = relay.redact(&client, &info, &mut asker, &mut request, &mut net);
    // Fires when Codex goes away (the answer it was reading is dropped):
    // the request upstream stops too.
    let cancel = CancellationToken::new();
    let stop_with_codex = cancel.clone().drop_guard();
    let upstream = Arc::clone(&relay.setup.upstream);
    let opened = relay
        .setup
        .gateway
        .request(&asker, &mut routing, &cancel, |attempt| {
            let out = shape::outgoing(&request, &attempt);
            let bytes = serde_json::to_vec(&Value::Object(out)).unwrap_or_default();
            note_sent(&relay, &info, &asker, &attempt.model, &bytes);
            net.row.bytes_up += bytes.len() as u64;
            net.row.model.clone_from(&attempt.model);
            upstream.send(bytes, stream, cancel.clone())
        })
        .await;
    if let Some(t) = client.turn().as_mut() {
        t.routing = routing;
    }
    if let Err(stop) = &opened {
        net.failed(stop);
    }
    match opened {
        Ok(opened) => {
            let (answer, ticket) = (opened.value, opened.ticket);
            if stream {
                streamed(answer, ticket, meter, stop_with_codex, stand_ins, net)
            } else {
                whole(answer, ticket, meter, stand_ins, net).await
            }
        }
        Err(GatewayStop::Fail(why)) => {
            if let Some(t) = client.turn().as_mut() {
                t.stop = Some(why.clone());
            }
            error(StatusCode::BAD_REQUEST, &why)
        }
        Err(GatewayStop::Cancelled) => error(StatusCode::BAD_REQUEST, "Stopped."),
        Err(GatewayStop::Upstream(err)) if err.kind == ErrorKind::ContextLength && stream => {
            too_long(&err.message)
        }
        Err(GatewayStop::Upstream(err)) => error(
            err.status
                .and_then(|s| StatusCode::from_u16(s).ok())
                .unwrap_or(StatusCode::BAD_REQUEST),
            &err.message,
        ),
    }
}

/// Notes what is about to be sent: the body in a file only this Mac's
/// user may read, and a row.
fn note_sent(relay: &Inner, info: &ClientInfo, asker: &Asker, model: &str, bytes: &[u8]) {
    let ts_ms = now_ms();
    let n = relay.serial.fetch_add(1, Ordering::Relaxed);
    let dir = info.context_dir.join(LOG_DIR);
    let path = dir.join(format!("{ts_ms}-{n}.json"));
    if let Err(err) = write_private(&dir, &path, bytes) {
        tracing::warn!(bot = %info.bot, %err, "couldn't keep what the relay sent");
        return;
    }
    let row = RelayLogRow {
        ts_ms,
        bot: info.bot.clone(),
        session: asker.session.clone(),
        turn: asker.turn,
        model: model.to_owned(),
        bytes: bytes.len() as u64,
        body_path: path,
    };
    if let Err(err) = relay.setup.log.add(&row) {
        tracing::warn!(bot = %info.bot, %err, "couldn't note what the relay sent");
    }
}

/// The folder under a bot's context folder that keeps what was sent.
pub const LOG_DIR: &str = "relay";

/// Where a chat's stand-ins are kept: beside what was sent (never swept
/// with it: the name doesn't start with a time).
#[must_use]
pub fn stand_ins_path(context_dir: &Path, session: &str) -> PathBuf {
    let safe: String = session
        .chars()
        .take(80)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    context_dir
        .join(LOG_DIR)
        .join(format!("stand-ins-{safe}.json"))
}

/// Writes `bytes` to `path` (in `dir`, made owner-only), owner-only, all
/// at once (a reader never sees half a file).
fn write_private(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let tmp = path.with_extension("part");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Most bytes of kept request bodies per bot; the oldest go first.
pub const MAX_LOG_BYTES: u64 = 200 * 1024 * 1024;

/// Deletes kept request bodies in `context_dir` older than `cutoff_ms`,
/// then the oldest while they take more than `max_bytes`. Returns how
/// many went.
#[must_use]
pub fn sweep(context_dir: &Path, cutoff_ms: i64, max_bytes: u64) -> usize {
    let Ok(entries) = std::fs::read_dir(context_dir.join(LOG_DIR)) else {
        return 0;
    };
    let mut files: Vec<(i64, u64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let ts = e
                .file_name()
                .to_str()
                .and_then(|n| n.split('-').next())
                .and_then(|t| t.parse::<i64>().ok())?;
            let bytes = e.metadata().map_or(0, |m| m.len());
            Some((ts, bytes, e.path()))
        })
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let mut gone = 0;
    for (ts, bytes, path) in files {
        if (ts < cutoff_ms || total > max_bytes) && std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(bytes);
            gone += 1;
        }
    }
    gone
}

/// A request's row for Settings → Activity, noted once it's known how it
/// went.
struct NetNote {
    log: Arc<dyn RelayLog>,
    row: AiExchange,
}

impl NetNote {
    /// A request starting now for `asker`.
    fn new(relay: &Inner, info: &ClientInfo, asker: &Asker) -> Self {
        Self {
            log: Arc::clone(&relay.setup.log),
            row: AiExchange {
                ts_ms: now_ms(),
                bot: info.bot.clone(),
                person: asker.person.clone(),
                ..AiExchange::default()
            },
        }
    }

    /// No model took the request: noted only if something was sent.
    fn failed(&mut self, stop: &GatewayStop) {
        if self.row.bytes_up == 0 {
            return;
        }
        self.row.outcome = match stop {
            GatewayStop::Cancelled => "cancelled".to_owned(),
            GatewayStop::Fail(why) => why.clone(),
            GatewayStop::Upstream(err) => err.message.clone(),
        };
        self.row.ms = elapsed_ms(self.row.ts_ms);
        self.log.exchange(&self.row);
    }

    /// The answer ended: the meter's row says how.
    fn answered(mut self, record: &CallRecord, bytes_down: u64) {
        if !record.model.is_empty() {
            self.row.model.clone_from(&record.model);
        }
        self.row.cost_usd = record.cost_usd;
        self.row.tokens_in = record.prompt_tokens;
        self.row.tokens_out = record.completion_tokens;
        self.row.outcome.clone_from(&record.outcome);
        self.row.bytes_down = bytes_down;
        self.row.ms = elapsed_ms(self.row.ts_ms);
        self.log.exchange(&self.row);
    }
}

fn elapsed_ms(since: i64) -> u64 {
    u64::try_from(now_ms().saturating_sub(since)).unwrap_or(0)
}

/// Meters an answer when it's dropped: finished, cut off, or abandoned.
struct Metered {
    ticket: Option<Ticket>,
    tap: tap::Tap,
    cut: Option<LlmError>,
    ended: bool,
    meter: Option<mpsc::UnboundedSender<CallRecord>>,
    /// Bytes of the answer so far.
    down: u64,
    /// Its row for Settings → Activity.
    net: Option<NetNote>,
}

impl Drop for Metered {
    fn drop(&mut self) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        let cut = if self.ended {
            self.cut.take()
        } else {
            Some(self.cut.take().unwrap_or_else(LlmError::cancelled))
        };
        let result = self.tap.result(cut);
        let meter = self.meter.take();
        let (net, down) = (self.net.take(), self.down);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let record = ticket.done(result).await;
                if let Some(net) = net {
                    net.answered(&record, down);
                }
                if let Some(meter) = meter {
                    let _ = meter.send(record);
                }
            });
        }
    }
}

/// A streamed answer on its way to Codex.
struct Streaming {
    body: futures_util::stream::BoxStream<'static, Result<Bytes, LlmError>>,
    metered: Metered,
    /// Stand-ins turned back, when the request had them.
    restorer: Option<SseRestorer>,
    _guard: tokio_util::sync::DropGuard,
}

impl Streaming {
    /// The next bytes for Codex (`None`: the answer is over).
    async fn next(&mut self) -> Option<Result<Bytes, std::io::Error>> {
        loop {
            match self.body.next().await {
                Some(Ok(bytes)) => {
                    self.metered.down += bytes.len() as u64;
                    self.metered.tap.push(&bytes);
                    let Some(restorer) = &mut self.restorer else {
                        return Some(Ok(bytes));
                    };
                    let out = restorer.push(&bytes);
                    // Only part of an event yet: wait for the rest.
                    if !out.is_empty() {
                        return Some(Ok(Bytes::from(out)));
                    }
                }
                Some(Err(err)) => {
                    self.metered.cut = Some(err.clone());
                    self.metered.ended = true;
                    return Some(Err(std::io::Error::other(err.message)));
                }
                None => {
                    self.metered.ended = true;
                    let rest = self.restorer.take().map(|mut r| r.finish())?;
                    return (!rest.is_empty()).then(|| Ok(Bytes::from(rest)));
                }
            }
        }
    }
}

/// Passes a streamed answer on as it comes (its stand-ins turned back),
/// metering it at the end.
fn streamed(
    answer: RawAnswer,
    ticket: Ticket,
    meter: Option<mpsc::UnboundedSender<CallRecord>>,
    stop_with_codex: tokio_util::sync::DropGuard,
    stand_ins: Option<Arc<StandIns>>,
    net: NetNote,
) -> Response {
    let state = Streaming {
        body: answer.body,
        metered: Metered {
            ticket: Some(ticket),
            tap: tap::Tap::default(),
            cut: None,
            ended: false,
            meter,
            down: 0,
            net: Some(net),
        },
        restorer: stand_ins.map(SseRestorer::new),
        _guard: stop_with_codex,
    };
    // Dropped unfinished (Codex went away): `Metered` counts it as
    // stopped, and the guard stops the request upstream.
    let body = futures_util::stream::unfold(Some(state), |state| async move {
        let mut state = state?;
        match state.next().await? {
            Ok(bytes) => Some((Ok(bytes), Some(state))),
            Err(err) => Some((Err(err), None)),
        }
    });
    (
        [(header::CONTENT_TYPE, answer.content_type)],
        Body::from_stream(body),
    )
        .into_response()
}

/// Reads a whole (not streamed) answer, meters it and passes it on (its
/// stand-ins turned back).
async fn whole(
    answer: RawAnswer,
    ticket: Ticket,
    meter: Option<mpsc::UnboundedSender<CallRecord>>,
    stand_ins: Option<Arc<StandIns>>,
    net: NetNote,
) -> Response {
    let mut metered = Metered {
        ticket: Some(ticket),
        tap: tap::Tap::default(),
        cut: None,
        ended: false,
        meter,
        down: 0,
        net: Some(net),
    };
    let mut body = answer.body;
    let mut all = Vec::new();
    while let Some(next) = body.next().await {
        match next {
            Ok(bytes) if all.len() + bytes.len() <= MAX_BODY => {
                metered.down += bytes.len() as u64;
                all.extend_from_slice(&bytes);
            }
            Ok(_) => {
                metered.cut = Some(LlmError::new(
                    ErrorKind::Protocol,
                    "The model's answer was too big.",
                ));
                break;
            }
            Err(err) => {
                metered.cut = Some(err);
                break;
            }
        }
    }
    metered.ended = true;
    if let Some(err) = metered.cut.clone() {
        return error(StatusCode::BAD_GATEWAY, &err.message);
    }
    metered.tap.whole(&all);
    if let Some(stand_ins) = stand_ins {
        all = stand_ins.restore_body(&all);
    }
    (
        [(header::CONTENT_TYPE, answer.content_type)],
        Body::from(all),
    )
        .into_response()
}

#[cfg(test)]
mod tests;
