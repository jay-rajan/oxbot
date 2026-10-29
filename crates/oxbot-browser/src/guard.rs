//! The guard: oxbot's own `DevTools` connection to a bot's Chrome, next to
//! the one the browser tool uses. It sends downloads to the folder of the bot
//! using the browser, takes screenshots for approval cards, and reads pages
//! for oxbot's safety checks (is this a password field?).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::http;

/// A running guard. Dropping it stops it.
pub(crate) struct Guard {
    commands: mpsc::Sender<Command>,
    task: JoinHandle<()>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum Command {
    Screenshot {
        page_url: Option<String>,
        reply: oneshot::Sender<Option<String>>,
    },
    Evaluate {
        page_url: Option<String>,
        expression: String,
        reply: oneshot::Sender<Option<Value>>,
    },
    SetDownloads(PathBuf),
    Close,
    /// Any `DevTools` call: on the browser, or on a tab (`on_page`) for
    /// calls Chrome only takes there, such as `Network.deleteCookies`.
    Call {
        method: String,
        params: Value,
        on_page: bool,
        reply: oneshot::Sender<Result<Value, String>>,
    },
}

impl Guard {
    /// Connects to the Chrome on `port`; downloads go to `downloads`.
    ///
    /// # Errors
    /// Fails if Chrome's `DevTools` connection can't be opened.
    pub(crate) async fn start(port: u16, downloads: PathBuf) -> Result<Self, String> {
        let version = http::get(port, "/json/version")
            .await
            .ok_or("Chrome's control port didn't answer")?;
        let ws_url = serde_json::from_str::<Value>(&version)
            .ok()
            .and_then(|v| v.get("webSocketDebuggerUrl")?.as_str().map(str::to_owned))
            .ok_or("Chrome gave no DevTools address")?;
        let (socket, _) = tokio_tungstenite::connect_async(ws_url.as_str())
            .await
            .map_err(|e| e.to_string())?;
        let (commands, rx) = mpsc::channel(8);
        let mut session = Session::new(downloads);
        let task = tokio::spawn(async move {
            if let Err(err) = session.run(socket, rx).await {
                tracing::debug!(%err, "browser guard stopped");
            }
        });
        Ok(Self { commands, task })
    }

    /// A JPEG of the page at `page_url` (or the page used last), as a
    /// `data:` URL; `None` if Chrome can't take one.
    pub(crate) async fn screenshot(&self, page_url: Option<&str>) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(Command::Screenshot {
                page_url: page_url.map(str::to_owned),
                reply,
            })
            .await
            .ok()?;
        tokio::time::timeout(Duration::from_secs(6), rx)
            .await
            .ok()?
            .ok()?
    }
}

