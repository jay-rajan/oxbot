//! What every oxbot engine shares.
//!
//! An engine runs a bot: today Claude Code (`claude`), and oxbot's own
//! agent loop on `OpenRouter`. The hub talks to each one only through this
//! crate:
//!
//! - [`Engine`] starts sessions; [`Session`] takes turns.
//! - An engine reports what happens as [`EngineEvent`]s, in order.
//! - Before any tool runs, the engine asks the hub with
//!   [`EngineEvent::Permission`], and oxbot's own tools run in the hub
//!   through [`EngineEvent::HostTool`]. Both carry a [`Reply`]: dropping it
//!   unanswered means "no", so a lost answer can never let a tool run.
//!
//! No I/O here: only types, traits, [`text`] helpers and the [`trust`]
//! lists.

mod event;
mod reply;
mod spec;
pub mod text;
pub mod trust;

use std::fmt;

pub use event::{
    Activity, CallRecord, EngineEvent, HostToolCall, LimitWindow, PermissionRequest, ToolUse,
    TurnUsage,
};
pub use futures_util::future::BoxFuture;
pub use reply::{Decision, Refusal, Reply, ReplyWait, ToolReply};
pub use spec::{
    BrowserSpec, GateMode, HostTools, McpCommand, ModelChoice, Privacy, Resume, ResumedTurn,
    SessionSpec, ToolSpec, TurnInput,
};

use serde::{Deserialize, Serialize};

/// Which engine runs a bot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    /// Claude Code on the Farm owner's own Claude plan.
    #[default]
    Claude,
    /// oxbot's own agent loop on `OpenRouter` models.
    OpenRouter,
}

impl EngineKind {
    /// Every engine, in the order settings show them.
    pub const ALL: [Self; 2] = [Self::Claude, Self::OpenRouter];

    /// Its name in files, the database and the protocol: `claude`,
    /// `openrouter`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::OpenRouter => "openrouter",
        }
    }

    /// The engine called `name` (as [`EngineKind::as_str`] writes it).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why an engine can't run right now, in words for the person.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NotReady(pub String);

/// Errors from an engine. Each shows its message as it is.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The engine can't run right now (e.g. not signed in).
    #[error(transparent)]
    NotReady(#[from] NotReady),
    /// The session couldn't be started.
    #[error("{0}")]
    Start(String),
    /// The session has stopped (its process exited, say).
    #[error("{0}")]
    Stopped(String),
    /// Anything else.
    #[error("{0}")]
    Other(String),
}

/// Starts sessions. One per engine per hub (shared state, such as a
/// request pacer, lives here).
pub trait Engine: Send + Sync + fmt::Debug {
    /// Which engine this is.
    fn kind(&self) -> EngineKind;

    /// Whether it can run now (`OpenRouter`: signed in; Claude: always, its
    /// setup checks show problems).
    ///
    /// # Errors
    /// [`NotReady`] with what's missing.
    fn ready(&self) -> Result<(), NotReady>;

    /// Starts a session for one bot.
    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<Started, EngineError>>;
}

/// A session that just started.
pub struct Started {
    /// The session, for sending turns.
    pub session: Box<dyn Session>,
    /// What it reports, in order. Closes when the session ends (the hub
    /// then cleans up).
    pub events: tokio::sync::mpsc::Receiver<EngineEvent>,
    /// A turn that was running when oxbot stopped and now goes on (loop
    /// engine only).
    pub resumed_turn: Option<ResumedTurn>,
}

impl fmt::Debug for Started {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Started")
            .field("session", &self.session.id())
            .field("resumed_turn", &self.resumed_turn)
            .finish_non_exhaustive()
    }
}

/// A running bot session (Claude: one `claude` child; loop: one
/// conversation and its tools).
pub trait Session: Send {
    /// The session's id, as saved with the bot.
    fn id(&self) -> &str;

    /// Starts a turn. Returns once the engine has it; the turn goes on and
    /// reports through the events.
    fn send(&mut self, turn: TurnInput) -> BoxFuture<'_, Result<(), EngineError>>;

    /// Stops the current turn; the session stays.
    fn interrupt(&mut self) -> BoxFuture<'_, Result<(), EngineError>>;

    /// Ends the session: a short grace period, then it's killed.
    fn shutdown(self: Box<Self>) -> BoxFuture<'static, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_names_round_trip() {
        for kind in EngineKind::ALL {
            assert_eq!(EngineKind::parse(kind.as_str()), Some(kind));
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.as_str())
            );
        }
        assert_eq!(EngineKind::parse("codex"), None);
        assert_eq!(EngineKind::default(), EngineKind::Claude);
        assert_eq!(
            serde_json::from_str::<EngineKind>("\"openrouter\"").unwrap(),
            EngineKind::OpenRouter
        );
    }

    #[test]
    fn errors_show_their_message_as_it_is() {
        assert_eq!(
            EngineError::Start("couldn't start claude: no such file".into()).to_string(),
            "couldn't start claude: no such file"
        );
        assert_eq!(
            EngineError::from(NotReady("Sign in with OpenRouter first.".into())).to_string(),
            "Sign in with OpenRouter first."
        );
    }
}
