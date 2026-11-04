//! What the loop saves: conversations, turns with their checkpoints, and
//! every model request. [`LoopStore`] is implemented by oxbot's database
//! (`oxbot-core`); [`MemoryLoopStore`] (feature `test-tools`) keeps it in
//! memory for tests.

use oxbot_engine::CallRecord;
use oxbot_llm::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A store error, in words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LoopStoreError(pub String);

/// One saved message of a conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    /// Its place in the conversation: 0, 1, 2… (append-only).
    pub idx: u32,
    /// The turn it belongs to.
    pub turn: u32,
    /// The message.
    pub message: Message,
    /// Its size estimate in tokens.
    pub tokens: u32,
    /// Outside text (web pages, files, other bots).
    pub untrusted: bool,
    /// Where the whole result is stored (`r12`), if it is.
    pub handle: Option<String>,
    /// What the context builder keeps about it (kind, page, cleared).
    pub meta: Value,
}

/// A conversation as opened.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    /// Its id.
    pub id: String,
    /// The context builder's state (`{}` when new).
    pub context: Value,
    /// Whether it was there already (a resume) rather than made now.
    pub existed: bool,
    /// Whether its stable prefix differs from the one saved.
    pub prefix_changed: bool,
}

/// Where a turn is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    /// Running (or was, when oxbot stopped).
    Running,
    /// It asked the person something and waits for the answer.
    WaitingUser,
    /// Done.
    Done,
    /// Failed.
    Failed,
    /// Stopped by the person, or by oxbot.
    Stopped,
}

impl TurnStatus {
    /// Its name in the database.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::WaitingUser => "waiting_user",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }

    /// The status called `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Running,
            Self::WaitingUser,
            Self::Done,
            Self::Failed,
            Self::Stopped,
        ]
        .into_iter()
        .find(|s| s.as_str() == name)
    }
}

/// A tool call that started and whose result isn't saved yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCall {
    /// The call's id.
    pub id: String,
    /// The tool.
    pub name: String,
    /// Its arguments, as JSON text.
    pub args: String,
    /// Whether it changes something outside oxbot (not re-run on resume).
    pub side_effect: bool,
}

/// One turn, as saved.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnRow {
    /// The conversation.
    pub session: String,
    /// Its number in the conversation.
    pub turn: u32,
    /// The bot.
    pub bot: String,
    /// The chat event that started it.
    pub user_seq: u64,
    /// Who started the task chain, as the hub wrote it.
    pub origin: Value,
    /// What it was asked.
    pub question: String,
    /// Where it is.
    pub status: TurnStatus,
    /// Steps done.
    pub step: u32,
    /// A call in flight.
    pub pending: Option<PendingCall>,
    /// The last model that answered.
    pub model: Option<String>,
    /// How often it was picked up after a restart.
    pub resumes: u32,
    /// A practice run.
    pub dry_run: bool,
}

/// A turn's state at a checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnState {
    /// Steps done.
    pub step: u32,
    /// The last model that answered.
    pub model: Option<String>,
    /// A side-effect call that started, or `None` once its result is saved.
    pub pending: Option<PendingCall>,
}

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

/// A progress note a bot left for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressNote {
    /// The turn it was left in.
    pub turn: u32,
    /// When.
    pub ms: i64,
    /// The note.
    pub note: String,
    /// Why it can't be trusted, if it was written after reading outside
    /// text.
    pub tainted_by: Option<String>,
}

/// What one model's token counts look like next to oxbot's estimate.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationRow {
    /// The model.
    pub model: String,
    /// Provider tokens per estimated token (smoothed).
    pub ratio: f64,
    /// Requests it was learned from.
    pub samples: u32,
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

/// Where the loop saves its state.
pub trait LoopStore: Send + Sync {
    /// Opens conversation `id` for `bot`: the saved one when `resume` and
    /// it's there (noting whether `prompt_hash` changed, and saving the new
    /// one), else a new one. Other open conversations of the bot are closed.
    ///
    /// # Errors
    /// The database failed.
    fn open_session(
        &self,
        bot: &str,
        id: &str,
        prompt_hash: &str,
        resume: bool,
    ) -> Result<SessionRow, LoopStoreError>;

    /// Every message of a conversation, in order.
    ///
    /// # Errors
    /// The database failed, or a message couldn't be read.
    fn messages(&self, session: &str) -> Result<Vec<StoredMessage>, LoopStoreError>;

    /// Saves a new turn.
    ///
    /// # Errors
    /// The database failed.
    fn begin_turn(&self, turn: &TurnRow) -> Result<(), LoopStoreError>;

