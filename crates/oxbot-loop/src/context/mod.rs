//! The context builder: what the model is sent each step. The stable
//! prefix (rules and tools, the same bytes every call) comes first, then
//! what the bot knows, then the conversation.
//!
//! #18 ships [`PlainContext`]: the prefix and every message since the
//! window's start (`context.from_idx`, moved on by the compactor). #19
//! replaces it through [`ContextBuilder`].

use oxbot_llm::{Message, ToolSpec};
use serde_json::Value;

use crate::KnowledgeBlock;
use crate::budget::{est, est_message, est_tools};
use crate::store::StoredMessage;

/// How many tokens the context may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBudget {
    /// The model's window (the smallest of the models that may answer).
    pub window: u64,
    /// Kept free for the answer.
    pub reserve: u64,
}

impl TokenBudget {
    /// Tokens the context may use.
    #[must_use]
    pub fn available(&self) -> u64 {
        self.window.saturating_sub(self.reserve)
    }
}

/// The last model request of the session, as the provider counted it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCall {
    /// The model that answered.
    pub model: String,
    /// Input tokens it counted.
    pub prompt_tokens: u64,
    /// How many messages were sent.
    pub messages_sent: usize,
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
    /// Their estimated tokens, tools included.
    pub est_tokens: u64,
    /// Whether they fit.
    pub pressure: Pressure,
    /// New builder state to save with the step, if it changed.
    pub context: Option<Value>,
    /// Messages (by `idx`) left out or shortened this time.
    pub cleared: Vec<u32>,
}

/// Builds what the model is sent.
pub trait ContextBuilder: Send + Sync + std::fmt::Debug {
    /// The messages for the next request.
    fn build(&self, view: &TaskView<'_>, budget: TokenBudget) -> Built;
}

/// Where the window starts in the conversation (`context.from_idx`).
#[must_use]
pub fn window_start(context: &Value) -> u32 {
    context
        .get("from_idx")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}

/// Rules, tools, knowledge, then every message in the window.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainContext;

impl ContextBuilder for PlainContext {
    fn build(&self, view: &TaskView<'_>, budget: TokenBudget) -> Built {
        let from = window_start(view.context);
        let mut messages = vec![Message::system(view.system)];
        let mut tokens = est(view.system) + est_tools(view.tools);
        if let Some(k) = view.knowledge.filter(|k| !k.text.is_empty()) {
            messages.push(Message::user(k.text.clone()));
            tokens += est(&k.text);
        }
        let mut cleared = Vec::new();
        for m in view.messages {
            if m.idx < from {
                cleared.push(m.idx);
                continue;
            }
            tokens += if m.tokens > 0 {
                u64::from(m.tokens)
            } else {
                est_message(&m.message)
            };
            messages.push(m.message.clone());
        }
        Built {
            messages,
            est_tokens: tokens,
            pressure: if tokens > budget.available() {
                Pressure::Compact
            } else {
                Pressure::Ok
            },
            context: None,
            cleared,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stored(idx: u32, turn: u32, text: &str) -> StoredMessage {
        StoredMessage {
            idx,
            turn,
            message: Message::user(text),
            tokens: 0,
            untrusted: false,
            handle: None,
            meta: json!({}),
        }
    }

    #[test]
    fn the_window_starts_where_the_context_says() {
        let msgs = [stored(0, 0, "old"), stored(1, 1, "new")];
        let ctx = json!({"from_idx": 1});
        let view = TaskView {
            system: "rules",
            tools: &[],
            knowledge: None,
            messages: &msgs,
            context: &ctx,
            person: None,
            now_ms: 0,
            last_call: None,
            turn: 1,
        };
        let built = PlainContext.build(
            &view,
            TokenBudget {
                window: 100_000,
                reserve: 4_096,
            },
        );
        assert_eq!(built.messages.len(), 2);
        assert_eq!(built.messages[1].text(), "new");
        assert_eq!(built.cleared, vec![0]);
        assert_eq!(built.pressure, Pressure::Ok);
        let tight = PlainContext.build(
            &view,
            TokenBudget {
                window: 10,
                reserve: 8,
            },
        );
        assert_eq!(tight.pressure, Pressure::Compact);
    }
}
