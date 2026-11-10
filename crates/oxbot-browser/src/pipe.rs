//! How oxbot talks to hidden Chrome without opening Chrome's control port.
//!
//! Chrome's `--remote-debugging-port` lets any program on the computer (any
//! user's) connect without a password, read every cookie and drive every
//! signed-in site. So hidden Chrome runs with `--remote-debugging-pipe`
//! instead: it reads `DevTools` messages on file 3 and writes its answers
//! on file 4, each ending in a NUL byte, and listens on no port at all.
//!
//! oxbot owns that pipe ([`Pipe`]) and shares it through a small local
//! door ([`ControlProxy`]): a WebSocket server on `127.0.0.1` at a random
//! port that accepts one path only, `/devtools/browser/<token>`, where the
//! token is 32 random bytes made when the [`crate::Browser`] is. Every
//! other path (`/json/version`, `/json/list`, …) and any request from a
//! web page (one with an `Origin`) is refused. oxbot's guards and the bots'
//! browser tool (`chrome-devtools-mcp --wsEndpoint=…`) connect there.
//!
//! Each connection gets a browser session of its own on the one pipe
//! (`Target.attachToBrowserTarget`), so it behaves like a separate
//! `DevTools` client: its own target discovery and auto-attach, its own
//! tab sessions. Message ids are renumbered on the way in and back on the
//! way out, and a connection can only use the tab sessions it opened.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

/// The path prefix of the one address the proxy accepts.
const PATH_PREFIX: &str = "/devtools/browser/";
/// How long oxbot waits for Chrome to answer one of its own pipe calls.
const CALL_WAIT: Duration = Duration::from_secs(10);

/// Starts `chrome` with `args` and `--remote-debugging-pipe`, its file 3
/// and 4 joined to a new [`Pipe`]. Chrome's own output is dropped.
///
/// Rust can't hand a child extra files without `unsafe`, so `/bin/sh`
/// moves the two pipes from its input and output to files 3 and 4, points
/// input and output at `/dev/null`, and `exec`s Chrome (same process).
///
/// # Errors
/// The program couldn't be started.
pub(crate) fn spawn_piped(chrome: &Path, args: &[String]) -> std::io::Result<(Child, Arc<Pipe>)> {
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"exec "$0" "$@" 3<&0 4>&1 0</dev/null 1>/dev/null"#)
        .arg(chrome)
        .args(args)
        .arg("--remote-debugging-pipe")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let (Some(to_chrome), Some(from_chrome)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(std::io::Error::other("Chrome's pipe wasn't made"));
    };
    Ok((child, Pipe::start(to_chrome, from_chrome)))
}

/// What to do with Chrome's answer to one message.
enum Pending {
    /// oxbot's own call.
    Own(oneshot::Sender<Value>),
    /// Nobody waits for it.
    Ignore,
    /// A connection's message: give the answer back with its own `id`.
    Client {
        client: u64,
        id: Value,
        method: String,
    },
}

/// One connection through the proxy.
struct Client {
    /// Its browser session on the pipe.
    session: String,
    out: mpsc::UnboundedSender<String>,
}

#[derive(Default)]
struct Routes {
    next_id: u64,
    next_client: u64,
    pending: HashMap<u64, Pending>,
    /// Which connection owns each `DevTools` session.
    owners: HashMap<String, u64>,
    clients: HashMap<u64, Client>,
    /// Chrome closed the pipe (it quit).
    closed: bool,
}

impl Routes {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn drop_client(&mut self, client: u64) -> Option<Client> {
        self.owners.retain(|_, c| *c != client);
        self.clients.remove(&client)
    }
}

/// Hidden Chrome's `DevTools` pipe, shared by any number of connections.
pub(crate) struct Pipe {
    to_chrome: mpsc::UnboundedSender<String>,
    routes: Mutex<Routes>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        for task in self
            .tasks
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            task.abort();
        }
    }
}

