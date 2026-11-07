//! The Codex engine in the database: each bot session's thread
//! (`codex_threads`), what oxbot's relay sent (`relay_log`; the bodies
//! are files under each bot's context folder), and the names the relay
//! swaps for stand-ins (everyone in the people list, the home area).

use oxbot_codex::{CodexStore, ThreadRow};
use oxbot_relay::personal::Known;
use oxbot_relay::{KnownNames, RelayLog, RelayLogRow};
use rusqlite::{OptionalExtension as _, params};

use super::Store;

#[allow(clippy::needless_pass_by_value, reason = "used as map_err(err)")]
fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

impl CodexStore for Store {
    fn thread(&self, bot: &str, session: &str) -> Result<Option<ThreadRow>, String> {
        self.conn()
            .query_row(
                "SELECT bot, session, thread_id, model, window, turns, created_ms, updated_ms
                 FROM codex_threads WHERE bot = ?1 AND session = ?2",
                params![bot, session],
                |r| {
                    Ok(ThreadRow {
                        bot: r.get(0)?,
                        session: r.get(1)?,
                        thread_id: r.get(2)?,
                        model: r.get(3)?,
                        window: u64::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
                        turns: r.get(5)?,
                        created_ms: r.get(6)?,
                        updated_ms: r.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(err)
    }

    fn save_thread(&self, row: &ThreadRow) -> Result<(), String> {
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO codex_threads
                   (bot, session, thread_id, model, window, turns, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.bot,
                    row.session,
                    row.thread_id,
                    row.model,
                    i64::try_from(row.window).unwrap_or(i64::MAX),
                    row.turns,
                    row.created_ms,
                    row.updated_ms,
                ],
            )
            .map_err(err)?;
        Ok(())
    }
}

impl RelayLog for Store {
    fn add(&self, row: &RelayLogRow) -> Result<(), String> {
        self.conn()
            .execute(
                "INSERT INTO relay_log (ts_ms, bot, session, turn, model, bytes, body_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.ts_ms,
                    row.bot,
                    row.session,
                    row.turn,
                    row.model,
                    i64::try_from(row.bytes).unwrap_or(i64::MAX),
                    row.body_path.display().to_string(),
                ],
            )
            .map_err(err)?;
        Ok(())
    }

    fn exchange(&self, row: &oxbot_relay::AiExchange) {
        self.note_ai(row);
    }
}

impl KnownNames for Store {
    fn known_names(&self) -> Vec<Known> {
        let mut known: Vec<Known> = match self.person_records() {
            Ok(people) => people.iter().map(|p| Known::person(&p.name)).collect(),
            Err(err) => {
                tracing::warn!(%err, "couldn't read the people list for stand-ins");
                Vec::new()
            }
        };
        // The home area is swapped out only when the owner said bots may
        // not use it: bots need the city for local searches.
        let bots_may_use_area = self
            .pref(crate::hub::PREF_HOME_AREA_USE)
            .ok()
            .flatten()
            .as_deref()
            != Some("0");
        if let Ok(Some(area)) = self.pref(crate::hub::PREF_HOME_AREA)
            && !area.trim().is_empty()
            && !bots_may_use_area
        {
            known.push(Known::place(&area));
        }
        known
    }
}

impl Store {
    /// Forgets relay log rows from before `cutoff_ms` (their files are
    /// swept with each bot's context folder). Returns how many went.
    ///
    /// # Errors
    /// The database failed.
    pub fn forget_relay_log(&self, cutoff_ms: i64) -> Result<usize, super::StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM relay_log WHERE ts_ms < ?1", [cutoff_ms])?)
    }

    /// The latest relay log rows of `bot`, newest first (a debug view of
    /// what was sent): when, the model, bytes and the file.
    ///
    /// # Errors
    /// The database failed.
    pub fn relay_log(&self, bot: &str, limit: u32) -> Result<Vec<RelayLogRow>, super::StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT ts_ms, bot, session, turn, model, bytes, body_path FROM relay_log
             WHERE bot = ?1 ORDER BY ts_ms DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![bot, limit], |r| {
            Ok(RelayLogRow {
                ts_ms: r.get(0)?,
                bot: r.get(1)?,
                session: r.get(2)?,
                turn: r.get(3)?,
                model: r.get(4)?,
                bytes: u64::try_from(r.get::<_, i64>(5)?).unwrap_or(0),
                body_path: std::path::PathBuf::from(r.get::<_, String>(6)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_and_the_relay_log_are_kept() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.thread("scout", "s1").unwrap(), None);
        let row = ThreadRow {
            bot: "scout".into(),
            session: "s1".into(),
            thread_id: "t-1".into(),
            model: "e2e/tool-model:free".into(),
            window: 131_072,
            turns: 2,
            created_ms: 5,
            updated_ms: 6,
        };
        store.save_thread(&row).unwrap();
        assert_eq!(store.thread("scout", "s1").unwrap(), Some(row.clone()));
        store
            .save_thread(&ThreadRow {
                turns: 3,
                ..row.clone()
            })
            .unwrap();
        assert_eq!(store.thread("scout", "s1").unwrap().unwrap().turns, 3);
        assert_eq!(store.thread("news", "s1").unwrap(), None);

        let log = |ts| RelayLogRow {
            ts_ms: ts,
            bot: "scout".into(),
            session: Some("s1".into()),
            turn: Some(1),
            model: "m".into(),
            bytes: 10,
            body_path: "/d/bots/scout/context/relay/1-0.json".into(),
        };
        store.add(&log(1)).unwrap();
        store.add(&log(3)).unwrap();
        assert_eq!(store.relay_log("scout", 5).unwrap(), vec![log(3), log(1)]);
        assert_eq!(store.forget_relay_log(2).unwrap(), 1);
        assert_eq!(store.relay_log("scout", 5).unwrap(), vec![log(3)]);
    }
}
