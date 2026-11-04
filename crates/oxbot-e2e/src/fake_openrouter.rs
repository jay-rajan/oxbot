//! A stand-in for `OpenRouter` in end-to-end tests: the parts of its API
//! oxbot uses, answering from the reply scripts (`e2e/scripts/`) so tests
//! are free, fast and give the same result every time. Shapes
//! follow `OpenRouter`'s (`OpenAI`-style) API docs.
//!
//! Under `/api/v1` (oxbot's `OXBOT_OPENROUTER_URL`):
//! - `GET /models`: five models; two pass oxbot's free filter
//!   (`e2e/tool-model:free`, `e2e/backup:free`), two don't (`e2e/small:free`
//!   has 8k context, `e2e/no-tools:free` no tools), and `e2e/paid` costs.
//! - `POST /chat/completions` (SSE when `stream`): picks the reply for the
//!   last user message and acts it out one tool call per request (see
//!   [`crate::script::plan`]); the number of tool results after that message
//!   says which call is next, so replies are stateless and a resumed turn
//!   gets the same next step. When the calls are done it answers in text:
//!   `denied` (or "I didn't: …") after `[oxbot said no]`, "That didn't
//!   work: …" after `[tool error]`, else `after` with `{result}`. Models in
//!   the reply's `busy` answer 429 (`Retry-After: 1`); the first model not
//!   busy from `model` then `models` answers (`OpenRouter`'s own fallback).
//!   Models in the reply's `data_policy` have no endpoint the account's
//!   privacy settings allow (404, as `OpenRouter` words it). With
//!   `provider.require_parameters`, a model is skipped when the request
//!   sends a parameter its list doesn't have (`e2e/backup:free` takes no
//!   `tool_choice` or `parallel_tool_calls`); none left is a 404.
//!   A reply's `stall_at` makes its first request after that many tool
//!   results wait a minute (for restart tests). The next call is counted
//!   from the stand-in's own call ids (`call_e2e_<n>_<step>`), so a turn
//!   whose older steps were summarized away goes on where it was; its
//!   `notes` are said with each call. The usage it reports counts the
//!   request at a token per 3 bytes.
//!   oxbot's helper requests are known by their fixed prompts: a summary
//!   (`COMPACT_V2`) gets a task state as JSON, a page question
//!   (`ASK_PAGE_V1`) the script's `ask_answer`; the log says which
//!   (`"helper"`). oxbot's own `[oxbot] …` notes in a user's role are not
//!   the person's message, and the rules line oxbot adds to the end of
//!   each request is taken off before reading it; when oxbot sends an
//!   answer back to be checked, the reply's `checked` (or the same answer)
//!   is given, and the log says `"checked": true`.
//!   Free requests count against a daily limit (`OXBOT_FAKE_OR_DAILY`,
//!   default 1000); past it, 429 "free-models-per-day".
//! - `POST /responses` (`OpenAI`'s Responses API, which oxbot's relay sends
//!   for Codex bots): the same replies as `/chat/completions`, read from
//!   the request turned into chat messages, answered as Responses events.
//!   Tool calls go by the request's own tool names (Codex's short ones).
//!   The log also lists the request's header names (never values), so a
//!   test can check what the relay stripped.
//! - `GET /key`: the key's limits, with the free daily count.
//! - `POST /auth/keys`: the PKCE exchange; gives the canary key.
//!
//! And the sign-in page at `/auth` (as `https://openrouter.ai/auth`): an
//! **Authorize oxbot** button sends the browser back to `callback_url` with
//! a code and the `state` (a second button sends a wrong `state`).
//!
//! The key is a canary (`sk-or-v1-e2e-<32 hex>`): chat and key calls must
//! carry it as the bearer (else 401), and the runner checks it never lands
//! in oxbot's files, logs or page. Each request is logged as a JSON line to
//! `OXBOT_FAKE_OR_LOG` (model, `provider` prefs, tool names, whether the
//! bearer matched, status): never a header, a code or the key.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::script::{Script, Usage, plan, unwrap_data_block, user_text};

/// The models it offers: id, context length, tools, price per token
/// (prompt, completion).
const MODELS: &[(&str, &str, u32, bool, f64, f64)] = &[
    (
        "e2e/tool-model:free",
        "E2E Tool Model (free)",
        131_072,
        true,
        0.0,
        0.0,
    ),
    (
        "e2e/backup:free",
        "E2E Backup (free)",
        65_536,
        true,
        0.0,
        0.0,
    ),
    ("e2e/small:free", "E2E Small (free)", 8_192, true, 0.0, 0.0),
    (
        "e2e/no-tools:free",
        "E2E No Tools (free)",
        131_072,
        false,
        0.0,
        0.0,
    ),
    ("e2e/paid", "E2E Paid", 200_000, true, 0.000_001, 0.000_005),
];

/// Usage reported when the reply doesn't say (the prompt is counted from
/// the request).
const DEFAULT_USAGE: Usage = Usage {
    prompt: 1000,
    completion: 50,
    cost: 0.0,
};

/// How oxbot's summary prompt starts (`COMPACT_V2`).
const COMPACT_PROMPT: &str = "You are compressing an agent's working memory";
/// How oxbot's page-question prompt starts (`ASK_PAGE_V1`).
const ASK_PAGE_PROMPT: &str = "You answer one question about one web page";
/// How the rules line oxbot adds to the end of each request starts.
const TAIL: &str = "\n\n[oxbot] Answer only from what you read.";
/// How oxbot's own notes in a user's role start.
const NOTE: &str = "[oxbot] ";
/// How oxbot's note sending an answer back to be checked starts.
const CHECK_NOTE: &str = "[oxbot] Before you answer, check it.";

/// The request without the rules line at the end of its last message.
fn without_tail(mut req: Value) -> Value {
    let cut = |s: &mut String| {
        if let Some(at) = s.find(TAIL) {
            s.truncate(at);
        }
    };
    for m in req["messages"].as_array_mut().into_iter().flatten() {
        match &mut m["content"] {
            Value::String(s) => cut(s),
            Value::Array(parts) => {
                for p in parts {
                    if let Some(Value::String(s)) = p.get_mut("text") {
                        cut(s);
                    }
                }
            }
            _ => {}
        }
    }
    req
}

/// Whether `m` is one of oxbot's own notes in a user's role.
fn is_note(m: &Value) -> bool {
    m["role"] == "user" && content_text(&m["content"]).starts_with(NOTE)
}

/// How the stand-in is set up.
#[derive(Debug, Clone)]
pub struct Config {
    /// The reply script.
    pub script: Script,
    /// Free requests allowed per day.
    pub daily: u32,
    /// Where requests are logged (JSON lines).
    pub log: Option<PathBuf>,
    /// The key it hands out and expects.
    pub canary: String,
    /// Context lengths that replace a model's own, e.g. for tests of a
    /// small window.
    pub contexts: HashMap<String, u32>,
    /// Where whole request bodies are recorded (JSON lines), for tests
    /// that compare what was sent.
    pub bodies: Option<PathBuf>,
}

