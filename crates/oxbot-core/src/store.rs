//! [`Store`]: every bot's history and Claude session, in SQLite.
//!
//! Events are numbered per bot from 1 with no gaps, so a client that saw up to
//! `seq` N can ask for everything after N and miss nothing.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use oxbot_claude::{LimitWindow, TurnUsage};
use oxbot_protocol::{BotEvent, BotUsage, EventKind, GateInfo, Person, PlanLimit, SiteInfo, Usage};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::bot::{personal_herder_id, personal_herder_id_for};
use crate::id::{BotId, BrowserOwner, PersonId};

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

/// One schema change: SQL, or Rust for what SQL can't do (it gets the
/// migration's transaction).
enum Step {
    Sql(&'static str),
    Rust(fn(&Transaction<'_>) -> Result<(), StoreError>),
}

/// Schema changes after the first release, in order. `PRAGMA user_version`
/// records how many have run.
const MIGRATIONS: &[Step] = &[
    // 1: remember the system prompt each session started with.
    Step::Sql("ALTER TABLE sessions ADD COLUMN prompt TEXT NOT NULL DEFAULT ''"),
    // 2: Claude usage per bot, and the plan's limits as last reported.
    Step::Sql(
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
    ),
    // 3: the people who send messages, each with their own color.
    Step::Sql(
        "CREATE TABLE people (
        name     TEXT PRIMARY KEY,
        color    INTEGER NOT NULL,
        first_ms INTEGER NOT NULL
    );",
    ),
    // 4: the sites the user signed in to through oxbot's sign-in window.
    Step::Sql(
        "CREATE TABLE sites (
        site         TEXT PRIMARY KEY,
        signed_in_ms INTEGER NOT NULL
    );",
    ),
    // 5: sign-ins from before the sites list, taken from the chat history
    // (as hosts; migration 9 turns them into main domains).
    Step::Sql(
        "INSERT OR IGNORE INTO sites (site, signed_in_ms)
        SELECT json_extract(body, '$.site'), MAX(ts_ms) FROM events
        WHERE json_extract(body, '$.kind') = 'sign_in'
          AND json_extract(body, '$.outcome') = 'done'
          AND json_extract(body, '$.site') IS NOT NULL
        GROUP BY json_extract(body, '$.site');",
    ),
    // 6: each person has their own browser: sites belong to a browser's
    // owner ('' is the home browser every shared bot uses).
    Step::Sql(
        "CREATE TABLE sites_by_owner (
        owner        TEXT    NOT NULL DEFAULT '',
        site         TEXT    NOT NULL,
        signed_in_ms INTEGER NOT NULL,
        PRIMARY KEY (owner, site)
    );
    INSERT INTO sites_by_owner (owner, site, signed_in_ms) SELECT '', site, signed_in_ms FROM sites;
    DROP TABLE sites;
    ALTER TABLE sites_by_owner RENAME TO sites;",
    ),
    // 7: Gates (phones and laptops) paired with this Farm. Only a hash of
    // each Gate's key is kept, so a copy of the database can't pose as one.
    Step::Sql(
        "CREATE TABLE gates (
        id           TEXT    PRIMARY KEY,
        person       TEXT    NOT NULL,
        name         TEXT    NOT NULL,
        key_hash     TEXT    NOT NULL UNIQUE,
        paired_ms    INTEGER NOT NULL,
        last_seen_ms INTEGER NOT NULL
    );",
    ),
    // 8: find each bot's latest reply without reading its whole history.
    Step::Sql(
        "CREATE INDEX IF NOT EXISTS events_replies ON events (bot, seq)
        WHERE json_extract(body, '$.kind') = 'assistant_message';",
    ),
    // 9: people get ids, so a new name never takes over someone's bots or
    // sign-ins. Browser owners and Gates point at ids; sites are named by
    // their main domain once, here, instead of on every start.
    Step::Rust(people_get_ids),
    // 10: whether a Claude session has read text oxbot doesn't trust (and
    // what), kept with the session because Claude remembers it on resume.
    Step::Rust(sessions_remember_taint),
];

/// Migration 10. Skipped if the column is already there (a database
/// rewound to an older version for a test keeps its columns).
fn sessions_remember_taint(tx: &Transaction<'_>) -> Result<(), StoreError> {
    let has: bool = tx.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('sessions') WHERE name = 'tainted_by'",
        [],
        |r| r.get(0),
    )?;
    if !has {
        tx.execute_batch("ALTER TABLE sessions ADD COLUMN tainted_by TEXT")?;
    }
    Ok(())
}

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
}

/// The history database. Calls are short and synchronous; one connection is
/// shared behind a mutex, which is plenty for one person's bots.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

fn color_name(color: i64) -> String {
    let i = usize::try_from(color).unwrap_or(0) % PEOPLE_COLORS.len();
    PEOPLE_COLORS[i].to_owned()
}

fn person(name: &str, color: i64) -> Person {
    Person {
        name: name.to_owned(),
        color: color_name(color),
    }
}

