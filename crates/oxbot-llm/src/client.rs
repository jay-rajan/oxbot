//! The model client: the [`ChatModel`] trait and [`OpenAiClient`], which
//! streams answers from an OpenAI-style chat API.
//!
//! The client talks only to its [`Endpoint`]: OpenRouter's fixed HTTPS
//! address, or a server on this Mac's loopback (an e2e stand-in, or later a
//! local model server). It follows no redirects, uses no proxy, sends no
//! attribution headers and no person data, and builds TLS with ring and the
//! macOS trust store.

use std::collections::VecDeque;
use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, RETRY_AFTER};
use reqwest::{StatusCode, Url};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::error::{ErrorKind, LlmError, parse_retry_after};
use crate::secret::KeyCell;
use crate::sse::{SseEvent, SseParser};
use crate::types::{ChatDelta, ChatRequest, ChatResponse, Provider, ResponseBuilder};
use crate::wire::{ChunkReader, request_body};

/// Something that answers chat requests, streamed.
pub trait ChatModel: Send + Sync + fmt::Debug {
    /// Which kind of service this is.
    fn provider(&self) -> Provider;

    /// Sends a request. The stream yields text and tool-call pieces, then
    /// [`ChatDelta::Done`]; or ends with one error. Cancelling the token
    /// (or dropping the stream) stops the request.
    fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatDelta, LlmError>>;
}

/// Sends a request and waits for the whole answer, calling `on_delta` for
/// each piece as it arrives (e.g. to show text live).
///
/// # Errors
/// The model's error, or a protocol error if the stream ended without
/// [`ChatDelta::Done`].
pub async fn complete(
    model: &dyn ChatModel,
    req: ChatRequest,
    cancel: CancellationToken,
    mut on_delta: impl FnMut(&ChatDelta),
) -> Result<ChatResponse, LlmError> {
    let mut stream = model.stream(req, cancel);
    let mut builder = ResponseBuilder::new();
    while let Some(delta) = stream.next().await {
        let delta = delta?;
        on_delta(&delta);
        builder.push(delta);
    }
    builder.finish().ok_or_else(|| {
        LlmError::new(
            ErrorKind::Protocol,
            "The model's answer stopped before it was complete.",
        )
    })
}

/// Where a client sends requests.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    base: Url,
    provider: Provider,
    loopback: bool,
}

/// OpenRouter's API base.
pub const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";

impl Endpoint {
    /// OpenRouter: `https://openrouter.ai/api/v1`.
    ///
    /// # Panics
    /// Never: the address is a valid constant (checked by a test).
    #[must_use]
    pub fn openrouter() -> Self {
        #[allow(clippy::expect_used)]
        let base = Url::parse(OPENROUTER_BASE).expect("constant URL");
        Self {
            base,
            provider: Provider::OpenRouter,
            loopback: false,
        }
    }

    /// A server on this Mac: `http(s)://127.0.0.1…`, `[::1]` or `localhost`
    /// with any port and path, e.g. `http://127.0.0.1:11434/v1`. For e2e
    /// stand-ins (`Provider::OpenRouter`) and local model servers
    /// (`Provider::Local`). Anything else is refused.
    ///
    /// # Errors
    /// Not a URL, not http(s), not loopback, or has a user, query or
    /// fragment.
    pub fn loopback(base: &str, provider: Provider) -> Result<Self, LlmError> {
        let refuse = |why: &str| {
            LlmError::new(
                ErrorKind::Config,
                format!("Model server address refused: {why}."),
            )
        };
        let base = Url::parse(base.trim_end_matches('/')).map_err(|_| refuse("not a URL"))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(refuse("only http and https"));
        }
        if !base.username().is_empty() || base.password().is_some() {
            return Err(refuse("no user names or passwords in the address"));
        }
        if base.query().is_some() || base.fragment().is_some() {
            return Err(refuse("no query or fragment"));
        }
        let loopback = match base.host() {
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
            Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
            None => false,
        };
        if !loopback {
            return Err(refuse("only this Mac (127.0.0.1, ::1 or localhost)"));
        }
        Ok(Self {
            base,
            provider,
            loopback: true,
        })
    }

    /// Which kind of service it is.
    #[must_use]
    pub fn provider(&self) -> Provider {
        self.provider
    }

    /// Whether this is a server on this Mac.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.loopback
    }

    /// The API base, e.g. `https://openrouter.ai/api/v1`.
    #[must_use]
    pub fn base(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    /// An API address: the base plus `path` (e.g. `chat/completions`).
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base(), path.trim_start_matches('/'))
    }

    /// A page on the service's site rather than its API: the base without
    /// a trailing `/api/v1`, plus `path` (e.g. `auth` for sign-in).
    #[must_use]
    pub fn site_url(&self, path: &str) -> String {
        let base = self.base();
        let site = base.strip_suffix("/api/v1").unwrap_or(base);
        format!("{}/{}", site, path.trim_start_matches('/'))
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Endpoint({:?}, {})", self.provider, self.base())
    }
}

