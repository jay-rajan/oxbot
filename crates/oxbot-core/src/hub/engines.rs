//! The engine every bot runs on, bots moving to it from engines oxbot no
//! longer has, and whether a task chain may reach a bot.
//!
//! Every bot runs on Codex: oxbot's own loop retired, and Claude left
//! before it. Codex runs on the household's `OpenRouter` account, so
//! anyone in the household may give a bot they can open tasks. A task chain's
//! [`ChainOrigin`] (who started it, carried unchanged when an Assistant passes
//! the task on) still decides the rest: whose cards a turn raises, and
//! [`may_task`] stops a chain passed on too many times.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use oxbot_protocol::EventKind;

use super::{Hub, HubError, lock};
use crate::bot::{Bot, Viewer, is_personal_assistant};
use crate::engine::{Engine, EngineKind};
use crate::id::{BotId, PersonId};
use crate::store::{Store, TurnOriginRecord};

/// The most bots a task may be passed on through.
pub(super) const MAX_VIA: usize = 4;

/// How a task chain's first message came in. Provenance only: never
/// trusted for access (that's `person` and `on_home_computer`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    /// The home computer connection (the Mac's own window, `oxbot chat`).
    /// Origins saved before the rename say `farm`.
    #[serde(alias = "farm")]
    HomeComputer,
    /// A paired device. Origins saved before the rename say `gate`.
    #[serde(alias = "gate")]
    Device,
    /// An end-to-end test acting as someone.
    Test,
    /// By name (`Hub::send`).
    #[default]
    Name,
    /// A schedule's run, as the person who made it ("scheduled by Sam").
    Schedule,
}

impl Entry {
    fn as_str(self) -> &'static str {
        match self {
            Self::HomeComputer => "home_computer",
            Self::Device => "device",
            Self::Test => "test",
            Self::Name => "name",
            Self::Schedule => "schedule",
        }
    }
}

/// Who a task chain acts for, from its first message to every bot it is
/// passed on to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainOrigin {
    /// The person the chain acts for (who sent the first message).
    pub person: Option<PersonId>,
    /// The first message came over the home computer connection. Origins
    /// saved before the rename call it `on_farm`.
    #[serde(alias = "on_farm")]
    pub on_home_computer: bool,
    /// How the first message came in.
    pub entry: Entry,
    /// The first message: its bot and event number, once recorded.
    pub root: Option<(BotId, u64)>,
    /// The bots that passed it on, in order.
    pub via: Vec<BotId>,
    /// What a bot that passed it on had read that oxbot doesn't trust.
    pub tainted_by: Option<String>,
    /// The schedule whose run started it ([`Entry::Schedule`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<i64>,
}

