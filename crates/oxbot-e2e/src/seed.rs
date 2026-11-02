//! A throwaway oxbot home made from `e2e/seed/`: its bots, and a fresh
//! `SQLite` database with oxbot's tables plus `seed.sql`.

use std::path::Path;

use anyhow::Context;

/// The schema version `seed.sql` is written for (people and browser
/// owners by name). The seed is put in a database of that version, then
/// oxbot upgrades it, as it would a real user's.
const SEED_SCHEMA: usize = 8;

/// Fills `home` from the seed: copies `seed/bots/`, creates `oxbot.db`
/// with oxbot's own tables as of [`SEED_SCHEMA`], runs `seed/seed.sql`
/// (where `NOW_MS` is the current time in milliseconds), upgrades it, then
/// runs `seed/seed-latest.sql` (rows for newer tables).
///
/// # Errors
/// Fails if a file can't be copied or the SQL fails.
pub fn make_home(repo: &Path, home: &Path) -> anyhow::Result<()> {
    let seed = repo.join("e2e/seed");
    copy_dir(&seed.join("bots"), &home.join("bots"))?;
    let db = home.join("oxbot.db");
    // oxbot's own migrations make the tables, exactly as the app would.
    oxbot_core::Store::open_at_version(&db, SEED_SCHEMA).context("creating the database")?;
    let sql_path = seed.join("seed.sql");
    if sql_path.exists() {
        let sql =
            std::fs::read_to_string(&sql_path)?.replace("NOW_MS", &crate::now_ms().to_string());
        let conn = rusqlite::Connection::open(&db)?;
        conn.execute_batch(&sql).context("running seed.sql")?;
    }
    drop(oxbot_core::Store::open(&db).context("upgrading the database")?);
    // Rows for tables newer than the seed's schema, once upgraded.
    let latest = seed.join("seed-latest.sql");
    if latest.exists() {
        let sql = std::fs::read_to_string(&latest)?.replace("NOW_MS", &crate::now_ms().to_string());
        let conn = rusqlite::Connection::open(&db)?;
        conn.execute_batch(&sql)
            .context("running seed-latest.sql")?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
