//! The hub's side of the Codex engine: starting oxbot's model relay and
//! the engine once the loop is set up (they need its routing, limits and
//! meter), looking for the `codex` program again when the owner points at
//! another, and what Settings → Engines shows about it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use oxbot_codex::{CodexEngine, CodexSetup, Found};
use oxbot_protocol::CodexView;
use oxbot_relay::{Relay, RelaySetup, Upstream};

use super::Hub;

/// Where the owner said the `codex` program is (empty: look for it).
pub(super) const PREF_CODEX_PATH: &str = "codex.path";
/// Longest a Codex turn works (time waiting on the person aside).
const MAX_TURN: Duration = Duration::from_mins(20);

/// What the hub needs to run bots on Codex: where their model requests
/// go (`OpenRouter`'s Responses API, a stand-in in tests) and where
/// `codex` is, if the environment says (`OXBOT_CODEX`).
#[derive(Clone)]
pub struct CodexParts {
    /// `OpenRouter` (with the household's key), or a stand-in.
    pub upstream: Arc<dyn Upstream>,
    /// `OXBOT_CODEX`, if set.
    pub program: Option<PathBuf>,
}

impl std::fmt::Debug for CodexParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodexParts")
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

impl Hub {
    /// Starts the relay and the Codex engine (once; needs `OpenRouter`'s
    /// loop). Call once at start, inside the runtime.
    pub async fn start_codex(&self) {
        let (Some(parts), Some(loop_engine)) =
            (self.config.codex.clone(), self.loop_engine.clone())
        else {
            return;
        };
        if self.codex.get().is_some() {
            return;
        }
        let relay = match Relay::start(RelaySetup {
            gateway: loop_engine.gateway(),
            upstream: parts.upstream,
            log: Arc::clone(&self.store) as _,
        })
        .await
        {
            Ok(relay) => Arc::new(relay),
            Err(err) => {
                tracing::warn!(%err, "the model relay couldn't start; Codex bots can't run");
                return;
            }
        };
        let found = self.find_codex_program().await;
        match &found {
            Found::Ready(b) => {
                tracing::info!(path = %b.path.display(), version = b.version, "found Codex");
            }
            Found::Unusable { why, .. } => tracing::info!(why, "Codex bots can't run"),
        }
        let engine = CodexEngine::new(CodexSetup {
            loop_engine,
            relay,
            store: Arc::clone(&self.store) as _,
            found,
            idle_stop: self.config.idle_stop,
            max_turn: MAX_TURN,
        })
        .await;
        match engine {
            Ok(engine) => {
                let _ = self.codex.set(Arc::new(engine));
            }
            Err(err) => tracing::warn!(%err, "the Codex engine couldn't start"),
        }
    }

    /// Looks for `codex`: where the owner said, else `OXBOT_CODEX`, else
    /// `PATH` and Homebrew's folders.
    async fn find_codex_program(&self) -> Found {
        let configured = self
            .store
            .pref(PREF_CODEX_PATH)
            .ok()
            .flatten()
            .filter(|p| !p.trim().is_empty())
            .map(PathBuf::from)
            .or_else(|| self.config.codex.as_ref().and_then(|p| p.program.clone()));
        let path_var = self
            .config
            .extra_env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("PATH").ok());
        let scratch = self.config.data_dir.join("codex-probe");
        oxbot_codex::binary::find(configured.as_deref(), path_var.as_deref(), &scratch).await
    }

    /// The owner pointed at `path` (empty: look for it): keep it, look
    /// again, and use what's found for new sessions.
    ///
    /// # Errors
    /// The setting couldn't be saved.
    pub async fn find_codex(&self, path: &str) -> Result<(), super::HubError> {
        self.store.set_pref(PREF_CODEX_PATH, path.trim())?;
        let found = self.find_codex_program().await;
        if let Some(engine) = self.codex.get() {
            engine.use_found(found);
        }
        Ok(())
    }

    /// Settings → Engines → Codex: what was found, for the Farm.
    #[must_use]
    pub fn codex_view(&self) -> CodexView {
        let configured = self
            .store
            .pref(PREF_CODEX_PATH)
            .ok()
            .flatten()
            .unwrap_or_default();
        let Some(engine) = self.codex.get() else {
            return CodexView {
                path: None,
                version: None,
                problem: Some(if self.loop_engine.is_none() {
                    "The AI helper needs the home's AI account (OpenRouter), which isn't connected yet.".to_owned()
                } else {
                    "The AI helper is still being looked for.".to_owned()
                }),
                configured,
            };
        };
        match engine.found() {
            Found::Ready(b) => CodexView {
                path: Some(b.path.display().to_string()),
                version: Some(b.version),
                problem: None,
                configured,
            },
            Found::Unusable { path, version, why } => CodexView {
                path: path.map(|p| p.display().to_string()),
                version,
                problem: Some(why),
                configured,
            },
        }
    }

    /// The Codex engine, if it started.
    pub(super) fn codex_engine(&self) -> Option<Arc<CodexEngine>> {
        self.codex.get().cloned()
    }

    /// Deletes what the relay sent more than 14 days ago: the files in
    /// each bot's context folder, and their rows.
    pub(super) fn sweep_relay_log(&self, bots: &[(crate::id::BotId, PathBuf)], now_ms: i64) {
        let cutoff = now_ms - oxbot_loop::results::KEEP_DAYS * 86_400_000;
        for (bot, dir) in bots {
            let gone = oxbot_relay::sweep(dir, cutoff, oxbot_relay::MAX_LOG_BYTES);
            if gone > 0 {
                tracing::info!(%bot, gone, "deleted what the relay sent long ago");
            }
        }
        if let Err(err) = self.store.forget_relay_log(cutoff) {
            tracing::warn!(%err, "couldn't forget the relay's old log");
        }
    }
}
