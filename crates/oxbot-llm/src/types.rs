//! Provider-neutral chat types: messages, tool calls, requests, the pieces
//! of a streamed answer, and token usage.
//!
//! These are oxbot's own shapes. The client turns them into the
//! OpenAI-style wire format when it sends a request; history is stored in
//! this shape (see [`Message::to_stored`]) so a change of provider never
//! rewrites it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::openrouter::ProviderPrefs;

/// Which kind of service answers a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// OpenRouter (`https://openrouter.ai/api/v1`).
    OpenRouter,
    /// An OpenAI-compatible server on this Mac (Ollama, LM Studio), on
    /// loopback only.
    Local,
}

impl Provider {
    /// The name stored with each model call, e.g. `openrouter`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Local => "local",
        }
    }
}

/// Who wrote a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The bot's rules: the stable prefix.
    System,
    /// A person (or another bot relaying a person's request).
    User,
    /// The model.
    Assistant,
    /// A tool's result, answering one of the model's tool calls.
    Tool,
}

impl Role {
    /// The wire and storage name, e.g. `assistant`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

/// One piece of a message's content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "lowercase")]
pub enum Part {
    /// Plain text.
    Text(String),
    /// An image, e.g. a screenshot, for models that take images.
    Image {
        /// Its media type, e.g. `image/png`.
        mime: String,
        /// The image bytes, base64 (standard alphabet, padded).
        base64: String,
    },
}

/// One chat message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who wrote it.
    pub role: Role,
    /// Its content. Empty for an assistant message that only calls tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// Tools the model asked to run, in order (assistant messages only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// The call this result answers (tool messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// The storage version written by [`Message::to_stored`].
pub const STORED_VERSION: u32 = 1;

#[derive(Serialize)]
struct StoredRef<'a> {
    v: u32,
    #[serde(flatten)]
    message: &'a Message,
}

#[derive(Deserialize)]
struct StoredOwned {
    v: u32,
    #[serde(flatten)]
    message: Message,
}

impl Message {
    /// A system message.
    #[must_use]
    pub fn system(text: impl Into<String>) -> Self {
        Self::with_text(Role::System, text)
    }

    /// A user message.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self::with_text(Role::User, text)
    }

    /// An assistant message: its text (may be empty) and tool calls.
    #[must_use]
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        let text = text.into();
        Self {
            role: Role::Assistant,
            parts: if text.is_empty() {
                Vec::new()
            } else {
                vec![Part::Text(text)]
            },
            tool_calls,
            tool_call_id: None,
        }
    }

    /// A tool result answering the call `tool_call_id`.
    #[must_use]
    pub fn tool(tool_call_id: impl Into<String>, parts: Vec<Part>) -> Self {
        Self {
            role: Role::Tool,
            parts,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    fn with_text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            parts: vec![Part::Text(text.into())],
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// All text parts, joined with blank lines (images left out).
    #[must_use]
    pub fn text(&self) -> String {
        let texts: Vec<&str> = self
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                Part::Image { .. } => None,
            })
            .collect();
        texts.join("\n\n")
    }

    /// Whether any part is an image.
    #[must_use]
    pub fn has_images(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::Image { .. }))
    }

    /// The message as stored JSON: oxbot's shape with `"v": 1`.
    ///
    /// # Errors
    /// Only if serde fails, which these types don't do in practice.
    pub fn to_stored(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&StoredRef {
            v: STORED_VERSION,
            message: self,
        })
    }

    /// Reads a message written by [`Message::to_stored`].
    ///
    /// # Errors
    /// The JSON is malformed or has a version this build doesn't know.
    pub fn from_stored(json: &str) -> Result<Self, StoredMessageError> {
        let stored: StoredOwned = serde_json::from_str(json)?;
        if stored.v != STORED_VERSION {
            return Err(StoredMessageError::Version(stored.v));
        }
        Ok(stored.message)
    }
}

/// Why a stored message couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum StoredMessageError {
    /// Not valid JSON for a message.
    #[error("stored message is malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// Written by a newer oxbot.
    #[error("stored message has unknown version {0}")]
    Version(u32),
}

/// A tool the model asked to run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The model's id for the call; the result answers it.
    pub id: String,
    /// The tool's name.
    pub name: String,
    /// The arguments, parsed. `Value::Null` when `raw` isn't a JSON object
    /// (see [`ToolCall::arguments_error`]).
    pub arguments: Value,
    /// The model's exact arguments text. Kept so a parse failure can be
    /// shown back to it, and resent byte for byte in later requests (a
    /// stable prefix lets providers reuse their cache).
    pub raw: String,
}

