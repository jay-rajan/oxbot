//! "Always allow" rules: who may set one, setting it from the server's own
//! card, letting a matching click through, and listing or removing them.

use oxbot_browser::Browser;
use oxbot_protocol::{AllowRule, Approval, ApprovalKind, EventKind, ServerMsg};

use super::approvals::answers;
use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::id::BotId;
use crate::policy;
use crate::store::RuleKey;

impl Hub {
    /// Whether `viewer` may answer `card` with "Always allow": a change
    /// card the policy marked as one to remember (never pay, book, delete,
    /// Enter, uploads, Herder's cards), shown to them, on a bot they may
    /// manage (its owner or the Farm), that isn't Herder.
    #[must_use]
    pub fn can_always(&self, card: &Approval, viewer: &Viewer) -> bool {
        if card.kind != ApprovalKind::Change || card.rule.is_none() {
            return false;
        }
        let viewer = self.resolve(viewer);
        answers(card, &viewer)
            && self
                .bot_for(&card.bot, &viewer)
                .is_ok_and(|b| !b.is_central() && b.manageable_by(&viewer))
    }

    /// Saves the rule `card` offers, set by `viewer`. The rule comes from
    /// the card oxbot stored, never from what the client sent.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] if the card doesn't offer it to `viewer`;
    /// fails if the database fails.
    pub(super) fn add_rule_from(&self, card: &Approval, viewer: &Viewer) -> Result<(), HubError> {
        let label = card.rule.clone().filter(|_| self.can_always(card, viewer));
        let Some(label) = label else {
            return Err(HubError::NotAllowed(
                "Always allow isn't offered for this card.".to_owned(),
            ));
        };
        let bot = self.bot(&card.bot)?;
        let key = RuleKey {
            site: card.site.clone(),
            action: card.action.clone(),
            label,
        };
        let viewer = self.resolve(viewer);
        self.store
            .add_allow_rule(&bot.id, &key, viewer.id.as_ref())?;
        tracing::info!(bot = %bot.id, site = key.site, label = key.label, "always allowed");
        self.announce_rules(&bot.id);
        Ok(())
    }

    /// The rule that lets `ask` through for `bot` without a card, if one
    /// does. The caller still checks the tab really shows its site (see
    /// [`tab_shows`]).
    fn matching_rule(&self, bot: &Bot, ask: &policy::Ask) -> Result<Option<RuleKey>, HubError> {
        let Some(label) = ask.rule.clone() else {
            return Ok(None);
        };
        if bot.is_central() || ask.kind != ApprovalKind::Change {
            return Ok(None);
        }
        let key = RuleKey {
            site: ask.site.clone(),
            action: ask.action.clone(),
            label,
        };
        Ok(self.store.allow_rule_matches(&bot.id, &key)?.then_some(key))
    }

