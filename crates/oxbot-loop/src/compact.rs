//! The compactor: makes room when the conversation outgrows the model's
//! window. #18 ships [`DropOldTurns`] (the window starts after the oldest
//! whole turns); #19 replaces it through [`Compactor`]. Any model call a
//! compactor makes goes through a [`ModelCaller`]: routed, paced, metered
//! and with the session's privacy, like every other request.

use futures_util::future::BoxFuture;
use oxbot_engine::CallKind;
use oxbot_llm::{LlmError, Message};
use serde_json::{Value, json};

use crate::budget::{est, est_message, est_tools};
use crate::context::{TaskView, TokenBudget, window_start};

/// Makes model requests on a session's behalf.
pub trait ModelCaller: Send + Sync {
    /// Sends `messages` (no tools) and returns the answer's text.
    fn call(
        &self,
        kind: CallKind,
        messages: Vec<Message>,
        max_tokens: u32,
    ) -> BoxFuture<'_, Result<String, LlmError>>;
}

/// What a compaction changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Compaction {
    /// The builder's new state (saved with the step).
    pub context: Value,
    /// Whether what was left out included outside text: the session's
    /// taint stays either way.
    pub untrusted: bool,
    /// Messages left out by this compaction.
    pub dropped: u32,
}

/// Makes room in a conversation.
pub trait Compactor: Send + Sync + std::fmt::Debug {
    /// The builder state after making room.
    fn compact<'a>(
        &'a self,
        view: &'a TaskView<'a>,
        budget: TokenBudget,
        call: &'a dyn ModelCaller,
    ) -> BoxFuture<'a, Result<Compaction, LlmError>>;
}

/// Drops the oldest whole turns (never the current one) until the rest
/// fits in 60% of the budget.
#[derive(Debug, Clone, Copy, Default)]
pub struct DropOldTurns;

impl Compactor for DropOldTurns {
    fn compact<'a>(
        &'a self,
        view: &'a TaskView<'a>,
        budget: TokenBudget,
        _call: &'a dyn ModelCaller,
    ) -> BoxFuture<'a, Result<Compaction, LlmError>> {
        let from = window_start(view.context);
        let fixed =
            est(view.system) + est_tools(view.tools) + view.knowledge.map_or(0, |k| est(&k.text));
        let target = budget.available() * 6 / 10;
        let window: Vec<_> = view.messages.iter().filter(|m| m.idx >= from).collect();
        let size = |m: &&crate::store::StoredMessage| {
            if m.tokens > 0 {
                u64::from(m.tokens)
            } else {
                est_message(&m.message)
            }
        };
        let mut total: u64 = fixed + window.iter().map(size).sum::<u64>();
        let mut start = from;
        let mut untrusted = view
            .context
            .get("dropped_untrusted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut dropped = 0;
        let mut i = 0;
        while total > target && i < window.len() {
            let turn = window[i].turn;
            if turn >= view.turn {
                break;
            }
            while i < window.len() && window[i].turn == turn {
                total = total.saturating_sub(size(&window[i]));
                untrusted |= window[i].untrusted;
                dropped += 1;
                i += 1;
            }
            start = window.get(i).map_or(start, |m| m.idx);
        }
        let mut context = view.context.clone();
        if !context.is_object() {
            context = json!({});
        }
        context["from_idx"] = json!(start);
        if untrusted {
            context["dropped_untrusted"] = json!(true);
        }
        Box::pin(async move {
            Ok(Compaction {
                context,
                untrusted,
                dropped,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoredMessage;

    struct NoCalls;
    impl ModelCaller for NoCalls {
        fn call(
            &self,
            _: CallKind,
            _: Vec<Message>,
            _: u32,
        ) -> BoxFuture<'_, Result<String, LlmError>> {
            Box::pin(async { Err(LlmError::cancelled()) })
        }
    }

    fn msg(idx: u32, turn: u32, untrusted: bool) -> StoredMessage {
        StoredMessage {
            idx,
            turn,
            message: Message::user("x".repeat(3500)),
            tokens: 0,
            untrusted,
            handle: None,
            meta: json!({}),
        }
    }

    #[tokio::test]
    async fn drops_old_turns_but_not_the_current_one() {
        let msgs = [
            msg(0, 0, true),
            msg(1, 0, false),
            msg(2, 1, false),
            msg(3, 2, false),
        ];
        let ctx = json!({});
        let view = TaskView {
            system: "rules",
            tools: &[],
            knowledge: None,
            messages: &msgs,
            context: &ctx,
            person: None,
            now_ms: 0,
            last_call: None,
            turn: 2,
        };
        let budget = TokenBudget {
            window: 3_000,
            reserve: 0,
        };
        let c = DropOldTurns.compact(&view, budget, &NoCalls).await.unwrap();
        assert_eq!(c.context["from_idx"], 3);
        assert!(c.untrusted);
        assert_eq!(c.context["dropped_untrusted"], true);
        assert_eq!(c.dropped, 3);
    }
}
