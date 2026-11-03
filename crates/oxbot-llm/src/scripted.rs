//! A scripted [`ChatModel`] for tests (feature `test-model`): answers come
//! from a queue (then an optional responder), and every request is kept so
//! tests can check what was sent (e.g. `data_collection: "deny"`).

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::client::ChatModel;
use crate::error::{ErrorKind, LlmError};
use crate::types::{CallUsage, ChatDelta, ChatRequest, FinishReason, Provider, ToolCall};

/// One scripted answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// An answer: text and tool calls (empty ids are filled in as
    /// `call_<n>`).
    Answer {
        /// The text.
        text: String,
        /// Tool calls.
        tool_calls: Vec<ToolCall>,
        /// Usage reported in the last piece.
        usage: CallUsage,
        /// The model named as having answered (default: the one asked).
        model: Option<String>,
    },
    /// The request fails before any answer.
    Fail(LlmError),
    /// Some text arrives, then the request fails (a mid-stream error).
    FailAfter {
        /// Text sent before the error.
        text: String,
        /// The error.
        error: LlmError,
    },
    /// Nothing arrives until the request is cancelled.
    Hang,
}

impl Step {
    /// A plain text answer.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Answer {
            text: text.into(),
            tool_calls: Vec::new(),
            usage: CallUsage::default(),
            model: None,
        }
    }

    /// One tool call, without text.
    #[must_use]
    pub fn tool(name: impl Into<String>, arguments: Value) -> Self {
        Self::tools(vec![ToolCall::new("", name, arguments)])
    }

    /// Several tool calls, without text.
    #[must_use]
    pub fn tools(tool_calls: Vec<ToolCall>) -> Self {
        Self::Answer {
            text: String::new(),
            tool_calls,
            usage: CallUsage::default(),
            model: None,
        }
    }

    /// A tool call whose arguments text isn't valid JSON.
    #[must_use]
    pub fn broken_tool(name: impl Into<String>, raw: impl Into<String>) -> Self {
        Self::tools(vec![ToolCall::from_raw("", name, raw)])
    }

    /// A failure of this kind.
    #[must_use]
    pub fn error(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Fail(LlmError::new(kind, message))
    }

    /// The same answer with this usage.
    #[must_use]
    pub fn with_usage(mut self, new: CallUsage) -> Self {
        if let Self::Answer { usage, .. } = &mut self {
            *usage = new;
        }
        self
    }

    /// The same answer, named as coming from `model`.
    #[must_use]
    pub fn from_model(mut self, name: impl Into<String>) -> Self {
        if let Self::Answer { model, .. } = &mut self {
            *model = Some(name.into());
        }
        self
    }
}

type Responder = Box<dyn Fn(&ChatRequest) -> Step + Send + Sync>;

/// A model that answers from a script.
pub struct ScriptedModel {
    provider: Provider,
    steps: Mutex<VecDeque<Step>>,
    responder: Option<Responder>,
    requests: Mutex<Vec<ChatRequest>>,
    calls: AtomicU64,
}

impl fmt::Debug for ScriptedModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedModel")
            .field("provider", &self.provider)
            .field("remaining", &self.remaining())
            .finish_non_exhaustive()
    }
}

impl Default for ScriptedModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptedModel {
    /// An OpenRouter-like model with an empty script.
    #[must_use]
    pub fn new() -> Self {
        Self {
            provider: Provider::OpenRouter,
            steps: Mutex::new(VecDeque::new()),
            responder: None,
            requests: Mutex::new(Vec::new()),
            calls: AtomicU64::new(0),
        }
    }

    /// The same, reporting this provider.
    #[must_use]
    pub fn with_provider(mut self, provider: Provider) -> Self {
        self.provider = provider;
        self
    }

    /// Answers with `responder` once the queue is empty.
    #[must_use]
    pub fn with_responder(
        mut self,
        responder: impl Fn(&ChatRequest) -> Step + Send + Sync + 'static,
    ) -> Self {
        self.responder = Some(Box::new(responder));
        self
    }

    /// Adds an answer to the queue (builder style).
    #[must_use]
    pub fn then(self, step: Step) -> Self {
        self.push(step);
        self
    }

    /// Adds an answer to the queue.
    pub fn push(&self, step: Step) {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(step);
    }

    /// Answers left in the queue.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Every request so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn next_step(&self, req: &ChatRequest) -> Step {
        let queued = self
            .steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        queued
            .or_else(|| self.responder.as_ref().map(|r| r(req)))
            .unwrap_or_else(|| {
                Step::error(
                    ErrorKind::Protocol,
                    "The scripted model has no more answers.",
                )
            })
    }
}

/// Splits text in two at a character boundary, to exercise assembly.
fn halves(text: &str) -> (&str, &str) {
    let mid = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(text.len(), |(i, _)| i);
    text.split_at(mid)
}

fn text_deltas(text: &str, out: &mut Vec<Result<ChatDelta, LlmError>>) {
    let (a, b) = halves(text);
    for part in [a, b] {
        if !part.is_empty() {
            out.push(Ok(ChatDelta::Text(part.to_owned())));
        }
    }
}