impl Config {
    /// From the environment: `OXBOT_FAKE_SCRIPT`, `OXBOT_FAKE_OR_DAILY`,
    /// `OXBOT_FAKE_OR_LOG`, `OXBOT_FAKE_OR_CONTEXT` (`model=tokens,…`),
    /// `OXBOT_FAKE_OR_BODIES` (a file); a new canary.
    ///
    /// # Errors
    /// Fails if no random key can be made.
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            script: Script::from_env(),
            daily: std::env::var("OXBOT_FAKE_OR_DAILY")
                .ok()
                .and_then(|d| d.parse().ok())
                .unwrap_or(1000),
            log: std::env::var_os("OXBOT_FAKE_OR_LOG").map(Into::into),
            canary: new_canary()?,
            contexts: parse_contexts(&std::env::var("OXBOT_FAKE_OR_CONTEXT").unwrap_or_default()),
            bodies: std::env::var_os("OXBOT_FAKE_OR_BODIES").map(Into::into),
        })
    }
}

/// `model=tokens,model=tokens` → a map (bad pairs are skipped).
#[must_use]
pub fn parse_contexts(text: &str) -> HashMap<String, u32> {
    text.split(',')
        .filter_map(|pair| {
            let (model, n) = pair.split_once('=')?;
            Some((model.trim().to_owned(), n.trim().parse().ok()?))
        })
        .collect()
}

/// A new canary key, `sk-or-v1-e2e-` and 32 random hex digits.
///
/// # Errors
/// Fails if the system has no randomness.
pub fn new_canary() -> anyhow::Result<String> {
    Ok(format!("sk-or-v1-e2e-{}", random_hex(16)?))
}

fn random_hex(bytes: usize) -> anyhow::Result<String> {
    use ring::rand::SecureRandom as _;
    let mut buf = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(buf.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// A sign-in waiting for its code to be exchanged.
#[derive(Debug, Clone)]
struct Pending {
    challenge: String,
    callback: String,
    state: String,
}

/// The stand-in's state.
#[derive(Debug)]
pub struct Fake {
    config: Config,
    used_today: AtomicU32,
    generations: AtomicU64,
    pending: Mutex<HashMap<String, Pending>>,
    log: Mutex<()>,
    counts: Mutex<Counts>,
    stalled: Mutex<std::collections::HashSet<String>>,
}

/// How long a stalled request waits.
const STALL: Duration = Duration::from_secs(60);

/// How many chat requests it got: all, answered, and answered per model
/// (`GET /count`).
#[derive(Debug, Default, serde::Serialize)]
struct Counts {
    chat: u64,
    ok: u64,
    by_model: std::collections::BTreeMap<String, u64>,
}

impl Fake {
    fn new(config: Config) -> Self {
        Self {
            config,
            used_today: AtomicU32::new(0),
            generations: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            log: Mutex::new(()),
            counts: Mutex::new(Counts::default()),
            stalled: Mutex::new(std::collections::HashSet::new()),
        }
    }
}

/// The stand-in's routes, on a fresh state.
pub fn router(config: Config) -> Router {
    let fake = Arc::new(Fake::new(config));
    Router::new()
        .route("/api/v1/models", get(models))
        .route("/api/v1/key", get(key))
        .route("/api/v1/chat/completions", post(chat))
        .route("/api/v1/responses", post(responses))
        .route("/api/v1/auth/keys", post(auth_keys))
        .route("/count", get(count))
        .route("/auth", get(auth_page))
        .route("/auth/approve", get(auth_approve))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "Not found", None, Value::Null) })
        .with_state(fake)
}

/// Whether the request carried the canary as its bearer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bearer {
    Ok,
    Missing,
    Wrong,
}

impl Bearer {
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Wrong => "wrong",
        }
    }
}

impl Fake {
    fn bearer(&self, headers: &HeaderMap) -> Bearer {
        let Some(value) = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
        else {
            return Bearer::Missing;
        };
        match value.strip_prefix("Bearer ") {
            Some(k) if same(k.as_bytes(), self.config.canary.as_bytes()) => Bearer::Ok,
            _ => Bearer::Wrong,
        }
    }

    fn record_body(&self, req: &Value) {
        let Some(path) = &self.config.bodies else {
            return;
        };
        let _guard = self.log.lock();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{req}");
        }
    }

    fn log(&self, mut entry: Value) {
        let Some(path) = &self.config.log else { return };
        entry["at"] = json!(crate::now_ms());
        let _guard = self.log.lock();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{entry}");
        }
    }
}

/// Compares secrets without leaking where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// An `OpenRouter`-shaped error.
fn error(status: StatusCode, message: &str, retry_after: Option<u64>, metadata: Value) -> Response {
    let mut body = json!({ "error": { "code": status.as_u16(), "message": message } });
    if !metadata.is_null() {
        body["error"]["metadata"] = metadata;
    }
    let mut response = (status, axum::Json(body)).into_response();
    if let Some(secs) = retry_after {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(secs));
    }
    response
}

/// An error a request ended with.
#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    retry_after: Option<u64>,
    metadata: Value,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
            metadata: Value::Null,
        }
    }

    fn into_response(self) -> Response {
        error(self.status, &self.message, self.retry_after, self.metadata)
    }
}

/// The catalog, as `GET /models` gives it.
#[must_use]
pub fn catalog<S: std::hash::BuildHasher>(contexts: &HashMap<String, u32, S>) -> Value {
    let data: Vec<Value> = MODELS
        .iter()
        .map(|&(id, name, context, tools, prompt, completion)| {
            let context = contexts.get(id).copied().unwrap_or(context);
            let params = params_of(id, tools);
            json!({
                "id": id,
                "canonical_slug": id.trim_end_matches(":free"),
                "name": name,
                "created": 1_760_000_000,
                "description": "A stand-in model for oxbot's end-to-end tests.",
                "context_length": context,
                "architecture": {
                    "modality": "text->text",
                    "input_modalities": ["text"],
                    "output_modalities": ["text"],
                    "tokenizer": "Other",
                    "instruct_type": null
                },
                "pricing": {
                    "prompt": price(prompt),
                    "completion": price(completion),
                    "request": "0",
                    "image": "0",
                    "web_search": "0",
                    "internal_reasoning": "0"
                },
                "top_provider": {
                    "context_length": context,
                    "max_completion_tokens": 8192,
                    "is_moderated": false
                },
                "per_request_limits": null,
                "supported_parameters": params
            })
        })
        .collect();
    json!({ "data": data })
}

