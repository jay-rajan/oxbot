//! What the server and the page need to know about bots' AI models: who
//! the owner is, whether bots can run, each bot's engine and model as
//! listed, and the household's model settings. The rules themselves live in `engines.rs`;
//! these are thin reads and writes around them.

use std::collections::BTreeSet;

use oxbot_protocol::{BotInfo, EnginePrefs, RouterUsage, ServerMsg};

use super::engines::{ChainOrigin, entry_of};
use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::engine::EngineKind;
use crate::id::BotId;

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
/// The most models in a bot's own list.
pub const MAX_BOT_MODELS: usize = 8;
/// The highest daily request cap (per bot or per person).
const MAX_DAILY_REQUESTS: u32 = 100_000;
/// The highest monthly spend on paid models, in US cents ($500).
const MAX_PAID_MONTHLY_CENTS: u32 = 50_000;

/// The notice in Settings → Engines and the README: where bots' model
/// calls go.
pub const ENGINES_NOTICE: &str = "Bots use AI models through the home's OpenRouter account, \
     free ones first; anyone at home can give them tasks. What a task sends goes only to \
     OpenRouter (or a model on this computer), never anywhere else.";

impl Hub {
    /// Whether `viewer` is the home computer's owner: the home computer connection, or the
    /// home computer's person (by id) on any device.
    #[must_use]
    pub fn is_owner_viewer(&self, viewer: &Viewer) -> bool {
        let viewer = self.resolve(viewer);
        self.is_owner(&ChainOrigin::from_viewer(&viewer, entry_of(&viewer)))
    }

    /// The home computer owner's name, for the page.
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

    /// Whether the household's `OpenRouter` sign-in is ready: bots can't
    /// use models without it (and Codex needs it too).
    #[must_use]
    pub fn openrouter_ready(&self) -> bool {
        self.loop_engine.as_ref().is_some_and(|e| e.ready().is_ok())
    }

    /// The engine bot `id` runs on now, if it exists.
    #[must_use]
    pub fn engine_of_bot(&self, id: &str) -> Option<EngineKind> {
        self.bot(id).ok().map(|b| self.engine_of(&b))
    }

    /// Fills `info`'s engine, model and daily limit, from `bot`.
    pub(super) fn fill_engine(&self, bot: &Bot, info: &mut BotInfo) {
        info.engine = crate::engine::to_wire(self.engine_of(bot));
        info.model.clone_from(&bot.loop_settings.model);
        info.models.clone_from(&bot.loop_settings.models);
        info.daily_requests = bot.loop_settings.daily_requests;
    }

    /// The household's engine settings (defaults until changed).
    #[must_use]
    pub fn engine_prefs(&self) -> EnginePrefs {
        engine_prefs_in(&self.store)
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
        self.loop_prefs_changed(&prefs);
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
        self.loop_catalog_changed(json, fetched_ms);
        Ok(())
    }