    /// One checkpoint, all at once: new messages, the turn's state, and the
    /// context builder's state if it changed.
    ///
    /// # Errors
    /// The database failed (nothing is saved).
    fn checkpoint(
        &self,
        session: &str,
        turn: u32,
        add: &[StoredMessage],
        state: &TurnState,
        context: Option<&Value>,
    ) -> Result<(), LoopStoreError>;

    /// Ends a turn.
    ///
    /// # Errors
    /// The database failed.
    fn end_turn(&self, session: &str, turn: u32, status: TurnStatus) -> Result<(), LoopStoreError>;

    /// Counts a pick-up after a restart.
    ///
    /// # Errors
    /// The database failed.
    fn turn_resumed(&self, session: &str, turn: u32) -> Result<(), LoopStoreError>;

    /// Turns still marked running.
    ///
    /// # Errors
    /// The database failed.
    fn running_turns(&self) -> Result<Vec<TurnRow>, LoopStoreError>;

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

    /// A saved setting of the loop's own (cached model list, browser tool
    /// list).
    ///
    /// # Errors
    /// The database failed.
    fn load_pref(&self, key: &str) -> Result<Option<String>, LoopStoreError>;

    /// Saves a setting of the loop's own.
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

    /// Saves a progress note.
    ///
    /// # Errors
    /// The database failed.
    fn add_progress(&self, session: &str, note: &ProgressNote) -> Result<(), LoopStoreError>;

    /// The last `n` progress notes of `session`, oldest first.
    ///
    /// # Errors
    /// The database failed.
    fn progress(&self, session: &str, n: usize) -> Result<Vec<ProgressNote>, LoopStoreError>;

    /// Every model's token calibration.
    ///
    /// # Errors
    /// The database failed.
    fn calibrations(&self) -> Result<Vec<CalibrationRow>, LoopStoreError>;

    /// Saves one model's token calibration.
    ///
    /// # Errors
    /// The database failed.
    fn save_calibration(&self, row: &CalibrationRow) -> Result<(), LoopStoreError>;

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

    use super::{
        CalibrationRow, CallRecord, CheckRow, LoopStore, LoopStoreError, ModelStat, ProgressNote,
        ResultRow, SessionRow, StoredMessage, TurnRow, TurnState, TurnStatus, Value,
    };

    #[derive(Default)]
    struct Inner {
        sessions: BTreeMap<String, (String, String, Value, bool)>,
        messages: BTreeMap<String, Vec<StoredMessage>>,
        turns: BTreeMap<(String, u32), TurnRow>,
        calls: Vec<CallRecord>,
        prefs: BTreeMap<String, String>,
        results: BTreeMap<(String, String), ResultRow>,
        progress: BTreeMap<String, Vec<ProgressNote>>,
        calibrations: BTreeMap<String, CalibrationRow>,
        checks: BTreeMap<(String, u32), CheckRow>,
        fail_checkpoints: bool,
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

        /// A saved turn.
        #[must_use]
        pub fn turn(&self, session: &str, turn: u32) -> Option<TurnRow> {
            self.inner().turns.get(&(session.to_owned(), turn)).cloned()
        }

        /// Every answer check saved, by session and turn.
        #[must_use]
        pub fn checks(&self) -> Vec<CheckRow> {
            self.inner().checks.values().cloned().collect()
        }

        /// Every saved turn.
        #[must_use]
        pub fn turns(&self) -> Vec<TurnRow> {
            self.inner().turns.values().cloned().collect()
        }

        /// The saved context state of a conversation.
        #[must_use]
        pub fn context(&self, session: &str) -> Option<Value> {
            self.inner().sessions.get(session).map(|s| s.2.clone())
        }

        /// Replaces the saved context state of a conversation (tests of
        /// what a later start makes of it).
        pub fn set_context(&self, session: &str, context: Value) {
            if let Some(s) = self.inner().sessions.get_mut(session) {
                s.2 = context;
            }
        }

        /// Makes every later checkpoint fail (tests of a broken disk).
        pub fn fail_checkpoints(&self) {
            self.inner().fail_checkpoints = true;
        }

        /// Marks a turn as running again, as if oxbot stopped mid-turn
        /// (tests of resuming).
        pub fn set_status(&self, session: &str, turn: u32, status: TurnStatus) {
            if let Some(t) = self.inner().turns.get_mut(&(session.to_owned(), turn)) {
                t.status = status;
            }
        }

        /// Sets a turn's call in flight (tests of resuming).
        pub fn set_pending(&self, session: &str, turn: u32, pending: Option<super::PendingCall>) {
            if let Some(t) = self.inner().turns.get_mut(&(session.to_owned(), turn)) {
                t.pending = pending;
            }
        }

