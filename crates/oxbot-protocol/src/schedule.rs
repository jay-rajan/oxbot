//! Schedules: a bot's task that runs on its own at set times, as the
//! person who made it. Made only through a schedule card the person
//! answered (see [`crate::ApprovalKind::Schedule`]).

use serde::{Deserialize, Serialize};

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Day {
    /// Monday.
    Mon,
    /// Tuesday.
    Tue,
    /// Wednesday.
    Wed,
    /// Thursday.
    Thu,
    /// Friday.
    Fri,
    /// Saturday.
    Sat,
    /// Sunday.
    Sun,
}

impl Day {
    /// Every day, Monday first.
    pub const ALL: [Self; 7] = [
        Self::Mon,
        Self::Tue,
        Self::Wed,
        Self::Thu,
        Self::Fri,
        Self::Sat,
        Self::Sun,
    ];
}

/// When a schedule runs, in the home's time zone. Times are 24-hour
/// `HH:MM`; a date-time is `YYYY-MM-DDTHH:MM`. No free-form rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleRule {
    /// Once, at a date and time.
    Once {
        /// `YYYY-MM-DDTHH:MM`, local.
        at: String,
    },
    /// Every day.
    Daily {
        /// `HH:MM`.
        time: String,
    },
    /// Monday to Friday.
    Weekdays {
        /// `HH:MM`.
        time: String,
    },
    /// On some days of the week.
    Weekly {
        /// The days (at least one).
        days: Vec<Day>,
        /// `HH:MM`.
        time: String,
    },
    /// On one day of the month; a day past the month's end is its last day.
    Monthly {
        /// 1 to 31.
        day: u8,
        /// `HH:MM`.
        time: String,
    },
}

/// A schedule a bot proposes, as its card shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScheduleDraft {
    /// What the bot will be asked each time.
    pub task: String,
    /// When.
    pub rule: ScheduleRule,
    /// Both in words, e.g. `Every weekday at 8:15 AM: today's prep plan`.
    pub text: String,
    /// The home's time zone, e.g. `America/Toronto`.
    pub tz: String,
    /// The bot it runs in, by name.
    pub bot_name: String,
    /// Where the request came from, when the bot had read something oxbot
    /// doesn't trust before asking (e.g. `a web page (https://…)`). The
    /// card says so: the person checks it's what they asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// The person's changes to a proposed schedule before they press Create
/// ("Change" on its card).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ScheduleEdit {
    /// The task, as they want it.
    pub task: String,
    /// When, as they want it.
    pub rule: ScheduleRule,
}

/// One schedule in a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a wire message: each flag is its own JSON field"
)]
pub struct ScheduleInfo {
    /// Its id.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub id: i64,
    /// The bot it runs in (its id as the client knows it).
    pub bot: String,
    /// The bot's name.
    pub bot_name: String,
    /// Who it runs as (their name as it is now).
    pub person: String,
    /// What the bot is asked.
    pub task: String,
    /// When.
    pub rule: ScheduleRule,
    /// Both in words.
    pub text: String,
    /// The time zone its times are in.
    pub tz: String,
    /// When it runs next (ms since the Unix epoch); `None` once a one-time
    /// schedule has run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub next_run_ms: Option<i64>,
    /// When it last ran or was due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub last_run_ms: Option<i64>,
    /// How that went: `ok`, `failed`, `running`, `busy`, `limit`,
    /// `missed`, `too_soon`, `no_access`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<String>,
    /// Paused: it doesn't run until resumed.
    pub paused: bool,
    /// Whether this client may pause or delete it. Decided by the server.
    #[serde(default)]
    pub can_change: bool,
    /// Whether it's this client's own.
    #[serde(default)]
    pub mine: bool,
    /// When it was made.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub created_ms: i64,
    /// The person's id, for the server's checks. Never sent.
    #[serde(skip)]
    pub person_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_small_tagged_objects() {
        let rule = ScheduleRule::Weekly {
            days: vec![Day::Sat],
            time: "18:00".into(),
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "weekly", "days": ["sat"], "time": "18:00"})
        );
        assert_eq!(serde_json::from_value::<ScheduleRule>(json).unwrap(), rule);
        assert!(
            serde_json::from_value::<ScheduleRule>(serde_json::json!({"kind": "cron", "x": "*"}))
                .is_err()
        );
        let info = ScheduleInfo {
            id: 1,
            bot: "b".into(),
            bot_name: "B".into(),
            person: "Sam".into(),
            task: "t".into(),
            rule,
            text: "x".into(),
            tz: "UTC".into(),
            next_run_ms: None,
            last_run_ms: None,
            last_outcome: None,
            paused: false,
            can_change: false,
            mine: false,
            created_ms: 0,
            person_id: "p_0123".into(),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.contains("p_0123"), "the person's id is never sent");
    }
}
