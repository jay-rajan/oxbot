//! The hub's side of what Codex bots share with oxbot (`oxbot-loop`, from
//! oxbot's own loop, which retired as an engine): building it with the
//! hub's store, limits and browser; the meter and its alerts; and the
//! sweep of stored results.

use std::sync::{Arc, PoisonError, Weak};

use oxbot_engine::{CallRecord, LimitWindow};
use oxbot_llm::{Catalog, ChatModel, KeyCell};
use oxbot_loop::meter::{next_utc_month_ms, now_ms, utc_day, utc_month_start};
use oxbot_loop::{
    Account, BrowserFactory, CatalogCache, GuardQuery, LimitHit, LoopEngine, LoopPrefs, LoopSetup,
    LoopStore, McpBrowserFactory, Pacer, UsageGuard,
};
use oxbot_protocol::{EnginePrefs, EnginesView, ServerMsg};

use super::engine_prefs::{PREF_CATALOG, PREF_CATALOG_MS, engine_prefs_in};
use super::{Hub, HubConfig};
use crate::id::{BotId, PersonId};
use crate::policy::LOOP_BROWSER_TOOLS;
use crate::store::Store;

/// What the hub needs to run bots on `OpenRouter`: the model client and its
/// key. Everything else (store, limits, browser) comes from the hub.
#[derive(Clone)]
pub struct LoopParts {
    /// The chat model: `OpenRouter`'s client (a scripted model in tests).
    pub model: Arc<dyn ChatModel>,
    /// The key: bots on `OpenRouter` run while it's set. `None`: no key
    /// needed (tests).
    pub key: Option<KeyCell>,
    /// Reads the key's limits (the free daily budget).
    pub account: Option<Arc<dyn Account>>,
    /// Makes each session's browser tool; `None`: `chrome-devtools-mcp`
    /// (or the e2e stand-in) over MCP.
    pub browser: Option<Arc<dyn BrowserFactory>>,
    /// Free model requests a minute, household-wide.
    pub free_per_minute: usize,
}

impl LoopParts {
    /// Parts for `model` with `key`, reading limits from `account`.
    #[must_use]
    pub fn new(
        model: Arc<dyn ChatModel>,
        key: Option<KeyCell>,
        account: Option<Arc<dyn Account>>,
    ) -> Self {
        Self {
            model,
            key,
            account,
            browser: None,
            free_per_minute: oxbot_loop::pacer::FREE_PER_MINUTE,
        }
    }
}

impl LoopParts {
    /// Parts for `OpenRouter` through `client`, with the household's `key`.
    #[must_use]
    pub fn openrouter(client: &oxbot_llm::OpenAiClient, key: KeyCell) -> Self {
        Self::new(
            Arc::new(client.clone()),
            Some(key),
            Some(Arc::new(client.clone())),
        )
    }
}

impl std::fmt::Debug for LoopParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopParts")
            .field("model", &self.model)
            .field("free_per_minute", &self.free_per_minute)
            .finish_non_exhaustive()
    }
}

/// One loop turn's model requests, for the chat footer.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct TurnMeter {
    pub requests: u32,
    pub cost_usd: f64,
    pub model: String,
}

/// The loop's household settings from the owner's engine settings.
fn loop_prefs(p: &EnginePrefs) -> LoopPrefs {
    LoopPrefs {
        paid_fallback: p.paid_fallback,
        paid_models: p.paid_models.clone(),
        assistant_model: p.assistant_model.clone(),
        ..LoopPrefs::default()
    }
}

/// The model list kept in `store`, if any.
fn kept_catalog(store: &Store) -> Option<(Catalog, i64)> {
    let json = store.pref(PREF_CATALOG).ok().flatten()?;
    let catalog = serde_json::from_str(&json).ok()?;
    let ms = store
        .pref(PREF_CATALOG_MS)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    Some((catalog, ms))
}