        /// Drops the last `n` messages of a conversation (tests of a
        /// checkpoint that never happened).
        pub fn drop_last(&self, session: &str, n: usize) {
            if let Some(list) = self.inner().messages.get_mut(session) {
                let keep = list.len().saturating_sub(n);
                list.truncate(keep);
            }
        }
    }

    impl LoopStore for MemoryLoopStore {
        fn open_session(
            &self,
            bot: &str,
            id: &str,
            prompt_hash: &str,
            resume: bool,
        ) -> Result<SessionRow, LoopStoreError> {
            let mut inner = self.inner();
            for (sid, s) in &mut inner.sessions {
                if s.0 == bot && sid != id {
                    s.3 = true;
                }
            }
            if resume && let Some(s) = inner.sessions.get_mut(id) {
                let changed = s.1 != prompt_hash;
                prompt_hash.clone_into(&mut s.1);
                s.3 = false;
                return Ok(SessionRow {
                    id: id.to_owned(),
                    context: s.2.clone(),
                    existed: true,
                    prefix_changed: changed,
                });
            }
            inner.sessions.insert(
                id.to_owned(),
                (
                    bot.to_owned(),
                    prompt_hash.to_owned(),
                    Value::Object(serde_json::Map::new()),
                    false,
                ),
            );
            inner.messages.insert(id.to_owned(), Vec::new());
            Ok(SessionRow {
                id: id.to_owned(),
                context: Value::Object(serde_json::Map::new()),
                existed: false,
                prefix_changed: false,
            })
        }

        fn messages(&self, session: &str) -> Result<Vec<StoredMessage>, LoopStoreError> {
            Ok(self
                .inner()
                .messages
                .get(session)
                .cloned()
                .unwrap_or_default())
        }

        fn begin_turn(&self, turn: &TurnRow) -> Result<(), LoopStoreError> {
            self.inner()
                .turns
                .insert((turn.session.clone(), turn.turn), turn.clone());
            Ok(())
        }

        fn checkpoint(
            &self,
            session: &str,
            turn: u32,
            add: &[StoredMessage],
            state: &TurnState,
            context: Option<&Value>,
        ) -> Result<(), LoopStoreError> {
            let mut inner = self.inner();
            if inner.fail_checkpoints {
                return Err(LoopStoreError("the disk is full".to_owned()));
            }
            inner
                .messages
                .entry(session.to_owned())
                .or_default()
                .extend(add.iter().cloned());
            if let Some(t) = inner.turns.get_mut(&(session.to_owned(), turn)) {
                t.step = state.step;
                t.model.clone_from(&state.model);
                t.pending.clone_from(&state.pending);
            }
            if let (Some(context), Some(s)) = (context, inner.sessions.get_mut(session)) {
                s.2 = context.clone();
            }
            Ok(())
        }

        fn end_turn(
            &self,
            session: &str,
            turn: u32,
            status: TurnStatus,
        ) -> Result<(), LoopStoreError> {
            if let Some(t) = self.inner().turns.get_mut(&(session.to_owned(), turn)) {
                t.status = status;
                t.pending = None;
            }
            Ok(())
        }

        fn turn_resumed(&self, session: &str, turn: u32) -> Result<(), LoopStoreError> {
            if let Some(t) = self.inner().turns.get_mut(&(session.to_owned(), turn)) {
                t.resumes += 1;
            }
            Ok(())
        }

        fn running_turns(&self) -> Result<Vec<TurnRow>, LoopStoreError> {
            Ok(self
                .inner()
                .turns
                .values()
                .filter(|t| t.status == TurnStatus::Running)
                .cloned()
                .collect())
        }

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

        fn add_progress(&self, session: &str, note: &ProgressNote) -> Result<(), LoopStoreError> {
            self.inner()
                .progress
                .entry(session.to_owned())
                .or_default()
                .push(note.clone());
            Ok(())
        }

        fn progress(&self, session: &str, n: usize) -> Result<Vec<ProgressNote>, LoopStoreError> {
            let inner = self.inner();
            let all = inner.progress.get(session).map_or(&[][..], Vec::as_slice);
            Ok(all[all.len().saturating_sub(n)..].to_vec())
        }

        fn calibrations(&self) -> Result<Vec<CalibrationRow>, LoopStoreError> {
            Ok(self.inner().calibrations.values().cloned().collect())
        }

        fn save_calibration(&self, row: &CalibrationRow) -> Result<(), LoopStoreError> {
            self.inner()
                .calibrations
                .insert(row.model.clone(), row.clone());
            Ok(())
        }

        fn record_check(&self, row: &CheckRow) -> Result<(), LoopStoreError> {
            self.inner()
                .checks
                .insert((row.session.clone(), row.turn), row.clone());
            Ok(())
        }
    }
}
