//! What the server and the page need to know about engines: who the owner
//! is, whether an engine can run, each bot's engine as listed, and the
//! one-time notice for the owner. The rules themselves live in
//! `engines.rs`; these are thin reads and writes around them.

use std::collections::BTreeSet;
use std::sync::PoisonError;

use oxbot_protocol::{BotInfo, EnginePrefs, EventKind, ReviewBot, RouterUsage, ServerMsg};

use super::engines::{ChainOrigin, entry_of};
use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::engine::EngineKind;
use crate::id::BotId;

/// Pref: the owner has seen the notice that Claude bots are owner-only.
const PREF_NOTICE_SEEN: &str = "engines_notice_seen";
/// Events read per page when looking back through a chat.
const LOOK_BACK_PAGE: usize = 500;
/// Pref: the household's engine settings ([`EnginePrefs`] as JSON).
const PREF_ENGINE_PREFS: &str = "engine_prefs";
/// Pref: `OpenRouter`'s model list as last read (`oxbot_llm::Catalog` as
/// JSON), so a restart offline still routes.
pub const PREF_CATALOG: &str = "openrouter_catalog";
/// Pref: when the model list was read (ms since the Unix epoch).
pub const PREF_CATALOG_MS: &str = "openrouter_catalog_ms";
/// The most paid models the owner may pick.
const MAX_PAID_MODELS: usize = 50;
/// The longest model id kept.
const MAX_MODEL_ID: usize = 200;
/// The highest daily request cap (per bot or per person).
const MAX_DAILY_REQUESTS: u32 = 100_000;
/// The highest daily spend on paid models, in US cents ($100).
const MAX_PAID_DAILY_CENTS: u32 = 10_000;

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

    /// The household's engine settings (defaults until changed).
    #[must_use]
    pub fn engine_prefs(&self) -> EnginePrefs {
        match self.store.pref(PREF_ENGINE_PREFS) {
            Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_else(|err| {
                tracing::warn!(%err, "engine settings unreadable; using the defaults");
                EnginePrefs::default()
            }),
            Ok(None) => EnginePrefs::default(),
            Err(err) => {
                tracing::warn!(%err, "couldn't read the engine settings");
                EnginePrefs::default()
            }
        }
    }

    /// Changes the household's engine settings.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] for a value out of range, or the database
    /// fails.
    pub fn set_engine_prefs(&self, prefs: EnginePrefs) -> Result<EnginePrefs, HubError> {
        let prefs = checked_prefs(prefs)?;
        let json =
            serde_json::to_string(&prefs).map_err(|e| HubError::NotAllowed(e.to_string()))?;
        self.store.set_pref(PREF_ENGINE_PREFS, &json)?;
        tracing::info!(paid = prefs.paid_fallback, "changed the engine settings");
        Ok(prefs)
    }

    /// `OpenRouter`'s model list as last read (`oxbot_llm::Catalog` JSON)
    /// and when, if ever.
    #[must_use]
    pub fn model_catalog(&self) -> Option<(String, i64)> {
        let json = self.store.pref(PREF_CATALOG).ok().flatten()?;
        let ms = self
            .store
            .pref(PREF_CATALOG_MS)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Some((json, ms))
    }

    /// Keeps the model list just read (`oxbot_llm::Catalog` JSON).
    ///
    /// # Errors
    /// Fails if the database fails.
    pub fn set_model_catalog(&self, json: &str, fetched_ms: i64) -> Result<(), HubError> {
        self.store.set_pref(PREF_CATALOG, json)?;
        self.store
            .set_pref(PREF_CATALOG_MS, &fetched_ms.to_string())?;
        Ok(())
    }

    /// Today's `OpenRouter` use (UTC day), per bot and per person, busiest
    /// first. Read from the meter's records of each model request; empty
    /// until bots run on `OpenRouter`.
    #[must_use]
    pub fn openrouter_usage_today(&self) -> Vec<RouterUsage> {
        // The loop's meter (model_calls) fills this; nothing is recorded
        // before it runs.
        Vec::new()
    }

    /// Sets an `OpenRouter` bot's own cap on model requests a day (`None`:
    /// the household's).
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's a Herder, the number is out
    /// of range, or its file can't be written.
    pub async fn set_bot_daily_limit(
        &self,
        id: &str,
        daily_requests: Option<u32>,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "Herder's limits are the household's.".to_owned(),
            ));
        }
        if let Some(n) = daily_requests
            && !(1..=MAX_DAILY_REQUESTS).contains(&n)
        {
            return Err(HubError::NotAllowed(format!(
                "A daily limit is 1 to {MAX_DAILY_REQUESTS} requests."
            )));
        }
        self.update_bot(&bot.id, |b| b.loop_settings.daily_requests = daily_requests)?;
        self.announce_bots().await;
        Ok(())
    }

    /// Tells those who see `bot` what its turn used (the chat's footer).
    pub fn announce_meter(&self, bot: &BotId, model: &str, requests: u32, cost_usd: f64) {
        self.announce(ServerMsg::Meter {
            bot: bot.to_string(),
            model: model.to_owned(),
            requests,
            cost_usd,
        });
    }

    /// Stops every running session on `kind` (after signing out of
    /// `OpenRouter`, say). Their chats stay; the next message starts anew.
    pub async fn stop_engine_sessions(&self, kind: EngineKind) {
        let ids: Vec<BotId> = self
            .runtimes
            .lock()
            .await
            .iter()
            .filter(|(_, r)| r.engine == kind)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.stop_claude(&id).await;
        }
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

