//! Engines per bot, and whether a task chain may reach a bot.
//!
//! Which engine, all here: every Herder (the Farm's and each person's own)
//! runs on Codex; any other bot runs on what its `bot.toml` says (Codex
//! for new bots and for bots that ran on Claude before it left oxbot).
//!
//! Both engines run on the household's `OpenRouter` account, so anyone in
//! the household may give a bot they can open tasks. A task chain's
//! [`ChainOrigin`] (who started it, carried unchanged when a Herder passes
//! the task on) still decides the rest: whose cards a turn raises, and
//! [`may_task`] stops a chain passed on too many times.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use oxbot_protocol::EventKind;

use super::{Hub, HubError, lock};
use crate::bot::{Bot, Viewer, is_personal_herder};
use crate::engine::{Engine, EngineKind};
use crate::id::{BotId, PersonId};
use crate::store::{Store, TurnOriginRecord};

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
    /// The engine that runs `bot`: Codex for every Herder (each keeps its
    /// own tools and card rules), else what its file says.
    pub(super) fn engine_of(&self, bot: &Bot) -> EngineKind {
        #[cfg(any(test, feature = "test-people"))]
        if let Some(kind) = self.config.engines_on {
            return kind;
        }
        if bot.is_central() {
            return EngineKind::Codex;
        }
        bot.engine
    }

    /// The engine `kind`, if it can run now. Both need the household's
    /// `OpenRouter` sign-in first: until then, bots on either say so the
    /// same way.
    ///
    /// # Errors
    /// [`HubError::EngineNotReady`] with what's missing.
    pub(super) fn engine(&self, kind: EngineKind) -> Result<Arc<dyn Engine>, HubError> {
        let loop_engine = match &self.loop_engine {
            Some(engine) if engine.ready().is_ok() => Arc::clone(engine),
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
        };
        let engine: Arc<dyn Engine> = match kind {
            EngineKind::OpenRouter => loop_engine,
            EngineKind::Codex => match self.codex_engine() {
                Some(engine) => engine,
                None => {
                    return Err(HubError::EngineNotReady(
                        "Codex isn't set up on this Mac.".to_owned(),
                    ));
                }
            },
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

    /// Whether `viewer` may give bot `id` tasks right now: it's theirs to
    /// open, the chain may reach it, and its engine can run.
    ///
    /// # Errors
    /// Why not, in words for them.
    pub fn may_send(&self, id: &str, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = self.resolve(viewer).into_owned();
        let bot = self.bot_for(id, &viewer)?;
        may_task(&ChainOrigin::from_viewer(&viewer, entry_of(&viewer)))?;
        self.engine(self.engine_of(&bot)).map(drop)
    }

    /// Switches bot `id` to `engine` (with `model`, if given) for `viewer`.
    /// Its running session stops; the next message starts one on the new
    /// engine.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's a Herder, the engine isn't
    /// set up, it's working, or its file can't be written.
    pub async fn set_engine(
        &self,
        id: &str,
        engine: EngineKind,
        model: Option<String>,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed("Herder's engine is fixed.".to_owned()));
        }
        self.engine(engine)?;
        let model = model.map(|m| m.trim().to_owned());
        // No turn may start while it changes engine.
        self.claim(&bot.id).await?;
        let changed = self.update_bot(&bot.id, |b| {
            b.engine = engine;
            // Both engines use the same OpenRouter models.
            if let Some(model) = model {
                b.loop_settings.model = (!model.is_empty()).then_some(model);
            }
        });
        if changed.is_ok() {
            tracing::info!(bot = %bot.id, %engine, "changed engine");
            self.stop_session(&bot.id).await;
        }
        lock(&self.starting).remove(&bot.id);
        changed?;
        self.announce_bots().await;
        Ok(())
    }

    /// The Farm owner's name, for messages: the Farm's person, once chosen.
    pub(super) fn owner_name(&self) -> String {
        self.farm_id()
            .and_then(|id| self.person_name(&id))
            .unwrap_or_else(|| "the Farm's owner".to_owned())
    }
}

/// The line a bot that ran on Claude gets in its chat, once, when it
/// moves to Codex.
pub const MOVED_TO_CODEX: &str = "This bot now runs on Codex (open models) through your \
     OpenRouter account. Its old Claude sessions can't be continued.";

