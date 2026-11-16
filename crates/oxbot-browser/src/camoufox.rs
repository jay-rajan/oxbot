//! The native Node adapter. Private administration uses stdio; model tools use
//! a capability-protected local broker with no administration methods.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use ring::rand::SecureRandom as _;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::visits::{NetEvent, Visits, Watch};
use crate::{BrowserError, BrowserSpec, BrowserStatus};

/// How often idle visits are checked for.
const VISIT_TICK: Duration = Duration::from_secs(15);

/// Visits under way, and where finished ones go.
#[derive(Default)]
struct VisitFeed {
    visits: Visits,
    watch: Option<Watch>,
}
impl VisitFeed {
    fn on(&mut self, event: &Value) {
        let Some(watch) = &self.watch else {
            return;
        };
        let tab = event["tab"].as_str().unwrap_or_default();
        let net = match event["kind"].as_str() {
            Some("request") => NetEvent::Request {
                tab,
                url: event["url"].as_str().unwrap_or_default(),
                top: event["top"] == true,
                redirect: event["redirect"] == true,
                up: event["up"].as_u64().unwrap_or(0),
            },
            Some("received") => NetEvent::Received {
                tab,
                bytes: event["bytes"].as_u64().unwrap_or(0),
            },
            Some("gone") => NetEvent::TabGone { tab },
            _ => return,
        };
        let done = self.visits.on(&net, now_ms(), watch.user().as_deref());
        watch.send(done);
    }
    fn tick(&mut self) {
        if let Some(watch) = &self.watch {
            let done = self.visits.tick(now_ms(), watch.user().as_deref());
            watch.send(done);
        }
    }
    fn end_all(&mut self) {
        let done = self.visits.end_all();
        if let Some(watch) = &self.watch {
            watch.send(done);
        }
    }
}
type Feed = Arc<StdMutex<VisitFeed>>;
fn feed_lock(feed: &Feed) -> std::sync::MutexGuard<'_, VisitFeed> {
    feed.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The explicit runtime, never discovered by falling back to a browser download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CamoufoxTool {
    /// Node already bundled with Oxbot.
    pub node: PathBuf,
    /// The generated native runtime directory.
    pub bundle: PathBuf,
    /// Test fixtures only: no visible window.
    pub headless: bool,
    /// End-to-end tests only: every page request goes through this HTTP
    /// proxy (the test site, which answers for `*.example.test`).
    pub proxy: Option<String>,
}
impl CamoufoxTool {
    /// Whether the files needed to start and list tools are present.
    #[must_use]
    pub fn available(&self) -> bool {
        self.node.is_file()
            && ["bundle.json", "runtime.mjs", "service.mjs", "mcp.mjs"]
                .iter()
                .all(|p| self.bundle.join(p).is_file())
    }
    /// A runtime inside the app's tools directory.
    #[must_use]
    pub fn find_in(dir: &Path) -> Option<Self> {
        let tool = Self {
            node: dir.join("node/bin/node"),
            bundle: dir.join("camoufox"),
            headless: false,
            proxy: None,
        };
        tool.available().then_some(tool)
    }
    /// The runtime built in this checkout (`node scripts/camoufox/build.mjs`,
    /// with Node from `scripts/fetch-tools.sh`), for development and tests.
    /// `OXBOT_CAMOUFOX_BUNDLE` names another bundle. `None` if not built.
    #[must_use]
    pub fn in_checkout() -> Option<Self> {
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        let tool = Self {
            node: target.join("tools-bundle/node/bin/node"),
            bundle: std::env::var_os("OXBOT_CAMOUFOX_BUNDLE")
                .map_or_else(|| target.join("camoufox-bundle"), PathBuf::from),
            headless: false,
            proxy: None,
        };
        tool.available().then_some(tool)
    }
}

/// Shared by every native browser owned by the home. The lock stays held until
/// an action completes, so delayed/cancelled queues cannot release a burst.
#[derive(Debug, Default)]
pub(crate) struct Courtesy {
    sites: Mutex<HashMap<String, Arc<Mutex<Option<Instant>>>>>,
}
impl Courtesy {
    async fn turn(
        &self,
        site: &str,
        gap: Duration,
        cancel: &CancellationToken,
    ) -> Result<OwnedMutexGuard<Option<Instant>>, String> {
        let lock = self
            .sites
            .lock()
            .await
            .entry(site.to_owned())
            .or_default()
            .clone();
        let guard = tokio::select! { () = cancel.cancelled() => return Err("Browser action cancelled before it ran".into()), guard = lock.lock_owned() => guard };
        if let Some(last) = *guard {
            tokio::select! { () = cancel.cancelled() => return Err("Browser action cancelled before it ran".into()), () = tokio::time::sleep_until(last + gap) => {} }
        }
        Ok(guard)
    }
}