/// The request parameters a model takes. The backup, like several real
/// free models, takes tools but not `tool_choice` or
/// `parallel_tool_calls`.
fn params_of(id: &str, tools: bool) -> Vec<&'static str> {
    let mut params = vec!["max_tokens", "temperature", "top_p", "stop"];
    if tools {
        params.push("tools");
        if id != "e2e/backup:free" {
            params.extend(["tool_choice", "parallel_tool_calls"]);
        }
    }
    params
}

/// Optional parameters a request may carry that `require_parameters`
/// checks.
const CHECKED_PARAMS: &[&str] = &[
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "max_tokens",
    "temperature",
    "top_p",
    "stop",
    "seed",
];

/// What `OpenRouter` says when the account's privacy settings rule out
/// every endpoint.
pub const DATA_POLICY_MESSAGE: &str = "No endpoints found matching your data policy (Free model \
                                       training). Configure: https://openrouter.ai/settings/privacy";

/// What `OpenRouter` says when no endpoint takes every parameter sent.
pub const PARAMETERS_MESSAGE: &str = "No endpoints found that can handle the requested parameters. \
                                      To learn more about provider routing, visit: \
                                      https://openrouter.ai/docs/guides/routing/provider-selection";

/// The candidates an endpoint could serve: not ruled out by the account's
/// data policy, and (with `require_parameters`) taking every parameter
/// sent.
fn servable(
    req: &Value,
    candidates: &[String],
    data_policy: &[String],
) -> Result<Vec<String>, ApiError> {
    let allowed: Vec<String> = candidates
        .iter()
        .filter(|m| !data_policy.contains(m))
        .cloned()
        .collect();
    if allowed.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, DATA_POLICY_MESSAGE));
    }
    if req["provider"]["require_parameters"] != true {
        return Ok(allowed);
    }
    let sent: Vec<&str> = CHECKED_PARAMS
        .iter()
        .copied()
        .filter(|p| req.get(*p).is_some_and(|v| !v.is_null()))
        .collect();
    let taking: Vec<String> = allowed
        .into_iter()
        .filter(|m| {
            let tools = MODELS.iter().find(|x| x.0 == m).is_some_and(|x| x.3);
            let params = params_of(m, tools);
            sent.iter().all(|p| params.contains(p))
        })
        .collect();
    if taking.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, PARAMETERS_MESSAGE));
    }
    Ok(taking)
}

/// A price per token as `OpenRouter` writes it (a string).
fn price(usd: f64) -> String {
    if usd == 0.0 {
        "0".to_owned()
    } else {
        format!("{usd:.9}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

fn model_info(id: &str) -> Option<(f64, f64)> {
    MODELS
        .iter()
        .find(|m| m.0 == id)
        .map(|&(_, _, _, _, p, c)| (p, c))
}

async fn models(State(fake): State<Arc<Fake>>) -> Response {
    fake.log(json!({ "path": "/models", "status": 200 }));
    axum::Json(catalog(&fake.config.contexts)).into_response()
}

async fn count(State(fake): State<Arc<Fake>>) -> Response {
    match fake.counts.lock() {
        Ok(c) => axum::Json(json!(*c)).into_response(),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "no count",
            None,
            Value::Null,
        ),
    }
}

async fn key(State(fake): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    let bearer = fake.bearer(&headers);
    if bearer != Bearer::Ok {
        fake.log(json!({ "path": "/key", "status": 401, "bearer": bearer.name() }));
        return unauthorized(bearer);
    }
    fake.log(json!({ "path": "/key", "status": 200, "bearer": "ok" }));
    let limit = fake.config.daily;
    let used = fake.used_today.load(Ordering::SeqCst).min(limit);
    axum::Json(json!({
        "data": {
            "label": "sk-or-v1-e2e…",
            "limit": null,
            "limit_reset": null,
            "limit_remaining": null,
            "include_byok_in_limit": false,
            "usage": 0,
            "usage_daily": 0,
            "usage_weekly": 0,
            "usage_monthly": 0,
            "byok_usage": 0,
            "is_free_tier": limit < 1000,
            "is_provisioning_key": false,
            "rate_limit": { "requests": -1, "interval": "10s" },
            "free_model_daily_requests": {
                "used": used,
                "limit": limit,
                "remaining": limit - used
            }
        }
    }))
    .into_response()
}

fn unauthorized(bearer: Bearer) -> Response {
    let message = match bearer {
        Bearer::Missing => "No auth credentials found",
        _ => "User not found.",
    };
    error(StatusCode::UNAUTHORIZED, message, None, Value::Null)
}

/// What one chat request answers.
#[derive(Debug)]
struct Answer {
    model: String,
    text: Option<String>,
    call: Option<(String, String, String)>,
    usage: Usage,
    delay_ms: u64,
    when: Option<String>,
    step: usize,
    stall: bool,
    /// One of oxbot's helper requests: `compact` or `ask_page`.
    helper: Option<&'static str>,
    /// It answers oxbot's note sending an answer back to be checked.
    checked: bool,
}

async fn chat(State(fake): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    let bearer = fake.bearer(&headers);
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let tools: Vec<&str> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    let mut entry = json!({
        "path": "/chat/completions",
        "bearer": bearer.name(),
        "model": req["model"],
        "models": req["models"],
        "stream": req["stream"].as_bool().unwrap_or(false),
        "provider": req["provider"],
        "tools": tools,
        "tool_choice": req["tool_choice"],
        "parallel_tool_calls": req["parallel_tool_calls"],
        "messages": req["messages"].as_array().map_or(0, Vec::len),
    });
    if let Ok(mut c) = fake.counts.lock() {
        c.chat += 1;
    }
    if bearer != Bearer::Ok {
        entry["status"] = json!(401);
        fake.log(entry);
        return unauthorized(bearer);
    }
    fake.record_body(&req);
    let req = without_tail(req);
    let answer = match fake.answer(&req, body.len()) {
        Ok(a) => a,
        Err(err) => {
            entry["status"] = json!(err.status.as_u16());
            entry["error"] = json!(err.message);
            fake.log(entry);
            return err.into_response();
        }
    };
    entry["status"] = json!(200);
    entry["routed"] = json!(answer.model);
    entry["when"] = json!(answer.when);
    entry["step"] = json!(answer.step);
    entry["called"] = json!(answer.call.as_ref().map(|c| &c.1));
    entry["stalled"] = json!(answer.stall);
    entry["helper"] = json!(answer.helper);
    entry["checked"] = json!(answer.checked);
    entry["tokens"] = json!(body.len() / 4);
    fake.log(entry);
    if let Ok(mut c) = fake.counts.lock() {
        c.ok += 1;
        *c.by_model.entry(answer.model.clone()).or_default() += 1;
    }
    if answer.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(answer.delay_ms)).await;
    }
    if answer.stall {
        tokio::time::sleep(STALL).await;
    }
    let id = format!(
        "gen-e2e-{}",
        fake.generations.fetch_add(1, Ordering::SeqCst) + 1
    );
    if req["stream"].as_bool().unwrap_or(false) {
        stream_response(&id, &answer)
    } else {
        axum::Json(whole_response(&id, &answer)).into_response()
    }
}