/// How the client proves who it is.
#[derive(Clone, Debug)]
pub enum Auth {
    /// No key (local servers).
    None,
    /// A bearer key from this cell, read per request.
    Key(KeyCell),
}

/// How long the client waits. Defaults: connect 10 s, first byte 60 s,
/// 90 s between chunks, 5 minutes per request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// To connect.
    pub connect: Duration,
    /// From sending the request to the answer's headers.
    pub first_byte: Duration,
    /// Between two pieces of the answer (keep-alive comments count).
    pub between_chunks: Duration,
    /// For the whole request.
    pub total: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            first_byte: Duration::from_secs(60),
            between_chunks: Duration::from_secs(90),
            total: Duration::from_secs(300),
        }
    }
}

/// A client's settings.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Where requests go.
    pub endpoint: Endpoint,
    /// The key, if any.
    pub auth: Auth,
    /// How long to wait.
    pub timeouts: Timeouts,
}

impl ClientConfig {
    /// OpenRouter with the key from `key`.
    #[must_use]
    pub fn openrouter(key: KeyCell) -> Self {
        Self {
            endpoint: Endpoint::openrouter(),
            auth: Auth::Key(key),
            timeouts: Timeouts::default(),
        }
    }
}

/// Biggest error body read (the rest is ignored).
const MAX_ERROR_BODY: usize = 64 * 1024;
/// Biggest JSON body read (the model list is a few MB).
pub(crate) const MAX_JSON_BODY: usize = 32 * 1024 * 1024;

/// An OpenAI-style chat client (OpenRouter, or a loopback server).
#[derive(Clone)]
pub struct OpenAiClient {
    http: reqwest::Client,
    config: Arc<ClientConfig>,
}

impl fmt::Debug for OpenAiClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiClient")
            .field("endpoint", &self.config.endpoint)
            .field("auth", &self.config.auth)
            .finish_non_exhaustive()
    }
}

impl OpenAiClient {
    /// A client with these settings.
    ///
    /// # Errors
    /// TLS or the HTTP client couldn't be set up.
    pub fn new(config: ClientConfig) -> Result<Self, LlmError> {
        let setup = |err: &dyn fmt::Display| {
            LlmError::new(
                ErrorKind::Config,
                format!("The model client couldn't be set up: {err}"),
            )
        };
        let tls = tls_config().map_err(|e| setup(&e))?;
        let https_only = !config.endpoint.is_loopback();
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .https_only(https_only)
            .connect_timeout(config.timeouts.connect)
            .build()
            .map_err(|e| setup(&e.without_url()))?;
        Ok(Self {
            http,
            config: Arc::new(config),
        })
    }

