//! Engines per bot, and who may give a bot tasks.
//!
//! Which engine, all here: the Farm's Herder runs on Claude (the owner's
//! lead bot); every other person's own Herder runs on `OpenRouter`; any
//! other bot runs on what its `bot.toml` says (Claude if it says nothing).
//!
//! Claude bots run on the Farm owner's own Claude plan, which nobody else
//! may use, so only the owner gives them tasks. [`Hub::may_task`] is the
//! one place that's decided, from the task chain's [`ChainOrigin`]: who
//! started it, carried unchanged when a Herder passes the task on, so no
//! one else's request reaches a Claude bot through another bot.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Hub, HubError, lock};
use crate::bot::{Bot, Viewer, is_personal_herder};
use crate::engine::{Engine, EngineKind};
use crate::id::{BotId, PersonId};
use crate::store::TurnOriginRecord;

/// The most bots a task may be passed on through.
pub(super) const MAX_VIA: usize = 4;

/// How a task chain's first message came in. Provenance only: never
/// trusted for access (that's `person` and `on_farm`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Entry {
    /// The Farm connection (the Mac's own window, `oxbot chat`).
    Farm,
    /// A paired Gate.
    Gate,
    /// An end-to-end test acting as someone.
    Test,
    /// By name (`Hub::send`).
    #[default]
    Name,
}

impl Entry {
    fn as_str(self) -> &'static str {
        match self {
            Self::Farm => "farm",
            Self::Gate => "gate",
            Self::Test => "test",
            Self::Name => "name",
        }
    }
}

/// Who a task chain acts for, from its first message to every bot it is
/// passed on to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainOrigin {
    /// The person the chain acts for (who sent the first message).
    pub person: Option<PersonId>,
    /// The first message came over the Farm connection.
    pub on_farm: bool,
    /// How the first message came in.
    pub entry: Entry,
    /// The first message: its bot and event number, once recorded.
    pub root: Option<(BotId, u64)>,
    /// The bots that passed it on, in order.
    pub via: Vec<BotId>,
    /// What a bot that passed it on had read that oxbot doesn't trust.
    pub tainted_by: Option<String>,
}

impl ChainOrigin {
    /// A chain `viewer` starts.
    #[must_use]
    pub fn from_viewer(viewer: &Viewer, entry: Entry) -> Self {
        Self {
            person: viewer.id.clone(),
            on_farm: viewer.farm,
            entry,
            ..Self::default()
        }
    }

    /// The same chain, passed on by `bot`, which had read `taint`.
    #[must_use]
    pub fn relayed_by(&self, bot: &BotId, taint: Option<String>) -> Self {
        let mut next = self.clone();
        next.via.push(bot.clone());
        if taint.is_some() {
            next.tainted_by = taint;
        }
        next
    }

    /// The person it acts for.
    #[must_use]
    pub fn asker(&self) -> Option<&PersonId> {
        self.person.as_ref()
    }

    /// As stored in `turn_origins`, for a turn of `engine`.
    pub(super) fn record(&self, engine: EngineKind) -> TurnOriginRecord {
        let (root_bot, root_seq) = self
            .root
            .as_ref()
            .map_or((String::new(), 0), |(bot, seq)| (bot.to_string(), *seq));
        TurnOriginRecord {
            person: self.person.as_ref().map(ToString::to_string),
            on_farm: self.on_farm,
            entry: self.entry.as_str().to_owned(),
            root_bot,
            root_seq,
            via: self.via.iter().map(ToString::to_string).collect(),
            engine: engine.as_str().to_owned(),
            tainted_by: self.tainted_by.clone(),
        }
    }
}

impl Hub {
    /// The engine that runs `bot`.
    pub(super) fn engine_of(&self, bot: &Bot) -> EngineKind {
        #[cfg(any(test, feature = "test-people"))]
        if self.config.pre_engine_tests {
            return EngineKind::Claude;
        }
        if bot.is_main_herder() {
            return EngineKind::Claude;
        }
        if is_personal_herder(bot.id.as_str()) {
            return EngineKind::OpenRouter;
        }
        bot.engine
    }