type Reply = oneshot::Sender<Result<Value, String>>;
struct Rpc {
    child: StdMutex<Option<Child>>,
    output: mpsc::UnboundedSender<String>,
    pending: StdMutex<HashMap<u64, Reply>>,
    next: AtomicU64,
    alive: AtomicBool,
    one: Mutex<()>,
}
impl Rpc {
    async fn start(
        tool: &CamoufoxTool,
        profile: &Path,
        env: &[(String, String)],
        status: watch::Sender<BrowserStatus>,
        feed: Feed,
        paused: Arc<AtomicBool>,
    ) -> Result<Arc<Self>, String> {
        let mut command = Command::new(&tool.node);
        command
            .arg(tool.bundle.join("service.mjs"))
            .arg("--bundle")
            .arg(&tool.bundle)
            .arg("--profile")
            .arg(profile)
            .arg("--headless")
            .arg(tool.headless.to_string());
        if let Some(proxy) = &tool.proxy {
            command.arg("--proxy").arg(proxy);
        }
        let mut child = command
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Could not start Camoufox: {e}"))?;
        let mut input = child.stdin.take().ok_or("No browser input pipe")?;
        let mut lines =
            BufReader::new(child.stdout.take().ok_or("No browser output pipe")?).lines();
        let line = tokio::time::timeout(Duration::from_secs(45), lines.next_line())
            .await
            .map_err(|_| "Camoufox did not start in time")?
            .map_err(|e| e.to_string())?
            .ok_or("Camoufox stopped during startup")?;
        let ready: Value =
            serde_json::from_str(&line).map_err(|_| "Invalid browser startup response")?;
        if ready["event"] != "ready" {
            return Err("Camoufox did not confirm it was ready".into());
        }
        let (output, mut incoming) = mpsc::unbounded_channel::<String>();
        let rpc = Arc::new(Self {
            child: StdMutex::new(Some(child)),
            output,
            pending: StdMutex::default(),
            next: AtomicU64::new(1),
            alive: AtomicBool::new(true),
            one: Mutex::new(()),
        });
        tokio::spawn(async move {
            while let Some(line) = incoming.recv().await {
                if input.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let reader = Arc::clone(&rpc);
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                if value["event"] == "closed" {
                    status.send_replace(BrowserStatus::Off);
                }
                if value["event"] == "net" {
                    feed_lock(&feed).on(&value);
                    continue;
                }
                // The person closed the sign-in page: bots may go on.
                if value["event"] == "resumed" {
                    paused.store(false, Ordering::SeqCst);
                    status.send_replace(BrowserStatus::Ready);
                    continue;
                }
                if let Some(id) = value["id"].as_u64()
                    && let Some(reply) = reader
                        .pending
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&id)
                {
                    let result = value
                        .get("error")
                        .and_then(Value::as_str)
                        .map_or_else(|| Ok(value["result"].clone()), |e| Err(e.to_owned()));
                    let _ = reply.send(result);
                }
            }
            reader.alive.store(false, Ordering::SeqCst);
            feed_lock(&feed).end_all();
            status.send_replace(BrowserStatus::Off);
            for (_, reply) in reader
                .pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .drain()
            {
                let _ = reply.send(Err("The browser stopped".into()));
            }
        });
        Ok(rpc)
    }
    async fn call(
        &self,
        method: &str,
        params: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, String> {
        let _one = if let Some(cancel) = cancel {
            tokio::select! { () = cancel.cancelled() => return Err("Browser action cancelled before it ran".into()), guard = self.one.lock() => guard }
        } else {
            self.one.lock().await
        };
        if cancel.is_some_and(CancellationToken::is_cancelled) {
            return Err("Browser action cancelled before it ran".into());
        }
        if !self.alive.load(Ordering::SeqCst) {
            return Err("The browser stopped".into());
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, reply);
        let line = format!("{}\n", json!({"id":id,"method":method,"params":params}));
        if self.output.send(line).is_err() {
            return Err("The browser stopped".into());
        }
        // Once dispatched, mutations finish under the site lock even if the
        // caller cancels. They are never automatically replayed.
        let result = tokio::time::timeout(Duration::from_secs(60), answer).await;
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        result
            .map_err(|_| "The browser operation timed out".to_owned())?
            .map_err(|_| "The browser stopped".to_owned())?
    }
    async fn stop(&self) {
        // Shutdown is a priority message: it must not wait behind a page that
        // stopped responding. Node closes the context and flushes the profile.
        let _ = self
            .output
            .send(format!("{}\n", json!({"method":"shutdown"})));
        let child = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut child) = child
            && tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .is_err()
        {
            let _ = child.kill().await;
        }
    }
    fn kill(&self) {
        if let Some(child) = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            let _ = child.start_kill();
        }
    }
}