    /// Whether one of `bot`'s rules lets `ask` through without a card. If
    /// so, its chat gets a quiet line. The site must be the one Chrome itself
    /// says the tab at `page_url` shows (`browser`, oxbot's own
    /// connection), not only what the browser tool's text says.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub(super) async fn allowed_by_rule(
        &self,
        bot: &Bot,
        ask: &policy::Ask,
        browser: Option<&Browser>,
        page_url: Option<&str>,
    ) -> Result<bool, HubError> {
        let Some(rule) = self.matching_rule(bot, ask)? else {
            return Ok(false);
        };
        let tab_url = match browser {
            Some(browser) => browser.page_url(page_url).await,
            None => None,
        };
        if !tab_shows(&rule.site, tab_url.as_deref()) {
            tracing::info!(bot = %bot.id, site = rule.site, "a rule matched, but the tab isn't on its site: asking");
            return Ok(false);
        }
        tracing::info!(bot = %bot.id, site = rule.site, label = rule.label, "allowed by a rule");
        self.record(
            &bot.id,
            EventKind::AllowedByRule {
                action: ask.action.clone(),
                target: ask.target.clone(),
                site: ask.site.clone(),
            },
        )?;
        Ok(true)
    }

    /// Bot `id`'s rules, for someone who may manage it.
    ///
    /// # Errors
    /// Fails if `viewer` may not manage it or the database fails.
    pub fn allow_rules_for(&self, id: &str, viewer: &Viewer) -> Result<Vec<AllowRule>, HubError> {
        let viewer = self.resolve(viewer);
        let bot = self.bot_to_manage(id, &viewer)?;
        self.allow_rules(&bot.id)
    }

    /// Removes bot `id`'s rule `rule`; the same click asks again.
    ///
    /// # Errors
    /// Fails if `viewer` may not manage it or the database fails.
    pub fn remove_allow_rule(&self, id: &str, rule: i64, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = self.resolve(viewer);
        let bot = self.bot_to_manage(id, &viewer)?;
        if self.store.remove_allow_rule(&bot.id, rule)? {
            tracing::info!(bot = %bot.id, rule, "removed an always-allow rule");
        }
        self.announce_rules(&bot.id);
        Ok(())
    }

    /// Whether `viewer` may manage bot `id` (and so see its rules).
    #[must_use]
    pub fn may_manage(&self, id: &str, viewer: &Viewer) -> bool {
        self.bot_to_manage(id, &self.resolve(viewer)).is_ok()
    }

    fn allow_rules(&self, bot: &BotId) -> Result<Vec<AllowRule>, HubError> {
        Ok(self
            .store
            .allow_rules(bot)?
            .into_iter()
            .map(|stored| AllowRule {
                by: stored.by_person.and_then(|id| self.person_name(&id)),
                ..stored.rule
            })
            .collect())
    }

    /// Tells clients `bot`'s rules changed (the server passes it only to
    /// those who may manage it).
    fn announce_rules(&self, bot: &BotId) {
        match self.allow_rules(bot) {
            Ok(rules) => self.announce(ServerMsg::AllowRules {
                bot: bot.to_string(),
                rules,
            }),
            Err(err) => tracing::warn!(%bot, %err, "couldn't read always-allow rules"),
        }
    }
}

