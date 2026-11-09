//! Scripts and batches: what the hub does for `read_pages` (copying a page
//! of the bot's browser and running the bot's script on the copy in the
//! sealed tab), which websites refuse `page_steps` (set to fine-grained
//! control by the owner in Settings → Websites; every other website allows
//! it), and the approval request before a bot runs a
//! small script in its folder (Yes · Always for this bot · No).

use std::sync::Arc;

use oxbot_browser::{is_home_network, site_of, web_host};
use oxbot_protocol::{EventKind, ServerMsg};

use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::engine::{Decision, SealedCall, SealedReply, SealedWork};
use crate::id::BotId;
use crate::policy::{self, SCRIPT_RULE_ACTION, SCRIPT_RULE_LABEL, SCRIPT_SITE};
use crate::store::RuleKey;

/// Where the websites with fine-grained control are kept (a JSON list of
/// main domains). The older `page_steps_sites` list (where several steps
/// were allowed, when that was off by default) is no longer read: those
/// sites now allow it like every other.
const PREF_FINE_GRAINED_SITES: &str = "fine_grained_sites";

/// The most websites that may be set to fine-grained control.
const MAX_FINE_GRAINED_SITES: usize = 200;

/// A bot's Always rule for small scripts: it may run them in its own
/// folder without asking (while it hasn't read untrusted text).
#[must_use]
pub(super) fn script_rule() -> RuleKey {
    RuleKey {
        site: SCRIPT_SITE.to_owned(),
        action: SCRIPT_RULE_ACTION.to_owned(),
        label: SCRIPT_RULE_LABEL.to_owned(),
    }
}

/// The main domain a setting names: `https://www.Amazon.ca/x` and
/// `amazon.ca` are both `amazon.ca`. `None` for anything that isn't a web
/// site on the internet.
#[must_use]
pub(super) fn step_site_of(given: &str) -> Option<String> {
    let given = given.trim();
    let url = if given.contains("://") {
        given.to_owned()
    } else {
        format!("https://{given}")
    };
    if is_home_network(&url) {
        return None;
    }
    web_host(&url).map(|host| site_of(&host))
}

/// The list as kept, read leniently (a broken value is an empty list).
fn read_sites(raw: Option<&str>) -> Vec<String> {
    raw.and_then(|t| serde_json::from_str::<Vec<String>>(t).ok())
        .unwrap_or_default()
}

/// `sites` with `site` on (or off), sorted, without repeats.
fn with_site(mut sites: Vec<String>, site: &str, on: bool) -> Vec<String> {
    sites.retain(|s| s != site);
    if on {
        sites.push(site.to_owned());
    }
    sites.sort();
    sites.dedup();
    sites
}

/// What the model is told when `page_steps` isn't allowed on `site`.
#[must_use]
pub(super) fn steps_refusal(site: Option<&str>) -> String {
    match site {
        Some(site) => format!(
            "{site} is set to fine-grained control: do the steps one at a time there. (The \
             person chose this in Settings → Websites.)"
        ),
        None => "Several steps at once only work on websites on the internet. Do the steps one at \
                 a time."
            .to_owned(),
    }
}

impl Hub {
    /// The websites with fine-grained control (bots do one step at a
    /// time there), sorted.
    #[must_use]
    pub fn fine_grained_sites(&self) -> Vec<String> {
        read_sites(
            self.store
                .pref(PREF_FINE_GRAINED_SITES)
                .ok()
                .flatten()
                .as_deref(),
        )
    }

    /// Sets fine-grained control on `site` (`on`: one step at a time), or
    /// clears it (several steps at once, the default). The home computer only;
    /// everyone gets the new list.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] for anyone but the home computer,
    /// [`HubError::BadSite`] for something that isn't a website, or the
    /// database failed.
    pub fn set_fine_grained_site(
        &self,
        site: &str,
        on: bool,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        if !self.resolve(viewer).home_computer {
            return Err(HubError::NotAllowed(
                "Only the home computer can change fine-grained control on websites.".to_owned(),
            ));
        }
        let main = step_site_of(site).ok_or_else(|| HubError::BadSite(site.trim().to_owned()))?;
        let sites = with_site(self.fine_grained_sites(), &main, on);
        if sites.len() > MAX_FINE_GRAINED_SITES {
            return Err(HubError::NotAllowed(format!(
                "At most {MAX_FINE_GRAINED_SITES} websites can have fine-grained control."
            )));
        }
        let json = serde_json::to_string(&sites).unwrap_or_else(|_| "[]".to_owned());
        self.store.set_pref(PREF_FINE_GRAINED_SITES, &json)?;
        tracing::info!(site = main, on, "fine-grained control");
        self.announce(ServerMsg::FineGrainedSites { sites });
        Ok(())
    }

