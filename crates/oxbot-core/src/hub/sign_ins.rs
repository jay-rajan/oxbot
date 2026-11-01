//! Signed-in sites: each browser's list, signing in from Settings, and
//! the `request_sign_in` cards bots raise.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use oxbot_browser::{site_of, web_host};
use oxbot_protocol::{EventKind, ServerMsg, SignInOutcome, SignInRequest, SiteInfo};
use tokio::sync::oneshot;

use super::{Hub, HubError, Shared, lock};
use crate::bot::Viewer;
use crate::id::BotId;
use crate::store::{HOME, Store};

impl Hub {
    /// The sites `owner`'s browser signed in to through oxbot, newest first.
    #[must_use]
    pub fn sites(&self, owner: &str) -> Vec<SiteInfo> {
        let mut sites = self.store.sites(owner).unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read the signed-in sites");
            Vec::new()
        });
        if let Some(shared) = self.browser_for(owner) {
            let names: Vec<&str> = sites.iter().map(|s| s.site.as_str()).collect();
            let icons = shared.browser.site_icons(&names);
            for (site, icon) in sites.iter_mut().zip(icons) {
                site.icon = icon;
            }
        }
        sites
    }

    /// The site lists `viewer` gets: the home browser's, then their own.
    #[must_use]
    pub fn sites_for(&self, viewer: &Viewer) -> Vec<ServerMsg> {
        let mut lists = vec![ServerMsg::Sites {
            sites: self.sites(HOME),
            owner: None,
        }];
        if !viewer.name.is_empty() {
            lists.push(ServerMsg::Sites {
                sites: self.sites(&viewer.name),
                owner: Some(viewer.name.clone()),
            });
        }
        lists
    }

    pub(super) fn announce_sites(&self, shared: &Shared) {
        self.announce(ServerMsg::Sites {
            sites: self.sites(&shared.owner),
            owner: shared.owner_label(),
        });
    }

    /// Opens a sign-in window on `url` in `owner`'s browser, for signing in
    /// to a site before any bot asks. When the window closes, the site joins
    /// that browser's list.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] while a bot is using that browser, or
    /// [`HubError::BadSite`] if `url` isn't a web address.
    pub async fn add_sign_in(&self, url: &str, owner: &str) -> Result<(), HubError> {
        let url = url.trim();
        let url = if url.contains("://") {
            url.to_owned()
        } else {
            format!("https://{url}")
        };
        let host = web_host(&url).ok_or_else(|| HubError::BadSite(url.clone()))?;
        let shared = self.idle_browser(owner)?;
        *lock(&shared.adding_site) = Some(host);
        shared.browser.open_window(&url).await?;
        Ok(())
    }

    /// Checks a browser's list against its cookies, drops sites it's no
    /// longer signed in to (say, the window closed before signing in), and
    /// tells every client. Returns whether the cookies could be read.
    pub(super) async fn refresh_sites(&self, shared: &Shared) -> bool {
        let checked = match shared.browser.cookie_sites().await {
            Ok(held) => {
                for site in self.store.sites(&shared.owner).unwrap_or_default() {
                    if !held.contains(&site.site) {
                        tracing::info!(
                            site = site.site,
                            browser = shared.owner,
                            "no sign-in cookies left; dropping it"
                        );
                        let _ = self.store.remove_site(&shared.owner, &site.site);
                    }
                }
                true
            }
            Err(err) => {
                tracing::debug!(%err, "couldn't check the browser's cookies");
                false
            }
        };
        self.announce_sites(shared);
        checked
    }

    /// Shows `site` in `owner`'s visible Chrome window, e.g. to check an
    /// account. Closing the window goes back to hidden Chrome.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] while a bot is using that browser, or
    /// [`HubError::BadSite`] if `site` isn't on its list.
    pub async fn open_site(&self, site: &str, owner: &str) -> Result<(), HubError> {
        // Only a site on that browser's list, never any address a client sends.
        let known = self
            .store
            .sites(owner)
            .unwrap_or_default()
            .iter()
            .any(|s| s.site == site);
        if !known {
            return Err(HubError::BadSite(site.to_owned()));
        }
        let shared = self.idle_browser(owner)?;
        shared
            .browser
            .open_window(&format!("https://{site}"))
            .await?;
        Ok(())
    }

    /// Signs `owner`'s browser out of `site` only.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] while a bot is using that browser, or the
    /// browser's error.
    pub async fn sign_out_site(&self, site: &str, owner: &str) -> Result<(), HubError> {
        let shared = self.browser_for(owner).ok_or(HubError::NoBrowser)?;
        let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned() else {
            return Err(self.busy_error(&shared));
        };
        shared.browser.forget_site(site).await?;
        if let Err(err) = self.store.remove_site(owner, site) {
            tracing::warn!(%err, "couldn't forget the site");
        }
        lock(&shared.last).take();
        self.announce_sites(&shared);
        self.notice_for(
            shared.owner_label(),
            &format!("Signed out of {site}. Bots will ask you to sign in again if they need it."),
        );
        Ok(())
    }

    /// A bot asks the user to sign in at `url`. Shows its people a card and
    /// waits until they're done, give up, or don't answer in time. Returns
    /// whether they signed in, and what to tell the bot.
    pub async fn request_sign_in(&self, bot: &BotId, url: &str, why: &str) -> (bool, String) {
        let Some(shared) = self
            .bot(bot.as_str())
            .ok()
            .and_then(|b| self.browser_of(&b))
        else {
            return (false, "This oxbot has no browser.".to_owned());
        };
        let Some(site) = web_host(url).filter(|_| !oxbot_browser::is_home_network(url)) else {
            return (
                false,
                "request_sign_in needs a web address on the internet (https://…), not one on this Mac or the home network.".to_owned(),
            );
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = SignInRequest {
            id,
            bot: bot.to_string(),
            url: url.to_owned(),
            site: site.clone(),
            why: why.trim().to_owned(),
            window_open: false,
        };
        let (tx, rx) = oneshot::channel();
        lock(&self.sign_ins).insert(id, (request.clone(), tx));
        let _ = self.updates.send(ServerMsg::SignInNeeded(request));
        let _ = self.updates.send(ServerMsg::Activity {
            bot: bot.to_string(),
            text: format!("Waiting for you to sign in to {site}…"),
        });
        tracing::info!(%bot, site, browser = shared.owner, "asking the user to sign in");
        // Open the sign-in window right away; the card's button is the fallback.
        if let Err(err) = self.open_sign_in(id).await {
            tracing::warn!(%bot, site, %err, "couldn't open the sign-in window");
        }

        let minutes = self.config.sign_in_timeout.as_secs() / 60;
        let answer = tokio::time::timeout(self.config.sign_in_timeout, rx).await;
        let (outcome, result) = match answer {
            Ok(Ok(true)) => (
                SignInOutcome::Done,
                (
                    true,
                    format!(
                        "The user finished with the sign-in window for {site} (they pressed I'm \
                         signed in, or closed the window). Chrome restarted, so list or open pages \
                         again, then check you're really signed in. If you're still signed out, say \
                         so plainly and don't ask to sign in again until the user asks you to."
                    ),
                ),
            ),
            Ok(Ok(false) | Err(_)) => (
                SignInOutcome::Cancelled,
                (
                    false,
                    "The sign-in was cancelled: the user pressed Cancel, or oxbot restarted. Tell \
                     them you stopped because the site needs signing in, and that they can ask you \
                     to try again."
                        .to_owned(),
                ),
            ),
            Err(_) => {
                self.resolve_sign_in(id, false);
                (
                    SignInOutcome::TimedOut,
                    (
                        false,
                        format!(
                            "The user didn't sign in within {minutes} minutes. Tell them you stopped."
                        ),
                    ),
                )
            }
        };
        shared.browser.close_window().await;
        if outcome == SignInOutcome::Done {
            if let Err(err) = self.store.add_site(&shared.owner, &site_of(&site)) {
                tracing::warn!(%err, "couldn't remember the site");
            }
            self.refresh_sites(&shared).await;
        }
        if let Err(err) = self.record(bot, EventKind::SignIn { site, outcome }) {
            tracing::warn!(%err, "couldn't record the sign-in");
        }
        result
    }

    /// Opens the sign-in window for request `id`, in its bot's browser.
    ///
    /// # Errors
    /// Fails if there's no such request or Chrome can't start.
    pub async fn open_sign_in(&self, id: u64) -> Result<(), HubError> {
        let request = lock(&self.sign_ins).get(&id).map(|(r, _)| r.clone());
        let Some(mut request) = request else {
            return Err(HubError::UnknownBot(format!("sign-in {id}")));
        };
        let shared = self
            .bot(&request.bot)
            .ok()
            .and_then(|b| self.browser_of(&b))
            .ok_or(HubError::NoBrowser)?;
        shared.browser.open_window(&request.url).await?;
        request.window_open = true;
        if let Some(entry) = lock(&self.sign_ins).get_mut(&id) {
            entry.0.window_open = true;
        }
        let _ = self.updates.send(ServerMsg::SignInNeeded(request));
        Ok(())
    }

    /// Whether `viewer` may see (and answer) sign-in request `id`.
    #[must_use]
    pub fn sign_in_visible(&self, id: u64, viewer: &Viewer) -> bool {
        let bot = lock(&self.sign_ins).get(&id).map(|(r, _)| r.bot.clone());
        bot.is_some_and(|b| self.bot_for(&b, viewer).is_ok())
    }

    /// The user finished (`true`) or gave up on (`false`) a sign-in. Returns
    /// whether it was still open.
    pub fn resolve_sign_in(&self, id: u64, signed_in: bool) -> bool {
        let entry = lock(&self.sign_ins).remove(&id);
        let Some((_, tx)) = entry else { return false };
        let _ = tx.send(signed_in);
        let _ = self.updates.send(ServerMsg::SignInResolved { id });
        true
    }

    /// Sign-in requests still waiting for the user.
    #[must_use]
    pub fn pending_sign_ins(&self) -> Vec<SignInRequest> {
        lock(&self.sign_ins)
            .values()
            .map(|(r, _)| r.clone())
            .collect()
    }

    /// A browser's sign-in window closed: requests on it whose window was
    /// open count as done (the user closes it when they've signed in).
    pub(super) fn sign_in_window_closed(&self, shared: &Shared) {
        if let Some(host) = lock(&shared.adding_site).take() {
            if let Err(err) = self.store.add_site(&shared.owner, &site_of(&host)) {
                tracing::warn!(%err, "couldn't remember the site");
            }
            self.announce_sites(shared);
        }
        let ids: Vec<u64> = lock(&self.sign_ins)
            .iter()
            .filter(|(_, (r, _))| {
                r.window_open
                    && self
                        .bot(&r.bot)
                        .is_ok_and(|b| b.browser_owner().as_deref() == Some(shared.owner.as_str()))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.resolve_sign_in(id, true);
        }
    }

    pub(super) fn cancel_sign_ins(&self, bot: &BotId) {
        let ids: Vec<u64> = lock(&self.sign_ins)
            .iter()
            .filter(|(_, (r, _))| r.bot == bot.as_str())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.resolve_sign_in(id, false);
        }
    }
}

/// Names every remembered site by its main domain (sign-ins taken from old
/// history are stored by host, e.g. `accounts.google.com`).
pub(super) fn tidy_sites(store: &Store) {
    for owner in store.site_owners().unwrap_or_default() {
        let Ok(sites) = store.sites(&owner) else {
            continue;
        };
        for site in sites {
            let main = site_of(&site.site);
            if main != site.site {
                let _ = store.remove_site(&owner, &site.site);
                let _ = store.add_site_at(&owner, &main, site.signed_in_ms);
            }
        }
    }
}
