//! Sessions: each bot's saved Claude session and what it has read.

use rusqlite::{OptionalExtension, params};

use super::{SavedSession, Store, StoreError, now_ms};
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

    /// Saves `bot`'s Claude session, or forgets it with `None`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_session(
        &self,
        bot: &BotId,
        session: Option<&SavedSession>,
    ) -> Result<(), StoreError> {
        let conn = self.conn();
        match session {
            Some(s) => conn.execute(
                // A different session starts clean (SET reads the old row).
                "INSERT INTO sessions (bot, claude_session, prompt, updated_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(bot) DO UPDATE SET
                   tainted_by = CASE WHEN claude_session = ?2 THEN tainted_by ELSE NULL END,
                   claude_session = ?2, prompt = ?3, updated_ms = ?4",
                params![bot.as_str(), s.id, s.prompt, now_ms()],
            )?,
            None => conn.execute("DELETE FROM sessions WHERE bot = ?1", params![bot.as_str()])?,
        };
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
