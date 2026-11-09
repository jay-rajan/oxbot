//! Signed-in sites: each browser's list, signing in from Settings, and
//! the `request_sign_in` cards bots raise.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use oxbot_browser::{CookieMarks, is_home_network, site_of, web_host};
use oxbot_protocol::{EventKind, ServerMsg, SignInOutcome, SignInRequest, SiteInfo};
use tokio::sync::oneshot;

use super::{Hub, HubError, Shared, lock};
use crate::bot::Viewer;
use crate::id::{BotId, BrowserOwner};
use crate::store::Role;

/// A site being signed in to from Settings, and the browser's cookies
/// from just before its window opened.
#[derive(Debug)]
pub(super) struct AddingSite {
    /// The host the window opened on.
    host: String,
    /// The cookies before (`None` if they couldn't be read).
    before: Option<CookieMarks>,
}

/// Whether `site` (a main domain), just signed in to through a window,
/// goes on (or stays on) its browser's list. Only if the window left a
/// cookie that wasn't there before it opened, or changed one's value or
/// expiry: an old, unchanged cookie from visiting the site says nothing
/// about this sign-in. A site already on the list
/// stays (the regular check drops it once it has no cookies at all), and
/// so does one on the home network (a router's password box leaves none).
/// If the cookies from before couldn't be read, any cookie will do.
pub(super) fn keep_signed_in_site(
    site: &str,
    listed_before: bool,
    before: Option<&CookieMarks>,
    after: &CookieMarks,
) -> bool {
    if listed_before || is_home_network(&format!("https://{site}/")) {
        return true;
    }
    match before {
        Some(before) => after.changed_since(before, site),
        None => after.has(site),
    }
}

