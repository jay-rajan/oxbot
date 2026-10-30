//! [`Browser`]: the Chrome every bot shares. It keeps its profile, so the
//! user signs in to a site once and every bot stays signed in. It runs
//! hidden, and comes up as a normal window only while the user signs in.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, watch};

use crate::guard::Guard;
use crate::{BrowserError, Tools, http};

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
    Hidden(Child),
    Window(Child),
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
        }))
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
        let mut child = self.spawn_chrome(None)?;
        if let Err(err) = self.wait_until_ready().await {
            let _ = child.kill().await;
            return Err(err);
        }
        self.start_guard().await;
        *state = State::Hidden(child);
        self.status.send_replace(BrowserStatus::Ready);
        Ok(())
    }

    /// Shows the browser as a normal window at `url` so the user can sign in,
    /// in place of the hidden Chrome and on the same port. If the window is
    /// already open, `url` opens in a new tab. When the user closes the
    /// window (or its last tab), hidden Chrome starts again straight away.
    ///
    /// # Errors
    /// Fails if Chrome is missing or doesn't start.
    pub async fn open_window(self: &Arc<Self>, url: &str) -> Result<(), BrowserError> {
        let mut state = self.state.lock().await;
        if let State::Window(child) = &mut *state
            && matches!(child.try_wait(), Ok(None))
        {
            drop(state);
            let path = format!("/json/new?{}", http::encode_url(url));
            if !http::put(self.port, &path).await {
                tracing::warn!(url, "couldn't open a tab in the sign-in window");
            }
            return Ok(());
        }
        if let Some(child) = state.take() {
            self.quit(child).await;
        }
        let child = match self.spawn_chrome(Some(url)) {
            Ok(child) => child,
            Err(err) => {
                self.status.send_replace(BrowserStatus::Off);
                return Err(err);
            }
        };
        *state = State::Window(child);
        drop(state);
        self.status.send_replace(BrowserStatus::SigningIn);
        if self.wait_until_ready().await.is_ok() {
            self.start_guard().await;
        }
        tokio::spawn(watch_window(Arc::downgrade(self)));
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

    /// Runs `expression` (JavaScript) in the page at `page_url` (or the one
    /// used last) and returns its JSON value. For oxbot's own checks only;
    /// bots never run scripts.
    pub async fn inspect(&self, page_url: Option<&str>, expression: &str) -> Option<Value> {
        let guard = self.guard.lock().await;
        guard.as_ref()?.evaluate(page_url, expression).await
    }

    /// The sites the shared browser holds cookies for, as main domains
    /// (`accounts.google.com` → `google.com`), sorted. Starts hidden Chrome
    /// if it's off.
    ///
    /// # Errors
    /// Fails if Chrome can't start or doesn't answer.
    pub async fn cookie_sites(self: &Arc<Self>) -> Result<Vec<String>, BrowserError> {
        let cookies = self.cookies().await?;
        let mut sites: Vec<String> = cookies
            .iter()
            .map(|c| site_of(c.get("domain").and_then(Value::as_str).unwrap_or("")))
            .filter(|s| !s.is_empty())
            .collect();
        sites.sort();
        sites.dedup();
        Ok(sites)
    }

    /// Signs the shared browser out of one site: deletes the cookies of
    /// `site` and its subdomains, and the data the site stored (local
    /// storage, databases, caches). Other sites stay signed in. Returns how
    /// many cookies went. Starts hidden Chrome if it's off.
    ///
    /// # Errors
    /// Fails if Chrome can't start or refuses.
    pub async fn forget_site(self: &Arc<Self>, site: &str) -> Result<usize, BrowserError> {
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
        tracing::info!(
            browser = self.label,
            site,
            cookies = gone,
            "signed out of one site"
        );
        Ok(gone)
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

    async fn start_guard(&self) {
        let downloads = self
            .downloads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Err(err) = tokio::fs::create_dir_all(&downloads).await {
            tracing::warn!(%err, "couldn't create the downloads folder");
        }
        match Guard::start(self.port, downloads).await {
            Ok(guard) => *self.guard.lock().await = Some(guard),
            Err(err) => {
                tracing::warn!(browser = self.label, %err, "browser guard didn't start (no screenshots or page checks)");
            }
        }
    }

    /// Open tabs, or `None` if Chrome doesn't answer.
    async fn page_count(&self) -> Option<usize> {
        http::get(self.port, "/json/list")
            .await
            .map(|body| http::page_ids(&body).len())
    }

    /// Stops a Chrome left on this profile by an oxbot that didn't shut down
    /// cleanly; otherwise the new one would find the profile in use.
    async fn kill_leftover(&self) {
        let pattern = format!(
            "--user-data-dir={}( |$)",
            regex_escape(&self.profile_dir.display().to_string())
        );
        let Ok(found) = Command::new("pgrep")
            .args(["-f", "--", &pattern])
            .output()
            .await
        else {
            return;
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
        if stopped {
            tracing::warn!(
                browser = self.label,
                "stopped a leftover Chrome on the shared profile"
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Starts Chrome: hidden on a blank page, or as a window at `url`.
    fn spawn_chrome(&self, window_url: Option<&str>) -> Result<Child, BrowserError> {
        let chrome = &self.tools.config().chrome;
        if !chrome.exists() {
            return Err(BrowserError::ChromeMissing(chrome.clone()));
        }
        let mut cmd = Command::new(chrome);
        cmd.arg(format!("--user-data-dir={}", self.profile_dir.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-sync",
                "--remote-debugging-address=127.0.0.1",
            ])
            .arg(format!("--remote-debugging-port={}", self.port));
        if let Some(url) = window_url {
            if self.tools.config().hidden_windows {
                cmd.arg("--headless=new");
            }
            cmd.arg(url);
        } else {
            // Hidden Chrome calls itself "HeadlessChrome", which many sites
            // block. It sends the same name as the user's normal Chrome.
            if let Some(agent) = self.user_agent.get_or_init(|| user_agent_for(chrome)) {
                cmd.arg(format!("--user-agent={agent}"));
            }
            cmd.args(["--headless=new", "about:blank"]);
        }
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
            hidden = window_url.is_none() || self.tools.config().hidden_windows,
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

/// Watches the sign-in window. When the user closes it (or its last tab),
/// oxbot quits it and starts hidden Chrome again on the same port at once,
/// so a bot in the middle of a task carries on.
async fn watch_window(browser: std::sync::Weak<Browser>) {
    let mut empty_checks = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Some(this) = browser.upgrade() else {
            return;
        };
        let exited = {
            let mut state = this.state.lock().await;
            match &mut *state {
                State::Window(child) => !matches!(child.try_wait(), Ok(None)),
                _ => return,
            }
        };
        if !exited {
            empty_checks = if this.page_count().await == Some(0) {
                empty_checks + 1
            } else {
                0
            };
            if empty_checks < 2 {
                continue;
            }
        }
        let mut state = this.state.lock().await;
        if matches!(*state, State::Window(_)) {
            this.back_to_hidden(&mut state).await;
        }
        return;
    }
}

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