    /// The engine `kind`, if it can run now.
    ///
    /// # Errors
    /// [`HubError::EngineNotReady`] with what's missing.
    pub(super) fn engine(&self, kind: EngineKind) -> Result<Arc<dyn Engine>, HubError> {
        let engine = match kind {
            EngineKind::Claude => Arc::clone(&self.claude),
            EngineKind::OpenRouter => match &self.loop_engine {
                Some(engine) if engine.ready().is_ok() => Arc::clone(engine) as Arc<dyn Engine>,
                // The Keychain is asking the person: say so, for everyone.
                Some(engine) if engine.waiting_for_keychain() => {
                    return Err(HubError::EngineNotReady(
                        oxbot_llm::KEYCHAIN_WAITING.to_owned(),
                    ));
                }
                _ => {
                    return Err(HubError::EngineNotReady(format!(
                        "OpenRouter isn't set up on this Mac yet; ask {}.",
                        self.owner_name()
                    )));
                }
            },
            EngineKind::Codex => {
                return Err(HubError::EngineNotReady(
                    "Codex isn't set up on this Mac.".to_owned(),
                ));
            }
        };
        engine
            .ready()
            .map_err(|why| HubError::EngineNotReady(why.0))?;
        Ok(engine)
    }

    /// Whether a chain started by `origin` is the Farm owner's: it came
    /// over the Farm connection, or from the Farm's person (by id, on any
    /// device). With no Farm person chosen, only the Farm connection.
    pub(super) fn is_owner(&self, origin: &ChainOrigin) -> bool {
        origin.on_farm || (origin.person.is_some() && origin.person == self.farm_id())
    }

    /// Whether a task chain from `origin` may reach `bot`. The only place
    /// it is decided: Claude bots take tasks only from the owner.
    ///
    /// # Errors
    /// [`HubError::OwnerOnly`] for someone else's task to a Claude bot,
    /// [`HubError::NotAllowed`] for a chain passed on too many times.
    pub(super) fn may_task(&self, bot: &Bot, origin: &ChainOrigin) -> Result<(), HubError> {
        if origin.via.len() > MAX_VIA {
            return Err(HubError::NotAllowed(
                "This task was passed on through too many bots.".to_owned(),
            ));
        }
        #[cfg(any(test, feature = "test-people"))]
        if self.config.pre_engine_tests {
            return Ok(());
        }
        if self.engine_of(bot) != EngineKind::Claude || self.is_owner(origin) {
            return Ok(());
        }
        Err(HubError::OwnerOnly {
            bot: bot.name.clone(),
            owner: self.owner_name(),
        })
    }

    /// Whether `viewer` may give bot `id` tasks right now: it's theirs to
    /// open, the owner-only rule allows it, and its engine can run.
    ///
    /// # Errors
    /// Why not, in words for them.
    pub fn may_send(&self, id: &str, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = self.resolve(viewer).into_owned();
        let bot = self.bot_for(id, &viewer)?;
        self.may_task(&bot, &ChainOrigin::from_viewer(&viewer, entry_of(&viewer)))?;
        self.engine(self.engine_of(&bot)).map(drop)
    }

