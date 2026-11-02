//! [`Store`]: every bot's history and Claude session, in SQLite.
//!
//! Events are numbered per bot from 1 with no gaps, so a client that saw up to
//! `seq` N can ask for everything after N and miss nothing.
//!
//! One file per table group: [`migrations`] (the schema and its changes),
//! people, sites, gates, events, sessions, usage, "Always allow" rules,
//! sidebar sections, research reports with the pages each session
//! opened, usage alerts and preferences.

mod alerts;
pub use alerts::{AlertChange, LimitAlert};
mod events;
pub use events::Replay;
mod gates;
mod migrations;
mod people;
mod prefs;
mod reports;
pub use reports::MAX_SESSION_PAGES;
mod rules;
pub use rules::{RuleKey, StoredRule};
mod sections;
pub use sections::{SectionError, SectionRecord};
mod sessions;
mod sites;
mod usage;

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use oxbot_protocol::{BotEvent, Person};
use rusqlite::Connection;

use crate::id::{BotId, BrowserOwner, PersonId};
use migrations::{MIGRATIONS, SCHEMA, migrate};
use people::fill_people;
#[cfg(test)]
pub(crate) use people::person_folder;

/// The home browser, as the sites list keys it.
pub const HOME: BrowserOwner = BrowserOwner::Home;

/// People's colors, in the order they're handed out: the first few are as
/// far apart as possible. All are mid-tones that read on white and on dark.
pub const PEOPLE_COLORS: [&str; 12] = [
    "#2F6FEB", "#E2701B", "#2E9A4E", "#B33BC9", "#0F9488", "#E0457B", "#B98A00", "#7A5AF8",
    "#0E93B8", "#A0663A", "#7C8A1C", "#5E6B7D",
];

/// A bot's saved Claude session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSession {
    /// Claude's session id.
    pub id: String,
    /// The system prompt the session started with. Claude keeps a session's
    /// first prompt, so a changed prompt needs a new session.
    pub prompt: String,
}

/// One page of a bot's history, from [`Store::history_page`].
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPage {
    /// The events, oldest first.
    pub events: Vec<BotEvent>,
    /// Number of the oldest event read (0 if none).
    pub first_seq: u64,
    /// Number of the newest event read (0 if none).
    pub last_seq: u64,
    /// Whether there are events before `first_seq`.
    pub has_older: bool,
}

/// A bot's latest event number and latest reply number (0 if none).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Seqs {
    /// Number of its latest event.
    pub last: u64,
    /// Number of its latest [`EventKind::AssistantMessage`].
    pub last_reply: u64,
}

/// A person oxbot knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonRecord {
    /// Their id, which never changes.
    pub id: PersonId,
    /// Their name as shown, e.g. `Sam`. No two people share one.
    pub name: String,
    /// Their color, e.g. `#2F6FEB`.
    pub color: String,
    /// The folder under `people/` that holds their own Chrome profile.
    pub folder: String,
    /// Their own Herder's bot id.
    pub herder: BotId,
    /// Whether they're the Farm's own person (the Mac's owner).
    pub farm: bool,
}

impl PersonRecord {
    /// As clients see them: name and color.
    #[must_use]
    pub fn person(&self) -> Person {
        Person {
            name: self.name.clone(),
            color: self.color.clone(),
        }
    }
}

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored event couldn't be read back.
    #[error("bad stored event: {0}")]
    Json(#[from] serde_json::Error),
    /// A newer oxbot has upgraded the database past what this one knows.
    /// Opening it anyway could undo or break that upgrade.
    #[error(
        "this database was upgraded by a newer oxbot (schema {found}; this oxbot knows up to \
         {known}). Update oxbot, or restore the copy saved before that upgrade (oxbot.db.v{known}.bak, \
         if there is one)"
    )]
    Newer {
        /// The database's schema version.
        found: usize,
        /// The newest schema this oxbot knows.
        known: usize,
    },
    /// The copy taken before an upgrade couldn't be written, so the
    /// upgrade didn't start.
    #[error("couldn't copy the database to {} before upgrading it: {why}", to.display())]
    Backup {
        /// Where the copy was going.
        to: std::path::PathBuf,
        /// What went wrong.
        why: String,
    },
}

/// The history database. Calls are short and synchronous; one connection is
/// shared behind a mutex, which is plenty for one person's bots.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl Store {
    /// Opens (and if needed creates) the database at `path`.
    ///
    /// # Errors
    /// Fails if SQLite can't open or set up the file.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        // A file made just now has nothing worth a copy before upgrading.
        let existed = path.exists();
        Self::setup(Connection::open(path)?, existed)
    }

    /// An in-memory database, for tests.
    ///
    /// # Errors
    /// Fails only if SQLite can't start.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::setup(Connection::open_in_memory()?, false)
    }

    /// Makes (or opens) the database at `path` with only the first
    /// `version` schema changes, as an older oxbot left it. For tests and
    /// test seeds written for that schema (the `test-support` feature);
    /// [`Store::open`] then upgrades it.
    ///
    /// # Errors
    /// Fails if SQLite can't open the file or a change fails.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_at_version(path: &Path, version: usize) -> Result<(), StoreError> {
        let existed = path.exists();
        let mut conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        migrate(&mut conn, version, existed)
    }

    /// Brings `conn` up to date; `back_up` says whether it's a database
    /// file that was there before, worth a copy before tables are rebuilt.
    fn setup(mut conn: Connection, back_up: bool) -> Result<Self, StoreError> {
        conn.execute_batch(SCHEMA)?;
        migrate(&mut conn, MIGRATIONS.len(), back_up)?;
        fill_people(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave SQLite half-written
        // (each call is one transaction), so carry on.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
