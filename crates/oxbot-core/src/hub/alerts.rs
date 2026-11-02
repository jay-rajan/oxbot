//! Usage alerts: when Claude usage passes 50% and then 80% of the plan's
//! 5-hour or weekly window, every client is told once per level per
//! window, and the server posts a Mac notification. The levels are kept
//! in the store, so a restart never alerts twice. Model-specific windows
//! (e.g. a weekly Opus limit) aren't alerted.
//!
//! Also the two notification preferences, and how a waiting bot may be
//! named in a notification.

use oxbot_claude::LimitWindow;
use oxbot_protocol::{ServerMsg, UsageAlert};

use super::{Hub, HubError};
use crate::id::BotId;
use crate::store::{AlertChange, LimitAlert, now_ms};

/// The windows that alert.
const ALERTED: [&str; 2] = ["five_hour", "seven_day"];
/// Reset times this close are the same window: the reported time can
/// shift a little between reports.
const SAME_WINDOW_MS: i64 = 30 * 60 * 1000;
/// The longest model name shown.
const MAX_MODEL: usize = 40;

/// Preference: tell the user when usage passes 50% and 80% (`1` or `0`).
pub const PREF_USAGE_ALERTS: &str = "usage_alerts";
/// Preference: a Mac notification when a bot waits for the user while
/// oxbot's window is hidden (`1` or `0`).
pub const PREF_NOTIFY_WHEN_HIDDEN: &str = "notify_when_hidden";

/// The level `used` (0 to 1) has passed, if any.
fn level_of(used: f64) -> Option<u8> {
    if used >= 0.8 {
        Some(80)
    } else if used >= 0.5 {
        Some(50)
    } else {
        None
    }
}