/// The models a request asks for, in order: `model`, then `models`.
fn candidates(req: &Value) -> Result<Vec<String>, ApiError> {
    let mut candidates: Vec<String> = Vec::new();
    for m in req["model"].as_str().into_iter().chain(
        req["models"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str),
    ) {
        if !candidates.iter().any(|c| c == m) {
            candidates.push(m.to_owned());
        }
    }
    if candidates.is_empty() {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "No models provided"));
    }
    if let Some(unknown) = candidates.iter().find(|m| model_info(m).is_none()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("{unknown} is not a valid model ID"),
        ));
    }
    Ok(candidates)
}

impl Fake {
    /// The first candidate not busy, with its prices; a free one counts
    /// against the daily limit.
    fn route(
        &self,
        candidates: &[String],
        busy: &[String],
        tokens: usize,
    ) -> Result<(String, f64, f64), ApiError> {
        let Some(model) = candidates.iter().find(|m| !busy.contains(m)).cloned() else {
            let first = candidates.first().map_or("", String::as_str);
            return Err(ApiError {
                status: StatusCode::TOO_MANY_REQUESTS,
                message: "Provider returned error".into(),
                retry_after: Some(1),
                metadata: json!({
                    "raw": format!("{first} is temporarily rate-limited upstream. Please retry shortly."),
                    "provider_name": "E2E"
                }),
            });
        };
        let context = self
            .config
            .contexts
            .get(&model)
            .copied()
            .or_else(|| MODELS.iter().find(|m| m.0 == model).map(|m| m.2))
            .unwrap_or(u32::MAX);
        if tokens > context as usize {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                message: format!(
                    "This endpoint's maximum context length is {context} tokens. However, you \
                     requested about {tokens} tokens. Please reduce the length of either one, \
                     or use the \"middle-out\" transform to compress your prompt automatically."
                ),
                retry_after: None,
                metadata: json!({ "provider_name": "E2E" }),
            });
        }
        let (prompt, completion) = model_info(&model).unwrap_or((0.0, 0.0));
        if prompt == 0.0 && completion == 0.0 {
            let limit = self.config.daily;
            let counted = self
                .used_today
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    (n < limit).then_some(n + 1)
                });
            if counted.is_err() {
                return Err(ApiError {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    message: "Rate limit exceeded: free-models-per-day. Add 10 credits to unlock \
                              1000 free model requests per day"
                        .to_owned(),
                    retry_after: None,
                    metadata: json!({ "headers": {
                        "X-RateLimit-Limit": limit.to_string(),
                        "X-RateLimit-Remaining": "0",
                    }}),
                });
            }
        }
        Ok((model, prompt, completion))
    }

    /// Whether this is the first request for `text` at `step` to stall.
    fn first_stall(&self, text: &str, step: usize) -> bool {
        self.stalled
            .lock()
            .is_ok_and(|mut seen| seen.insert(format!("{step}\u{0}{text}")))
    }

    fn answer(&self, req: &Value, body_len: usize) -> Result<Answer, ApiError> {
        let bad = |m: String| ApiError::new(StatusCode::BAD_REQUEST, m);
        let messages = req["messages"]
            .as_array()
            .filter(|m| !m.is_empty())
            .ok_or_else(|| bad("messages: an array of at least one message is required".into()))?;
        check_messages(messages).map_err(bad)?;
        let tools = req["tools"].as_array().cloned().unwrap_or_default();
        for t in &tools {
            if t["type"] != "function" || t["function"]["name"].as_str().is_none() {
                return Err(bad(format!(
                    "tools: each tool needs type \"function\" and a name: {t}"
                )));
            }
        }
        let candidates = candidates(req)?;
        let system = messages
            .first()
            .filter(|m| m["role"] == "system")
            .map(|m| content_text(&m["content"]))
            .unwrap_or_default();
        if system.starts_with(COMPACT_PROMPT) || system.starts_with(ASK_PAGE_PROMPT) {
            return self.helper(req, &candidates, &system, body_len);
        }
        let last_user = messages
            .iter()
            .rposition(|m| m["role"] == "user" && !is_note(m));
        let checking = messages
            .last()
            .is_some_and(|m| is_note(m) && content_text(&m["content"]).starts_with(CHECK_NOTE));
        let text = last_user
            .map(|i| user_text(&messages[i]["content"]))
            .unwrap_or_default();
        let reply = self.config.script.pick(&text);
        let (model, prompt_price, completion_price) = self.route(
            &servable(req, &candidates, &reply.data_policy)?,
            &reply.busy,
            body_len / 4,
        )?;

        let after_user = last_user.map_or(&messages[..], |i| &messages[i + 1..]);
        let results: Vec<String> = after_user
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| content_text(&m["content"]))
            .collect();
        let step = next_step(after_user, results.len());
        let needs_page = |name: &str| {
            let own = name.rsplit("__").next().unwrap_or(name);
            tools.iter().any(|t| {
                (t["function"]["name"] == name || t["function"]["name"] == own)
                    && t["function"]["parameters"]["required"]
                        .as_array()
                        .is_some_and(|r| r.iter().any(|k| k == "pageId"))
            })
        };
        let calls = plan(&reply, &text, needs_page);
        let usage = reply
            .usage
            .unwrap_or_else(|| counted(body_len, prompt_price, completion_price));
        let mut answer = Answer {
            model,
            text: None,
            call: None,
            usage,
            delay_ms: if step == 0 { reply.delay_ms } else { 0 },
            when: reply.when.clone(),
            step,
            stall: reply.stall_at == Some(step) && self.first_stall(&text, step),
            helper: None,
            checked: checking,
        };
        let last = results.last().map_or("", String::as_str);
        let refused = last.strip_prefix("[oxbot said no]");
        let broke = last.strip_prefix("[tool error]");
        if let (Some(call), None, None) = (calls.get(step), refused, broke) {
            let user_index = last_user.unwrap_or(0);
            answer.call = Some((
                format!("call_e2e_{user_index}_{step}"),
                call.name.clone(),
                call.args.to_string(),
            ));
            if step == 0 && !reply.read {
                answer.text.clone_from(&reply.say);
            }
            if let Some(n) = reply.notes {
                answer.text = Some(notes(step, n));
            }
            return Ok(answer);
        }
        answer.text = Some(
            if let Some(checked) = reply.checked.clone().filter(|_| checking) {
                checked
            } else {
                final_text(&reply, &text, calls.is_empty(), last)
            },
        );
        Ok(answer)
    }
}