impl ToolCall {
    /// A call from the model's raw arguments text. Empty text means no
    /// arguments (`{}`), as some models send for tools without any.
    #[must_use]
    pub fn from_raw(
        id: impl Into<String>,
        name: impl Into<String>,
        raw: impl Into<String>,
    ) -> Self {
        let raw = raw.into();
        let arguments = parse_arguments(&raw).unwrap_or(Value::Null);
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            raw,
        }
    }

    /// A call with arguments already parsed (tests, scripted models).
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        let raw = arguments.to_string();
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            raw,
        }
    }

    /// Why the arguments couldn't be read, if they couldn't.
    #[must_use]
    pub fn arguments_error(&self) -> Option<String> {
        parse_arguments(&self.raw).err()
    }
}

/// Parses a tool call's arguments text into a JSON object.
///
/// # Errors
/// The text isn't JSON, or is JSON but not an object.
pub fn parse_arguments(raw: &str) -> Result<Value, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err("the arguments must be a JSON object".to_owned()),
        Err(err) => Err(format!("the arguments aren't valid JSON: {err}")),
    }
}

/// A tool the model may call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Its name, e.g. `mcp__chrome-devtools__click`.
    pub name: String,
    /// What it does, for the model.
    pub description: String,
    /// JSON Schema of its arguments (an object schema).
    pub parameters: Value,
}

/// Whether and which tool the model must call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ToolChoice {
    /// The model decides.
    #[default]
    Auto,
    /// No tools this time.
    None,
    /// It must call some tool.
    Required,
    /// It must call this tool.
    Tool(String),
}

/// One request to a chat model. `Default` leaves every option unset; use
/// [`ChatRequest::new`] for oxbot's defaults.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatRequest {
    /// The model to ask first.
    pub model: String,
    /// Models OpenRouter may try next on its side (its `models` list).
    /// Ignored by local servers.
    pub fallbacks: Vec<String>,
    /// The conversation, system message first.
    pub messages: Vec<Message>,
    /// Tools the model may call, in a fixed order.
    pub tools: Vec<ToolSpec>,
    /// Tool choice (ignored when there are no tools).
    pub tool_choice: ToolChoice,
    /// `Some(false)`: at most one tool call per answer.
    pub parallel_tool_calls: Option<bool>,
    /// Longest answer, in tokens.
    pub max_tokens: Option<u32>,
    /// Sampling temperature (`None`: the model's default).
    pub temperature: Option<f32>,
    /// Sampling seed, for repeatable runs where the model supports it.
    pub seed: Option<u64>,
    /// OpenRouter's provider routing (privacy, order). `None` for local.
    pub provider: Option<ProviderPrefs>,
}

impl ChatRequest {
    /// A request with oxbot's defaults: tools chosen by the model, one tool
    /// call per answer, no fallbacks, no provider preferences.
    #[must_use]
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            fallbacks: Vec::new(),
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: Some(false),
            max_tokens: None,
            temperature: None,
            seed: None,
            provider: None,
        }
    }
}

/// Why the model stopped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum FinishReason {
    /// It finished its answer.
    Stop,
    /// It wants tools run.
    ToolCalls,
    /// It hit the token limit.
    Length,
    /// The provider's filter stopped it.
    ContentFilter,
    /// An error ended the answer.
    Error,
    /// Something else, as the provider named it.
    Other(String),
}

impl FinishReason {
    /// The wire name, e.g. `tool_calls`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stop => "stop",
            Self::ToolCalls => "tool_calls",
            Self::Length => "length",
            Self::ContentFilter => "content_filter",
            Self::Error => "error",
            Self::Other(s) => s,
        }
    }
}

impl From<&str> for FinishReason {
    fn from(s: &str) -> Self {
        match s {
            "stop" | "end_turn" => Self::Stop,
            "tool_calls" | "function_call" | "tool_use" => Self::ToolCalls,
            "length" | "max_tokens" => Self::Length,
            "content_filter" => Self::ContentFilter,
            "error" => Self::Error,
            other => Self::Other(other.to_owned()),
        }
    }
}

