//! Paced visits: oxbot waits between browser actions on the same site, so
//! bots never hit a site with a burst of page loads or clicks. It's a fixed
//! courtesy gap that every bot shares, not a disguise: nothing is made to
//! look human.

use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

/// The least time between actions on one site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pace {
    /// Between page loads (opening or going to a page).
    pub load: Duration,
    /// Between any two actions (a click, typing, a page load).
    pub act: Duration,
}

impl Pace {
    /// What bots use: a page every 8 seconds, an action every 3.
    pub const POLITE: Self = Self {
        load: Duration::from_secs(8),
        act: Duration::from_secs(3),
    };
    /// No waiting (tests, and the stand-in sites of end-to-end tests).
    pub const NONE: Self = Self {
        load: Duration::ZERO,
        act: Duration::ZERO,
    };
}

impl Default for Pace {
    fn default() -> Self {
        Self::POLITE
    }
}

/// What a browser call does to a site, for pacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Opens or goes to a page.
    Load,
    /// Acts on the open page.
    Act,
}

impl Step {
    /// The step a browser tool takes, or `None` for one that never
    /// reaches the site (reading a snapshot, listing tabs, waiting).
    pub(super) fn of(tool: &str) -> Option<Self> {
        let name = tool.rsplit("__").next().unwrap_or(tool);
        match name {
            "navigate_page" | "new_page" => Some(Self::Load),
            "click" | "click_at" | "fill" | "fill_form" | "type_text" | "press_key" | "hover"
            | "drag" | "upload_file" | "handle_dialog" => Some(Self::Act),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Last {
    load: Option<Instant>,
    act: Instant,
}

/// When each site was last loaded and acted on.
#[derive(Debug, Default)]
pub(super) struct Pacer {
    sites: std::sync::Mutex<HashMap<String, Last>>,
}

impl Pacer {
    /// Waits until `step` on `site` keeps to `pace`, then counts it as
    /// done now. Bots on the same site queue behind each other.
    pub(super) async fn wait(&self, site: &str, step: Step, pace: Pace) {
        if pace.load.is_zero() && pace.act.is_zero() {
            return;
        }
        let wait = {
            let mut sites = self
                .sites
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let due = sites.get(site).map_or(now, |last| {
                let after_act = last.act + pace.act;
                match (step, last.load) {
                    (Step::Load, Some(load)) => after_act.max(load + pace.load),
                    _ => after_act,
                }
            });
            let at = due.max(now);
            let entry = sites.entry(site.to_owned()).or_insert(Last {
                load: None,
                act: at,
            });
            entry.act = at;
            if step == Step::Load {
                entry.load = Some(at);
            }
            at - now
        };
        if !wait.is_zero() {
            tracing::debug!(site, ?wait, "pacing the visit");
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_steps_that_reach_the_site_are_paced() {
        assert_eq!(Step::of("mcp__chrome-devtools__new_page"), Some(Step::Load));
        assert_eq!(Step::of("mcp__chrome-devtools__click"), Some(Step::Act));
        assert_eq!(Step::of("mcp__chrome-devtools__take_snapshot"), None);
        assert_eq!(Step::of("mcp__chrome-devtools__list_pages"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn page_loads_on_one_site_wait_their_turn() {
        let pacer = Pacer::default();
        let start = tokio::time::Instant::now();
        pacer.wait("walmart.ca", Step::Load, Pace::POLITE).await;
        assert_eq!(
            start.elapsed(),
            Duration::ZERO,
            "the first visit runs at once"
        );
        pacer.wait("walmart.ca", Step::Act, Pace::POLITE).await;
        pacer.wait("walmart.ca", Step::Load, Pace::POLITE).await;
        assert!(
            start.elapsed() >= Pace::POLITE.load,
            "{:?}",
            start.elapsed()
        );
        let other = tokio::time::Instant::now();
        pacer.wait("example.com", Step::Load, Pace::POLITE).await;
        assert_eq!(
            other.elapsed(),
            Duration::ZERO,
            "another site isn't held up"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn no_pace_never_waits() {
        let pacer = Pacer::default();
        let start = tokio::time::Instant::now();
        for _ in 0..5 {
            pacer.wait("walmart.ca", Step::Load, Pace::NONE).await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}
