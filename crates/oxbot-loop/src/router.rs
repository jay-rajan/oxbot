//! The model router: which model answers each step.
//!
//! Candidates come from the live model list (free models that take tools
//! and have a big enough window), ranked by how they did lately, with a
//! bot's pinned model first. Paid models come only after every free one,
//! and only if the owner switched them on. Within a turn the router sticks
//! to the model that answered; a model that fails cools down for everyone.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use oxbot_engine::ModelChoice;
use oxbot_llm::{Catalog, MIN_CONTEXT, ModelInfo};
use tokio::time::Instant;

use crate::LoopPrefs;
use crate::meter::now_ms;
use crate::store::{LoopStore, ModelStat};

/// First cool-down after a model is busy or rate-limited.
const FIRST_COOLDOWN: Duration = Duration::from_secs(30);
/// Longest cool-down.
const MAX_COOLDOWN: Duration = Duration::from_mins(10);
/// How long ranking stats are kept before being read again.
const STATS_FOR: Duration = Duration::from_mins(10);
/// The window ranking looks back over.
const STATS_DAYS: i64 = 7;

/// The model list, kept for the loop. The host refreshes it.
#[derive(Debug, Default)]
pub struct CatalogCache {
    catalog: RwLock<Arc<Catalog>>,
    fetched_ms: RwLock<i64>,
}

impl CatalogCache {
    /// A cache holding `catalog`.
    #[must_use]
    pub fn new(catalog: Catalog) -> Self {
        let cache = Self::default();
        cache.set(catalog, now_ms());
        cache
    }

    /// The list as last read.
    #[must_use]
    pub fn get(&self) -> Arc<Catalog> {
        Arc::clone(&self.catalog.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// A newly read list, fetched at `fetched_ms`.
    pub fn set(&self, catalog: Catalog, fetched_ms: i64) {
        *self.catalog.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(catalog);
        *self
            .fetched_ms
            .write()
            .unwrap_or_else(PoisonError::into_inner) = fetched_ms;
    }

    /// When it was fetched (0: never).
    #[must_use]
    pub fn fetched_ms(&self) -> i64 {
        *self
            .fetched_ms
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// One model the router may use.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Its id.
    pub id: String,
    /// What the model list says about it (none for a pinned id it lacks).
    pub info: Option<ModelInfo>,
    /// A free model.
    pub free: bool,
}

/// The model (and server-side fallbacks) for one request.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// Asked first.
    pub primary: Candidate,
    /// OpenRouter may try these next on its side.
    pub fallbacks: Vec<String>,
    /// The smallest window of all of them.
    pub window: u64,
    /// The longest answer to ask for.
    pub max_tokens: u32,
    /// Request parameters all of them take (none known for a model the
    /// list doesn't describe).
    pub supported: Vec<String>,
}

/// What the router found.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// Use this.
    Route(Route),
    /// Every candidate is cooling down; the first is free again then.
    AllCooling(Instant),
    /// No candidate at all.
    None,
}

/// A turn's routing state.
#[derive(Debug, Clone, Default)]
pub struct TurnRouting {
    /// The bot's model choice.
    pub choice: ModelChoice,
    /// It's a Herder.
    pub central: bool,
    /// The model that answered last in this turn.
    pub sticky: Option<String>,
    /// Models that refused this session's requests as invalid.
    pub excluded: HashSet<String>,
    /// Models with no provider for this turn's data rules.
    pub no_provider: HashSet<String>,
    /// Only models whose window is at least this (an engine that was told
    /// a window must never get a smaller one). Models the list doesn't
    /// describe are kept.
    pub min_window: Option<u64>,
}

struct Cooling {
    until: Instant,
    next: Duration,
}

#[derive(Default)]
struct State {
    cooling: HashMap<String, Cooling>,
    stats: Option<(Instant, Vec<ModelStat>)>,
}

/// Picks models. One per engine, shared by its sessions.
pub struct Router {
    state: Mutex<State>,
    catalog: Arc<CatalogCache>,
    store: Arc<dyn crate::store::LoopStore>,
    prefs: Arc<RwLock<LoopPrefs>>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Router")
    }
}

