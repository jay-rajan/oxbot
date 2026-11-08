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
use crate::id::{BotId, BrowserOwner};
#[cfg(test)]
pub(super) use crate::store::person_folder;

/// What anyone but the owner hears when they ask for a browser of their own.
pub(super) const NO_OWN_BROWSER: &str = "Only the owner of the home computer has private sign-ins. \
     Your bots use the home browser, which everyone shares.";

impl Hub {
    /// What the home browser is doing.
    #[must_use]
    pub fn browser_state(&self) -> BrowserState {
        self.owner_state(&BrowserOwner::Home)
    }

    /// What `owner`'s browser is doing (`Off` until it's first needed).
    pub(super) fn owner_state(&self, owner: &BrowserOwner) -> BrowserState {
        if self.config.browser.is_none() {
            return BrowserState::Disabled;
        }
        lock(&self.browsers)
            .get(owner)
            .map_or(BrowserState::Off, |s| {
                browser_state(*s.browser.status().borrow())
            })
    }

    /// `owner`'s browser, made the first time it's needed. `None` if bots
    /// get no browser (or the person is unknown).
    pub(super) fn browser_for(&self, owner: &BrowserOwner) -> Option<Arc<Shared>> {
        let tools = self.config.browser.clone()?;
        // Only the owner has a browser of their own; anyone else's folder
        // stays on disk, unused.
        if let BrowserOwner::Person(id) = owner
            && !self.is_farm_person(id.as_str())
        {
            tracing::debug!(person = %id, "only the owner has a browser of their own");
            return None;
        }
        let mut browsers = lock(&self.browsers);
        if let Some(shared) = browsers.get(owner) {
            return Some(Arc::clone(shared));
        }
        let home_profile = tools.config().profile_dir.clone();
        let (label, profile_dir) = match owner {
            BrowserOwner::Home => ("home".to_owned(), home_profile),
            BrowserOwner::Person(id) => {
                // The person's own folder: from before ids it's named after
                // them, so their sign-ins carry over.
                let folder = match self.store.person_by_id(id) {
                    Ok(Some(person)) if safe_folder(&person.folder) => person.folder,
                    Ok(_) => {
                        tracing::warn!(person = %id, "no browser folder for this person");
                        return None;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "couldn't look up a person");
                        return None;
                    }
                };
                let root = home_profile
                    .parent()
                    .map_or_else(|| home_profile.clone(), std::path::Path::to_path_buf);
                (
                    folder.clone(),
                    root.join("people").join(folder).join("profile"),
                )
            }
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
        // Settings → Activity: which sites its bots visit.
        browser.count_visits(self.visit_sink(owner));
        let shared = Arc::new(Shared {
            owner: owner.clone(),
            browser,
            lease: Arc::new(Mutex::new(())),
            holder: std::sync::Mutex::new(None),
            last: std::sync::Mutex::new(None),
            last_used: std::sync::Mutex::new(Instant::now()),
            adding_site: std::sync::Mutex::new(None),
            sites_checked: std::sync::atomic::AtomicBool::new(false),
        });
        browsers.insert(owner.clone(), Arc::clone(&shared));
        drop(browsers);
        tokio::spawn(watch_browser(self.me.clone(), Arc::clone(&shared)));
        Some(shared)
    }

    /// The browser `bot` uses: its owner's own if it's private, else home.
    pub(super) fn browser_of(&self, bot: &Bot) -> Option<Arc<Shared>> {
        // A practice run has its own Chrome, never a live one.
        if super::trials::is_trial(&bot.id) {
            return self.trial_browser(&bot.id);
        }
        self.browser_for(&self.browser_owner_of(bot)?)
    }

    /// Whose browser `bot` uses (see [`Bot::browser_owner`]), except that
    /// only the owner has a browser of their own: anyone else's private
    /// bot (their own Assistant) uses the home browser.
    pub(super) fn browser_owner_of(&self, bot: &Bot) -> Option<BrowserOwner> {
        match bot.browser_owner()? {
            BrowserOwner::Person(id) if !self.is_farm_person(id.as_str()) => {
                Some(BrowserOwner::Home)
            }
            owner => Some(owner),
        }
    }

    /// Refuses a browser of their own for anyone but the owner (`Ok` for
    /// the home browser and the owner's).
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] for someone else's own browser.
    pub(super) fn own_browser_allowed(&self, owner: &BrowserOwner) -> Result<(), HubError> {
        match owner {
            BrowserOwner::Person(id) if !self.is_farm_person(id.as_str()) => {
                Err(HubError::NotAllowed(NO_OWN_BROWSER.to_owned()))
            }
            _ => Ok(()),
        }
    }

    /// `None` for the home browser, else its owner's name, as
    /// [`ServerMsg::Sites`] and notices name it.
    pub(super) fn owner_label(&self, owner: &BrowserOwner) -> Option<String> {
        match owner {
            BrowserOwner::Home => None,
            BrowserOwner::Person(id) => {
                Some(self.person_name(id).unwrap_or_else(|| id.to_string()))
            }
        }
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
            shared.browser.set_user(Some(bot.to_string()));
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
        tracing::info!(%bot, browser = %shared.owner, "has the browser");
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
        let dir = self.run_bot(bot.as_str())?.work_dir().join("downloads");
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
                shared.browser.set_user(None);
            }
            *lock(&shared.last_used) = Instant::now();
        }
    }

    /// `owner`'s browser, if no bot is using it right now.
    pub(super) fn idle_browser(&self, owner: &BrowserOwner) -> Result<Arc<Shared>, HubError> {
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

    /// Signs `owner`'s browser out of every site by deleting its profile.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] if a bot is using it, or the profile can't
    /// be deleted.
    pub async fn sign_out_all(&self, owner: impl Into<BrowserOwner>) -> Result<(), HubError> {
        let owner = owner.into();
        self.own_browser_allowed(&owner)?;
        let shared = self.browser_for(&owner).ok_or(HubError::NoBrowser)?;
        let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned() else {
            return Err(self.busy_error(&shared));
        };
        shared.browser.sign_out().await?;
        lock(&shared.last).take();
        let cleared = self.store.clear_sites(&owner);
        self.announce_sites(&shared);
        // The browser is signed out either way; say so if the list wasn't cleared.
        cleared?;
        self.notice_for(
            &owner,
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

/// Whether a person's folder name is safe to join onto `people/`.
fn safe_folder(folder: &str) -> bool {
    !folder.is_empty()
        && !folder.starts_with('.')
        && folder
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}
