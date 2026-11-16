//! [`Browser`]: the browser every bot of one owner shares, the Camoufox
//! (Firefox) that ships inside oxbot.app. It keeps its profile, so the user
//! signs in to a site once and every bot stays signed in. It runs as a real
//! window minimized to the Dock, under oxbot's control, and comes forward
//! only while the user signs in (automation is paused meanwhile).
//!
//! The browser itself runs in a Node helper oxbot owns over private stdio
//! (see `camoufox`). Bots reach it only through a capability-protected
//! local address that offers the browser tools and nothing else.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::camoufox::Camoufox;
use crate::docfetch;
use crate::netguard::{BlockedRequest, NetMode};
use crate::sealed::{Copied, Ran};
use crate::visits::{VisitSink, Watch};
use crate::{BrowserError, CookieMarks, ElementFacts, Tools};

/// Why practice runs refuse to start: their network guard was built on
/// Chrome's `DevTools` and hasn't been rebuilt for Camoufox yet. Refusing is
/// the safe answer; a practice run without its guard could write to sites.
pub const NO_PRACTICE_RUNS: &str = "Practice runs aren't available with oxbot's browser yet. Run the task for real, with approvals on.";

/// Why sealed script reads refuse: they ran in a network-less Chrome tab
/// that Camoufox has no equivalent of yet.
pub const NO_SEALED_READS: &str =
    "Script reads aren't available with oxbot's browser yet; read the page snapshot instead.";

/// A picture of the sign-in page (see [`Browser::sign_in_frame`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInFrame {
    /// A JPEG, base64.
    pub image: String,
    /// The page's width in CSS pixels.
    pub width: u32,
    /// Its height.
    pub height: u32,
    /// Its address.
    pub url: String,
}

/// An element uid as the browser knows it (see [`Browser::uid_check`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UidCheck {
    /// Not on the page in use any more (it changed since the snapshot, or
    /// another tab is in use): a click on it would fail.
    Gone,
    /// Its role and name as the browser's snapshot recorded them, and
    /// whether the live page still shows it so.
    Live {
        /// e.g. `link`.
        role: String,
        /// e.g. `View Availability by Month`.
        name: String,
        /// The page still shows exactly this element with that role and
        /// name.
        shown: bool,
    },
}

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
    /// Running, minimized, under oxbot's control.
    Ready,
    /// In front of the user for a sign-in; automation is paused.
    SigningIn,
}

/// The shared browser. Use it as `Arc<Browser>`.
pub struct Browser {
    native: Arc<Camoufox>,
    label: String,
    profile_dir: PathBuf,
    status: watch::Sender<BrowserStatus>,
    user: Arc<std::sync::Mutex<Option<String>>>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("label", &self.label)
            .field("profile_dir", &self.profile_dir)
            .finish_non_exhaustive()
    }
}

impl Browser {
    /// A browser for `spec`. Nothing starts until it's needed.
    ///
    /// # Errors
    /// Fails if no free local port can be found for the tool address.
    pub fn new(tools: &Tools, spec: BrowserSpec) -> std::io::Result<Arc<Self>> {
        let status = watch::channel(BrowserStatus::Off).0;
        let native = Camoufox::new(
            tools.config().camoufox.clone(),
            &spec,
            tools.config().env.clone(),
            status.clone(),
            Arc::clone(&tools.courtesy),
        )?;
        Ok(Arc::new(Self {
            native,
            label: spec.label,
            profile_dir: spec.profile_dir,
            status,
            user: Arc::new(std::sync::Mutex::new(None)),
        }))
    }

