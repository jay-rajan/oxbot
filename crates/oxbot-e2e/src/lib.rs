//! End-to-end tests for oxbot, run by `/pr_test`.
//!
//! - [`spec`]: the test files in `e2e/tests/` and the repo's `e2e/pr_test.toml`.
//! - [`pick`]: which tests a change needs, and why.
//! - [`script`]: the stand-ins' reply scripts (`e2e/claude/`).
//! - [`fake_openrouter`], [`fake_devtools`]: stand-ins for `OpenRouter` and
//!   the browser tool, for bots on oxbot's own loop.
//! - [`canary`]: checks a run never wrote the stand-in's key anywhere.
//! - [`seed`]: a throwaway oxbot home made from `e2e/seed/`.
//! - [`serve`]: starts oxbot on it in test mode with the stand-in Claude.
//! - [`driver`]: Chrome (`DevTools`) and Safari (`WebDriver`), behind one API.
//! - [`steps`]: runs a test's steps, a screenshot per step.
//! - [`compare`]: screenshots against approved baselines.
//! - [`report`]: the results as a page.

pub mod app;
pub mod canary;
pub mod compare;
pub mod driver;
pub mod fake_devtools;
pub mod fake_openrouter;
pub mod http;
pub mod pick;
pub mod report;
pub mod script;
pub mod seed;
pub mod serve;
pub mod spec;
pub mod steps;

/// When something happened, in milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}