/// `prefs` with ids trimmed, or why it can't be kept.
fn checked_prefs(mut prefs: EnginePrefs) -> Result<EnginePrefs, HubError> {
    let refuse = |why: String| Err(HubError::NotAllowed(why));
    if !(1..=MAX_DAILY_REQUESTS).contains(&prefs.bot_daily_requests)
        || prefs
            .person_daily_requests
            .is_some_and(|n| !(1..=MAX_DAILY_REQUESTS).contains(&n))
    {
        return refuse(format!(
            "A daily limit is 1 to {MAX_DAILY_REQUESTS} requests."
        ));
    }
    if prefs.paid_daily_cents > MAX_PAID_DAILY_CENTS {
        return refuse("Paid models may spend at most $100.00 a day.".to_owned());
    }
    let id_ok = |id: &str| {
        !id.is_empty() && id.len() <= MAX_MODEL_ID && id.chars().all(|c| c.is_ascii_graphic())
    };
    prefs.paid_models = prefs
        .paid_models
        .iter()
        .map(|m| m.trim().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if prefs.paid_models.len() > MAX_PAID_MODELS || !prefs.paid_models.iter().all(|m| id_ok(m)) {
        return refuse("Those aren't model ids oxbot can use.".to_owned());
    }
    prefs.herder_model = prefs
        .herder_model
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty() && m != "auto");
    if prefs.herder_model.as_deref().is_some_and(|m| !id_ok(m)) {
        return refuse("That isn't a model id oxbot can use.".to_owned());
    }
    Ok(prefs)
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

    #[tokio::test]
    async fn engine_settings_are_checked_and_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        assert_eq!(hub.engine_prefs(), EnginePrefs::default());
        assert_eq!(hub.engine_prefs().bot_daily_requests, 200);
        let kept = hub
            .set_engine_prefs(EnginePrefs {
                paid_fallback: true,
                paid_models: vec![" vendor/small ".into(), "vendor/small".into()],
                herder_model: Some("auto".into()),
                ..EnginePrefs::default()
            })
            .unwrap();
        assert_eq!(kept.paid_models, ["vendor/small"]);
        assert_eq!(kept.herder_model, None);
        assert_eq!(hub.engine_prefs(), kept);
        for bad in [
            EnginePrefs {
                bot_daily_requests: 0,
                ..EnginePrefs::default()
            },
            EnginePrefs {
                paid_daily_cents: 1_000_000,
                ..EnginePrefs::default()
            },
            EnginePrefs {
                paid_models: vec!["a b".into()],
                ..EnginePrefs::default()
            },
        ] {
            assert!(hub.set_engine_prefs(bad).is_err());
        }
        assert_eq!(hub.engine_prefs(), kept, "a refused change keeps the old");

        assert_eq!(hub.model_catalog(), None);
        hub.set_model_catalog(r#"{"models":[]}"#, 5).unwrap();
        assert_eq!(
            hub.model_catalog(),
            Some((r#"{"models":[]}"#.to_owned(), 5))
        );
    }

    #[tokio::test]
    async fn a_bots_own_daily_limit_is_its_managers_to_set() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let jay = hub.farm_viewer("Jay");
        hub.set_bot_daily_limit("scout", Some(2), &jay)
            .await
            .unwrap();
        assert_eq!(
            hub.bot("scout").unwrap().loop_settings.daily_requests,
            Some(2)
        );
        assert!(
            hub.set_bot_daily_limit("scout", Some(0), &jay)
                .await
                .is_err()
        );
        assert!(
            hub.set_bot_daily_limit("assistant", Some(5), &jay)
                .await
                .is_err()
        );
        let ann = hub.person_named("Ann").unwrap();
        let ann = Viewer::person(ann.id, "Ann", false);
        assert!(hub.set_bot_daily_limit("scout", None, &ann).await.is_err());
        hub.set_bot_daily_limit("scout", None, &jay).await.unwrap();
        assert_eq!(hub.bot("scout").unwrap().loop_settings.daily_requests, None);
    }
}