/// The folder name for a person's own browser, as oxbot made it before
/// people had ids: their name made safe for a folder, plus a short hash of
/// the exact name, so `Sam` and `sam` never share sign-ins. People from then
/// keep theirs (their sign-ins are in it); newer people's is their id.
pub(crate) fn person_folder(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "person" } else { safe };
    // FNV-1a: stable across runs and Rust versions, unlike `DefaultHasher`.
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!(
        "{}-{:08x}",
        safe.chars().take(32).collect::<String>(),
        hash >> 32
    )
}

const PERSON_COLUMNS: &str = "id, name, color, folder, herder, farm";

fn person_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PersonRecord> {
    let id: String = r.get(0)?;
    let name: String = r.get(1)?;
    let bad = |e: String| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    };
    let id = PersonId::new(id).map_err(|e| bad(e.to_string()))?;
    // Filled at open; the old derivations are what an older oxbot used.
    let folder: Option<String> = r.get(3)?;
    let herder: Option<String> = r.get(4)?;
    let herder = herder
        .and_then(|h| BotId::new(h).ok())
        .unwrap_or_else(|| personal_herder_id(&name));
    Ok(PersonRecord {
        folder: folder.unwrap_or_else(|| person_folder(&name)),
        herder,
        color: color_name(r.get(2)?),
        farm: r.get::<_, i64>(5)? != 0,
        id,
        name,
    })
}

const GATE_SELECT: &str =
    "SELECT g.id, COALESCE(p.name, ''), g.name, g.paired_ms, g.last_seen_ms, g.person_id
     FROM gates g LEFT JOIN people p ON p.id = g.person_id";

fn gate_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(GateInfo, String)> {
    Ok((
        GateInfo {
            id: row.get(0)?,
            person: row.get(1)?,
            name: row.get(2)?,
            paired_ms: row.get(3)?,
            last_seen_ms: row.get(4)?,
            online: false,
        },
        row.get(5)?,
    ))
}

/// The first color nobody has yet (or, once all are taken, the next in turn).
fn next_color(conn: &Connection) -> Result<i64, StoreError> {
    let mut stmt = conn.prepare("SELECT color FROM people")?;
    let taken: Vec<i64> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let n = i64::try_from(PEOPLE_COLORS.len()).unwrap_or(12);
    Ok((0..n)
        .find(|c| !taken.contains(c))
        .unwrap_or_else(|| i64::try_from(taken.len()).unwrap_or(0) % n))
}

fn find_person(conn: &Connection, by: &str, key: &str) -> Result<Option<PersonRecord>, StoreError> {
    Ok(conn
        .query_row(
            &format!("SELECT {PERSON_COLUMNS} FROM people WHERE {by} = ?1"),
            [key],
            person_row,
        )
        .optional()?)
}

/// The person called exactly `name`, made if there's none. The flag says
/// whether they're new. A new person's browser folder is their id, and
/// their Herder's id comes from their name unless someone has it already.
fn find_or_create(conn: &Connection, name: &str) -> Result<(PersonRecord, bool), StoreError> {
    if let Some(found) = find_person(conn, "name", name)? {
        return Ok((found, false));
    }
    let id = PersonId::random();
    let by_name = personal_herder_id(name);
    let taken: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM people WHERE herder = ?1)",
        [by_name.as_str()],
        |r| r.get(0),
    )?;
    let herder = if taken {
        personal_herder_id_for(name, &id)
    } else {
        by_name
    };
    conn.execute(
        "INSERT INTO people (id, name, color, first_ms, folder, herder) VALUES (?1, ?2, ?3, ?4, ?1, ?5)",
        params![id.as_str(), name, next_color(conn)?, now_ms(), herder.as_str()],
    )?;
    let made = find_person(conn, "id", id.as_str())?
        .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
    Ok((made, true))
}

