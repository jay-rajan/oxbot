//! Codex processes: which one serves a session ([`process_key`], the one
//! place that's decided), started on first use with a fresh config and its
//! own relay token, kept while sessions use it, started again after a
//! crash (waiting longer after each one), and stopped when nothing has
//! used it for a while.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use oxbot_engine::SessionSpec;
use oxbot_relay::{ClientInfo, Relay, RelayClient};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::binary::CodexBinary;
use crate::config;
use crate::rpc::{Launch, Process};

/// First wait before starting a process again after it crashed.
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
/// Longest wait.
const MAX_BACKOFF: Duration = Duration::from_mins(1);

/// Which Codex process serves a session, and where it keeps its home.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    /// Its name in logs and the supervisor, e.g. `bot:scout`.
    pub id: String,
    /// Its CODEX_HOME.
    pub home: PathBuf,
}

/// The process for `spec`'s session: one per bot, whoever asks, with its
/// home in the bot's folder (`bots/<id>/codex`). The one place this is
/// decided: a process per person would change only this.
#[must_use]
pub fn process_key(spec: &SessionSpec) -> ProcessKey {
    ProcessKey {
        id: format!("bot:{}", spec.bot),
        home: bot_dir(&spec.context_dir).join("codex"),
    }
}

/// The bot's folder from its context folder (`bots/<id>/context`).
fn bot_dir(context_dir: &Path) -> PathBuf {
    if context_dir.file_name().is_some_and(|n| n == "context") {
        context_dir
            .parent()
            .map_or_else(|| context_dir.to_path_buf(), Path::to_path_buf)
    } else {
        context_dir.to_path_buf()
    }
}

/// A process in use: its JSON-RPC, its place at the relay, and which
/// start of it this is (a thread loaded in an earlier one isn't loaded in
/// this one).
#[derive(Debug, Clone)]
pub struct Running {
    /// The process.
    pub process: Arc<Process>,
    /// Its place at the relay.
    pub relay: Arc<RelayClient>,
    /// Counts starts of this key's process.
    pub generation: u64,
}

#[derive(Default)]
struct Slot {
    running: Option<Running>,
    generation: u64,
    users: usize,
    last_used: Option<Instant>,
    /// Crashes in a row (reset when one lives long enough).
    crashes: u32,
    not_before: Option<Instant>,
    started: Option<Instant>,
}

/// Starts and keeps the processes.
pub struct Supervisor {
    binary: CodexBinary,
    relay: Arc<Relay>,
    refuse_proxy: String,
    idle: Duration,
    slots: Mutex<HashMap<String, Arc<Mutex<Slot>>>>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor")
            .field("binary", &self.binary)
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// A supervisor running `binary`, its processes reaching `relay`.
    #[must_use]
    pub fn new(
        binary: CodexBinary,
        relay: Arc<Relay>,
        refuse_proxy: String,
        idle: Duration,
    ) -> Self {
        Self {
            binary,
            relay,
            refuse_proxy,
            idle,
            slots: Mutex::new(HashMap::new()),
        }
    }

    async fn slot(&self, key: &ProcessKey) -> Arc<Mutex<Slot>> {
        Arc::clone(self.slots.lock().await.entry(key.id.clone()).or_default())
    }

    /// The process for `key`, started if it isn't running (after the
    /// wait a crash calls for). `info` is the bot as the relay applies it.
    ///
    /// # Errors
    /// It couldn't start, in words for the person.
    pub async fn get(&self, key: &ProcessKey, info: ClientInfo) -> Result<Running, String> {
        let slot = self.slot(key).await;
        let mut slot = slot.lock().await;
        slot.last_used = Some(Instant::now());
        if let Some(running) = &slot.running {
            if !running.process.is_gone() {
                running.relay.update(info.choice.clone(), info.privacy);
                return Ok(running.clone());
            }
            // It crashed: wait longer each time it does so soon.
            let lived = slot.started.map_or(Duration::ZERO, |s| s.elapsed());
            slot.crashes = if lived > MAX_BACKOFF {
                1
            } else {
                slot.crashes + 1
            };
            let wait = FIRST_BACKOFF
                .saturating_mul(2u32.saturating_pow(slot.crashes.saturating_sub(1)))
                .min(MAX_BACKOFF);
            tracing::warn!(
                key = key.id,
                crashes = slot.crashes,
                "Codex stopped; starting it again"
            );
            slot.not_before = Some(Instant::now() + wait);
            slot.running = None;
        }
        if let Some(at) = slot.not_before {
            tokio::time::sleep_until(at).await;
        }
        let (relay, token) = self.relay.register(info.clone())?;
        config::write(&key.home, &config::config_toml(&self.relay.base_url()))
            .map_err(|e| format!("couldn't write the AI helper's settings: {e}"))?;
        let launch = Launch {
            program: self.binary.path.clone(),
            home: key.home.clone(),
            token,
            refuse_proxy: self.refuse_proxy.clone(),
        };
        match Process::start(&launch, &info.bot).await {
            Ok(process) => {
                slot.generation += 1;
                slot.started = Some(Instant::now());
                slot.not_before = None;
                let running = Running {
                    process,
                    relay: Arc::new(relay),
                    generation: slot.generation,
                };
                slot.running = Some(running.clone());
                tracing::info!(
                    key = key.id,
                    version = self.binary.version,
                    "Codex is running"
                );
                Ok(running)
            }
            Err(err) => {
                slot.crashes += 1;
                let wait = FIRST_BACKOFF
                    .saturating_mul(2u32.saturating_pow(slot.crashes.saturating_sub(1)))
                    .min(MAX_BACKOFF);
                slot.not_before = Some(Instant::now() + wait);
                Err(format!("The AI helper couldn't start: {err}"))
            }
        }
    }