    /// A practice run's browser. Not available: see [`NO_PRACTICE_RUNS`].
    ///
    /// # Errors
    /// Always [`std::io::ErrorKind::Unsupported`].
    pub fn new_trial(
        _tools: Arc<Tools>,
        _spec: BrowserSpec,
        _mode: NetMode,
    ) -> std::io::Result<Arc<Self>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            NO_PRACTICE_RUNS,
        ))
    }

    /// Counts its visits to websites: each finished [`crate::Visit`] goes to
    /// `sink` (see [`crate::visits`]).
    pub fn count_visits(&self, sink: VisitSink) {
        self.native.count_visits(Watch {
            sink,
            user: Arc::clone(&self.user),
        });
    }

    /// Who is using it from now on (a bot's id; `None`: no one). Visits
    /// started by someone else end.
    pub fn set_user(&self, user: Option<String>) {
        *self
            .user
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = user;
    }

    /// Whether this is a practice run's browser (never, for now).
    #[must_use]
    pub fn is_trial(&self) -> bool {
        false
    }

    /// For a practice run's browser: whether its network guard is on.
    /// Always true for a live one, which is the only kind.
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "callers await it, as they will once Camoufox has its own"
    )]
    pub async fn net_ready(&self, _wait: Duration) -> bool {
        true
    }

    /// For a practice run's browser: the requests its network guard
    /// stopped. Always none.
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "callers await it, as they will once Camoufox has its own"
    )]
    pub async fn blocked_requests(&self) -> (Vec<BlockedRequest>, usize) {
        (Vec::new(), 0)
    }

    /// Where a bot's browser tool reaches this browser: an
    /// `http://127.0.0.1:…` address whose path holds a random secret. The
    /// same for the life of this `Browser`, so a restarted browser is found
    /// again by every bot's tool. Keep it as secret as a password: it lets
    /// the holder drive every signed-in site.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        self.native.endpoint()
    }

    /// The icon saved for `site`. Camoufox's icons aren't read yet: `None`.
    #[must_use]
    pub fn site_icon(&self, site: &str) -> Option<String> {
        self.site_icons(&[site]).pop().flatten()
    }

    /// Like [`Browser::site_icon`] for several sites. One entry per site.
    #[must_use]
    pub fn site_icons(&self, sites: &[&str]) -> Vec<Option<String>> {
        vec![None; sites.len()]
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

    /// Makes sure the browser is running. Restarts it if it exited.
    ///
    /// # Errors
    /// Fails if the bundled browser is missing or doesn't start.
    pub async fn ensure_running(&self) -> Result<(), BrowserError> {
        self.native.ensure().await
    }

    /// Whether the browser runs hidden (no window) for the bot using it
    /// now. Takes effect at its next start or [`Browser::use_mode`].
    pub fn set_hidden(&self, hidden: bool) {
        self.native.set_hidden(hidden);
    }

    /// Starts the browser in the mode last asked for, starting a running
    /// one over (signed in as before, its tabs gone) if it's in the other
    /// mode and no sign-in is open. Returns whether it started over.
    ///
    /// # Errors
    /// Fails if the bundled browser is missing or doesn't start.
    pub async fn use_mode(&self) -> Result<bool, BrowserError> {
        self.native
            .ensure_as(Some(self.native.wants_hidden()))
            .await
    }

    /// Brings the browser forward at `url` so the user can sign in. Bots'
    /// tools are paused until [`Browser::close_window`].
    ///
    /// # Errors
    /// Fails if the browser can't start.
    pub async fn open_window(self: &Arc<Self>, url: &str) -> Result<(), BrowserError> {
        self.native.open(url).await
    }

    /// Ends a sign-in: closes the pages opened for it, minimizes the browser
    /// and lets bots use it again.
    pub async fn close_window(&self) {
        self.native.resume().await;
    }

    /// Whether the browser is in front of the user for a sign-in.
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "callers await it, as they will once Camoufox has its own"
    )]
    pub async fn is_window_open(&self) -> bool {
        *self.status.borrow() == BrowserStatus::SigningIn
    }

    /// Closes every tab and leaves one blank page, so the next bot starts
    /// fresh (it stays signed in). Does nothing if the browser isn't running.
    pub async fn reset_tabs(&self) {
        if self.native.is_running() {
            let _ = self.native.call("reset_tabs", json!({})).await;
        }
    }

    /// Sends downloads to `dir` from now on (the folder of the bot using the
    /// browser). The bot's tool may only touch files in `dir`'s parent.
    pub async fn set_downloads(&self, dir: &Path) {
        if let Err(err) = self.native.call("downloads", json!({ "dir": dir })).await {
            tracing::warn!(browser = self.label, %err, "couldn't set the downloads folder");
        }
    }

    /// A picture of the page at `page_url` (or the one used last), as a
    /// `data:` URL.
    pub async fn screenshot(&self, page_url: Option<&str>) -> Option<String> {
        self.native
            .call("screenshot", json!({ "page_url": page_url }))
            .await
            .ok()
            .and_then(|v| v.as_str().map(|s| format!("data:image/png;base64,{s}")))
    }

    /// Runs `expression` in the page at `page_url` (or the one used last)
    /// and returns its JSON value. For oxbot's own checks only; bots never
    /// run scripts. The page's own scripts share this world.
    pub async fn inspect(&self, page_url: Option<&str>, expression: &str) -> Option<Value> {
        self.native
            .call(
                "inspect",
                json!({ "page_url": page_url, "expression": expression }),
            )
            .await
            .ok()
    }

    /// Copies a page for a sealed read. Not available: see
    /// [`NO_SEALED_READS`].
    ///
    /// # Errors
    /// Always.
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "callers await it, as they will once Camoufox has its own"
    )]
    pub async fn copy_page(&self, _page_url: &str) -> Result<Copied, BrowserError> {
        Err(BrowserError::Browser(NO_SEALED_READS.into()))
    }

    /// Reads `url` from the tab at `page_url`, which must be on `origin`:
    /// one GET with that site's own sign-in (see [`crate::docfetch`]). The
    /// caller decides which addresses may be read.
    ///
    /// # Errors
    /// Why nothing was read, in plain words.
    pub async fn fetch_in_page(
        &self,
        page_url: &str,
        origin: &str,
        url: &str,
    ) -> Result<docfetch::Fetched, String> {
        let expression = docfetch::fetch_expression(
            origin,
            url,
            docfetch::MAX_TEXT_CHARS,
            docfetch::FETCH_LIMIT.as_millis(),
        );
        docfetch::read_fetched(
            &self
                .native
                .call(
                    "inspect",
                    json!({ "page_url": page_url, "expression": expression }),
                )
                .await?,
        )
    }

    /// Runs a bot's script on a sealed copy. Not available: see
    /// [`NO_SEALED_READS`].
    ///
    /// # Errors
    /// Always.
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "callers await it, as they will once Camoufox has its own"
    )]
    pub async fn read_sealed(
        &self,
        _html: &str,
        _url: &str,
        _script: &str,
        _follow: Option<&str>,
    ) -> Result<Ran, String> {
        Err(NO_SEALED_READS.into())
    }

    /// Closes the sealed tab (there never is one).
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "callers await it, as they will once Camoufox has its own"
    )]
    pub async fn close_sealed(&self) {}

    /// The facts of the elements named `name` with `role` in the page at
    /// `page_url` (or the one used last): what oxbot's approval checks need.
    /// At most 8. Empty if oxbot couldn't look.
    pub async fn describe(
        &self,
        page_url: Option<&str>,
        role: &str,
        name: &str,
    ) -> Vec<ElementFacts> {
        self.native
            .call(
                "describe",
                json!({
                    "page_url": page_url,
                    "role": role,
                    "name": name,
                    "function": crate::ELEMENT_FACTS_FN,
                }),
            )
            .await
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(ElementFacts::from_value)
            .collect()
    }

    /// What the browser's own snapshot recorded for element `uid`, and
    /// whether the live page still shows it so. `None` if oxbot couldn't
    /// look.
    pub async fn uid_check(&self, uid: &str) -> Option<UidCheck> {
        let v = self
            .native
            .call("uid_check", json!({ "uid": uid }))
            .await
            .ok()?;
        if v["live"].as_bool() != Some(true) {
            return Some(UidCheck::Gone);
        }
        Some(UidCheck::Live {
            role: v["role"].as_str()?.to_owned(),
            name: v["name"].as_str()?.to_owned(),
            shown: v["shown"].as_bool() == Some(true),
        })
    }

    /// The sign-in page the person is on, as it looks now: a JPEG
    /// (base64), the page's size in CSS pixels, and its address. For a
    /// device that can't see the window.
    ///
    /// # Errors
    /// No sign-in is open, or the browser couldn't take the picture.
    pub async fn sign_in_frame(&self) -> Result<SignInFrame, BrowserError> {
        let v = self
            .native
            .call("signin_frame", json!({}))
            .await
            .map_err(BrowserError::Browser)?;
        let size = |key: &str| u32::try_from(v[key].as_u64().unwrap_or(0)).unwrap_or(0);
        Ok(SignInFrame {
            image: v["image"].as_str().unwrap_or_default().to_owned(),
            width: size("width"),
            height: size("height"),
            url: v["url"].as_str().unwrap_or_default().to_owned(),
        })
    }

    /// The person's tap, typing, key or scroll on the sign-in page
    /// (`{"kind": "tap", "x": 10, "y": 20}`, as the app sends it).
    ///
    /// # Errors
    /// No sign-in is open, or the browser refused it (an unknown key).
    pub async fn sign_in_input(&self, input: Value) -> Result<(), BrowserError> {
        self.native
            .call("signin_input", json!({ "input": input }))
            .await
            .map(|_| ())
            .map_err(BrowserError::Browser)
    }

    /// How many elements in the page at `page_url` have `role` and are
    /// named exactly `name`. `None` if oxbot couldn't look.
    pub async fn count_named(&self, page_url: &str, role: &str, name: &str) -> Option<usize> {
        self.native
            .call(
                "count_named",
                json!({ "page_url": page_url, "role": role, "name": name }),
            )
            .await
            .ok()
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
    }

    /// The address of the page at `page_url` (or the one used last), as the
    /// browser has it. `None` if oxbot couldn't look.
    pub async fn page_url(&self, page_url: Option<&str>) -> Option<String> {
        self.native
            .call("page_url", json!({ "page_url": page_url }))
            .await
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
    }

    /// The sites the shared browser holds cookies for, as main domains
    /// (`accounts.google.com` → `google.com`), sorted. Starts the browser if
    /// it's off.
    ///
    /// # Errors
    /// Fails if the browser can't start or doesn't answer.
    pub async fn cookie_sites(self: &Arc<Self>) -> Result<Vec<String>, BrowserError> {
        Ok(self.cookie_marks().await?.sites())
    }

    /// Which cookies the browser holds, by site (names and keyed hashes of
    /// values, never values), to compare before and after a sign-in.
    /// Starts the browser if it's off.
    ///
    /// # Errors
    /// Fails if the browser can't start or doesn't answer.
    pub async fn cookie_marks(self: &Arc<Self>) -> Result<CookieMarks, BrowserError> {
        Ok(CookieMarks::from_cookies(&self.cookies().await?))
    }

    /// Signs the shared browser out of one site: deletes the cookies of
    /// `site` and its subdomains, and what its open pages stored. Other
    /// sites stay signed in. Returns how many cookies went. Starts the
    /// browser if it's off.
    ///
    /// # Errors
    /// Fails if the browser can't start or refuses.
    pub async fn sign_out_site(self: &Arc<Self>, site: &str) -> Result<usize, BrowserError> {
        let gone = self
            .native
            .call("sign_out_site", json!({ "site": site_of(site) }))
            .await
            .map_err(BrowserError::Browser)?;
        Ok(gone
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0))
    }

    /// The cookies of `site` (a main domain) and its subdomains, as
    /// [`Browser::add_cookies`] takes them: to share a sign-in with
    /// another browser. Starts the browser if it's off.
    ///
    /// # Errors
    /// Fails if the browser can't start or doesn't answer.
    pub async fn site_cookies(self: &Arc<Self>, site: &str) -> Result<Vec<Value>, BrowserError> {
        Ok(crate::cookies::site_cookie_params(
            &self.cookies().await?,
            site,
        ))
    }

    /// Adds `cookies` (from [`Browser::site_cookies`]) to this browser,
    /// replacing ones of the same name, domain and path. Returns how many.
    /// Starts the browser if it's off.
    ///
    /// # Errors
    /// Fails if the browser can't start or refuses them.
    pub async fn add_cookies(self: &Arc<Self>, cookies: Vec<Value>) -> Result<usize, BrowserError> {
        if cookies.is_empty() {
            return Ok(0);
        }
        let added = self
            .native
            .call("add_cookies", json!({ "cookies": cookies }))
            .await
            .map_err(BrowserError::Browser)?;
        Ok(added
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0))
    }

    /// Every cookie in the shared browser.
    async fn cookies(self: &Arc<Self>) -> Result<Vec<Value>, BrowserError> {
        self.native
            .call("cookies", json!({}))
            .await
            .map(|v| v.as_array().cloned().unwrap_or_default())
            .map_err(BrowserError::Browser)
    }

    /// Stops the browser, letting it save its profile. Sign-ins stay.
    pub async fn shutdown(&self) {
        self.native.stop().await;
    }

    /// Stops the browser and deletes the profile, signing out of every site.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CamoufoxTool, ToolsConfig};

    fn spec(root: &Path) -> BrowserSpec {
        BrowserSpec {
            label: "test".into(),
            profile_dir: root.join("profile"),
            downloads: root.join("downloads"),
        }
    }

    #[tokio::test]
    async fn a_missing_browser_is_a_clear_error() {
        let tmp = tempfile::tempdir().unwrap();
        let browser = Browser::new(
            &Tools::new(ToolsConfig::in_home(tmp.path())),
            spec(tmp.path()),
        )
        .unwrap();
        let err = browser.ensure_running().await.unwrap_err();
        assert!(matches!(err, BrowserError::Missing), "{err}");
        assert!(
            browser
                .open_window("https://accounts.google.com")
                .await
                .is_err()
        );
        browser.shutdown().await;
        assert_eq!(*browser.status().borrow(), BrowserStatus::Off);
    }

    /// A stand-in browser helper (`sh` for Node): it notes whether each
    /// start was hidden, and answers every call.
    fn stand_in(root: &Path) -> CamoufoxTool {
        let bundle = root.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        for file in ["bundle.json", "runtime.mjs", "mcp.mjs"] {
            std::fs::write(bundle.join(file), "").unwrap();
        }
        std::fs::write(
            bundle.join("service.mjs"),
            r#"echo "$6" >> "$(dirname "$0")/starts"
echo '{"event":"ready"}'
while read -r line; do
  case "$line" in *'"shutdown"'*) exit 0;; esac
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] && echo "{\"id\":$id,\"result\":true}"
done
"#,
        )
        .unwrap();
        CamoufoxTool {
            node: "/bin/sh".into(),
            bundle,
            headless: false,
            proxy: None,
        }
    }

    #[tokio::test]
    async fn a_hidden_browser_comes_forward_for_a_sign_in_and_hides_again() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = stand_in(tmp.path());
        let starts = || std::fs::read_to_string(tool.bundle.join("starts")).unwrap_or_default();
        let mut config = ToolsConfig::in_home(tmp.path());
        config.camoufox = Some(tool.clone());
        let browser = Browser::new(&Tools::new(config), spec(tmp.path())).unwrap();
        browser.set_hidden(true);
        browser.ensure_running().await.unwrap();
        assert!(!browser.use_mode().await.unwrap(), "already hidden");
        assert_eq!(starts(), "true\n");
        // A sign-in (or robot check) brings it forward as a window.
        browser
            .open_window("https://example.test/login")
            .await
            .unwrap();
        assert_eq!(starts(), "true\nfalse\n");
        assert_eq!(*browser.status().borrow(), BrowserStatus::SigningIn);
        // Never hidden away mid sign-in.
        assert!(!browser.use_mode().await.unwrap());
        browser.close_window().await;
        // The next turn hides it again; the run it replaced doesn't mark it off.
        assert!(browser.use_mode().await.unwrap());
        assert_eq!(starts(), "true\nfalse\ntrue\n");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(*browser.status().borrow(), BrowserStatus::Ready);
        // A bot that browses with a window gets one.
        browser.set_hidden(false);
        assert!(browser.use_mode().await.unwrap());
        assert_eq!(starts(), "true\nfalse\ntrue\nfalse\n");
        browser.shutdown().await;
        assert_eq!(*browser.status().borrow(), BrowserStatus::Off);
    }

    #[tokio::test]
    async fn signing_out_deletes_the_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("profile");
        std::fs::create_dir_all(profile.join("storage")).unwrap();
        let browser = Browser::new(
            &Tools::new(ToolsConfig::in_home(tmp.path())),
            spec(tmp.path()),
        )
        .unwrap();
        browser.shutdown().await;
        assert!(profile.exists(), "shutting down keeps sign-ins");
        browser.sign_out().await.unwrap();
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn practice_runs_and_sealed_reads_refuse() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = Tools::new(ToolsConfig::in_home(tmp.path()));
        let err =
            Browser::new_trial(Arc::clone(&tools), spec(tmp.path()), NetMode::Strict).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        let browser = Browser::new(&tools, spec(tmp.path())).unwrap();
        assert!(browser.copy_page("https://example.com/").await.is_err());
        assert!(browser.read_sealed("", "", "", None).await.is_err());
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
}
