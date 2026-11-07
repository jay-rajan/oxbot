//! Usage alerts: when use passes 50%, then 80%, then all of one of the
//! household's `OpenRouter` limits (free requests today, paid spending
//! this month, the key's credit, a bot's requests today), every client is told
//! once per level per window, and the server posts a Mac notification. The
//! levels are kept in the store, so a restart never alerts twice.
//!
//! Also the two notification preferences, and how a waiting bot may be
//! named in a notification.

use oxbot_protocol::{ServerMsg, UsageAlert};

use super::{Hub, HubError};
use crate::engine::LimitWindow;
use crate::store::{AlertChange, LimitAlert, now_ms};

/// `OpenRouter`'s windows that alert (the meter makes them): free requests
/// today, paid spending this calendar month (UTC), the key's credit, and
/// each bot's requests today (`openrouter_bot_daily:<bot>`). A row left
/// from the old daily spending window never shows.
const OPENROUTER_ALERTED: [&str; 3] = [
    "openrouter_free_daily",
    "openrouter_spend_monthly",
    "openrouter_credits",
];
const BOT_DAILY: &str = "openrouter_bot_daily:";

/// Whether window `name` alerts: one of `OpenRouter`'s (an alert row left
/// from Claude's windows never shows).
fn alerted(name: &str) -> bool {
    OPENROUTER_ALERTED.contains(&name) || name.starts_with(BOT_DAILY)
}
/// Reset times this close are the same window: the reported time can
/// shift a little between reports.
const SAME_WINDOW_MS: i64 = 30 * 60 * 1000;

/// Preference: tell the user when usage passes 50% and 80% (`1` or `0`).
pub const PREF_USAGE_ALERTS: &str = "usage_alerts";

/// The level `used` (0 to 1) has passed, if any: 50, 80, or 100 when used
/// up.
fn level_of(used: f64) -> Option<u8> {
    if used >= 1.0 {
        Some(100)
    } else if used >= 0.8 {
        Some(80)
    } else if used >= 0.5 {
        Some(50)
    } else {
        None
    }
}

/// What a window the meter just measured does to its alert row `prev`, at
/// `now_ms`. A new level in the same window alerts; a jump past both
/// alerts once, at 80. A later reset time is a new window, which starts
/// over; an earlier one is an old report and is ignored. With no reset
/// time to go by, the alert re-arms once usage drops below 50%.
pub(crate) fn decide(prev: Option<&LimitAlert>, w: &LimitWindow, now_ms: i64) -> AlertChange {
    if !alerted(&w.name) {
        return AlertChange::Keep;
    }
    let resets = w.resets_at.saturating_mul(1000);
    if resets > 0 && resets <= now_ms {
        // That window is over already.
        return AlertChange::Keep;
    }
    let level = level_of(w.used);
    let record = |level: u8, resets_at_ms: i64| {
        AlertChange::Record(LimitAlert {
            name: w.name.clone(),
            resets_at_ms,
            level,
            used: w.used,
            alerted_ms: now_ms,
        })
    };
    let Some(prev) = prev else {
        return level.map_or(AlertChange::Keep, |l| record(l, resets));
    };
    if resets == 0 || prev.resets_at_ms == 0 {
        return match level {
            None => AlertChange::Clear,
            Some(l) if l > prev.level => record(l, resets),
            Some(_) => AlertChange::Keep,
        };
    }
    if resets > prev.resets_at_ms + SAME_WINDOW_MS {
        return level.map_or(AlertChange::Keep, |l| record(l, resets));
    }
    if resets < prev.resets_at_ms - SAME_WINDOW_MS {
        return AlertChange::Keep;
    }
    match level {
        // The same window keeps its first reset time, so it can't drift.
        Some(l) if l > prev.level => record(l, prev.resets_at_ms),
        _ => AlertChange::Keep,
    }
}

/// `in 2 h 10 min`, `in 3 days 4 h`; `None` if unknown or past.
fn resets_in(resets_at_ms: i64, now_ms: i64) -> Option<String> {
    if resets_at_ms <= now_ms || resets_at_ms == 0 {
        return None;
    }
    let mins = (resets_at_ms - now_ms + 59_999) / 60_000;
    let (days, hours, mins) = (mins / 1440, mins % 1440 / 60, mins % 60);
    Some(match (days, hours) {
        (0, 0) => format!("in {mins} min"),
        (0, h) => format!("in {h} h {mins} min"),
        (1, 0) => "in 1 day".to_owned(),
        (1, h) => format!("in 1 day {h} h"),
        (d, 0) => format!("in {d} days"),
        (d, h) => format!("in {d} days {h} h"),
    })
}

