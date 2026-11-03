//! What the server and the page need to know about engines: who the owner
//! is, whether an engine can run, each bot's engine as listed, and the
//! one-time notice for the owner. The rules themselves live in
//! `engines.rs`; these are thin reads and writes around them.

use std::collections::BTreeSet;
use std::sync::PoisonError;

use oxbot_protocol::{BotInfo, EventKind, ReviewBot};

use super::engines::{ChainOrigin, entry_of};
use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::engine::EngineKind;
use crate::id::BotId;

/// Pref: the owner has seen the notice that Claude bots are owner-only.
const PREF_NOTICE_SEEN: &str = "engines_notice_seen";
/// Events read per page when looking back through a chat.
const LOOK_BACK_PAGE: usize = 500;

/// The notice in Settings → Engines, the README and the owner's banner.
pub const ENGINES_NOTICE: &str = "Claude bots run on your own Claude plan. Anthropic's terms don't \
     let others use your account, so only you can give them tasks. Everyone else uses OpenRouter bots.";

impl Hub {
    /// Whether `viewer` is the Farm's owner: the Farm connection, or the
    /// Farm's person (by id) on any device.
    #[must_use]
    pub fn is_owner_viewer(&self, viewer: &Viewer) -> bool {
        let viewer = self.resolve(viewer);
        self.is_owner(&ChainOrigin::from_viewer(&viewer, entry_of(&viewer)))
    }

    /// The Farm owner's name, for the page.
    #[must_use]
    pub fn owner_display_name(&self) -> String {
        self.owner_name()
    }

    /// Whether bots on `kind` can run now; if not, why, in words.
    ///
    /// # Errors
    /// What's missing.
    pub fn engine_ready(&self, kind: EngineKind) -> Result<(), String> {
        self.engine(kind).map(drop).map_err(|e| e.to_string())
    }

    /// The engine bot `id` runs on now, if it exists.
    #[must_use]
    pub fn engine_of_bot(&self, id: &str) -> Option<EngineKind> {
        self.bot(id).ok().map(|b| self.engine_of(&b))
    }

    /// Fills `info`'s engine, model and who alone may task it, from `bot`.
    pub(super) fn fill_engine(&self, bot: &Bot, info: &mut BotInfo) {
        let engine = self.engine_of(bot);
        info.engine = match engine {
            EngineKind::Claude => oxbot_protocol::EngineKind::Claude,
            EngineKind::OpenRouter => oxbot_protocol::EngineKind::OpenRouter,
        };
        info.model = match engine {
            EngineKind::Claude => bot.model.clone(),
            EngineKind::OpenRouter => bot.loop_settings.model.clone(),
        };
        info.daily_requests = match engine {
            EngineKind::Claude => None,
            EngineKind::OpenRouter => bot.loop_settings.daily_requests,
        };
        // Owner-only exactly when the rule would refuse someone else.
        let someone_else = ChainOrigin::default();
        info.task_owner = matches!(
            self.may_task(bot, &someone_else),
            Err(HubError::OwnerOnly { .. })
        )
        .then(|| self.farm_id().map(|id| id.to_string()).unwrap_or_default());
    }

    /// Whether the owner still has to see the notice.
    #[must_use]
    pub fn engines_notice_due(&self) -> bool {
        !matches!(self.store.pref(PREF_NOTICE_SEEN), Ok(Some(v)) if v == "1")
    }