impl From<String> for FinishReason {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<FinishReason> for String {
    fn from(f: FinishReason) -> Self {
        f.as_str().to_owned()
    }
}

/// Tokens and cost of one model request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CallUsage {
    /// Input tokens, cached ones included.
    pub prompt_tokens: u64,
    /// Output tokens, reasoning included.
    pub completion_tokens: u64,
    /// Input tokens read from the provider's cache.
    pub cached_tokens: u64,
    /// Output tokens spent on reasoning.
    pub reasoning_tokens: u64,
    /// What the request cost in US dollars, when the service says
    /// (OpenRouter does; local servers don't).
    pub cost_usd: Option<f64>,
}

/// One piece of a streamed answer.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatDelta {
    /// More answer text.
    Text(String),
    /// Part of a tool call. Parts with the same `index` belong together:
    /// `id` and `name` come once, `args` text is appended.
    ToolCallPart {
        /// Which call in this answer.
        index: u32,
        /// The call's id, when this part carries it.
        id: Option<String>,
        /// The tool's name, when this part carries it.
        name: Option<String>,
        /// More arguments text.
        args: String,
    },
    /// The answer is complete. Always the last item of a successful stream.
    Done {
        /// The model that answered (may differ from the one asked, after
        /// OpenRouter's fallbacks).
        model: String,
        /// OpenRouter's generation id, for looking up the request later.
        generation_id: Option<String>,
        /// Why it stopped.
        finish: FinishReason,
        /// Tokens and cost.
        usage: CallUsage,
    },
}

/// A complete answer, put together from [`ChatDelta`]s.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatResponse {
    /// The assistant message: text and tool calls.
    pub message: Message,
    /// The model that answered.
    pub model: String,
    /// OpenRouter's generation id.
    pub generation_id: Option<String>,
    /// Why it stopped.
    pub finish: FinishReason,
    /// Tokens and cost.
    pub usage: CallUsage,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

/// Puts a streamed answer together: text appended, tool calls assembled by
/// index, arguments parsed at the end.
#[derive(Default)]
pub struct ResponseBuilder {
    text: String,
    calls: BTreeMap<u32, PartialCall>,
    done: Option<(String, Option<String>, FinishReason, CallUsage)>,
}

