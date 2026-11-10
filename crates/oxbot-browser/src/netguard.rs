//! The network guard of a practice-run Chrome: oxbot's own `DevTools`
//! connection that lets pages read but never write.
//!
//! Every tab, frame and worker is held when it starts
//! (`waitForDebuggerOnStart`) until the guard has turned on request
//! interception (`Fetch`) in it. Then:
//!
//! - GET, HEAD and OPTIONS go through;
//! - [`NetMode::Strict`]: every other request is failed and remembered
//!   (method, host and path only: no query, no body);
//! - [`NetMode::XhrAllowed`]: a POST sent by a script (`XHR`, `Fetch`) goes
//!   through too; form posts, PUT, PATCH, DELETE and beacons don't;
//! - `WebSocket`s (and the other socket kinds) are off in both modes:
//!   Chrome's interception never sees them, so a script run in every page,
//!   frame and worker before its own replaces their constructors.
//! - Requests no tab owns (beacons sent as a page closes) go through
//!   browser-wide interception, with the same rules.
//!
//! It fails closed: a target runs only once both are on in it; one that
//! can't be armed is closed (and if even that fails, the guard stops being
//! ready). [`NetGuard::ready`] is true only while the connection is up and
//! every tab is armed, and the hub refuses browser tools until then.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// How much a practice run's pages may send.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NetMode {
    /// Only GET, HEAD and OPTIONS.
    #[default]
    Strict,
    /// Also POSTs from scripts (sites that read with POST).
    XhrAllowed,
}

/// A request the guard stopped. No query string or body is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRequest {
    /// Its method, e.g. `POST`.
    pub method: String,
    /// Its host, e.g. `jobs.example.test`.
    pub host: String,
    /// Its path, e.g. `/post`.
    pub path: String,
}

/// Whether a request with `method`, of `resource_type` (`Document`,
/// `XHR`, `Fetch`, `Ping`…, as `DevTools` names it), may go out in `mode`.
#[must_use]
pub fn may_send(method: &str, resource_type: &str, mode: NetMode) -> bool {
    let method = method.to_ascii_uppercase();
    if matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS") {
        return true;
    }
    mode == NetMode::XhrAllowed && method == "POST" && matches!(resource_type, "XHR" | "Fetch")
}

/// What stops `WebSocket`s, which Chrome's request interception and URL
/// blocking never see: run in every page, frame and worker before its own
/// scripts, it replaces the socket constructors with ones that refuse.
pub const NO_SOCKETS: &str = r#"(() => {
  const refuse = function () {
    throw new DOMException("WebSockets are off in an oxbot practice run", "SecurityError");
  };
  for (const name of ["WebSocket", "WebSocketStream", "WebTransport", "RTCPeerConnection", "webkitRTCPeerConnection"]) {
    try { Object.defineProperty(globalThis, name, { value: refuse, writable: false, configurable: false }); } catch (e) {}
  }
})();"#;

/// Target kinds that can make requests, and so get guarded.
const NETWORK_TARGETS: [&str; 6] = [
    "page",
    "iframe",
    "worker",
    "shared_worker",
    "service_worker",
    "webview",
];
/// Target kinds with frames (documents), where the socket block is added
/// for every new document.
const FRAME_TARGETS: [&str; 3] = ["page", "iframe", "webview"];
/// At most this many blocked requests are kept per run.
const MAX_BLOCKED: usize = 500;

/// What the guard knows, shared with the hub.
#[derive(Debug, Default)]
struct Shared {
    /// The `DevTools` connection is up.
    connected: bool,
    /// Browser-wide interception is on (requests no tab owns, such as
    /// beacons).
    browser_armed: bool,
    /// Each tab's `DevTools` session → whether it's guarded.
    pages: HashMap<String, bool>,
    /// Requests it stopped, oldest first.
    blocked: Vec<BlockedRequest>,
    /// How many it stopped in all (`blocked` keeps the first ones).
    blocked_count: usize,
    /// A target that couldn't be armed couldn't be closed either.
    broken: bool,
}

