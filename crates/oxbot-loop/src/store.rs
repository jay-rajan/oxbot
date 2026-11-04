//! What Codex bots' tools and requests save: stored tool results, every
//! model request, answer checks and a few settings. [`LoopStore`] is
//! implemented by oxbot's database (`oxbot-core`); [`MemoryLoopStore`]
//! (feature `test-tools`) keeps it in memory for tests.

use oxbot_engine::CallRecord;
use serde::{Deserialize, Serialize};

/// A store error, in words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LoopStoreError(pub String);

/// How one model did lately, for the router.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelStat {
    /// The model.
    pub model: String,
    /// Requests that worked.
    pub ok: u32,
    /// All requests.
    pub total: u32,
    /// Average time of the ones that worked.
    pub avg_latency_ms: u64,
}

/// What a stored tool result holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    /// A browser page snapshot.
    Page,
    /// Any other text.
    Text,
    /// A table or list read off a page (`extract_rows`), as JSON; its
    /// handle is `d<N>`.
    Rows,
}

impl ResultKind {
    /// Its name in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Page => "page",
            Self::Text => "text",
            Self::Rows => "rows",
        }
    }

    /// The kind called `name` (anything unknown is text).
    #[must_use]
    pub fn parse(name: &str) -> Self {
        match name {
            "page" => Self::Page,
            "rows" => Self::Rows,
            _ => Self::Text,
        }
    }
}

/// The index row of one stored tool result (its text is a file in the
/// session's context folder).
#[derive(Debug, Clone, PartialEq)]
pub struct ResultRow {
    /// The conversation.
    pub session: String,
    /// `r<N>`.
    pub handle: String,
    /// The bot.
    pub bot: String,
    /// The turn it came in.
    pub turn: u32,
    /// The step it came in.
    pub step: u32,
    /// The tool that returned it.
    pub tool: String,
    /// A page or other text.
    pub kind: ResultKind,
    /// The page it shows, for pages.
    pub page_url: Option<String>,
    /// The page's title, for pages.
    pub page_title: Option<String>,
    /// Its size.
    pub bytes: u64,
    /// Its estimated tokens.
    pub est_tokens: u64,
    /// Why it can't be trusted (outside text), if so.
    pub tainted_by: Option<String>,
    /// When it was stored.
    pub created_ms: i64,
}

/// How one turn's final answer was checked: whether it was sent back once
/// to be checked, and the values in it that nothing the bot read had,
/// before and after. Owner-only data; nothing leaves the Mac.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRow {
    /// The conversation.
    pub session: String,
    /// The turn.
    pub turn: u32,
    /// The answer was sent back once to be checked.
    pub bounced: bool,
    /// Values not in anything read, in the first answer.
    pub ungrounded_before: Vec<String>,
    /// Values not in anything read, in the answer accepted.
    pub ungrounded_after: Vec<String>,
    /// How long the check took, from the first answer to the last, in ms.
    pub ms: i64,
}

/// Where bots' results, requests and checks are saved.
pub trait LoopStore: Send + Sync {
    /// Saves one model request.
    ///
    /// # Errors
    /// The database failed.
    fn record_call(&self, call: &CallRecord) -> Result<(), LoopStoreError>;

    /// How each model did since `since_ms` (`OpenRouter` requests only).
    ///
    /// # Errors
    /// The database failed.
    fn model_stats(&self, since_ms: i64) -> Result<Vec<ModelStat>, LoopStoreError>;

    /// A saved setting of its own (the browser tool list).
    ///
    /// # Errors
    /// The database failed.
    fn load_pref(&self, key: &str) -> Result<Option<String>, LoopStoreError>;

    /// Saves a setting of its own.
    ///
    /// # Errors
    /// The database failed.
    fn save_pref(&self, key: &str, value: &str) -> Result<(), LoopStoreError>;

    /// Saves the index row of a stored tool result.
    ///
    /// # Errors
    /// The database failed.
    fn save_result(&self, row: &ResultRow) -> Result<(), LoopStoreError>;

    /// A stored result of `session` by its handle.
    ///
    /// # Errors
    /// The database failed.
    fn result(&self, session: &str, handle: &str) -> Result<Option<ResultRow>, LoopStoreError>;

