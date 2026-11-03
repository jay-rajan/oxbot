//! One-shot answers from the hub to an engine, which fail closed: a
//! [`Reply`] dropped unanswered sends its [`Refusal`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::sync::oneshot;

/// The answer an engine gets when nobody answered.
pub trait Refusal {
    /// The "no" for a dropped or lost answer.
    fn refusal() -> Self;
}

/// The gate's answer to a [`crate::PermissionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run the tool as asked.
    Allow,
    /// Don't; the bot is told this.
    Deny(String),
    /// A practice run: don't run it, and tell the model this text as its
    /// result.
    Simulate(String),
}

impl Refusal for Decision {
    fn refusal() -> Self {
        Self::Deny("oxbot couldn't check this".to_owned())
    }
}

/// The result of one of oxbot's own tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolReply {
    /// Whether it worked.
    pub ok: bool,
    /// What to tell the bot.
    pub text: String,
}

impl Refusal for ToolReply {
    fn refusal() -> Self {
        Self {
            ok: false,
            text: "oxbot couldn't run this".to_owned(),
        }
    }
}

/// Where one answer goes. Answer it with [`Reply::send`]; dropping it
/// unanswered sends `T`'s [`Refusal`], so a lost answer is always a no.
pub struct Reply<T: Refusal>(Option<oneshot::Sender<T>>);

impl<T: Refusal> Reply<T> {
    /// A reply and what the engine awaits for it.
    #[must_use]
    pub fn channel() -> (Self, ReplyWait<T>) {
        let (tx, rx) = oneshot::channel();
        (Self(Some(tx)), ReplyWait(rx))
    }

    /// Answers. Nothing happens if the engine stopped waiting.
    pub fn send(mut self, answer: T) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(answer);
        }
    }

    /// Whether the engine stopped waiting (its turn was stopped, say).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.as_ref().is_none_or(oneshot::Sender::is_closed)
    }
}

impl<T: Refusal> Drop for Reply<T> {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(T::refusal());
        }
    }
}

impl<T: Refusal> fmt::Debug for Reply<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "Reply(waiting)"
        } else {
            "Reply(sent)"
        })
    }
}

/// The engine's side of a [`Reply`]: resolves to the answer, or to the
/// refusal if the reply was dropped.
#[derive(Debug)]
pub struct ReplyWait<T>(oneshot::Receiver<T>);

impl<T: Refusal> Future for ReplyWait<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        Pin::new(&mut self.0)
            .poll(cx)
            .map(|answer| answer.unwrap_or_else(|_| T::refusal()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_arrives() {
        let (reply, wait) = Reply::<Decision>::channel();
        assert!(!reply.is_closed());
        reply.send(Decision::Allow);
        assert_eq!(wait.await, Decision::Allow);
    }

    #[tokio::test]
    async fn a_dropped_reply_is_a_no() {
        let (reply, wait) = Reply::<Decision>::channel();
        drop(reply);
        assert_eq!(
            wait.await,
            Decision::Deny("oxbot couldn't check this".into())
        );
        let (reply, wait) = Reply::<ToolReply>::channel();
        drop(reply);
        assert_eq!(
            wait.await,
            ToolReply {
                ok: false,
                text: "oxbot couldn't run this".into()
            }
        );
    }

    #[tokio::test]
    async fn a_reply_dropped_inside_a_task_is_a_no() {
        let (reply, wait) = Reply::<Decision>::channel();
        tokio::spawn(async move {
            let _held = reply;
        })
        .await
        .unwrap();
        assert!(matches!(wait.await, Decision::Deny(_)));
    }

    #[test]
    fn a_reply_knows_when_nobody_waits() {
        let (reply, wait) = Reply::<ToolReply>::channel();
        drop(wait);
        assert!(reply.is_closed());
        // Answering then is fine.
        reply.send(ToolReply {
            ok: true,
            text: "done".into(),
        });
    }
}
