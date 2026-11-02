//! The schema and its changes, in order.
//!
//! Migrations only go forward: once one has shipped it never changes, and an
//! older oxbot refuses a database a newer one has upgraded (see
//! [`StoreError::Newer`]). Going back means restoring the copy taken before
//! a step that rebuilds tables (`oxbot.db.v<N>.bak`). A step written in Rust
//! uses frozen copies of whatever it computes (the `v9_*` functions), so
//! later changes to the live code can't change what an old upgrade does.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, Transaction, params};

use super::{PEOPLE_COLORS, StoreError};
use crate::id::PersonId;

pub(super) const SCHEMA: &str = "
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
pub(super) enum Step {
    Sql(&'static str),
    Rust(fn(&Transaction<'_>) -> Result<(), StoreError>),
    /// Rust that rebuilds tables: the database file is copied first.
    Rebuild(fn(&Transaction<'_>) -> Result<(), StoreError>),
}

/// Schema changes after the first release, in order. `PRAGMA user_version`
/// records how many have run.
pub(super) const MIGRATIONS: &[Step] = &[
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
    Step::Rebuild(people_get_ids),
    // 10: whether a Claude session has read text oxbot doesn't trust (and
    // what), kept with the session because Claude remembers it on resume.
    Step::Rust(sessions_remember_taint),
    // 11: a browser folder and a Herder belong to one person, and there's
    // one Farm person at most. A folder or Herder held twice (only possible
    // by hand or by an older oxbot) is cleared from the later person, who
    // gets a fresh one when the store opens.
    Step::Sql(
        "UPDATE people SET farm = 0 WHERE farm <> 0
           AND rowid <> (SELECT MIN(rowid) FROM people WHERE farm <> 0);
        UPDATE people SET farm = 1 WHERE farm <> 0;
        UPDATE people SET folder = NULL WHERE folder IS NOT NULL
           AND rowid <> (SELECT MIN(p.rowid) FROM people p WHERE p.folder = people.folder);
        UPDATE people SET herder = NULL WHERE herder IS NOT NULL
           AND rowid <> (SELECT MIN(p.rowid) FROM people p WHERE p.herder = people.herder);
        CREATE UNIQUE INDEX IF NOT EXISTS people_folder ON people (folder);
        CREATE UNIQUE INDEX IF NOT EXISTS people_herder ON people (herder);
        CREATE UNIQUE INDEX IF NOT EXISTS people_one_farm ON people (farm) WHERE farm = 1;",
    ),
    // 12: "Always allow" rules: a bot may do one action on one element
    // label on one exact host without asking. `by_person` is who set it
    // (NULL: the Farm with no person).
    Step::Sql(
        "CREATE TABLE IF NOT EXISTS allow_rules (
            id         INTEGER PRIMARY KEY,
            bot        TEXT    NOT NULL,
            site       TEXT    NOT NULL,
            action     TEXT    NOT NULL,
            label      TEXT    NOT NULL,
            by_person  TEXT,
            created_ms INTEGER NOT NULL,
            UNIQUE (bot, site, action, label)
        );
        CREATE INDEX IF NOT EXISTS allow_rules_bot ON allow_rules (bot);",
    ),
];

/// Migration 10. Skipped if the column is already there (a database
/// rewound to an older version for a test keeps its columns).
pub(super) fn sessions_remember_taint(tx: &Transaction<'_>) -> Result<(), StoreError> {
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

/// Migration 9: everyone oxbot knew by name gets an id. Their browser
/// folder and Herder stay what they were (so no sign-in or chat is lost),
/// browser owners and Gates point at the id, and sites are named by their
/// main domain once (and checked against Chrome's cookies next start).
pub(super) fn people_get_ids(tx: &Transaction<'_>) -> Result<(), StoreError> {
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
            farm     INTEGER NOT NULL DEFAULT 0 CHECK (farm IN (0, 1))
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
            params![owner, v9_site_of(&site), when],
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
pub(super) fn add_known_people(
    tx: &Transaction<'_>,
    known: &[(String, Option<i64>, i64)],
) -> Result<HashMap<String, PersonId>, StoreError> {
    let mut ids: HashMap<String, PersonId> = HashMap::new();
    let mut colors: Vec<i64> = known.iter().filter_map(|k| k.1).collect();
    let mut herders = HashSet::new();
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
        let mut herder = v9_herder_id(name);
        if !herders.insert(herder.clone()) {
            herder = v9_herder_id_for(name, &id);
        }
        tx.execute(
            "INSERT INTO people_v9 (id, name, color, first_ms, folder, herder) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id.as_str(), name, color, first_ms, v9_person_folder(name), herder],
        )?;
        ids.insert(name.clone(), id);
    }
    Ok(ids)
}

/// FNV-1a over `bytes`, as oxbot used it when migration 9 shipped.
fn v9_fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The browser folder oxbot made for `name` before people had ids. Frozen
/// copy of `person_folder` as migration 9 shipped.
pub(super) fn v9_person_folder(name: &str) -> String {
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
    format!(
        "{}-{:08x}",
        safe.chars().take(32).collect::<String>(),
        v9_fnv(name.as_bytes()) >> 32
    )
}

/// The letters and digits of `name` a Herder id starts with (at most 20).
fn v9_slug(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(20)
        .collect()
}

/// `assistant--<slug>-<tail>`, or `assistant--<tail>` without a slug.
fn v9_herder(slug: &str, tail: &str) -> String {
    if slug.is_empty() {
        format!("assistant--{tail}")
    } else {
        format!("assistant--{slug}-{tail}")
    }
}

/// A person's Herder id from their name. Frozen copy of
/// `personal_herder_id` as migration 9 shipped.
pub(super) fn v9_herder_id(name: &str) -> String {
    let hash = v9_fnv(name.trim().to_lowercase().as_bytes());
    v9_herder(&v9_slug(name), &format!("{:06x}", hash & 0xff_ffff))
}

/// A person's Herder id from their id, when the one from their name was
/// taken. Frozen copy of `personal_herder_id_for` as migration 9 shipped.
pub(super) fn v9_herder_id_for(name: &str, id: &PersonId) -> String {
    let hex = id.as_str().trim_start_matches("p_");
    v9_herder(&v9_slug(name), hex.get(..8).unwrap_or(hex))
}

/// A host's main domain, the way migration 9 named sites. Frozen copy of
/// `oxbot_browser::site_of` (the public suffix list itself comes from the
/// `psl` crate; the golden test notices if an update changes an answer).
pub(super) fn v9_site_of(host: &str) -> String {
    let host = host.trim().trim_start_matches('.').to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host;
    }
    psl::domain_str(&host).map_or(host.clone(), str::to_owned)
}

/// Runs the schema changes not run yet, up to `upto`. A database a newer
/// oxbot has upgraded is refused; with `can_back_up` (a file that was
/// there before), it's copied to `<file>.v<version>.bak` before a step that
/// rebuilds tables.
pub(super) fn migrate(
    conn: &mut Connection,
    upto: usize,
    can_back_up: bool,
) -> Result<(), StoreError> {
    let done: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let done = usize::try_from(done).unwrap_or(0);
    if done > MIGRATIONS.len() {
        return Err(StoreError::Newer {
            found: done,
            known: MIGRATIONS.len(),
        });
    }
    let mut pending = MIGRATIONS.iter().take(upto).skip(done);
    if can_back_up && pending.any(|step| matches!(step, Step::Rebuild(_))) {
        back_up(conn, done)?;
    }
    for (i, step) in MIGRATIONS.iter().enumerate().take(upto).skip(done) {
        // A migration and its version bump commit together, so a crash
        // mid-way (6 and 9 rebuild tables) leaves the database as it was.
        let tx = conn.transaction()?;
        match step {
            Step::Sql(sql) => tx.execute_batch(sql)?,
            Step::Rust(run) | Step::Rebuild(run) => run(&tx)?,
        }
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
        if i + 1 == MIGRATIONS.len() && done > 0 {
            tracing::info!(from = done, to = i + 1, "upgraded the database");
        }
    }
    Ok(())
}

/// Where the copy of `db` taken at schema `version` goes.
pub(super) fn backup_path(db: &Path, version: usize) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(format!(".v{version}.bak"));
    PathBuf::from(name)
}

/// Copies a file database, as it is at schema `version`, next to itself
/// before an upgrade rebuilds tables. `VACUUM INTO` writes one consistent
/// file, WAL included. An in-memory database has nothing to keep, and a
/// copy already there (from an upgrade that failed) is kept as it is.
fn back_up(conn: &Connection, version: usize) -> Result<(), StoreError> {
    let Some(db) = conn.path().filter(|p| !p.is_empty()) else {
        return Ok(());
    };
    let to = backup_path(Path::new(db), version);
    if to.exists() {
        tracing::info!(copy = %to.display(), "a copy from before this upgrade is already there");
        return Ok(());
    }
    let failed = |why: String| StoreError::Backup {
        to: to.clone(),
        why,
    };
    let target = to
        .to_str()
        .ok_or_else(|| failed("the path isn't valid UTF-8".to_owned()))?;
    conn.execute("VACUUM INTO ?1", [target])
        .map_err(|e| failed(e.to_string()))?;
    tracing::info!(copy = %to.display(), "copied the database before upgrading it");
    Ok(())
}