/// Whether the tab Chrome itself reports (`tab_url`, from oxbot's own
/// connection) is on `site`, so a rule for `site` may apply. The site the
/// browser tool names can come from a page's title; this can't. No answer
/// (no guard) means no. A blank tab counts: a click there can't reach any
/// site.
pub(super) fn tab_shows(site: &str, tab_url: Option<&str>) -> bool {
    match tab_url {
        Some("about:blank") => true,
        Some(url) => oxbot_browser::web_host(url).is_some_and(|host| host == site),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::hub::HubConfig;
    use crate::id::PersonId;
    use crate::store::Store;

    #[test]
    fn only_the_tab_chrome_reports_counts() {
        assert!(tab_shows(
            "www.example.com",
            Some("https://www.example.com/a?b")
        ));
        assert!(!tab_shows("www.example.com", Some("https://example.com/")));
        assert!(!tab_shows("example.com", Some("https://evil.example.net/")));
        assert!(!tab_shows("example.com", None), "no guard: no rule");
        assert!(!tab_shows("example.com", Some("chrome://newtab/")));
        assert!(tab_shows("example.com", Some("about:blank")));
    }

    /// A hub with Sam's shared bot `shop`, Herder, and Jay as the Farm's person.
    fn hub(dir: &std::path::Path) -> (Arc<Hub>, PersonId, PersonId) {
        let bots = dir.join("bots");
        std::fs::create_dir_all(&bots).unwrap();
        let store = Store::open_in_memory().unwrap();
        let jay = store.find_or_create_person("Jay").unwrap().0.id;
        let sam = store.find_or_create_person("Sam").unwrap().0.id;
        let mut shop = Bot::create(&bots, BotId::new("shop").unwrap(), "Shop", "").unwrap();
        shop.owner = Some(sam.clone());
        shop.created_by = Some("Sam".into());
        shop.save().unwrap();
        let herder = Bot::create(
            &bots,
            BotId::new(crate::bot::DEFAULT_BOT).unwrap(),
            "Herder",
            "",
        )
        .unwrap();
        let hub = Hub::new(vec![shop, herder], store, HubConfig::new(&bots));
        (hub, jay, sam)
    }

    fn save_card(asker: &PersonId) -> Approval {
        Approval {
            id: 1,
            bot: "shop".into(),
            kind: ApprovalKind::Change,
            action: "click".into(),
            target: "button \"Save\"".into(),
            site: "www.example.com".into(),
            page: None,
            detail: None,
            screenshot: None,
            asker: Some("Sam".into()),
            asker_id: Some(asker.to_string()),
            always: false,
            rule: Some("button \"Save\"".into()),
        }
    }

    fn save_ask(site: &str, label: &str) -> policy::Ask {
        policy::Ask {
            rule: Some(label.into()),
            ..policy::Ask::new(ApprovalKind::Change, "click", label, site)
        }
    }

    #[tokio::test]
    async fn a_rule_matches_only_its_bot_host_and_label_and_goes_when_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, _, sam) = hub(tmp.path());
        let sam_viewer = Viewer::gate("Sam");
        hub.add_rule_from(&save_card(&sam), &sam_viewer).unwrap();
        let shop = hub.bot("shop").unwrap();
        let save = save_ask("www.example.com", "button \"Save\"");
        assert!(hub.matching_rule(&shop, &save).unwrap().is_some());
        // Another host, label or bot still asks.
        for other in [
            save_ask("example.com", "button \"Save\""),
            save_ask("www.example.com", "button \"Save draft\""),
            save_ask("www.example.com", "button \"save\""),
        ] {
            assert!(
                hub.matching_rule(&shop, &other).unwrap().is_none(),
                "{other:?}"
            );
        }
        let herder = hub.bot(crate::bot::DEFAULT_BOT).unwrap();
        assert!(hub.matching_rule(&herder, &save).unwrap().is_none());
        // A card that names no rule (Enter, pay, uploads) never matches.
        let plain = policy::Ask::new(
            ApprovalKind::Change,
            "click",
            "button \"Save\"",
            "www.example.com",
        );
        assert!(hub.matching_rule(&shop, &plain).unwrap().is_none());

        let rules = hub.allow_rules_for("shop", &sam_viewer).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].by.as_deref(), Some("Sam"));
        assert_eq!(rules[0].label, "button \"Save\"");
        hub.remove_allow_rule("shop", rules[0].id, &sam_viewer)
            .unwrap();
        assert!(hub.matching_rule(&shop, &save).unwrap().is_none());
    }

    #[tokio::test]
    async fn only_the_bots_owner_or_the_farm_may_always_allow() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, sam) = hub(tmp.path());
        let card = save_card(&sam);
        assert!(hub.can_always(&card, &Viewer::gate("Sam")));
        assert!(hub.can_always(&card, &Viewer::farm("Jay")));
        // Jay on a phone isn't shown a card Sam's turn raised.
        assert!(!hub.can_always(&card, &Viewer::gate("Jay")));
        // A card Jay's turn raised on Sam's bot: Jay answers it, but only
        // once (he doesn't own the bot).
        let jays = Approval {
            asker: Some("Jay".into()),
            asker_id: Some(jay.to_string()),
            ..save_card(&sam)
        };
        assert!(!hub.can_always(&jays, &Viewer::gate("Jay")));
        assert!(matches!(
            hub.add_rule_from(&jays, &Viewer::gate("Jay")),
            Err(HubError::NotAllowed(_))
        ));
        // Cards the policy didn't mark, and other kinds, never.
        for refused in [
            Approval {
                rule: None,
                ..save_card(&sam)
            },
            Approval {
                kind: ApprovalKind::Upload,
                ..save_card(&sam)
            },
            Approval {
                kind: ApprovalKind::CreateBot,
                ..save_card(&sam)
            },
            Approval {
                kind: ApprovalKind::GiveTask,
                ..save_card(&sam)
            },
            Approval {
                bot: crate::bot::DEFAULT_BOT.into(),
                ..save_card(&sam)
            },
        ] {
            assert!(
                !hub.can_always(&refused, &Viewer::farm("Jay")),
                "{refused:?}"
            );
            assert!(hub.add_rule_from(&refused, &Viewer::farm("Jay")).is_err());
        }
        assert_eq!(
            hub.allow_rules_for("shop", &Viewer::farm("Jay")).unwrap(),
            []
        );
        // Listing and removing are for those who manage the bot.
        assert!(hub.allow_rules_for("shop", &Viewer::gate("Jay")).is_err());
        assert!(
            hub.remove_allow_rule("shop", 1, &Viewer::gate("Jay"))
                .is_err()
        );
        assert!(!hub.may_manage("shop", &Viewer::gate("Jay")));
        assert!(hub.may_manage("shop", &Viewer::gate("Sam")));
    }
}