impl Pipe {
    /// Starts reading and writing Chrome's pipe: `to_chrome` is its file 3,
    /// `from_chrome` its file 4.
    pub(crate) fn start(
        to_chrome: impl AsyncWrite + Unpin + Send + 'static,
        from_chrome: impl AsyncRead + Unpin + Send + 'static,
    ) -> Arc<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let pipe = Arc::new(Self {
            to_chrome: tx,
            routes: Mutex::new(Routes::default()),
            tasks: Mutex::new(Vec::new()),
        });
        let mut to_chrome = to_chrome;
        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                let mut bytes = msg.into_bytes();
                bytes.push(0);
                if to_chrome.write_all(&bytes).await.is_err() || to_chrome.flush().await.is_err() {
                    break;
                }
            }
        });
        let weak = Arc::downgrade(&pipe);
        let reader = tokio::spawn(async move {
            let mut from_chrome = BufReader::new(from_chrome);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match from_chrome.read_until(0, &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if buf.last() == Some(&0) {
                    buf.pop();
                }
                let Some(pipe) = weak.upgrade() else { return };
                if let Ok(msg) = serde_json::from_slice::<Value>(&buf) {
                    pipe.route_from_chrome(msg);
                }
            }
            if let Some(pipe) = weak.upgrade() {
                pipe.closed();
            }
        });
        pipe.tasks_lock().extend([writer, reader]);
        pipe
    }

    fn routes(&self) -> MutexGuard<'_, Routes> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tasks_lock(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Chrome quit: every connection ends.
    fn closed(&self) {
        let mut routes = self.routes();
        routes.closed = true;
        routes.clients.clear();
        routes.owners.clear();
        routes.pending.clear();
    }

    /// Whether Chrome has closed the pipe.
    pub(crate) fn is_closed(&self) -> bool {
        self.routes().closed
    }

    /// Calls `method` on Chrome's root session for oxbot itself (no
    /// connection sees it) and returns the result, or Chrome's error.
    ///
    /// # Errors
    /// Chrome refused, quit, or didn't answer in time.
    pub(crate) async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        {
            let mut routes = self.routes();
            if routes.closed {
                return Err("Chrome has quit".to_owned());
            }
            let id = routes.id();
            routes.pending.insert(id, Pending::Own(tx));
            let msg = json!({ "id": id, "method": method, "params": params });
            if self.to_chrome.send(msg.to_string()).is_err() {
                return Err("Chrome's pipe is closed".to_owned());
            }
        }
        let answer = tokio::time::timeout(CALL_WAIT, rx)
            .await
            .map_err(|_| format!("Chrome didn't answer {method}"))?
            .map_err(|_| "Chrome has quit".to_owned())?;
        if let Some(error) = answer.get("error") {
            return Err(error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Chrome refused")
                .to_owned());
        }
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    }

    /// A new connection: a browser session of its own. Chrome's messages
    /// for it arrive on the returned channel, which ends when Chrome quits
    /// or the session goes.
    ///
    /// # Errors
    /// Chrome refused the session, or has quit.
    pub(crate) async fn open_client(
        &self,
    ) -> Result<(u64, mpsc::UnboundedReceiver<String>), String> {
        let made = self.call("Target.attachToBrowserTarget", json!({})).await?;
        let session = made
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or("Chrome gave no browser session")?
            .to_owned();
        let (out, rx) = mpsc::unbounded_channel();
        let mut routes = self.routes();
        if routes.closed {
            return Err("Chrome has quit".to_owned());
        }
        routes.next_client += 1;
        let client = routes.next_client;
        routes.owners.insert(session.clone(), client);
        routes.clients.insert(client, Client { session, out });
        Ok((client, rx))
    }

    /// The connection `client` ended: its browser session (and with it
    /// every tab session it opened) is let go.
    pub(crate) fn close_client(&self, client: u64) {
        let mut routes = self.routes();
        let Some(gone) = routes.drop_client(client) else {
            return;
        };
        let id = routes.id();
        routes.pending.insert(id, Pending::Ignore);
        let msg = json!({
            "id": id,
            "method": "Target.detachFromTarget",
            "params": { "sessionId": gone.session },
        });
        let _ = self.to_chrome.send(msg.to_string());
    }

    /// Passes `text` from connection `client` to Chrome: a message without
    /// a session goes to the connection's browser session; one naming a
    /// session must name one the connection opened. Returns an error
    /// answer to send back instead, if it can't go.
    pub(crate) fn send_from(&self, client: u64, text: &str) -> Option<String> {
        let Ok(Value::Object(mut msg)) = serde_json::from_str::<Value>(text) else {
            return None;
        };
        let original = msg.get("id").cloned().unwrap_or(Value::Null);
        let refuse = |message: &str| {
            Some(
                json!({ "id": original, "error": { "code": -32001, "message": message } })
                    .to_string(),
            )
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let mut routes = self.routes();
        let Some(own) = routes.clients.get(&client).map(|c| c.session.clone()) else {
            return refuse("the browser connection has ended");
        };
        match msg.get("sessionId").and_then(Value::as_str) {
            Some(session) if routes.owners.get(session) == Some(&client) => {}
            Some(_) => return refuse("Session with given id not found."),
            None => {
                msg.insert("sessionId".to_owned(), Value::String(own));
            }
        }
        let id = routes.id();
        routes.pending.insert(
            id,
            Pending::Client {
                client,
                id: original,
                method,
            },
        );
        msg.insert("id".to_owned(), Value::from(id));
        let _ = self.to_chrome.send(Value::Object(msg).to_string());
        None
    }

    /// Routes one message from Chrome.
    fn route_from_chrome(&self, mut msg: Value) {
        let mut routes = self.routes();
        let client = if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            match routes.pending.remove(&id) {
                Some(Pending::Own(tx)) => {
                    let _ = tx.send(msg);
                    return;
                }
                Some(Pending::Ignore) | None => return,
                Some(Pending::Client { client, id, method }) => {
                    msg["id"] = id;
                    if method == "Target.attachToTarget"
                        && let Some(session) =
                            msg.pointer("/result/sessionId").and_then(Value::as_str)
                    {
                        routes.owners.insert(session.to_owned(), client);
                    }
                    client
                }
            }
        } else {
            let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
            let child = msg
                .pointer("/params/sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let Some(session) = msg.get("sessionId").and_then(Value::as_str) else {
                // The root session: only a browser session going matters.
                if method == "Target.detachedFromTarget"
                    && let Some(child) = child
                    && let Some(&client) = routes.owners.get(&child)
                    && routes
                        .clients
                        .get(&client)
                        .is_some_and(|c| c.session == child)
                {
                    routes.drop_client(client);
                }
                return;
            };
            let Some(&client) = routes.owners.get(session) else {
                return;
            };
            match (method, child) {
                ("Target.attachedToTarget", Some(child)) => {
                    routes.owners.insert(child, client);
                }
                ("Target.detachedFromTarget", Some(child)) => {
                    routes.owners.remove(&child);
                }
                _ => {}
            }
            client
        };
        let Some(c) = routes.clients.get(&client) else {
            return;
        };
        if msg.get("sessionId").and_then(Value::as_str) == Some(c.session.as_str())
            && let Some(map) = msg.as_object_mut()
        {
            map.remove("sessionId");
        }
        let _ = c.out.send(msg.to_string());
    }
}

