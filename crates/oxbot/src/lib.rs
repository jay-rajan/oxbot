//! The `oxbot` program's pieces, kept in a library so integration tests can
//! drive them:
//!
//! - [`server`]: `oxbot serve`, the WebSocket server
//! - [`chat`]: `oxbot chat`, the terminal client
//! - [`client`]: one-shot requests from CLI commands to a running server
//! - [`logs`]: relays the server's log to the apps
//! - [`runtime`]: starts and stops a whole server (CLI and desktop app)
//! - [`setup`]: what oxbot needs on the Mac, and where to find it

pub mod chat;
pub mod client;
pub mod logs;
pub mod runtime;
pub mod server;
pub mod setup;

/// Default port for `oxbot serve` and `oxbot chat`.
pub const DEFAULT_PORT: u16 = 7878;
