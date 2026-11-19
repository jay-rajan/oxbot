//! oxbot's own updates, on the home computer.
//!
//! - At start (after a minute) and every 6 hours, or when Settings asks, it
//!   reads the channel's feed on GitHub Releases.
//! - A newer version is downloaded in the background while bots keep
//!   working. The app's signature is checked with the update key's public
//!   half (built into this app), and must name the version the feed
//!   announced, so an old release can't pass for a new one. Then every
//!   pack the new version runs with is put in oxbot's tools folder
//!   ([`oxbot::update::gather`]).
//! - It installs at night (2–5 am), or sooner when asked ("Restart to
//!   update"), but only while the home is quiet: no bot working, nobody's
//!   answer or sign-in awaited, no schedule about to run. It stops oxbot
//!   cleanly, replaces the app and starts it again. A database change in
//!   the new version copies the database first (store migrations).
//! - A build without an update key (a local build) has no updates.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrono::Timelike as _;
use oxbot::update::{Fetched, feed_url, notes_url};
use oxbot_core::hub::{Hub, OxbotCall};
use oxbot_protocol::{AppSettings, AppUpdate, ServerMsg, UpdateState};
use tauri::{AppHandle, Manager as _};
use tauri_plugin_updater::UpdaterExt as _;

/// The channel this build follows.
const CHANNEL: &str = "beta";
/// How often it asks for a newer version.
const EVERY: Duration = Duration::from_hours(6);
/// How long after start it first asks, so oxbot's own start comes first.
const FIRST: Duration = Duration::from_secs(60);
/// Night hours (local) when a waiting update installs on its own.
const NIGHT: std::ops::Range<u32> = 2..5;

/// The app's updater, kept in Tauri's state.
pub struct Updates {
    app: AppHandle,
    inner: Mutex<Inner>,
    wake: tokio::sync::Notify,
}

struct Inner {
    status: AppUpdate,
    ready: Option<Ready>,
    /// Settings asked for a check.
    asked: bool,
    /// "Restart to update": install as soon as the home is quiet.
    soon: bool,
    last_check: Option<Instant>,
}

/// A downloaded, checked update with all its packs in place.
struct Ready {
    update: tauri_plugin_updater::Update,
    bytes: Vec<u8>,
}

impl Updates {
    /// The updater, if this build has an update key; it starts asking
    /// after a minute.
    pub fn start(app: &AppHandle) -> Option<Arc<Self>> {
        if !has_key(app) {
            tracing::info!("this build has no update key: no automatic updates");
            return None;
        }
        let me = Arc::new(Self {
            app: app.clone(),
            inner: Mutex::new(Inner {
                status: AppUpdate {
                    state: UpdateState::UpToDate,
                    version: None,
                    notes_url: None,
                    message: None,
                    checked_ms: None,
                },
                ready: None,
                asked: false,
                soon: false,
                last_check: None,
            }),
            wake: tokio::sync::Notify::new(),
        });
        let run = Arc::clone(&me);
        tauri::async_runtime::spawn(async move { run.run().await });
        Some(me)
    }

    /// Where it stands, for Settings.
    pub fn status(&self) -> AppUpdate {
        self.lock().status.clone()
    }

    /// Settings' "Check for updates".
    pub fn ask(&self) {
        self.lock().asked = true;
        self.wake.notify_one();
    }