/// What a reply says when its calls are done: what the files hold (with
/// `read`), its words (no calls), what it couldn't do after a refusal or a
/// failed call (the last result `last`), else `after`.
fn final_text(reply: &crate::script::Reply, text: &str, no_calls: bool, last: &str) -> String {
    let refused = last.strip_prefix("[oxbot said no]");
    let broke = last.strip_prefix("[tool error]");
    if reply.read {
        crate::script::read_answer(reply.say.as_deref(), &crate::script::attached(text))
    } else if no_calls {
        reply.say.clone().unwrap_or_default()
    } else if let Some(why) = refused {
        reply
            .denied
            .clone()
            .unwrap_or_else(|| format!("I didn't: {}", why.trim()))
    } else if let Some(what) = broke {
        format!("That didn't work: {}", what.trim())
    } else {
        reply
            .after
            .as_deref()
            .unwrap_or("Done.")
            .replace("{result}", &unwrap_data_block(last))
    }
}

/// The next call of a reply's plan: after the last one the stand-in made
/// (its id, `call_e2e_<n>_<step>`, says which), else as many as there are
/// results.
fn next_step(after_user: &[Value], results: usize) -> usize {
    after_user
        .iter()
        .filter(|m| m["role"] == "assistant")
        .flat_map(|m| m["tool_calls"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| {
            c["id"]
                .as_str()?
                .strip_prefix("call_e2e_")?
                .rsplit('_')
                .next()?
                .parse::<usize>()
                .ok()
        })
        .max()
        .map_or(results, |n| n + 1)
}

/// The usage reported for a request of `body_len` bytes: a token per 3
/// bytes in, a short answer out.
fn counted(body_len: usize, prompt_price: f64, completion_price: f64) -> Usage {
    let usage = Usage {
        prompt: body_len as u64 / 3,
        ..DEFAULT_USAGE
    };
    Usage {
        cost: precise_cost(usage, prompt_price, completion_price),
        ..usage
    }
}

/// A chatty model's notes for step `step`, about `n` characters.
fn notes(step: usize, n: usize) -> String {
    let line = format!("Notes for step {step}: looked at what this page lists and kept going. ");
    line.repeat(n / line.len() + 1).chars().take(n).collect()
}

impl Fake {
    /// Answers one of oxbot's helper requests: a summary or a page
    /// question.
    fn helper(
        &self,
        req: &Value,
        candidates: &[String],
        system: &str,
        body_len: usize,
    ) -> Result<Answer, ApiError> {
        let (model, prompt_price, completion_price) =
            self.route(&servable(req, candidates, &[])?, &[], body_len / 4)?;
        let input = req["messages"]
            .as_array()
            .and_then(|m| m.get(1))
            .map(|m| content_text(&m["content"]))
            .unwrap_or_default();
        let (helper, text) = if system.starts_with(COMPACT_PROMPT) {
            let steps =
                input.matches("\n[step ").count() + usize::from(input.starts_with("[step "));
            let summary = json!({
                "done": [{"step": steps, "what": format!("looked at the pages of the first {steps} steps")}],
                "open": ["finish the task"],
                "handles": [{"id": "r1", "what": "the first page"}],
                "progress_note": "Looked at the pages so far; go on from the last one."
            });
            ("compact", summary.to_string())
        } else {
            let answer = self
                .config
                .script
                .reply
                .iter()
                .find_map(|r| r.ask_answer.clone())
                .unwrap_or_else(|| "not on this page".to_owned());
            ("ask_page", answer)
        };
        Ok(Answer {
            model,
            text: Some(text),
            call: None,
            usage: counted(body_len, prompt_price, completion_price),
            delay_ms: COMPACT_DELAY_MS,
            when: None,
            step: 0,
            stall: false,
            helper: Some(helper),
            checked: false,
        })
    }
}

/// How long a helper request takes, so its activity line can be seen.
const COMPACT_DELAY_MS: u64 = 1_500;

/// The cost of `usage` at a model's prices.
fn precise_cost(usage: Usage, prompt: f64, completion: f64) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "token counts are small")]
    let cost = usage.prompt as f64 * prompt + usage.completion as f64 * completion;
    cost
}

/// A message's text: a string, or its text parts joined.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Checks messages as providers do: known roles, tool calls with string
/// arguments, and each tool result answering an earlier call.
fn check_messages(messages: &[Value]) -> Result<(), String> {
    let mut asked: Vec<&str> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        match m["role"].as_str() {
            Some("system" | "user") => {}
            Some("assistant") => {
                for call in m["tool_calls"].as_array().into_iter().flatten() {
                    let id = call["id"]
                        .as_str()
                        .ok_or_else(|| format!("messages[{i}]: a tool call has no id"))?;
                    if call["type"] != "function"
                        || call["function"]["name"].as_str().is_none()
                        || !call["function"]["arguments"].is_string()
                    {
                        return Err(format!(
                            "messages[{i}]: tool calls need type \"function\", a name and arguments as a JSON string"
                        ));
                    }
                    asked.push(id);
                }
            }
            Some("tool") => {
                let id = m["tool_call_id"].as_str().unwrap_or("");
                if !asked.contains(&id) {
                    return Err(format!(
                        "messages[{i}]: tool result for unknown tool_call_id {id:?}"
                    ));
                }
            }
            other => return Err(format!("messages[{i}]: unknown role {other:?}")),
        }
    }
    Ok(())
}

fn finish(answer: &Answer) -> &'static str {
    if answer.call.is_some() {
        "tool_calls"
    } else {
        "stop"
    }
}

fn usage_json(u: Usage) -> Value {
    json!({
        "prompt_tokens": u.prompt,
        "completion_tokens": u.completion,
        "total_tokens": u.prompt + u.completion,
        "cost": u.cost,
        "is_byok": false,
        "prompt_tokens_details": { "cached_tokens": 0 },
        "completion_tokens_details": { "reasoning_tokens": 0 }
    })
}

fn created() -> i64 {
    crate::now_ms() / 1000
}

fn whole_response(id: &str, answer: &Answer) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": answer.text,
        "refusal": null,
        "reasoning": null,
    });
    if let Some((call_id, name, args)) = &answer.call {
        message["tool_calls"] = json!([{
            "id": call_id,
            "index": 0,
            "type": "function",
            "function": { "name": name, "arguments": args }
        }]);
    }
    json!({
        "id": id,
        "provider": "E2E",
        "model": answer.model,
        "object": "chat.completion",
        "created": created(),
        "choices": [{
            "index": 0,
            "logprobs": null,
            "finish_reason": finish(answer),
            "native_finish_reason": finish(answer),
            "message": message
        }],
        "usage": usage_json(answer.usage)
    })
}

/// Splits `s` into about `n` pieces on character boundaries.
fn pieces(s: &str, n: usize) -> Vec<&str> {
    let size = s.len().div_ceil(n.max(1)).max(1);
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let mut at = size.min(rest.len());
        while !rest.is_char_boundary(at) {
            at += 1;
        }
        let (a, b) = rest.split_at(at);
        out.push(a);
        rest = b;
    }
    out
}

