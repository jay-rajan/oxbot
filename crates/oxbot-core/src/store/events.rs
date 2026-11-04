//! Events: each bot's history, numbered from 1, and its latest numbers.

use std::collections::HashMap;

use oxbot_protocol::{BotEvent, EventKind};
use rusqlite::{OptionalExtension, params};

use super::{HistoryPage, Seqs, Store, StoreError, now_ms};
use crate::id::{BotId, PersonId};

/// A stored event row as `(seq, ts_ms, body)`.
pub(super) type Row = (i64, i64, String);

pub(super) fn event_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
}

/// Reads a stored event back. An event this version can't read (say,
/// written by a newer oxbot) is `None`, so the rest of the chat still shows.
pub(super) fn parse_event(bot: &BotId, (seq, ts_ms, body): Row) -> Option<BotEvent> {
    match serde_json::from_str(&body) {
        Ok(kind) => Some(BotEvent {
            bot: bot.to_string(),
            seq: u64::try_from(seq).unwrap_or(0),
            ts_ms,
            kind,
        }),
        Err(err) => {
            tracing::warn!(%bot, seq, %err, "skipping a stored event this version can't read");
            None
        }
    }
}

/// Parses rows read earlier (outside the lock), skipping unreadable ones.
fn parse_rows(bot: &BotId, rows: Vec<Row>) -> Vec<BotEvent> {
    rows.into_iter()
        .filter_map(|row| parse_event(bot, row))
        .collect()
}

/// Every bot's latest event and reply numbers (see [`Store::all_seqs`]).
/// The reply lookup has the same condition as the `events_replies` index,
/// so it's used.
pub(super) const ALL_SEQS: &str = "
    WITH RECURSIVE bots(bot) AS (
        SELECT MIN(bot) FROM events
        UNION ALL
        SELECT (SELECT MIN(e.bot) FROM events e WHERE e.bot > bots.bot)
        FROM bots WHERE bots.bot IS NOT NULL
    )
    SELECT bot,
        (SELECT MAX(seq) FROM events e WHERE e.bot = bots.bot),
        COALESCE((SELECT MAX(seq) FROM events e WHERE e.bot = bots.bot
            AND json_extract(e.body, '$.kind') = 'assistant_message'), 0)
    FROM bots WHERE bot IS NOT NULL";

/// What [`Store::replay`] gives a client catching up.
#[derive(Debug, Clone, PartialEq)]
pub enum Replay {
    /// Every event it missed, oldest first.
    Events(Vec<BotEvent>),
    /// Too many to send one by one: the latest page instead.
    Page(HistoryPage),
}

/// Adds an event for `bot` inside the caller's transaction, numbered
/// after its latest.
pub(super) fn append_in(
    tx: &rusqlite::Transaction<'_>,
    bot: &BotId,
    kind: EventKind,
) -> Result<BotEvent, StoreError> {
    let body = serde_json::to_string(&kind)?;
    let ts_ms = now_ms();
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE bot = ?1",
        params![bot.as_str()],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO events (bot, seq, ts_ms, body) VALUES (?1, ?2, ?3, ?4)",
        params![bot.as_str(), seq, ts_ms, body],
    )?;
    Ok(BotEvent {
        bot: bot.to_string(),
        seq: u64::try_from(seq).unwrap_or(0),
        ts_ms,
        kind,
    })
}