    /// The owner has seen the notice.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub fn set_engines_notice_seen(&self) -> Result<(), HubError> {
        self.store.set_pref(PREF_NOTICE_SEEN, "1")?;
        Ok(())
    }

    /// Claude bots (Herders aside) that people other than the Farm's
    /// person sent messages to since `since_ms`, with who. Senders are
    /// matched to people by name, without making anyone new; names oxbot
    /// doesn't know are left out.
    #[must_use]
    pub fn claude_bots_others_used(&self, since_ms: i64) -> Vec<ReviewBot> {
        let farm = self.farm_id();
        let list: Vec<Bot> = self
            .bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|b| !b.is_central() && self.engine_of(b) == EngineKind::Claude)
            .cloned()
            .collect();
        list.into_iter()
            .filter_map(|b| {
                let names = self.senders_since(&b.id, since_ms);
                let used_by: Vec<String> = names
                    .into_iter()
                    .filter_map(|name| self.store.person_by_name(&name).ok().flatten())
                    .filter(|p| Some(&p.id) != farm.as_ref())
                    .map(|p| p.name)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                (!used_by.is_empty()).then(|| ReviewBot {
                    bot: b.id.to_string(),
                    name: b.name,
                    used_by,
                })
            })
            .collect()
    }

    /// Who sent `bot` messages since `since_ms`, newest pages first, by name.
    fn senders_since(&self, bot: &BotId, since_ms: i64) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut before = None;
        loop {
            let page = match self.store.history_page(bot, before, LOOK_BACK_PAGE) {
                Ok(page) => page,
                Err(err) => {
                    tracing::warn!(%bot, %err, "couldn't read a chat to review engines");
                    break;
                }
            };
            let mut older = false;
            for e in &page.events {
                if e.ts_ms < since_ms {
                    older = true;
                    continue;
                }
                if let EventKind::UserMessage {
                    from: Some(from), ..
                } = &e.kind
                {
                    names.insert(from.clone());
                }
            }
            if older || !page.has_older || page.first_seq == 0 {
                break;
            }
            before = Some(page.first_seq);
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::HubConfig;
    use crate::store::Store;

    fn hub(dir: &std::path::Path) -> Arc<Hub> {
        let mut scout = Bot::create(dir, BotId::new("scout").unwrap(), "Scout", "").unwrap();
        scout.engine = EngineKind::OpenRouter;
        scout.loop_settings.daily_requests = Some(20);
        scout.save().unwrap();
        let bots = vec![
            Bot::create(dir, BotId::new("assistant").unwrap(), "Herder", "").unwrap(),
            Bot::create(dir, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
            Bot::create(dir, BotId::new("notes").unwrap(), "Notes", "").unwrap(),
            scout,
        ];
        let config = HubConfig {
            pre_engine_tests: false,
            ..HubConfig::for_app(dir)
        };
        let hub = Hub::new(bots, Store::open_in_memory().unwrap(), config);
        hub.farm_person("Jay").unwrap();
        hub
    }

    #[tokio::test]
    async fn claude_bots_are_listed_as_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let jay = hub.farm_id().unwrap().to_string();
        let list = hub.bots().await.unwrap();
        let info = |id: &str| list.iter().find(|b| b.id == id).unwrap().clone();
        assert_eq!(info("recipes").task_owner.as_deref(), Some(jay.as_str()));
        assert_eq!(info("recipes").engine, oxbot_protocol::EngineKind::Claude);
        assert_eq!(info("scout").task_owner, None);
        assert_eq!(info("scout").engine, oxbot_protocol::EngineKind::OpenRouter);
        assert_eq!(info("scout").daily_requests, Some(20));

        let ann = hub.person_named("Ann").unwrap();
        let ann = Viewer::person(ann.id, "Ann", false);
        let seen = ann.redact(list.clone());
        let can = |id: &str| seen.iter().find(|b| b.id == id).unwrap().can_task;
        assert!(!can("recipes") && can("scout"));
        let jay_phone = Viewer::person(hub.farm_id().unwrap(), "Jay", false);
        assert!(jay_phone.redact(list.clone()).iter().all(|b| b.can_task));
        assert!(
            hub.farm_viewer("Jay")
                .redact(list)
                .iter()
                .all(|b| b.can_task)
        );
        assert!(hub.is_owner_viewer(&jay_phone) && !hub.is_owner_viewer(&ann));
    }

    #[tokio::test]
    async fn others_who_used_claude_bots_are_found_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        hub.person_named("Ann").unwrap();
        let said = |bot: &str, from: &str| {
            hub.store
                .append(
                    &BotId::new(bot).unwrap(),
                    EventKind::UserMessage {
                        text: "hi".into(),
                        from: Some(from.into()),
                        files: Vec::new(),
                    },
                )
                .unwrap();
        };
        said("recipes", "Ann");
        said("recipes", "Jay");
        said("recipes", "Ann");
        said("notes", "Jay");
        // Nobody oxbot knows: left out, and nobody is made.
        said("notes", "Stranger");
        said("scout", "Ann");
        let review = hub.claude_bots_others_used(0);
        assert_eq!(
            review,
            [ReviewBot {
                bot: "recipes".into(),
                name: "Recipes".into(),
                used_by: vec!["Ann".into()],
            }]
        );
        assert!(hub.store.person_by_name("Stranger").unwrap().is_none());
        // Only messages since then count.
        assert_eq!(hub.claude_bots_others_used(i64::MAX), []);

        assert!(hub.engines_notice_due());
        hub.set_engines_notice_seen().unwrap();
        assert!(!hub.engines_notice_due());
    }
}