    /// Switches bot `id` to `engine` (with `model`, if given) for `viewer`.
    /// Only the owner may switch a bot to Claude, and a bot anyone else uses
    /// (shared, or someone else's private one) only with
    /// `confirm_owner_only`: from then on only the owner gives it tasks.
    /// Its running session stops; the next message starts one on the new
    /// engine.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's a Herder, the switch isn't
    /// theirs to make, the engine isn't set up, it's working, or its file
    /// can't be written.
    pub async fn set_engine(
        &self,
        id: &str,
        engine: EngineKind,
        model: Option<String>,
        viewer: &Viewer,
        confirm_owner_only: bool,
    ) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed("Herder's engine is fixed.".to_owned()));
        }
        match engine {
            EngineKind::Claude => {
                let owner = self.owner_name();
                if !self.is_owner(&ChainOrigin::from_viewer(viewer, entry_of(viewer))) {
                    return Err(HubError::NotAllowed(format!(
                        "Only {owner} can switch a bot to Claude: it runs on {owner}'s own Claude plan."
                    )));
                }
                let only_theirs = bot.private && bot.owner.is_some() && bot.owner == self.farm_id();
                if bot.engine != EngineKind::Claude && !only_theirs && !confirm_owner_only {
                    return Err(HubError::NotAllowed(format!(
                        "Switching {} to Claude makes it owner-only: only you can give it tasks. \
                         Others can still read its chat and reports.",
                        bot.name
                    )));
                }
            }
            EngineKind::OpenRouter | EngineKind::Codex => {
                self.engine(engine)?;
            }
        }
        let model = model.map(|m| m.trim().to_owned());
        // No turn may start while it changes engine.
        self.claim(&bot.id).await?;
        let changed = self.update_bot(&bot.id, |b| {
            b.engine = engine;
            if let Some(model) = model {
                let model = (!model.is_empty()).then_some(model);
                match engine {
                    EngineKind::Claude => b.model = model,
                    // Codex bots use the same OpenRouter models.
                    EngineKind::OpenRouter | EngineKind::Codex => b.loop_settings.model = model,
                }
            }
        });
        if changed.is_ok() {
            tracing::info!(bot = %bot.id, %engine, "changed engine");
            self.stop_claude(&bot.id).await;
        }
        lock(&self.starting).remove(&bot.id);
        changed?;
        self.announce_bots().await;
        Ok(())
    }

    /// What a Herder hears when a task it would pass on may not reach
    /// `bot`.
    pub(super) fn owner_only_message(&self, bot: &Bot) -> String {
        let owner = self.owner_name();
        format!(
            "{} runs on {owner}'s Claude plan, so only {owner} can give it tasks. Tell the user to \
             ask {owner}, or use another bot.",
            bot.name
        )
    }

    /// The Farm owner's name, for messages: the Farm's person, once chosen.
    pub(super) fn owner_name(&self) -> String {
        self.farm_id()
            .and_then(|id| self.person_name(&id))
            .unwrap_or_else(|| "the Farm's owner".to_owned())
    }
}