/// The alert as shown in the banner, the notification and `oxbot chat`.
fn usage_alert(a: &LimitAlert, now_ms: i64, bot_name: &dyn Fn(&str) -> String) -> UsageAlert {
    let pct = (a.used * 100.0).round();
    let reset = resets_in(a.resets_at_ms, now_ms)
        .map(|when| format!(" Resets {when}."))
        .unwrap_or_default();
    let message = if let Some(bot) = a.name.strip_prefix(BOT_DAILY) {
        format!(
            "{}'s AI questions today are at {pct}%.{reset}",
            bot_name(bot)
        )
    } else {
        match a.name.as_str() {
            "openrouter_free_daily" => {
                format!("Free AI questions today are at {pct}%.{reset}")
            }
            "openrouter_spend_monthly" => {
                format!("Paid AI spending this month is at {pct}% of the limit.{reset}")
            }
            _ => format!("The AI account's credit is {pct}% used."),
        }
    };
    UsageAlert {
        limit: a.name.clone(),
        level: a.level,
        used: a.used,
        resets_at_ms: a.resets_at_ms,
        message,
    }
}

impl Hub {
    /// Records the levels the reported windows passed and, if alerts are
    /// on, tells everyone. Off, the levels are still recorded, so turning
    /// alerts back on never brings a late alert for a level already
    /// passed.
    pub(super) fn check_limit_alerts(&self, windows: &[LimitWindow]) {
        let now = now_ms();
        let recorded = match self
            .store
            .change_limit_alerts(windows, |prev, w| decide(prev, w, now))
        {
            Ok(recorded) => recorded,
            Err(err) => {
                tracing::warn!(%err, "couldn't check the usage alerts");
                return;
            }
        };
        if recorded.is_empty() {
            return;
        }
        let on = self.usage_alerts_on();
        for a in recorded {
            tracing::info!(
                limit = a.name,
                level = a.level,
                on,
                "usage passed {}%",
                a.level
            );
            if on {
                let _ = self
                    .updates
                    .send(ServerMsg::UsageAlert(usage_alert(&a, now, &|id| {
                        self.bot_name_of(id)
                    })));
            }
        }
    }

    /// A bot's name by id, or the id if it's gone.
    fn bot_name_of(&self, id: &str) -> String {
        self.bot(id).map_or_else(|_| id.to_owned(), |b| b.name)
    }