/// Migration 9: everyone oxbot knew by name gets an id. Their browser
/// folder and Herder stay what they were (so no sign-in or chat is lost),
/// browser owners and Gates point at the id, and sites are named by their
/// main domain once (and checked against Chrome's cookies next start).
fn people_get_ids(tx: &Transaction<'_>) -> Result<(), StoreError> {
    // Everyone known by name, in the order oxbot met them: people who sent
    // messages, then browser owners and Gates' people.
    let mut known: Vec<(String, Option<i64>, i64)> = Vec::new();
    for sql in [
        "SELECT name, color, first_ms FROM people ORDER BY first_ms, rowid",
        "SELECT owner, NULL, MIN(signed_in_ms) FROM sites WHERE owner <> '' GROUP BY owner ORDER BY 3",
        "SELECT person, NULL, MIN(paired_ms) FROM gates GROUP BY person ORDER BY 3",
    ] {
        let mut stmt = tx.prepare(sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        known.extend(rows.collect::<Result<Vec<_>, _>>()?);
    }
    tx.execute_batch(
        "CREATE TABLE people_v9 (
            id       TEXT    PRIMARY KEY DEFAULT ('p_' || lower(hex(randomblob(8)))),
            name     TEXT    NOT NULL UNIQUE,
            color    INTEGER NOT NULL,
            first_ms INTEGER NOT NULL,
            folder   TEXT,
            herder   TEXT,
            farm     INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE gates_v9 (
            id           TEXT    PRIMARY KEY,
            person_id    TEXT    NOT NULL,
            name         TEXT    NOT NULL,
            key_hash     TEXT    NOT NULL UNIQUE,
            paired_ms    INTEGER NOT NULL,
            last_seen_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS site_checks (owner TEXT PRIMARY KEY);",
    )?;
    let ids = add_known_people(tx, &known)?;
    let key = |name: &str| ids.get(name).map_or("", PersonId::as_str).to_owned();

    // Sites: owners become ids, hosts become main domains.
    let mut stmt = tx.prepare("SELECT owner, site, signed_in_ms FROM sites")?;
    let sites: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    tx.execute("DELETE FROM sites", [])?;
    for (owner, site, when) in sites {
        let owner = if owner.is_empty() { owner } else { key(&owner) };
        tx.execute(
            "INSERT INTO sites (owner, site, signed_in_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(owner, site) DO UPDATE SET signed_in_ms = MAX(signed_in_ms, excluded.signed_in_ms)",
            params![owner, oxbot_browser::site_of(&site), when],
        )?;
    }
    // Names changed without Chrome being asked: check them on next start.
    tx.execute(
        "INSERT OR IGNORE INTO site_checks (owner) SELECT DISTINCT owner FROM sites",
        [],
    )?;

    let mut stmt =
        tx.prepare("SELECT id, person, name, key_hash, paired_ms, last_seen_ms FROM gates")?;
    let gates: Vec<(String, String, String, String, i64, i64)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (id, person, name, key_hash, paired, seen) in gates {
        tx.execute(
            "INSERT INTO gates_v9 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, key(&person), name, key_hash, paired, seen],
        )?;
    }
    tx.execute_batch(
        "DROP TABLE people;
        ALTER TABLE people_v9 RENAME TO people;
        DROP TABLE gates;
        ALTER TABLE gates_v9 RENAME TO gates;",
    )?;
    Ok(())
}

/// Adds everyone in `known` (name, color, first seen) to `people_v9` with a
/// new id, keeping their color, their old browser folder and their old
/// Herder id. Returns each name's id.
fn add_known_people(
    tx: &Transaction<'_>,
    known: &[(String, Option<i64>, i64)],
) -> Result<HashMap<String, PersonId>, StoreError> {
    let mut ids: HashMap<String, PersonId> = HashMap::new();
    let mut colors: Vec<i64> = known.iter().filter_map(|k| k.1).collect();
    let mut herders = std::collections::HashSet::new();
    let n = i64::try_from(PEOPLE_COLORS.len()).unwrap_or(12);
    for (name, color, first_ms) in known {
        if name.is_empty() || ids.contains_key(name) {
            continue;
        }
        let id = PersonId::random();
        let color = color.unwrap_or_else(|| {
            let c = (0..n)
                .find(|c| !colors.contains(c))
                .unwrap_or_else(|| i64::try_from(colors.len()).unwrap_or(0) % n);
            colors.push(c);
            c
        });
        // `Sam` and `sam` had the same Herder id: the first keeps it.
        let mut herder = personal_herder_id(name);
        if !herders.insert(herder.clone()) {
            herder = personal_herder_id_for(name, &id);
        }
        tx.execute(
            "INSERT INTO people_v9 (id, name, color, first_ms, folder, herder) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id.as_str(), name, color, first_ms, person_folder(name), herder.as_str()],
        )?;
        ids.insert(name.clone(), id);
    }
    Ok(ids)
}

fn insert_gate(
    conn: &Connection,
    id: &str,
    person: &PersonRecord,
    name: &str,
    key_hash: &str,
) -> Result<GateInfo, StoreError> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO gates (id, person_id, name, key_hash, paired_ms, last_seen_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id, person.id.as_str(), name, key_hash, now],
    )?;
    Ok(GateInfo {
        id: id.to_owned(),
        person: person.name.clone(),
        name: name.to_owned(),
        paired_ms: now,
        last_seen_ms: now,
        online: false,
    })
}

/// Gives people added without a folder or Herder (by an older oxbot, or a
/// test seed) the ones that older oxbot would have used.
fn fill_people(conn: &Connection) -> Result<(), StoreError> {
    let mut stmt =
        conn.prepare("SELECT id, name FROM people WHERE folder IS NULL OR herder IS NULL")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (id, name) in rows {
        conn.execute(
            "UPDATE people SET folder = COALESCE(folder, ?2), herder = COALESCE(herder, ?3) WHERE id = ?1",
            params![id, person_folder(&name), personal_herder_id(&name).as_str()],
        )?;
    }
    Ok(())
}

