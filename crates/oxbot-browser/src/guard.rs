//! The guard: oxbot's own `DevTools` connection to a bot's Chrome, next to
//! the one the browser tool uses. It sends downloads to the folder of the bot
//! using the browser, takes screenshots for approval cards, and reads pages
//! for oxbot's safety checks (is this a password field? is this button a
//! search form's?). Those reads run in an isolated world of their own, so
//! the page's scripts can't see or change them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

use crate::element::{ELEMENT_FACTS_FN, ElementFacts};
use crate::http;

/// The name of oxbot's isolated world in each page.
const WORLD: &str = "oxbot-check";
/// Objects oxbot holds while it looks at an element, released together.
const OBJECT_GROUP: &str = "oxbot-check";
/// At most this many elements with the same role and name are looked at.
const MAX_HITS: usize = 8;
/// Isolated worlds remembered (one per tab).
const MAX_WORLDS: usize = 64;

/// A running guard. Dropping it stops it.
pub(crate) struct Guard {
    commands: mpsc::Sender<Command>,
    task: JoinHandle<()>,
    /// Each tab's isolated world: `DevTools` session → the world.
    worlds: Mutex<HashMap<String, World>>,
}

/// oxbot's isolated world in one document.
#[derive(Debug, Clone, PartialEq, Eq)]
struct World {
    frame: String,
    /// Changes when the frame loads a new document (the world goes with it).
    loader: String,
    context: u64,
}

/// Where a `DevTools` call goes.
enum Target {
    /// The browser itself.
    Browser,
    /// The tab showing this address, else the one that changed last.
    Page(Option<String>),
    /// A tab already picked.
    Session(String),
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
    /// The `DevTools` session of the tab showing an address (or the one
    /// that changed last).
    PickPage {
        page_url: Option<String>,
        reply: oneshot::Sender<Option<String>>,
    },
    SetDownloads(PathBuf),
    Close,
    /// Any `DevTools` call: on the browser, or on a tab for calls Chrome
    /// only takes there, such as `Network.deleteCookies`.
    Call {
        method: String,
        params: Value,
        target: Target,
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
        Ok(Self {
            commands,
            task,
            worlds: Mutex::new(HashMap::new()),
        })
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
    /// Runs `expression` in oxbot's isolated world of the page at
    /// `page_url` (or the page used last) and returns its value; `None` if
    /// it can't.
    pub(crate) async fn evaluate(&self, page_url: Option<&str>, expression: &str) -> Option<Value> {
        tokio::time::timeout(Duration::from_secs(4), async {
            let session = self.pick_page(page_url).await?;
            let params = |context: u64| {
                json!({ "expression": expression, "contextId": context, "returnByValue": true })
            };
            let (reply, _) = self
                .in_world(&session, |context| {
                    self.call_on(&session, "Runtime.evaluate", params(context))
                })
                .await?;
            reply.pointer("/result/value").cloned()
        })
        .await
        .ok()
        .flatten()
    }

    /// What the elements with accessibility `role` and `name` on the page
    /// at `page_url` (or the page used last) are: one entry per match, at
    /// most [`MAX_HITS`]. Empty if there are none or Chrome can't tell
    /// within 1.5 seconds.
    pub(crate) async fn describe(
        &self,
        page_url: Option<&str>,
        role: &str,
        name: &str,
    ) -> Vec<ElementFacts> {
        let Some(session) = self.pick_page(page_url).await else {
            return Vec::new();
        };
        let found = tokio::time::timeout(
            Duration::from_millis(1500),
            self.describe_in(&session, role, name),
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        let _ = self
            .call_on(
                &session,
                "Runtime.releaseObjectGroup",
                json!({ "objectGroup": OBJECT_GROUP }),
            )
            .await;
        found
    }

    async fn describe_in(
        &self,
        session: &str,
        role: &str,
        name: &str,
    ) -> Option<Vec<ElementFacts>> {
        let (document, context) = self
            .in_world(session, |context| {
                self.call_on(
                    session,
                    "Runtime.evaluate",
                    json!({ "expression": "document", "contextId": context, "objectGroup": OBJECT_GROUP }),
                )
            })
            .await?;
        let document = document.pointer("/result/objectId")?.as_str()?.to_owned();
        let mut query = json!({ "objectId": document, "accessibleName": name });
        if !role.is_empty() {
            query["role"] = json!(role);
        }
        let tree = self
            .call_on(session, "Accessibility.queryAXTree", query)
            .await
            .ok()?;
        let nodes: Vec<u64> = tree
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .filter(|n| n.get("ignored").and_then(Value::as_bool) != Some(true))
                    .filter_map(|n| n.get("backendDOMNodeId").and_then(Value::as_u64))
                    .take(MAX_HITS)
                    .collect()
            })
            .unwrap_or_default();
        let mut found = Vec::new();
        for node in nodes {
            let Ok(object) = self
                .call_on(
                    session,
                    "DOM.resolveNode",
                    json!({ "backendNodeId": node, "executionContextId": context, "objectGroup": OBJECT_GROUP }),
                )
                .await
            else {
                continue;
            };
            let Some(object) = object.pointer("/object/objectId").and_then(Value::as_str) else {
                continue;
            };
            let facts = self
                .call_on(
                    session,
                    "Runtime.callFunctionOn",
                    json!({ "functionDeclaration": ELEMENT_FACTS_FN, "objectId": object, "returnByValue": true }),
                )
                .await
                .ok()
                .and_then(|v| v.pointer("/result/value").and_then(ElementFacts::from_value));
            found.extend(facts);
        }
        Some(found)
    }

