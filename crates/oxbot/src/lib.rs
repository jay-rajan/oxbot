//! The `oxbot` program's pieces, kept in a library so integration tests can
//! drive them:
//!
//! - [`server`]: `oxbot serve`, the WebSocket server
//! - [`chat`]: `oxbot chat`, the terminal client
//! - [`dictation`]: dictation with the Mac's microphone, on-device
//! - [`client`]: one-shot requests from CLI commands to a running server
//! - [`door`]: the Gate door phones and laptops pair and connect through
//! - [`keychain`]: where the `OpenRouter` key is kept (the Keychain)
//! - [`openrouter_auth`]: "Sign in with `OpenRouter`" and the key
//! - [`logs`]: relays the server's log to the apps
//! - [`notify`]: Mac notifications for usage alerts and waiting bots
//! - [`runtime`]: starts and stops a whole server (CLI and desktop app)
//! - [`setup`]: what oxbot needs on the Mac, and where to find it

pub mod chat;
pub mod client;
pub mod compare;
pub mod dictation;
pub mod door;
pub mod keychain;
pub mod logs;
pub mod notify;
pub mod openrouter_auth;
pub mod runtime;
pub mod secret;
pub mod server;
pub mod setup;

/// Default port for `oxbot serve` and `oxbot chat`.
pub const DEFAULT_PORT: u16 = 7878;