    /// Why `page_steps` may not run on the page at `page_url`, or `None`
    /// if it may: any website on the internet without fine-grained control.
    pub(super) fn steps_not_allowed(&self, page_url: Option<&str>) -> Option<String> {
        match page_url.and_then(step_site_of) {
            Some(site) if self.fine_grained_sites().contains(&site) => {
                Some(steps_refusal(Some(&site)))
            }
            Some(_) => None,
            None => Some(steps_refusal(None)),
        }
    }

    /// The gate's answer before a program runs in `bot`'s folder: the
    /// card (a kid's goes to a parent), or yes at once if the bot has an
    /// Always rule for small scripts and its session hasn't read untrusted
    /// text (which may have asked for the program).
    pub(super) async fn script_permission(
        &self,
        bot: &Bot,
        program: &str,
    ) -> Result<Decision, HubError> {
        let (taint, asker) = {
            let runtimes = self.runtimes.lock().await;
            let r = runtimes.get(&bot.id);
            (
                r.and_then(|r| r.taint.clone()),
                r.and_then(|r| r.origin.asker().cloned()),
            )
        };
        if taint.is_none() && self.store.allow_rule_matches(&bot.id, &script_rule())? {
            tracing::info!(bot = %bot.id, "a small script, allowed by a rule");
            self.record(
                &bot.id,
                EventKind::AllowedByRule {
                    action: "run a small script in".to_owned(),
                    target: format!("{} folder", policy::possessive(&bot.name)),
                    site: SCRIPT_SITE.to_owned(),
                },
            )?;
            return Ok(Decision::Allow);
        }
        let mut ask = policy::script_ask(&bot.name, program);
        ask.answerer = asker.as_ref().and_then(|p| self.answerer_for(p));
        if let Some(read) = taint {
            // The card says why an Always rule didn't apply.
            ask.detail = Some(format!(
                "{}\n\n(It has read {read} in this conversation, so oxbot asks even if you said \
                 Always.)",
                ask.detail.unwrap_or_default()
            ));
        }
        Box::pin(self.ask_user(bot, ask, None)).await
    }

    /// `read_pages`' work in `bot`'s browser (see [`SealedWork`]).
    pub(super) async fn sealed_work(self: Arc<Self>, bot: BotId, call: SealedCall) {
        let reply = match call.work {
            SealedWork::Read {
                url,
                script,
                follow,
            } => {
                self.read_sealed(&bot, &url, &script, follow.as_deref())
                    .await
            }
            SealedWork::Done => {
                if let Some(shared) = self.runtime_browser(&bot).await {
                    shared.browser.close_sealed().await;
                }
                SealedReply {
                    ok: true,
                    ..SealedReply::default()
                }
            }
        };
        call.reply.send(reply);
    }

    /// The browser `bot`'s session runs with.
    async fn runtime_browser(&self, bot: &BotId) -> Option<Arc<super::Shared>> {
        self.runtimes
            .lock()
            .await
            .get(bot)
            .and_then(|r| r.browser.clone())
    }

