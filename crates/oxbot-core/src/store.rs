//! [`Store`]: every bot's history and Claude session, in SQLite.
//!
//! Events are numbered per bot from 1 with no gaps, so a client that saw up to
//! `seq` N can ask for everything after N and miss nothing.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use oxbot_claude::{LimitWindow, TurnUsage};
use oxbot_protocol::{BotEvent, BotUsage, EventKind, Person, PlanLimit, SiteInfo, Usage};
use rusqlite::{Connection, OptionalExtension, params};

use crate::id::BotId;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS events (
    bot   TEXT    NOT NULL,
    seq   INTEGER NOT NULL,
    ts_ms INTEGER NOT NULL,
    body  TEXT    NOT NULL,  -- EventKind as JSON
    PRIMARY KEY (bot, seq)
);
CREATE TABLE IF NOT EXISTS sessions (
    bot            TEXT PRIMARY KEY,
    claude_session TEXT NOT NULL,
    updated_ms     INTEGER NOT NULL
);
";

/// Schema changes after the first release, in order. `PRAGMA user_version`
/// records how many have run.
const MIGRATIONS: &[&str] = &[
    // 1: remember the system prompt each session started with.
    "ALTER TABLE sessions ADD COLUMN prompt TEXT NOT NULL DEFAULT ''",
    // 2: Claude usage per bot, and the plan's limits as last reported.
    "CREATE TABLE usage (
        bot                TEXT PRIMARY KEY,
        turns              INTEGER NOT NULL DEFAULT 0,
        input_tokens       INTEGER NOT NULL DEFAULT 0,
        output_tokens      INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd           REAL    NOT NULL DEFAULT 0
    );
    CREATE TABLE limits (
        name         TEXT PRIMARY KEY,
        used         REAL    NOT NULL,
        resets_at_ms INTEGER NOT NULL,
        status       TEXT    NOT NULL,
        seen_ms      INTEGER NOT NULL
    );",
    // 3: the people who send messages, each with their own color.
    "CREATE TABLE people (
        name     TEXT PRIMARY KEY,
        color    INTEGER NOT NULL,
        first_ms INTEGER NOT NULL
    );",
    // 4: the sites the user signed in to through oxbot's sign-in window.
    "CREATE TABLE sites (
        site         TEXT PRIMARY KEY,
        signed_in_ms INTEGER NOT NULL
    );",
    // 5: sign-ins from before the sites list, taken from the chat history
    // (as hosts; the hub turns them into main domains).
    "INSERT OR IGNORE INTO sites (site, signed_in_ms)
        SELECT json_extract(body, '$.site'), MAX(ts_ms) FROM events
        WHERE json_extract(body, '$.kind') = 'sign_in'
          AND json_extract(body, '$.outcome') = 'done'
          AND json_extract(body, '$.site') IS NOT NULL
        GROUP BY json_extract(body, '$.site');",
    // 6: each person has their own browser: sites belong to a browser's
    // owner ('' is the home browser every shared bot uses).
    "CREATE TABLE sites_by_owner (
        owner        TEXT    NOT NULL DEFAULT '',
        site         TEXT    NOT NULL,
        signed_in_ms INTEGER NOT NULL,
        PRIMARY KEY (owner, site)
    );
    INSERT INTO sites_by_owner (owner, site, signed_in_ms) SELECT '', site, signed_in_ms FROM sites;
    DROP TABLE sites;
    ALTER TABLE sites_by_owner RENAME TO sites;",
];

/// The owner name of the home browser's sites.
pub const HOME: &str = "";

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

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored event couldn't be read back.
    #[error("bad stored event: {0}")]
    Json(#[from] serde_json::Error),
}

/// The history database. Calls are short and synchronous; one connection is
/// shared behind a mutex, which is plenty for one person's bots.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