/// The SSE events of a streamed answer, as `OpenRouter` sends them: a
/// processing comment, the role, text and tool-call pieces, the finish,
/// usage, then `[DONE]`.
fn stream_events(id: &str, answer: &Answer) -> Vec<String> {
    let chunk = |delta: Value, finish: Option<&str>| {
        let event = json!({
            "id": id,
            "provider": "E2E",
            "model": answer.model,
            "object": "chat.completion.chunk",
            "created": created(),
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish,
                "native_finish_reason": finish,
                "logprobs": null
            }]
        });
        format!("data: {event}\n\n")
    };
    let mut events = vec![": OPENROUTER PROCESSING\n\n".to_owned()];
    events.push(chunk(json!({ "role": "assistant", "content": "" }), None));
    if let Some(text) = answer.text.as_deref().filter(|t| !t.is_empty()) {
        for piece in pieces(text, 3) {
            events.push(chunk(
                json!({ "role": "assistant", "content": piece }),
                None,
            ));
        }
    }
    if let Some((call_id, name, args)) = &answer.call {
        events.push(chunk(
            json!({ "role": "assistant", "content": null, "tool_calls": [{
                "index": 0, "id": call_id, "type": "function",
                "function": { "name": name, "arguments": "" }
            }]}),
            None,
        ));
        for piece in pieces(args, 2) {
            events.push(chunk(
                json!({ "role": "assistant", "content": null, "tool_calls": [{
                    "index": 0, "function": { "arguments": piece }
                }]}),
                None,
            ));
        }
    }
    events.push(chunk(
        json!({ "role": "assistant", "content": "" }),
        Some(finish(answer)),
    ));
    let mut last = json!({
        "id": id,
        "provider": "E2E",
        "model": answer.model,
        "object": "chat.completion.chunk",
        "created": created(),
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": "" },
            "finish_reason": null,
            "native_finish_reason": null,
            "logprobs": null
        }]
    });
    last["usage"] = usage_json(answer.usage);
    events.push(format!("data: {last}\n\n"));
    events.push("data: [DONE]\n\n".to_owned());
    events
}

fn stream_response(id: &str, answer: &Answer) -> Response {
    // Sent in small pieces with short pauses, one event split in two, so a
    // client's parser meets events across reads.
    let mut parts: Vec<String> = Vec::new();
    for (i, event) in stream_events(id, answer).into_iter().enumerate() {
        if i == 2 {
            let split = pieces(&event, 2);
            parts.extend(split.into_iter().map(str::to_owned));
        } else {
            parts.push(event);
        }
    }
    let stream = futures_util::stream::unfold(parts.into_iter(), |mut rest| async move {
        let next = rest.next()?;
        tokio::time::sleep(Duration::from_millis(3)).await;
        Some((Ok::<_, std::convert::Infallible>(Bytes::from(next)), rest))
    });
    let mut response = Body::from_stream(stream).into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// `base64url` without padding.
#[must_use]
pub fn base64url(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]));
        }
    }
    out
}

/// The PKCE S256 challenge for a verifier.
#[must_use]
pub fn challenge(verifier: &str) -> String {
    base64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Percent-encodes everything but unreserved characters.
fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn page(status: StatusCode, title: &str, body: &str) -> Response {
    (
        status,
        Html(format!(
            "<!doctype html><meta charset=utf-8><title>{title}</title>\
             <body style=\"font:16px system-ui;margin:40px\"><h1>{title}</h1>{body}</body>"
        )),
    )
        .into_response()
}

async fn auth_page(
    State(fake): State<Arc<Fake>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let callback = q.get("callback_url").cloned().unwrap_or_default();
    let loopback = ["http://localhost:", "http://127.0.0.1:"]
        .iter()
        .any(|p| callback.starts_with(p));
    let problem = if !loopback {
        Some("callback_url must be a loopback http address")
    } else if q.get("code_challenge_method").map(String::as_str) != Some("S256") {
        Some("code_challenge_method must be S256")
    } else if q.get("code_challenge").is_none_or(|c| c.len() < 43) {
        Some("code_challenge is missing")
    } else {
        None
    };
    if let Some(problem) = problem {
        fake.log(json!({ "path": "/auth", "status": 400, "error": problem }));
        return page(
            StatusCode::BAD_REQUEST,
            "Can't sign in",
            &html_escape(problem),
        );
    }
    let Ok(code) = random_hex(16) else {
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Can't sign in",
            "no randomness",
        );
    };
    let pending = Pending {
        challenge: q.get("code_challenge").cloned().unwrap_or_default(),
        callback,
        state: q.get("state").cloned().unwrap_or_default(),
    };
    if let Ok(mut map) = fake.pending.lock() {
        map.insert(code.clone(), pending);
    }
    fake.log(json!({ "path": "/auth", "status": 200, "has_state": q.contains_key("state") }));
    let label = html_escape(q.get("key_label").map_or("An app", String::as_str));
    page(
        StatusCode::OK,
        "OpenRouter (stand-in)",
        &format!(
            "<p>{label} would like an API key for your OpenRouter account.</p>\
             <form method=get action=\"/auth/approve\">\
             <input type=hidden name=code value=\"{code}\">\
             <button>Authorize oxbot</button> \
             <button name=wrong_state value=1>Send a wrong state</button></form>"
        ),
    )
}

async fn auth_approve(
    State(fake): State<Arc<Fake>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let code = q.get("code").cloned().unwrap_or_default();
    let Some(pending) = fake.pending.lock().ok().and_then(|m| m.get(&code).cloned()) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Can't sign in",
            "This sign-in has expired.",
        );
    };
    let state = if q.contains_key("wrong_state") {
        format!("{}x", pending.state)
    } else {
        pending.state.clone()
    };
    let sep = if pending.callback.contains('?') {
        '&'
    } else {
        '?'
    };
    let to = format!(
        "{}{sep}code={}&state={}",
        pending.callback,
        pct(&code),
        pct(&state)
    );
    fake.log(json!({ "path": "/auth/approve", "status": 302 }));
    let mut response = StatusCode::FOUND.into_response();
    if let Ok(v) = HeaderValue::from_str(&to) {
        response.headers_mut().insert(header::LOCATION, v);
    }
    response
}

async fn auth_keys(State(fake): State<Arc<Fake>>, body: Bytes) -> Response {
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let code = req["code"].as_str().unwrap_or("");
    let verifier = req["code_verifier"].as_str().unwrap_or("");
    // Single use, as a real code is.
    let pending = fake.pending.lock().ok().and_then(|mut m| m.remove(code));
    let ok = req["code_challenge_method"] == "S256"
        && pending
            .as_ref()
            .is_some_and(|p| same(challenge(verifier).as_bytes(), p.challenge.as_bytes()));
    let status = if ok { 200 } else { 400 };
    fake.log(json!({ "path": "/auth/keys", "status": status }));
    if !ok {
        return error(
            StatusCode::BAD_REQUEST,
            "Invalid code or code_verifier",
            None,
            Value::Null,
        );
    }
    axum::Json(json!({ "key": fake.config.canary, "user_id": "user_e2e" })).into_response()
}