impl Shared {
    fn ready(&self) -> bool {
        self.connected
            && !self.broken
            && self.browser_armed
            && !self.pages.is_empty()
            && self.pages.values().all(|on| *on)
    }
}

/// A running network guard. Dropping it stops it (and with it the
/// browser tools: it is no longer ready).
pub(crate) struct NetGuard {
    shared: Arc<Mutex<Shared>>,
    task: JoinHandle<()>,
}

impl Drop for NetGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl NetGuard {
    /// Connects to the Chrome at `endpoint` (see [`crate::Browser::endpoint`]) and starts guarding every tab.
    ///
    /// # Errors
    /// Fails if Chrome's `DevTools` connection can't be opened.
    pub(crate) async fn start(endpoint: &str, mode: NetMode) -> Result<Self, String> {
        let (socket, _) = tokio_tungstenite::connect_async(endpoint)
            .await
            .map_err(|e| e.to_string())?;
        let shared = Arc::new(Mutex::new(Shared {
            connected: true,
            ..Shared::default()
        }));
        let mut machine = Machine::new(mode, Arc::clone(&shared));
        let task = tokio::spawn(async move {
            let (mut sink, mut stream) = socket.split();
            let mut outbox = machine.start();
            loop {
                for msg in outbox.drain(..) {
                    if sink.send(Message::Text(msg.into())).await.is_err() {
                        machine.disconnected();
                        return;
                    }
                }
                match stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(msg) = serde_json::from_str::<Value>(&text) {
                            outbox = machine.on_message(&msg);
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => {
                        machine.disconnected();
                        return;
                    }
                }
            }
        });
        Ok(Self { shared, task })
    }

    /// Whether it's guarding every tab right now: connected, at least one
    /// tab, and interception on in each.
    pub(crate) fn ready(&self) -> bool {
        lock(&self.shared).ready()
    }