    /// The alerts still live (their window hasn't reset), for a client
    /// that connects now. None while alerts are off.
    #[must_use]
    pub fn live_usage_alerts(&self) -> Vec<UsageAlert> {
        if !self.usage_alerts_on() {
            return Vec::new();
        }
        let now = now_ms();
        let alerts = self.store.limit_alerts().unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read the usage alerts");
            Vec::new()
        });
        let live: Vec<_> = alerts
            .into_iter()
            .filter(|a| alerted(&a.name))
            // With no reset time (the key's credit), an alert lasts 5 hours.
            .filter(|a| {
                a.resets_at_ms > now
                    || (a.resets_at_ms == 0 && now - a.alerted_ms < 5 * 3600 * 1000)
            })
            .collect();
        live.iter()
            .map(|a| usage_alert(a, now, &|id| self.bot_name_of(id)))
            .collect()
    }

    fn pref_on(&self, key: &str) -> bool {
        match self.store.pref(key) {
            Ok(value) => value.as_deref() != Some("0"),
            Err(err) => {
                tracing::warn!(%err, key, "couldn't read a preference");
                true
            }
        }
    }

    /// Whether usage alerts are on (they are unless switched off).
    #[must_use]
    pub fn usage_alerts_on(&self) -> bool {
        self.pref_on(PREF_USAGE_ALERTS)
    }

    /// Turns usage alerts on or off.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub fn set_usage_alerts(&self, on: bool) -> Result<(), HubError> {
        self.store
            .set_pref(PREF_USAGE_ALERTS, if on { "1" } else { "0" })?;
        Ok(())
    }

    /// The name a notification on the Mac's screen may show for `bot`:
    /// `None` for a bot that's private to someone other than the Farm's
    /// own person (the Mac's screen isn't theirs), or a bot oxbot doesn't
    /// know.
    #[must_use]
    pub fn notice_name(&self, bot: &str) -> Option<String> {
        let bot = self.bot(bot).ok()?;
        let theirs = bot.owner.is_some() && bot.owner != self.farm_id();
        let own = bot.private || (bot.is_central() && !bot.is_main_herder());
        (!(theirs && own)).then(|| self.as_herder(&bot).name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;
    const IN_3H: i64 = (NOW + 3 * 3600 * 1000) / 1000;

    fn window(name: &str, used: f64, resets_at: i64) -> LimitWindow {
        LimitWindow {
            name: name.to_owned(),
            used,
            resets_at,
            status: "allowed".to_owned(),
        }
    }

    /// Runs `reports` through [`decide`] as the store would, returning
    /// the levels alerted.
    fn run(reports: &[(f64, i64)]) -> Vec<u8> {
        let mut row: Option<LimitAlert> = None;
        let mut alerted = Vec::new();
        for &(used, resets_at) in reports {
            match decide(
                row.as_ref(),
                &window("openrouter_free_daily", used, resets_at),
                NOW,
            ) {
                AlertChange::Keep => {}
                AlertChange::Clear => row = None,
                AlertChange::Record(a) => {
                    alerted.push(a.level);
                    row = Some(a);
                }
            }
        }
        alerted
    }

    #[test]
    fn each_level_alerts_once_per_window() {
        assert_eq!(
            run(&[
                (0.49, IN_3H),
                (0.5, IN_3H),
                (0.55, IN_3H),
                (0.8, IN_3H),
                (0.95, IN_3H)
            ]),
            [50, 80]
        );
        assert_eq!(run(&[(0.85, IN_3H), (1.0, IN_3H), (1.2, IN_3H)]), [80, 100]);
    }

    #[test]
    fn openrouter_windows_also_alert_when_used_up() {
        let mut row: Option<LimitAlert> = None;
        let mut levels = Vec::new();
        for used in [0.5, 0.8, 1.0, 1.0] {
            let w = window("openrouter_bot_daily:scout", used, IN_3H);
            if let AlertChange::Record(a) = decide(row.as_ref(), &w, NOW) {
                levels.push(a.level);
                row = Some(a);
            }
        }
        assert_eq!(levels, [50, 80, 100]);
        let a = row.unwrap();
        let alert = usage_alert(&a, NOW, &|_| "Scout".to_owned());
        assert_eq!(
            alert.message,
            "Scout's AI questions today are at 100%. Resets in 3 h 0 min."
        );
        // Unknown names never alert, nor Claude's old windows.
        for name in ["openrouter_other", "five_hour", "seven_day"] {
            assert!(matches!(
                decide(None, &window(name, 0.9, IN_3H), NOW),
                AlertChange::Keep
            ));
        }
        let free = LimitAlert {
            name: "openrouter_free_daily".into(),
            ..a
        };
        assert!(
            usage_alert(&free, NOW, &|id| id.to_owned())
                .message
                .starts_with("Free AI questions today are at 100%.")
        );
    }

    #[test]
    fn a_jump_past_both_levels_alerts_once() {
        assert_eq!(run(&[(0.3, IN_3H), (0.85, IN_3H), (0.9, IN_3H)]), [80]);
    }

    #[test]
    fn a_new_window_starts_over_and_jitter_is_the_same_window() {
        let next = IN_3H + 5 * 3600;
        assert_eq!(
            run(&[
                (0.6, IN_3H),
                (0.6, IN_3H + 600),
                (0.6, IN_3H - 600),
                (0.6, next)
            ]),
            [50, 50]
        );
        // A new window below 50% alerts only once it passes 50%.
        assert_eq!(run(&[(0.6, IN_3H), (0.1, next), (0.55, next)]), [50, 50]);
    }

    #[test]
    fn an_older_report_is_ignored() {
        let next = IN_3H + 5 * 3600;
        assert_eq!(run(&[(0.6, next), (0.85, IN_3H)]), [50]);
    }

    #[test]
    fn with_no_reset_time_it_rearms_below_half() {
        assert_eq!(run(&[(0.6, 0), (0.7, 0), (0.6, 0)]), [50]);
        assert_eq!(run(&[(0.6, 0), (0.3, 0), (0.6, 0)]), [50, 50]);
    }

    #[test]
    fn finished_windows_never_alert() {
        let over = window("openrouter_free_daily", 0.9, NOW / 1000 - 60);
        assert_eq!(decide(None, &over, NOW), AlertChange::Keep);
        let spend = window("openrouter_spend_monthly", 0.9, IN_3H);
        let old = window("openrouter_spend_daily", 0.9, IN_3H);
        assert_eq!(decide(None, &old, NOW), AlertChange::Keep, "retired");
        assert!(matches!(
            decide(None, &spend, NOW),
            AlertChange::Record(LimitAlert { level: 80, .. })
        ));
    }

    #[test]
    fn the_message_names_the_limit_and_the_reset() {
        let a = LimitAlert {
            name: "openrouter_spend_monthly".to_owned(),
            resets_at_ms: NOW + ((12 * 24 + 4) * 60 + 10) * 60 * 1000,
            level: 50,
            used: 0.523,
            alerted_ms: NOW,
        };
        assert_eq!(
            usage_alert(&a, NOW, &|id| id.to_owned()).message,
            "Paid AI spending this month is at 52% of the limit. Resets in 12 days 4 h."
        );
        let credit = LimitAlert {
            name: "openrouter_credits".to_owned(),
            resets_at_ms: 0,
            level: 80,
            used: 0.81,
            alerted_ms: NOW,
        };
        assert_eq!(
            usage_alert(&credit, NOW, &|id| id.to_owned()).message,
            "The AI account's credit is 81% used."
        );
        assert_eq!(
            resets_in(NOW + (3 * 1440 + 4 * 60) * 60 * 1000, NOW).as_deref(),
            Some("in 3 days 4 h")
        );
    }
}