/// A Responses API request as chat messages: its instructions as the
/// system message, each input item as the message it stands for, and its
/// function tools in chat's shape.
#[must_use]
pub fn responses_as_chat(req: &Value) -> Value {
    let mut messages = Vec::new();
    if let Some(sys) = req["instructions"].as_str() {
        messages.push(json!({"role": "system", "content": sys}));
    }
    for item in req["input"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("message") | None => {
                let role = item["role"].as_str().unwrap_or("user");
                let role = if role == "developer" { "system" } else { role };
                let text = match &item["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(parts) => parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join(""),
                    _ => String::new(),
                };
                messages.push(json!({"role": role, "content": text}));
            }
            Some("function_call") => {
                messages.push(
                    json!({"role": "assistant", "content": null, "tool_calls": [{
                        "id": item["call_id"], "type": "function",
                        "function": {"name": item["name"], "arguments": item["arguments"]}
                    }]}),
                );
            }
            Some("function_call_output") => {
                let output = match &item["output"] {
                    Value::String(s) => s.clone(),
                    other => content_text(other),
                };
                messages.push(
                    json!({"role": "tool", "tool_call_id": item["call_id"], "content": output}),
                );
            }
            _ => {}
        }
    }
    let tools: Vec<Value> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "function")
        .map(|t| {
            json!({"type": "function", "function": {
            "name": t["name"], "description": t["description"], "parameters": t["parameters"]}})
        })
        .collect();
    let mut chat = json!({
        "model": req["model"],
        "messages": messages,
        "stream": req["stream"],
        "tools": tools,
    });
    for key in [
        "models",
        "provider",
        "tool_choice",
        "parallel_tool_calls",
        "temperature",
        "top_p",
    ] {
        if let Some(v) = req.get(key) {
            chat[key] = v.clone();
        }
    }
    chat
}

/// The request's tool that a planned call names: the same name, or its
/// own name (`click` for `mcp__chrome-devtools__click`).
fn requested_name(req: &Value, planned: &str) -> String {
    let own = planned.rsplit("__").next().unwrap_or(planned);
    let names: Vec<&str> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .collect();
    if names.contains(&planned) {
        planned.to_owned()
    } else if names.contains(&own) {
        own.to_owned()
    } else {
        planned.to_owned()
    }
}

async fn responses(State(fake): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    let bearer = fake.bearer(&headers);
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut header_names: Vec<String> = headers.keys().map(|k| k.as_str().to_owned()).collect();
    header_names.sort();
    let tools: Vec<&str> = req["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["name"].as_str())
        .collect();
    let mut entry = json!({
        "path": "/responses",
        "bearer": bearer.name(),
        "model": req["model"],
        "models": req["models"],
        "stream": req["stream"].as_bool().unwrap_or(false),
        "provider": req["provider"],
        "tools": tools,
        "headers": header_names,
        "user_agent": headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()),
        "keys": req.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
        "input": req["input"].as_array().map_or(0, Vec::len),
    });
    if let Ok(mut c) = fake.counts.lock() {
        c.chat += 1;
    }
    if bearer != Bearer::Ok {
        entry["status"] = json!(401);
        fake.log(entry);
        return unauthorized(bearer);
    }
    fake.record_body(&req);
    let chat = responses_as_chat(&req);
    let mut answer = match fake.answer(&chat, body.len()) {
        Ok(a) => a,
        Err(err) => {
            entry["status"] = json!(err.status.as_u16());
            entry["error"] = json!(err.message);
            fake.log(entry);
            return err.into_response();
        }
    };
    if let Some(call) = &mut answer.call {
        call.1 = requested_name(&req, &call.1);
    }
    entry["status"] = json!(200);
    entry["routed"] = json!(answer.model);
    entry["when"] = json!(answer.when);
    entry["step"] = json!(answer.step);
    entry["called"] = json!(answer.call.as_ref().map(|c| &c.1));
    entry["checked"] = json!(answer.checked);
    fake.log(entry);
    if let Ok(mut c) = fake.counts.lock() {
        c.ok += 1;
        *c.by_model.entry(answer.model.clone()).or_default() += 1;
    }
    if answer.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(answer.delay_ms)).await;
    }
    if answer.stall {
        tokio::time::sleep(STALL).await;
    }
    let id = format!(
        "gen-e2e-{}",
        fake.generations.fetch_add(1, Ordering::SeqCst) + 1
    );
    let (items, response) = responses_output(&id, &answer);
    if !req["stream"].as_bool().unwrap_or(false) {
        return axum::Json(response).into_response();
    }
    responses_stream(&id, &answer.model, &items, &response)
}

/// A Responses answer as its stream of events.
fn responses_stream(id: &str, model: &str, items: &[Value], response: &Value) -> Response {
    let mut events =
        vec![json!({"type": "response.created", "response": {"id": id, "model": model}})];
    for item in items {
        if item["type"] == "message" {
            let empty =
                json!({"type": "message", "id": item["id"], "role": "assistant", "content": []});
            events.push(json!({"type": "response.output_item.added", "item": empty}));
            let text = item["content"][0]["text"].as_str().unwrap_or("");
            for piece in pieces(text, 3) {
                events.push(json!({"type": "response.output_text.delta", "item_id": item["id"], "delta": piece}));
            }
        } else {
            events.push(json!({"type": "response.output_item.added", "item": item}));
        }
        events.push(json!({"type": "response.output_item.done", "item": item}));
    }
    events.push(json!({"type": "response.completed", "response": response}));
    let parts: Vec<String> = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap_or("")))
        .collect();
    let stream = futures_util::stream::unfold(parts.into_iter(), |mut rest| async move {
        let next = rest.next()?;
        tokio::time::sleep(Duration::from_millis(3)).await;
        Some((Ok::<_, std::convert::Infallible>(Bytes::from(next)), rest))
    });
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