impl ChainOrigin {
    /// A chain `viewer` starts.
    #[must_use]
    pub fn from_viewer(viewer: &Viewer, entry: Entry) -> Self {
        Self {
            person: viewer.id.clone(),
            on_home_computer: viewer.home_computer,
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

    /// A run of schedule `id`, as `person`: never the home computer connection,
    /// whoever made it.
    #[must_use]
    pub fn scheduled(person: PersonId, id: i64) -> Self {
        Self {
            person: Some(person),
            on_home_computer: false,
            entry: Entry::Schedule,
            schedule: Some(id),
            ..Self::default()
        }
    }

    /// Whether this is a schedule's own run (not a task passed on from one).
    #[must_use]
    pub fn is_scheduled_run(&self) -> bool {
        self.entry == Entry::Schedule && self.via.is_empty()
    }

    /// As stored in `turn_origins`, for a turn of `engine`.
    pub(super) fn record(&self, engine: EngineKind) -> TurnOriginRecord {
        let (root_bot, root_seq) = self
            .root
            .as_ref()
            .map_or((String::new(), 0), |(bot, seq)| (bot.to_string(), *seq));
        TurnOriginRecord {
            person: self.person.as_ref().map(ToString::to_string),
            on_home_computer: self.on_home_computer,
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
    /// The engine that runs `bot`: Codex, the only one. (The one place the
    /// hub decides it, so a later engine changes only this.)
    #[allow(
        clippy::unused_self,
        reason = "the hub's one place to decide a bot's engine"
    )]
    pub(super) fn engine_of(&self, bot: &Bot) -> EngineKind {
        bot.engine
    }

    /// The engine `kind`, if it can run now. It needs the household's
    /// `OpenRouter` sign-in first, then the AI helper (Codex) found.
    ///
    /// # Errors
    /// [`HubError::EngineNotReady`] with what's missing.
    pub(super) fn engine(&self, kind: EngineKind) -> Result<Arc<dyn Engine>, HubError> {
        match &self.loop_engine {
            Some(engine) if engine.ready().is_ok() => {}
            // The Keychain is asking the person: say so, for everyone.
            Some(engine) if engine.waiting_for_keychain() => {
                return Err(HubError::EngineNotReady(
                    oxbot_llm::KEYCHAIN_WAITING.to_owned(),
                ));
            }
            _ => {
                return Err(HubError::EngineNotReady(format!(
                    "The home's AI account (OpenRouter) isn't connected yet; ask {}.",
                    self.owner_name()
                )));
            }
        }
        let engine: Arc<dyn Engine> = match kind {
            EngineKind::Codex => match self.codex_engine() {
                Some(engine) => engine,
                None => {
                    return Err(HubError::EngineNotReady(
                        "The AI helper isn't set up on the home computer.".to_owned(),
                    ));
                }
            },
        };
        engine
            .ready()
            .map_err(|why| HubError::EngineNotReady(why.0))?;
        Ok(engine)
    }

    /// Whether a chain started by `origin` is the home computer owner's: it came
    /// over the home computer connection, or from the home computer's person (by id, on any
    /// device). With no home computer person chosen, only the home computer connection.
    pub(super) fn is_owner(&self, origin: &ChainOrigin) -> bool {
        origin.on_home_computer
            || (origin.person.is_some() && origin.person == self.owner_person_id())
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

    /// Sets bot `id`'s AI model (`model`; empty: Auto) or its own ordered
    /// list (`models`; empty: automatic) on `engine` (Codex) for `viewer`.
    /// A list replaces a pinned model; a model set alone (an older page)
    /// replaces the list. Its running session stops; the next message
    /// starts one with the new models.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's an Assistant, the list isn't
    /// one oxbot can keep, the engine isn't set up, it's working, or its
    /// file can't be written.
    pub async fn set_engine(
        &self,
        id: &str,
        engine: EngineKind,
        model: Option<String>,
        models: Option<Vec<String>>,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "The Assistant's AI model is set by the owner.".to_owned(),
            ));
        }
        let models = models
            .map(super::engine_prefs::checked_models)
            .transpose()?;
        self.engine(engine)?;
        self.change_models(&bot.id, Some(engine), model, models)
            .await
    }

    /// Gives bot `id` its own ordered list of models (empty: automatic),
    /// for `viewer`, as the New bot form does right after making it. Unlike
    /// [`Hub::set_engine`] it works before the home's AI account is
    /// connected: the list waits in the bot's file.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's an Assistant, the list isn't
    /// one oxbot can keep, it's working, or its file can't be written.
    pub async fn set_bot_models(
        &self,
        id: &str,
        models: Vec<String>,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "The Assistant's AI model is set by the owner.".to_owned(),
            ));
        }
        let models = super::engine_prefs::checked_models(models)?;
        self.change_models(&bot.id, None, None, Some(models)).await
    }

    /// Writes a bot's engine, pinned model and list, stopping its session.
    async fn change_models(
        &self,
        id: &BotId,
        engine: Option<EngineKind>,
        model: Option<String>,
        models: Option<Vec<String>>,
    ) -> Result<(), HubError> {
        let model = model.map(|m| m.trim().to_owned());
        // No turn may start while it changes model.
        self.claim(id).await?;
        let changed = self.update_bot(id, |b| {
            if let Some(engine) = engine {
                b.engine = engine;
            }
            match (model, models) {
                (_, Some(models)) => b.loop_settings.set_models(models),
                (Some(model), None) => {
                    b.loop_settings.models.clear();
                    b.loop_settings.model = (!model.is_empty()).then_some(model);
                }
                (None, None) => {}
            }
        });
        if changed.is_ok() {
            tracing::info!(bot = %id, "changed its AI models");
            self.stop_session(id).await;
        }
        lock(&self.starting).remove(id);
        changed?;
        self.announce_bots().await;
        Ok(())
    }

    /// The home computer owner's name, for messages: the home computer's person, once chosen.
    pub(super) fn owner_name(&self) -> String {
        self.owner_person_id()
            .and_then(|id| self.person_name(&id))
            .unwrap_or_else(|| "the owner".to_owned())
    }
}

