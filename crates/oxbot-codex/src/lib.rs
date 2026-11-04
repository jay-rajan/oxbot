//! The Codex engine for oxbot: Codex CLI (`codex app-server`, 0.148.x)
//! runs a bot's agent loop; oxbot keeps everything else.
//!
//! - [`binary`]: finding `codex` and checking its version.
//! - [`config`]: the `config.toml` oxbot writes for every process.
//! - [`rpc`]: one process and its JSON-RPC over stdio.
//! - [`supervisor`]: which process serves a session (one per bot), and
//!   starting, restarting and stopping them.
//! - [`engine`]: the [`oxbot_engine::Engine`] and its sessions: threads,
//!   turns, oxbot's tools as Codex's dynamic tools, limits and checks.
//! - [`guard`]: the proxy that refuses whatever isn't the relay.
//!
//! Codex reaches nothing but oxbot's relay ([`oxbot_relay`]) on
//! 127.0.0.1: its model provider is the relay, everything that would go
//! out on its own is off in its config, and any other connection goes to
//! a proxy that refuses it.

// Product names (Codex, OpenRouter) read better in docs without backticks.
#![allow(clippy::doc_markdown)]

pub mod binary;
pub mod config;
pub mod engine;
pub mod guard;
pub mod names;
pub mod rpc;
pub mod store;
pub mod supervisor;

pub use binary::{CodexBinary, Found};
pub use engine::{CodexEngine, CodexSetup};
pub use store::{CodexStore, MemoryCodexStore, ThreadRow};
