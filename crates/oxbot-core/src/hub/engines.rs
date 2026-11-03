//! Engines per bot: which engine runs a bot, and whether it can run now.
//!
//! The rules, all here: the Farm's Herder runs on Claude (the owner's lead
//! bot); every other person's own Herder runs on `OpenRouter`; any other bot
//! runs on what its `bot.toml` says (Claude if it says nothing).

use std::sync::Arc;

use super::{Hub, HubError};
use crate::bot::{Bot, is_personal_herder};
use crate::engine::{Engine, EngineKind};

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
            EngineKind::OpenRouter => {
                return Err(HubError::EngineNotReady(format!(
                    "OpenRouter isn't set up on this Mac yet; ask {}.",
                    self.owner_name()
                )));
            }
        };
        engine
            .ready()
            .map_err(|why| HubError::EngineNotReady(why.0))?;
        Ok(engine)
    }

    /// The Farm owner's name, for messages: the Farm's person, once chosen.
    pub(super) fn owner_name(&self) -> String {
        self.farm_id()
            .and_then(|id| self.person_name(&id))
            .unwrap_or_else(|| "the Farm's owner".to_owned())
    }
}