/// Bots whose files still say Claude (or no engine: made before engines)
/// run on Codex from now on: each gets [`MOVED_TO_CODEX`] in its chat
/// once, then its `bot.toml` is rewritten (atomically) to say Codex. Its
/// chat and history stay; its Claude session is never resumed (no engine
/// goes on with a `claude` session). A person's own Herder ran on oxbot's
/// loop, not Claude: its file is just brought up to date. Call before
/// anything else saves the bots, with nobody listening yet.
pub(super) fn upgrade_from_claude(bots: &mut [Bot], store: &Store) {
    for bot in bots.iter_mut().filter(|b| b.from_claude) {
        if !is_personal_herder(bot.id.as_str()) {
            let note = EventKind::Note {
                text: MOVED_TO_CODEX.to_owned(),
            };
            if let Err(err) = store.note_engine_upgrade(&bot.id, "claude", EngineKind::Codex, note)
            {
                // It runs on Codex anyway; its file is upgraded next time.
                tracing::warn!(bot = %bot.id, %err, "couldn't note a bot's move to Codex");
                continue;
            }
        }
        match bot.save() {
            Ok(()) => {
                bot.from_claude = false;
                tracing::info!(bot = %bot.id, "moved a bot from Claude to Codex");
            }
            Err(err) => tracing::warn!(bot = %bot.id, %err, "couldn't upgrade a bot's file"),
        }
    }
}

/// Whether a task chain from `origin` may reach a bot: the one place every
/// message and passed-on task is checked before it's recorded (which bots
/// a person may open is decided before, by who they are).
///
/// # Errors
/// [`HubError::NotAllowed`] for a chain passed on too many times.
pub(super) fn may_task(origin: &ChainOrigin) -> Result<(), HubError> {
    if origin.via.len() > MAX_VIA {
        return Err(HubError::NotAllowed(
            "This task was passed on through too many bots.".to_owned(),
        ));
    }
    Ok(())
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
    use crate::bot::personal_herder_id;

    /// A hub with `recipes` (a bot from before engines: Codex now),
    /// `scout` on `OpenRouter`, and the Farm's Herder; Jay is the Farm's
    /// person.
    fn hub(dir: &std::path::Path) -> Arc<Hub> {
        let mut scout = Bot::create(dir, BotId::new("scout").unwrap(), "Scout", "").unwrap();
        scout.engine = EngineKind::OpenRouter;
        scout.save().unwrap();
        let bots = vec![
            Bot::create(dir, BotId::new("assistant").unwrap(), "Herder", "").unwrap(),
            Bot::create(dir, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
            scout,
        ];
        let hub = Hub::new(bots, Store::open_in_memory().unwrap(), HubConfig::new(dir));
        hub.farm_person("Jay").unwrap();
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
    async fn anyone_may_task_a_bot_until_a_chain_is_passed_on_too_often() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let jay = hub.farm_id();
        let ann = hub.person_named("Ann").unwrap().id;
        let herder = BotId::new("assistant").unwrap();
        let cases = [
            ("the Farm connection", origin(None, true)),
            ("the Farm's person on a phone", origin(jay, false)),
            ("Ann", origin(Some(ann.clone()), false)),
            ("nobody known", origin(None, false)),
            (
                "Ann, passed on by Herder",
                origin(Some(ann.clone()), false).relayed_by(&herder, Some("a web page".into())),
            ),
        ];
        for (who, chain) in cases {
            assert!(may_task(&chain).is_ok(), "{who}");
        }
        // Ann may give both bots tasks (Recipes on Codex, Scout on the loop),
        // as far as who she is goes; here neither engine is set up.
        let ann = Viewer::person(ann, "Ann", false);
        for bot in ["recipes", "scout"] {
            assert!(
                matches!(hub.may_send(bot, &ann), Err(HubError::EngineNotReady(_))),
                "{bot}"
            );
        }
        // A chain passed on too many times stops.
        let mut long = origin(None, true);
        for _ in 0..=MAX_VIA {
            long = long.relayed_by(&herder, None);
        }
        assert!(matches!(may_task(&long), Err(HubError::NotAllowed(_))));
    }

    #[tokio::test]
    async fn every_herder_runs_on_codex_and_other_bots_on_their_own_engine() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let ann = hub.herder_for("Ann").unwrap();
        for id in ["assistant", ann.as_str(), "recipes"] {
            assert_eq!(hub.engine_of_bot(id), Some(EngineKind::Codex), "{id}");
        }
        assert_eq!(hub.engine_of_bot("scout"), Some(EngineKind::OpenRouter));
        assert_eq!(ann, personal_herder_id("Ann"));
        // Herders' engines are fixed.
        assert!(matches!(
            hub.set_engine(
                "assistant",
                EngineKind::OpenRouter,
                None,
                &hub.farm_viewer("Jay")
            )
            .await,
            Err(HubError::NotAllowed(_))
        ));
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
        let record = next.record(EngineKind::Codex);
        assert_eq!(
            (record.root_bot.as_str(), record.root_seq),
            ("assistant", 3)
        );
        assert_eq!(
            (record.entry.as_str(), record.engine.as_str()),
            ("name", "codex")
        );
    }
}