/// The loop engine for `parts`, on the hub's store, limits and browser.
pub(super) fn build_engine(
    parts: &LoopParts,
    store: &Arc<Store>,
    config: &HubConfig,
    hub: Weak<Hub>,
) -> Arc<LoopEngine> {
    let loop_store: Arc<dyn LoopStore> = Arc::clone(store) as Arc<dyn LoopStore>;
    let catalog = Arc::new(CatalogCache::default());
    if let Some((list, ms)) = kept_catalog(store) {
        catalog.set(list, ms);
    }
    let stand_in = config
        .browser
        .as_ref()
        .is_some_and(|t| t.config().mcp_override.is_some());
    let browser = parts.browser.clone().unwrap_or_else(|| {
        // The tool list is cached per tool version (the stand-in apart).
        let key = if stand_in {
            "devtools_tools:stand-in".to_owned()
        } else {
            format!("devtools_tools:{}", oxbot_browser::MCP_VERSION)
        };
        Arc::new(McpBrowserFactory::new(Arc::clone(&loop_store), key))
    });
    let mut setup = LoopSetup::new(Arc::clone(&parts.model), loop_store);
    setup.catalog = catalog;
    setup.guard = Arc::new(HubGuard(hub));
    setup.pacer = Pacer::with_rate(parts.free_per_minute, parts.account.clone());
    *setup.prefs.write().unwrap_or_else(PoisonError::into_inner) =
        loop_prefs(&engine_prefs_in(store));
    setup.browser = Some(browser);
    setup.browser_allow = LOOP_BROWSER_TOOLS.iter().map(|t| (*t).to_owned()).collect();
    // A page naming one uid twice keeps the label the gate is strictest
    // about, as the element book does.
    setup.strictness = Arc::new(super::elements::strictness);
    setup.key.clone_from(&parts.key);
    let engine = Arc::new(LoopEngine::new(setup));
    if parts.account.is_some() {
        engine.pacer().refresh_soon();
    }
    engine
}

/// The hub's limits on model requests: per bot and per person a day, and
/// money on paid models a calendar month (UTC).
struct HubGuard(Weak<Hub>);

impl UsageGuard for HubGuard {
    fn check(&self, q: &GuardQuery<'_>) -> Result<(), LimitHit> {
        let Some(hub) = self.0.upgrade() else {
            return Ok(());
        };
        let day = utc_day(now_ms());
        let prefs = hub.engine_prefs();
        let store = &hub.store;
        let read = |r: Result<u32, crate::store::StoreError>| {
            r.unwrap_or_else(|err| {
                tracing::warn!(%err, "couldn't count model requests");
                0
            })
        };
        if let Ok(bot) = hub.bot(q.bot) {
            let cap = if bot.is_central() {
                prefs.bot_daily_requests
            } else {
                bot.loop_settings
                    .daily_requests
                    .unwrap_or(prefs.bot_daily_requests)
            };
            if read(store.bot_requests(&day, q.bot)) >= cap {
                return Err(LimitHit(format!(
                    "{} has used its {cap} AI questions for today. It can go on tomorrow.",
                    bot.name
                )));
            }
        }
        if let (Some(person), Some(cap)) = (q.person, prefs.person_daily_requests)
            && read(store.person_requests(&day, person)) >= cap
        {
            let name = PersonId::new(person)
                .ok()
                .and_then(|id| hub.person_name(&id))
                .unwrap_or_else(|| "This person".to_owned());
            return Err(LimitHit(format!(
                "{name} has used their {cap} AI questions for today. They can go on tomorrow."
            )));
        }
        if !q.free
            && let Some(why) = paid_limit(store, &prefs, now_ms())
        {
            return Err(LimitHit(why));
        }
        Ok(())
    }
}

/// Paid spending this calendar month (UTC) at `now`, in US dollars.
fn month_spend(store: &Store, now: i64) -> f64 {
    store
        .spend_since(&utc_month_start(now))
        .unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read this month's AI spending");
            0.0
        })
}

/// Why paid models can't be used at `now`, if this month's paid spending
/// has reached the owner's monthly limit. Free models aren't affected.
fn paid_limit(store: &Store, prefs: &EnginePrefs, now: i64) -> Option<String> {
    let cap = f64::from(prefs.paid_monthly_cents) / 100.0;
    (month_spend(store, now) >= cap).then(|| {
        format!(
            "Paid AI spending has reached this month's ${cap:.2} limit. Free models keep \
             working; paid ones start again next month, or the owner can raise the limit \
             (Settings → General)."
        )
    })
}

impl Hub {
    /// Paid AI spending this calendar month (UTC), in US dollars.
    #[must_use]
    pub fn paid_spent_this_month(&self) -> f64 {
        month_spend(&self.store, now_ms())
    }
}

impl Hub {
    /// Why a scheduled run of `bot` for `person` can't start now, if a
    /// limit stops it: the bot's or the person's questions for today, or
    /// the home's free questions (with paid ones off, or their cap reached).
    pub(super) fn run_limit(&self, bot: &BotId, person: &PersonId) -> Option<String> {
        let guard = HubGuard(self.me.clone());
        let query = |free| GuardQuery {
            bot: bot.as_str(),
            person: Some(person.as_str()),
            free,
        };
        if let Err(hit) = guard.check(&query(true)) {
            return Some(hit.0);
        }
        let engine = self.loop_engine.as_ref()?;
        if !engine.pacer().free_used_up() {
            return None;
        }
        if !self.engine_prefs().paid_fallback {
            return Some(
                "the home's free AI questions for today are used up, and paid models are off"
                    .to_owned(),
            );
        }
        guard.check(&query(false)).err().map(|hit| hit.0)
    }
}