/// The line a bot that ran on Claude gets in its chat, once, when it
/// moves to Codex.
pub const MOVED_FROM_CLAUDE: &str = "This bot now uses free and paid AI models through your \
     OpenRouter account. Its old Claude sessions can't be continued.";

/// The line a bot that ran on oxbot's own loop (retired) gets in its chat,
/// once, when it moves to Codex.
pub const MOVED_FROM_LOOP: &str = "This bot now uses free and paid AI models through your \
     OpenRouter account, in a new way. It starts a new conversation: a task it was in the \
     middle of can't be continued, so send it again if you still need it.";

/// Bots whose files still name an engine oxbot no longer has run on Codex
/// from now on: Claude (or no engine: made before engines) and oxbot's own
/// loop. Each gets its line in its chat once ([`MOVED_FROM_CLAUDE`],
/// [`MOVED_FROM_LOOP`]; noted with the move in `engine_upgrades`), then
/// its `bot.toml` is rewritten (atomically) to say Codex. Its chat and
/// history stay; its old session is never resumed (no engine goes on with
/// a `claude` or `openrouter` session). A person's own Assistant whose file
/// names no engine ran on the loop, not Claude. Call before anything else
/// saves the bots, with nobody listening yet.
pub(super) fn upgrade_from_claude(bots: &mut [Bot], store: &Store) {
    for bot in bots.iter_mut() {
        let Some(mut from) = bot.moved_from else {
            continue;
        };
        if from == crate::bot::CLAUDE && is_personal_assistant(bot.id.as_str()) {
            from = crate::bot::LOOP;
        }
        let text = if from == crate::bot::CLAUDE {
            MOVED_FROM_CLAUDE
        } else {
            MOVED_FROM_LOOP
        };
        let note = EventKind::Note {
            text: text.to_owned(),
        };
        if let Err(err) = store.note_engine_upgrade(&bot.id, from, EngineKind::Codex, note) {
            // It runs on Codex anyway; its file is upgraded next time.
            tracing::warn!(bot = %bot.id, %err, "couldn't note a bot's move to Codex");
            continue;
        }
        let was = bot.moved_from.take();
        match bot.save() {
            Ok(()) => tracing::info!(bot = %bot.id, from, "moved a bot to Codex"),
            Err(err) => {
                // Tried again at the next start (its line isn't added twice).
                bot.moved_from = was;
                tracing::warn!(bot = %bot.id, %err, "couldn't upgrade a bot's file");
            }
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

/// How a viewer's message comes in: the home computer connection, or a device.
pub(super) fn entry_of(viewer: &Viewer) -> Entry {
    if viewer.home_computer {
        Entry::HomeComputer
    } else {
        Entry::Device
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HubConfig;
    use crate::bot::personal_assistant_id;

    /// A hub with `recipes`, `scout` and the home computer's Assistant; Jay is the
    /// home computer's person.
    fn hub(dir: &std::path::Path) -> Arc<Hub> {
        let scout = Bot::create(dir, BotId::new("scout").unwrap(), "Scout", "").unwrap();
        let bots = vec![
            Bot::create(dir, BotId::new("assistant").unwrap(), "Assistant", "").unwrap(),
            Bot::create(dir, BotId::new("recipes").unwrap(), "Recipes", "").unwrap(),
            scout,
        ];
        let hub = Hub::new(bots, Store::open_in_memory().unwrap(), HubConfig::new(dir));
        hub.owner_person("Jay").unwrap();
        hub
    }

    fn origin(person: Option<PersonId>, on_home_computer: bool) -> ChainOrigin {
        ChainOrigin {
            person,
            on_home_computer,
            ..ChainOrigin::default()
        }
    }

    #[tokio::test]
    async fn anyone_may_task_a_bot_until_a_chain_is_passed_on_too_often() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let jay = hub.owner_person_id();
        let ann = hub.person_named("Ann").unwrap().id;
        let assistant = BotId::new("assistant").unwrap();
        let cases = [
            ("the home computer connection", origin(None, true)),
            ("the home computer's person on a phone", origin(jay, false)),
            ("Ann", origin(Some(ann.clone()), false)),
            ("nobody known", origin(None, false)),
            (
                "Ann, passed on by the Assistant",
                origin(Some(ann.clone()), false).relayed_by(&assistant, Some("a web page".into())),
            ),
        ];
        for (who, chain) in cases {
            assert!(may_task(&chain).is_ok(), "{who}");
        }
        // Ann may give both bots tasks, as far as who she is goes; here
        // Codex isn't set up.
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
            long = long.relayed_by(&assistant, None);
        }
        assert!(matches!(may_task(&long), Err(HubError::NotAllowed(_))));
    }

    #[tokio::test]
    async fn every_bot_runs_on_codex() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let ann = hub.assistant_for("Ann").unwrap();
        for id in ["assistant", ann.as_str(), "recipes", "scout"] {
            assert_eq!(hub.engine_of_bot(id), Some(EngineKind::Codex), "{id}");
        }
        assert_eq!(ann, personal_assistant_id("Ann"));
        // An Assistant's AI model is the owner's to set.
        assert!(matches!(
            hub.set_engine(
                "assistant",
                EngineKind::Codex,
                Some("m:free".into()),
                None,
                &hub.home_computer_viewer("Jay")
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
        let assistant = BotId::new("assistant").unwrap();
        let next = start.relayed_by(&assistant, Some("a web page".into()));
        assert_eq!(next.asker(), Some(&sam));
        assert!(!next.on_home_computer);
        assert_eq!(next.root, start.root);
        assert_eq!(next.via, std::slice::from_ref(&assistant));
        assert_eq!(next.tainted_by.as_deref(), Some("a web page"));
        // Taint once carried stays.
        assert_eq!(
            next.relayed_by(&assistant, None).tainted_by.as_deref(),
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

    #[test]
    fn a_scheduled_run_acts_for_its_person_and_never_as_the_home_computer() {
        let sam = PersonId::random();
        let run = ChainOrigin::scheduled(sam.clone(), 7);
        assert_eq!(
            (run.asker(), run.on_home_computer, run.schedule),
            (Some(&sam), false, Some(7))
        );
        assert!(run.is_scheduled_run());
        assert_eq!(run.record(EngineKind::Codex).entry, "schedule");
        // A task the run passes on keeps its person, but isn't the run.
        let passed = run.relayed_by(&BotId::new("assistant").unwrap(), None);
        assert!(!passed.is_scheduled_run() && passed.asker() == Some(&sam));
        // Old saved origins (no schedule, the old names) still read.
        let old: ChainOrigin = serde_json::from_str(
            r#"{"person":null,"on_farm":true,"entry":"farm","root":null,"via":[],"tainted_by":null}"#,
        )
        .unwrap();
        assert_eq!(old.schedule, None);
        assert!(old.on_home_computer && old.entry == Entry::HomeComputer);
    }
}
