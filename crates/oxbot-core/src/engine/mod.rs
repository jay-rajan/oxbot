//! Engines: what runs a bot. The shared types and traits come from
//! `oxbot-engine` (re-exported here).
//!
//! - [`claude`]: Claude Code, as oxbot has always run it.

pub mod claude;

pub use oxbot_engine::*;

/// The provider of Claude's own model requests in the meter
/// (`model_calls.provider`).
pub const CLAUDE_PROVIDER: &str = "anthropic";
