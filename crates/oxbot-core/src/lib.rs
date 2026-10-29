//! oxbot's logic, with no networking:
//!
//! - [`bot`]: bots on disk (`bot.toml`, `instructions.md`, a `work/` folder)
//! - [`store`]: every bot's history and Claude session, in SQLite
//! - [`hub`]: runs the bots, one long-lived Claude per bot, and broadcasts
//!   what happens
//! - [`policy`]: what a bot may do with a tool, decided by oxbot
//! - [`tools`]: oxbot's own tool for bots (`request_sign_in`), hosted in-process
//! - [`prompt`]: the system prompt every bot gets
//! - [`home`]: where all of that lives on disk

pub mod bot;
pub mod home;
pub mod hub;
pub mod id;
pub mod policy;
pub mod prompt;
pub mod store;
pub mod tools;

pub use bot::{Bot, BotError};
pub use home::Home;
pub use hub::{Hub, HubConfig, HubError};
pub use id::{BotId, BotIdError, MAX_BOT_ID_LEN};
pub use prompt::{Capabilities, system_prompt};
pub use store::{SavedSession, Store, StoreError};