impl ChatModel for ScriptedModel {
    fn provider(&self) -> Provider {
        self.provider
    }

    fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ChatDelta, LlmError>> {
        let step = self.next_step(&req);
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        let asked = req.model.clone();
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(req);
        if cancel.is_cancelled() {
            return stream::iter([Err(LlmError::cancelled())]).boxed();
        }
        let mut out = Vec::new();
        match step {
            Step::Answer {
                text,
                tool_calls,
                usage,
                model,
            } => {
                text_deltas(&text, &mut out);
                let finish = if tool_calls.is_empty() {
                    FinishReason::Stop
                } else {
                    FinishReason::ToolCalls
                };
                for (i, call) in tool_calls.into_iter().enumerate() {
                    let index = u32::try_from(i).unwrap_or(u32::MAX);
                    let id = if call.id.is_empty() {
                        format!("call_{n}_{i}")
                    } else {
                        call.id
                    };
                    let (a, b) = halves(&call.raw);
                    out.push(Ok(ChatDelta::ToolCallPart {
                        index,
                        id: Some(id),
                        name: Some(call.name),
                        args: a.to_owned(),
                    }));
                    out.push(Ok(ChatDelta::ToolCallPart {
                        index,
                        id: None,
                        name: None,
                        args: b.to_owned(),
                    }));
                }
                out.push(Ok(ChatDelta::Done {
                    model: model.unwrap_or(asked),
                    generation_id: Some(format!("gen-scripted-{n}")),
                    finish,
                    usage,
                }));
            }
            Step::Fail(err) => out.push(Err(err)),
            Step::FailAfter { text, error } => {
                text_deltas(&text, &mut out);
                out.push(Err(error.partial(true)));
            }
            Step::Hang => {
                return stream::once(async move {
                    cancel.cancelled().await;
                    Err(LlmError::cancelled())
                })
                .boxed();
            }
        }
        stream::iter(out).boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::complete;
    use crate::openrouter::ProviderPrefs;
    use crate::types::Message;
    use serde_json::json;

    fn req(model: &str) -> ChatRequest {
        let mut r = ChatRequest::new(model, vec![Message::user("hi")]);
        r.provider = Some(ProviderPrefs::private_bot());
        r
    }

    #[tokio::test]
    async fn answers_in_order_and_records_requests() {
        let model = ScriptedModel::new()
            .then(Step::tool("Read", json!({"file_path": "a.txt"})))
            .then(Step::text("All done.").from_model("other/model:free"));
        let first = complete(&model, req("a/m:free"), CancellationToken::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(first.message.tool_calls[0].name, "Read");
        assert_eq!(
            first.message.tool_calls[0].arguments,
            json!({"file_path": "a.txt"})
        );
        assert_eq!(first.message.tool_calls[0].id, "call_0_0");
        assert_eq!(first.finish, FinishReason::ToolCalls);
        assert_eq!(first.model, "a/m:free");

        let mut seen = String::new();
        let second = complete(&model, req("a/m:free"), CancellationToken::new(), |d| {
            if let ChatDelta::Text(t) = d {
                seen.push_str(t);
            }
        })
        .await
        .unwrap();
        assert_eq!(seen, "All done.");
        assert_eq!(second.model, "other/model:free");

        let sent = model.requests();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].provider.as_ref().unwrap().denies_data_collection());
        let none_left = complete(&model, req("a/m:free"), CancellationToken::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(none_left.kind, ErrorKind::Protocol);
    }

    #[tokio::test]
    async fn responder_answers_after_the_queue() {
        let model = ScriptedModel::new().with_responder(|r| {
            if r.model == "busy:free" {
                Step::error(ErrorKind::RateLimited, "busy")
            } else {
                Step::text("ok")
            }
        });
        let busy = complete(&model, req("busy:free"), CancellationToken::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(busy.kind, ErrorKind::RateLimited);
        let ok = complete(&model, req("fine:free"), CancellationToken::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(ok.message.text(), "ok");
    }

    #[tokio::test]
    async fn mid_stream_failure_and_broken_calls() {
        let model = ScriptedModel::new()
            .then(Step::FailAfter {
                text: "partial".into(),
                error: LlmError::new(ErrorKind::Overloaded, "x"),
            })
            .then(Step::broken_tool("click", "{oops"));
        let err = complete(&model, req("m"), CancellationToken::new(), |_| {})
            .await
            .unwrap_err();
        assert!(err.partial);
        let broken = complete(&model, req("m"), CancellationToken::new(), |_| {})
            .await
            .unwrap();
        assert!(broken.message.tool_calls[0].arguments_error().is_some());
        assert_eq!(broken.message.tool_calls[0].raw, "{oops");
    }

    #[tokio::test]
    async fn hang_waits_for_cancel() {
        let model = ScriptedModel::new().then(Step::Hang);
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            stop.cancel();
        });
        let err = complete(&model, req("m"), cancel, |_| {})
            .await
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Cancelled);
    }
}