/// Seconds since the Unix epoch of the next 00:00 UTC.
fn next_utc_midnight_s(now_ms: i64) -> i64 {
    (now_ms.div_euclid(86_400_000) + 1) * 86_400
}

#[allow(clippy::cast_precision_loss)]
fn share(used: f64, of: f64) -> f64 {
    if of <= 0.0 { 0.0 } else { used / of }
}

/// How often stored loop results are swept.
const SWEEP_CONTEXT_MS: i64 = 6 * 3_600_000;

impl Hub {
    /// Deletes stored loop results (page snapshots, big tool results) of
    /// sessions whose last turn is more than 14 days old, then the oldest
    /// while a bot keeps more than 200 MB. A bot's current session is kept.
    /// At most every 6 hours.
    pub fn sweep_context(&self) {
        use std::sync::atomic::Ordering;
        let now = now_ms();
        let last = self.context_swept.load(Ordering::Relaxed);
        if now - last < SWEEP_CONTEXT_MS {
            return;
        }
        self.context_swept.store(now, Ordering::Relaxed);
        let bots: Vec<(BotId, std::path::PathBuf)> = self
            .bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|b| (b.id.clone(), b.dir.join("context")))
            .collect();
        self.sweep_relay_log(&bots, now);
        for (bot, dir) in bots {
            let keep: Vec<String> = self
                .store
                .session(&bot)
                .ok()
                .flatten()
                .map(|s| s.id)
                .into_iter()
                .collect();
            // The relay's log has its own sweep (above), by age.
            let mut keep_folders = keep.clone();
            keep_folders.push(oxbot_relay::LOG_DIR.to_owned());
            let last = |session: &str| self.store.session_last_ms(session).ok().flatten();
            let gone = oxbot_loop::results::sweep(
                &dir,
                &last,
                &keep_folders,
                now,
                oxbot_loop::results::MAX_BOT_BYTES,
            );
            if !gone.is_empty() {
                tracing::info!(%bot, sessions = gone.len(), "deleted old stored results");
                if let Err(err) = self.store.forget_loop_results(bot.as_str(), &gone) {
                    tracing::warn!(%err, "couldn't forget old stored results");
                }
            }
            // The current session keeps going, but not its old results.
            // Their rows stay, so handles are never given out twice and a
            // read-back says the result is gone.
            for session in &keep {
                let folder = dir.join(oxbot_loop::results::folder_name(session));
                let handles = oxbot_loop::results::sweep_session(
                    &folder,
                    now,
                    oxbot_loop::results::MAX_BOT_BYTES,
                );
                if !handles.is_empty() {
                    tracing::info!(%bot, results = handles.len(), "deleted old stored results");
                }
            }
        }
    }

    /// The owner changed the engine settings: the loop follows.
    pub(super) fn loop_prefs_changed(&self, prefs: &EnginePrefs) {
        if let Some(engine) = &self.loop_engine {
            *engine
                .prefs()
                .write()
                .unwrap_or_else(PoisonError::into_inner) = loop_prefs(prefs);
        }
    }

    /// A new model list was read: the loop routes on it.
    pub(super) fn loop_catalog_changed(&self, json: &str, fetched_ms: i64) {
        if let Some(engine) = &self.loop_engine {
            match serde_json::from_str::<Catalog>(json) {
                Ok(list) => engine.catalog().set(list, fetched_ms),
                Err(err) => tracing::warn!(%err, "couldn't read the model list for the loop"),
            }
        }
    }

    /// One model request of `bot`'s turn: the turn's meter, and the
    /// `OpenRouter` alerts.
    pub(super) async fn note_model_call(&self, bot: &BotId, record: &CallRecord) {
        if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
            r.meter.requests += 1;
            r.meter.cost_usd += record.cost_usd;
            if !record.model.is_empty() {
                r.meter.model.clone_from(&record.model);
            }
        }
        let windows = self.openrouter_windows(bot);
        if !windows.is_empty() {
            self.check_limit_alerts(&windows);
        }
        self.cue_data_policy();
    }

    /// Tells viewers when the `OpenRouter` account's privacy settings
    /// started or stopped ruling out the free models. A failed request
    /// leaves no meter row, so this runs at each turn's end too.
    pub(super) fn cue_data_policy(&self) {
        let blocked = self.openrouter_data_policy_blocked();
        let shown = self
            .data_policy_shown
            .swap(blocked, std::sync::atomic::Ordering::Relaxed);
        if blocked != shown {
            // A cue: each viewer gets their own engines view.
            self.announce(ServerMsg::Engines(EnginesView {
                codex: None,
                codex_ready: false,
                notice: super::ENGINES_NOTICE.to_owned(),
                owner: self.owner_display_name(),
                openrouter_ready: self.openrouter_ready(),
                models: Vec::new(),
                paid_models: Vec::new(),
                model_lists: Vec::new(),
                openrouter: None,
            }));
        }
    }

    /// Whether the `OpenRouter` account's own privacy settings ruled out
    /// the free models lately (see [`oxbot_loop::DATA_POLICY_HELP`]).
    #[must_use]
    pub fn openrouter_data_policy_blocked(&self) -> bool {
        self.loop_engine
            .as_ref()
            .is_some_and(|e| e.data_policy_blocked())
    }

    /// `OpenRouter`'s limits as alert windows: free requests today, paid
    /// spending this month, the key's credit, and `bot`'s own requests
    /// today.
    fn openrouter_windows(&self, bot: &BotId) -> Vec<LimitWindow> {
        let now = now_ms();
        let day = utc_day(now);
        let midnight = next_utc_midnight_s(now);
        let window = |name: String, used: f64, resets_at: i64| LimitWindow {
            name,
            used,
            resets_at,
            status: "allowed".to_owned(),
        };
        let mut out = Vec::new();
        let key = self.loop_engine.as_ref().and_then(|e| e.pacer().key_info());
        if let Some(limit) = key
            .as_ref()
            .and_then(|k| k.free_daily)
            .map(|q| q.limit)
            .filter(|l| *l > 0)
        {
            let used = self.store.free_requests(&day).unwrap_or(0);
            out.push(window(
                "openrouter_free_daily".to_owned(),
                share(f64::from(used), f64::from(limit)),
                midnight,
            ));
        }
        if let Some((limit, left)) = key
            .as_ref()
            .and_then(|k| Some((k.limit_usd?, k.remaining_usd?)))
            .filter(|(l, _)| *l > 0.0)
        {
            out.push(window(
                "openrouter_credits".to_owned(),
                1.0 - share(left, limit),
                0,
            ));
        }
        let prefs = self.engine_prefs();
        if prefs.paid_fallback {
            out.push(window(
                "openrouter_spend_monthly".to_owned(),
                share(
                    month_spend(&self.store, now),
                    f64::from(prefs.paid_monthly_cents) / 100.0,
                ),
                next_utc_month_ms(now) / 1000,
            ));
        }
        if let Ok(b) = self.bot(bot.as_str()) {
            let cap = if b.is_central() {
                prefs.bot_daily_requests
            } else {
                b.loop_settings
                    .daily_requests
                    .unwrap_or(prefs.bot_daily_requests)
            };
            let used = self.store.bot_requests(&day, bot.as_str()).unwrap_or(0);
            out.push(window(
                format!("openrouter_bot_daily:{bot}"),
                share(f64::from(used), f64::from(cap)),
                midnight,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use oxbot_engine::CallKind;

    fn paid_call(day: &str, cost_usd: f64) -> CallRecord {
        CallRecord {
            ts_ms: 1,
            day: day.into(),
            bot: "scout".into(),
            provider: "openrouter".into(),
            requested: "x/paid".into(),
            model: "x/paid".into(),
            outcome: "ok".into(),
            cost_usd,
            kind: CallKind::Chat,
            ..CallRecord::default()
        }
    }

    #[test]
    fn paid_spending_counts_this_month_only() {
        let store = Store::open_in_memory().unwrap();
        let prefs = EnginePrefs::default();
        // 2026-11-06 12:00 UTC.
        let now = 1_793_491_200_000 + 5 * 86_400_000 + 12 * 3_600_000;
        // Last month's spending, over the whole cap, doesn't count.
        store.record_call(&paid_call("2026-10-31", 25.0)).unwrap();
        assert_eq!(paid_limit(&store, &prefs, now), None);
        store.record_call(&paid_call("2026-11-01", 6.0)).unwrap();
        assert_eq!(paid_limit(&store, &prefs, now), None, "$6 of $10");
        store.record_call(&paid_call("2026-11-06", 4.0)).unwrap();
        assert_eq!(
            paid_limit(&store, &prefs, now).as_deref(),
            Some(
                "Paid AI spending has reached this month's $10.00 limit. Free models keep \
                 working; paid ones start again next month, or the owner can raise the limit \
                 (Settings → General)."
            )
        );
        let raised = EnginePrefs {
            paid_monthly_cents: 2500,
            ..EnginePrefs::default()
        };
        assert_eq!(paid_limit(&store, &raised, now), None, "$10 of $25");
        // The first of next month starts over.
        let december = next_utc_month_ms(now);
        assert_eq!(paid_limit(&store, &prefs, december), None);
    }

    #[test]
    fn midnight() {
        assert_eq!(next_utc_midnight_s(1_793_577_600_000), 1_793_664_000);
        assert_eq!(next_utc_midnight_s(1_793_577_599_999), 1_793_577_600);
    }
}
