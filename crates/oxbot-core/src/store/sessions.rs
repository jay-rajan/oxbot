//! Sessions: each bot's saved engine session (its Claude session), the
//! engine it belongs to, and what it has read.

use rusqlite::{OptionalExtension, params};

use super::{SavedSession, Store, StoreError, now_ms};
use crate::engine::EngineKind;
use crate::id::BotId;

impl Store {
    /// The Claude session saved for `bot`, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn session(&self, bot: &BotId) -> Result<Option<SavedSession>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT claude_session, prompt FROM sessions WHERE bot = ?1",
                params![bot.as_str()],
                |r| {
                    Ok(SavedSession {
                        id: r.get(0)?,
                        prompt: r.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// The engine `bot`'s saved session belongs to (`claude` for sessions
    /// saved before engines), if one is saved.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn session_engine(&self, bot: &BotId) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT engine FROM sessions WHERE bot = ?1",
                params![bot.as_str()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Saves `bot`'s Claude session, or forgets it with `None`. A
    /// different session (or none) starts clean: nothing read, no pages
    /// opened.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_session(
        &self,
        bot: &BotId,
        session: Option<&SavedSession>,
    ) -> Result<(), StoreError> {
        self.save_session(bot, session, EngineKind::Claude)
    }

    /// Like [`Store::set_session`], for a session of `engine`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn save_session(
        &self,
        bot: &BotId,
        session: Option<&SavedSession>,
        engine: EngineKind,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if let Some(s) = session {
            tx.execute(
                "DELETE FROM session_pages WHERE bot = ?1 AND NOT EXISTS (
                   SELECT 1 FROM sessions WHERE bot = ?1 AND claude_session = ?2)",
                params![bot.as_str(), s.id],
            )?;
            tx.execute(
                // SET reads the old row.
                "INSERT INTO sessions (bot, claude_session, prompt, updated_ms, engine)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(bot) DO UPDATE SET
                   tainted_by = CASE WHEN claude_session = ?2 THEN tainted_by ELSE NULL END,
                   claude_session = ?2, prompt = ?3, updated_ms = ?4, engine = ?5",
                params![bot.as_str(), s.id, s.prompt, now_ms(), engine.as_str()],
            )?;
        } else {
            tx.execute("DELETE FROM sessions WHERE bot = ?1", params![bot.as_str()])?;
            tx.execute(
                "DELETE FROM session_pages WHERE bot = ?1",
                params![bot.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// What `bot`'s saved Claude session has read that oxbot doesn't trust,
    /// if anything (see `hub::taint`).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn session_taint(&self, bot: &BotId) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT tainted_by FROM sessions WHERE bot = ?1",
                params![bot.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Records what `bot`'s saved session has read that oxbot doesn't trust
    /// (`None`: nothing). Does nothing if no session is saved.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_session_taint(&self, bot: &BotId, why: Option<&str>) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE sessions SET tainted_by = ?2 WHERE bot = ?1",
            params![bot.as_str(), why],
        )?;
        Ok(())
    }
}