/// An answer as Responses output items, and the whole response object.
fn responses_output(id: &str, answer: &Answer) -> (Vec<Value>, Value) {
    let mut items = Vec::new();
    if let Some(text) = answer.text.as_deref().filter(|t| !t.is_empty()) {
        items.push(json!({"type": "message", "id": format!("msg_{id}"), "role": "assistant",
            "status": "completed", "content": [{"type": "output_text", "text": text, "annotations": []}]}));
    }
    if let Some((call_id, name, args)) = &answer.call {
        items.push(
            json!({"type": "function_call", "id": format!("fc_{call_id}"), "call_id": call_id,
            "name": name, "arguments": args, "status": "completed"}),
        );
    }
    let u = answer.usage;
    let response = json!({
        "id": id,
        "object": "response",
        "status": "completed",
        "model": answer.model,
        "output": items,
        "usage": {
            "input_tokens": u.prompt,
            "output_tokens": u.completion,
            "total_tokens": u.prompt + u.completion,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0},
            "cost": u.cost,
        }
    });
    (items, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636() {
        // RFC 7636, appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
    }

    #[test]
    fn a_stalled_step_stalls_once() {
        let script: Script = toml::from_str(
            "[[reply]]\nwhen = \"slow\"\ntool = \"mcp__chrome-devtools__click\"\nargs = { uid = \"1_2\" }\nstall_at = 0\n",
        )
        .unwrap();
        let fake = Fake::new(Config {
            script,
            daily: 1000,
            log: None,
            canary: new_canary().unwrap(),
            contexts: HashMap::new(),
            bodies: None,
        });
        let req = json!({"model": "e2e/tool-model:free",
                         "messages": [{"role": "user", "content": "[Jay] slow please"}]});
        assert!(
            fake.answer(&req, 100).unwrap().stall,
            "the first request waits"
        );
        assert!(
            !fake.answer(&req, 100).unwrap().stall,
            "the same step again answers"
        );
        let other = json!({"model": "e2e/tool-model:free",
                           "messages": [{"role": "user", "content": "hello"}]});
        assert!(!fake.answer(&other, 100).unwrap().stall);
    }

    #[test]
    fn canaries_are_fresh() {
        let a = new_canary().unwrap();
        assert!(a.starts_with("sk-or-v1-e2e-"));
        assert_eq!(a.len(), "sk-or-v1-e2e-".len() + 32);
        assert_ne!(a, new_canary().unwrap());
    }

    #[test]
    fn the_catalog_has_two_usable_free_models() {
        let cat = catalog(&HashMap::new());
        let small = catalog(&parse_contexts("e2e/tool-model:free=16000, bad"));
        assert_eq!(small["data"][0]["context_length"], 16000);
        let usable: Vec<&str> = cat["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| {
                m["id"].as_str().unwrap().ends_with(":free")
                    && m["pricing"]["prompt"] == "0"
                    && m["pricing"]["completion"] == "0"
                    && m["context_length"].as_u64().unwrap() >= 64_000
                    && m["supported_parameters"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|p| p == "tools")
            })
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(usable, ["e2e/tool-model:free", "e2e/backup:free"]);
        assert_eq!(cat["data"][4]["pricing"]["prompt"], "0.000001");
    }

    #[test]
    fn oxbots_notes_and_rules_line_are_not_the_persons_words() {
        let req = without_tail(json!({"messages": [
            {"role": "user", "content": "find lamps\n\n[oxbot] Answer only from what you read. Check."},
            {"role": "tool", "tool_call_id": "c", "content": [{"type": "text", "text": "page\n\n[oxbot] Answer only from what you read."}]},
            {"role": "user", "content": "[oxbot] Before you answer, check it. The task was: «find lamps»."}
        ]}));
        let m = req["messages"].as_array().unwrap();
        assert_eq!(content_text(&m[0]["content"]), "find lamps");
        assert_eq!(content_text(&m[1]["content"]), "page");
        assert!(!is_note(&m[0]) && is_note(&m[2]));
    }

    #[test]
    fn stream_pieces_rebuild_the_answer() {
        let answer = Answer {
            model: "e2e/tool-model:free".into(),
            text: Some("Héllo there, café".into()),
            call: Some(("c1".into(), "Read".into(), "{\"file_path\":\"/x\"}".into())),
            usage: DEFAULT_USAGE,
            delay_ms: 0,
            when: None,
            step: 0,
            stall: false,
            helper: None,
            checked: false,
        };
        let events = stream_events("gen-1", &answer);
        assert_eq!(events[0], ": OPENROUTER PROCESSING\n\n");
        assert_eq!(events.last().unwrap(), "data: [DONE]\n\n");
        let mut text = String::new();
        let mut args = String::new();
        let mut finish = None;
        let mut usage = None;
        for e in &events {
            let Some(data) = e.strip_prefix("data: ") else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            let c = &v["choices"][0];
            text.push_str(c["delta"]["content"].as_str().unwrap_or(""));
            if let Some(a) = c["delta"]["tool_calls"][0]["function"]["arguments"].as_str() {
                args.push_str(a);
            }
            if let Some(f) = c["finish_reason"].as_str() {
                finish = Some(f.to_owned());
            }
            if v["usage"].is_object() {
                usage = Some(v["usage"]["prompt_tokens"].clone());
            }
        }
        assert_eq!(text, "Héllo there, café");
        assert_eq!(args, "{\"file_path\":\"/x\"}");
        assert_eq!(finish.as_deref(), Some("tool_calls"));
        assert_eq!(usage, Some(json!(1000)));
    }

    #[test]
    fn tool_results_must_answer_a_call() {
        let ok = [
            json!({"role":"user","content":"hi"}),
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"a","type":"function","function":{"name":"Read","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"a","content":"x"}),
        ];
        assert!(check_messages(&ok).is_ok());
        let orphan = [json!({"role":"tool","tool_call_id":"b","content":"x"})];
        assert!(check_messages(&orphan).is_err());
        let object_args = [
            json!({"role":"assistant","tool_calls":[{"id":"a","type":"function","function":{"name":"Read","arguments":{}}}]}),
        ];
        assert!(check_messages(&object_args).is_err());
    }

    #[test]
    fn a_responses_request_reads_as_its_chat_messages() {
        let req = json!({
            "model": "e2e/tool-model:free", "stream": true, "instructions": "rules",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "search jobs"}]},
                {"type": "function_call", "call_id": "call_e2e_1_0", "name": "navigate_page", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_e2e_1_0", "output": "ok"}
            ],
            "tools": [{"type": "function", "name": "navigate_page", "parameters": {"type": "object"}}],
            "provider": {"require_parameters": true}
        });
        let chat = responses_as_chat(&req);
        assert_eq!(
            chat["messages"][0],
            json!({"role": "system", "content": "rules"})
        );
        assert_eq!(chat["messages"][1]["content"], "search jobs");
        assert_eq!(chat["messages"][2]["tool_calls"][0]["id"], "call_e2e_1_0");
        assert_eq!(chat["messages"][3]["tool_call_id"], "call_e2e_1_0");
        assert_eq!(chat["tools"][0]["function"]["name"], "navigate_page");
        assert_eq!(chat["provider"]["require_parameters"], true);
        assert!(check_messages(chat["messages"].as_array().unwrap()).is_ok());
        assert_eq!(
            requested_name(&req, "mcp__chrome-devtools__navigate_page"),
            "navigate_page"
        );
        assert_eq!(requested_name(&req, "save_report"), "save_report");
    }
}
