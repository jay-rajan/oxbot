//! Scripts and batches: what the hub does for `read_pages` (copying a page
//! of the bot's browser and running the bot's script on the copy in the
//! sealed tab), which websites allow `page_steps` (marked by the owner in
//! Settings → Websites), and the approval request before a bot runs a
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

/// Where the websites marked for several steps at once are kept (a JSON
/// list of main domains).
const PREF_STEP_SITES: &str = "page_steps_sites";

/// The most websites that may be marked.
const MAX_STEP_SITES: usize = 200;

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
            "Several steps at once aren't turned on for {site}. Do the steps one at a time. (The \
             person can turn it on for {site} in Settings → Websites.)"
        ),
        None => "Several steps at once only work on a website the person marked for it. Do the \
                 steps one at a time."
            .to_owned(),
    }
}

impl Hub {
    /// The websites where bots may do several steps at once, sorted.
    #[must_use]
    pub fn step_sites(&self) -> Vec<String> {
        read_sites(self.store.pref(PREF_STEP_SITES).ok().flatten().as_deref())
    }

    /// Lets bots do several steps at once on `site` (`on`), or not. The
    /// Farm only; everyone gets the new list.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] for anyone but the Farm,
    /// [`HubError::BadSite`] for something that isn't a website, or the
    /// database failed.
    pub fn set_step_site(&self, site: &str, on: bool, viewer: &Viewer) -> Result<(), HubError> {
        if !self.resolve(viewer).farm {
            return Err(HubError::NotAllowed(
                "Only the home computer can change where bots do several steps at once.".to_owned(),
            ));
        }
        let main = step_site_of(site).ok_or_else(|| HubError::BadSite(site.trim().to_owned()))?;
        let sites = with_site(self.step_sites(), &main, on);
        if sites.len() > MAX_STEP_SITES {
            return Err(HubError::NotAllowed(format!(
                "At most {MAX_STEP_SITES} websites can be marked."
            )));
        }
        let json = serde_json::to_string(&sites).unwrap_or_else(|_| "[]".to_owned());
        self.store.set_pref(PREF_STEP_SITES, &json)?;
        tracing::info!(site = main, on, "several steps at once");
        self.announce(ServerMsg::StepSites { sites });
        Ok(())
    }

    /// Why `page_steps` may not run on the page at `page_url`, or `None`
    /// if its website is marked for it.
    pub(super) fn steps_not_allowed(&self, page_url: Option<&str>) -> Option<String> {
        let site = page_url.and_then(step_site_of);
        match site {
            Some(site) if self.step_sites().contains(&site) => None,
            other => Some(steps_refusal(other.as_deref())),
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
    fn refusals_say_where_to_turn_it_on() {
        let text = steps_refusal(Some("example.com"));
        assert!(text.contains("aren't turned on for example.com"), "{text}");
        assert!(text.contains("Settings → Websites"), "{text}");
        assert!(steps_refusal(None).contains("one at a time"));
    }

    #[tokio::test]
    async fn only_the_home_computer_marks_websites_and_everyone_hears() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = Hub::new(
            vec![],
            crate::store::Store::open_in_memory().unwrap(),
            super::super::HubConfig::new(tmp.path()),
        );
        let mut updates = hub.subscribe();
        assert!(matches!(
            hub.set_step_site("shop.example.com", true, &Viewer::gate("Sam")),
            Err(HubError::NotAllowed(_))
        ));
        assert!(matches!(
            hub.set_step_site("192.168.1.1", true, &Viewer::farm("Jay")),
            Err(HubError::BadSite(_))
        ));
        assert_eq!(hub.step_sites(), Vec::<String>::new());
        hub.set_step_site(
            "https://www.shop.example.com/cart",
            true,
            &Viewer::farm("Jay"),
        )
        .unwrap();
        assert_eq!(hub.step_sites(), ["example.com"]);
        let heard = loop {
            if let ServerMsg::StepSites { sites } = updates.recv().await.unwrap() {
                break sites;
            }
        };
        assert_eq!(heard, ["example.com"]);
        // Any page of the site may do several steps at once; others not.
        assert_eq!(
            hub.steps_not_allowed(Some("https://shop.example.com/p/1")),
            None
        );
        let no = hub
            .steps_not_allowed(Some("https://forms.example.net/"))
            .unwrap();
        assert!(no.contains("example.net"), "{no}");
        assert!(hub.steps_not_allowed(None).is_some());
        hub.set_step_site("example.com", false, &Viewer::farm("Jay"))
            .unwrap();
        assert_eq!(hub.step_sites(), Vec::<String>::new());
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