    /// Waits up to `wait` for [`NetGuard::ready`].
    pub(crate) async fn wait_ready(&self, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        loop {
            if self.ready() {
                return true;
            }
            if Instant::now() >= until || self.task.is_finished() {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The requests it stopped so far, and how many in all.
    pub(crate) fn blocked(&self) -> (Vec<BlockedRequest>, usize) {
        let shared = lock(&self.shared);
        (shared.blocked.clone(), shared.blocked_count)
    }
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A call whose answer matters.
#[derive(Debug, Clone)]
enum Pending {
    /// Browser-wide interception.
    BrowserFetch,
    /// Interception in a target's session.
    Fetch { session: String },
    /// The socket block in a target's session.
    Sockets { session: String },
    /// Closing a target that couldn't be armed.
    Close { session: String },
}

/// A target being set up.
#[derive(Debug, Clone, Default)]
struct Arming {
    /// Interception is on.
    fetch: bool,
    /// Sockets are off.
    sockets: bool,
    /// It waits for the guard before it runs.
    waiting: bool,
    /// Its id, to close it.
    target_id: String,
}

/// The guard's side of the connection, without the socket (so it can be
/// tested with made-up `DevTools` messages).
struct Machine {
    mode: NetMode,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    /// Sessions still being set up.
    arming: HashMap<String, Arming>,
    shared: Arc<Mutex<Shared>>,
}

impl Machine {
    fn new(mode: NetMode, shared: Arc<Mutex<Shared>>) -> Self {
        Self {
            mode,
            next_id: 1,
            pending: HashMap::new(),
            arming: HashMap::new(),
            shared,
        }
    }

    fn call(
        &mut self,
        out: &mut Vec<String>,
        method: &str,
        params: &Value,
        session: Option<&str>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            msg["sessionId"] = json!(session);
        }
        out.push(msg.to_string());
        id
    }

    /// The first calls: hold every new target until it's set up, and
    /// intercept requests no tab owns (beacons, say).
    fn start(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        self.call(&mut out, "Target.setAutoAttach", &auto_attach(), None);
        let id = self.call(&mut out, "Fetch.enable", &intercept_all(), None);
        self.pending.insert(id, Pending::BrowserFetch);
        out
    }

    fn disconnected(&self) {
        lock(&self.shared).connected = false;
    }

    fn on_message(&mut self, msg: &Value) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            if let Some(pending) = self.pending.remove(&id) {
                self.answered(&mut out, pending, msg.get("error").is_none());
            }
            return out;
        }
        let params = msg.get("params").unwrap_or(&Value::Null);
        let session = msg.get("sessionId").and_then(Value::as_str);
        match msg.get("method").and_then(Value::as_str) {
            Some("Target.attachedToTarget") => self.attached(&mut out, params),
            Some("Target.detachedFromTarget") => {
                let gone = str_of(params, "sessionId");
                self.arming.remove(gone);
                lock(&self.shared).pages.remove(gone);
            }
            Some("Fetch.requestPaused") => self.paused(&mut out, session, params),
            _ => {}
        }
        out
    }

    /// A new target: turn interception and the socket block on in it,
    /// catch what it starts in turn, then let it run. Anything that can't
    /// make requests just runs.
    fn attached(&mut self, out: &mut Vec<String>, params: &Value) {
        let session = str_of(params, "sessionId").to_owned();
        let info = params.get("targetInfo").unwrap_or(&Value::Null);
        let kind = str_of(info, "type");
        let waiting = params
            .get("waitingForDebugger")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !NETWORK_TARGETS.contains(&kind) {
            // Nothing that can make requests: it just runs.
            if waiting {
                self.run(out, &session);
            }
            return;
        }
        if kind == "page" {
            lock(&self.shared).pages.insert(session.clone(), false);
        }
        self.arming.insert(
            session.clone(),
            Arming {
                waiting,
                target_id: str_of(info, "targetId").to_owned(),
                ..Arming::default()
            },
        );
        let fetch = self.call(out, "Fetch.enable", &intercept_all(), Some(&session));
        self.pending.insert(
            fetch,
            Pending::Fetch {
                session: session.clone(),
            },
        );
        let sockets = if FRAME_TARGETS.contains(&kind) {
            // Every document from now on, and the one there now: this is
            // what keeps sockets off, so it's the call that must work.
            self.call(out, "Page.enable", &json!({}), Some(&session));
            let id = self.call(
                out,
                "Page.addScriptToEvaluateOnNewDocument",
                &json!({ "source": NO_SOCKETS, "runImmediately": true }),
                Some(&session),
            );
            self.call(
                out,
                "Network.setBypassServiceWorker",
                &json!({ "bypass": true }),
                Some(&session),
            );
            id
        } else {
            // A worker has one global scope, not yet running.
            self.call(
                out,
                "Runtime.evaluate",
                &json!({ "expression": NO_SOCKETS }),
                Some(&session),
            )
        };
        self.pending.insert(
            sockets,
            Pending::Sockets {
                session: session.clone(),
            },
        );
        // Frames and workers this one starts are held the same way.
        self.call(out, "Target.setAutoAttach", &auto_attach(), Some(&session));
        // It runs once both answers came back fine (see `answered`).
    }

    fn run(&mut self, out: &mut Vec<String>, session: &str) {
        self.call(
            out,
            "Runtime.runIfWaitingForDebugger",
            &json!({}),
            Some(session),
        );
    }

    fn answered(&mut self, out: &mut Vec<String>, pending: Pending, ok: bool) {
        let (session, interception) = match pending {
            Pending::BrowserFetch => {
                if ok {
                    lock(&self.shared).browser_armed = true;
                } else {
                    tracing::warn!("the practice-run guard couldn't intercept browser requests");
                }
                return;
            }
            Pending::Close { session } => {
                if !ok {
                    // Unguarded and still there: nothing may run now.
                    tracing::warn!(session, "the practice-run guard couldn't close a target");
                    lock(&self.shared).broken = true;
                }
                return;
            }
            Pending::Fetch { session } => (session, true),
            Pending::Sockets { session } => (session, false),
        };
        let Some(state) = self.arming.get_mut(&session) else {
            return;
        };
        if !ok {
            // Held, never run: closed so it can't load or send anything.
            tracing::info!(
                session,
                interception,
                "the practice-run guard couldn't arm a target; closing it"
            );
            let target_id = state.target_id.clone();
            self.arming.remove(&session);
            let id = self.call(
                out,
                "Target.closeTarget",
                &json!({ "targetId": target_id }),
                None,
            );
            self.pending.insert(id, Pending::Close { session });
            return;
        }
        if interception {
            state.fetch = true;
        } else {
            state.sockets = true;
        }
        if state.fetch && state.sockets {
            let waiting = state.waiting;
            self.arming.remove(&session);
            if let Some(armed) = lock(&self.shared).pages.get_mut(&session) {
                *armed = true;
            }
            if waiting {
                self.run(out, &session);
            }
        }
    }

    /// A request is waiting: let it go, or stop it and remember it.
    fn paused(&mut self, out: &mut Vec<String>, session: Option<&str>, params: &Value) {
        let request_id = str_of(params, "requestId").to_owned();
        let request = params.get("request").unwrap_or(&Value::Null);
        let method = str_of(request, "method");
        let resource = str_of(params, "resourceType");
        if may_send(method, resource, self.mode) {
            self.call(
                out,
                "Fetch.continueRequest",
                &json!({ "requestId": request_id }),
                session,
            );
            return;
        }
        self.call(
            out,
            "Fetch.failRequest",
            &json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
            session,
        );
        let (host, path) = host_and_path(str_of(request, "url"));
        tracing::info!(method, host, "practice run: blocked a request");
        let mut shared = lock(&self.shared);
        shared.blocked_count += 1;
        if shared.blocked.len() < MAX_BLOCKED {
            shared.blocked.push(BlockedRequest {
                method: method.to_ascii_uppercase(),
                host,
                path,
            });
        }
    }
}

fn intercept_all() -> Value {
    json!({ "patterns": [{ "urlPattern": "*", "requestStage": "Request" }] })
}

fn auto_attach() -> Value {
    json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true })
}