impl Guard {
    /// Runs `expression` in the page at `page_url` (or the page used last)
    /// and returns its value; `None` if it can't.
    pub(crate) async fn evaluate(&self, page_url: Option<&str>, expression: &str) -> Option<Value> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(Command::Evaluate {
                page_url: page_url.map(str::to_owned),
                expression: expression.to_owned(),
                reply,
            })
            .await
            .ok()?;
        tokio::time::timeout(Duration::from_secs(4), rx)
            .await
            .ok()?
            .ok()?
    }

    /// Calls `method` and returns Chrome's result, or its error message.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        on_page: bool,
    ) -> Result<Value, String> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(Command::Call {
                method: method.to_owned(),
                params,
                on_page,
                reply,
            })
            .await
            .map_err(|_| "the browser guard stopped".to_owned())?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| format!("Chrome didn't answer {method}"))?
            .map_err(|_| "the browser guard stopped".to_owned())?
    }

    /// Asks Chrome to close cleanly, saving cookies and sign-ins.
    pub(crate) async fn close_browser(&self) {
        let _ = self.commands.send(Command::Close).await;
    }

    /// Sends downloads to `dir` from now on.
    pub(crate) async fn set_downloads(&self, dir: &Path) {
        let _ = self
            .commands
            .send(Command::SetDownloads(dir.to_owned()))
            .await;
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The guard's side of the `DevTools` connection.
struct Session {
    downloads: PathBuf,
    next_id: u64,
    /// Calls waiting for Chrome's reply.
    waiting: HashMap<u64, oneshot::Sender<Option<Value>>>,
    /// Every tab: target id → its address.
    tabs: HashMap<String, String>,
    /// Tabs we're attached to: `DevTools` session id → target id.
    attached: HashMap<String, String>,
    /// The tab whose page changed most recently (target id).
    last_tab: Option<String>,
    /// Calls whose whole reply (result or error) is wanted, not just the result.
    errors_wanted: std::collections::HashSet<u64>,
    outbox: Vec<String>,
}

impl Session {
    fn new(downloads: PathBuf) -> Self {
        Self {
            downloads,
            next_id: 1,
            waiting: HashMap::new(),
            tabs: HashMap::new(),
            attached: HashMap::new(),
            last_tab: None,
            errors_wanted: std::collections::HashSet::new(),
            outbox: Vec::new(),
        }
    }

    /// Queues a call; returns its id.
    fn call(&mut self, method: &str, params: &Value, session: Option<&str>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            msg["sessionId"] = json!(session);
        }
        self.outbox.push(msg.to_string());
        id
    }

    async fn run(
        &mut self,
        socket: Socket,
        mut commands: mpsc::Receiver<Command>,
    ) -> Result<(), String> {
        let (mut sink, mut stream) = socket.split();
        self.call(
            "Target.setDiscoverTargets",
            &json!({ "discover": true }),
            None,
        );
        self.send_downloads();
        // Attach to tabs (without holding them) so screenshots can be taken.
        self.call(
            "Target.setAutoAttach",
            &json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true }),
            None,
        );
        loop {
            for msg in self.outbox.drain(..) {
                sink.send(Message::Text(msg.into()))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            tokio::select! {
                frame = stream.next() => match frame {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(msg) = serde_json::from_str::<Value>(&text) {
                            self.handle(&msg);
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e.to_string()),
                    None => return Err("Chrome closed the connection".to_owned()),
                },
                Some(command) = commands.recv() => self.command(command),
            }
        }
    }

    fn send_downloads(&mut self) {
        let downloads = self.downloads.display().to_string();
        self.call(
            "Browser.setDownloadBehavior",
            &json!({ "behavior": "allow", "downloadPath": downloads, "eventsEnabled": false }),
            None,
        );
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Close => {
                self.call("Browser.close", &json!({}), None);
            }
            Command::SetDownloads(dir) => {
                self.downloads = dir;
                self.send_downloads();
            }
            Command::Call {
                method,
                params,
                on_page,
                reply,
            } => {
                let session = if on_page {
                    let Some(session) = self.pick_page(None) else {
                        let _ = reply.send(Err("no tab to work in".to_owned()));
                        return;
                    };
                    Some(session)
                } else {
                    None
                };
                let id = self.call(&method, &params, session.as_deref());
                let (tx, rx) = oneshot::channel();
                self.errors_wanted.insert(id);
                self.waiting.insert(id, tx);
                tokio::spawn(async move {
                    let answer = rx.await.ok().flatten().unwrap_or(Value::Null);
                    let _ = reply.send(match answer.get("error") {
                        Some(err) => Err(str_of(err, "message").to_owned()),
                        None => Ok(answer.get("result").cloned().unwrap_or(Value::Null)),
                    });
                });
            }
            Command::Evaluate {
                page_url,
                expression,
                reply,
            } => {
                let Some(session) = self.pick_page(page_url.as_deref()) else {
                    let _ = reply.send(None);
                    return;
                };
                let id = self.call(
                    "Runtime.evaluate",
                    &json!({ "expression": expression, "returnByValue": true }),
                    Some(&session),
                );
                let (tx, rx) = oneshot::channel();
                self.waiting.insert(id, tx);
                tokio::spawn(async move {
                    let value = rx
                        .await
                        .ok()
                        .flatten()
                        .and_then(|v| v.pointer("/result/value").cloned());
                    let _ = reply.send(value);
                });
            }
            Command::Screenshot { page_url, reply } => {
                let Some(session) = self.pick_page(page_url.as_deref()) else {
                    let _ = reply.send(None);
                    return;
                };
                let id = self.call(
                    "Page.captureScreenshot",
                    &json!({ "format": "jpeg", "quality": 55 }),
                    Some(&session),
                );
                let (tx, rx) = oneshot::channel();
                self.waiting.insert(id, tx);
                tokio::spawn(async move {
                    let shot = rx.await.ok().flatten().and_then(|v| {
                        v.get("data")
                            .and_then(Value::as_str)
                            .map(|d| format!("data:image/jpeg;base64,{d}"))
                    });
                    let _ = reply.send(shot);
                });
            }
        }
    }

    /// The session of the tab showing `page_url`, else of the tab that
    /// changed last.
    fn pick_page(&self, page_url: Option<&str>) -> Option<String> {
        let target = page_url
            .and_then(|url| self.tabs.iter().find(|(_, u)| u.as_str() == url))
            .map(|(t, _)| t.clone())
            .or_else(|| self.last_tab.clone());
        let session_of = |t: &str| {
            self.attached
                .iter()
                .find(|(_, target)| target.as_str() == t)
                .map(|(s, _)| s.clone())
        };
        target
            .and_then(|t| session_of(&t))
            .or_else(|| self.attached.keys().next().cloned())
    }

    fn handle(&mut self, msg: &Value) {
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            if let Some(tx) = self.waiting.remove(&id) {
                let reply = if self.errors_wanted.remove(&id) {
                    Some(msg.clone())
                } else {
                    msg.get("result").cloned()
                };
                let _ = tx.send(reply);
            }
            return;
        }
        let params = msg.get("params").unwrap_or(&Value::Null);
        match msg.get("method").and_then(Value::as_str) {
            Some("Target.targetCreated" | "Target.targetInfoChanged") => {
                let info = params.get("targetInfo").unwrap_or(&Value::Null);
                if str_of(info, "type") == "page" {
                    let target = str_of(info, "targetId").to_owned();
                    self.tabs
                        .insert(target.clone(), str_of(info, "url").to_owned());
                    self.last_tab = Some(target);
                }
            }
            Some("Target.targetDestroyed") => {
                self.tabs.remove(str_of(params, "targetId"));
            }
            Some("Target.attachedToTarget") => {
                let session = str_of(params, "sessionId").to_owned();
                let info = params.get("targetInfo").unwrap_or(&Value::Null);
                if str_of(info, "type") == "page" {
                    self.attached
                        .insert(session, str_of(info, "targetId").to_owned());
                } else {
                    self.call(
                        "Target.detachFromTarget",
                        &json!({ "sessionId": session }),
                        None,
                    );
                }
            }
            Some("Target.detachedFromTarget") => {
                self.attached.remove(str_of(params, "sessionId"));
            }
            _ => {}
        }
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