    /// Copies the page at `url` in `bot`'s browser and runs `script` on
    /// the copy in the sealed tab.
    async fn read_sealed(
        &self,
        bot: &BotId,
        url: &str,
        script: &str,
        follow: Option<&str>,
    ) -> SealedReply {
        let Some(shared) = self.runtime_browser(bot).await else {
            return SealedReply::failed("This bot has no browser right now.");
        };
        match self.take_browser(bot).await {
            Ok(true) => {}
            _ => return SealedReply::failed("The turn ended while it waited for the browser."),
        }
        let copied = match shared.browser.copy_page(url).await {
            Ok(c) => c,
            Err(err) => return SealedReply::failed(format!("the page couldn't be copied ({err})")),
        };
        // Only a web page, as Chrome itself says the tab shows.
        let at = if copied.url.is_empty() {
            url.to_owned()
        } else {
            copied.url.clone()
        };
        if web_host(&at).is_none() || is_home_network(&at) {
            return SealedReply::failed(format!("{at} isn't a web page oxbot reads"));
        }
        match shared
            .browser
            .read_sealed(&copied.html, &at, script, follow)
            .await
        {
            Ok(ran) => SealedReply {
                ok: true,
                value: ran.value,
                error: String::new(),
                next: ran.next,
                cut: copied.cut,
            },
            Err(err) => SealedReply {
                cut: copied.cut,
                ..SealedReply::failed(format!("the script failed: {err}"))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_are_named_by_their_main_domain() {
        assert_eq!(
            step_site_of("https://www.Amazon.ca/dp/1").as_deref(),
            Some("amazon.ca")
        );
        assert_eq!(
            step_site_of(" shop.example.test ").as_deref(),
            Some("example.test")
        );
        assert_eq!(step_site_of("amazon.ca").as_deref(), Some("amazon.ca"));
        assert_eq!(step_site_of("http://192.168.1.1/"), None);
        assert_eq!(step_site_of("localhost"), None);
        assert_eq!(step_site_of("file:///etc"), None);
        assert_eq!(step_site_of(""), None);
    }

    #[test]
    fn the_list_is_kept_sorted_once_each() {
        let on = with_site(vec!["b.com".into()], "a.com", true);
        assert_eq!(on, ["a.com", "b.com"]);
        assert_eq!(with_site(on.clone(), "a.com", true), on);
        assert_eq!(with_site(on, "a.com", false), ["b.com"]);
        assert_eq!(read_sites(Some("not json")), Vec::<String>::new());
        assert_eq!(read_sites(None), Vec::<String>::new());
    }

    #[test]
    fn refusals_say_where_it_was_chosen() {
        let text = steps_refusal(Some("example.com"));
        assert!(
            text.contains("example.com is set to fine-grained control"),
            "{text}"
        );
        assert!(text.contains("Settings → Websites"), "{text}");
        assert!(steps_refusal(None).contains("one at a time"));
    }

    #[tokio::test]
    async fn only_the_home_computer_sets_fine_grained_control_and_everyone_hears() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = Hub::new(
            vec![],
            crate::store::Store::open_in_memory().unwrap(),
            super::super::HubConfig::new(tmp.path()),
        );
        let mut updates = hub.subscribe();
        assert!(matches!(
            hub.set_fine_grained_site("shop.example.com", true, &Viewer::device("Sam")),
            Err(HubError::NotAllowed(_))
        ));
        assert!(matches!(
            hub.set_fine_grained_site("192.168.1.1", true, &Viewer::home_computer("Jay")),
            Err(HubError::BadSite(_))
        ));
        assert_eq!(hub.fine_grained_sites(), Vec::<String>::new());
        // By default every website may do several steps at once.
        assert_eq!(
            hub.steps_not_allowed(Some("https://shop.example.com/p/1")),
            None
        );
        hub.set_fine_grained_site(
            "https://www.shop.example.com/cart",
            true,
            &Viewer::home_computer("Jay"),
        )
        .unwrap();
        assert_eq!(hub.fine_grained_sites(), ["example.com"]);
        let heard = loop {
            if let ServerMsg::FineGrainedSites { sites } = updates.recv().await.unwrap() {
                break sites;
            }
        };
        assert_eq!(heard, ["example.com"]);
        // Every page of that site goes one step at a time; others don't.
        let no = hub
            .steps_not_allowed(Some("https://shop.example.com/p/1"))
            .unwrap();
        assert!(no.contains("example.com"), "{no}");
        assert_eq!(
            hub.steps_not_allowed(Some("https://forms.example.net/")),
            None
        );
        assert!(hub.steps_not_allowed(None).is_some());
        assert!(
            hub.steps_not_allowed(Some("http://192.168.1.1/")).is_some(),
            "never on the home network"
        );
        hub.set_fine_grained_site("example.com", false, &Viewer::home_computer("Jay"))
            .unwrap();
        assert_eq!(hub.fine_grained_sites(), Vec::<String>::new());
        assert_eq!(
            hub.steps_not_allowed(Some("https://shop.example.com/p/1")),
            None
        );
    }

    #[test]
    fn a_script_rule_reads_in_the_bots_settings() {
        let r = script_rule();
        assert_eq!(
            format!("{} {} on {}", r.action, r.label, r.site),
            "run small scripts in its own folder on the home computer"
        );
    }
}