/// One owner/profile. The broker survives browser restarts; Node never owns a
/// public HTTP listener for browser administration.
pub(crate) struct Camoufox {
    tool: Option<CamoufoxTool>,
    feed: Feed,
    profile: PathBuf,
    env: Vec<(String, String)>,
    status: watch::Sender<BrowserStatus>,
    endpoint: String,
    host: String,
    path: String,
    listener: StdMutex<Option<std::net::TcpListener>>,
    server: StdMutex<Option<JoinHandle<()>>>,
    rpc: StdMutex<Option<Arc<Rpc>>>,
    start: Mutex<()>,
    paused: Arc<AtomicBool>,
    stopping: AtomicBool,
    courtesy: Arc<Courtesy>,
    requests: Mutex<HashMap<String, CancellationToken>>,
}
impl Camoufox {
    pub(crate) fn new(
        tool: Option<CamoufoxTool>,
        spec: &BrowserSpec,
        env: Vec<(String, String)>,
        status: watch::Sender<BrowserStatus>,
        courtesy: Arc<Courtesy>,
    ) -> std::io::Result<Arc<Self>> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let host = listener.local_addr()?.to_string();
        let mut random = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| std::io::Error::other("No browser capability could be created"))?;
        let token: String = random.iter().fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        });
        Ok(Arc::new(Self {
            tool,
            feed: Feed::default(),
            profile: spec.profile_dir.clone(),
            env,
            status,
            endpoint: format!("http://{host}/{token}"),
            host,
            path: format!("/{token}"),
            listener: StdMutex::new(Some(listener)),
            server: StdMutex::default(),
            rpc: StdMutex::default(),
            start: Mutex::new(()),
            paused: Arc::new(AtomicBool::new(false)),
            stopping: AtomicBool::new(false),
            courtesy,
            requests: Mutex::default(),
        }))
    }
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }
    /// Counts visits from now on.
    pub(crate) fn count_visits(&self, watch: Watch) {
        feed_lock(&self.feed).watch = Some(watch);
    }
    pub(crate) fn is_running(&self) -> bool {
        self.current().is_some()
    }
    fn current(&self) -> Option<Arc<Rpc>> {
        self.rpc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .filter(|rpc| rpc.alive.load(Ordering::SeqCst))
    }
    pub(crate) fn ensure(
        self: &Arc<Self>,
    ) -> futures_util::future::BoxFuture<'_, Result<(), BrowserError>> {
        Box::pin(async move {
            if self.stopping.load(Ordering::SeqCst) {
                return Err(BrowserError::Browser("The browser is stopping".into()));
            }
            let tool = match &self.tool {
                Some(tool) if tool.available() => tool,
                _ => return Err(BrowserError::Missing),
            };
            if self.current().is_some() {
                return Ok(());
            }
            let _start = self.start.lock().await;
            if self.current().is_some() {
                return Ok(());
            }
            if let Some(listener) = self
                .listener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .map_err(|e| BrowserError::Browser(e.to_string()))?;
                let router = Router::new()
                    .route(&self.path, post(public_tool))
                    .layer(DefaultBodyLimit::max(2_000_000))
                    .with_state(Arc::downgrade(self));
                let task = tokio::spawn(async move {
                    let _ = axum::serve(listener, router).await;
                });
                *self
                    .server
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
            }
            let rpc = Rpc::start(
                tool,
                &self.profile,
                &self.env,
                self.status.clone(),
                Arc::clone(&self.feed),
                Arc::clone(&self.paused),
            )
            .await
            .map_err(BrowserError::Browser)?;
            // Ends idle visits while this run of the browser lasts.
            let ticking = Arc::downgrade(&rpc);
            let feed = Arc::clone(&self.feed);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(VISIT_TICK).await;
                    match ticking.upgrade() {
                        Some(rpc) if rpc.alive.load(Ordering::SeqCst) => feed_lock(&feed).tick(),
                        _ => break,
                    }
                }
            });
            self.paused.store(false, Ordering::SeqCst);
            *self
                .rpc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rpc);
            self.status.send_replace(BrowserStatus::Ready);
            Ok(())
        })
    }
    pub(crate) async fn call(
        self: &Arc<Self>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.ensure().await.map_err(|e| e.to_string())?;
        self.current()
            .ok_or("The browser stopped")?
            .call(method, params, None)
            .await
    }
    pub(crate) async fn open(self: &Arc<Self>, url: &str) -> Result<(), BrowserError> {
        self.ensure().await?;
        self.paused.store(true, Ordering::SeqCst);
        self.status.send_replace(BrowserStatus::SigningIn);
        self.call("open_window", json!({"url":url}))
            .await
            .map_err(BrowserError::Browser)?;
        tracing::info!("the browser is in front for a sign-in");
        Ok(())
    }
    pub(crate) async fn resume(self: &Arc<Self>) {
        if self.call("close_window", json!({})).await.is_ok() {
            self.paused.store(false, Ordering::SeqCst);
            self.status.send_replace(BrowserStatus::Ready);
        }
    }
    pub(crate) async fn stop(&self) {
        let _start = self.start.lock().await;
        self.stopping.store(true, Ordering::SeqCst);
        for cancel in self.requests.lock().await.values() {
            cancel.cancel();
        }
        let rpc = self
            .rpc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(rpc) = rpc {
            rpc.stop().await;
        }
        self.status.send_replace(BrowserStatus::Off);
        self.stopping.store(false, Ordering::SeqCst);
    }
    async fn execute(
        self: Arc<Self>,
        body: Value,
        cancel: CancellationToken,
    ) -> Result<Value, String> {
        if cancel.is_cancelled() {
            return Err("Browser action cancelled before it ran".into());
        }
        if self.paused.load(Ordering::SeqCst) {
            return Err("You are using the browser; automation is paused".into());
        }
        self.ensure().await.map_err(|e| e.to_string())?;
        let name = body["name"].as_str().ok_or("Missing browser action")?;
        let changing = matches!(
            name,
            "navigate_page"
                | "new_page"
                | "click"
                | "fill"
                | "fill_form"
                | "type_text"
                | "press_key"
                | "hover"
                | "drag"
                | "upload_file"
                | "handle_dialog"
        );
        let load = matches!(name, "navigate_page" | "new_page" | "click")
            || name == "press_key" && body["args"]["key"].as_str() == Some("Enter");
        let url = if matches!(name, "navigate_page" | "new_page") {
            body["args"]["url"].as_str().map(str::to_owned)
        } else {
            self.call("page_url", json!({}))
                .await
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
        };
        let mut guard = if changing {
            if let Some(host) = url.as_deref().and_then(crate::web_host) {
                Some(
                    self.courtesy
                        .turn(
                            &crate::site_of(&host),
                            Duration::from_secs(if load { 8 } else { 3 }),
                            &cancel,
                        )
                        .await?,
                )
            } else {
                None
            }
        } else {
            None
        };
        // read_sites loads several sites at once: each waits its turn (in
        // the sites' order, so two calls can't hold each other up).
        let mut guards = Vec::new();
        if name == "read_sites" {
            let sites: std::collections::BTreeSet<String> = body["args"]["urls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(crate::web_host)
                .map(|host| crate::site_of(&host))
                .collect();
            for site in sites {
                guards.push(
                    self.courtesy
                        .turn(&site, Duration::from_secs(8), &cancel)
                        .await?,
                );
            }
        }
        if self.paused.load(Ordering::SeqCst) {
            return Err("You are using the browser; automation is paused".into());
        }
        let result = self
            .current()
            .ok_or("The browser stopped")?
            .call("tool", body, Some(&cancel))
            .await;
        for guard in guards.iter_mut().chain(guard.as_mut()) {
            **guard = Some(Instant::now());
        }
        result
    }
}
impl Drop for Camoufox {
    fn drop(&mut self) {
        if let Some(task) = self
            .server
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        if let Some(rpc) = self
            .rpc
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            rpc.kill();
        }
    }
}
async fn public_tool(
    State(weak): State<Weak<Camoufox>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let Some(browser) = weak.upgrade() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"The browser is unavailable"})),
        );
    };
    if headers.contains_key("origin")
        || headers.get("host").and_then(|s| s.to_str().ok()) != Some(browser.host.as_str())
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":"Browser access refused"})),
        );
    }
    let id = body["operation"].as_str().unwrap_or("").to_owned();
    if id.len() > 128 || id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Missing browser operation id"})),
        );
    }
    if body["cancel"] == true {
        browser
            .requests
            .lock()
            .await
            .entry(id.clone())
            .or_insert_with(CancellationToken::new)
            .cancel();
        let owner = Arc::downgrade(&browser);
        let expired = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(120)).await;
            if let Some(owner) = owner.upgrade() {
                owner.requests.lock().await.remove(&expired);
            }
        });
        return (StatusCode::OK, Json(json!({})));
    }
    let cancel = CancellationToken::new();
    {
        let mut requests = browser.requests.lock().await;
        if requests.contains_key(&id) {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"Duplicate browser operation"})),
            );
        }
        requests.insert(id.clone(), cancel.clone());
    }
    let (reply, answer) = oneshot::channel();
    let owner = Arc::clone(&browser);
    tokio::spawn(async move {
        let result = Arc::clone(&owner).execute(body, cancel).await;
        owner.requests.lock().await.remove(&id);
        let _ = reply.send(result);
    });
    match answer.await {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, Json(json!({"error":error}))),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"The browser stopped"})),
        ),
    }
}
