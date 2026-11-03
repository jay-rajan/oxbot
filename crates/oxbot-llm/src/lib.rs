//! A model client for OpenAI-style chat APIs, for oxbot's own agent loop.
//!
//! - [`types`]: provider-neutral messages, tool calls, requests, streamed
//!   answer pieces and usage.
//! - [`error`]: errors sorted into kinds the router acts on.
//! - [`openrouter`]: OpenRouter's provider preferences (privacy).
//! - [`sse`] and [`wire`]: the stream format and the OpenAI-style JSON.
//!
//! No oxbot types here: the loop and the hub build on this crate.

// Product names (OpenRouter, OpenAI) read better in docs without backticks.
#![allow(clippy::doc_markdown)]

pub mod error;
pub mod openrouter;
pub mod sse;
pub mod types;
pub mod wire;

pub use error::{ErrorKind, LlmError, RetryClass, parse_retry_after};
pub use openrouter::{DataCollection, ProviderPrefs};
pub use types::{
    CallUsage, ChatDelta, ChatRequest, ChatResponse, FinishReason, Message, Part, Provider,
    ResponseBuilder, Role, StoredMessageError, ToolCall, ToolChoice, ToolSpec, parse_arguments,
};
