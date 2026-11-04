//! Schedules and their runs (`schedules`, `schedule_runs`, migration 24).

use std::collections::HashMap;

use oxbot_protocol::ScheduleRule;
use rusqlite::{OptionalExtension, params};

use super::events::from_sql;
use super::{Store, StoreError, now_ms};
use crate::id::{BotId, PersonId};

/// A schedule as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleRecord {
    /// Its id.
    pub id: i64,
    /// The bot it runs in.
    pub bot: BotId,
    /// Who it runs as.
    pub person: PersonId,
    /// What the bot is asked.
    pub task: String,
    /// When.
    pub rule: ScheduleRule,
    /// The time zone its times were set in.
    pub tz: String,
    /// When it runs next; `None` once a one-time schedule has run.
    pub next_run_ms: Option<i64>,
    /// When it last ran.
    pub last_run_ms: Option<i64>,
    /// Paused: it doesn't run.
    pub paused: bool,
    /// When it was made.
    pub created_ms: i64,
    /// When it last changed.
    pub updated_ms: i64,
}

/// A schedule to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSchedule {
    /// The bot it runs in.
    pub bot: BotId,
    /// Who it runs as.
    pub person: PersonId,
    /// What the bot is asked.
    pub task: String,
    /// When.
    pub rule: ScheduleRule,
    /// The home's time zone now.
    pub tz: String,
    /// Its first run.
    pub next_run_ms: Option<i64>,
}

/// One run of a schedule (or one it skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    /// Its id.
    pub id: i64,
    /// The schedule.
    pub schedule: i64,
    /// When it was due.
    pub due_ms: i64,
    /// When it started, if it did.
    pub started_ms: Option<i64>,
    /// When it ended, if it did.
    pub ended_ms: Option<i64>,
    /// `running`, `ok`, `failed`, `busy`, `limit`, `missed`, `too_soon` or
    /// `no_access`.
    pub outcome: String,
    /// Its message in the bot's chat (`events.seq`), if it started.
    pub turn: Option<u64>,
}

const COLUMNS: &str = "id, bot, person, task_text, rule_json, tz, next_run_ms, last_run_ms, paused, created_ms, updated_ms";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduleRecord> {
    let bad = |i: usize, e: String| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into())
    };
    let bot: String = r.get(1)?;
    let person: String = r.get(2)?;
    let rule: String = r.get(4)?;
    Ok(ScheduleRecord {
        id: r.get(0)?,
        bot: BotId::new(bot).map_err(|e| bad(1, e.to_string()))?,
        person: PersonId::new(person).map_err(|e| bad(2, e.to_string()))?,
        task: r.get(3)?,
        rule: serde_json::from_str(&rule).map_err(|e| bad(4, e.to_string()))?,
        tz: r.get(5)?,
        next_run_ms: r.get(6)?,
        last_run_ms: r.get(7)?,
        paused: r.get::<_, i64>(8)? != 0,
        created_ms: r.get(9)?,
        updated_ms: r.get(10)?,
    })
}

const RUN_COLUMNS: &str = "id, schedule, due_ms, started_ms, ended_ms, outcome, turn";

fn run_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    Ok(RunRecord {
        id: r.get(0)?,
        schedule: r.get(1)?,
        due_ms: r.get(2)?,
        started_ms: r.get(3)?,
        ended_ms: r.get(4)?,
        outcome: r.get(5)?,
        turn: r.get::<_, Option<i64>>(6)?.map(from_sql),
    })
}

