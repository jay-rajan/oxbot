//! The gate: before any tool runs, the loop asks the hub
//! ([`EngineEvent::Permission`]) and obeys. A tool can only be run with a
//! [`Permit`], and only [`GateCheck::check`] makes one, so there is no way
//! from "the model asked for a tool" to "the tool ran" that skips oxbot's
//! check. A lost answer is a no.

use oxbot_engine::{Decision, EngineEvent, PermissionRequest, Reply};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Leave to run one tool call. Only the gate makes these.
#[derive(Debug)]
pub struct Permit {
    tool_use_id: String,
}

impl Permit {
    /// The call it's for.
    #[must_use]
    pub fn tool_use_id(&self) -> &str {
        &self.tool_use_id
    }
}

/// The gate's answer.
#[derive(Debug)]
pub enum Gated {
    /// Run it.
    Run(Permit),
    /// Don't; tell the model this.
    Denied(String),
    /// A practice run: don't run it; this is its result.
    Simulated(String),
    /// Asked quietly: it needs the person's OK, so it didn't run; what it
    /// would ask.
    WouldAsk(String),
}

/// Asks the hub about each tool call.
#[derive(Debug, Clone)]
pub struct GateCheck {
    events: mpsc::Sender<EngineEvent>,
}

impl GateCheck {
    /// A gate that asks through `events`.
    #[must_use]
    pub fn new(events: mpsc::Sender<EngineEvent>) -> Self {
        Self { events }
    }

    /// Asks whether `tool` may run with `input` for call `tool_use_id`.
    /// `None` if `cancel` fired first (the turn stops).
    pub async fn check(
        &self,
        tool: &str,
        input: &Value,
        tool_use_id: &str,
        cancel: &CancellationToken,
    ) -> Option<Gated> {
        self.ask(tool, input, tool_use_id, false, cancel).await
    }

    /// Like [`GateCheck::check`], but never asks the person: a call that
    /// would need their OK comes back [`Gated::WouldAsk`] (one step of a
    /// batch, which stops there).
    pub async fn check_quiet(
        &self,
        tool: &str,
        input: &Value,
        tool_use_id: &str,
        cancel: &CancellationToken,
    ) -> Option<Gated> {
        self.ask(tool, input, tool_use_id, true, cancel).await
    }

    async fn ask(
        &self,
        tool: &str,
        input: &Value,
        tool_use_id: &str,
        quiet: bool,
        cancel: &CancellationToken,
    ) -> Option<Gated> {
        let (reply, wait) = Reply::<Decision>::channel();
        let request = PermissionRequest {
            tool: tool.to_owned(),
            input: input.clone(),
            tool_use_id: tool_use_id.to_owned(),
            quiet,
            reply,
        };
        // A closed channel drops the request, and with it the reply: a no.
        let _ = self.events.send(EngineEvent::Permission(request)).await;
        let decision = tokio::select! {
            d = wait => d,
            () = cancel.cancelled() => return None,
        };
        Some(match decision {
            Decision::Allow => Gated::Run(Permit {
                tool_use_id: tool_use_id.to_owned(),
            }),
            Decision::Deny(why) => Gated::Denied(why),
            Decision::Simulate(text) => Gated::Simulated(text),
            // Only a quiet check may be told this; anything else is a no.
            Decision::WouldAsk(what) if quiet => Gated::WouldAsk(what),
            Decision::WouldAsk(what) => {
                Gated::Denied(format!("This needs the person's OK: {what}"))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn the_hubs_answer_decides() {
        let (tx, mut rx) = mpsc::channel(4);
        let gate = GateCheck::new(tx);
        let cancel = CancellationToken::new();
        let hub = tokio::spawn(async move {
            let mut n = 0;
            while let Some(EngineEvent::Permission(req)) = rx.recv().await {
                n += 1;
                match n {
                    1 => req.reply.send(Decision::Allow),
                    2 => req.reply.send(Decision::Deny("no".into())),
                    // Dropped unanswered.
                    _ => drop(req),
                }
            }
        });
        let input = json!({"uid": "1_2"});
        assert!(matches!(
            gate.check("mcp__chrome-devtools__click", &input, "c1", &cancel).await,
            Some(Gated::Run(p)) if p.tool_use_id() == "c1"
        ));
        assert!(matches!(
            gate.check("x", &input, "c2", &cancel).await,
            Some(Gated::Denied(why)) if why == "no"
        ));
        assert!(matches!(
            gate.check("x", &input, "c3", &cancel).await,
            Some(Gated::Denied(why)) if why == "oxbot couldn't check this"
        ));
        drop(gate);
        hub.await.unwrap();
    }

    #[tokio::test]
    async fn a_quiet_check_never_asks_and_hears_what_would_be_asked() {
        let (tx, mut rx) = mpsc::channel(4);
        let gate = GateCheck::new(tx);
        let cancel = CancellationToken::new();
        let hub = tokio::spawn(async move {
            while let Some(EngineEvent::Permission(req)) = rx.recv().await {
                let what = format!("quiet={}", req.quiet);
                req.reply.send(Decision::WouldAsk(what));
            }
        });
        let input = json!({"uid": "1_2"});
        assert!(matches!(
            gate.check_quiet("x", &input, "c1", &cancel).await,
            Some(Gated::WouldAsk(what)) if what == "quiet=true"
        ));
        // A loud check told "would ask" is a no.
        assert!(matches!(
            gate.check("x", &input, "c2", &cancel).await,
            Some(Gated::Denied(why)) if why.contains("quiet=false")
        ));
        drop(gate);
        hub.await.unwrap();
    }

    #[tokio::test]
    async fn no_hub_is_a_no_and_cancel_stops_waiting() {
        let (tx, rx) = mpsc::channel(4);
        drop(rx);
        let cancel = CancellationToken::new();
        assert!(matches!(
            GateCheck::new(tx)
                .check("x", &json!({}), "c", &cancel)
                .await,
            Some(Gated::Denied(_))
        ));
        let (tx, _rx) = mpsc::channel(4);
        cancel.cancel();
        assert!(
            GateCheck::new(tx)
                .check("x", &json!({}), "c", &cancel)
                .await
                .is_none()
        );
    }
}
