//! Who started each turn's task chain (`turn_origins`): one row per
//! message that started a turn.

use rusqlite::{OptionalExtension, params};

use super::events::from_sql;
use super::{Store, StoreError};
use crate::id::BotId;

/// Who started a turn's task chain, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOriginRecord {
    /// The person the chain started with, by id.
    pub person: Option<String>,
    /// Whether it started on the home computer connection.
    pub on_home_computer: bool,
    /// How it came in: `home_computer`, `device`, `test` or `name`.
    pub entry: String,
    /// The bot that got the chain's first message.
    pub root_bot: String,
    /// That message's event number.
    pub root_seq: u64,
    /// The bots that passed it on, in order.
    pub via: Vec<String>,
    /// The engine that ran the turn.
    pub engine: String,
    /// What a bot that passed it on had read that oxbot doesn't trust.
    pub tainted_by: Option<String>,
}

impl Store {
    /// Records who started the turn of `bot`'s message `seq` (kept as it
    /// was first recorded).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_turn_origin(
        &self,
        bot: &BotId,
        seq: u64,
        origin: &TurnOriginRecord,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT OR IGNORE INTO turn_origins
               (bot, seq, person, on_home_computer, entry, root_bot, root_seq, via, engine, tainted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                bot.as_str(),
                i64::try_from(seq).unwrap_or(i64::MAX),
                origin.person,
                origin.on_home_computer,
                origin.entry,
                origin.root_bot,
                i64::try_from(origin.root_seq).unwrap_or(i64::MAX),
                serde_json::to_string(&origin.via)?,
                origin.engine,
                origin.tainted_by,
            ],
        )?;
        Ok(())
    }

    /// Who started the turn of `bot`'s message `seq`, if recorded.
    ///
    /// # Errors
    /// Fails if SQLite fails or the row can't be read.
    pub fn turn_origin(
        &self,
        bot: &BotId,
        seq: u64,
    ) -> Result<Option<TurnOriginRecord>, StoreError> {
        let row = self
            .conn()
            .query_row(
                "SELECT person, on_home_computer, entry, root_bot, root_seq, via, engine, tainted_by
                 FROM turn_origins WHERE bot = ?1 AND seq = ?2",
                params![bot.as_str(), i64::try_from(seq).unwrap_or(i64::MAX)],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, bool>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((person, on_home_computer, entry, root_bot, root_seq, via, engine, tainted_by)) =
            row
        else {
            return Ok(None);
        };
        Ok(Some(TurnOriginRecord {
            person,
            on_home_computer,
            entry,
            root_bot,
            root_seq: from_sql(root_seq),
            via: serde_json::from_str(&via)?,
            engine,
            tainted_by,
        }))
    }
}