impl Store {
    /// Adds a schedule, unless its person has `max` already. Returns it,
    /// or `None` when they're at the limit.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_schedule(
        &self,
        new: &NewSchedule,
        max: usize,
    ) -> Result<Option<ScheduleRecord>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let have: i64 = tx.query_row(
            "SELECT COUNT(*) FROM schedules WHERE person = ?1",
            [new.person.as_str()],
            |r| r.get(0),
        )?;
        if usize::try_from(have).unwrap_or(usize::MAX) >= max {
            return Ok(None);
        }
        let now = now_ms();
        tx.execute(
            "INSERT INTO schedules (bot, person, task_text, rule_json, tz, next_run_ms, paused, created_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
            params![
                new.bot.as_str(),
                new.person.as_str(),
                new.task,
                serde_json::to_string(&new.rule)?,
                new.tz,
                new.next_run_ms,
                now
            ],
        )?;
        let id = tx.last_insert_rowid();
        let made = tx.query_row(
            &format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1"),
            [id],
            row,
        )?;
        tx.commit()?;
        Ok(Some(made))
    }

    /// The schedule `id`, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn schedule(&self, id: i64) -> Result<Option<ScheduleRecord>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1"),
                [id],
                row,
            )
            .optional()?)
    }

    /// Every schedule, soonest first (ones with no next run last).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn schedules(&self) -> Result<Vec<ScheduleRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules ORDER BY next_run_ms IS NULL, next_run_ms, id"
        ))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The schedules not paused whose next run is at or before `now_ms`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn due_schedules(&self, now_ms: i64) -> Result<Vec<ScheduleRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules
             WHERE paused = 0 AND next_run_ms IS NOT NULL AND next_run_ms <= ?1
             ORDER BY next_run_ms, id"
        ))?;
        let rows = stmt.query_map([now_ms], row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The soonest next run of any schedule, paused ones included.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn earliest_next_run(&self) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT MIN(next_run_ms) FROM schedules", [], |r| r.get(0))?)
    }

    /// Sets when schedule `id` runs next (and, if given, when it last ran).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_schedule_runs(
        &self,
        id: i64,
        next_run_ms: Option<i64>,
        last_run_ms: Option<i64>,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE schedules SET next_run_ms = ?2, last_run_ms = COALESCE(?3, last_run_ms), updated_ms = ?4
             WHERE id = ?1",
            params![id, next_run_ms, last_run_ms, now_ms()],
        )?;
        Ok(())
    }

    /// Pauses or resumes schedule `id`, with its next run (resuming starts
    /// from now, never catching up). Returns whether it exists.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_schedule_paused(
        &self,
        id: i64,
        paused: bool,
        next_run_ms: Option<i64>,
    ) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "UPDATE schedules SET paused = ?2, next_run_ms = ?3, updated_ms = ?4 WHERE id = ?1",
            params![id, paused, next_run_ms, now_ms()],
        )? > 0)
    }

    /// Changes where schedule `id`'s times are kept (the Mac moved to
    /// another time zone) and its next run.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_schedule_tz(
        &self,
        id: i64,
        tz: &str,
        next_run_ms: Option<i64>,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE schedules SET tz = ?2, next_run_ms = ?3, updated_ms = ?4 WHERE id = ?1",
            params![id, tz, next_run_ms, now_ms()],
        )?;
        Ok(())
    }

    /// Deletes schedule `id` and its runs. Returns whether it existed.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn delete_schedule(&self, id: i64) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM schedule_runs WHERE schedule = ?1", [id])?;
        let gone = tx.execute("DELETE FROM schedules WHERE id = ?1", [id])? > 0;
        tx.commit()?;
        Ok(gone)
    }

    /// Notes a run of schedule `id` (or one it skipped): when it was due,
    /// when it started (if it did), its outcome and its chat message.
    /// Returns the run's id.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_schedule_run(
        &self,
        schedule: i64,
        due_ms: i64,
        started_ms: Option<i64>,
        outcome: &str,
        turn: Option<u64>,
    ) -> Result<i64, StoreError> {
        let conn = self.conn();
        // A run that didn't start ended when it was noted.
        let ended = started_ms.is_none().then(now_ms);
        conn.execute(
            "INSERT INTO schedule_runs (schedule, due_ms, started_ms, ended_ms, outcome, turn)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                schedule,
                due_ms,
                started_ms,
                ended,
                outcome,
                turn.map(|t| i64::try_from(t).unwrap_or(i64::MAX))
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// A run ended: its outcome (`ok`, `failed`) and when.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn end_schedule_run(
        &self,
        run: i64,
        outcome: &str,
        ended_ms: i64,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE schedule_runs SET outcome = ?2, ended_ms = ?3 WHERE id = ?1",
            params![run, outcome, ended_ms],
        )?;
        Ok(())
    }

    /// Runs that started and never ended (oxbot stopped meanwhile) end as
    /// `failed`. Returns how many.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn end_unfinished_runs(&self) -> Result<usize, StoreError> {
        Ok(self.conn().execute(
            "UPDATE schedule_runs SET outcome = 'failed', ended_ms = ?1 WHERE outcome = 'running'",
            [now_ms()],
        )?)
    }

    /// Each schedule's latest run, by schedule id.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn latest_schedule_runs(&self) -> Result<HashMap<i64, RunRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_runs
             WHERE id IN (SELECT MAX(id) FROM schedule_runs GROUP BY schedule)"
        ))?;
        let rows = stmt.query_map([], run_row)?;
        rows.map(|r| r.map(|run| (run.schedule, run)))
            .collect::<Result<_, _>>()
            .map_err(StoreError::from)
    }

    /// Schedule `id`'s runs, oldest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn schedule_runs(&self, id: i64) -> Result<Vec<RunRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM schedule_runs WHERE schedule = ?1 ORDER BY id"
        ))?;
        let rows = stmt.query_map([id], run_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_protocol::Day;

    fn new(bot: &str, person: &PersonId, next: i64) -> NewSchedule {
        NewSchedule {
            bot: BotId::new(bot).unwrap(),
            person: person.clone(),
            task: "today's prep plan".into(),
            rule: ScheduleRule::Weekly {
                days: vec![Day::Sat],
                time: "18:00".into(),
            },
            tz: "America/Toronto".into(),
            next_run_ms: Some(next),
        }
    }

    #[test]
    fn schedules_are_kept_with_their_runs_and_go_with_their_bot() {
        let store = Store::open_in_memory().unwrap();
        let sam = PersonId::random();
        let a = store
            .add_schedule(&new("prep", &sam, 100), 30)
            .unwrap()
            .unwrap();
        let b = store
            .add_schedule(&new("news", &sam, 50), 30)
            .unwrap()
            .unwrap();
        assert_eq!(
            store.schedule(a.id).unwrap().unwrap().task,
            "today's prep plan"
        );
        assert_eq!(
            store
                .schedules()
                .unwrap()
                .iter()
                .map(|s| s.id)
                .collect::<Vec<_>>(),
            [b.id, a.id],
            "soonest first"
        );
        assert_eq!(store.due_schedules(60).unwrap().len(), 1);
        assert_eq!(store.earliest_next_run().unwrap(), Some(50));

        // Paused: never due.
        assert!(store.set_schedule_paused(b.id, true, Some(50)).unwrap());
        assert!(
            store
                .due_schedules(1000)
                .unwrap()
                .iter()
                .all(|s| s.id != b.id)
        );

        let run = store
            .add_schedule_run(a.id, 100, Some(101), "running", Some(7))
            .unwrap();
        store
            .add_schedule_run(a.id, 90, None, "missed", None)
            .unwrap();
        assert_eq!(
            store.latest_schedule_runs().unwrap()[&a.id].outcome,
            "missed"
        );
        store.end_schedule_run(run, "ok", 120).unwrap();
        let runs = store.schedule_runs(a.id).unwrap();
        assert_eq!(
            (runs[0].outcome.as_str(), runs[0].turn, runs[0].ended_ms),
            ("ok", Some(7), Some(120))
        );
        assert!(runs[1].ended_ms.is_some(), "a skipped run is over at once");
        store.set_schedule_runs(a.id, None, Some(101)).unwrap();
        let a2 = store.schedule(a.id).unwrap().unwrap();
        assert_eq!((a2.next_run_ms, a2.last_run_ms), (None, Some(101)));

        store.delete_bot(&BotId::new("prep").unwrap()).unwrap();
        assert!(store.schedule(a.id).unwrap().is_none());
        assert_eq!(store.schedule_runs(a.id).unwrap(), []);
        assert!(store.delete_schedule(b.id).unwrap());
        assert!(!store.delete_schedule(b.id).unwrap());
    }

    #[test]
    fn a_person_has_at_most_so_many() {
        let store = Store::open_in_memory().unwrap();
        let sam = PersonId::random();
        for _ in 0..3 {
            assert!(
                store
                    .add_schedule(&new("prep", &sam, 1), 3)
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            store
                .add_schedule(&new("prep", &sam, 1), 3)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .add_schedule(&new("prep", &PersonId::random(), 1), 3)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn runs_cut_short_by_a_stop_end_as_failed() {
        let store = Store::open_in_memory().unwrap();
        let s = store
            .add_schedule(&new("prep", &PersonId::random(), 1), 30)
            .unwrap()
            .unwrap();
        store
            .add_schedule_run(s.id, 1, Some(2), "running", Some(3))
            .unwrap();
        assert_eq!(store.end_unfinished_runs().unwrap(), 1);
        assert_eq!(store.schedule_runs(s.id).unwrap()[0].outcome, "failed");
    }
}
