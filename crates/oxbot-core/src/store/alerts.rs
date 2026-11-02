//! The usage alerts already sent: one row per plan window, holding the
//! window it was for and the highest level alerted in it.

use oxbot_claude::LimitWindow;
use rusqlite::{OptionalExtension as _, Row, params};

use super::{Store, StoreError};

/// What was alerted for one of the plan's windows.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitAlert {
    /// The window's name, e.g. `five_hour`.
    pub name: String,
    /// When that window resets (ms since the Unix epoch; 0 if unknown).
    pub resets_at_ms: i64,
    /// The highest level alerted: 50 or 80.
    pub level: u8,
    /// How much was used when it alerted, from 0 to 1.
    pub used: f64,
    /// When it alerted (ms since the Unix epoch).
    pub alerted_ms: i64,
}

/// What to do with a window's alert row, decided from the row and the
/// window as Claude just reported it.
#[derive(Debug, Clone, PartialEq)]
pub enum AlertChange {
    /// Nothing new.
    Keep,
    /// Forget it: the next level passed alerts again.
    Clear,
    /// A new level was passed: record it (and alert).
    Record(LimitAlert),
}

fn limit_alert(r: &Row<'_>) -> rusqlite::Result<LimitAlert> {
    Ok(LimitAlert {
        name: r.get(0)?,
        resets_at_ms: r.get(1)?,
        level: u8::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
        used: r.get(3)?,
        alerted_ms: r.get(4)?,
    })
}

impl Store {
    /// What was alerted for `name`'s window, if anything.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn limit_alert(&self, name: &str) -> Result<Option<LimitAlert>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT name, resets_at_ms, level, used, alerted_ms FROM limit_alerts WHERE name = ?1",
                [name],
                limit_alert,
            )
            .optional()?)
    }

    /// Every window's latest alert, soonest reset first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn limit_alerts(&self) -> Result<Vec<LimitAlert>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT name, resets_at_ms, level, used, alerted_ms FROM limit_alerts ORDER BY resets_at_ms",
        )?;
        let rows = stmt
            .query_map([], limit_alert)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Records that `alert`'s window reached its level, replacing what was
    /// there for that window name.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_limit_alert(&self, alert: &LimitAlert) -> Result<(), StoreError> {
        put(&self.conn(), alert)
    }

    /// Decides, for each window, what changes (`decide` gets the window's
    /// row, if any), and saves it, all in one transaction: two bots
    /// reporting at once can't both alert. Returns the alerts recorded.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn change_limit_alerts(
        &self,
        windows: &[LimitWindow],
        decide: impl Fn(Option<&LimitAlert>, &LimitWindow) -> AlertChange,
    ) -> Result<Vec<LimitAlert>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut recorded = Vec::new();
        for w in windows {
            let row = tx
                .query_row(
                    "SELECT name, resets_at_ms, level, used, alerted_ms FROM limit_alerts WHERE name = ?1",
                    [&w.name],
                    limit_alert,
                )
                .optional()?;
            match decide(row.as_ref(), w) {
                AlertChange::Keep => {}
                AlertChange::Clear => {
                    tx.execute("DELETE FROM limit_alerts WHERE name = ?1", [&w.name])?;
                }
                AlertChange::Record(alert) => {
                    put(&tx, &alert)?;
                    recorded.push(alert);
                }
            }
        }
        tx.commit()?;
        Ok(recorded)
    }
}

fn put(conn: &rusqlite::Connection, a: &LimitAlert) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO limit_alerts (name, resets_at_ms, level, used, alerted_ms) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(name) DO UPDATE SET
           resets_at_ms = excluded.resets_at_ms,
           level = excluded.level,
           used = excluded.used,
           alerted_ms = excluded.alerted_ms",
        params![a.name, a.resets_at_ms, a.level, a.used, a.alerted_ms],
    )?;
    Ok(())
}
