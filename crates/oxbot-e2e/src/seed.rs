//! A throwaway oxbot home made from `e2e/seed/`: its bots, and a fresh
//! `SQLite` database with oxbot's tables plus `seed.sql`.

use std::path::Path;

use anyhow::Context;

/// Fills `home` from the seed: copies `seed/bots/`, creates `oxbot.db`
/// with oxbot's own tables, then runs `seed/seed.sql` (where `NOW_MS` is
/// the current time in milliseconds).
///
/// # Errors
/// Fails if a file can't be copied or the SQL fails.
pub fn make_home(repo: &Path, home: &Path) -> anyhow::Result<()> {
    let seed = repo.join("e2e/seed");
    copy_dir(&seed.join("bots"), &home.join("bots"))?;
    let db = home.join("oxbot.db");
    // oxbot's own migrations make the tables, exactly as the app would.
    drop(oxbot_core::Store::open(&db).context("creating the database")?);
    let sql_path = seed.join("seed.sql");
    if sql_path.exists() {
        let sql =
            std::fs::read_to_string(&sql_path)?.replace("NOW_MS", &crate::now_ms().to_string());
        let conn = rusqlite::Connection::open(&db)?;
        conn.execute_batch(&sql).context("running seed.sql")?;
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