pub(super) fn to_sql(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

pub(super) fn from_sql(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

impl Store {
    /// Forgets everything about a deleted bot: its history, session, usage,
    /// "Always allow" rules, reports, the pages its session opened, who
    /// started its turns, its loop conversations and turns, its test cases,
    /// practice runs, oxbot-tool calls and thumbs, its Codex threads and
    /// what the relay sent for it, and
    /// where anyone filed it in their sections (a new bot can get its id).
    /// Returns the people whose sections it was in.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn delete_bot(&self, bot: &BotId) -> Result<Vec<PersonId>, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for table in ["loop_messages", "loop_progress", "loop_checks"] {
            tx.execute(
                &format!(
                    "DELETE FROM {table} WHERE session IN (SELECT id FROM loop_sessions WHERE bot = ?1)"
                ),
                [bot.as_str()],
            )?;
        }
        tx.execute("DELETE FROM loop_sessions WHERE bot = ?1", [bot.as_str()])?;
        tx.execute(
            "DELETE FROM loop_checks WHERE session IN (SELECT session FROM codex_threads WHERE bot = ?1)",
            [bot.as_str()],
        )?;
        let filed: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT DISTINCT person_id FROM bot_sections WHERE bot = ?1")?;
            let rows = stmt.query_map([bot.as_str()], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        tx.execute(
            "DELETE FROM trial_events WHERE run IN (SELECT id FROM trial_runs WHERE bot = ?1)",
            [bot.as_str()],
        )?;
        tx.execute(
            "DELETE FROM schedule_runs WHERE schedule IN (SELECT id FROM schedules WHERE bot = ?1)",
            [bot.as_str()],
        )?;
        tx.execute("DELETE FROM schedules WHERE bot = ?1", [bot.as_str()])?;
        for table in [
            "events",
            "sessions",
            "usage",
            "allow_rules",
            "bot_sections",
            "reports",
            "session_pages",
            "turn_origins",
            "loop_turns",
            "loop_results",
            "turn_tools",
            "test_cases",
            "trial_batches",
            "trial_runs",
            "turn_ratings",
            "codex_threads",
            "relay_log",
            "engine_upgrades",
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE bot = ?1"),
                [bot.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(filed
            .into_iter()
            .filter_map(|id| PersonId::new(id).ok())
            .collect())
    }

    /// Stores a new event for `bot` and returns it with its number.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn append(&self, bot: &BotId, kind: EventKind) -> Result<BotEvent, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let event = append_in(&tx, bot, kind)?;
        tx.commit()?;
        Ok(event)
    }

    /// Events for `bot` with `seq > since`, oldest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn history(&self, bot: &BotId, since: u64) -> Result<Vec<BotEvent>, StoreError> {
        let rows = self.rows_after(bot, since, None)?;
        Ok(parse_rows(bot, rows))
    }

    /// What a client that has `bot`'s events up to `since` needs to catch
    /// up: the rest, if there are at most `limit`; otherwise only the latest
    /// page of `limit` (it asks for older pages as it needs them).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn replay(&self, bot: &BotId, since: u64, limit: usize) -> Result<Replay, StoreError> {
        // One more than allowed, to know whether there are too many.
        let rows = self.rows_after(bot, since, Some(limit.saturating_add(1)))?;
        if rows.len() > limit {
            return Ok(Replay::Page(self.history_page(bot, None, limit)?));
        }
        Ok(Replay::Events(parse_rows(bot, rows)))
    }

    /// The stored rows of `bot` after `since`, oldest first, at most `limit`.
    /// The lock is let go before anything is parsed.
    fn rows_after(
        &self,
        bot: &BotId,
        since: u64,
        limit: Option<usize>,
    ) -> Result<Vec<Row>, StoreError> {
        let limit = limit.map_or(-1, |n| i64::try_from(n).unwrap_or(i64::MAX));
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, ts_ms, body FROM events WHERE bot = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![bot.as_str(), to_sql(since), limit], event_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// At most `limit` of `bot`'s events just before event `before` (the
    /// latest ones with `None`), oldest first. Reads backwards along the
    /// primary key, so it costs the same however long the history is.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn history_page(
        &self,
        bot: &BotId,
        before: Option<u64>,
        limit: usize,
    ) -> Result<HistoryPage, StoreError> {
        // One more than asked, to know whether there are older ones.
        let want = i64::try_from(limit).unwrap_or(i64::MAX).saturating_add(1);
        let before = before.map_or(i64::MAX, to_sql);
        let mut rows = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT seq, ts_ms, body FROM events WHERE bot = ?1 AND seq < ?2
                 ORDER BY seq DESC LIMIT ?3",
            )?;
            stmt.query_map(params![bot.as_str(), before, want], event_row)?
                .collect::<Result<Vec<_>, _>>()?
        };
        let has_older = rows.len() > limit;
        rows.truncate(limit);
        rows.reverse();
        let first_seq = rows.first().map_or(0, |r| from_sql(r.0));
        let last_seq = rows.last().map_or(0, |r| from_sql(r.0));
        Ok(HistoryPage {
            events: parse_rows(bot, rows),
            first_seq,
            last_seq,
            has_older,
        })
    }

    /// `bot`'s event number `seq`, if it's stored and readable.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn event(&self, bot: &BotId, seq: u64) -> Result<Option<BotEvent>, StoreError> {
        let row = self
            .conn()
            .query_row(
                "SELECT seq, ts_ms, body FROM events WHERE bot = ?1 AND seq = ?2",
                params![bot.as_str(), to_sql(seq)],
                event_row,
            )
            .optional()?;
        Ok(row.and_then(|row| parse_event(bot, row)))
    }

    /// Every bot's latest event and reply numbers, keyed by bot id. One
    /// query that hops from bot to bot along the primary key and looks up
    /// each one's latest event (primary key) and reply (`events_replies`):
    /// a few index seeks per bot, however long the histories are.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn all_seqs(&self) -> Result<HashMap<String, Seqs>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(ALL_SEQS)?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (bot, last, last_reply) = row?;
            out.insert(
                bot,
                Seqs {
                    last: from_sql(last),
                    last_reply: from_sql(last_reply),
                },
            );
        }
        Ok(out)
    }

    /// Number of the latest event for `bot`, or 0.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn last_seq(&self, bot: &BotId) -> Result<u64, StoreError> {
        let seq: i64 = self.conn().query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE bot = ?1",
            params![bot.as_str()],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(seq).unwrap_or(0))
    }

    /// Number of the latest reply from `bot`, or 0.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn last_reply_seq(&self, bot: &BotId) -> Result<u64, StoreError> {
        let seq: i64 = self.conn().query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events \
             WHERE bot = ?1 AND json_extract(body, '$.kind') = 'assistant_message'",
            params![bot.as_str()],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(seq).unwrap_or(0))
    }
}