impl Hub {
    /// The sites `owner`'s browser signed in to through oxbot, newest first.
    #[must_use]
    pub fn sites(&self, owner: impl Into<BrowserOwner>) -> Vec<SiteInfo> {
        let owner = owner.into();
        let mut sites = self.store.sites(&owner).unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read the signed-in sites");
            Vec::new()
        });
        if let Some(shared) = self.browser_for(&owner) {
            let names: Vec<&str> = sites.iter().map(|s| s.site.as_str()).collect();
            let icons = shared.browser.site_icons(&names);
            for (site, icon) in sites.iter_mut().zip(icons) {
                site.icon = icon;
            }
        }
        sites
    }

    /// `viewer`'s own browser, for Settings → Websites. Only the owner has
    /// one: everyone else's bots use the home browser.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] for someone oxbot can't place, or anyone
    /// but the owner.
    pub fn own_browser_of(&self, viewer: &Viewer) -> Result<BrowserOwner, HubError> {
        let viewer = self.resolve(viewer);
        let Some(id) = viewer.id.clone() else {
            return Err(HubError::NotAllowed(
                "oxbot doesn't know whose browser that is.".to_owned(),
            ));
        };
        let owner = BrowserOwner::Person(id);
        self.own_browser_allowed(&owner)?;
        Ok(owner)
    }

    /// The site lists `viewer` gets: the home browser's, then their own
    /// (the owner only: nobody else has a browser of their own), then the
    /// websites with fine-grained control.
    #[must_use]
    pub fn sites_for(&self, viewer: &Viewer) -> Vec<ServerMsg> {
        let viewer = self.resolve(viewer);
        let mut lists = vec![ServerMsg::Sites {
            sites: self.sites(BrowserOwner::Home),
            owner: None,
            owner_id: None,
        }];
        if let Some(id) = viewer
            .id
            .as_ref()
            .filter(|id| self.is_owner_person(id.as_str()))
        {
            lists.push(ServerMsg::Sites {
                sites: self.sites(BrowserOwner::Person(id.clone())),
                owner: Some(viewer.name.clone()),
                owner_id: Some(id.to_string()),
            });
        }
        lists.push(ServerMsg::FineGrainedSites {
            sites: self.fine_grained_sites(),
        });
        lists
    }

    pub(super) fn announce_sites(&self, shared: &Shared) {
        self.announce(ServerMsg::Sites {
            sites: self.sites(&shared.owner),
            owner: self.owner_label(&shared.owner),
            owner_id: super::owner_id(&shared.owner),
        });
    }

    /// Opens a sign-in window on `url` in `owner`'s browser, for signing in
    /// to a site before any bot asks. When the window closes, the site joins
    /// that browser's list.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] while a bot is using that browser, or
    /// [`HubError::BadSite`] if `url` isn't a web address.
    pub async fn add_sign_in(
        &self,
        url: &str,
        owner: impl Into<BrowserOwner>,
    ) -> Result<(), HubError> {
        let url = url.trim();
        let url = if url.contains("://") {
            url.to_owned()
        } else {
            format!("https://{url}")
        };
        let host = web_host(&url).ok_or_else(|| HubError::BadSite(url.clone()))?;
        let owner = owner.into();
        self.own_browser_allowed(&owner)?;
        let shared = self.idle_browser(&owner)?;
        let before = self.cookies_before_sign_in(&shared).await;
        *lock(&shared.adding_site) = Some(AddingSite { host, before });
        if let Err(err) = shared.browser.open_window(&url).await {
            // No window, so nothing to add when one closes.
            lock(&shared.adding_site).take();
            return Err(err.into());
        }
        Ok(())
    }

    /// Checks a browser's list against its cookies, drops sites it's no
    /// longer signed in to (say, the window closed before signing in, or
    /// the address was mistyped; never one on the home network), and
    /// tells every client. Returns whether the cookies could be read.
    pub(super) async fn refresh_sites(&self, shared: &Shared) -> bool {
        let checked = match shared.browser.cookie_sites().await {
            Ok(held) => {
                self.keep_sites_with_cookies(shared, held);
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

    /// Drops `shared`'s sites with no cookie in `held` (sites with
    /// cookies), except ones on the home network.
    fn keep_sites_with_cookies(&self, shared: &Shared, mut held: Vec<String>) {
        // Devices on the home network often sign in without cookies
        // (a router's password box): cookies say nothing about them.
        held.extend(
            self.store
                .sites(&shared.owner)
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.site)
                .filter(|s| is_home_network(&format!("https://{s}/"))),
        );
        match self.store.keep_only_sites(&shared.owner, &held) {
            Ok(gone) if !gone.is_empty() => tracing::info!(
                browser = %shared.owner,
                dropped = gone.len(),
                "no sign-in cookies left for some sites; dropped them"
            ),
            Ok(_) => {}
            Err(err) => tracing::warn!(%err, "couldn't drop signed-out sites"),
        }
        if let Err(err) = self.store.site_check_done(&shared.owner) {
            tracing::warn!(%err, "couldn't note the sites were checked");
        }
    }

    /// The browser's cookies just before a sign-in window opens, to compare
    /// with after. `None` if they can't be read (then any cookie counts).
    async fn cookies_before_sign_in(&self, shared: &Shared) -> Option<CookieMarks> {
        match shared.browser.cookie_marks().await {
            Ok(marks) => Some(marks),
            Err(err) => {
                tracing::debug!(%err, "couldn't read the cookies before signing in");
                None
            }
        }
    }

    /// After a sign-in window for `host` closes: the site goes on the list
    /// if [`keep_signed_in_site`] says so, the rest of the list is checked
    /// against the cookies, and every client is told. Returns whether the
    /// site was kept.
    async fn settle_sign_in(
        &self,
        shared: &Shared,
        host: &str,
        before: Option<CookieMarks>,
    ) -> bool {
        let site = site_of(host);
        let listed = self
            .store
            .sites(&shared.owner)
            .unwrap_or_default()
            .iter()
            .any(|s| s.site == site);
        // Both sets of value hashes go as soon as they're compared.
        let (keep, held) = match shared.browser.cookie_marks().await {
            Ok(after) => (
                keep_signed_in_site(&site, listed, before.as_ref(), &after),
                Some(after.sites()),
            ),
            Err(err) => {
                // Can't tell: keep it, as the user says they signed in.
                tracing::debug!(%err, "couldn't read the cookies after signing in");
                (true, None)
            }
        };
        drop(before);
        if keep {
            if let Err(err) = self.store.add_site(&shared.owner, &site) {
                tracing::warn!(%err, "couldn't remember the site");
            }
        } else {
            tracing::info!(browser = %shared.owner, "the sign-in window left no new cookies; not adding the site");
        }
        if let Some(held) = held {
            self.keep_sites_with_cookies(shared, held);
        }
        self.announce_sites(shared);
        keep
    }

    /// Shows `site` in `owner`'s visible Chrome window, e.g. to check an
    /// account. Closing the window goes back to hidden Chrome.
    ///
    /// # Errors
    /// [`HubError::BrowserBusy`] while a bot is using that browser, or
    /// [`HubError::BadSite`] if `site` isn't on its list.
    pub async fn open_site(
        &self,
        site: &str,
        owner: impl Into<BrowserOwner>,
    ) -> Result<(), HubError> {
        let owner = owner.into();
        self.own_browser_allowed(&owner)?;
        // Only a site on that browser's list, never any address a client sends.
        let known = self
            .store
            .sites(&owner)
            .unwrap_or_default()
            .iter()
            .any(|s| s.site == site);
        if !known {
            return Err(HubError::BadSite(site.to_owned()));
        }
        let shared = self.idle_browser(&owner)?;
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
    pub async fn sign_out_site(
        &self,
        site: &str,
        owner: impl Into<BrowserOwner>,
    ) -> Result<(), HubError> {
        let owner = owner.into();
        self.own_browser_allowed(&owner)?;
        let shared = self.browser_for(&owner).ok_or(HubError::NoBrowser)?;
        let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned() else {
            return Err(self.busy_error(&shared));
        };
        shared.browser.sign_out_site(site).await?;
        let removed = self.store.remove_site(&owner, site);
        lock(&shared.last).take();
        self.announce_sites(&shared);
        removed?;
        self.notice_for(
            &owner,
            &format!("Signed out of {site}. Bots will ask you to sign in again if they need it."),
        );
        Ok(())
    }

    /// Shares `viewer`'s own sign-in to `site` with the home: copies that
    /// site's cookies from their browser into the home one, so shared bots
    /// are signed in too, and lists it there. Their own copy stays, so
    /// their private bots keep working. Every client hears both lists.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] for someone oxbot can't place, a kid
    /// account (the home browser is everyone's), a site not on their own
    /// list or one their browser holds no cookies for;
    /// [`HubError::BrowserBusy`] while a bot uses either browser,
    /// or the browser's error.
    pub async fn share_site(&self, site: &str, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = self.resolve(viewer);
        let Some(person) = viewer.id.clone() else {
            return Err(HubError::NotAllowed(
                "oxbot doesn't know whose sign-in that is.".to_owned(),
            ));
        };
        if self.role_of(&person) == Role::Kid {
            return Err(HubError::NotAllowed(
                "Ask a grown-up to share a sign-in with everyone at home.".to_owned(),
            ));
        }
        let own = BrowserOwner::Person(person);
        // Only the owner has sign-ins of their own to share.
        self.own_browser_allowed(&own)?;
        // Only a site on their own list, never any address a client sends.
        let site = self
            .store
            .sites(&own)
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.site)
            .find(|s| s == site)
            .ok_or_else(|| {
                HubError::NotAllowed(format!("You aren't signed in to {site} on your own."))
            })?;
        let from = self.browser_for(&own).ok_or(HubError::NoBrowser)?;
        let to = self
            .browser_for(&BrowserOwner::Home)
            .ok_or(HubError::NoBrowser)?;
        let Ok(_from_lease) = Arc::clone(&from.lease).try_lock_owned() else {
            return Err(self.busy_error(&from));
        };
        let Ok(_to_lease) = Arc::clone(&to.lease).try_lock_owned() else {
            return Err(self.busy_error(&to));
        };
        let cookies = from.browser.site_cookies(&site).await?;
        if cookies.is_empty() {
            return Err(HubError::NotAllowed(format!(
                "You're no longer signed in to {site}. Sign in again, then share it."
            )));
        }
        to.browser.add_cookies(cookies).await?;
        self.store.add_site(&BrowserOwner::Home, &site)?;
        tracing::info!(browser = %own, "shared a sign-in with the home browser");
        self.announce_sites(&to);
        self.announce_sites(&from);
        self.notice_for(
            &own,
            &format!("{site} is shared: bots everyone uses are signed in to it too."),
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
                "request_sign_in needs a web address on the internet (https://…), not one on the home computer or the home network.".to_owned(),
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
            // A person's own browser: only they sign it in, even when
            // others may use the bot (shared with picked people).
            only: match &shared.owner {
                BrowserOwner::Person(id) => Some(id.to_string()),
                BrowserOwner::Home => None,
            },
        };
        let (tx, rx) = oneshot::channel();
        lock(&self.sign_ins).insert(id, (request.clone(), tx));
        let _ = self.updates.send(ServerMsg::SignInNeeded(request));
        let _ = self.updates.send(ServerMsg::Activity {
            bot: bot.to_string(),
            text: format!("Waiting for you to sign in to {site}…"),
        });
        tracing::info!(%bot, browser = %shared.owner, "asking the user to sign in");
        self.scheduled_run_waiting(bot, "needs you to sign in");
        let before = self.cookies_before_sign_in(&shared).await;
        // Open the sign-in window right away; the card's button is the fallback.
        if let Err(err) = self.open_sign_in(id).await {
            tracing::warn!(%bot, %err, "couldn't open the sign-in window");
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
            self.settle_sign_in(&shared, &site, before).await;
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
        let card = lock(&self.sign_ins).get(&id).map(|(r, _)| r.clone());
        let viewer = self.resolve(viewer);
        card.is_some_and(|r| self.bot_for(&r.bot, &viewer).is_ok() && signs_in(&r, &viewer))
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
    /// A site added from Settings is kept only if the window left new
    /// cookies for it (a cancelled or mistyped sign-in leaves none).
    pub(super) fn sign_in_window_closed(&self, shared: &Arc<Shared>) {
        if let Some(adding) = lock(&shared.adding_site).take()
            && let Some(hub) = self.me.upgrade()
        {
            let shared = Arc::clone(shared);
            tokio::spawn(async move {
                hub.settle_sign_in(&shared, &adding.host, adding.before)
                    .await;
            });
        }
        let ids: Vec<u64> = lock(&self.sign_ins)
            .iter()
            .filter(|(_, (r, _))| {
                r.window_open
                    && self
                        .bot(&r.bot)
                        .is_ok_and(|b| self.browser_owner_of(&b).as_ref() == Some(&shared.owner))
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

/// Whether `viewer` may see and answer sign-in request `r` (besides being
/// able to open its bot): the home computer, or anyone for the home's browser, but a
/// person's own browser only by that person (by id).
#[must_use]
pub fn signs_in(r: &SignInRequest, viewer: &Viewer) -> bool {
    viewer.home_computer
        || r.only
            .as_deref()
            .is_none_or(|id| viewer.is_person(Some(id), None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn marks(cookies: &[(&str, &str)]) -> CookieMarks {
        let cookies: Vec<serde_json::Value> = cookies
            .iter()
            .map(|(name, domain)| json!({ "name": name, "domain": domain, "path": "/" }))
            .collect();
        CookieMarks::from_cookies(&cookies)
    }

    #[test]
    fn a_new_site_is_kept_only_if_the_window_left_new_cookies() {
        // Visited before: an old cookie, so having one proves nothing.
        let before = marks(&[("prefs", ".bank.example")]);
        let cancelled = before.clone();
        assert!(!keep_signed_in_site(
            "bank.example",
            false,
            Some(&before),
            &cancelled
        ));
        let signed_in = marks(&[("prefs", ".bank.example"), ("session", "bank.example")]);
        assert!(keep_signed_in_site(
            "bank.example",
            false,
            Some(&before),
            &signed_in
        ));
        // A mistyped address leaves no cookie at all.
        assert!(!keep_signed_in_site(
            "bnak.example",
            false,
            Some(&before),
            &signed_in
        ));
    }

    #[test]
    fn a_new_site_is_kept_if_the_window_changed_an_old_cookie() {
        let sid = |value: &str| {
            CookieMarks::from_cookies(&[
                json!({ "name": "sid", "domain": ".bank.example", "path": "/", "value": value }),
            ])
        };
        let before = sid("guest");
        assert!(!keep_signed_in_site(
            "bank.example",
            false,
            Some(&before),
            &sid("guest")
        ));
        assert!(keep_signed_in_site(
            "bank.example",
            false,
            Some(&before),
            &sid("signed-in")
        ));
    }

    #[test]
    fn listed_and_home_network_sites_stay_and_unknown_before_takes_any_cookie() {
        let none = CookieMarks::default();
        let before = marks(&[("sid", ".mail.example")]);
        assert!(
            keep_signed_in_site("mail.example", true, Some(&before), &before),
            "already signed in before"
        );
        assert!(keep_signed_in_site(
            "192.168.1.1",
            false,
            Some(&none),
            &none
        ));
        assert!(keep_signed_in_site(
            "router.local",
            false,
            Some(&none),
            &none
        ));
        // Cookies from before unknown: any cookie for it will do.
        assert!(keep_signed_in_site("mail.example", false, None, &before));
        assert!(!keep_signed_in_site("other.example", false, None, &before));
    }

    #[tokio::test]
    async fn sharing_a_sign_in_copies_it_into_the_home_browser() {
        let tmp = tempfile::tempdir().unwrap();
        let bots = tmp.path().join("bots");
        std::fs::create_dir_all(&bots).unwrap();
        let mut tools = oxbot_browser::ToolsConfig::in_home(&tmp.path().join("home"));
        tools.tools_dir = tmp.path().join("no-tools");
        let chrome = tools.chrome.exists();
        let hub = Hub::new(
            vec![],
            crate::store::Store::open_in_memory().unwrap(),
            crate::HubConfig {
                browser: Some(oxbot_browser::Tools::new(tools)),
                ..crate::HubConfig::new(&bots)
            },
        );
        let jay = hub.owner_person("Jay").unwrap();
        let kim = hub.person_named("Kim").unwrap();
        hub.store.set_role(&kim.id, Role::Kid).unwrap();
        let jay_phone = Viewer::person(jay.id.clone(), "Jay", false);
        let mine = BrowserOwner::Person(jay.id.clone());
        let refused = |r: Result<(), HubError>| match r {
            Err(HubError::NotAllowed(why)) => why,
            other => panic!("{other:?}"),
        };
        // Only a site on their own list.
        assert_eq!(
            refused(hub.share_site("bank.example", &jay_phone).await),
            "You aren't signed in to bank.example on your own."
        );
        // Never a kid's: the home browser is everyone's.
        let kims = BrowserOwner::Person(kim.id.clone());
        hub.store.add_site(&kims, "games.example").unwrap();
        let kid = Viewer::person(kim.id.clone(), "Kim", false);
        assert!(refused(hub.share_site("games.example", &kid).await).contains("grown-up"));
        assert_eq!(hub.sites(BrowserOwner::Home), Vec::new());
        if !chrome {
            eprintln!("Chrome isn't installed; skipping the copy");
            return;
        }
        // Jay is signed in to the bank in his own browser (a temp profile).
        let own = hub.browser_for(&mine).unwrap();
        own.browser
            .add_cookies(vec![
                json!({"name": "sid", "value": "s3cret", "domain": ".bank.example", "path": "/",
                       "secure": true, "httpOnly": true, "expires": 2_000_000_000}),
                json!({"name": "g", "value": "1", "domain": ".google.com", "path": "/",
                       "secure": true, "expires": 2_000_000_000}),
            ])
            .await
            .unwrap();
        hub.store.add_site(&mine, "bank.example").unwrap();
        let mut updates = hub.subscribe();
        hub.share_site("bank.example", &jay_phone).await.unwrap();
        let home = hub.browser_for(&BrowserOwner::Home).unwrap();
        assert_eq!(home.browser.cookie_sites().await.unwrap(), ["bank.example"]);
        assert_eq!(
            own.browser.cookie_sites().await.unwrap(),
            ["bank.example", "google.com"],
            "the private copy stays"
        );
        let listed =
            |o: BrowserOwner| -> Vec<String> { hub.sites(o).into_iter().map(|s| s.site).collect() };
        assert_eq!(listed(BrowserOwner::Home), ["bank.example"]);
        assert_eq!(listed(mine.clone()), ["bank.example"]);
        // Both lists go out.
        let mut lists = Vec::new();
        while let Ok(msg) = updates.try_recv() {
            if let ServerMsg::Sites { owner_id, .. } = msg {
                lists.push(owner_id);
            }
        }
        assert_eq!(lists, [None, Some(jay.id.to_string())]);
        home.browser.shutdown().await;
        own.browser.shutdown().await;
    }

    #[tokio::test]
    async fn a_private_bot_signs_in_with_its_makers_own_browser() {
        let tmp = tempfile::tempdir().unwrap();
        let bots = tmp.path().join("bots");
        std::fs::create_dir_all(&bots).unwrap();
        let tools = oxbot_browser::ToolsConfig::in_home(&tmp.path().join("home"));
        let hub = Hub::new(
            vec![],
            crate::store::Store::open_in_memory().unwrap(),
            crate::HubConfig {
                browser: Some(oxbot_browser::Tools::new(tools)),
                ..crate::HubConfig::new(&bots)
            },
        );
        let jay = hub.owner_person("Jay").unwrap();
        let phone = Viewer::person(jay.id.clone(), "Jay", false);
        let diary = hub
            .create_bot_as("Diary", "", Some(&phone), true)
            .await
            .unwrap();
        let shared = hub
            .create_bot_as("Recipes", "", Some(&phone), false)
            .await
            .unwrap();
        let browser = |id: &str| hub.browser_of(&hub.bot(id).unwrap()).unwrap().owner.clone();
        assert_eq!(browser(&diary.id), BrowserOwner::Person(jay.id.clone()));
        assert_eq!(browser(&shared.id), BrowserOwner::Home);
    }
}