    /// Runs `call` with oxbot's isolated world in `session`'s page, making
    /// the world again once if the page has replaced it. Returns the reply
    /// and the world's context id.
    async fn in_world<F, Fut>(&self, session: &str, call: F) -> Option<(Value, u64)>
    where
        F: Fn(u64) -> Fut,
        Fut: std::future::Future<Output = Result<Value, String>>,
    {
        for _ in 0..2 {
            let world = self.world(session).await?;
            match call(world.context).await {
                Ok(value) if value.get("exceptionDetails").is_none() => {
                    return Some((value, world.context));
                }
                Ok(_) => return None,
                Err(err) => {
                    tracing::debug!(%err, "isolated world call failed; making it again");
                    self.forget_world(session);
                }
            }
        }
        None
    }

    /// oxbot's isolated world in `session`'s main frame, made once per
    /// document.
    async fn world(&self, session: &str) -> Option<World> {
        let tree = self
            .call_on(session, "Page.getFrameTree", json!({}))
            .await
            .ok()?;
        let frame = tree.pointer("/frameTree/frame")?;
        let (id, loader) = (str_of(frame, "id"), str_of(frame, "loaderId"));
        if let Some(world) = self.worlds_lock().get(session).cloned()
            && world.frame == id
            && world.loader == loader
        {
            return Some(world);
        }
        let made = self
            .call_on(
                session,
                "Page.createIsolatedWorld",
                json!({ "frameId": id, "worldName": WORLD, "grantUniveralAccess": false }),
            )
            .await
            .ok()?;
        let world = World {
            frame: id.to_owned(),
            loader: loader.to_owned(),
            context: made.get("executionContextId")?.as_u64()?,
        };
        let mut worlds = self.worlds_lock();
        if worlds.len() >= MAX_WORLDS {
            worlds.clear();
        }
        worlds.insert(session.to_owned(), world.clone());
        Some(world)
    }

    fn forget_world(&self, session: &str) {
        self.worlds_lock().remove(session);
    }

    fn worlds_lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, World>> {
        self.worlds
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The `DevTools` session of the tab showing `page_url` (or the one
    /// that changed last).
    async fn pick_page(&self, page_url: Option<&str>) -> Option<String> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(Command::PickPage {
                page_url: page_url.map(str::to_owned),
                reply,
            })
            .await
            .ok()?;
        rx.await.ok().flatten()
    }

    /// Calls `method` on the browser, or on a tab (`on_page`), and returns
    /// Chrome's result, or its error message.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        on_page: bool,
    ) -> Result<Value, String> {
        let target = if on_page {
            Target::Page(None)
        } else {
            Target::Browser
        };
        self.send_call(method, params, target).await
    }

    async fn call_on(&self, session: &str, method: &str, params: Value) -> Result<Value, String> {
        self.send_call(method, params, Target::Session(session.to_owned()))
            .await
    }

    async fn send_call(
        &self,
        method: &str,
        params: Value,
        target: Target,
    ) -> Result<Value, String> {
        let (reply, rx) = oneshot::channel();
        self.commands
            .send(Command::Call {
                method: method.to_owned(),
                params,
                target,
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
            Command::PickPage { page_url, reply } => {
                let _ = reply.send(self.pick_page(page_url.as_deref()));
            }
            Command::Call {
                method,
                params,
                target,
                reply,
            } => {
                let session = match target {
                    Target::Browser => None,
                    Target::Session(session) => Some(session),
                    Target::Page(page_url) => {
                        let Some(session) = self.pick_page(page_url.as_deref()) else {
                            let _ = reply.send(Err("no tab to work in".to_owned()));
                            return;
                        };
                        Some(session)
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs Chrome: `cargo test -p oxbot-browser -- --ignored real_chrome`.
    #[tokio::test]
    #[ignore = "starts Chrome"]
    async fn real_chrome_reads_forms_in_an_isolated_world() {
        let dir = tempfile::tempdir().unwrap();
        let port = 9400 + u16::try_from(std::process::id() % 400).unwrap();
        let page = "<html><body>\
            <form method=get action=/s><input type=search name=q value=x><button>Go now</button></form>\
            <form method=post action=/p><input name=method value=get><input name=t value=hi><button type=submit>Send it</button></form>\
            <script>Object.defineProperty(HTMLButtonElement.prototype, 'form', {get(){return null}});</script>\
            </body></html>";
        let mut chrome = std::process::Command::new(crate::DEFAULT_CHROME)
            .args([
                "--headless=new",
                &format!("--remote-debugging-port={port}"),
                &format!("--user-data-dir={}", dir.path().display()),
                "--no-first-run",
                &format!("data:text/html,{}", crate::http::encode_url(page)),
            ])
            .spawn()
            .unwrap();
        let mut guard = None;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Ok(g) = Guard::start(port, dir.path().to_owned()).await {
                guard = Some(g);
                break;
            }
        }
        let guard = guard.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
        let go = guard.describe(None, "button", "Go now").await;
        let send = guard.describe(None, "button", "Send it").await;
        let typed = guard
            .evaluate(
                None,
                "String(Object.getOwnPropertyDescriptor(HTMLButtonElement.prototype, 'form').get).includes('native code')",
            )
            .await;
        let _ = chrome.kill();
        let _ = chrome.wait();
        assert_eq!(go.len(), 1, "{go:?}");
        assert!(go[0].submits && go[0].in_search);
        assert_eq!(go[0].form_method.as_deref(), Some("get"));
        assert_eq!(send.len(), 1, "{send:?}");
        assert!(send[0].submits, "the page's own getter doesn't reach oxbot");
        assert_eq!(
            send[0].form_method.as_deref(),
            Some("post"),
            "a field named method doesn't either"
        );
        assert_eq!(send[0].filled_fields, 2);
        assert_eq!(
            typed,
            Some(json!(true)),
            "evaluate runs in the isolated world"
        );
    }
}