    /// The client's endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.config.endpoint
    }

    /// GETs JSON from `path` under the API base, with the key if `with_key`.
    pub(crate) async fn get_json(
        &self,
        path: &str,
        with_key: bool,
    ) -> Result<Zeroizing<Vec<u8>>, LlmError> {
        let mut req = self
            .http
            .get(self.config.endpoint.url(path))
            .header(ACCEPT, "application/json");
        if with_key {
            req = req.header(reqwest::header::AUTHORIZATION, self.bearer()?);
        }
        self.send_json(req).await
    }

    /// POSTs a JSON body to `path` under the API base, without the key.
    pub(crate) async fn post_json(
        &self,
        path: &str,
        body: Vec<u8>,
    ) -> Result<Zeroizing<Vec<u8>>, LlmError> {
        let req = self
            .http
            .post(self.config.endpoint.url(path))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body);
        self.send_json(req).await
    }

    async fn send_json(
        &self,
        req: reqwest::RequestBuilder,
    ) -> Result<Zeroizing<Vec<u8>>, LlmError> {
        let t = self.config.timeouts;
        let response = tokio::time::timeout(t.first_byte, req.timeout(t.total).send())
            .await
            .map_err(|_| timed_out(false))?
            .map_err(|e| from_reqwest(&e, false))?;
        let status = response.status();
        let retry_after = retry_after(response.headers());
        let limit = if status.is_success() {
            MAX_JSON_BODY
        } else {
            MAX_ERROR_BODY
        };
        let body = read_body(response, limit, t.between_chunks).await?;
        if !status.is_success() {
            return Err(LlmError::from_response(
                status.as_u16(),
                retry_after,
                &String::from_utf8_lossy(&body),
            ));
        }
        Ok(body)
    }

    fn bearer(&self) -> Result<reqwest::header::HeaderValue, LlmError> {
        let not_signed_in = || {
            LlmError::new(
                ErrorKind::NotSignedIn,
                "OpenRouter isn't signed in on this Mac.",
            )
        };
        match &self.config.auth {
            Auth::Key(cell) if cell.is_loading() => Err(LlmError::new(
                ErrorKind::NotSignedIn,
                crate::secret::KEYCHAIN_WAITING,
            )),
            Auth::Key(cell) => cell
                .get()
                .and_then(|k| k.bearer())
                .ok_or_else(not_signed_in),
            Auth::None => Err(not_signed_in()),
        }
    }

    async fn open(
        &self,
        req: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<Reading, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::cancelled());
        }
        let provider = self.config.endpoint.provider();
        let body = serde_json::to_vec(&request_body(req, provider)).map_err(|e| {
            LlmError::new(
                ErrorKind::Protocol,
                format!("The request couldn't be written: {e}"),
            )
        })?;
        let t = self.config.timeouts;
        let mut builder = self
            .http
            .post(self.config.endpoint.url("chat/completions"))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "text/event-stream")
            .timeout(t.total)
            .body(body);
        if let Auth::Key(_) = &self.config.auth {
            builder = builder.header(reqwest::header::AUTHORIZATION, self.bearer()?);
        }
        let deadline = Instant::now() + t.total;
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(LlmError::cancelled()),
            sent = tokio::time::timeout(t.first_byte, builder.send()) => {
                sent.map_err(|_| timed_out(false))?.map_err(|e| from_reqwest(&e, false))?
            }
        };
        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after(response.headers());
            let body = tokio::select! {
                () = cancel.cancelled() => return Err(LlmError::cancelled()),
                body = read_body(response, MAX_ERROR_BODY, t.between_chunks) => body?,
            };
            return Err(error_response(status, retry_after, &body));
        }
        let json = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim_start().starts_with("application/json"));
        Ok(Reading {
            body: response.bytes_stream().boxed(),
            sse: SseParser::new(),
            chunks: ChunkReader::new(),
            queue: VecDeque::new(),
            json: json.then(Vec::new),
            cancel: cancel.clone(),
            between_chunks: t.between_chunks,
            deadline,
            ended: false,
            failed: None,
        })
    }
}

/// An answer the Responses API began: its type and its body as it comes.
pub struct RawAnswer {
    /// Its `Content-Type`.
    pub content_type: String,
    /// The body, chunk by chunk (stops at the client's timeouts).
    pub body: BoxStream<'static, Result<Bytes, LlmError>>,
}

impl fmt::Debug for RawAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawAnswer")
            .field("content_type", &self.content_type)
            .finish_non_exhaustive()
    }
}

/// The only User-Agent oxbot sends with a relayed request.
pub const RELAY_USER_AGENT: &str = "oxbot";