/// The local door to hidden Chrome's pipe (see the module docs). It keeps
/// its port and token for the life of its [`crate::Browser`], so a
/// restarted Chrome is reached at the same address.
pub(crate) struct ControlProxy {
    endpoint: String,
    token: String,
    current: Arc<Mutex<Option<Arc<Pipe>>>>,
    listener: Mutex<Option<std::net::TcpListener>>,
    accept: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for ControlProxy {
    fn drop(&mut self) {
        if let Some(task) = self
            .accept
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

impl ControlProxy {
    /// A proxy on a free `127.0.0.1` port with a new random token. It
    /// accepts connections once [`ControlProxy::serve`] is called.
    ///
    /// # Errors
    /// No free port, or no random bytes.
    pub(crate) fn new() -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let token = random_token()?;
        Ok(Self {
            endpoint: format!("ws://127.0.0.1:{port}{PATH_PREFIX}{token}"),
            token,
            current: Arc::new(Mutex::new(None)),
            listener: Mutex::new(Some(listener)),
            accept: Mutex::new(None),
        })
    }

    /// The one address it accepts (secret: it lets the holder drive Chrome).
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Connections go to `pipe` from now on (`None`: Chrome isn't running,
    /// so they're refused). Starts accepting on first use.
    ///
    /// # Errors
    /// The listener couldn't join the async runtime.
    pub(crate) fn serve(&self, pipe: Option<Arc<Pipe>>) -> std::io::Result<()> {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = pipe;
        let mut accept = self.accept.lock().unwrap_or_else(PoisonError::into_inner);
        if accept.is_some() {
            return Ok(());
        }
        let Some(listener) = self
            .listener
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return Ok(());
        };
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let token = self.token.clone();
        let current = Arc::clone(&self.current);
        *accept = Some(tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let pipe = current
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                tokio::spawn(serve_one(tcp, token.clone(), pipe));
            }
        }));
        Ok(())
    }
}

