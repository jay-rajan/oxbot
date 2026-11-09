//! [`Browser`]: the Chrome every bot shares. It keeps its profile, so the
//! user signs in to a site once and every bot stays signed in. It runs
//! hidden, under oxbot's control, and comes up as a normal window only
//! while the user signs in.
//!
//! The sign-in window is plain Chrome on the same profile: no remote
//! debugging and no automation flags, so sites that look for automated
//! browsers (X's sign-in, say) load as they would in the person's own
//! Chrome, and no "controlled by automated software" bar shows. oxbot
//! doesn't talk to it at all: it waits for it to quit, or (on a Mac, where
//! Chrome stays open with no windows) for its last window to close, then
//! quits it and starts hidden Chrome again. Whether the sign-in worked is
//! read from the cookies after that (see `oxbot_core`'s sign-ins).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};

use crate::guard::Guard;
use crate::netguard::{BlockedRequest, NetGuard, NetMode};
use crate::sealed::{self, Copied, Ran, SealedTab};
use crate::visits::{VisitSink, Watch};
use crate::{BrowserError, CookieMarks, ElementFacts, Tools, http};

/// What the shared browser should be.
#[derive(Debug, Clone)]
pub struct BrowserSpec {
    /// A name for logs.
    pub label: String,
    /// The kept profile folder: cookies and sign-ins live here.
    pub profile_dir: PathBuf,
    /// Where downloads go until [`Browser::set_downloads`] changes it.
    pub downloads: PathBuf,
}

/// What the browser is doing, for showing to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserStatus {
    /// Not running.
    Off,
    /// Hidden Chrome is running.
    Ready,
    /// The visible sign-in window is open.
    SigningIn,
}

enum State {
    Off,
    /// Hidden Chrome, under oxbot's control.
    Hidden(Child),
    /// The plain sign-in window (oxbot has no connection to it).
    Window(Child),
}

/// How Chrome is started (see [`chrome_args`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launch<'a> {
    /// Hidden and under oxbot's control on `port`, sending `user_agent`.
    Hidden {
        port: u16,
        user_agent: Option<&'a str>,
    },
    /// The plain sign-in window at `url` (`headless`: end-to-end tests).
    Window { url: &'a str, headless: bool },
}

/// Chrome's command line (after the program) for `launch` on `profile`,
/// with `extra` flags (end-to-end tests' host rules).
fn chrome_args(profile: &Path, launch: &Launch<'_>, extra: &[String]) -> Vec<String> {
    let mut args = vec![
        format!("--user-data-dir={}", profile.display()),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-sync".to_owned(),
    ];
    match launch {
        Launch::Hidden { port, user_agent } => {
            args.extend([
                "--remote-debugging-address=127.0.0.1".to_owned(),
                format!("--remote-debugging-port={port}"),
                // WebRTC only through a proxy: in a sealed tab (whose proxy
                // goes nowhere) it can't send anything at all.
                "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
            ]);
            args.extend(extra.iter().cloned());
            // Hidden Chrome calls itself "HeadlessChrome", which many sites
            // block. It sends the same name as the user's normal Chrome.
            if let Some(agent) = user_agent {
                args.push(format!("--user-agent={agent}"));
            }
            args.extend(["--headless=new".to_owned(), "about:blank".to_owned()]);
        }
        Launch::Window { url, headless } => {
            args.extend(extra.iter().cloned());
            if *headless {
                args.push("--headless=new".to_owned());
            }
            args.push((*url).to_owned());
        }
    }
    args
}

impl State {
    fn child(&mut self) -> Option<&mut Child> {
        match self {
            Self::Hidden(c) | Self::Window(c) => Some(c),
            Self::Off => None,
        }
    }

    fn take(&mut self) -> Option<Child> {
        match std::mem::replace(self, Self::Off) {
            Self::Hidden(c) | Self::Window(c) => Some(c),
            Self::Off => None,
        }
    }
}

