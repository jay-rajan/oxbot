//! A model client for OpenAI-style chat APIs, for oxbot's own agent loop.
//!
//! - [`types`]: provider-neutral messages, tool calls, requests, streamed
//!   answer pieces and usage.
//! - [`error`]: errors sorted into kinds the router acts on.
//! - [`openrouter`]: OpenRouter's provider preferences (privacy), model
//!   list with the loop's capability filter, and key info.
//! - [`client`]: the [`ChatModel`] trait and [`OpenAiClient`] (streaming,
//!   TLS with ring and the macOS trust store, loopback-only otherwise).
//! - [`sse`] and [`wire`]: the stream format and the OpenAI-style JSON.
//! - [`secret`]: [`ApiKey`] and [`KeyCell`], which never show the key.
//! - [`oauth`]: "Sign in with OpenRouter" (PKCE).
//! - `scripted` (feature `test-model`): a scripted model for tests.
//!
//! Local servers (#22) plug in as an [`Endpoint::loopback`] with
//! [`Provider::Local`], or as another [`ChatModel`] implementation (e.g.
//! Ollama's own API); the loop only sees the trait.
//!
//! No oxbot types here: the loop and the hub build on this crate.

// Product names (OpenRouter, OpenAI) read better in docs without backticks.
#![allow(clippy::doc_markdown)]

pub mod client;
pub mod error;
pub mod oauth;
pub mod openrouter;
#[cfg(feature = "test-model")]
pub mod scripted;
pub mod secret;
pub mod sse;
pub mod types;
pub mod wire;

pub use client::{
    Auth, ChatModel, ClientConfig, Endpoint, OPENROUTER_BASE, OpenAiClient, Timeouts, complete,
};

pub use error::{ErrorKind, LlmError, RetryClass, parse_retry_after};
pub use oauth::{Pkce, auth_url, exchange_code, random_state};
pub use openrouter::{
    Catalog, DataCollection, KeyInfo, MIN_CONTEXT, ModelInfo, ProviderPrefs, Quota,
};
#[cfg(feature = "test-model")]
pub use scripted::{ScriptedModel, Step};
pub use secret::{
    ApiKey, KEYCHAIN_WAITING, KeyCell, KeyError, MemorySecrets, SecretError, SecretStore,
};
pub use tokio_util::sync::CancellationToken;
pub use types::{
    CallUsage, ChatDelta, ChatRequest, ChatResponse, FinishReason, Message, Part, Provider,
    ResponseBuilder, Role, StoredMessageError, ToolCall, ToolChoice, ToolSpec, parse_arguments,
};