/// A stored event row as `(seq, ts_ms, body)`.
type Row = (i64, i64, String);

fn event_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
}

/// Reads a stored event back. An event this version can't read (say,
/// written by a newer oxbot) is `None`, so the rest of the chat still shows.
fn parse_event(bot: &BotId, (seq, ts_ms, body): Row) -> Option<BotEvent> {
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

fn to_sql(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn from_sql(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// Runs the schema changes not run yet, up to `upto`.
fn migrate(conn: &mut Connection, upto: usize) -> Result<(), StoreError> {
    let done: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let done = usize::try_from(done).unwrap_or(0);
    for (i, step) in MIGRATIONS.iter().enumerate().take(upto).skip(done) {
        // A migration and its version bump commit together, so a crash
        // mid-way (6 and 9 rebuild tables) leaves the database as it was.
        let tx = conn.transaction()?;
        match step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::Rust(run) => run(&tx)?,
        }
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
        if i + 1 == MIGRATIONS.len() && done > 0 {
            tracing::info!(from = done, to = i + 1, "upgraded the database");
        }
    }
    Ok(())
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

    /// Makes (or opens) the database at `path` with only the first
    /// `version` schema changes, as an older oxbot left it. For tests and
    /// test seeds written for that schema; [`Store::open`] then upgrades it.
    ///
    /// # Errors
    /// Fails if SQLite can't open the file or a change fails.
    pub fn open_at_version(path: &Path, version: usize) -> Result<(), StoreError> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        migrate(&mut conn, version)
    }

    fn setup(mut conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(SCHEMA)?;
        migrate(&mut conn, MIGRATIONS.len())?;
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

    /// The person called `name`, giving them the first color nobody has yet
    /// if they're new. The flag says whether they're new.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person(&self, name: &str) -> Result<(Person, bool), StoreError> {
        let (found, new) = self.find_or_create_person(name)?;
        Ok((found.person(), new))
    }

    /// The person called exactly `name`, made if there's none. The flag says
    /// whether they're new.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn find_or_create_person(&self, name: &str) -> Result<(PersonRecord, bool), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let found = find_or_create(&tx, name)?;
        tx.commit()?;
        Ok(found)
    }

    /// The person with this id, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_by_id(&self, id: &PersonId) -> Result<Option<PersonRecord>, StoreError> {
        find_person(&self.conn(), "id", id.as_str())
    }

    /// The person called exactly `name`, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_by_name(&self, name: &str) -> Result<Option<PersonRecord>, StoreError> {
        find_person(&self.conn(), "name", name)
    }

    /// Everyone oxbot knows, in the order it met them.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_records(&self) -> Result<Vec<PersonRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PERSON_COLUMNS} FROM people ORDER BY first_ms, rowid"
        ))?;
        let rows = stmt.query_map([], person_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The Farm's own person, once chosen.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn farm_person(&self) -> Result<Option<PersonRecord>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {PERSON_COLUMNS} FROM people WHERE farm = 1 ORDER BY rowid LIMIT 1"
                ),
                [],
                person_row,
            )
            .optional()?)
    }

    /// Makes `id` the Farm's own person (and nobody else).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_farm_person(&self, id: &PersonId) -> Result<(), StoreError> {
        self.conn()
            .execute("UPDATE people SET farm = (id = ?1)", [id.as_str()])?;
        Ok(())
    }

    /// Changes a person's name. Returns `false` (and changes nothing) if
    /// someone else has that name.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn rename_person(&self, id: &PersonId, name: &str) -> Result<bool, StoreError> {
        let conn = self.conn();
        if find_person(&conn, "name", name)?.is_some_and(|p| &p.id != id) {
            return Ok(false);
        }
        conn.execute(
            "UPDATE people SET name = ?2 WHERE id = ?1",
            params![id.as_str(), name],
        )?;
        Ok(true)
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

    /// Remembers that `owner`'s browser (as [`BrowserOwner::key`] names it)
    /// signed in to `site` (a main domain) now.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_site(&self, owner: impl AsRef<str>, site: &str) -> Result<(), StoreError> {
        self.add_site_at(owner, site, now_ms())
    }

    /// Remembers a sign-in to `site` at `when_ms`; a later one wins.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_site_at(
        &self,
        owner: impl AsRef<str>,
        site: &str,
        when_ms: i64,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO sites (owner, site, signed_in_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(owner, site) DO UPDATE SET signed_in_ms = MAX(signed_in_ms, excluded.signed_in_ms)",
            params![owner.as_ref(), site, when_ms],
        )?;
        Ok(())
    }

    /// Forgets `site` for `owner`'s browser. Returns whether it was there.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn remove_site(&self, owner: impl AsRef<str>, site: &str) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "DELETE FROM sites WHERE owner = ?1 AND site = ?2",
            params![owner.as_ref(), site],
        )? > 0)
    }

    /// Forgets every site for `owner`'s browser, in one go. Returns how
    /// many there were.
    ///
    /// # Errors
    /// Fails if SQLite fails (then none are forgotten).
    pub fn clear_sites(&self, owner: impl AsRef<str>) -> Result<usize, StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM sites WHERE owner = ?1", [owner.as_ref()])?)
    }

    /// Forgets `owner`'s sites that aren't in `held` (the sites its browser
    /// still has cookies for), in one transaction. Returns the ones
    /// forgotten.
    ///
    /// # Errors
    /// Fails if SQLite fails (then none are forgotten).
    pub fn keep_only_sites(
        &self,
        owner: impl AsRef<str>,
        held: &[String],
    ) -> Result<Vec<String>, StoreError> {
        let owner = owner.as_ref();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let known: Vec<String> = {
            let mut stmt = tx.prepare("SELECT site FROM sites WHERE owner = ?1")?;
            let rows = stmt.query_map([owner], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let gone: Vec<String> = known.into_iter().filter(|s| !held.contains(s)).collect();
        for site in &gone {
            tx.execute(
                "DELETE FROM sites WHERE owner = ?1 AND site = ?2",
                params![owner, site],
            )?;
        }
        tx.commit()?;
        Ok(gone)
    }

    /// The sites `owner`'s browser signed in to, newest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn sites(&self, owner: impl AsRef<str>) -> Result<Vec<SiteInfo>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT site, signed_in_ms FROM sites WHERE owner = ?1
             ORDER BY signed_in_ms DESC, site",
        )?;
        let rows = stmt.query_map([owner.as_ref()], |r| {
            Ok(SiteInfo {
                site: r.get(0)?,
                signed_in_ms: r.get(1)?,
                icon: None,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every browser owner with remembered sites (home's is empty).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn site_owners(&self) -> Result<Vec<String>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT DISTINCT owner FROM sites ORDER BY owner")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Whether `owner`'s sites should be checked against its cookies (they
    /// were renamed by an upgrade and Chrome wasn't asked yet).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn site_check_due(&self, owner: impl AsRef<str>) -> Result<bool, StoreError> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM site_checks WHERE owner = ?1)",
            [owner.as_ref()],
            |r| r.get(0),
        )?)
    }

    /// `owner`'s sites were checked against its cookies.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn site_check_done(&self, owner: impl AsRef<str>) -> Result<(), StoreError> {
        self.conn()
            .execute("DELETE FROM site_checks WHERE owner = ?1", [owner.as_ref()])?;
        Ok(())
    }

    /// Remembers a newly paired Gate for the person called exactly `person`
    /// (made if new). `key_hash` is the hash of its key.
    ///
    /// # Errors
    /// Fails if the database does (a key already used, say).
    pub fn add_gate(
        &self,
        id: &str,
        person: &str,
        name: &str,
        key_hash: &str,
    ) -> Result<GateInfo, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (found, _) = find_or_create(&tx, person)?;
        let gate = insert_gate(&tx, id, &found, name, key_hash)?;
        tx.commit()?;
        Ok(gate)
    }

    /// Remembers a newly paired Gate for the person `person`.
    ///
    /// # Errors
    /// Fails if there's no such person or the database fails.
    pub fn add_gate_for(
        &self,
        id: &str,
        person: &PersonId,
        name: &str,
        key_hash: &str,
    ) -> Result<GateInfo, StoreError> {
        let conn = self.conn();
        let found = find_person(&conn, "id", person.as_str())?
            .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
        insert_gate(&conn, id, &found, name, key_hash)
    }

    /// The Gate holding the key with this hash, if it's still paired; marks
    /// it as seen now.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn gate_by_key(&self, key_hash: &str) -> Result<Option<GateInfo>, StoreError> {
        Ok(self.gate_and_person_by_key(key_hash)?.map(|(gate, _)| gate))
    }

    /// Like [`Store::gate_by_key`], with the id of the Gate's person.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn gate_and_person_by_key(
        &self,
        key_hash: &str,
    ) -> Result<Option<(GateInfo, PersonId)>, StoreError> {
        let conn = self.conn();
        let now = now_ms();
        conn.execute(
            "UPDATE gates SET last_seen_ms = ?2 WHERE key_hash = ?1",
            params![key_hash, now],
        )?;
        let found = conn
            .query_row(
                &format!("{GATE_SELECT} WHERE g.key_hash = ?1"),
                params![key_hash],
                gate_row,
            )
            .optional()?;
        Ok(found.and_then(|(gate, person)| Some((gate, PersonId::new(person).ok()?))))
    }

    /// Every paired Gate, oldest first.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn gates(&self) -> Result<Vec<GateInfo>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{GATE_SELECT} ORDER BY g.paired_ms, g.id"))?;
        let gates = stmt
            .query_map([], gate_row)?
            .map(|r| r.map(|(gate, _)| gate))
            .collect::<Result<_, _>>()?;
        Ok(gates)
    }

    /// Forgets a Gate; its key stops working. Returns whether it was paired.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn remove_gate(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM gates WHERE id = ?1", params![id])?
            > 0)
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
    /// Fails if SQLite fails.
    pub fn history(&self, bot: &BotId, since: u64) -> Result<Vec<BotEvent>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, ts_ms, body FROM events WHERE bot = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let rows = stmt
            .query_map(params![bot.as_str(), to_sql(since)], event_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|row| parse_event(bot, row))
            .collect())
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
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT seq, ts_ms, body FROM events WHERE bot = ?1 AND seq < ?2
             ORDER BY seq DESC LIMIT ?3",
        )?;
        // One more than asked, to know whether there are older ones.
        let want = i64::try_from(limit).unwrap_or(i64::MAX).saturating_add(1);
        let before = before.map_or(i64::MAX, to_sql);
        let mut rows = stmt
            .query_map(params![bot.as_str(), before, want], event_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let has_older = rows.len() > limit;
        rows.truncate(limit);
        rows.reverse();
        let first_seq = rows.first().map_or(0, |r| from_sql(r.0));
        let last_seq = rows.last().map_or(0, |r| from_sql(r.0));
        Ok(HistoryPage {
            events: rows
                .into_iter()
                .filter_map(|row| parse_event(bot, row))
                .collect(),
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

    /// Every bot's latest event and reply numbers, keyed by bot id. Two
    /// indexed queries, whatever the size of the history.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn all_seqs(&self) -> Result<HashMap<String, Seqs>, StoreError> {
        let conn = self.conn();
        let mut out: HashMap<String, Seqs> = HashMap::new();
        let mut stmt = conn.prepare("SELECT bot, MAX(seq) FROM events GROUP BY bot")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (bot, seq) = row?;
            out.entry(bot).or_default().last = from_sql(seq);
        }
        // The same condition as the events_replies index, so it's used.
        let mut stmt = conn.prepare(
            "SELECT bot, MAX(seq) FROM events
             WHERE json_extract(body, '$.kind') = 'assistant_message' GROUP BY bot",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (bot, seq) = row?;
            out.entry(bot).or_default().last_reply = from_sql(seq);
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
    fn sites_are_cleared_or_trimmed_per_browser() {
        let store = Store::open_in_memory().unwrap();
        for site in ["a.example", "b.example", "c.example"] {
            store.add_site(HOME, site).unwrap();
        }
        store.add_site("p_1", "a.example").unwrap();
        let gone = store
            .keep_only_sites(HOME, &["b.example".to_owned()])
            .unwrap();
        assert_eq!(gone, ["a.example", "c.example"]);
        let left: Vec<_> = store
            .sites(HOME)
            .unwrap()
            .into_iter()
            .map(|s| s.site)
            .collect();
        assert_eq!(left, ["b.example"]);
        assert_eq!(store.clear_sites(HOME).unwrap(), 1);
        assert_eq!(store.sites(HOME).unwrap().len(), 0);
        assert_eq!(
            store.sites("p_1").unwrap().len(),
            1,
            "other browsers keep theirs"
        );
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
            conn.execute_batch("DELETE FROM sites; DROP TABLE gates; PRAGMA user_version = 4;")
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
    fn a_removed_gate_s_key_stops_working() {
        let store = Store::open_in_memory().unwrap();
        store.add_gate("g1", "Jay", "Pixel", "hash-1").unwrap();
        store.add_gate("g2", "Sam", "Laptop", "hash-2").unwrap();
        assert_eq!(store.gate_by_key("hash-1").unwrap().unwrap().person, "Jay");
        assert!(store.gate_by_key("nope").unwrap().is_none());
        assert_eq!(store.gates().unwrap().len(), 2);
        assert!(store.remove_gate("g1").unwrap());
        assert!(store.gate_by_key("hash-1").unwrap().is_none());
        assert!(!store.remove_gate("g1").unwrap());
        assert!(
            store.add_gate("g3", "Ann", "Phone", "hash-2").is_err(),
            "keys are unique"
        );
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
    fn an_event_this_version_cant_read_doesnt_hide_the_chat() {
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
            .conn()
            .execute(
                "INSERT INTO events (bot, seq, ts_ms, body) VALUES ('a', 2, 0, '{\"kind\":\"from_the_future\"}')",
                [],
            )
            .unwrap();
        store
            .append(
                &a,
                EventKind::AssistantMessage {
                    text: "hello".into(),
                },
            )
            .unwrap();
        let seqs: Vec<u64> = store
            .history(&a, 0)
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(seqs, [1, 3], "the unreadable one is skipped, the rest show");
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

    /// `n` events for `bot`: every tenth a reply, the rest tool steps.
    fn seed(store: &Store, bot: &BotId, n: u64) {
        for i in 1..=n {
            let kind = if i % 10 == 0 {
                EventKind::AssistantMessage {
                    text: format!("reply {i}"),
                }
            } else {
                EventKind::ToolUse {
                    name: "Read".into(),
                    summary: format!("step {i}"),
                }
            };
            store.append(bot, kind).unwrap();
        }
    }

    #[test]
    fn history_pages_back_to_the_start() {
        let store = Store::open_in_memory().unwrap();
        let a = bot("a");
        seed(&store, &a, 25);
        seed(&store, &bot("b"), 3);
        let latest = store.history_page(&a, None, 10).unwrap();
        let seqs = |p: &HistoryPage| p.events.iter().map(|e| e.seq).collect::<Vec<_>>();
        assert_eq!(seqs(&latest), (16..=25).collect::<Vec<_>>());
        assert_eq!((latest.first_seq, latest.last_seq), (16, 25));
        assert!(latest.has_older);
        let middle = store.history_page(&a, Some(16), 10).unwrap();
        assert_eq!(seqs(&middle), (6..=15).collect::<Vec<_>>());
        assert!(middle.has_older);
        let first = store.history_page(&a, Some(6), 10).unwrap();
        assert_eq!(seqs(&first), (1..=5).collect::<Vec<_>>());
        assert!(!first.has_older);
        // Exactly a page left: nothing older.
        assert!(!store.history_page(&a, Some(11), 10).unwrap().has_older);
        let empty = store.history_page(&bot("c"), None, 10).unwrap();
        assert!(empty.events.is_empty() && !empty.has_older && empty.last_seq == 0);
    }

    #[test]
    fn every_bots_latest_numbers_come_from_the_index() {
        let store = Store::open_in_memory().unwrap();
        seed(&store, &bot("a"), 25);
        seed(&store, &bot("b"), 3);
        let all = store.all_seqs().unwrap();
        assert_eq!(
            all["a"],
            Seqs {
                last: 25,
                last_reply: 20
            }
        );
        assert_eq!(
            all["b"],
            Seqs {
                last: 3,
                last_reply: 0
            }
        );
        assert_eq!(store.event(&bot("a"), 20).unwrap().unwrap().seq, 20);
        assert!(store.event(&bot("a"), 26).unwrap().is_none());
        let plan: String = store
            .conn()
            .query_row(
                "EXPLAIN QUERY PLAN SELECT bot, MAX(seq) FROM events
                 WHERE json_extract(body, '$.kind') = 'assistant_message' GROUP BY bot",
                [],
                |r| r.get(3),
            )
            .unwrap();
        assert!(plan.contains("events_replies"), "{plan}");
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
    fn a_sessions_taint_lasts_until_a_new_session() {
        let store = Store::open_in_memory().unwrap();
        let saved = |id: &str| SavedSession {
            id: id.into(),
            prompt: "p".into(),
        };
        store.set_session_taint(&bot("a"), Some("x")).unwrap();
        assert_eq!(store.session_taint(&bot("a")).unwrap(), None, "no session");
        store.set_session(&bot("a"), Some(&saved("s1"))).unwrap();
        assert_eq!(store.session_taint(&bot("a")).unwrap(), None);
        store
            .set_session_taint(&bot("a"), Some("a web page"))
            .unwrap();
        // Resuming saves the same session again: the taint stays.
        store.set_session(&bot("a"), Some(&saved("s1"))).unwrap();
        assert_eq!(
            store.session_taint(&bot("a")).unwrap().as_deref(),
            Some("a web page")
        );
        // A new session starts clean.
        store.set_session(&bot("a"), Some(&saved("s2"))).unwrap();
        assert_eq!(store.session_taint(&bot("a")).unwrap(), None);
        store.set_session_taint(&bot("a"), Some("y")).unwrap();
        store.set_session(&bot("a"), None).unwrap();
        store.set_session(&bot("a"), Some(&saved("s2"))).unwrap();
        assert_eq!(store.session_taint(&bot("a")).unwrap(), None, "forgotten");
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

    /// A database exactly as oxbot left it before people had ids (version
    /// 8, made by the real migrations), with people, browser owners and
    /// Gates known only by name.
    fn version_8_database(path: &Path) {
        Store::open_at_version(path, 8).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "INSERT INTO people (name, color, first_ms) VALUES ('Jay', 3, 100), ('Sam', 1, 200), ('sam', 2, 300);
             INSERT INTO sites (owner, site, signed_in_ms) VALUES
               ('', 'accounts.google.com', 1010), ('', 'google.com', 1005), ('', 'wikipedia.org', 1007),
               ('Sam', 'bank.example', 1020), ('Ann', 'mail.example.com', 1030);
             INSERT INTO gates (id, person, name, key_hash, paired_ms, last_seen_ms) VALUES
               ('g1', 'Ann', 'Pixel', 'hash-1', 1040, 1040), ('g2', 'Lee', 'Laptop', 'hash-2', 1050, 1050);",
        )
        .unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 8);
    }

    #[test]
    fn people_known_by_name_get_ids_and_keep_their_folders_and_herders() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v8.db");
        version_8_database(&path);

        let store = Store::open(&path).unwrap();
        let people = store.person_records().unwrap();
        let names: Vec<&str> = people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["Jay", "Sam", "sam", "Ann", "Lee"],
            "site owners and Gates' people too"
        );
        for p in &people {
            assert_eq!(PersonId::new(p.id.as_str()).as_ref(), Ok(&p.id));
            // Their Chrome profile stays where it was: no sign-in is lost.
            assert_eq!(p.folder, person_folder(&p.name), "{}", p.name);
            assert!(!p.farm, "chosen later, by the hub");
        }
        let person = |name: &str| people.iter().find(|p| p.name == name).unwrap().clone();
        assert_eq!(person("Jay").color, PEOPLE_COLORS[3], "colors are kept");
        assert_eq!(person("Sam").color, PEOPLE_COLORS[1]);
        assert_eq!(
            person("Ann").color,
            PEOPLE_COLORS[0],
            "new ones get a free color"
        );
        // Their Herders keep their ids; `sam` no longer shares Sam's.
        for name in ["Jay", "Sam", "Ann", "Lee"] {
            assert_eq!(person(name).herder, personal_herder_id(name), "{name}");
        }
        assert_ne!(person("sam").herder, person("Sam").herder);
        assert!(crate::bot::is_personal_herder(
            person("sam").herder.as_str()
        ));

        // Browser owners are ids now, and hosts are main domains.
        let sites = |owner: &str| -> Vec<(String, i64)> {
            store
                .sites(owner)
                .unwrap()
                .into_iter()
                .map(|s| (s.site, s.signed_in_ms))
                .collect()
        };
        assert_eq!(
            sites(""),
            [
                ("google.com".to_owned(), 1010),
                ("wikipedia.org".to_owned(), 1007)
            ]
        );
        assert_eq!(
            sites(person("Sam").id.as_str()),
            [("bank.example".to_owned(), 1020)]
        );
        assert_eq!(
            sites(person("Ann").id.as_str()),
            [("example.com".to_owned(), 1030)]
        );
        assert!(sites("Sam").is_empty(), "never by name again");
        // Checked against Chrome's cookies on the next start.
        assert!(store.site_check_due(HOME).unwrap());
        assert!(store.site_check_due(person("Sam").id.as_str()).unwrap());
        store.site_check_done(HOME).unwrap();
        assert!(!store.site_check_due(HOME).unwrap());

        // Gates point at their person.
        let (gate, id) = store.gate_and_person_by_key("hash-1").unwrap().unwrap();
        assert_eq!((gate.person.as_str(), id), ("Ann", person("Ann").id));
        let (gate, id) = store.gate_and_person_by_key("hash-2").unwrap().unwrap();
        assert_eq!((gate.person.as_str(), id), ("Lee", person("Lee").id));

        // Opening it again changes nothing.
        drop(store);
        let again = Store::open(&path).unwrap();
        assert_eq!(again.person_records().unwrap(), people);
        assert_eq!(
            sites_of(&again, person("Sam").id.as_str()),
            ["bank.example"]
        );
        let version: i64 = again
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(usize::try_from(version).unwrap(), MIGRATIONS.len());
    }

    fn sites_of(store: &Store, owner: &str) -> Vec<String> {
        store
            .sites(owner)
            .unwrap()
            .into_iter()
            .map(|s| s.site)
            .collect()
    }

    #[test]
    fn people_added_by_an_older_oxbot_get_the_folder_it_would_have_used() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO people (name, color, first_ms) VALUES ('Sam', 0, 1)",
                [],
            )
            .unwrap();
        fill_people(&store.conn()).unwrap();
        let sam = store.person_by_name("Sam").unwrap().unwrap();
        assert_eq!(sam.folder, person_folder("Sam"));
        assert_eq!(sam.herder, personal_herder_id("Sam"));
    }

    #[test]
    fn new_people_get_their_id_as_folder_and_never_share_a_herder() {
        let store = Store::open_in_memory().unwrap();
        let (sam, new) = store.find_or_create_person("Sam").unwrap();
        assert!(new);
        assert_eq!(sam.folder, sam.id.as_str());
        assert_eq!(sam.herder, personal_herder_id("Sam"));
        let (other, _) = store.find_or_create_person("sam").unwrap();
        assert_ne!(other.id, sam.id, "names are told apart exactly");
        assert_ne!(other.herder, sam.herder);
        assert_eq!(
            store.find_or_create_person("Sam").unwrap(),
            (sam.clone(), false)
        );

        assert!(!store.rename_person(&other.id, "Sam").unwrap(), "taken");
        assert!(store.rename_person(&sam.id, "Samuel").unwrap());
        let renamed = store.person_by_id(&sam.id).unwrap().unwrap();
        assert_eq!(
            (
                renamed.name.as_str(),
                renamed.folder.as_str(),
                &renamed.herder
            ),
            ("Samuel", sam.folder.as_str(), &sam.herder)
        );
        store.set_farm_person(&other.id).unwrap();
        assert_eq!(store.farm_person().unwrap().unwrap().id, other.id);
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
