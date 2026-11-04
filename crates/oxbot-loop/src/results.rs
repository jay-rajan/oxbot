//! Stored tool results: whole page snapshots and big results go to
//! `<context_dir>/<session>/r<N>.txt` with an index row, and the model
//! gets a short head with the handle (`r12`) to read more on demand.
//! Tables and lists read off a page (`extract_rows`) are stored the same
//! way as `d<N>.txt` (JSON), numbered on their own.
//!
//! The folder is outside `work/`, so the bot's file tools can't change a
//! stored result. Handles are looked up through the index (session,
//! handle): a path is never built from what a model wrote.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use crate::store::{LoopStore, ResultKind, ResultRow};

/// A session folder goes this many days after its last turn.
pub const KEEP_DAYS: i64 = 14;
/// Most bytes of stored results per bot; the oldest sessions go first.
pub const MAX_BOT_BYTES: u64 = 200 * 1024 * 1024;

/// Whether `handle` looks like one oxbot made: `r` (a result) or `d` (a
/// dataset) and 1 to 6 digits.
#[must_use]
pub fn valid_handle(handle: &str) -> bool {
    handle
        .strip_prefix(['r', 'd'])
        .is_some_and(|n| (1..=6).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `handle` names a dataset (`d12`).
#[must_use]
pub fn is_dataset(handle: &str) -> bool {
    handle.starts_with('d') && valid_handle(handle)
}

/// What to store with a result.
#[derive(Debug, Clone)]
pub struct NewResult<'a> {
    /// The turn.
    pub turn: u32,
    /// The step.
    pub step: u32,
    /// The tool.
    pub tool: &'a str,
    /// Page or text.
    pub kind: ResultKind,
    /// The page's address, for pages.
    pub page_url: Option<String>,
    /// The page's title, for pages.
    pub page_title: Option<String>,
    /// Why it can't be trusted, if so.
    pub tainted_by: Option<String>,
    /// The whole text.
    pub text: &'a str,
}

/// One session's stored results.
pub struct ResultStore {
    store: Arc<dyn LoopStore>,
    bot: String,
    session: String,
    dir: PathBuf,
    next: AtomicU32,
    /// The next dataset number.
    next_data: AtomicU32,
    /// Serializes writes (the counter and the file).
    write: Mutex<()>,
}

impl std::fmt::Debug for ResultStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResultStore")
            .field("session", &self.session)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl ResultStore {
    /// The results of `session` of `bot`, under `context_dir/<session>/`.
    #[must_use]
    pub fn new(store: Arc<dyn LoopStore>, bot: &str, session: &str, context_dir: &Path) -> Self {
        let last = |prefix| {
            store
                .last_result_number(session, prefix)
                .unwrap_or_else(|err| {
                    tracing::warn!(%err, "couldn't read the stored results' numbers");
                    0
                })
        };
        let (results, datasets) = (last('r'), last('d'));
        Self {
            dir: context_dir.join(folder_name(session)),
            store,
            bot: bot.to_owned(),
            session: session.to_owned(),
            next: AtomicU32::new(results + 1),
            next_data: AtomicU32::new(datasets + 1),
            write: Mutex::new(()),
        }
    }

    /// The session's folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stores `new` and returns its row (with the handle: `d<N>` for a
    /// dataset, else `r<N>`).
    ///
    /// # Errors
    /// The disk or the database failed.
    pub fn put(&self, new: &NewResult<'_>) -> Result<ResultRow, String> {
        let _guard = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        let handle = if new.kind == ResultKind::Rows {
            format!("d{}", self.next_data.fetch_add(1, Ordering::Relaxed))
        } else {
            format!("r{}", self.next.fetch_add(1, Ordering::Relaxed))
        };
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        std::fs::write(self.dir.join(format!("{handle}.txt")), new.text)
            .map_err(|e| e.to_string())?;
        let row = ResultRow {
            session: self.session.clone(),
            handle,
            bot: self.bot.clone(),
            turn: new.turn,
            step: new.step,
            tool: new.tool.to_owned(),
            kind: new.kind,
            page_url: new.page_url.clone(),
            page_title: new.page_title.clone(),
            bytes: new.text.len() as u64,
            est_tokens: crate::budget::est(new.text),
            tainted_by: new.tainted_by.clone(),
            created_ms: crate::meter::now_ms(),
        };
        self.store.save_result(&row).map_err(|e| e.to_string())?;
        Ok(row)
    }

    /// The row of `handle`, if this session stored it.
    #[must_use]
    pub fn row(&self, handle: &str) -> Option<ResultRow> {
        if !valid_handle(handle) {
            return None;
        }
        self.store.result(&self.session, handle).ok().flatten()
    }

    /// The text of `handle`, if this session stored it.
    ///
    /// # Errors
    /// What to tell the model.
    pub fn get(&self, handle: &str) -> Result<(ResultRow, String), String> {
        let Some(row) = self.row(handle) else {
            return Err(format!(
                "There is no stored result \"{}\" in this conversation. Handles look like r12.",
                handle.chars().take(20).collect::<String>()
            ));
        };
        // The row's own handle (checked above), never the model's text.
        let path = self.dir.join(format!("{}.txt", row.handle));
        let text = std::fs::read_to_string(&path).map_err(|_| {
            format!(
                "{} was stored but is gone (old results are deleted).",
                row.handle
            )
        })?;
        Ok((row, text))
    }
}

/// A session id as a folder name: letters, digits, `-` and `_` only.
#[must_use]
pub fn folder_name(session: &str) -> String {
    let name: String = session
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if name.is_empty() {
        "session".to_owned()
    } else {
        name
    }
}

/// Deletes old stored results under one bot's `context_dir`: a session
/// folder whose last turn (`last_active`, else the folder's own time) is
/// more than [`KEEP_DAYS`] old, then the oldest folders while the bot has
/// more than `max_bytes`. Folders in `keep` are never deleted. Returns the
/// session folders deleted.
pub fn sweep(
    context_dir: &Path,
    last_active: &dyn Fn(&str) -> Option<i64>,
    keep: &[String],
    now_ms: i64,
    max_bytes: u64,
) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(context_dir) else {
        return Vec::new();
    };
    let keep: Vec<String> = keep.iter().map(|k| folder_name(k)).collect();
    let mut folders: Vec<(String, i64, u64)> = Vec::new();
    for entry in entries.flatten() {
        // Plain folders only, never a link.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let active = last_active(&name).unwrap_or(modified);
        folders.push((name, active, folder_bytes(&entry.path())));
    }
    folders.sort_by_key(|f| (f.1, f.0.clone()));
    let mut total: u64 = folders.iter().map(|f| f.2).sum();
    let cutoff = now_ms - KEEP_DAYS * 86_400_000;
    let mut gone = Vec::new();
    for (name, active, bytes) in folders {
        if keep.contains(&name) {
            continue;
        }
        if active < cutoff || total > max_bytes {
            let path = context_dir.join(&name);
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    total = total.saturating_sub(bytes);
                    gone.push(name);
                }
                Err(err) => tracing::warn!(%err, "couldn't delete old stored results"),
            }
        }
    }
    gone
}

