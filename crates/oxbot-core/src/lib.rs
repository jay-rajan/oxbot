//! oxbot's logic, with no networking:
//!
//! - [`attach`]: files sent with messages: what they are, and what Claude
//!   reads of them
//! - [`bot`]: bots on disk (`bot.toml`, `instructions.md`, a `work/` folder)
//! - [`store`]: every bot's history and Claude session, in SQLite
//! - [`hub`]: runs the bots, one long-lived Claude per bot, and broadcasts
//!   what happens
//! - [`engine`]: what runs a bot (the types every engine shares)
//! - [`policy`]: what a bot may do with a tool, decided by oxbot
//! - [`tools`]: oxbot's own tools for bots (`request_sign_in`, `save_report`,
//!   Herder's team tools), hosted in-process
//! - [`prompt`]: the system prompt every bot gets
//! - [`report`]: research reports: their checks and their safe HTML page
//! - [`home`]: where all of that lives on disk

pub mod attach;
pub mod bot;
pub mod compare;
pub mod engine;
pub mod home;
pub mod hub;
pub mod id;
pub mod policy;
pub mod prompt;
pub mod report;
pub mod store;
pub mod tools;

pub use bot::{Bot, BotError, Viewer};
pub use engine::text::data_block;
pub use home::Home;
pub use hub::{CodexParts, Hub, HubConfig, HubError, LoopParts};
pub use id::{BotId, BotIdError, MAX_BOT_ID_LEN};
pub use prompt::{Capabilities, system_prompt};
pub use store::{SavedSession, Store, StoreError};
