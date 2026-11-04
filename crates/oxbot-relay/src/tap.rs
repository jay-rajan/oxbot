//! Reading a relayed answer as it passes back to Codex, for the meter: the
//! model that answered, its id, the tokens and the cost from the final
//! `response.completed` (or `response.incomplete`), or how it failed. The
//! bytes themselves go on unchanged.

use oxbot_llm::sse::{SseEvent, SseParser};
use oxbot_llm::{CallUsage, ErrorKind, LlmError};
use oxbot_loop::gateway::AnswerUsage;
use serde_json::Value;

/// How an answer ended, as far as the stream said.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Ending {
    /// Still coming.
    #[default]
    Open,
    /// Finished, with what to meter.
    Done(AnswerUsage),
    /// The provider said it failed.
    Failed(String),
}

/// Watches one answer.
#[derive(Debug)]
pub struct Tap {
    sse: SseParser,
    ending: Ending,
}

impl Default for Tap {
    fn default() -> Self {
        Self {
            sse: SseParser::new(),
            ending: Ending::Open,
        }
    }
}

impl Tap {
    /// Reads one chunk of a streamed answer.
    pub fn push(&mut self, bytes: &[u8]) {
        let Ok(events) = self.sse.push(bytes) else {
            return;
        };
        for event in events {
            if let SseEvent::Data(data) = event
                && let Ok(v) = serde_json::from_str::<Value>(&data)
            {
                self.event(&v);
            }
        }
    }

    fn event(&mut self, v: &Value) {
        match v.get("type").and_then(Value::as_str) {
            Some("response.completed" | "response.incomplete") => {
                self.ending = Ending::Done(usage_of(&v["response"]));
            }
            Some("response.failed") => {
                let why = v["response"]["error"]["message"]
                    .as_str()
                    .unwrap_or("the model's answer failed")
                    .to_owned();
                self.ending = Ending::Failed(why);
            }
            Some("error") => {
                let why = v["message"].as_str().unwrap_or("stream error").to_owned();
                self.ending = Ending::Failed(why);
            }
            _ => {}
        }
    }

    /// Reads a whole JSON answer (not streamed).
    pub fn whole(&mut self, bytes: &[u8]) {
        if let Ok(v) = serde_json::from_slice::<Value>(bytes) {
            self.ending = if v.get("error").is_some_and(|e| !e.is_null()) {
                Ending::Failed(
                    v["error"]["message"]
                        .as_str()
                        .unwrap_or("the model's answer failed")
                        .to_owned(),
                )
            } else {
                Ending::Done(usage_of(&v))
            };
        }
    }

    /// What to meter once the body ended (`cut`: it stopped early).
    ///
    /// # Errors
    /// The answer failed, or stopped before it was complete.
    pub fn result(&self, cut: Option<LlmError>) -> Result<AnswerUsage, LlmError> {
        match &self.ending {
            Ending::Done(usage) => Ok(usage.clone()),
            Ending::Failed(why) => Err(LlmError::new(ErrorKind::Server, why.clone())),
            Ending::Open => Err(cut.unwrap_or_else(|| {
                LlmError::new(
                    ErrorKind::Protocol,
                    "The model's answer stopped before it was complete.",
                )
            })),
        }
    }
}

/// The meter's view of a response object.
#[allow(clippy::cast_sign_loss)]
fn usage_of(response: &Value) -> AnswerUsage {
    let u = &response["usage"];
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    AnswerUsage {
        model: response["model"].as_str().unwrap_or_default().to_owned(),
        generation_id: response["id"].as_str().map(str::to_owned),
        usage: CallUsage {
            prompt_tokens: n(&u["input_tokens"]),
            completion_tokens: n(&u["output_tokens"]),
            cached_tokens: n(&u["input_tokens_details"]["cached_tokens"]),
            reasoning_tokens: n(&u["output_tokens_details"]["reasoning_tokens"]),
            cost_usd: u["cost"].as_f64(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_final_event_gives_the_usage_however_it_is_split() {
        let stream = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
            event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n\
            event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"gen-1\",\
            \"model\":\"e2e/tool-model:free\",\"usage\":{\"input_tokens\":120,\"output_tokens\":7,\
            \"input_tokens_details\":{\"cached_tokens\":100},\"cost\":0.25}}}\n\n";
        for split in [1, 7, 50, stream.len()] {
            let mut tap = Tap::default();
            for chunk in stream.as_bytes().chunks(split) {
                tap.push(chunk);
            }
            let got = tap.result(None).unwrap();
            assert_eq!(got.model, "e2e/tool-model:free");
            assert_eq!(got.generation_id.as_deref(), Some("gen-1"));
            assert_eq!(got.usage.prompt_tokens, 120);
            assert_eq!(got.usage.completion_tokens, 7);
            assert_eq!(got.usage.cached_tokens, 100);
            assert_eq!(got.usage.cost_usd, Some(0.25));
        }
    }

    #[test]
    fn a_cut_or_failed_stream_is_an_error() {
        let mut tap = Tap::default();
        tap.push(b"data: {\"type\":\"response.created\"}\n\n");
        assert_eq!(
            tap.result(Some(LlmError::cancelled())).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        tap.push(b"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"no\"}}}\n\n");
        assert_eq!(tap.result(None).unwrap_err().message, "no");
        let mut whole = Tap::default();
        whole.whole(br#"{"id":"g","model":"m","usage":{"input_tokens":3,"output_tokens":1}}"#);
        assert_eq!(whole.result(None).unwrap().usage.prompt_tokens, 3);
    }
}
