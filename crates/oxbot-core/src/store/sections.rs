//! Sidebar sections: each person's own, and which of them each bot is in.
//!
//! Every query is scoped by the person, so a section id from someone else
//! works as if it didn't exist.

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::{Store, StoreError, now_ms};
use crate::id::{BotId, PersonId};

/// One of a person's sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionRecord {
    /// Its id, e.g. `s_0123456789abcdef`.
    pub id: String,
    /// Its name as shown.
    pub name: String,
}

/// Why a section change was refused, or the database failing. Nothing
/// changed either way.
#[derive(Debug, thiserror::Error)]
pub enum SectionError {
    /// The person already has a section called that (ignoring case).
    #[error("there's already a section called that")]
    NameTaken,
    /// The person has as many sections as they may.
    #[error("too many sections")]
    TooMany,
    /// The person has no section with that id.
    #[error("no such section")]
    NoSuchSection,
    /// SQLite failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<rusqlite::Error> for SectionError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}

/// Whether `person` has a section other than `except` called `name`,
/// ignoring case (SQLite's `NOCASE` only folds ASCII).
fn name_taken(
    tx: &Transaction<'_>,
    person: &PersonId,
    name: &str,
    except: Option<&str>,
) -> Result<bool, SectionError> {
    let want = name.to_lowercase();
    let mut stmt = tx.prepare("SELECT id, name FROM sections WHERE person_id = ?1")?;
    let rows = stmt.query_map([person.as_str()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, other) = row?;
        if Some(id.as_str()) != except && other.to_lowercase() == want {
            return Ok(true);
        }
    }
    Ok(false)
}

fn owns(tx: &Transaction<'_>, person: &PersonId, id: &str) -> Result<bool, SectionError> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM sections WHERE person_id = ?1 AND id = ?2",
            params![person.as_str(), id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

impl Store {
    /// `person`'s sections, in order.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn sections(&self, person: &PersonId) -> Result<Vec<SectionRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name FROM sections WHERE person_id = ?1 ORDER BY position, created_ms, id",
        )?;
        let rows = stmt.query_map([person.as_str()], |r| {
            Ok(SectionRecord {
                id: r.get(0)?,
                name: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Which of `person`'s sections each bot is in: (bot id, section id).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn placements(&self, person: &PersonId) -> Result<Vec<(String, String)>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT bot, section_id FROM bot_sections WHERE person_id = ?1 ORDER BY bot",
        )?;
        let rows = stmt.query_map([person.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Makes a section for `person` called `name` (already checked), last
    /// in their list, and puts `bot` in it if given. `max` is the most
    /// sections a person may have. Returns its id.
    ///
    /// # Errors
    /// [`SectionError::NameTaken`], [`SectionError::TooMany`], or SQLite.
    pub fn create_section(
        &self,
        person: &PersonId,
        name: &str,
        bot: Option<&BotId>,
        max: usize,
    ) -> Result<String, SectionError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sections WHERE person_id = ?1",
            [person.as_str()],
            |r| r.get(0),
        )?;
        if usize::try_from(count).unwrap_or(usize::MAX) >= max {
            return Err(SectionError::TooMany);
        }
        if name_taken(&tx, person, name, None)? {
            return Err(SectionError::NameTaken);
        }
        let id: String = tx.query_row(
            "INSERT INTO sections (person_id, name, position, created_ms)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(position), 0) + 1 FROM sections WHERE person_id = ?1), ?3)
             RETURNING id",
            params![person.as_str(), name, now_ms()],
            |r| r.get(0),
        )?;
        if let Some(bot) = bot {
            tx.execute(
                "INSERT OR REPLACE INTO bot_sections (person_id, bot, section_id) VALUES (?1, ?2, ?3)",
                params![person.as_str(), bot.as_str(), id],
            )?;
        }
        tx.commit()?;
        Ok(id)
    }

    /// Renames `person`'s section `id` to `name` (already checked).
    ///
    /// # Errors
    /// [`SectionError::NoSuchSection`], [`SectionError::NameTaken`], or
    /// SQLite.
    pub fn rename_section(
        &self,
        person: &PersonId,
        id: &str,
        name: &str,
    ) -> Result<(), SectionError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !owns(&tx, person, id)? {
            return Err(SectionError::NoSuchSection);
        }
        if name_taken(&tx, person, name, Some(id))? {
            return Err(SectionError::NameTaken);
        }
        tx.execute(
            "UPDATE sections SET name = ?3 WHERE person_id = ?1 AND id = ?2",
            params![person.as_str(), id, name],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes `person`'s section `id`. Its bots go back to not being in a
    /// section; nothing else changes.
    ///
    /// # Errors
    /// [`SectionError::NoSuchSection`], or SQLite.
    pub fn delete_section(&self, person: &PersonId, id: &str) -> Result<(), SectionError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !owns(&tx, person, id)? {
            return Err(SectionError::NoSuchSection);
        }
        tx.execute(
            "DELETE FROM bot_sections WHERE person_id = ?1 AND section_id = ?2",
            params![person.as_str(), id],
        )?;
        tx.execute(
            "DELETE FROM sections WHERE person_id = ?1 AND id = ?2",
            params![person.as_str(), id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Puts `bot` in `person`'s section `section`, or takes it out of
    /// their sections with `None`.
    ///
    /// # Errors
    /// [`SectionError::NoSuchSection`] if `section` isn't theirs, or SQLite.
    pub fn place_bot(
        &self,
        person: &PersonId,
        bot: &BotId,
        section: Option<&str>,
    ) -> Result<(), SectionError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        match section {
            Some(id) => {
                if !owns(&tx, person, id)? {
                    return Err(SectionError::NoSuchSection);
                }
                tx.execute(
                    "INSERT OR REPLACE INTO bot_sections (person_id, bot, section_id) VALUES (?1, ?2, ?3)",
                    params![person.as_str(), bot.as_str(), id],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM bot_sections WHERE person_id = ?1 AND bot = ?2",
                    params![person.as_str(), bot.as_str()],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