/// The shared Chrome. Use it as `Arc<Browser>`.
pub struct Browser {
    tools: Arc<Tools>,
    label: String,
    profile_dir: PathBuf,
    downloads: std::sync::Mutex<PathBuf>,
    port: u16,
    state: Mutex<State>,
    guard: Mutex<Option<Guard>>,
    status: watch::Sender<BrowserStatus>,
    /// The user agent hidden Chrome sends, worked out on first start.
    user_agent: std::sync::OnceLock<Option<String>>,
    /// A practice run's Chrome: how much its pages may send.
    net: Option<NetMode>,
    /// Its network guard, while Chrome runs.
    net_guard: Mutex<Option<NetGuard>>,
    /// The sealed tab of a `read_pages` run, while one is open.
    sealed: Mutex<Option<SealedTab>>,
    /// Where its visits to websites go, if they're counted.
    visit_sink: std::sync::Mutex<Option<VisitSink>>,
    /// Who is using it now (the hub's tag), for the visits.
    user: Arc<std::sync::Mutex<Option<String>>>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("label", &self.label)
            .field("profile", &self.profile_dir)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl Browser {
    /// A browser for `spec`. Nothing starts until it's needed.
    ///
    /// # Errors
    /// Fails if no free local port can be found for Chrome's control port.
    pub fn new(tools: Arc<Tools>, spec: BrowserSpec) -> std::io::Result<Arc<Self>> {
        Self::make(tools, spec, None)
    }

    /// A practice run's browser for `spec`: every tab is held until its
    /// network guard is on (see [`crate::netguard`]), and pages may send
    /// only what `mode` lets through. Nothing starts until it's needed.
    ///
    /// # Errors
    /// Fails if no free local port can be found for Chrome's control port.
    pub fn new_trial(
        tools: Arc<Tools>,
        spec: BrowserSpec,
        mode: NetMode,
    ) -> std::io::Result<Arc<Self>> {
        Self::make(tools, spec, Some(mode))
    }

    fn make(
        tools: Arc<Tools>,
        spec: BrowserSpec,
        net: Option<NetMode>,
    ) -> std::io::Result<Arc<Self>> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        Ok(Arc::new(Self {
            tools,
            label: spec.label,
            profile_dir: spec.profile_dir,
            downloads: std::sync::Mutex::new(spec.downloads),
            port,
            state: Mutex::new(State::Off),
            guard: Mutex::new(None),
            status: watch::channel(BrowserStatus::Off).0,
            user_agent: std::sync::OnceLock::new(),
            net,
            net_guard: Mutex::new(None),
            sealed: Mutex::new(None),
            visit_sink: std::sync::Mutex::new(None),
            user: Arc::new(std::sync::Mutex::new(None)),
        }))
    }