fn person(name: &str, color: i64) -> Person {
    let i = usize::try_from(color).unwrap_or(0) % PEOPLE_COLORS.len();
    Person {
        name: name.to_owned(),
        color: PEOPLE_COLORS[i].to_owned(),
    }
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
        Self::setup(Connection::open(path)?)
    }

    /// An in-memory database, for tests.
    ///
    /// # Errors
    /// Fails only if SQLite can't start.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::setup(Connection::open_in_memory()?)
    }

    fn setup(mut conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(SCHEMA)?;
        let done: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let done = usize::try_from(done).unwrap_or(0);
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(done) {
            // A migration and its version bump commit together, so a crash
            // mid-way (6 rebuilds a table) leaves the database as it was.
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave SQLite half-written
        // (each call is one transaction), so carry on.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The person called `name`, giving them the first color nobody has yet
    /// if they're new. The flag says whether they're new.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person(&self, name: &str) -> Result<(Person, bool), StoreError> {
        let conn = self.conn();
        let found: Option<i64> = conn
            .query_row("SELECT color FROM people WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(color) = found {
            return Ok((person(name, color), false));
        }
        let mut stmt = conn.prepare("SELECT color FROM people")?;
        let taken: Vec<i64> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        let n = i64::try_from(PEOPLE_COLORS.len()).unwrap_or(12);
        let color = (0..n)
            .find(|c| !taken.contains(c))
            .unwrap_or_else(|| i64::try_from(taken.len()).unwrap_or(0) % n);
        conn.execute(
            "INSERT INTO people (name, color, first_ms) VALUES (?1, ?2, ?3)",
            params![name, color, now_ms()],
        )?;
        Ok((person(name, color), true))
    }

    /// Everyone who has sent a message, in the order oxbot first saw them.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn people(&self) -> Result<Vec<Person>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT name, color FROM people ORDER BY first_ms, rowid")?;
        let rows = stmt.query_map([], |r| Ok(person(&r.get::<_, String>(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Remembers that `owner`'s browser ([`HOME`] for the home one) signed
    /// in to `site` (a main domain) now.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_site(&self, owner: &str, site: &str) -> Result<(), StoreError> {
        self.add_site_at(owner, site, now_ms())
    }

    /// Remembers a sign-in to `site` at `when_ms`; a later one wins.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_site_at(&self, owner: &str, site: &str, when_ms: i64) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO sites (owner, site, signed_in_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(owner, site) DO UPDATE SET signed_in_ms = MAX(signed_in_ms, excluded.signed_in_ms)",
            params![owner, site, when_ms],
        )?;
        Ok(())
    }

    /// Forgets `site` for `owner`'s browser. Returns whether it was there.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn remove_site(&self, owner: &str, site: &str) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "DELETE FROM sites WHERE owner = ?1 AND site = ?2",
            params![owner, site],
        )? > 0)
    }

    /// The sites `owner`'s browser signed in to, newest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn sites(&self, owner: &str) -> Result<Vec<SiteInfo>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT site, signed_in_ms FROM sites WHERE owner = ?1
             ORDER BY signed_in_ms DESC, site",
        )?;
        let rows = stmt.query_map([owner], |r| {
            Ok(SiteInfo {
                site: r.get(0)?,
                signed_in_ms: r.get(1)?,
                icon: None,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every browser owner with remembered sites ([`HOME`] included).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn site_owners(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT owner FROM sites ORDER BY owner")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Forgets everything about a deleted bot: its history, session and usage.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn delete_bot(&self, bot: &BotId) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for table in ["events", "sessions", "usage"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE bot = ?1"),
                [bot.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Stores a new event for `bot` and returns it with its number.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn append(&self, bot: &BotId, kind: EventKind) -> Result<BotEvent, StoreError> {
        let body = serde_json::to_string(&kind)?;
        let ts_ms = now_ms();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE bot = ?1",
            params![bot.as_str()],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO events (bot, seq, ts_ms, body) VALUES (?1, ?2, ?3, ?4)",
            params![bot.as_str(), seq, ts_ms, body],
        )?;
        tx.commit()?;
        Ok(BotEvent {
            bot: bot.to_string(),
            seq: u64::try_from(seq).unwrap_or(0),
            ts_ms,
            kind,
        })
    }

    /// Adds one finished turn's tokens and cost to `bot`'s totals.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_usage(&self, bot: &BotId, turn: &TurnUsage) -> Result<(), StoreError> {
        let n = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        self.conn().execute(
            "INSERT INTO usage (bot, turns, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd)
             VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(bot) DO UPDATE SET
               turns = turns + 1,
               input_tokens = input_tokens + excluded.input_tokens,
               output_tokens = output_tokens + excluded.output_tokens,
               cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
               cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens,
               cost_usd = cost_usd + excluded.cost_usd",
            params![
                bot.as_str(),
                n(turn.input_tokens),
                n(turn.output_tokens),
                n(turn.cache_read_tokens),
                n(turn.cache_write_tokens),
                turn.cost_usd
            ],
        )?;
        Ok(())
    }

    /// Replaces the plan's limits with what Claude just reported.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_limits(&self, windows: &[LimitWindow]) -> Result<(), StoreError> {
        let seen = now_ms();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM limits", [])?;
        for w in windows {
            tx.execute(
                "INSERT INTO limits (name, used, resets_at_ms, status, seen_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![w.name, w.used, w.resets_at.saturating_mul(1000), w.status, seen],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The plan's limits and every bot's totals, most used bot first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn usage(&self) -> Result<Usage, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT name, used, resets_at_ms, status, seen_ms FROM limits ORDER BY resets_at_ms",
        )?;
        let mut limits_seen_ms = 0;
        let limits = stmt
            .query_map([], |r| {
                Ok((
                    PlanLimit {
                        name: r.get(0)?,
                        used: r.get(1)?,
                        resets_at_ms: r.get(2)?,
                        status: r.get(3)?,
                    },
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(limit, seen)| {
                limits_seen_ms = limits_seen_ms.max(seen);
                limit
            })
            .collect();
        let mut stmt = conn.prepare(
            "SELECT bot, turns, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd
             FROM usage ORDER BY input_tokens + output_tokens + cache_read_tokens + cache_write_tokens DESC",
        )?;
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        let bots = stmt
            .query_map([], |r| {
                Ok(BotUsage {
                    bot: r.get(0)?,
                    turns: n(r.get(1)?),
                    input_tokens: n(r.get(2)?),
                    output_tokens: n(r.get(3)?),
                    cache_read_tokens: n(r.get(4)?),
                    cache_write_tokens: n(r.get(5)?),
                    cost_usd: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Usage {
            limits,
            limits_seen_ms,
            bots,
        })
    }

    /// Events for `bot` with `seq > since`, oldest first.
    ///
    /// # Errors
    /// Fails if SQLite fails or a stored event is unreadable.
    pub fn history(&self, bot: &BotId, since: u64) -> Result<Vec<BotEvent>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, ts_ms, body FROM events WHERE bot = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt.query_map(
            params![bot.as_str(), i64::try_from(since).unwrap_or(i64::MAX)],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?;
        let mut events = Vec::new();
        for row in rows {
            let (seq, ts_ms, body) = row?;
            events.push(BotEvent {
                bot: bot.to_string(),
                seq: u64::try_from(seq).unwrap_or(0),
                ts_ms,
                kind: serde_json::from_str(&body)?,
            });
        }
        Ok(events)
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
                "INSERT INTO sessions (bot, claude_session, prompt, updated_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(bot) DO UPDATE SET claude_session = ?2, prompt = ?3, updated_ms = ?4",
                params![bot.as_str(), s.id, s.prompt, now_ms()],
            )?,
            None => conn.execute("DELETE FROM sessions WHERE bot = ?1", params![bot.as_str()])?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_protocol::SignInOutcome;

    #[test]
    fn each_person_keeps_their_own_color() {
        let store = Store::open_in_memory().unwrap();
        let (jay, new) = store.person("Jay").unwrap();
        assert!(new);
        let (anu, _) = store.person("Anu").unwrap();
        assert_ne!(jay.color, anu.color);
        assert_eq!(store.person("Jay").unwrap(), (jay.clone(), false));
        assert_eq!(store.people().unwrap(), [jay, anu]);
        let all: std::collections::HashSet<String> = (0..12)
            .map(|i| store.person(&format!("p{i}")).unwrap().0.color)
            .collect();
        assert_eq!(all.len(), 12, "the first 12 people all differ");
    }

    fn bot(id: &str) -> BotId {
        BotId::new(id).unwrap()
    }

    #[test]
    fn sign_ins_from_before_the_sites_list_are_found_in_the_history() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("oxbot.db");
        {
            // A database from before migration 5, with two sign-ins in it.
            let store = Store::open(&path).unwrap();
            let bot = bot("a");
            for (site, outcome) in [
                ("substack.com", SignInOutcome::Done),
                ("mail.example.com", SignInOutcome::Cancelled),
            ] {
                store
                    .append(
                        &bot,
                        EventKind::SignIn {
                            site: site.into(),
                            outcome,
                        },
                    )
                    .unwrap();
            }
            let conn = store.conn();
            conn.execute_batch("DELETE FROM sites; PRAGMA user_version = 4;")
                .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let names: Vec<String> = store
            .sites(HOME)
            .unwrap()
            .into_iter()
            .map(|s| s.site)
            .collect();
        assert_eq!(names, ["substack.com"], "only finished sign-ins count");
    }

    #[test]
    fn signed_in_sites_are_kept_until_signed_out() {
        let store = Store::open_in_memory().unwrap();
        store.add_site(HOME, "substack.com").unwrap();
        store.add_site(HOME, "google.com").unwrap();
        store.add_site(HOME, "substack.com").unwrap(); // signing in again updates it
        let names: Vec<String> = store
            .sites(HOME)
            .unwrap()
            .into_iter()
            .map(|s| s.site)
            .collect();
        assert_eq!(names.len(), 2);
        assert!(store.remove_site(HOME, "google.com").unwrap());
        assert!(!store.remove_site(HOME, "google.com").unwrap());
        assert_eq!(store.sites(HOME).unwrap()[0].site, "substack.com");
    }

    #[test]
    fn each_browser_keeps_its_own_sites() {
        let store = Store::open_in_memory().unwrap();
        store.add_site(HOME, "google.com").unwrap();
        store.add_site("Sam", "google.com").unwrap();
        store.add_site("Sam", "bank.example").unwrap();
        assert_eq!(store.sites(HOME).unwrap().len(), 1);
        assert_eq!(store.sites("Sam").unwrap().len(), 2);
        assert!(store.remove_site("Sam", "google.com").unwrap());
        assert_eq!(store.sites(HOME).unwrap().len(), 1, "home keeps its own");
        assert_eq!(store.site_owners().unwrap(), ["", "Sam"]);
    }

    #[test]
    fn a_deleted_bot_leaves_nothing_behind() {
        let store = Store::open_in_memory().unwrap();
        let a = bot("a");
        store
            .append(
                &a,
                EventKind::UserMessage {
                    text: "hi".into(),
                    from: None,
                },
            )
            .unwrap();
        store
            .append(
                &bot("b"),
                EventKind::UserMessage {
                    text: "hi".into(),
                    from: None,
                },
            )
            .unwrap();
        store.delete_bot(&a).unwrap();
        assert_eq!(store.last_seq(&a).unwrap(), 0);
        assert_eq!(store.last_seq(&bot("b")).unwrap(), 1);
    }

    #[test]
    fn events_are_numbered_per_bot() {
        let store = Store::open_in_memory().unwrap();
        let a = store
            .append(
                &bot("a"),
                EventKind::UserMessage {
                    text: "hi".into(),
                    from: None,
                },
            )
            .unwrap();
        let b = store
            .append(
                &bot("b"),
                EventKind::UserMessage {
                    text: "yo".into(),
                    from: None,
                },
            )
            .unwrap();
        let a2 = store
            .append(
                &bot("a"),
                EventKind::AssistantMessage {
                    text: "hello".into(),
                },
            )
            .unwrap();
        assert_eq!((a.seq, b.seq, a2.seq), (1, 1, 2));
        assert_eq!(store.last_seq(&bot("a")).unwrap(), 2);
        assert_eq!(store.last_seq(&bot("c")).unwrap(), 0);
        // Only replies count for "new messages": b has none, a's is #2.
        assert_eq!(store.last_reply_seq(&bot("a")).unwrap(), 2);
        assert_eq!(store.last_reply_seq(&bot("b")).unwrap(), 0);
    }

    #[test]
    fn history_returns_events_after_since() {
        let store = Store::open_in_memory().unwrap();
        for i in 0..3 {
            store
                .append(
                    &bot("a"),
                    EventKind::UserMessage {
                        text: i.to_string(),
                        from: None,
                    },
                )
                .unwrap();
        }
        let after_one = store.history(&bot("a"), 1).unwrap();
        assert_eq!(after_one.iter().map(|e| e.seq).collect::<Vec<_>>(), [2, 3]);
        assert_eq!(
            after_one[0].kind,
            EventKind::UserMessage {
                text: "1".into(),
                from: None
            }
        );
    }

    #[test]
    fn sessions_can_be_saved_replaced_and_forgotten() {
        let store = Store::open_in_memory().unwrap();
        let saved = |id: &str| SavedSession {
            id: id.into(),
            prompt: "p".into(),
        };
        assert_eq!(store.session(&bot("a")).unwrap(), None);
        store.set_session(&bot("a"), Some(&saved("s1"))).unwrap();
        store.set_session(&bot("a"), Some(&saved("s2"))).unwrap();
        assert_eq!(store.session(&bot("a")).unwrap(), Some(saved("s2")));
        store.set_session(&bot("a"), None).unwrap();
        assert_eq!(store.session(&bot("a")).unwrap(), None);
    }

    #[test]
    fn an_old_database_is_migrated() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("old.db");
        // The milestone 2 schema, without the prompt column.
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE sessions (bot TEXT PRIMARY KEY, claude_session TEXT NOT NULL, updated_ms INTEGER NOT NULL);
                 INSERT INTO sessions VALUES ('a', 's1', 0);",
            )
            .unwrap();
        let store = Store::open(&path).unwrap();
        let saved = store.session(&bot("a")).unwrap().unwrap();
        assert_eq!((saved.id.as_str(), saved.prompt.as_str()), ("s1", ""));
        drop(store);
        Store::open(&path).unwrap(); // migrating twice is a no-op
    }

    #[test]
    fn history_survives_reopening() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("t.db");
        Store::open(&path)
            .unwrap()
            .append(
                &bot("a"),
                EventKind::UserMessage {
                    text: "kept".into(),
                    from: None,
                },
            )
            .unwrap();
        let events = Store::open(&path).unwrap().history(&bot("a"), 0).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn usage_adds_up_and_limits_are_replaced() {
        let store = Store::open_in_memory().unwrap();
        let bot = BotId::new("news").unwrap();
        let turn = TurnUsage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: 100,
            cache_write_tokens: 50,
            cost_usd: 0.25,
        };
        store.add_usage(&bot, &turn).unwrap();
        store.add_usage(&bot, &turn).unwrap();
        let usage = store.usage().unwrap();
        assert_eq!(usage.bots.len(), 1);
        assert_eq!((usage.bots[0].turns, usage.bots[0].output_tokens), (2, 10));
        assert!((usage.bots[0].cost_usd - 0.5).abs() < 1e-9);
        assert_eq!(usage.limits, []);

        let window = |name: &str, used: f64, resets_at: i64| LimitWindow {
            name: name.into(),
            used,
            resets_at,
            status: "allowed".into(),
        };
        store
            .set_limits(&[window("seven_day", 0.3, 20), window("five_hour", 0.1, 10)])
            .unwrap();
        store
            .set_limits(&[window("five_hour", 0.2, 10), window("seven_day", 0.4, 20)])
            .unwrap();
        let usage = store.usage().unwrap();
        assert_eq!(usage.limits.len(), 2);
        assert_eq!(
            (usage.limits[0].name.as_str(), usage.limits[0].used),
            ("five_hour", 0.2)
        );
        assert_eq!(usage.limits[1].resets_at_ms, 20_000);
        assert!(usage.limits_seen_ms > 0);
    }
}