impl OpenAiClient {
    /// POSTs `body` (a Responses API request, already checked by the
    /// caller) to `responses` under the API base with the key, and gives
    /// back the answer as it streams. Only the key, the content type and
    /// a fixed User-Agent go with it: no other header.
    ///
    /// # Errors
    /// Not signed in, the service refused the request (its error, with
    /// `Retry-After`), or it couldn't be reached.
    pub async fn post_responses(
        &self,
        body: Vec<u8>,
        stream: bool,
        cancel: &CancellationToken,
    ) -> Result<RawAnswer, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::cancelled());
        }
        let t = self.config.timeouts;
        let accept = if stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        let mut builder = self
            .http
            .post(self.config.endpoint.url("responses"))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, accept)
            .header(reqwest::header::USER_AGENT, RELAY_USER_AGENT)
            .timeout(t.total)
            .body(body);
        if let Auth::Key(_) = &self.config.auth {
            builder = builder.header(reqwest::header::AUTHORIZATION, self.bearer()?);
        }
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(LlmError::cancelled()),
            sent = tokio::time::timeout(t.first_byte, builder.send()) => {
                sent.map_err(|_| timed_out(false))?.map_err(|e| from_reqwest(&e, false))?
            }
        };
        let status = response.status();
        if !status.is_success() {
            let retry_after = retry_after(response.headers());
            let body = tokio::select! {
                () = cancel.cancelled() => return Err(LlmError::cancelled()),
                body = read_body(response, MAX_ERROR_BODY, t.between_chunks) => body?,
            };
            return Err(error_response(status, retry_after, &body));
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let between = t.between_chunks;
        let deadline = Instant::now() + t.total;
        let body = response.bytes_stream();
        let chunks = stream::unfold(Some(body), move |body| async move {
            let mut body = body?;
            let wait = between.min(deadline.saturating_duration_since(Instant::now()));
            match tokio::time::timeout(wait, body.next()).await {
                Err(_) => Some((Err(timed_out(true)), None)),
                Ok(Some(Ok(bytes))) => Some((Ok(bytes), Some(body))),
                Ok(Some(Err(err))) => Some((Err(from_reqwest(&err, true)), None)),
                Ok(None) => None,
            }
        })
        .boxed();
        Ok(RawAnswer {
            content_type,
            body: chunks,
        })
    }
}

impl ChatModel for OpenAiClient {
    fn provider(&self) -> Provider {
        self.config.endpoint.provider()
    }

    fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatDelta, LlmError>> {
        let client = self.clone();
        let state = State::Start {
            client,
            req: Box::new(req),
            cancel,
        };
        stream::unfold(state, |state| async move {
            match state {
                State::Start {
                    client,
                    req,
                    cancel,
                } => match client.open(&req, &cancel).await {
                    Ok(mut reading) => {
                        let item = next_item(&mut reading).await?;
                        let next = State::after(item.is_ok(), reading);
                        Some((item, next))
                    }
                    Err(err) => Some((Err(err), State::Finished)),
                },
                State::Reading(mut reading) => {
                    let item = next_item(&mut reading).await?;
                    let next = State::after(item.is_ok(), *reading);
                    Some((item, next))
                }
                State::Finished => None,
            }
        })
        .boxed()
    }
}

enum State {
    Start {
        client: OpenAiClient,
        req: Box<ChatRequest>,
        cancel: CancellationToken,
    },
    Reading(Box<Reading>),
    Finished,
}

impl State {
    /// The stream ends after an error.
    fn after(ok: bool, reading: Reading) -> Self {
        if ok {
            Self::Reading(Box::new(reading))
        } else {
            Self::Finished
        }
    }
}

struct Reading {
    body: BoxStream<'static, reqwest::Result<Bytes>>,
    sse: SseParser,
    chunks: ChunkReader,
    queue: VecDeque<ChatDelta>,
    /// `Some` when the server answered with plain JSON instead of a stream.
    json: Option<Vec<u8>>,
    cancel: CancellationToken,
    between_chunks: Duration,
    deadline: Instant,
    ended: bool,
    /// An error to report once the queued pieces are out.
    failed: Option<LlmError>,
}

/// The next piece of the answer, or `None` when it is complete.
async fn next_item(r: &mut Reading) -> Option<Result<ChatDelta, LlmError>> {
    loop {
        if let Some(delta) = r.queue.pop_front() {
            return Some(Ok(delta));
        }
        if r.ended {
            return None;
        }
        if let Some(err) = r.failed.take() {
            r.ended = true;
            return Some(Err(err));
        }
        if let Err(err) = read_more(r).await {
            // Pieces that arrived before the error still go out first, so
            // the result doesn't depend on how the network split them.
            r.failed = Some(err);
        }
    }
}