impl ResponseBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one piece.
    pub fn push(&mut self, delta: ChatDelta) {
        match delta {
            ChatDelta::Text(t) => self.text.push_str(&t),
            ChatDelta::ToolCallPart {
                index,
                id,
                name,
                args,
            } => {
                let call = self.calls.entry(index).or_default();
                if let Some(id) = id.filter(|i| !i.is_empty()) {
                    call.id = id;
                }
                if let Some(name) = name.filter(|n| !n.is_empty()) {
                    call.name = name;
                }
                call.args.push_str(&args);
            }
            ChatDelta::Done {
                model,
                generation_id,
                finish,
                usage,
            } => {
                self.done = Some((model, generation_id, finish, usage));
            }
        }
    }

    /// The text so far.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The answer, or `None` if the stream never said it was done.
    /// Calls without an id get `call_<index>` (some local servers omit it).
    #[must_use]
    pub fn finish(self) -> Option<ChatResponse> {
        let (model, generation_id, finish, usage) = self.done?;
        let tool_calls = self
            .calls
            .into_iter()
            .map(|(index, c)| {
                let id = if c.id.is_empty() {
                    format!("call_{index}")
                } else {
                    c.id
                };
                ToolCall::from_raw(id, c.name, c.args)
            })
            .collect();
        Some(ChatResponse {
            message: Message::assistant(self.text, tool_calls),
            model,
            generation_id,
            finish,
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stored_messages_round_trip_with_a_version() {
        let calls = vec![ToolCall::new(
            "c1",
            "Read",
            json!({"file_path": "notes.txt"}),
        )];
        let messages = [
            Message::system("rules"),
            Message::user("hello"),
            Message::assistant("", calls),
            Message::tool(
                "c1",
                vec![
                    Part::Text("done".into()),
                    Part::Image {
                        mime: "image/png".into(),
                        base64: "AAAA".into(),
                    },
                ],
            ),
        ];
        for m in &messages {
            let json = m.to_stored().unwrap();
            assert!(json.starts_with("{\"v\":1,"), "{json}");
            assert_eq!(&Message::from_stored(&json).unwrap(), m);
        }
    }

    #[test]
    fn stored_message_of_unknown_version_is_refused() {
        let err = Message::from_stored(r#"{"v":2,"role":"user"}"#).unwrap_err();
        assert!(matches!(err, StoredMessageError::Version(2)));
        assert!(Message::from_stored(r#"{"role":"user"}"#).is_err());
    }

    #[test]
    fn assistant_without_text_has_no_parts() {
        let m = Message::assistant("", Vec::new());
        assert_eq!(m.parts, Vec::<Part>::new());
        assert_eq!(m.text(), "");
    }

    #[test]
    fn text_joins_text_parts_and_skips_images() {
        let m = Message::tool(
            "x",
            vec![
                Part::Text("a".into()),
                Part::Image {
                    mime: "image/png".into(),
                    base64: String::new(),
                },
                Part::Text("b".into()),
            ],
        );
        assert_eq!(m.text(), "a\n\nb");
        assert!(m.has_images());
    }

    #[test]
    fn tool_call_arguments_are_parsed_or_kept_raw() {
        let ok = ToolCall::from_raw("1", "click", r#"{"uid":"3_4"}"#);
        assert_eq!(ok.arguments, json!({"uid": "3_4"}));
        assert!(ok.arguments_error().is_none());

        let empty = ToolCall::from_raw("2", "list_pages", "  ");
        assert_eq!(empty.arguments, json!({}));

        let bad = ToolCall::from_raw("3", "click", r#"{"uid": "3_4""#);
        assert_eq!(bad.arguments, Value::Null);
        assert_eq!(bad.raw, r#"{"uid": "3_4""#);
        assert!(bad.arguments_error().unwrap().contains("valid JSON"));

        let not_object = ToolCall::from_raw("4", "click", "[1]");
        assert!(not_object.arguments_error().unwrap().contains("object"));
    }

    #[test]
    fn builder_assembles_tool_calls_by_index() {
        let mut b = ResponseBuilder::new();
        b.push(ChatDelta::Text("Look".into()));
        b.push(ChatDelta::Text("ing".into()));
        b.push(ChatDelta::ToolCallPart {
            index: 1,
            id: Some("b".into()),
            name: Some("Read".into()),
            args: "{\"file_".into(),
        });
        b.push(ChatDelta::ToolCallPart {
            index: 0,
            id: None,
            name: Some("Glob".into()),
            args: "{}".into(),
        });
        b.push(ChatDelta::ToolCallPart {
            index: 1,
            id: None,
            name: None,
            args: "path\":\"a\"}".into(),
        });
        assert_eq!(b.text(), "Looking");
        b.push(ChatDelta::Done {
            model: "m".into(),
            generation_id: Some("gen-1".into()),
            finish: FinishReason::ToolCalls,
            usage: CallUsage {
                prompt_tokens: 3,
                ..CallUsage::default()
            },
        });
        let r = b.finish().unwrap();
        assert_eq!(r.message.text(), "Looking");
        assert_eq!(r.message.tool_calls.len(), 2);
        assert_eq!(r.message.tool_calls[0].id, "call_0");
        assert_eq!(r.message.tool_calls[0].name, "Glob");
        assert_eq!(r.message.tool_calls[1].id, "b");
        assert_eq!(r.message.tool_calls[1].arguments, json!({"file_path": "a"}));
        assert_eq!(r.finish, FinishReason::ToolCalls);
        assert_eq!(r.usage.prompt_tokens, 3);
    }

    #[test]
    fn builder_without_done_has_no_answer() {
        let mut b = ResponseBuilder::new();
        b.push(ChatDelta::Text("half".into()));
        assert!(b.finish().is_none());
    }

    #[test]
    fn finish_reasons_map_from_wire_names() {
        assert_eq!(FinishReason::from("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(FinishReason::from("stop"), FinishReason::Stop);
        assert_eq!(FinishReason::from("length"), FinishReason::Length);
        assert_eq!(FinishReason::from("error"), FinishReason::Error);
        assert_eq!(
            FinishReason::from("weird"),
            FinishReason::Other("weird".into())
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::ToolCalls).unwrap(),
            "\"tool_calls\""
        );
    }

    #[test]
    fn request_defaults_ask_for_one_tool_call_at_a_time() {
        let r = ChatRequest::new("m", vec![Message::user("hi")]);
        assert_eq!(r.parallel_tool_calls, Some(false));
        assert_eq!(r.tool_choice, ToolChoice::Auto);
        assert!(r.provider.is_none());
        assert_eq!(r.temperature, None);
        assert_eq!(r.seed, None);
        let d = ChatRequest::default();
        assert_eq!(d.parallel_tool_calls, None);
        assert_eq!(d.messages, Vec::<Message>::new());
    }
}