/// How a viewer's message comes in: the Farm connection, or a Gate.
pub(super) fn entry_of(viewer: &Viewer) -> Entry {
    if viewer.farm {
        Entry::Farm
    } else {
        Entry::Gate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HubConfig;
    use crate::store::Store;

    /// A hub with every rule on: `recipes` on Claude, `scout` on
    /// `OpenRouter`, and the Farm's Herder; Jay is the Farm's person if
    /// `farm`.
    fn hub(dir: &std::path::Path, farm: bool) -> Arc<Hub> {
        let mut scout = Bot::create(dir, BotId::new("scout").unwrap(), "Scout", "").unwrap();
        scout.engine = EngineKind::OpenRouter;
        scout.save().unwrap();
        let bots = vec![
            Bot::create(dir, BotId::new("assistant").unwrap(), "Herder", "").unwrap(),
            Bot::create(dir, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
            scout,
        ];
        let config = HubConfig {
            pre_engine_tests: false,
            ..HubConfig::for_app(dir)
        };
        let hub = Hub::new(bots, Store::open_in_memory().unwrap(), config);
        if farm {
            hub.farm_person("Jay").unwrap();
        }
        hub
    }

    fn origin(person: Option<PersonId>, on_farm: bool) -> ChainOrigin {
        ChainOrigin {
            person,
            on_farm,
            ..ChainOrigin::default()
        }
    }

    #[tokio::test]
    async fn only_the_owners_chains_reach_claude_bots() {
        for farm_chosen in [true, false] {
            let tmp = tempfile::tempdir().unwrap();
            let hub = hub(tmp.path(), farm_chosen);
            let jay = hub.farm_id();
            let ann = hub.person_named("Ann").unwrap().id;
            let herder = BotId::new("assistant").unwrap();
            let recipes = hub.bot("recipes").unwrap();
            let scout = hub.bot("scout").unwrap();
            let cases = [
                ("the Farm connection", origin(None, true), true),
                (
                    "the Farm's person on a phone",
                    origin(jay.clone(), false),
                    farm_chosen,
                ),
                ("Ann", origin(Some(ann.clone()), false), false),
                ("nobody known", origin(None, false), false),
                (
                    "Ann, passed on by Herder",
                    origin(Some(ann.clone()), false).relayed_by(&herder, None),
                    false,
                ),
                (
                    "the owner, passed on by Herder",
                    origin(None, true).relayed_by(&herder, Some("a web page".into())),
                    true,
                ),
            ];
            for (who, chain, owner) in cases {
                assert_eq!(
                    hub.may_task(&recipes, &chain).is_ok(),
                    owner,
                    "{who} (Farm person chosen: {farm_chosen})"
                );
                assert!(
                    hub.may_task(&scout, &chain).is_ok(),
                    "anyone may task an OpenRouter bot: {who}"
                );
            }
            // A chain passed on too many times stops, on every engine.
            let mut long = origin(None, true);
            for _ in 0..=MAX_VIA {
                long = long.relayed_by(&herder, None);
            }
            assert!(matches!(
                hub.may_task(&scout, &long),
                Err(HubError::NotAllowed(_))
            ));
        }
    }

    #[test]
    fn a_relayed_chain_keeps_who_started_it() {
        let sam = PersonId::random();
        let start = ChainOrigin {
            root: Some((BotId::new("assistant").unwrap(), 3)),
            ..origin(Some(sam.clone()), false)
        };
        let herder = BotId::new("assistant").unwrap();
        let next = start.relayed_by(&herder, Some("a web page".into()));
        assert_eq!(next.asker(), Some(&sam));
        assert!(!next.on_farm);
        assert_eq!(next.root, start.root);
        assert_eq!(next.via, std::slice::from_ref(&herder));
        assert_eq!(next.tainted_by.as_deref(), Some("a web page"));
        // Taint once carried stays.
        assert_eq!(
            next.relayed_by(&herder, None).tainted_by.as_deref(),
            Some("a web page")
        );
        let json = serde_json::to_value(&next).unwrap();
        assert_eq!(serde_json::from_value::<ChainOrigin>(json).unwrap(), next);
        let record = next.record(EngineKind::Claude);
        assert_eq!(
            (record.root_bot.as_str(), record.root_seq),
            ("assistant", 3)
        );
        assert_eq!(record.entry, "name");
    }

    #[tokio::test]
    async fn herder_lists_claude_bots_as_owner_only_to_others() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path(), true);
        let herder = BotId::new("assistant").unwrap();
        let ann = hub.person_named("Ann").unwrap();
        let ann_viewer = Viewer::person(ann.id.clone(), "Ann", false);
        let (list, _) = hub
            .team(&herder, &ann_viewer, &origin(Some(ann.id), false))
            .await;
        let line = |name: &str| {
            list.lines()
                .find(|l| l.starts_with(&format!("- {name} ")))
                .unwrap()
                .to_owned()
        };
        assert!(
            line("Recipes").contains("owner only (runs on Jay's Claude plan)"),
            "{list}"
        );
        assert!(line("Scout").contains(": free."), "{list}");
        let (list, _) = hub
            .team(&herder, &hub.farm_viewer("Jay"), &origin(None, true))
            .await;
        assert!(!list.contains("owner only"), "{list}");
    }

    #[tokio::test]
    async fn a_task_someone_else_started_never_reaches_a_claude_bot() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path(), true);
        let herder = BotId::new("assistant").unwrap();
        let ann = hub.person_named("Ann").unwrap();
        let ann_viewer = Viewer::person(ann.id.clone(), "Ann", false);
        let task = super::super::team::Task {
            target: "recipes",
            message: "make dinner",
            wait: false,
        };
        let (ok, said) = hub
            .give_task(&herder, &ann_viewer, &origin(Some(ann.id), false), task)
            .await;
        assert!(!ok);
        assert_eq!(
            said,
            "Recipes runs on Jay's Claude plan, so only Jay can give it tasks. Tell the user to \
             ask Jay, or use another bot."
        );
        assert_eq!(hub.history("recipes", 0).unwrap(), []);
    }
}
