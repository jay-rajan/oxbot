//! Each owner's Chrome: making it when first needed, lending it to one bot
//! at a time for a turn, signing it out, and telling clients when it
//! changes state.

use std::sync::Arc;
use std::time::Instant;

use oxbot_browser::{Browser, BrowserSpec, BrowserStatus};
use oxbot_protocol::{BrowserState, ServerMsg};
use tokio::sync::Mutex;

use super::{Hub, HubError, Runtime, Shared, lock};
use crate::bot::Bot;
use crate::id::BotId;
use crate::store::HOME;

impl Hub {
    /// What the home browser is doing.
    #[must_use]
    pub fn browser_state(&self) -> BrowserState {
        self.owner_state(HOME)
    }

    /// What `owner`'s browser is doing (`Off` until it's first needed).
    pub(super) fn owner_state(&self, owner: &str) -> BrowserState {
        if self.config.browser.is_none() {
            return BrowserState::Disabled;
        }
        lock(&self.browsers)
            .get(owner)
            .map_or(BrowserState::Off, |s| {
                browser_state(*s.browser.status().borrow())
            })
    }

    /// `owner`'s browser ([`HOME`] or a person's name), made the first time
    /// it's needed. `None` if bots get no browser.
    pub(super) fn browser_for(&self, owner: &str) -> Option<Arc<Shared>> {
        let tools = self.config.browser.clone()?;
        let mut browsers = lock(&self.browsers);
        if let Some(shared) = browsers.get(owner) {
            return Some(Arc::clone(shared));
        }
        let home_profile = tools.config().profile_dir.clone();
        let (label, profile_dir) = if owner == HOME {
            ("home".to_owned(), home_profile)
        } else {
            let folder = person_folder(owner);
            let root = home_profile
                .parent()
                .map_or_else(|| home_profile.clone(), std::path::Path::to_path_buf);
            (
                folder.clone(),
                root.join("people").join(folder).join("profile"),
            )
        };
        let spec = BrowserSpec {
            label,
            profile_dir,
            downloads: self.config.bots_dir.clone(),
        };
        let browser = match Browser::new(tools, spec) {
            Ok(browser) => browser,
            Err(err) => {
                tracing::error!(%err, "no local port for Chrome; bots get no browser");
                return None;
            }
        };
        let shared = Arc::new(Shared {
            owner: owner.to_owned(),
            browser,
            lease: Arc::new(Mutex::new(())),
            holder: std::sync::Mutex::new(None),
            last: std::sync::Mutex::new(None),
            last_used: std::sync::Mutex::new(Instant::now()),
            adding_site: std::sync::Mutex::new(None),
            sites_checked: std::sync::atomic::AtomicBool::new(false),
        });
        browsers.insert(owner.to_owned(), Arc::clone(&shared));
        drop(browsers);
        tokio::spawn(watch_browser(self.me.clone(), Arc::clone(&shared)));
        Some(shared)
    }

    /// The browser `bot` uses: its creator's own if it's private, else home.
    pub(super) fn browser_of(&self, bot: &Bot) -> Option<Arc<Shared>> {
        self.browser_for(&bot.browser_owner()?)
    }

    /// Every browser made so far.
    pub(super) fn all_browsers(&self) -> Vec<Arc<Shared>> {
        lock(&self.browsers).values().cloned().collect()
    }

    /// Gives `bot` its browser for the rest of its turn, waiting if another
    /// bot on the same browser has it. Returns `false` if the bot's turn
    /// ended while it waited.
    pub(super) async fn take_browser(&self, bot: &BotId) -> Result<bool, HubError> {
        let (shared, held) = {
            let runtimes = self.runtimes.lock().await;
            let Some(runtime) = runtimes.get(bot) else {
                return Ok(true);
            };
            let Some(shared) = runtime.browser.clone() else {
                return Ok(true);
            };
            (shared, runtime.lease.is_some())
        };
        if held {
            *lock(&shared.last_used) = Instant::now();
            Self::browser_running(&shared).await;
            return Ok(true);
        }
        let guard = if let Ok(guard) = Arc::clone(&shared.lease).try_lock_owned() {
            guard
        } else {
            let holder = lock(&shared.holder)
                .clone()
                .map_or_else(|| "another bot".to_owned(), |id| self.bot_name(&id));
            let _ = self.updates.send(ServerMsg::Activity {
                bot: bot.to_string(),
                text: format!("Waiting for the browser ({holder} is using it)…"),
            });
            Arc::clone(&shared.lease).lock_owned().await
        };
        {
            let mut runtimes = self.runtimes.lock().await;
            let Some(runtime) = runtimes.get_mut(bot).filter(|r| r.busy) else {
                return Ok(false);
            };
            runtime.lease = Some(guard);
            let previous = lock(&shared.last).replace(bot.clone());
            // Another bot used the browser since: what this bot read is stale.
            if previous.as_ref().is_some_and(|p| p != bot) {
                runtime.page = None;
                runtime.elements.clear();
            }
            *lock(&shared.holder) = Some(bot.clone());
            *lock(&shared.last_used) = Instant::now();
            if previous.as_ref() == Some(bot) {
                drop(runtimes);
                Self::browser_running(&shared).await;
                return self.downloads_for(&shared, bot).await.map(|()| true);
            }
        }
        Self::browser_running(&shared).await;
        shared.browser.reset_tabs().await;
        self.downloads_for(&shared, bot).await?;
        tracing::info!(%bot, browser = shared.owner, "has the browser");
        Ok(true)
    }