/// What a window Claude just reported does to its alert row `prev`, at
/// `now_ms`. A new level in the same window alerts; a jump past both
/// alerts once, at 80. A later reset time is a new window, which starts
/// over; an earlier one is an old report and is ignored. With no reset
/// time to go by, the alert re-arms once usage drops below 50%.
pub(crate) fn decide(prev: Option<&LimitAlert>, w: &LimitWindow, now_ms: i64) -> AlertChange {
    if !ALERTED.contains(&w.name.as_str()) {
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

/// A model as people say it: `claude-sonnet-4-5-20250929` → `Sonnet 4.5`,
/// `haiku` → `Haiku`. Anything else is shown as given.
#[must_use]
pub fn pretty_model(model: &str) -> String {
    let model = model.trim();
    // `claude-opus-4-5[1m]`: the part in brackets is a context size.
    let base = model.split('[').next().unwrap_or(model);
    let mut family = None;
    let mut version = Vec::new();
    for part in base.split(['-', '.']) {
        match part.to_ascii_lowercase().as_str() {
            "opus" => family = Some("Opus"),
            "sonnet" => family = Some("Sonnet"),
            "haiku" => family = Some("Haiku"),
            // Version numbers are short; dates and the rest are skipped.
            p if !p.is_empty() && p.len() <= 2 && p.bytes().all(|b| b.is_ascii_digit()) => {
                version.push(part);
            }
            _ => {}
        }
    }
    match family {
        Some(f) if version.is_empty() => f.to_owned(),
        Some(f) => format!("{f} {}", version.join(".")),
        None => model.chars().take(MAX_MODEL).collect(),
    }
}

/// A window as said in an alert.
fn limit_label(name: &str) -> &'static str {
    match name {
        "five_hour" => "5-hour limit",
        _ => "weekly limit",
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

/// `A`, `A and B`, `A, B and C`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The alert as shown in the banner, the notification and `oxbot chat`.
fn usage_alert(a: &LimitAlert, models: Vec<String>, now_ms: i64) -> UsageAlert {
    let pct = (a.used * 100.0).round();
    let reset = resets_in(a.resets_at_ms, now_ms)
        .map(|when| format!(" Resets {when}."))
        .unwrap_or_default();
    let uses = if models.is_empty() {
        "Claude's default model".to_owned()
    } else {
        and_list(&models)
    };
    let message = format!(
        "Claude's {} is at {pct}%.{reset} Your bots use {uses}.",
        limit_label(&a.name)
    );
    UsageAlert {
        limit: a.name.clone(),
        level: a.level,
        used: a.used,
        resets_at_ms: a.resets_at_ms,
        models,
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
        let models = self.models_in_use();
        for a in recorded {
            tracing::info!(
                limit = a.name,
                level = a.level,
                on,
                "Claude usage passed {}%",
                a.level
            );
            if on {
                let _ =
                    self.updates
                        .send(ServerMsg::UsageAlert(usage_alert(&a, models.clone(), now)));
            }
        }
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
            .filter(|a| ALERTED.contains(&a.name.as_str()))
            // With no reset time, a 5-hour window's alert lasts 5 hours.
            .filter(|a| {
                a.resets_at_ms > now
                    || (a.resets_at_ms == 0 && now - a.alerted_ms < 5 * 3600 * 1000)
            })
            .collect();
        if live.is_empty() {
            return Vec::new();
        }
        let models = self.models_in_use();
        live.iter()
            .map(|a| usage_alert(a, models.clone(), now))
            .collect()
    }

    /// The models the bots use, as shown: what each bot's Claude last
    /// said, else what the bots ask for, sorted, each once.
    fn models_in_use(&self) -> Vec<String> {
        let bots = self
            .bots
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let reported = self.store.bot_models().unwrap_or_else(|err| {
            tracing::warn!(%err, "couldn't read the bots' models");
            Vec::new()
        });
        let mut models: Vec<String> = reported
            .into_iter()
            .filter(|(bot, _)| BotId::new(bot).is_ok_and(|id| bots.contains_key(&id)))
            .map(|(_, model)| pretty_model(&model))
            .collect();
        if models.is_empty() {
            models = bots
                .values()
                .filter_map(|b| b.model.as_deref())
                .map(pretty_model)
                .collect();
        }
        models.retain(|m| !m.is_empty());
        models.sort();
        models.dedup();
        models
    }

    /// Notes the model `bot`'s Claude said it uses.
    pub(super) fn note_model(&self, bot: &BotId, model: &str) {
        if model.trim().is_empty() {
            return;
        }
        if let Err(err) = self.store.set_model(bot, model) {
            tracing::warn!(%bot, %err, "couldn't note the bot's model");
        }
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

    /// Whether a bot waiting while oxbot's window is hidden posts a Mac
    /// notification (it does unless switched off).
    #[must_use]
    pub fn notify_when_hidden(&self) -> bool {
        self.pref_on(PREF_NOTIFY_WHEN_HIDDEN)
    }

    /// Turns the notification for a waiting bot on or off.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub fn set_notify_when_hidden(&self, on: bool) -> Result<(), HubError> {
        self.store
            .set_pref(PREF_NOTIFY_WHEN_HIDDEN, if on { "1" } else { "0" })?;
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
            match decide(row.as_ref(), &window("five_hour", used, resets_at), NOW) {
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
    fn model_windows_and_finished_windows_never_alert() {
        let opus = window("seven_day_opus", 0.9, IN_3H);
        assert_eq!(decide(None, &opus, NOW), AlertChange::Keep);
        let over = window("five_hour", 0.9, NOW / 1000 - 60);
        assert_eq!(decide(None, &over, NOW), AlertChange::Keep);
        let week = window("seven_day", 0.9, IN_3H);
        assert!(matches!(
            decide(None, &week, NOW),
            AlertChange::Record(LimitAlert { level: 80, .. })
        ));
    }

    #[test]
    fn models_read_as_people_say_them() {
        assert_eq!(pretty_model("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(pretty_model("claude-opus-4-1"), "Opus 4.1");
        assert_eq!(pretty_model("claude-haiku-4-5"), "Haiku 4.5");
        assert_eq!(pretty_model("claude-3-5-haiku-20241022"), "Haiku 3.5");
        assert_eq!(pretty_model("claude-opus-4-5[1m]"), "Opus 4.5");
        assert_eq!(pretty_model("sonnet"), "Sonnet");
        assert_eq!(pretty_model("fake"), "fake");
        assert_eq!(pretty_model(""), "");
    }

    #[test]
    fn the_message_names_the_limit_the_reset_and_the_models() {
        let a = LimitAlert {
            name: "five_hour".to_owned(),
            resets_at_ms: NOW + (2 * 60 + 10) * 60 * 1000,
            level: 50,
            used: 0.523,
            alerted_ms: NOW,
        };
        let alert = usage_alert(&a, vec!["Haiku 4.5".into(), "Sonnet 4.5".into()], NOW);
        assert_eq!(
            alert.message,
            "Claude's 5-hour limit is at 52%. Resets in 2 h 10 min. Your bots use Haiku 4.5 and Sonnet 4.5."
        );
        let week = LimitAlert {
            name: "seven_day".to_owned(),
            resets_at_ms: NOW + (3 * 1440 + 4 * 60) * 60 * 1000,
            level: 80,
            used: 0.81,
            alerted_ms: NOW,
        };
        assert_eq!(
            usage_alert(&week, Vec::new(), NOW).message,
            "Claude's weekly limit is at 81%. Resets in 3 days 4 h. Your bots use Claude's default model."
        );
    }
}