    /// A session started using `key`'s process.
    pub async fn hold(&self, key: &ProcessKey) {
        let slot = self.slot(key).await;
        let mut slot = slot.lock().await;
        slot.users += 1;
        slot.last_used = Some(Instant::now());
    }

    /// A session stopped using `key`'s process: it stops once nothing
    /// else does.
    pub async fn release(&self, key: &ProcessKey) {
        let slot = self.slot(key).await;
        let mut slot = slot.lock().await;
        slot.users = slot.users.saturating_sub(1);
        if slot.users == 0
            && let Some(running) = slot.running.take()
        {
            running.process.stop().await;
            tracing::info!(key = key.id, "stopped Codex");
        }
    }

    /// `key`'s process did something now.
    pub async fn touch(&self, key: &ProcessKey) {
        self.slot(key).await.lock().await.last_used = Some(Instant::now());
    }

    /// Stops processes nothing used for the idle time.
    pub async fn stop_idle(&self) {
        let slots: Vec<(String, Arc<Mutex<Slot>>)> = self
            .slots
            .lock()
            .await
            .iter()
            .map(|(k, s)| (k.clone(), Arc::clone(s)))
            .collect();
        for (key, slot) in slots {
            let Ok(mut slot) = slot.try_lock() else {
                continue;
            };
            let idle = slot.last_used.is_none_or(|t| t.elapsed() >= self.idle);
            if idle && let Some(running) = slot.running.take() {
                running.process.stop().await;
                tracing::info!(key, "stopped idle Codex");
            }
        }
    }

    /// Stops every process.
    pub async fn stop_all(&self) {
        let slots: Vec<Arc<Mutex<Slot>>> = self.slots.lock().await.values().cloned().collect();
        for slot in slots {
            if let Some(running) = slot.lock().await.running.take() {
                running.process.stop().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_process_per_bot_in_the_bots_folder() {
        let spec = |bot: &str, ctx: &str| SessionSpec {
            bot: bot.into(),
            work_dir: PathBuf::from("/d/bots/x/work"),
            context_dir: PathBuf::from(ctx),
            knowledge_dir: None,
            system_prompt: String::new(),
            keeps_files: false,
            resume: oxbot_engine::Resume::New("s".into()),
            model: oxbot_engine::ModelChoice::Auto,
            browser: None,
            host_tools: oxbot_engine::HostTools::default(),
            host_tool_specs: Vec::new(),
            privacy: oxbot_engine::Privacy::Standard,
            gate_mode: oxbot_engine::GateMode::Live,
            env: Vec::new(),
            tool_wait: Duration::from_secs(1),
        };
        let a = process_key(&spec("scout", "/d/bots/scout/context"));
        assert_eq!(a.id, "bot:scout");
        assert_eq!(a.home, PathBuf::from("/d/bots/scout/codex"));
        // Another session of the same bot: the same process.
        assert_eq!(process_key(&spec("scout", "/d/bots/scout/context")), a);
        assert_ne!(process_key(&spec("news", "/d/bots/news/context")), a);
        // A context folder elsewhere keeps the home inside it.
        assert_eq!(
            process_key(&spec("t1", "/d/trials/t1")).home,
            PathBuf::from("/d/trials/t1/codex")
        );
    }
}