    /// Starts Chrome if it isn't running. A failure is only logged: the
    /// browser tool then tells the bot it can't connect.
    async fn browser_running(shared: &Shared) {
        if *shared.browser.status().borrow() == BrowserStatus::Off
            && let Err(err) = shared.browser.ensure_running().await
        {
            tracing::warn!(%err, "couldn't start Chrome");
        }
    }

    async fn downloads_for(&self, shared: &Shared, bot: &BotId) -> Result<(), HubError> {
        let dir = self.bot(bot.as_str())?.work_dir().join("downloads");
        shared.browser.set_downloads(&dir).await;
        Ok(())
    }

    /// Lets go of the browser if `runtime` holds it.
    pub(super) fn release_browser(bot: &BotId, runtime: &mut Runtime) {
        if runtime.lease.take().is_some()
            && let Some(shared) = &runtime.browser
        {
            let mut holder = lock(&shared.holder);
            if holder.as_ref() == Some(bot) {
                *holder = None;
            }
            *lock(&shared.last_used) = Instant::now();
        }
    }

    /// `owner`'s browser, if no bot is using it right now.
    pub(super) fn idle_browser(&self, owner: &str) -> Result<Arc<Shared>, HubError> {
        let shared = self.browser_for(owner).ok_or(HubError::NoBrowser)?;
        if let Some(holder) = lock(&shared.holder).clone() {
            return Err(HubError::BrowserBusy(self.bot_name(&holder)));
        }
        Ok(shared)
    }

    pub(super) fn busy_error(&self, shared: &Shared) -> HubError {
        let holder = lock(&shared.holder)
            .clone()
            .map_or_else(|| "A bot".to_owned(), |id| self.bot_name(&id));
        HubError::BrowserBusy(holder)
    }

    /// Signs `owner`'s browser ([`HOME`] or a person) out of every site by
    /// deleting its profile.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] if a bot is using it, or the profile can't
    /// be deleted.
    pub async fn sign_out_all(&self, owner: &str) -> Result<(), HubError> {
        let shared = self.browser_for(owner).ok_or(HubError::NoBrowser)?;
        let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned() else {
            return Err(self.busy_error(&shared));
        };
        shared.browser.sign_out().await?;
        lock(&shared.last).take();
        for site in self.store.sites(owner).unwrap_or_default() {
            let _ = self.store.remove_site(owner, &site.site);
        }
        self.announce_sites(&shared);
        self.notice_for(
            shared.owner_label(),
            "Signed out of every site. Bots will ask you to sign in again where they need to.",
        );
        Ok(())
    }
}

pub(super) fn browser_state(status: BrowserStatus) -> BrowserState {
    match status {
        BrowserStatus::Off => BrowserState::Off,
        BrowserStatus::Ready => BrowserState::Ready,
        BrowserStatus::SigningIn => BrowserState::SigningIn,
    }
}

/// Tells clients when a browser changes state, and counts a closed
/// sign-in window as "signed in".
async fn watch_browser(hub: std::sync::Weak<Hub>, shared: Arc<Shared>) {
    let mut status = shared.browser.status();
    let shared = Arc::downgrade(&shared);
    let mut before = *status.borrow();
    while status.changed().await.is_ok() {
        let now = *status.borrow_and_update();
        let Some(hub) = hub.upgrade() else { return };
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if before == BrowserStatus::SigningIn && now != BrowserStatus::SigningIn {
            hub.sign_in_window_closed(&shared);
        }
        before = now;
        hub.announce_bots().await;
    }
}

/// The folder name for a person's own browser: their name made safe for a
/// folder, plus a short hash of the exact name, so `Sam` and `sam` never
/// share sign-ins.
pub(super) fn person_folder(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "person" } else { safe };
    // FNV-1a: stable across runs and Rust versions, unlike `DefaultHasher`.
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!(
        "{}-{:08x}",
        safe.chars().take(32).collect::<String>(),
        hash >> 32
    )
}