/// The host and path of `url`, without query or fragment.
fn host_and_path(url: &str) -> (String, String) {
    match url::Url::parse(url) {
        Ok(u) => (u.host_str().unwrap_or("").to_owned(), u.path().to_owned()),
        Err(_) => (String::new(), String::new()),
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(mode: NetMode) -> (Machine, Arc<Mutex<Shared>>) {
        let shared = Arc::new(Mutex::new(Shared {
            connected: true,
            ..Shared::default()
        }));
        let mut m = Machine::new(mode, Arc::clone(&shared));
        for call in parse(&m.start()) {
            m.on_message(&json!({"id": call["id"], "result": {}}));
        }
        (m, shared)
    }

    fn parse(out: &[String]) -> Vec<Value> {
        out.iter()
            .map(|m| serde_json::from_str(m).unwrap())
            .collect()
    }

    fn ready(shared: &Mutex<Shared>) -> bool {
        lock(shared).ready()
    }

    fn attached(session: &str, kind: &str) -> Value {
        json!({
            "method": "Target.attachedToTarget",
            "params": {"sessionId": session, "waitingForDebugger": true,
                "targetInfo": {"type": kind, "targetId": format!("t-{session}")}}
        })
    }

    /// Attaches a target and answers every call it makes with `ok`;
    /// returns those calls, then what the answers led to (not answered).
    fn attach(m: &mut Machine, session: &str, kind: &str, ok: bool) -> Vec<Value> {
        let mut out = parse(&m.on_message(&attached(session, kind)));
        let mut after = Vec::new();
        for call in &out {
            let reply = if ok {
                json!({"id": call["id"], "result": {}})
            } else {
                json!({"id": call["id"], "error": {"message": "no"}})
            };
            after.extend(parse(&m.on_message(&reply)));
        }
        out.extend(after);
        out
    }

    fn methods(calls: &[Value]) -> Vec<&str> {
        calls
            .iter()
            .map(|c| c["method"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn only_reads_go_out_in_strict_mode() {
        for method in ["GET", "head", "OPTIONS"] {
            assert!(may_send(method, "Document", NetMode::Strict), "{method}");
        }
        for (method, kind) in [
            ("POST", "Document"),
            ("POST", "XHR"),
            ("PUT", "Fetch"),
            ("DELETE", "XHR"),
            ("PATCH", "Fetch"),
            ("POST", "Ping"),
        ] {
            assert!(!may_send(method, kind, NetMode::Strict), "{method} {kind}");
        }
    }

    #[test]
    fn script_posts_go_out_only_in_xhr_mode() {
        assert!(may_send("POST", "XHR", NetMode::XhrAllowed));
        assert!(may_send("POST", "Fetch", NetMode::XhrAllowed));
        for (method, kind) in [
            ("POST", "Document"),
            ("POST", "Ping"),
            ("PUT", "XHR"),
            ("DELETE", "Fetch"),
        ] {
            assert!(
                !may_send(method, kind, NetMode::XhrAllowed),
                "{method} {kind}"
            );
        }
    }

    #[test]
    fn a_new_tab_is_held_until_it_is_guarded() {
        let shared = Arc::new(Mutex::new(Shared {
            connected: true,
            ..Shared::default()
        }));
        let mut m = Machine::new(NetMode::Strict, Arc::clone(&shared));
        let start = parse(&m.start());
        assert_eq!(start[0]["method"], "Target.setAutoAttach");
        assert_eq!(start[0]["params"]["waitForDebuggerOnStart"], true);
        assert_eq!(start[1]["method"], "Fetch.enable");
        assert!(start[1].get("sessionId").is_none(), "browser-wide");

        let out = parse(&m.on_message(&attached("s1", "page")));
        let sent = methods(&out);
        assert_eq!(sent.first(), Some(&"Fetch.enable"));
        assert!(
            !sent.contains(&"Runtime.runIfWaitingForDebugger"),
            "held until it's armed"
        );
        assert!(sent.contains(&"Page.addScriptToEvaluateOnNewDocument"));
        assert!(sent.contains(&"Network.setBypassServiceWorker"));
        assert!(out.iter().all(|c| c["sessionId"] == "s1"));
        let mut resumed = Vec::new();
        for call in &out {
            resumed.extend(parse(
                &m.on_message(&json!({"id": call["id"], "result": {}})),
            ));
        }
        assert_eq!(methods(&resumed), ["Runtime.runIfWaitingForDebugger"]);
        assert!(!ready(&shared), "not until browser-wide interception is on");
        m.on_message(&json!({"id": start[1]["id"], "result": {}}));
        assert!(ready(&shared));

        // Another tab: not ready until it's guarded too.
        let late = parse(&m.on_message(&attached("s2", "page")));
        assert!(!ready(&shared));
        for call in &late {
            let _ = m.on_message(&json!({"id": call["id"], "result": {}}));
        }
        assert!(ready(&shared));
        m.on_message(
            &json!({"method": "Target.detachedFromTarget", "params": {"sessionId": "s2"}}),
        );
        assert!(ready(&shared));
        m.disconnected();
        assert!(!ready(&shared), "fails closed without the connection");
    }

    #[test]
    fn a_tab_that_cant_be_guarded_keeps_the_browser_closed() {
        let (mut m, shared) = machine(NetMode::Strict);
        attach(&mut m, "s1", "page", false);
        assert!(!ready(&shared));
    }

    #[test]
    fn a_target_that_cant_be_armed_never_runs_and_is_closed() {
        let (mut m, shared) = machine(NetMode::Strict);
        attach(&mut m, "p", "page", true);
        // An iframe whose socket block fails, its interception fine.
        let out = parse(&m.on_message(&attached("f", "iframe")));
        let mut after = Vec::new();
        for call in &out {
            let failed = call["method"] == "Page.addScriptToEvaluateOnNewDocument";
            let reply = if failed {
                json!({"id": call["id"], "error": {"message": "no"}})
            } else {
                json!({"id": call["id"], "result": {}})
            };
            after.extend(parse(&m.on_message(&reply)));
        }
        assert_eq!(methods(&after), ["Target.closeTarget"]);
        assert_eq!(after[0]["params"]["targetId"], "t-f");
        assert!(
            after[0].get("sessionId").is_none(),
            "closed from the browser"
        );
        assert!(ready(&shared), "closed: the other tabs go on");
        // A worker whose closing fails too: the guard stops being ready.
        let worker = attach(&mut m, "w", "worker", false);
        assert!(!methods(&worker).contains(&"Runtime.runIfWaitingForDebugger"));
        let close = worker
            .iter()
            .find(|c| c["method"] == "Target.closeTarget")
            .unwrap();
        m.on_message(&json!({"id": close["id"], "error": {"message": "no"}}));
        assert!(!ready(&shared), "fails closed");
    }

    #[test]
    fn workers_are_guarded_and_other_targets_just_run() {
        let (mut m, shared) = machine(NetMode::Strict);
        attach(&mut m, "p", "page", true);
        let worker = attach(&mut m, "w", "service_worker", true);
        let sent = methods(&worker);
        assert!(sent.contains(&"Fetch.enable"));
        assert!(sent.contains(&"Runtime.evaluate"), "sockets off first");
        assert!(!sent.contains(&"Page.addScriptToEvaluateOnNewDocument"));
        assert_eq!(sent.last(), Some(&"Runtime.runIfWaitingForDebugger"));
        let other = attach(&mut m, "o", "auction_worklet", true);
        assert_eq!(other.len(), 1);
        assert_eq!(other[0]["method"], "Runtime.runIfWaitingForDebugger");
        assert!(ready(&shared), "only tabs count for ready");
    }

    #[test]
    fn writes_are_failed_and_remembered_without_query_or_body() {
        let (mut m, shared) = machine(NetMode::Strict);
        attach(&mut m, "s1", "page", true);
        let paused = |id: &str, method: &str, kind: &str, url: &str| {
            json!({"method": "Fetch.requestPaused", "sessionId": "s1", "params": {
                "requestId": id, "resourceType": kind,
                "request": {"url": url, "method": method, "postData": "secret=1"}}})
        };
        let out = parse(&m.on_message(&paused(
            "r1",
            "GET",
            "Document",
            "https://jobs.example.test/?q=x",
        )));
        assert_eq!(out[0]["method"], "Fetch.continueRequest");
        assert_eq!(out[0]["params"]["requestId"], "r1");
        let out = parse(&m.on_message(&paused(
            "r2",
            "POST",
            "Document",
            "https://jobs.example.test/post?token=abc",
        )));
        assert_eq!(out[0]["method"], "Fetch.failRequest");
        assert_eq!(out[0]["params"]["errorReason"], "BlockedByClient");
        assert_eq!(out[0]["sessionId"], "s1");
        // A beacon, which no tab owns: answered browser-wide.
        let out = parse(
            &m.on_message(&json!({"method": "Fetch.requestPaused", "params": {
            "requestId": "r3", "resourceType": "Ping",
            "request": {"url": "https://jobs.example.test/beacon", "method": "POST"}}})),
        );
        assert_eq!(out[0]["method"], "Fetch.failRequest");
        assert!(out[0].get("sessionId").is_none());
        let s = lock(&shared);
        assert_eq!(s.blocked_count, 2);
        assert_eq!(
            s.blocked[0],
            BlockedRequest {
                method: "POST".into(),
                host: "jobs.example.test".into(),
                path: "/post".into(),
            }
        );
    }

    #[test]
    fn xhr_mode_lets_script_posts_through() {
        let (mut m, shared) = machine(NetMode::XhrAllowed);
        attach(&mut m, "s1", "page", true);
        let out = parse(
            &m.on_message(&json!({"method": "Fetch.requestPaused", "sessionId": "s1",
            "params": {"requestId": "r", "resourceType": "Fetch",
                "request": {"url": "https://jobs.example.test/graphql", "method": "POST"}}})),
        );
        assert_eq!(out[0]["method"], "Fetch.continueRequest");
        assert_eq!(lock(&shared).blocked_count, 0);
    }
}