    /// Counts its visits to websites from the next time Chrome starts:
    /// each finished [`crate::Visit`] goes to `sink` (see
    /// [`crate::visits`]).
    pub fn count_visits(&self, sink: VisitSink) {
        *self
            .visit_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sink);
    }

    /// Who is using it from now on (a bot's id; `None`: no one). Visits
    /// started by someone else end.
    pub fn set_user(&self, user: Option<String>) {
        *self
            .user
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = user;
    }

    /// Whether this is a practice run's browser.
    #[must_use]
    pub fn is_trial(&self) -> bool {
        self.net.is_some()
    }

    /// For a practice run's browser: whether its network guard is on in
    /// every tab, waiting up to `wait` for it. Always true for a live one.
    pub async fn net_ready(&self, wait: Duration) -> bool {
        if self.net.is_none() {
            return true;
        }
        match self.net_guard.lock().await.as_ref() {
            Some(guard) => guard.wait_ready(wait).await,
            None => false,
        }
    }

    /// For a practice run's browser: the requests its network guard
    /// stopped (the first ones), and how many in all.
    pub async fn blocked_requests(&self) -> (Vec<BlockedRequest>, usize) {
        match self.net_guard.lock().await.as_ref() {
            Some(guard) => guard.blocked(),
            None => (Vec::new(), 0),
        }
    }

    /// Chrome's control port, the same for the life of this `Browser`, so a
    /// restarted Chrome is found again by every bot's browser tool.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The icon Chrome saved for `site` (a main domain) when a page of it
    /// was open, as a `data:image/png` URL. Read from the profile on disk:
    /// nothing is downloaded. `None` if Chrome has none.
    #[must_use]
    pub fn site_icon(&self, site: &str) -> Option<String> {
        self.site_icons(&[site]).pop().flatten()
    }

    /// Like [`Browser::site_icon`] for several sites, reading Chrome's file
    /// once. One entry per site, in order.
    #[must_use]
    pub fn site_icons(&self, sites: &[&str]) -> Vec<Option<String>> {
        site_icons(&self.profile_dir.join("Default/Favicons"), sites)
    }

    /// The kept profile folder.
    #[must_use]
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }

    /// Live status.
    #[must_use]
    pub fn status(&self) -> watch::Receiver<BrowserStatus> {
        self.status.subscribe()
    }

    /// Makes sure Chrome is running: the hidden one, or the sign-in window if
    /// it's open. Restarts it if it exited.
    ///
    /// # Errors
    /// Fails if Chrome is missing or doesn't start.
    pub async fn ensure_running(&self) -> Result<(), BrowserError> {
        let mut state = self.state.lock().await;
        if state
            .child()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
        {
            return Ok(());
        }
        if !matches!(*state, State::Off) {
            tracing::warn!(browser = self.label, "Chrome had exited; restarting it");
        }
        self.start_hidden(&mut state).await
    }

    async fn start_hidden(&self, state: &mut State) -> Result<(), BrowserError> {
        self.kill_leftover().await;
        let chrome = self.tools.config().chrome.clone();
        let user_agent = self.user_agent.get_or_init(|| user_agent_for(&chrome));
        let mut child = self.spawn_chrome(&Launch::Hidden {
            port: self.port,
            user_agent: user_agent.as_deref(),
        })?;
        if let Err(err) = self.wait_until_ready().await {
            let _ = child.kill().await;
            return Err(err);
        }
        self.start_guard().await;
        if let Err(err) = self.start_net_guard().await {
            // Fail closed: a practice run's Chrome without its guard isn't used.
            let _ = child.kill().await;
            return Err(err);
        }
        *state = State::Hidden(child);
        self.status.send_replace(BrowserStatus::Ready);
        Ok(())
    }

    /// Shows the browser as a plain Chrome window at `url` so the user can
    /// sign in, in place of the hidden Chrome (one Chrome per profile at a
    /// time): no remote control, no automation flags. If the window is
    /// already open, `url` opens in a new tab of it. When the user closes
    /// the window, hidden Chrome starts again straight away.
    ///
    /// # Errors
    /// Fails if Chrome is missing or doesn't start.
    pub async fn open_window(self: &Arc<Self>, url: &str) -> Result<(), BrowserError> {
        let headless = self.tools.config().hidden_windows;
        let window = Launch::Window { url, headless };
        let mut state = self.state.lock().await;
        if let State::Window(child) = &mut *state
            && matches!(child.try_wait(), Ok(None))
        {
            drop(state);
            if headless {
                tracing::warn!("a hidden sign-in window can't take another tab");
                return Ok(());
            }
            // Chrome hands a second start on the same profile to the one
            // running, which opens `url` in a new tab; this one then exits.
            let mut forward = self.spawn_chrome(&window)?;
            tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(30), forward.wait()).await;
            });
            return Ok(());
        }
        if let Some(child) = state.take() {
            self.quit(child).await;
        }
        let child = match self.spawn_chrome(&window) {
            Ok(child) => child,
            Err(err) => {
                self.status.send_replace(BrowserStatus::Off);
                return Err(err);
            }
        };
        *state = State::Window(child);
        drop(state);
        self.status.send_replace(BrowserStatus::SigningIn);
        tokio::spawn(watch_window(Arc::downgrade(self), !headless));
        Ok(())
    }

    /// Closes the sign-in window (if open) and starts hidden Chrome again on
    /// the same port, so a waiting bot can carry on.
    pub async fn close_window(&self) {
        let mut state = self.state.lock().await;
        if !matches!(*state, State::Window(_)) {
            return;
        }
        self.back_to_hidden(&mut state).await;
    }

    /// Quits the window (Chrome saves the new sign-in to the profile) and
    /// starts hidden Chrome.
    async fn back_to_hidden(&self, state: &mut State) {
        if let Some(child) = state.take() {
            self.quit(child).await;
        }
        tracing::info!(
            browser = self.label,
            "sign-in window closed; back to hidden Chrome"
        );
        if let Err(err) = self.start_hidden(state).await {
            tracing::error!(browser = self.label, %err, "couldn't restart hidden Chrome");
            self.status.send_replace(BrowserStatus::Off);
        }
    }

    /// Whether the sign-in window is open.
    pub async fn is_window_open(&self) -> bool {
        matches!(*self.state.lock().await, State::Window(_))
    }

    /// Closes every tab and leaves one blank page, so the next bot starts
    /// fresh (it stays signed in). Does nothing if Chrome isn't running.
    pub async fn reset_tabs(&self) {
        let Some(list) = http::get(self.port, "/json/list").await else {
            return;
        };
        let old = http::page_ids(&list);
        if !http::put(self.port, "/json/new?about:blank").await {
            tracing::warn!("couldn't open a blank tab");
            return;
        }
        for id in old {
            let _ = http::get(self.port, &format!("/json/close/{id}")).await;
        }
    }

    /// Sends downloads to `dir` from now on (the folder of the bot using the
    /// browser).
    pub async fn set_downloads(&self, dir: &Path) {
        if let Err(err) = tokio::fs::create_dir_all(dir).await {
            tracing::warn!(%err, "couldn't create the downloads folder");
        }
        dir.clone_into(
            &mut self
                .downloads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if let Some(guard) = self.guard.lock().await.as_ref() {
            guard.set_downloads(dir).await;
        }
    }

    /// A picture of the page at `page_url` (or the one used last), as a
    /// `data:` URL.
    pub async fn screenshot(&self, page_url: Option<&str>) -> Option<String> {
        let guard = self.guard.lock().await;
        guard.as_ref()?.screenshot(page_url).await
    }

    /// Runs `expression` (JavaScript) in oxbot's isolated world of the page
    /// at `page_url` (or the one used last) and returns its JSON value. For
    /// oxbot's own checks only; bots never run scripts.
    pub async fn inspect(&self, page_url: Option<&str>, expression: &str) -> Option<Value> {
        let guard = self.guard.lock().await;
        guard.as_ref()?.evaluate(page_url, expression).await
    }

    /// Copies the page at `page_url` (its HTML, up to
    /// [`sealed::MAX_PAGE_BYTES`]), from oxbot's isolated world: the page's
    /// scripts can't see or change the copy as it's made.
    ///
    /// # Errors
    /// The guard isn't running or Chrome didn't answer in time.
    pub async fn copy_page(&self, page_url: &str) -> Result<Copied, BrowserError> {
        let value = self
            .inspect(
                Some(page_url),
                &sealed::copy_expression(sealed::MAX_PAGE_BYTES),
            )
            .await
            .ok_or_else(|| BrowserError::Chrome("the page couldn't be copied".into()))?;
        sealed::read_copy(&value)
            .ok_or_else(|| BrowserError::Chrome("the page couldn't be copied".into()))
    }

    /// Runs the bot's `script` on a copy (`html`, from `url`) in the sealed
    /// tab (see [`crate::sealed`]), opening it if it isn't open yet; with
    /// `follow`, also finds the next page's link on the copy. The tab stays
    /// open for the next page until [`Browser::close_sealed`].
    ///
    /// # Errors
    /// What went wrong, in plain words (the script's own error, or the tab
    /// couldn't be opened).
    pub async fn read_sealed(
        &self,
        html: &str,
        url: &str,
        script: &str,
        follow: Option<&str>,
    ) -> Result<Ran, String> {
        let expression = sealed::sealed_expression(html, url, script, follow, sealed::SCRIPT_LIMIT);
        let guard = self.guard.lock().await;
        let guard = guard.as_ref().ok_or("the browser isn't running")?;
        let mut open = self.sealed.lock().await;
        // A tab Chrome lost (it restarted, say) is opened again once.
        for attempt in 0..2 {
            let tab = if let Some(tab) = open.as_ref() {
                tab.clone()
            } else {
                let tab = guard
                    .open_sealed()
                    .await
                    .map_err(|e| format!("the sealed tab couldn't be opened: {e}"))?;
                *open = Some(tab.clone());
                tab
            };
            match guard
                .run_sealed(&tab, &expression, sealed::SCRIPT_LIMIT)
                .await
            {
                Ok(reply) => return sealed::read_outcome(&reply),
                Err(err) if attempt == 0 => {
                    tracing::debug!(%err, "the sealed tab didn't answer; opening it again");
                    guard.close_sealed(&tab).await;
                    *open = None;
                }
                Err(err) => return Err(format!("the sealed tab didn't answer: {err}")),
            }
        }
        Err("the sealed tab didn't answer".to_owned())
    }

    /// Closes the sealed tab, if one is open.
    pub async fn close_sealed(&self) {
        let Some(tab) = self.sealed.lock().await.take() else {
            return;
        };
        if let Some(guard) = self.guard.lock().await.as_ref() {
            guard.close_sealed(&tab).await;
        }
    }

    /// What the elements with accessibility `role` and `name` on the page at
    /// `page_url` (or the one used last) are: whether they submit a form,
    /// and what kind. One entry per match; empty if none was found in time.
    pub async fn describe(
        &self,
        page_url: Option<&str>,
        role: &str,
        name: &str,
    ) -> Vec<ElementFacts> {
        let guard = self.guard.lock().await;
        match guard.as_ref() {
            Some(guard) => guard.describe(page_url, role, name).await,
            None => Vec::new(),
        }
    }

    /// The address Chrome reports for the tab at `page_url` (or the one
    /// used last), as oxbot's own connection sees it: unlike the browser
    /// tool's page list, a page's title can't change it. `None` if the guard
    /// isn't running.
    pub async fn page_url(&self, page_url: Option<&str>) -> Option<String> {
        let guard = self.guard.lock().await;
        guard.as_ref()?.page_url(page_url).await
    }

    /// The sites the shared browser holds cookies for, as main domains
    /// (`accounts.google.com` → `google.com`), sorted. Starts hidden Chrome
    /// if it's off.
    ///
    /// # Errors
    /// Fails if Chrome can't start or doesn't answer.
    pub async fn cookie_sites(self: &Arc<Self>) -> Result<Vec<String>, BrowserError> {
        Ok(self.cookie_marks().await?.sites())
    }

    /// Which cookies the browser holds, by site (names and keyed hashes of
    /// values, never values), to compare before and after a sign-in window.
    /// Starts hidden Chrome if it's off.
    ///
    /// # Errors
    /// Fails if Chrome can't start or doesn't answer.
    pub async fn cookie_marks(self: &Arc<Self>) -> Result<CookieMarks, BrowserError> {
        Ok(CookieMarks::from_cookies(&self.cookies().await?))
    }

    /// Signs the shared browser out of one site: deletes the cookies of
    /// `site` and its subdomains, and the data the site stored (local
    /// storage, databases, caches). Other sites stay signed in. Returns how
    /// many cookies went. Starts hidden Chrome if it's off.
    ///
    /// The site's history and saved icon stay: Chrome's remote control has
    /// no call to delete one site's history, and Chrome holds its `History`
    /// and `Favicons` files open while it runs, so they can't be edited
    /// safely either.
    ///
    /// # Errors
    /// Fails if Chrome can't start or refuses.
    pub async fn sign_out_site(self: &Arc<Self>, site: &str) -> Result<usize, BrowserError> {
        let site = site_of(site);
        let cookies = self.cookies().await?;
        let guard = self.guard.lock().await;
        let guard = guard
            .as_ref()
            .ok_or_else(|| BrowserError::Chrome("no DevTools connection".into()))?;
        let mut hosts = vec![site.clone()];
        let mut gone = 0;
        for cookie in cookies {
            let domain = cookie.get("domain").and_then(Value::as_str).unwrap_or("");
            if site_of(domain) != site {
                continue;
            }
            let mut params = serde_json::json!({
                "name": cookie.get("name").cloned().unwrap_or(Value::Null),
                "domain": domain,
                "path": cookie.get("path").cloned().unwrap_or_else(|| "/".into()),
            });
            if let Some(key) = cookie.get("partitionKey") {
                params["partitionKey"] = key.clone();
            }
            guard
                .call("Network.deleteCookies", params, true)
                .await
                .map_err(BrowserError::Chrome)?;
            gone += 1;
            let host = domain.trim_start_matches('.').to_owned();
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        for host in hosts {
            let params =
                serde_json::json!({ "origin": format!("https://{host}"), "storageTypes": "all" });
            if let Err(err) = guard.call("Storage.clearDataForOrigin", params, true).await {
                tracing::debug!(host, err, "couldn't clear site data");
            }
        }
        tracing::info!(cookies = gone, "signed out of one site");
        Ok(gone)
    }

    /// The cookies of `site` (a main domain) and its subdomains, as
    /// [`Browser::add_cookies`] takes them: to share a sign-in with
    /// another browser. Starts hidden Chrome if it's off.
    ///
    /// # Errors
    /// Fails if Chrome can't start or doesn't answer.
    pub async fn site_cookies(self: &Arc<Self>, site: &str) -> Result<Vec<Value>, BrowserError> {
        Ok(crate::cookies::site_cookie_params(
            &self.cookies().await?,
            site,
        ))
    }

    /// Adds `cookies` (from [`Browser::site_cookies`]) to this browser,
    /// replacing ones of the same name, domain and path. Returns how many.
    /// Starts hidden Chrome if it's off.
    ///
    /// # Errors
    /// Fails if Chrome can't start or refuses them.
    pub async fn add_cookies(self: &Arc<Self>, cookies: Vec<Value>) -> Result<usize, BrowserError> {
        if cookies.is_empty() {
            return Ok(0);
        }
        self.ensure_running().await?;
        let n = cookies.len();
        let guard = self.guard.lock().await;
        let guard = guard
            .as_ref()
            .ok_or_else(|| BrowserError::Chrome("no DevTools connection".into()))?;
        guard
            .call(
                "Storage.setCookies",
                serde_json::json!({ "cookies": cookies }),
                false,
            )
            .await
            .map_err(BrowserError::Chrome)?;
        tracing::info!(
            browser = self.label,
            cookies = n,
            "added a shared sign-in's cookies"
        );
        Ok(n)
    }

    /// Every cookie in the shared browser (`Storage.getCookies`).
    async fn cookies(self: &Arc<Self>) -> Result<Vec<Value>, BrowserError> {
        self.ensure_running().await?;
        let guard = self.guard.lock().await;
        let guard = guard
            .as_ref()
            .ok_or_else(|| BrowserError::Chrome("no DevTools connection".into()))?;
        let result = guard
            .call("Storage.getCookies", serde_json::json!({}), false)
            .await
            .map_err(BrowserError::Chrome)?;
        Ok(result
            .get("cookies")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Stops Chrome, letting it save its profile. Sign-ins stay.
    pub async fn shutdown(&self) {
        let child = self.state.lock().await.take();
        match child {
            Some(child) => self.quit(child).await,
            None => drop(self.guard.lock().await.take()),
        }
        self.status.send_replace(BrowserStatus::Off);
    }

    /// Quits Chrome so it writes everything to the profile first. Chrome
    /// keeps new cookies in memory for a while, and a signal can stop it
    /// before they're saved, losing a fresh sign-in. So oxbot asks Chrome
    /// itself to close (`Browser.close`), and only signals it if that fails.
    async fn quit(&self, mut child: Child) {
        self.net_guard.lock().await.take();
        if let Some(guard) = self.guard.lock().await.take() {
            guard.close_browser().await;
            if tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .is_ok()
            {
                return;
            }
            tracing::warn!(
                browser = self.label,
                "Chrome didn't close when asked; stopping it"
            );
        }
        stop_chrome(&mut child).await;
    }

    /// Stops Chrome and deletes the profile, signing out of every site.
    ///
    /// # Errors
    /// Fails if the profile folder can't be deleted.
    pub async fn sign_out(&self) -> Result<(), BrowserError> {
        self.shutdown().await;
        if self.profile_dir.exists() {
            tokio::fs::remove_dir_all(&self.profile_dir)
                .await
                .map_err(|source| BrowserError::Remove {
                    path: self.profile_dir.clone(),
                    source,
                })?;
        }
        tracing::info!(browser = self.label, "signed out of every site");
        Ok(())
    }

    /// A practice run's network guard, before any page loads. Nothing for
    /// a live browser.
    async fn start_net_guard(&self) -> Result<(), BrowserError> {
        let Some(mode) = self.net else {
            return Ok(());
        };
        let guard = NetGuard::start(self.port, mode)
            .await
            .map_err(BrowserError::Chrome)?;
        *self.net_guard.lock().await = Some(guard);
        Ok(())
    }

    async fn start_guard(&self) {
        let downloads = self
            .downloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Err(err) = tokio::fs::create_dir_all(&downloads).await {
            tracing::warn!(%err, "couldn't create the downloads folder");
        }
        let watch = self
            .visit_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .map(|sink| Watch {
                sink,
                user: Arc::clone(&self.user),
            });
        match Guard::start(self.port, downloads, watch).await {
            Ok(guard) => *self.guard.lock().await = Some(guard),
            Err(err) => {
                tracing::warn!(browser = self.label, %err, "browser guard didn't start (no screenshots or page checks)");
            }
        }
    }

    /// Stops a Chrome left on this profile by an oxbot that didn't shut down
    /// cleanly; otherwise the new one would find the profile in use.
    async fn kill_leftover(&self) {
        let pattern = format!(
            "--user-data-dir={}( |$)",
            regex_escape(&self.profile_dir.display().to_string())
        );
        if stop_main_chromes(&pattern).await {
            tracing::warn!(
                browser = self.label,
                "stopped a leftover Chrome on the shared profile"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Starts Chrome as `launch` says.
    fn spawn_chrome(&self, launch: &Launch<'_>) -> Result<Child, BrowserError> {
        let chrome = &self.tools.config().chrome;
        if !chrome.exists() {
            return Err(BrowserError::ChromeMissing(chrome.clone()));
        }
        let mut cmd = Command::new(chrome);
        cmd.args(chrome_args(
            &self.profile_dir,
            launch,
            &self.tools.config().chrome_args,
        ));
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(|source| BrowserError::Spawn {
            program: chrome.display().to_string(),
            source,
        })?;
        tracing::info!(
            browser = self.label,
            window = matches!(launch, Launch::Window { .. }),
            pid = child.id(),
            "Chrome started"
        );
        Ok(child)
    }

    async fn wait_until_ready(&self) -> Result<(), BrowserError> {
        for _ in 0..50 {
            if http::get(self.port, "/json/version").await.is_some() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(BrowserError::NotReady)
    }
}

/// Watches the sign-in window. When the user closes it, oxbot quits it
/// and starts hidden Chrome again on the same port at once, so a bot in
/// the middle of a task carries on. Closed means Chrome quit, or (with
/// `count_windows`: a visible window on a Mac, where Chrome stays open
/// with no windows) its last window went, once one had shown.
async fn watch_window(browser: std::sync::Weak<Browser>, count_windows: bool) {
    let mut seen_window = false;
    let mut empty_checks = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Some(this) = browser.upgrade() else {
            return;
        };
        let (exited, pid) = {
            let mut state = this.state.lock().await;
            match &mut *state {
                State::Window(child) => (!matches!(child.try_wait(), Ok(None)), child.id()),
                _ => return,
            }
        };
        if !exited {
            let windows = match pid {
                Some(pid) if count_windows => window_count(pid).await,
                _ => None,
            };
            match windows {
                Some(0) if seen_window => empty_checks += 1,
                Some(0) | None => continue,
                Some(_) => {
                    seen_window = true;
                    empty_checks = 0;
                }
            }
            if empty_checks < 2 {
                continue;
            }
            tracing::info!(
                browser = this.label,
                "the sign-in window's last window closed"
            );
        }
        let mut state = this.state.lock().await;
        if matches!(*state, State::Window(_)) {
            this.back_to_hidden(&mut state).await;
        }
        return;
    }
}

/// How many browser windows process `pid` has open (minimized ones too),
/// from macOS's window list: Chrome's own small helper windows (menu
/// bar strips, 1×1 ones) don't count. `None` where that can't be read
/// (not a Mac, or `osascript` failed). Needs no permission: only each
/// window's owner, layer and size are read, never its title.
async fn window_count(pid: u32) -> Option<usize> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let script = format!(
        "ObjC.import('CoreGraphics'); \
         const all = ObjC.deepUnwrap(ObjC.castRefToObject(\
           $.CGWindowListCopyWindowInfo($.kCGWindowListOptionAll, 0))) || []; \
         String(all.filter(w => w.kCGWindowOwnerPID === {pid} && w.kCGWindowLayer === 0 \
           && w.kCGWindowBounds && w.kCGWindowBounds.Width >= {MIN_WINDOW} \
           && w.kCGWindowBounds.Height >= {MIN_WINDOW}).length)"
    );
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("/usr/bin/osascript")
            .args(["-l", "JavaScript", "-e", &script])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// Smallest width and height (points) of a window that counts as a
/// browser window in [`window_count`].
const MIN_WINDOW: u32 = 100;

/// Asks Chrome to quit normally (SIGTERM), so it writes cookies and sign-ins
/// to disk, and only forces it after 10 seconds. A forced kill loses recent
/// sign-ins and marks the profile as crashed.
async fn stop_chrome(child: &mut Child) {
    if let Some(pid) = child.id() {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await;
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_ok()
        {
            return;
        }
        tracing::warn!(pid, "Chrome didn't quit in time; forcing it");
    }
    let _ = child.kill().await;
}

/// Stops every main Chrome whose command line matches `pattern` (an
/// extended regex for `pgrep -f`). Returns whether it stopped any.
async fn stop_main_chromes(pattern: &str) -> bool {
    let Ok(found) = Command::new("pgrep")
        .args(["-f", "--", pattern])
        .output()
        .await
    else {
        return false;
    };
    let mut stopped = false;
    for pid in String::from_utf8_lossy(&found.stdout).split_whitespace() {
        // Helpers (`--type=renderer`…) of a Chrome that's just quitting
        // carry the profile too; only a main Chrome is a leftover.
        let Ok(cmd) = Command::new("ps")
            .args(["-o", "command=", "-p", pid])
            .output()
            .await
        else {
            continue;
        };
        if is_main_chrome(&String::from_utf8_lossy(&cmd.stdout)) {
            let _ = Command::new("kill").args(["-TERM", pid]).status().await;
            stopped = true;
        }
    }
    stopped
}

/// Stops any Chrome still running on a profile inside `root` (practice
/// runs an oxbot that stopped mid-run left behind).
pub(crate) async fn stop_chromes_under(root: &Path) {
    let pattern = format!(
        "--user-data-dir={}/",
        regex_escape(&root.display().to_string())
    );
    if stop_main_chromes(&pattern).await {
        tracing::warn!("stopped a practice run's leftover Chrome");
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Whether a process command line is a main Chrome, not one of its helpers.
fn is_main_chrome(command: &str) -> bool {
    !command.trim().is_empty() && !command.contains(" --type=")
}

/// Escapes `text` for a `pgrep` (extended regex) pattern.
fn regex_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The icon saved in Chrome's `Favicons` database for pages of `site` (or
/// its subdomains), closest to 32 px, as a `data:image/png` URL.
fn site_icons(db: &Path, sites: &[&str]) -> Vec<Option<String>> {
    let none = || vec![None; sites.len()];
    if !db.is_file() {
        return none();
    }
    // Read-only and "immutable": oxbot never writes Chrome's files, and
    // doesn't wait on Chrome's lock. `%`, `?` and `#` in the path would be
    // read as part of the URI, so they're escaped.
    let path = db
        .display()
        .to_string()
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        format!("file:{path}?immutable=1"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return none();
    };
    let Ok(mut query) = conn.prepare(
        "SELECT b.image_data FROM icon_mapping m JOIN favicon_bitmaps b ON b.icon_id = m.icon_id
         WHERE m.page_url LIKE 'http%://' || ?1 || '/%' ESCAPE '\\'
            OR m.page_url LIKE 'http%://%.' || ?1 || '/%' ESCAPE '\\'
         ORDER BY ABS(b.width - 32) LIMIT 1",
    ) else {
        return none();
    };
    sites
        .iter()
        .map(|site| {
            // A site's own `%` and `_` match only themselves.
            let pattern = site
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            let png: Vec<u8> = query.query_row([pattern], |r| r.get(0)).ok()?;
            png.starts_with(b"\x89PNG")
                .then(|| format!("data:image/png;base64,{}", base64(&png)))
        })
        .collect()
}

/// Standard base64, for a small icon.
fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> shift) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The main domain of a host or cookie domain, the way people name a site:
/// `accounts.google.com` and `.google.com` → `google.com`,
/// `jay.substack.com` → `substack.com`, `www.bbc.co.uk` → `bbc.co.uk`.
/// Addresses without one (`127.0.0.1`, `localhost`) stay as they are.
#[must_use]
pub fn site_of(host: &str) -> String {
    let host = host.trim().trim_start_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    psl::domain_str(&host).map_or(host.clone(), str::to_owned)
}

/// The user agent normal Chrome sends, for this Chrome install (asks
/// `chrome --version`). `None` if the version can't be read; hidden Chrome
/// then keeps its own.
fn user_agent_for(chrome: &Path) -> Option<String> {
    let out = std::process::Command::new(chrome)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    user_agent(&String::from_utf8_lossy(&out.stdout))
}

/// Builds the user agent from `chrome --version` output such as
/// "Google Chrome 156.0.8078.73". Chrome reports only its major version.
fn user_agent(version: &str) -> Option<String> {
    let major = version
        .split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?
        .split('.')
        .next()?;
    if major.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let platform = if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else if cfg!(target_os = "windows") {
        "Windows NT 10.0; Win64; x64"
    } else {
        "X11; Linux x86_64"
    };
    Some(format!(
        "Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{major}.0.0.0 Safari/537.36"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolsConfig;

    #[tokio::test]
    async fn missing_chrome_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = ToolsConfig::in_home(tmp.path());
        config.chrome = PathBuf::from("/no/chrome");
        let spec = BrowserSpec {
            label: "test".into(),
            profile_dir: tmp.path().join("profile"),
            downloads: tmp.path().join("downloads"),
        };
        let browser = Browser::new(Tools::new(config), spec).unwrap();
        assert!(matches!(
            browser.ensure_running().await,
            Err(BrowserError::ChromeMissing(_))
        ));
        assert!(matches!(
            browser.open_window("https://accounts.google.com").await,
            Err(BrowserError::ChromeMissing(_))
        ));
        browser.shutdown().await;
        assert_eq!(*browser.status().borrow(), BrowserStatus::Off);
    }

    #[tokio::test]
    async fn signing_out_deletes_the_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        let spec = BrowserSpec {
            label: "test".into(),
            profile_dir: profile.clone(),
            downloads: tmp.path().join("downloads"),
        };
        let browser = Browser::new(Tools::new(ToolsConfig::in_home(tmp.path())), spec).unwrap();
        browser.shutdown().await;
        assert!(profile.exists(), "shutting down keeps sign-ins");
        browser.sign_out().await.unwrap();
        assert!(!profile.exists());
    }

    /// Flags that let a program control Chrome, or tell sites it's
    /// automated: never on the sign-in window.
    const CONTROL_FLAGS: [&str; 5] = [
        "--remote-debugging-port",
        "--remote-debugging-address",
        "--remote-debugging-pipe",
        "--enable-automation",
        "--user-agent",
    ];

    #[test]
    fn the_sign_in_window_is_plain_chrome() {
        let profile = Path::new("/p/profile");
        let window = chrome_args(
            profile,
            &Launch::Window {
                url: "https://x.com/i/flow/login",
                headless: false,
            },
            &[],
        );
        assert_eq!(
            window,
            [
                "--user-data-dir=/p/profile",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-sync",
                "https://x.com/i/flow/login",
            ]
        );
        for flag in CONTROL_FLAGS {
            assert!(
                window.iter().all(|a| !a.starts_with(flag)),
                "{flag} on the sign-in window"
            );
        }
        // Hidden Chrome is the one oxbot controls.
        let hidden = chrome_args(
            profile,
            &Launch::Hidden {
                port: 9222,
                user_agent: Some("Mozilla/5.0 Chrome/156.0.0.0"),
            },
            &[],
        );
        assert!(hidden.contains(&"--remote-debugging-port=9222".to_owned()));
        assert!(hidden.contains(&"--headless=new".to_owned()));
        assert_eq!(hidden[0], "--user-data-dir=/p/profile", "the same profile");
    }

    #[tokio::test]
    async fn a_mac_counts_no_windows_for_a_process_without_any() {
        if !cfg!(target_os = "macos") {
            return;
        }
        // This test's own process shows no window.
        assert_eq!(window_count(std::process::id()).await, Some(0));
    }

    #[test]
    fn only_a_main_chrome_counts_as_a_leftover() {
        assert!(is_main_chrome(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --user-data-dir=/p --headless=new"
        ));
        assert!(!is_main_chrome(
            "/Applications/Google Chrome.app/.../Google Chrome Helper (Renderer) --type=renderer --user-data-dir=/p"
        ));
        assert!(!is_main_chrome(""));
    }

    #[test]
    fn profile_paths_are_escaped_for_pgrep() {
        assert_eq!(
            regex_escape("/Users/me/Library/Application Support/oxbot (1)/x.y"),
            "/Users/me/Library/Application Support/oxbot \\(1\\)/x\\.y"
        );
    }

    #[test]
    fn hidden_chrome_names_itself_like_normal_chrome() {
        let agent = user_agent("Google Chrome 156.0.8078.73 \n").unwrap();
        assert!(agent.contains(" Chrome/156.0.0.0 "), "{agent}");
        assert!(!agent.contains("Headless"));
        assert_eq!(user_agent("Google Chrome"), None);
        assert_eq!(user_agent(""), None);
    }

    #[test]
    fn sites_are_named_by_their_main_domain() {
        assert_eq!(site_of("accounts.google.com"), "google.com");
        assert_eq!(site_of(".google.com"), "google.com");
        assert_eq!(site_of("jayrajan.substack.com"), "substack.com");
        assert_eq!(site_of("www.bbc.co.uk"), "bbc.co.uk");
        assert_eq!(site_of("127.0.0.1"), "127.0.0.1");
        assert_eq!(site_of("localhost"), "localhost");
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn a_site_icon_comes_from_chromes_favicons_file() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("Favicons");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE icon_mapping (page_url TEXT, icon_id INTEGER);
             CREATE TABLE favicon_bitmaps (icon_id INTEGER, width INTEGER, image_data BLOB);
             INSERT INTO icon_mapping VALUES ('https://jay.substack.com/p/1', 1);
             INSERT INTO favicon_bitmaps VALUES (1, 16, X'89504E4701'), (1, 32, X'89504E4702');",
        )
        .unwrap();
        drop(conn);
        let icon = site_icons(&db, &["substack.com"]).pop().flatten().unwrap();
        assert_eq!(
            icon,
            format!("data:image/png;base64,{}", base64(b"\x89PNG\x02"))
        );
        assert_eq!(site_icons(&db, &["google.com"]).pop().flatten(), None);
        assert_eq!(
            site_icons(&tmp.path().join("none"), &["substack.com"])
                .pop()
                .flatten(),
            None
        );
    }
}