/// Reads one chunk from the network and queues what it completes.
async fn read_more(r: &mut Reading) -> Result<(), LlmError> {
    let partial = r.chunks.has_content();
    let wait = r
        .between_chunks
        .min(r.deadline.saturating_duration_since(Instant::now()));
    let next = tokio::select! {
        () = r.cancel.cancelled() => return Err(LlmError::cancelled().partial(partial)),
        next = tokio::time::timeout(wait, r.body.next()) => next.map_err(|_| timed_out(partial))?,
    };
    match next {
        Some(Ok(bytes)) => {
            if let Some(buf) = &mut r.json {
                if buf.len() + bytes.len() > MAX_JSON_BODY {
                    return Err(LlmError::new(
                        ErrorKind::Protocol,
                        "The model's answer was too big.",
                    ));
                }
                buf.extend_from_slice(&bytes);
                return Ok(());
            }
            let events = r.sse.push(&bytes).map_err(|e| protocol(&e, partial))?;
            queue_events(r, events)
        }
        Some(Err(err)) => Err(from_reqwest(&err, partial)),
        None => end_of_body(r),
    }
}

fn queue_events(r: &mut Reading, events: Vec<SseEvent>) -> Result<(), LlmError> {
    for event in events {
        match event {
            SseEvent::Comment => {}
            SseEvent::Data(data) => {
                let deltas = r.chunks.chunk(&data)?;
                r.queue.extend(deltas);
            }
            SseEvent::Done => return finish(r),
        }
    }
    Ok(())
}

fn end_of_body(r: &mut Reading) -> Result<(), LlmError> {
    if let Some(buf) = r.json.take() {
        let deltas = r.chunks.whole(&String::from_utf8_lossy(&buf))?;
        r.queue.extend(deltas);
        return finish(r);
    }
    let last = r
        .sse
        .finish()
        .map_err(|e| protocol(&e, r.chunks.has_content()))?;
    if let Some(event) = last {
        queue_events(r, vec![event])?;
        if r.ended {
            return Ok(());
        }
    }
    finish(r)
}

/// Queues the closing piece. An answer without a finish reason was cut off.
fn finish(r: &mut Reading) -> Result<(), LlmError> {
    let Some(done) = r.chunks.done() else {
        return Err(LlmError::new(
            ErrorKind::Protocol,
            "The model's answer stopped before it was complete.",
        )
        .partial(r.chunks.has_content()));
    };
    r.queue.push_back(done);
    r.ended = true;
    Ok(())
}

fn error_response(status: StatusCode, retry_after: Option<Duration>, body: &[u8]) -> LlmError {
    LlmError::from_response(status.as_u16(), retry_after, &String::from_utf8_lossy(body))
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after)
}

/// Reads a body up to `limit` bytes (the rest is dropped), waiting at most
/// `between` for each piece. Wiped from memory when dropped: it may hold a
/// key (sign-in answer).
async fn read_body(
    response: reqwest::Response,
    limit: usize,
    between: Duration,
) -> Result<Zeroizing<Vec<u8>>, LlmError> {
    let mut body = Zeroizing::new(Vec::new());
    let mut stream = response.bytes_stream();
    loop {
        let next = tokio::time::timeout(between, stream.next())
            .await
            .map_err(|_| timed_out(false))?;
        match next {
            Some(Ok(bytes)) => {
                let room = limit.saturating_sub(body.len());
                body.extend_from_slice(&bytes[..bytes.len().min(room)]);
                if body.len() >= limit {
                    return Ok(body);
                }
            }
            Some(Err(err)) => return Err(from_reqwest(&err, false)),
            None => return Ok(body),
        }
    }
}

fn timed_out(partial: bool) -> LlmError {
    LlmError::new(
        ErrorKind::Timeout,
        "The model service took too long to answer.",
    )
    .partial(partial)
}

fn protocol(err: &dyn fmt::Display, partial: bool) -> LlmError {
    LlmError::new(
        ErrorKind::Protocol,
        format!("The model service sent a broken stream: {err}"),
    )
    .partial(partial)
}

fn from_reqwest(err: &reqwest::Error, partial: bool) -> LlmError {
    if err.is_timeout() {
        return timed_out(partial);
    }
    let kind = if err.is_builder() {
        ErrorKind::Config
    } else {
        ErrorKind::Network
    };
    // The address is in the error already; keep only the cause chain.
    let mut message = String::from("Couldn't reach the model service");
    let mut source = std::error::Error::source(err);
    if let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
        if let Some(root) = source {
            message.push_str(": ");
            message.push_str(&root.to_string());
        }
    }
    message.push('.');
    LlmError::new(kind, message).partial(partial)
}

/// TLS 1.2+ with ring and the system trust store (managed and corporate
/// roots work), offering HTTP/2 and HTTP/1.1.
fn tls_config() -> Result<rustls::ClientConfig, rustls::Error> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}