impl Router {
    /// A router over `catalog`, ranking with stats from `store`.
    #[must_use]
    pub fn new(
        catalog: Arc<CatalogCache>,
        store: Arc<dyn LoopStore>,
        prefs: Arc<RwLock<LoopPrefs>>,
    ) -> Self {
        Self {
            state: Mutex::new(State::default()),
            catalog,
            store,
            prefs,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn prefs(&self) -> LoopPrefs {
        self.prefs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Every model the router may use for `choice`, best first.
    #[must_use]
    pub fn candidates(&self, choice: &ModelChoice, central: bool) -> Vec<Candidate> {
        let catalog = self.catalog.get();
        let prefs = self.prefs();
        let named = |id: &str| Candidate {
            id: id.to_owned(),
            info: catalog.get(id).cloned(),
            free: catalog
                .get(id)
                .map_or_else(|| id.ends_with(":free"), |m| m.free),
        };
        if let ModelChoice::Only(id) = choice {
            return vec![named(id)];
        }
        let mut list = Vec::new();
        let pinned = match choice {
            ModelChoice::Pin(id) => Some(id.clone()),
            _ if central => prefs.herder_model.clone(),
            _ => None,
        };
        if let Some(id) = pinned.filter(|id| !id.is_empty() && id != "auto") {
            list.push(named(&id));
        }
        let mut free: Vec<&ModelInfo> = catalog.free_for_loop().collect();
        let scores = self.scores(&free);
        free.sort_by(|a, b| {
            let sa = scores.get(&a.id).copied().unwrap_or(0.0);
            let sb = scores.get(&b.id).copied().unwrap_or(0.0);
            sb.total_cmp(&sa).then_with(|| a.id.cmp(&b.id))
        });
        list.extend(free.into_iter().map(|m| Candidate {
            id: m.id.clone(),
            info: Some(m.clone()),
            free: true,
        }));
        if prefs.paid_fallback {
            for id in &prefs.paid_models {
                if let Some(m) = catalog.get(id).filter(|m| !m.free && m.fits_loop()) {
                    list.push(Candidate {
                        id: m.id.clone(),
                        info: Some(m.clone()),
                        free: false,
                    });
                }
            }
        }
        let mut seen = HashSet::new();
        list.retain(|c| seen.insert(c.id.clone()));
        list
    }

    /// Scores for ranking: 0.6 × recent success (with a prior of 7 in
    /// 10) + 0.2 × window rank + 0.2 × speed rank.
    #[allow(clippy::cast_precision_loss)]
    fn scores(&self, models: &[&ModelInfo]) -> HashMap<String, f64> {
        let stats = self.stats();
        let by: HashMap<&str, &ModelStat> = stats.iter().map(|s| (s.model.as_str(), s)).collect();
        let n = models.len().max(2) - 1;
        let rank = |sorted: &[&str], id: &str| {
            sorted
                .iter()
                .position(|x| *x == id)
                .map_or(0.5, |p| 1.0 - p as f64 / n as f64)
        };
        let mut by_ctx: Vec<&ModelInfo> = models.to_vec();
        by_ctx.sort_by_key(|m| std::cmp::Reverse(m.context_length));
        let by_ctx: Vec<&str> = by_ctx.iter().map(|m| m.id.as_str()).collect();
        let mut timed: Vec<(&str, u64)> = models
            .iter()
            .filter_map(|m| {
                by.get(m.id.as_str())
                    .filter(|s| s.ok > 0)
                    .map(|s| (m.id.as_str(), s.avg_latency_ms))
            })
            .collect();
        timed.sort_by_key(|(_, l)| *l);
        let by_speed: Vec<&str> = timed.iter().map(|(id, _)| *id).collect();
        models
            .iter()
            .map(|m| {
                let (ok, total) = by.get(m.id.as_str()).map_or((0, 0), |s| (s.ok, s.total));
                let success = (f64::from(ok) + 7.0) / (f64::from(total) + 10.0);
                let speed = if by_speed.contains(&m.id.as_str()) {
                    rank(&by_speed, &m.id)
                } else {
                    0.5
                };
                (
                    m.id.clone(),
                    0.6 * success + 0.2 * rank(&by_ctx, &m.id) + 0.2 * speed,
                )
            })
            .collect()
    }

    fn stats(&self) -> Vec<ModelStat> {
        if let Some((at, stats)) = &self.state().stats
            && at.elapsed() < STATS_FOR
        {
            return stats.clone();
        }
        let since = now_ms() - STATS_DAYS * 86_400_000;
        let stats = self.store.model_stats(since).unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read model stats");
            Vec::new()
        });
        self.state().stats = Some((Instant::now(), stats.clone()));
        stats
    }

    /// The model for the next request of a turn. `free_ok`: free models
    /// may be used (the pacer hasn't run out).
    #[must_use]
    pub fn pick(&self, turn: &TurnRouting, free_ok: bool) -> Pick {
        let candidates = self.candidates(&turn.choice, turn.central);
        if candidates.is_empty() {
            return Pick::None;
        }
        let now = Instant::now();
        let state = self.state();
        let cooling_until = |id: &str| {
            state
                .cooling
                .get(id)
                .filter(|c| c.until > now)
                .map(|c| c.until)
        };
        let usable: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| !turn.excluded.contains(&c.id) && !turn.no_provider.contains(&c.id))
            .filter(|c| {
                turn.min_window.is_none_or(|min| {
                    c.info
                        .as_ref()
                        .is_none_or(|m| u64::from(m.context_length) >= min)
                })
            })
            .filter(|c| free_ok || !c.free)
            .collect();
        let ready: Vec<&Candidate> = usable
            .iter()
            .copied()
            .filter(|c| cooling_until(&c.id).is_none())
            .collect();
        let Some(primary) = turn
            .sticky
            .as_ref()
            .and_then(|s| ready.iter().find(|c| &c.id == s))
            .or_else(|| ready.first())
            .copied()
        else {
            return usable
                .iter()
                .filter_map(|c| cooling_until(&c.id))
                .min()
                .map_or(Pick::None, Pick::AllCooling);
        };
        let fallbacks: Vec<&Candidate> = if matches!(turn.choice, ModelChoice::Only(_)) {
            Vec::new()
        } else {
            ready
                .iter()
                .copied()
                .filter(|c| c.id != primary.id && c.free == primary.free)
                .take(2)
                .collect()
        };
        let window = std::iter::once(primary)
            .chain(fallbacks.iter().copied())
            .map(|c| {
                c.info
                    .as_ref()
                    .map_or(u64::from(MIN_CONTEXT), |m| u64::from(m.context_length))
            })
            .min()
            .unwrap_or(u64::from(MIN_CONTEXT));
        let cap = self.prefs().max_tokens;
        let max_tokens = primary.info.as_ref().map_or(cap, |m| m.max_tokens(cap));
        let supported = primary
            .info
            .as_ref()
            .map(|m| m.supported_parameters.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                fallbacks
                    .iter()
                    .all(|c| c.info.as_ref().is_some_and(|m| m.supports(p)))
            })
            .collect();
        Pick::Route(Route {
            primary: primary.clone(),
            fallbacks: fallbacks.into_iter().map(|c| c.id.clone()).collect(),
            window,
            max_tokens,
            supported,
        })
    }

    /// `model` was busy or rate-limited: cool it down (as long as the
    /// service asked, else 30 s doubling up to 10 minutes).
    pub fn cool(&self, model: &str, retry_after: Option<Duration>) {
        let mut state = self.state();
        let now = Instant::now();
        let entry = state.cooling.entry(model.to_owned()).or_insert(Cooling {
            until: now,
            next: FIRST_COOLDOWN,
        });
        let wait = retry_after.unwrap_or(entry.next).min(MAX_COOLDOWN);
        entry.until = now + wait;
        entry.next = (entry.next * 2).min(MAX_COOLDOWN);
    }

    /// `model` answered: its cool-down starts over.
    pub fn ok(&self, model: &str) {
        self.state().cooling.remove(model);
    }

    /// The model list in use.
    #[must_use]
    pub fn catalog(&self) -> Arc<Catalog> {
        self.catalog.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryLoopStore;
    use oxbot_engine::CallRecord;

    fn model(id: &str, ctx: u32, free: bool) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: id.into(),
            context_length: ctx,
            max_completion: Some(8000),
            prompt_usd_per_token: if free { 0.0 } else { 1e-6 },
            completion_usd_per_token: if free { 0.0 } else { 1e-6 },
            tools: true,
            images_in: false,
            text_io: true,
            free,
            supported_parameters: ["tools", "tool_choice", "max_tokens", "seed"]
                .map(str::to_owned)
                .to_vec(),
        }
    }

    fn router(prefs: LoopPrefs, store: Arc<MemoryLoopStore>) -> Router {
        let catalog = Catalog {
            models: vec![
                model("a:free", 128_000, true),
                model("b:free", 64_000, true),
                model("c:free", 200_000, true),
                model("small:free", 8_000, true),
                model("paid", 200_000, false),
            ],
        };
        Router::new(
            Arc::new(CatalogCache::new(catalog)),
            store,
            Arc::new(RwLock::new(prefs)),
        )
    }

    fn ids(list: &[Candidate]) -> Vec<&str> {
        list.iter().map(|c| c.id.as_str()).collect()
    }

    #[test]
    fn free_models_ranked_paid_only_when_on() {
        let store = Arc::new(MemoryLoopStore::new());
        let r = router(LoopPrefs::default(), Arc::clone(&store));
        // No stats: bigger windows first.
        assert_eq!(
            ids(&r.candidates(&ModelChoice::Auto, false)),
            ["c:free", "a:free", "b:free"]
        );
        let r = router(
            LoopPrefs {
                paid_fallback: true,
                paid_models: vec!["paid".into(), "unknown".into()],
                ..LoopPrefs::default()
            },
            store,
        );
        let list = r.candidates(&ModelChoice::Pin("b:free".into()), false);
        assert_eq!(ids(&list), ["b:free", "c:free", "a:free", "paid"]);
        assert_eq!(
            ids(&r.candidates(&ModelChoice::Only("x/y".into()), false)),
            ["x/y"]
        );
    }

    #[test]
    fn success_counts_most() {
        let store = Arc::new(MemoryLoopStore::new());
        let now = now_ms();
        for (model, outcome) in [
            ("c:free", "busy"),
            ("c:free", "busy"),
            ("c:free", "busy"),
            ("c:free", "busy"),
            ("c:free", "busy"),
            ("b:free", "ok"),
            ("b:free", "ok"),
            ("b:free", "ok"),
        ] {
            store
                .record_call(&CallRecord {
                    ts_ms: now,
                    requested: model.into(),
                    model: model.into(),
                    outcome: outcome.into(),
                    latency_ms: 100,
                    ..CallRecord::default()
                })
                .unwrap();
        }
        let r = router(LoopPrefs::default(), store);
        assert_eq!(
            ids(&r.candidates(&ModelChoice::Auto, false)),
            ["b:free", "a:free", "c:free"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn sticky_cooldowns_and_fallbacks() {
        let r = router(LoopPrefs::default(), Arc::new(MemoryLoopStore::new()));
        let mut turn = TurnRouting::default();
        let Pick::Route(route) = r.pick(&turn, true) else {
            panic!()
        };
        assert_eq!(route.primary.id, "c:free");
        assert_eq!(route.fallbacks, ["a:free", "b:free"]);
        assert_eq!(route.window, 64_000, "the smallest window of all three");
        assert_eq!(route.max_tokens, 4096);
        turn.sticky = Some("a:free".into());
        let Pick::Route(route) = r.pick(&turn, true) else {
            panic!()
        };
        assert_eq!(route.primary.id, "a:free");
        r.cool("a:free", None);
        r.cool("c:free", Some(Duration::from_secs(5)));
        let Pick::Route(route) = r.pick(&turn, true) else {
            panic!()
        };
        assert_eq!(route.primary.id, "b:free");
        assert_eq!(route.fallbacks, Vec::<String>::new());
        r.cool("b:free", None);
        let Pick::AllCooling(until) = r.pick(&turn, true) else {
            panic!()
        };
        assert_eq!(until, Instant::now() + Duration::from_secs(5));
        tokio::time::advance(Duration::from_secs(6)).await;
        let Pick::Route(route) = r.pick(&turn, true) else {
            panic!()
        };
        assert_eq!(route.primary.id, "c:free");
        // No free models allowed and no paid ones: nothing.
        assert_eq!(r.pick(&turn, false), Pick::None);
    }

    #[test]
    fn only_means_no_fallbacks() {
        let r = router(LoopPrefs::default(), Arc::new(MemoryLoopStore::new()));
        let turn = TurnRouting {
            choice: ModelChoice::Only("a:free".into()),
            ..TurnRouting::default()
        };
        let Pick::Route(route) = r.pick(&turn, true) else {
            panic!()
        };
        assert_eq!(route.primary.id, "a:free");
        assert_eq!(route.fallbacks, Vec::<String>::new());
        assert_eq!(route.window, 128_000);
    }

    #[test]
    fn a_route_sends_only_parameters_every_model_takes() {
        let mut c = model("c:free", 200_000, true);
        c.supported_parameters = vec!["tools".into(), "max_tokens".into()];
        let catalog = Catalog {
            models: vec![c, model("a:free", 128_000, true)],
        };
        let r = Router::new(
            Arc::new(CatalogCache::new(catalog)),
            Arc::new(MemoryLoopStore::new()),
            Arc::new(RwLock::new(LoopPrefs::default())),
        );
        let Pick::Route(route) = r.pick(&TurnRouting::default(), true) else {
            panic!()
        };
        assert_eq!(route.fallbacks, ["a:free"]);
        assert_eq!(route.supported, ["tools", "max_tokens"]);

        let only_a = TurnRouting {
            choice: ModelChoice::Only("a:free".into()),
            ..TurnRouting::default()
        };
        let Pick::Route(route) = r.pick(&only_a, true) else {
            panic!()
        };
        assert_eq!(
            route.supported,
            ["tools", "tool_choice", "max_tokens", "seed"]
        );

        let unknown = TurnRouting {
            choice: ModelChoice::Only("x/unlisted".into()),
            ..TurnRouting::default()
        };
        let Pick::Route(route) = r.pick(&unknown, true) else {
            panic!()
        };
        assert_eq!(
            route.supported,
            Vec::<String>::new(),
            "nothing optional for an unknown model"
        );
    }
}