    /// The highest number `session` has used for handles starting with
    /// `prefix` (`r` for results, `d` for datasets; 0 for none).
    ///
    /// # Errors
    /// The database failed.
    fn last_result_number(&self, session: &str, prefix: char) -> Result<u32, LoopStoreError>;

    /// Saves how a turn's answer was checked (one row per turn).
    ///
    /// # Errors
    /// The database failed.
    fn record_check(&self, row: &CheckRow) -> Result<(), LoopStoreError>;
}

#[cfg(any(test, feature = "test-tools"))]
pub use memory::MemoryLoopStore;

#[cfg(any(test, feature = "test-tools"))]
mod memory {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::{CallRecord, CheckRow, LoopStore, LoopStoreError, ModelStat, ResultRow};

    #[derive(Default)]
    struct Inner {
        calls: Vec<CallRecord>,
        prefs: BTreeMap<String, String>,
        results: BTreeMap<(String, String), ResultRow>,
        checks: BTreeMap<(String, u32), CheckRow>,
    }

    /// A [`LoopStore`] in memory, for tests.
    #[derive(Default)]
    pub struct MemoryLoopStore(Mutex<Inner>);

    impl std::fmt::Debug for MemoryLoopStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("MemoryLoopStore")
        }
    }

    impl MemoryLoopStore {
        /// An empty store.
        #[must_use]
        pub fn new() -> Self {
            Self::default()
        }

        fn inner(&self) -> MutexGuard<'_, Inner> {
            self.0.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// Every model request saved.
        #[must_use]
        pub fn calls(&self) -> Vec<CallRecord> {
            self.inner().calls.clone()
        }

        /// Every answer check saved, by session and turn.
        #[must_use]
        pub fn checks(&self) -> Vec<CheckRow> {
            self.inner().checks.values().cloned().collect()
        }
    }

    impl LoopStore for MemoryLoopStore {
        fn record_call(&self, call: &CallRecord) -> Result<(), LoopStoreError> {
            self.inner().calls.push(call.clone());
            Ok(())
        }

        fn model_stats(&self, since_ms: i64) -> Result<Vec<ModelStat>, LoopStoreError> {
            let mut by: BTreeMap<String, (u32, u32, u64)> = BTreeMap::new();
            for c in self.inner().calls.iter().filter(|c| c.ts_ms >= since_ms) {
                let model = if c.model.is_empty() {
                    &c.requested
                } else {
                    &c.model
                };
                let e = by.entry(model.clone()).or_default();
                e.1 += 1;
                if c.outcome == "ok" {
                    e.0 += 1;
                    e.2 += c.latency_ms;
                }
            }
            Ok(by
                .into_iter()
                .map(|(model, (ok, total, lat))| ModelStat {
                    model,
                    ok,
                    total,
                    avg_latency_ms: if ok == 0 { 0 } else { lat / u64::from(ok) },
                })
                .collect())
        }

        fn load_pref(&self, key: &str) -> Result<Option<String>, LoopStoreError> {
            Ok(self.inner().prefs.get(key).cloned())
        }

        fn save_pref(&self, key: &str, value: &str) -> Result<(), LoopStoreError> {
            self.inner().prefs.insert(key.to_owned(), value.to_owned());
            Ok(())
        }

        fn save_result(&self, row: &ResultRow) -> Result<(), LoopStoreError> {
            self.inner()
                .results
                .insert((row.session.clone(), row.handle.clone()), row.clone());
            Ok(())
        }

        fn result(&self, session: &str, handle: &str) -> Result<Option<ResultRow>, LoopStoreError> {
            Ok(self
                .inner()
                .results
                .get(&(session.to_owned(), handle.to_owned()))
                .cloned())
        }

        fn last_result_number(&self, session: &str, prefix: char) -> Result<u32, LoopStoreError> {
            Ok(self
                .inner()
                .results
                .keys()
                .filter(|(s, _)| s == session)
                .filter_map(|(_, h)| h.strip_prefix(prefix)?.parse().ok())
                .max()
                .unwrap_or(0))
        }

        fn record_check(&self, row: &CheckRow) -> Result<(), LoopStoreError> {
            self.inner()
                .checks
                .insert((row.session.clone(), row.turn), row.clone());
            Ok(())
        }
    }
}
