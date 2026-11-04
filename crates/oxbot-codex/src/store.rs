//! Where the Codex engine keeps each bot session's thread.

/// A bot session's Codex thread (`codex_threads`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRow {
    /// The bot.
    pub bot: String,
    /// The bot's session.
    pub session: String,
    /// Codex's id for the thread.
    pub thread_id: String,
    /// The model it was started for.
    pub model: String,
    /// The window Codex was told.
    pub window: u64,
    /// Turns so far.
    pub turns: u32,
    /// When it was made, ms since the Unix epoch.
    pub created_ms: i64,
    /// When it last changed.
    pub updated_ms: i64,
}

/// The store the Codex engine uses (the hub's database).
pub trait CodexStore: Send + Sync {
    /// The thread of `bot`'s session `session`, if it has one.
    ///
    /// # Errors
    /// The store failed.
    fn thread(&self, bot: &str, session: &str) -> Result<Option<ThreadRow>, String>;

    /// Saves (or replaces) a thread.
    ///
    /// # Errors
    /// The store failed.
    fn save_thread(&self, row: &ThreadRow) -> Result<(), String>;
}

/// In memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryCodexStore(std::sync::Mutex<Vec<ThreadRow>>);

impl CodexStore for MemoryCodexStore {
    fn thread(&self, bot: &str, session: &str) -> Result<Option<ThreadRow>, String> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "poisoned".to_owned())?
            .iter()
            .find(|r| r.bot == bot && r.session == session)
            .cloned())
    }

    fn save_thread(&self, row: &ThreadRow) -> Result<(), String> {
        let mut rows = self.0.lock().map_err(|_| "poisoned".to_owned())?;
        rows.retain(|r| !(r.bot == row.bot && r.session == row.session));
        rows.push(row.clone());
        Ok(())
    }
}
