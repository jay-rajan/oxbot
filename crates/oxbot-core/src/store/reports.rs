//! Research reports, and the web pages each bot's session has
//! opened (a report may cite only those).

use oxbot_protocol::{BotEvent, EventKind, ReportInfo};
use rusqlite::{OptionalExtension as _, params};

use super::events::append_in;
use super::{Store, StoreError, now_ms};
use crate::id::{BotId, PersonId};
use crate::report::{Report, Source, Visit};

/// The most opened pages kept per bot; the oldest go first.
pub const MAX_SESSION_PAGES: usize = 500;

/// One row of `reports` as read back.
type Row = (
    String,
    i64,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn report_from(row: Row) -> Result<Report, StoreError> {
    let (id, created_ms, asked_by, question, title, summary, markdown, sources, links) = row;
    let sources: Vec<Source> = serde_json::from_str(&sources)?;
    let links: Vec<String> = serde_json::from_str(&links)?;
    Ok(Report {
        id,
        created_ms,
        // An id that doesn't parse (edited by hand) shows as nobody.
        asked_by: asked_by.and_then(|id| PersonId::new(id).ok()),
        question,
        title,
        summary,
        markdown,
        sources,
        links,
    })
}

const COLUMNS: &str =
    "id, created_ms, asked_by, question, title, summary, markdown, sources, links";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
    ))
}

impl Store {
    /// Saves `report` for `bot` together with its card in the chat (an
    /// [`EventKind::Report`]), in one transaction: there's never a card
    /// without its report, or the other way round. The report's
    /// `created_ms` becomes the event's time.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_report(&self, bot: &BotId, report: &mut Report) -> Result<BotEvent, StoreError> {
        let sources = serde_json::to_string(&report.sources)?;
        let links = serde_json::to_string(&report.links)?;
        let kind = EventKind::Report {
            id: report.id.clone(),
            title: report.title.clone(),
            summary: report.summary.clone(),
            sources: u32::try_from(report.sources.len()).unwrap_or(u32::MAX),
        };
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let event = append_in(&tx, bot, kind)?;
        report.created_ms = event.ts_ms;
        tx.execute(
            "INSERT INTO reports (id, bot, seq, created_ms, asked_by, question, title, summary,
                                  markdown, sources, links)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                report.id,
                bot.as_str(),
                i64::try_from(event.seq).unwrap_or(i64::MAX),
                report.created_ms,
                report.asked_by.as_ref().map(PersonId::as_str),
                report.question,
                report.title,
                report.summary,
                report.markdown,
                sources,
                links,
            ],
        )?;
        tx.commit()?;
        Ok(event)
    }

    /// `bot`'s report `id`. Looked up by both, so an id from another bot
    /// finds nothing.
    ///
    /// # Errors
    /// Fails if SQLite fails or the row can't be read.
    pub fn report(&self, bot: &BotId, id: &str) -> Result<Option<Report>, StoreError> {
        let row = self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM reports WHERE bot = ?1 AND id = ?2"),
                params![bot.as_str(), id],
                read_row,
            )
            .optional()?;
        row.map(report_from).transpose()
    }

    /// `bot`'s reports, newest first, as a list shows them.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn reports(&self, bot: &BotId) -> Result<Vec<ReportInfo>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, title, summary, created_ms, json_array_length(sources) FROM reports
             WHERE bot = ?1 ORDER BY created_ms DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([bot.as_str()], |r| {
            Ok(ReportInfo {
                id: r.get(0)?,
                title: r.get(1)?,
                summary: r.get(2)?,
                created_ms: r.get(3)?,
                sources: u32::try_from(r.get::<_, i64>(4)?).unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Titles of the reports `bot` saved after its event `since`, oldest
    /// first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn report_titles_since(&self, bot: &BotId, since: u64) -> Result<Vec<String>, StoreError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT title FROM reports WHERE bot = ?1 AND seq > ?2 ORDER BY seq")?;
        let rows = stmt.query_map(
            params![bot.as_str(), i64::try_from(since).unwrap_or(i64::MAX)],
            |r| r.get(0),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every one of `bot`'s reports in full, oldest first (to keep with a
    /// deleted bot's files).
    ///
    /// # Errors
    /// Fails if SQLite fails or a row can't be read.
    pub fn all_reports(&self, bot: &BotId) -> Result<Vec<Report>, StoreError> {
        let rows: Vec<Row> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM reports WHERE bot = ?1 ORDER BY created_ms, rowid"
            ))?;
            stmt.query_map([bot.as_str()], read_row)?
                .collect::<Result<_, _>>()?
        };
        rows.into_iter().map(report_from).collect()
    }

    /// Deletes `bot`'s report `id`. Returns whether it was there.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn delete_report(&self, bot: &BotId, id: &str) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "DELETE FROM reports WHERE bot = ?1 AND id = ?2",
            params![bot.as_str(), id],
        )? > 0)
    }

    /// Notes that `bot`'s current session opened the web page `url`
    /// (normalized) titled `title`. The first time counts; at most
    /// [`MAX_SESSION_PAGES`] are kept, the oldest dropped.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_session_page(&self, bot: &BotId, url: &str, title: &str) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO session_pages (bot, url, title, first_ms) VALUES (?1, ?2, ?3, ?4)",
            params![bot.as_str(), url, title, now_ms()],
        )?;
        tx.execute(
            "DELETE FROM session_pages WHERE bot = ?1 AND rowid NOT IN (
                SELECT rowid FROM session_pages WHERE bot = ?1
                ORDER BY first_ms DESC, rowid DESC LIMIT ?2)",
            params![
                bot.as_str(),
                i64::try_from(MAX_SESSION_PAGES).unwrap_or(i64::MAX)
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The web pages `bot`'s current session has opened: (url,
    /// visit), oldest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn session_pages(&self, bot: &BotId) -> Result<Vec<(String, Visit)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT url, title, first_ms FROM session_pages WHERE bot = ?1
             ORDER BY first_ms, rowid",
        )?;
        let rows = stmt.query_map([bot.as_str()], |r| {
            Ok((
                r.get(0)?,
                Visit {
                    title: r.get(1)?,
                    first_ms: r.get(2)?,
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