/// Inside a session that stays (a bot's current one), deletes stored
/// results older than [`KEEP_DAYS`], then the oldest while the folder has
/// more than `max_bytes`. Only `rN.txt` plain files. Returns the handles
/// deleted (a read-back of one then says it's gone).
#[must_use]
pub fn sweep_session(dir: &Path, now_ms: i64, max_bytes: u64) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, i64, u64)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| {
            let handle = e.file_name().to_str()?.strip_suffix(".txt")?.to_owned();
            if !valid_handle(&handle) {
                return None;
            }
            let meta = e.metadata().ok()?;
            let ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
            Some((handle, ms, meta.len()))
        })
        .collect();
    files.sort_by_key(|f| (f.1, f.0.clone()));
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    let cutoff = now_ms - KEEP_DAYS * 86_400_000;
    let mut gone = Vec::new();
    for (handle, ms, bytes) in files {
        if ms >= cutoff && total <= max_bytes {
            break;
        }
        if std::fs::remove_file(dir.join(format!("{handle}.txt"))).is_ok() {
            total = total.saturating_sub(bytes);
            gone.push(handle);
        }
    }
    gone
}

/// Bytes of the plain files directly in `dir`.
fn folder_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryLoopStore;

    fn put(results: &ResultStore, text: &str) -> ResultRow {
        results
            .put(&NewResult {
                turn: 0,
                step: 1,
                tool: "mcp__chrome-devtools__take_snapshot",
                kind: ResultKind::Page,
                page_url: Some("https://shop.example.com/".into()),
                page_title: None,
                tainted_by: Some("a web page".into()),
                text,
            })
            .unwrap()
    }

    #[test]
    fn handles_are_checked_and_never_paths() {
        assert!(valid_handle("r1") && valid_handle("r123456"));
        for bad in [
            "", "r", "r1234567", "R1", "r1a", "../r1", "r-1", "1", "r 1", "d", "x1",
        ] {
            assert!(!valid_handle(bad), "{bad}");
        }
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryLoopStore::new());
        let results = ResultStore::new(store.clone(), "scout", "s/1", tmp.path());
        let row = put(&results, "hello");
        assert_eq!(row.handle, "r1");
        // Datasets count on their own.
        let data = results
            .put(&NewResult {
                kind: ResultKind::Rows,
                ..NewResult {
                    turn: 0,
                    step: 1,
                    tool: "mcp__oxbot__extract_rows",
                    kind: ResultKind::Page,
                    page_url: None,
                    page_title: None,
                    tainted_by: None,
                    text: "{}",
                }
            })
            .unwrap();
        assert_eq!(data.handle, "d1");
        assert!(is_dataset("d1") && !is_dataset("r1") && valid_handle("d1"));
        assert_eq!(results.get("d1").unwrap().0.kind, ResultKind::Rows);
        assert_eq!(put(&results, "again").handle, "r2");
        // The folder name is made safe; the file is outside any work folder.
        assert_eq!(results.dir(), tmp.path().join("s_1"));
        assert!(tmp.path().join("s_1/r1.txt").exists());
        assert_eq!(results.get("r1").unwrap().1, "hello");
        assert!(results.get("../r1").is_err());
        assert!(results.get("r9").unwrap_err().contains("no stored result"));
        // Another session can't read it.
        let other = ResultStore::new(store.clone(), "scout", "s2", tmp.path());
        assert!(other.get("r1").is_err());
        // A new store for the same session goes on counting.
        let again = ResultStore::new(store, "scout", "s/1", tmp.path());
        assert_eq!(put(&again, "third").handle, "r3");
    }

    #[test]
    fn the_current_session_drops_its_oldest_results_over_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, bytes) in [("r1", 300), ("r2", 300), ("r3", 300)] {
            std::fs::write(tmp.path().join(format!("{name}.txt")), "x".repeat(bytes)).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::fs::write(tmp.path().join("notes.md"), "not ours").unwrap();
        let now = crate::meter::now_ms();
        assert_eq!(sweep_session(tmp.path(), now, 1_000), Vec::<String>::new());
        assert_eq!(sweep_session(tmp.path(), now, 650), ["r1"]);
        // Two weeks on, the rest goes too; other files never do.
        assert_eq!(
            sweep_session(tmp.path(), now + 15 * 86_400_000, 1_000),
            ["r2", "r3"]
        );
        assert!(tmp.path().join("notes.md").exists());
    }

    #[test]
    fn the_sweep_drops_old_sessions_then_the_oldest_over_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, bytes) in [("old", 10), ("mid", 300), ("new", 300), ("open", 300)] {
            std::fs::create_dir_all(tmp.path().join(name)).unwrap();
            std::fs::write(tmp.path().join(name).join("r1.txt"), "x".repeat(bytes)).unwrap();
        }
        // A link is never followed or deleted.
        std::os::unix::fs::symlink(tmp.path().join("new"), tmp.path().join("link")).unwrap();
        let now = 100 * 86_400_000;
        let day = 86_400_000;
        let active = |s: &str| match s {
            "old" => Some(now - 20 * day),
            "mid" => Some(now - 3 * day),
            "new" => Some(now - day),
            "open" => Some(now - 30 * day),
            _ => None,
        };
        let gone = sweep(tmp.path(), &active, &["open".to_owned()], now, 700);
        // "old" is past 14 days; then "mid" goes for the 700-byte cap.
        assert_eq!(gone, ["old", "mid"]);
        assert!(tmp.path().join("new/r1.txt").exists());
        assert!(
            tmp.path().join("open/r1.txt").exists(),
            "the open session stays"
        );
        assert!(tmp.path().join("link").exists());
    }
}