    /// Today's `OpenRouter` use (UTC day), per bot and per person, busiest
    /// first. Read from the meter's records of each model request; empty
    /// until bots run on `OpenRouter`.
    #[must_use]
    pub fn openrouter_usage_today(&self) -> Vec<RouterUsage> {
        let day = oxbot_loop::meter::utc_day(crate::store::now_ms());
        let usage = match self.store.day_usage(&day) {
            Ok(usage) => usage,
            Err(err) => {
                tracing::warn!(%err, "couldn't read today's OpenRouter use");
                return Vec::new();
            }
        };
        let row = |t: &crate::store::CallTotals| RouterUsage {
            bot: None,
            person: None,
            requests: t.requests,
            prompt_tokens: t.prompt_tokens,
            completion_tokens: t.completion_tokens,
            cost_usd: t.cost_usd,
            model: t.main_model.clone(),
        };
        let mut rows: Vec<RouterUsage> = usage
            .bots
            .iter()
            .map(|(bot, t)| RouterUsage {
                bot: Some(bot.clone()),
                ..row(t)
            })
            .chain(usage.people.iter().filter_map(|(id, t)| {
                let name = crate::id::PersonId::new(id.clone())
                    .ok()
                    .and_then(|id| self.person_name(&id))?;
                Some(RouterUsage {
                    person: Some(name),
                    ..row(t)
                })
            }))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.requests));
        rows
    }

    /// Sets an `OpenRouter` bot's own cap on model requests a day (`None`:
    /// the household's).
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's an Assistant, the number is out
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
                "The Assistant's limits are the home's.".to_owned(),
            ));
        }
        if let Some(n) = daily_requests
            && !(1..=MAX_DAILY_REQUESTS).contains(&n)
        {
            return Err(HubError::NotAllowed(format!(
                "A daily limit is 1 to {MAX_DAILY_REQUESTS} AI questions."
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
            self.stop_session(&id).await;
        }
    }
}

/// The household's engine settings in `store` (defaults until changed).
pub(super) fn engine_prefs_in(store: &crate::store::Store) -> EnginePrefs {
    match store.pref(PREF_ENGINE_PREFS) {
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

/// Whether `id` (trimmed) can be a model id: 1 to [`MAX_MODEL_ID`]
/// printable ASCII characters, no spaces.
fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_MODEL_ID && id.chars().all(|c| c.is_ascii_graphic())
}

/// A bot's own model list with each id trimmed and each once (in its
/// order), or why it can't be kept.
///
/// # Errors
/// [`HubError::NotAllowed`], in words: too many models, or one that isn't
/// a model id.
pub fn checked_models(models: Vec<String>) -> Result<Vec<String>, HubError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for id in models {
        let id = id.trim().to_owned();
        if !id_ok(&id) {
            return Err(HubError::NotAllowed(
                "That list has something that isn't a model id oxbot can use.".to_owned(),
            ));
        }
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    if out.len() > MAX_BOT_MODELS {
        return Err(HubError::NotAllowed(format!(
            "A bot can have at most {MAX_BOT_MODELS} models in its list."
        )));
    }
    Ok(out)
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
            "A daily limit is 1 to {MAX_DAILY_REQUESTS} AI questions."
        ));
    }
    if prefs.paid_monthly_cents > MAX_PAID_MONTHLY_CENTS {
        return refuse("Paid models may spend at most $500.00 a month.".to_owned());
    }
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
    prefs.assistant_model = prefs
        .assistant_model
        .map(|m| m.trim().to_owned())
        .filter(|m| !m.is_empty() && m != "auto");
    if prefs.assistant_model.as_deref().is_some_and(|m| !id_ok(m)) {
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
        scout.loop_settings.daily_requests = Some(20);
        scout.save().unwrap();
        let bots = vec![
            Bot::create(dir, BotId::new("assistant").unwrap(), "Assistant", "").unwrap(),
            Bot::create(dir, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
            Bot::create(dir, BotId::new("notes").unwrap(), "Notes", "").unwrap(),
            scout,
        ];
        let hub = Hub::new(bots, Store::open_in_memory().unwrap(), HubConfig::new(dir));
        hub.owner_person("Jay").unwrap();
        hub
    }

    #[tokio::test]
    async fn bots_are_listed_with_their_engine_and_anyone_may_task_them() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let list = hub.bots().await.unwrap();
        let info = |id: &str| list.iter().find(|b| b.id == id).unwrap().clone();
        assert_eq!(info("recipes").engine, oxbot_protocol::EngineKind::Codex);
        assert_eq!(info("assistant").engine, oxbot_protocol::EngineKind::Codex);
        assert_eq!(info("scout").engine, oxbot_protocol::EngineKind::Codex);
        assert_eq!(info("scout").daily_requests, Some(20));

        let ann = hub.person_named("Ann").unwrap();
        let ann = Viewer::person(ann.id, "Ann", false);
        let seen = ann.redact(list.clone());
        let can = |id: &str| seen.iter().find(|b| b.id == id).unwrap().can_task;
        assert!(can("recipes") && can("scout") && can("notes"));
        let jay_phone = Viewer::person(hub.owner_person_id().unwrap(), "Jay", false);
        assert!(hub.is_owner_viewer(&jay_phone) && !hub.is_owner_viewer(&ann));
    }

    #[test]
    fn paid_spending_is_capped_at_500_dollars_a_month() {
        let at = |cents| {
            checked_prefs(EnginePrefs {
                paid_monthly_cents: cents,
                ..EnginePrefs::default()
            })
        };
        assert_eq!(EnginePrefs::default().paid_monthly_cents, 1000, "$10.00");
        assert_eq!(at(50_000).unwrap().paid_monthly_cents, 50_000);
        assert_eq!(at(0).unwrap().paid_monthly_cents, 0, "no paid spending");
        match at(50_001) {
            Err(HubError::NotAllowed(why)) => {
                assert_eq!(why, "Paid models may spend at most $500.00 a month.");
            }
            other => panic!("{other:?}"),
        }
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
                assistant_model: Some("auto".into()),
                ..EnginePrefs::default()
            })
            .unwrap();
        assert_eq!(kept.paid_models, ["vendor/small"]);
        assert_eq!(kept.assistant_model, None);
        assert_eq!(hub.engine_prefs(), kept);
        for bad in [
            EnginePrefs {
                bot_daily_requests: 0,
                ..EnginePrefs::default()
            },
            EnginePrefs {
                paid_monthly_cents: MAX_PAID_MONTHLY_CENTS + 1,
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
    async fn a_bots_own_model_list_is_checked_kept_and_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let jay = hub.home_computer_viewer("Jay");
        hub.set_bot_models(
            "scout",
            vec![" x/b:free ".into(), "x/paid".into(), "x/b:free".into()],
            &jay,
        )
        .await
        .unwrap();
        let scout = hub.bot("scout").unwrap();
        assert_eq!(scout.loop_settings.models, ["x/b:free", "x/paid"]);
        let list = hub.bots().await.unwrap();
        let info = list.iter().find(|b| b.id == "scout").unwrap();
        assert_eq!(info.models, ["x/b:free", "x/paid"]);

        let nine: Vec<String> = (0..9).map(|i| format!("x/m{i}:free")).collect();
        for (bad, why) in [
            (nine, "A bot can have at most 8 models in its list."),
            (
                vec!["x/a b".into()],
                "That list has something that isn't a model id oxbot can use.",
            ),
            (
                vec![" ".into()],
                "That list has something that isn't a model id oxbot can use.",
            ),
            (
                vec!["x/".repeat(101)],
                "That list has something that isn't a model id oxbot can use.",
            ),
        ] {
            match hub.set_bot_models("scout", bad, &jay).await {
                Err(HubError::NotAllowed(said)) => assert_eq!(said, why),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(
            hub.bot("scout").unwrap().loop_settings.models,
            ["x/b:free", "x/paid"],
            "a refused list keeps the old"
        );
        // The Assistant's is the owner's to set, in Settings.
        assert!(matches!(
            hub.set_bot_models("assistant", vec!["x/a:free".into()], &jay)
                .await,
            Err(HubError::NotAllowed(_))
        ));
        // Back to automatic.
        hub.set_bot_models("scout", Vec::new(), &jay).await.unwrap();
        assert_eq!(
            hub.bot("scout").unwrap().loop_settings.models,
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn a_bots_own_daily_limit_is_its_managers_to_set() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let jay = hub.home_computer_viewer("Jay");
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