/// One connection: checks its request, then passes messages both ways
/// until either side ends.
async fn serve_one(tcp: tokio::net::TcpStream, token: String, pipe: Option<Arc<Pipe>>) {
    let pipe = pipe.filter(|p| !p.is_closed());
    #[allow(clippy::result_large_err, reason = "tungstenite's callback type")]
    let check = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let has_origin = request.headers().contains_key("origin");
        if !allowed(request.uri().path(), &token, has_origin) {
            return Err(refusal(StatusCode::NOT_FOUND));
        }
        if pipe.is_none() {
            return Err(refusal(StatusCode::SERVICE_UNAVAILABLE));
        }
        Ok(response)
    };
    let Ok(socket) = tokio_tungstenite::accept_hdr_async(tcp, check).await else {
        return;
    };
    let Some(pipe) = pipe else { return };
    let Ok((client, mut from_chrome)) = pipe.open_client().await else {
        return;
    };
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            out = from_chrome.recv() => match out {
                Some(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Some(refused) = pipe.send_from(client, &text)
                        && sink.send(Message::Text(refused.into())).await.is_err()
                    {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    pipe.close_client(client);
    let _ = sink.close().await;
}

fn refusal(status: StatusCode) -> ErrorResponse {
    let mut response = ErrorResponse::new(None);
    *response.status_mut() = status;
    response
}

/// Whether a WebSocket request for `path` may connect: exactly the token
/// path, and not from a web page (browsers always send an `Origin`).
fn allowed(path: &str, token: &str, has_origin: bool) -> bool {
    !has_origin
        && path
            .strip_prefix(PATH_PREFIX)
            .is_some_and(|given| same_secret(given.as_bytes(), token.as_bytes()))
}

/// Compares two secrets in time that doesn't depend on where they differ.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 32 random bytes as hex.
fn random_token() -> std::io::Result<String> {
    use std::fmt::Write as _;
    let mut bytes = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| std::io::Error::other("no random bytes"))?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// Starts a throwaway hidden Chrome (`DEFAULT_CHROME`) with `args`, its
/// pipe behind a new proxy, for tests.
#[cfg(test)]
pub(crate) async fn test_chrome(args: &[String]) -> (Child, ControlProxy) {
    let (child, pipe) = spawn_piped(Path::new(crate::DEFAULT_CHROME), args).unwrap();
    pipe.call("Browser.getVersion", json!({})).await.unwrap();
    let proxy = ControlProxy::new().unwrap();
    proxy.serve(Some(pipe)).unwrap();
    (child, proxy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt as _;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

    #[test]
    fn only_the_token_path_from_a_program_may_connect() {
        let token = "ab".repeat(32);
        assert!(allowed(
            &format!("/devtools/browser/{token}"),
            &token,
            false
        ));
        assert!(
            !allowed(&format!("/devtools/browser/{token}"), &token, true),
            "a web page"
        );
        assert!(!allowed("/devtools/browser/", &token, false));
        assert!(!allowed(
            &format!("/devtools/browser/{token}x"),
            &token,
            false
        ));
        assert!(!allowed(&format!("/devtools/page/{token}"), &token, false));
        assert!(!allowed("/json/version", &token, false));
        assert!(!allowed("/json/list", &token, false));
        assert!(!allowed(&format!("/json/{token}"), &token, false));
        assert!(!allowed(
            &format!("/devtools/browser/{}", "cd".repeat(32)),
            &token,
            false
        ));
    }

    #[test]
    fn tokens_are_random_and_long() {
        let (a, b) = (random_token().unwrap(), random_token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    /// A stand-in Chrome on the other end of a pipe: it answers
    /// `Target.attachToBrowserTarget` with a new session, `Target.attachToTarget`
    /// with a tab session (after announcing it), and anything else by
    /// echoing the method and session back.
    fn fake_chrome() -> Arc<Pipe> {
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let (from_chrome, to_chrome) = tokio::io::split(ours);
        let (mut their_read, mut their_write) = tokio::io::split(theirs);
        tokio::spawn(async move {
            let mut sessions = 0;
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            while their_read.read_exact(&mut byte).await.is_ok() {
                if byte[0] != 0 {
                    buf.push(byte[0]);
                    continue;
                }
                let msg: Value = serde_json::from_slice(&buf).unwrap();
                buf.clear();
                let id = msg["id"].clone();
                let session = msg.get("sessionId").cloned();
                let mut out = Vec::new();
                match msg["method"].as_str().unwrap() {
                    "Target.attachToBrowserTarget" => {
                        sessions += 1;
                        out.push(json!({ "id": id, "result": { "sessionId": format!("B{sessions}") } }));
                    }
                    "Target.attachToTarget" => {
                        sessions += 1;
                        let tab = format!("T{sessions}");
                        out.push(json!({ "method": "Target.attachedToTarget", "sessionId": session, "params": { "sessionId": tab } }));
                        out.push(json!({ "id": id, "sessionId": session, "result": { "sessionId": tab } }));
                    }
                    method => out.push(json!({ "id": id, "sessionId": session, "result": { "method": method, "on": session } })),
                }
                for m in out {
                    let mut bytes = m.to_string().into_bytes();
                    bytes.push(0);
                    their_write.write_all(&bytes).await.unwrap();
                }
            }
        });
        Pipe::start(to_chrome, from_chrome)
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<String>) -> Value {
        let text = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[tokio::test]
    async fn each_connection_has_its_own_session_and_ids() {
        let pipe = fake_chrome();
        let (a, mut a_rx) = pipe.open_client().await.unwrap();
        let (b, mut b_rx) = pipe.open_client().await.unwrap();
        assert!(
            pipe.send_from(a, r#"{"id":1,"method":"Browser.getVersion"}"#)
                .is_none()
        );
        assert!(
            pipe.send_from(b, r#"{"id":1,"method":"Target.getTargets"}"#)
                .is_none()
        );
        let got_a = next(&mut a_rx).await;
        let got_b = next(&mut b_rx).await;
        // Each answer comes back with its own id, on its own browser
        // session, which the connection never sees.
        assert_eq!(
            got_a,
            json!({ "id": 1, "result": { "method": "Browser.getVersion", "on": "B1" } })
        );
        assert_eq!(
            got_b,
            json!({ "id": 1, "result": { "method": "Target.getTargets", "on": "B2" } })
        );

        // A tab session belongs to the connection that opened it.
        pipe.send_from(
            a,
            r#"{"id":2,"method":"Target.attachToTarget","params":{"targetId":"x"}}"#,
        );
        let event = next(&mut a_rx).await;
        assert_eq!(event["method"], "Target.attachedToTarget");
        assert_eq!(next(&mut a_rx).await["result"]["sessionId"], "T3");
        assert!(
            pipe.send_from(
                a,
                r#"{"id":3,"method":"Runtime.evaluate","sessionId":"T3"}"#
            )
            .is_none()
        );
        let reply = next(&mut a_rx).await;
        assert_eq!(reply["result"]["on"], "T3");
        assert_eq!(reply["sessionId"], "T3");
        let refused = pipe
            .send_from(
                b,
                r#"{"id":4,"method":"Runtime.evaluate","sessionId":"T3"}"#,
            )
            .unwrap();
        assert!(refused.contains("not found"), "{refused}");
        let refused = pipe
            .send_from(
                b,
                r#"{"id":5,"method":"Network.getAllCookies","sessionId":"B1"}"#,
            )
            .unwrap();
        assert!(
            refused.contains("not found"),
            "another connection's browser session"
        );

        // A closed connection's sessions go with it.
        pipe.close_client(a);
        assert!(
            pipe.send_from(a, r#"{"id":6,"method":"Browser.getVersion"}"#)
                .is_some()
        );
        assert!(pipe.routes().owners.values().all(|c| *c == b));
    }

    fn proxy() -> (ControlProxy, Arc<Pipe>) {
        let proxy = ControlProxy::new().unwrap();
        let pipe = fake_chrome();
        proxy.serve(Some(Arc::clone(&pipe))).unwrap();
        (proxy, pipe)
    }

    async fn http_get(port: &str, path: &str) -> String {
        let mut tcp = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        tcp.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
        let mut out = String::new();
        let _ = tokio::time::timeout(Duration::from_secs(3), tcp.read_to_string(&mut out)).await;
        out
    }

    #[tokio::test]
    async fn the_proxy_answers_only_its_token_path() {
        let (proxy, _pipe) = proxy();
        let endpoint = proxy.endpoint().to_owned();
        let port = endpoint
            .trim_start_matches("ws://127.0.0.1:")
            .split('/')
            .next()
            .unwrap()
            .to_owned();
        let token = endpoint.rsplit('/').next().unwrap().to_owned();

        // Chrome's usual HTTP addresses give nothing away.
        for path in [
            "/json/version",
            "/json/list",
            "/json",
            &format!("/json/version/{token}"),
        ] {
            let answer = http_get(&port, path).await;
            assert!(!answer.contains("200"), "{path}: {answer}");
            assert!(!answer.contains(&token), "{path}: {answer}");
            assert!(!answer.contains("webSocketDebuggerUrl"), "{path}: {answer}");
        }
        // A WebSocket to any other path, or from a web page, is refused.
        for url in [
            format!("ws://127.0.0.1:{port}/devtools/browser/{}", "0".repeat(64)),
            format!("ws://127.0.0.1:{port}/devtools/browser"),
            format!("ws://127.0.0.1:{port}/json/version"),
            format!("ws://127.0.0.1:{port}/"),
        ] {
            assert!(
                tokio_tungstenite::connect_async(url.as_str())
                    .await
                    .is_err(),
                "{url}"
            );
        }
        let mut from_page = endpoint.as_str().into_client_request().unwrap();
        from_page
            .headers_mut()
            .insert("Origin", "https://evil.example".parse().unwrap());
        assert!(
            tokio_tungstenite::connect_async(from_page).await.is_err(),
            "a web page"
        );

        // The token path is a DevTools connection.
        let (mut socket, _) = tokio_tungstenite::connect_async(endpoint.as_str())
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"id":7,"method":"Target.getTargets"}"#.into(),
            ))
            .await
            .unwrap();
        let Some(Ok(Message::Text(answer))) = socket.next().await else {
            panic!("no answer");
        };
        let answer: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["result"]["method"], "Target.getTargets");
    }

    #[tokio::test]
    async fn the_proxy_refuses_everyone_while_chrome_is_off() {
        let (proxy, _pipe) = proxy();
        proxy.serve(None).unwrap();
        assert!(
            tokio_tungstenite::connect_async(proxy.endpoint())
                .await
                .is_err()
        );
    }
}