    /// Settings' "Restart to update".
    ///
    /// # Errors
    /// No update is ready.
    pub fn install_soon(&self) -> Result<(), String> {
        {
            let mut inner = self.lock();
            if inner.ready.is_none() {
                return Err("No update is ready yet.".to_owned());
            }
            inner.soon = true;
        }
        self.wake.notify_one();
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn run(self: Arc<Self>) {
        // A debug build's update test: ask at once, install when ready.
        let now = dev_override("OXBOT_UPDATE_NOW").is_some();
        tokio::time::sleep(if now { Duration::from_secs(5) } else { FIRST }).await;
        loop {
            let due = {
                let inner = self.lock();
                inner.asked || inner.last_check.is_none_or(|t| t.elapsed() >= EVERY)
            };
            if due {
                self.check().await;
            }
            self.maybe_install().await;
            // Once a minute: the night window and a quiet home come and go.
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(60)) => {}
                () = self.wake.notified() => {}
            }
        }
    }

    /// Asks the feed, and fetches a newer version if there is one.
    async fn check(&self) {
        let ready_version = {
            let mut inner = self.lock();
            inner.asked = false;
            inner.last_check = Some(Instant::now());
            inner.ready.as_ref().map(|r| r.update.version.clone())
        };
        if ready_version.is_none() {
            self.set(UpdateState::Checking, None, None).await;
        }
        let started = Instant::now();
        let ts_ms = oxbot::runtime::now_ms();
        let found = self.find().await;
        self.log(OxbotCall {
            ts_ms,
            host: "github.com".to_owned(),
            label: "checked for a newer oxbot".to_owned(),
            ms: elapsed_ms(started),
            outcome: found
                .as_ref()
                .map_or_else(Clone::clone, |_| "ok".to_owned()),
            ..OxbotCall::default()
        })
        .await;
        self.lock().status.checked_ms = Some(ts_ms);
        let update = match found {
            Ok(Some(update)) => update,
            Ok(None) => {
                if ready_version.is_none() {
                    self.set(UpdateState::UpToDate, None, None).await;
                }
                return;
            }
            Err(why) => {
                tracing::warn!(%why, "couldn't check for an update");
                if ready_version.is_none() {
                    self.set(UpdateState::Failed, None, Some(why)).await;
                }
                return;
            }
        };
        if ready_version.as_deref() == Some(update.version.as_str()) {
            return;
        }
        let version = update.version.clone();
        self.set(
            UpdateState::Downloading,
            Some(&version),
            Some("Downloading in the background; bots keep working.".to_owned()),
        )
        .await;
        match self.fetch(&update).await {
            Ok(bytes) => {
                {
                    let mut inner = self.lock();
                    inner.ready = Some(Ready { update, bytes });
                    inner.soon |= dev_override("OXBOT_UPDATE_NOW").is_some();
                }
                self.set(UpdateState::Ready, Some(&version), Some(waiting_line(None)))
                    .await;
                tracing::info!(%version, "an update is ready");
            }
            Err(why) => {
                tracing::warn!(%version, %why, "couldn't download the update");
                self.set(UpdateState::Failed, Some(&version), Some(why))
                    .await;
            }
        }
    }

    /// The feed's newer version, if any.
    async fn find(&self) -> Result<Option<tauri_plugin_updater::Update>, String> {
        let url = feed().parse().map_err(|e| format!("{e}"))?;
        let mut builder = self.app.updater_builder();
        if let Some(key) = dev_override("OXBOT_UPDATE_PUBKEY") {
            builder = builder.pubkey(key);
        }
        let updater = builder
            .endpoints(vec![url])
            .and_then(tauri_plugin_updater::UpdaterBuilder::build)
            .map_err(|e| e.to_string())?;
        updater.check().await.map_err(|e| e.to_string())
    }

    /// Downloads and checks the app, then puts its packs in place.
    async fn fetch(&self, update: &tauri_plugin_updater::Update) -> Result<Vec<u8>, String> {
        let started = Instant::now();
        let ts_ms = oxbot::runtime::now_ms();
        let got = update.download(|_, _| {}, || {}).await;
        self.log(OxbotCall {
            ts_ms,
            host: "github.com".to_owned(),
            label: format!("downloaded oxbot {}", update.version),
            bytes_down: got.as_ref().map_or(0, |b| b.len() as u64),
            ms: elapsed_ms(started),
            outcome: got
                .as_ref()
                .map_or_else(ToString::to_string, |_| "ok".to_owned()),
            ..OxbotCall::default()
        })
        .await;
        // The signature (and the version it names) is checked here.
        let bytes = got.map_err(|e| format!("The update didn't check out: {e}"))?;
        let data = self
            .data_dir()
            .await
            .ok_or_else(|| "oxbot isn't running".to_owned())?;
        let updates = data.join("updates");
        tokio::fs::create_dir_all(&updates)
            .await
            .map_err(|e| e.to_string())?;
        let archive = updates.join(format!("oxbot-{}.app.tar.gz", update.version));
        tokio::fs::write(&archive, &bytes)
            .await
            .map_err(|e| e.to_string())?;
        let packs = oxbot::update::packs_in_update(&archive).await;
        let _ = tokio::fs::remove_file(&archive).await;
        let packs = packs?;
        let hub = self.hub().await;
        let on_fetch = move |f: Fetched| {
            if let Some(hub) = &hub {
                hub.note_oxbot_call(OxbotCall {
                    ts_ms: f.ts_ms,
                    host: "github.com".to_owned(),
                    label: format!("downloaded {} for an oxbot update", f.pack),
                    bytes_down: f.bytes,
                    ms: f.ms,
                    outcome: f.outcome,
                    ..OxbotCall::default()
                });
            }
        };
        oxbot::update::gather(
            &packs,
            &data.join("tools"),
            &resources().join("tools"),
            &on_fetch,
        )
        .await?;
        Ok(bytes)
    }

    /// Installs a ready update if it's time and the home is quiet.
    async fn maybe_install(&self) {
        let (version, soon) = {
            let inner = self.lock();
            let Some(ready) = &inner.ready else { return };
            (ready.update.version.clone(), inner.soon)
        };
        let night = NIGHT.contains(&chrono::Local::now().hour());
        if !soon && !night {
            return;
        }
        let Some(hub) = self.hub().await else { return };
        if let Some(why) = hub.restart_waits_for(oxbot::runtime::now_ms()).await {
            if soon {
                self.set(
                    UpdateState::Ready,
                    Some(&version),
                    Some(waiting_line(Some(&why))),
                )
                .await;
            }
            return;
        }
        self.install(&version).await;
    }

    /// Stops oxbot, replaces the app and starts it again.
    async fn install(&self, version: &str) {
        let Some(ready) = self.lock().ready.take() else {
            return;
        };
        self.set(
            UpdateState::Installing,
            Some(version),
            Some("oxbot is restarting with the new version.".to_owned()),
        )
        .await;
        tracing::info!(%version, "installing the update");
        // A moment for open pages to show it.
        tokio::time::sleep(Duration::from_secs(1)).await;
        let state = self.app.state::<crate::desktop::Oxbot>();
        if let Some(server) = state.server.lock().await.take() {
            server.stop().await;
        }
        if let Err(err) = ready.update.install(&ready.bytes) {
            // Start again as we were; the next check tries again.
            tracing::error!(%err, "couldn't install the update");
        }
        self.app.restart();
    }

    async fn set(&self, state: UpdateState, version: Option<&str>, message: Option<String>) {
        {
            let mut inner = self.lock();
            inner.status.state = state;
            inner.status.version = version.map(ToOwned::to_owned);
            inner.status.notes_url = version.map(notes_url);
            inner.status.message = message;
        }
        // Each page gets its own Settings again.
        if let Some(hub) = self.hub().await {
            hub.announce(ServerMsg::Settings(AppSettings::default()));
        }
    }

    async fn log(&self, call: OxbotCall) {
        if let Some(hub) = self.hub().await {
            hub.note_oxbot_call(call);
        }
    }

    async fn hub(&self) -> Option<Arc<Hub>> {
        let state = self.app.try_state::<crate::desktop::Oxbot>()?;
        let server = state.server.lock().await;
        server.as_ref().map(|s| Arc::clone(&s.hub))
    }

    async fn data_dir(&self) -> Option<std::path::PathBuf> {
        let state = self.app.try_state::<crate::desktop::Oxbot>()?;
        let server = state.server.lock().await;
        server.as_ref().map(|s| s.home.root().to_owned())
    }
}

/// What a ready update says while it waits.
fn waiting_line(why: Option<&str>) -> String {
    match why {
        Some(why) => format!("Installs as soon as it can: {why}."),
        None => "Installs tonight while no bot is busy, or restart now.".to_owned(),
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Whether this build can check updates: an update key in its config (or,
/// in a debug build, `OXBOT_UPDATE_PUBKEY`).
fn has_key(app: &AppHandle) -> bool {
    let configured = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .is_some_and(|k| !k.trim().is_empty());
    configured || dev_override("OXBOT_UPDATE_PUBKEY").is_some()
}

/// The feed this app reads (a debug build may point it elsewhere, for tests).
fn feed() -> String {
    dev_override("OXBOT_UPDATE_FEED").unwrap_or_else(|| feed_url(CHANNEL))
}

/// A test setting, honoured only in debug builds: a release can't be
/// pointed at another feed or key.
fn dev_override(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    } else {
        None
    }
}

/// The app's Resources folder.
fn resources() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("Resources")))
        .unwrap_or_default()
}
