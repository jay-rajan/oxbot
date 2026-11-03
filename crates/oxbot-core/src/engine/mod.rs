//! Engines: what runs a bot. The shared types and traits come from
//! `oxbot-engine` (re-exported here).
//!
//! - [`claude`]: Claude Code, as oxbot has always run it.

pub mod claude;

pub use oxbot_engine::*;
