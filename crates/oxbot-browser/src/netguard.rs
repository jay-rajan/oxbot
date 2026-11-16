//! Practice runs' network rules. The guard that enforced them was built on
//! Chrome's `DevTools` and has no Camoufox equivalent yet, so
//! [`crate::Browser::new_trial`] refuses (see [`crate::NO_PRACTICE_RUNS`]).
//! These are the shapes the hub keeps using.

/// How much a practice run's pages may send.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NetMode {
    /// Only GET, HEAD and OPTIONS.
    #[default]
    Strict,
    /// Also POSTs from scripts (sites that read with POST).
    XhrAllowed,
}

/// A request the guard stopped. No query string or body is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRequest {
    /// Its method, e.g. `POST`.
    pub method: String,
    /// Its host, e.g. `jobs.example.test`.
    pub host: String,
    /// Its path, e.g. `/post`.
    pub path: String,
}
