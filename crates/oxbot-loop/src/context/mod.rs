//! The context builder: what the model is sent each step.
//!
//! Layout of every request (each layer is unit-tested for stable bytes):
//! 1. one system message: the bot's rules (tools go in `tools`, in their
//!    fixed order); the same bytes every call;
//! 2. the window header ([`header`]): the day, what the bot knows, and
//!    after a compaction the task state, frozen for the window and put in
//!    front of the window's first message;
//! 3. the conversation since the window started, append-only; old tool
//!    results become one-line stubs when it gets long ([`window`]);
//! 4. now and then, the to-do list recited at the end of the newest tool
//!    result.
//!
//! [`WindowedContext`] does all of that. When it's still too long, the
//! compactor ([`crate::compact`]) summarizes into a task state and a new
//! window starts.

pub mod header;
pub mod normalize;
pub mod state;
pub mod window;

use oxbot_llm::{Message, ToolSpec};
use serde_json::Value;

use crate::KnowledgeBlock;
pub use crate::budget::TokenBudget;
use crate::store::StoredMessage;
pub use window::WindowedContext;

/// The last model request of the session, as the provider counted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCall {
    /// The model that answered.
    pub model: String,
    /// Input tokens it counted.
    pub prompt_tokens: u64,
    /// Output tokens it counted.
    pub completion_tokens: u64,
    /// How many messages were sent.
    pub messages_sent: usize,
    /// The newest stored message that was sent.
    pub last_idx: Option<u32>,
    /// The builder's generation then (see [`state::WindowState::generation`]).
    pub generation: u64,
}

/// Everything a builder may use.
#[derive(Debug, Clone, Copy)]
pub struct TaskView<'a> {
    /// The system prompt (rules), part of the stable prefix.
    pub system: &'a str,
    /// The tools, in their fixed order, part of the stable prefix.
    pub tools: &'a [ToolSpec],
    /// What the bot knows that matters for this turn.
    pub knowledge: Option<&'a KnowledgeBlock>,
    /// The whole conversation, in order.
    pub messages: &'a [StoredMessage],
    /// The builder's own state, kept with the conversation.
    pub context: &'a Value,
    /// The person the turn acts for.
    pub person: Option<&'a str>,
    /// Now, in milliseconds since the Unix epoch.
    pub now_ms: i64,
    /// The session's last model request.
    pub last_call: Option<&'a LastCall>,
    /// The turn being run.
    pub turn: u32,
    /// Provider tokens per estimated token, for the models that may answer.
    pub ratio: f64,
    /// The models that may answer the next request.
    pub answering: &'a [String],
}

/// Whether the context fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressure {
    /// It fits.
    Ok,
    /// It's over the budget: compact first.
    Compact,
}

/// What a builder made.
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    /// The messages to send, system first.
    pub messages: Vec<Message>,
    /// Their tokens as the provider will likely count them, tools included.
    pub est_tokens: u64,
    /// Whether they fit.
    pub pressure: Pressure,
    /// New builder state to save with the step, if it changed.
    pub context: Option<Value>,
    /// Messages (by `idx`) left out or shown as stubs.
    pub cleared: Vec<u32>,
    /// The newest stored message sent.
    pub last_idx: Option<u32>,
    /// The builder's generation for these bytes.
    pub generation: u64,
}

/// Builds what the model is sent.
pub trait ContextBuilder: Send + Sync + std::fmt::Debug {
    /// The messages for the next request.
    fn build(&self, view: &TaskView<'_>, budget: TokenBudget) -> Built;
}

/// Where the window starts in the conversation.
#[must_use]
pub fn window_start(context: &Value) -> u32 {
    state::WindowState::from_value(context).start_idx
}
