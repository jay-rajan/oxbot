//! Schedule rules: when a schedule runs, in words, and its next run.
//!
//! A rule is small and structured ([`ScheduleRule`] on the wire): once at
//! a date and time, daily, weekdays, weekly on some days, or monthly on a
//! day, at a time of day in the home's time zone. Never free-form cron.
//!
//! Times are wall-clock times in a [`Zone`]: the Mac's own ([`MacZone`])
//! in the app. Around daylight saving changes:
//! - a time the clocks skip (2:30 when 2:00 jumps to 3:00) runs as late as
//!   the jump (3:30);
//! - a time that happens twice (1:30 when 2:00 goes back to 1:00) runs
//!   once, the first time.
//!
//! A monthly day past a month's end (the 31st in April) is its last day.
//!
//! Catching up ([`Rule::catch_up`]): when the Mac slept or oxbot was
//! closed through some runs, the most recent one runs once (late), as long
//! as the run after it isn't due yet; older ones are noted missed. A run
//! that starts more than [`LATE_MS`] after it was due says it was late.

use std::fmt::Write as _;

use chrono::{
    DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone,
    Timelike, Weekday,
};
use oxbot_protocol::{Day, ScheduleRule};
use serde_json::Value;

/// A run that starts more than this after it was due (the Mac asleep,
/// oxbot closed) is late, and says so.
pub const LATE_MS: i64 = 5 * 60_000;
/// The most missed runs one check notes for a schedule.
pub const MAX_MISSED: usize = 60;
/// The least time between two scheduled runs of one bot.
pub const MIN_GAP_MS: i64 = 15 * 60_000;
/// The most schedules one person may have.
pub const MAX_PER_PERSON: usize = 30;
/// The longest task a schedule may hold, in characters.
pub const MAX_TASK: usize = 1000;

/// A time zone: what the clocks on the wall say at an instant.
pub trait Zone: Send + Sync + std::fmt::Debug {
    /// Its IANA name, e.g. `America/Toronto`.
    fn name(&self) -> String;
    /// Seconds east of UTC at `utc_ms` (ms since the Unix epoch).
    fn offset_at(&self, utc_ms: i64) -> i32;
    /// The offsets (seconds east of UTC) at which the wall shows `local`:
    /// none in a gap, two when it happens twice.
    fn offsets_for(&self, local: NaiveDateTime) -> Vec<i32>;
}

/// The Mac's own time zone, as macOS has it now.
#[derive(Debug, Clone, Copy, Default)]
pub struct MacZone;

impl Zone for MacZone {
    fn name(&self) -> String {
        iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_owned())
    }

    fn offset_at(&self, utc_ms: i64) -> i32 {
        chrono::Local
            .timestamp_millis_opt(utc_ms)
            .single()
            .map_or(0, |t| t.offset().fix().local_minus_utc())
    }

    fn offsets_for(&self, local: NaiveDateTime) -> Vec<i32> {
        match chrono::Local.offset_from_local_datetime(&local) {
            LocalResult::Single(o) => vec![o.fix().local_minus_utc()],
            LocalResult::Ambiguous(a, b) => {
                vec![a.fix().local_minus_utc(), b.fix().local_minus_utc()]
            }
            LocalResult::None => Vec::new(),
        }
    }
}

/// Why a rule isn't valid, in words for a bot or a person.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct RuleError(pub String);

fn bad(why: impl Into<String>) -> RuleError {
    RuleError(why.into())
}

/// On which days a rule runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Days {
    Once(NaiveDate),
    Daily,
    Weekdays,
    /// A bit per weekday, Monday first.
    Weekly(u8),
    Monthly(u8),
}

/// A checked rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    days: Days,
    time: NaiveTime,
}

/// `Monday`'s bit is 1, `Sunday`'s 64.
fn bit(day: Weekday) -> u8 {
    1 << day.num_days_from_monday()
}

fn weekday(day: Day) -> Weekday {
    match day {
        Day::Mon => Weekday::Mon,
        Day::Tue => Weekday::Tue,
        Day::Wed => Weekday::Wed,
        Day::Thu => Weekday::Thu,
        Day::Fri => Weekday::Fri,
        Day::Sat => Weekday::Sat,
        Day::Sun => Weekday::Sun,
    }
}

const MON_TO_FRI: u8 = 0b001_1111;

/// `08:15`, `8:15`, `18:00`; also `8:15 AM`, `6 PM`, `6pm`.
fn parse_time(text: &str) -> Result<NaiveTime, RuleError> {
    let wrong = || {
        bad(format!(
            "\"{text}\" isn't a time; use 24-hour HH:MM, like 08:15 or 18:00"
        ))
    };
    let t = text.trim().to_ascii_lowercase();
    let (clock, half) = if let Some(rest) = t.strip_suffix("am") {
        (rest.trim(), Some(false))
    } else if let Some(rest) = t.strip_suffix("pm") {
        (rest.trim(), Some(true))
    } else {
        (t.as_str(), None)
    };
    let (h, m) = match clock.split_once(':') {
        Some((h, m)) => (h, m),
        None if half.is_some() => (clock, "0"),
        None => return Err(wrong()),
    };
    if h.is_empty() || h.len() > 2 || m.is_empty() || m.len() > 2 {
        return Err(wrong());
    }
    let h: u32 = h.parse().map_err(|_| wrong())?;
    let m: u32 = m.parse().map_err(|_| wrong())?;
    let h = match half {
        Some(_) if !(1..=12).contains(&h) => return Err(wrong()),
        Some(pm) => h % 12 + if pm { 12 } else { 0 },
        None => h,
    };
    NaiveTime::from_hms_opt(h, m, 0).ok_or_else(wrong)
}

/// `2026-10-20T09:00` (or with a space; seconds ignored).
fn parse_at(text: &str) -> Result<(NaiveDate, NaiveTime), RuleError> {
    let wrong = || {
        bad(format!(
            "\"{text}\" isn't a date and time; use YYYY-MM-DDTHH:MM, like 2026-10-20T09:00"
        ))
    };
    let t = text.trim();
    let (date, time) = t.split_once(['T', ' ']).ok_or_else(wrong)?;
    let date = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").map_err(|_| wrong())?;
    let time = time.trim();
    // Seconds, if any, are dropped.
    let time = match time.match_indices(':').nth(1) {
        Some((i, _)) => &time[..i],
        None => time,
    };
    Ok((date, parse_time(time).map_err(|_| wrong())?))
}

impl Rule {
    /// Checks a rule from the wire.
    ///
    /// # Errors
    /// Says what's wrong with it.
    pub fn parse(rule: &ScheduleRule) -> Result<Self, RuleError> {
        Ok(match rule {
            ScheduleRule::Once { at } => {
                let (date, time) = parse_at(at)?;
                Self {
                    days: Days::Once(date),
                    time,
                }
            }
            ScheduleRule::Daily { time } => Self {
                days: Days::Daily,
                time: parse_time(time)?,
            },
            ScheduleRule::Weekdays { time } => Self {
                days: Days::Weekdays,
                time: parse_time(time)?,
            },
            ScheduleRule::Weekly { days, time } => {
                let mask = days.iter().fold(0, |m, d| m | bit(weekday(*d)));
                if mask == 0 {
                    return Err(bad("A weekly schedule needs at least one day."));
                }
                Self {
                    // Every day, or Monday to Friday, says so.
                    days: match mask {
                        0b111_1111 => Days::Daily,
                        MON_TO_FRI => Days::Weekdays,
                        m => Days::Weekly(m),
                    },
                    time: parse_time(time)?,
                }
            }
            ScheduleRule::Monthly { day, time } => {
                if !(1..=31).contains(day) {
                    return Err(bad("A monthly schedule's day is 1 to 31."));
                }
                Self {
                    days: Days::Monthly(*day),
                    time: parse_time(time)?,
                }
            }
        })
    }

    /// A rule as a bot wrote it in a tool call: the wire's shape, with
    /// day names spelled out or not and any case.
    ///
    /// # Errors
    /// Says what's wrong with it, for the bot.
    pub fn from_tool(value: &Value) -> Result<(Self, ScheduleRule), RuleError> {
        let text = |k: &str| value.get(k).and_then(Value::as_str).map(str::trim);
        let kind = text("kind").unwrap_or_default().to_ascii_lowercase();
        let time = || {
            text("time").map(str::to_owned).ok_or_else(|| {
                bad(format!(
                    "rule.time is needed for a {kind} schedule, like 08:15"
                ))
            })
        };
        let wire = match kind.as_str() {
            "once" => ScheduleRule::Once {
                at: text("at")
                    .ok_or_else(|| {
                        bad("rule.at is needed for a one-time schedule, like 2026-10-20T09:00")
                    })?
                    .to_owned(),
            },
            "daily" => ScheduleRule::Daily { time: time()? },
            "weekdays" => ScheduleRule::Weekdays { time: time()? },
            "weekly" => {
                let names = value.get("days").and_then(Value::as_array).ok_or_else(|| {
                    bad("rule.days is needed for a weekly schedule, like [\"sat\"]")
                })?;
                let mut days = Vec::new();
                for name in names {
                    let name = name
                        .as_str()
                        .unwrap_or_default()
                        .trim()
                        .to_ascii_lowercase();
                    let day = Day::ALL
                        .into_iter()
                        .find(|d| {
                            name.len() >= 3 && day_name(*d).to_ascii_lowercase().starts_with(&name)
                        })
                        .ok_or_else(|| {
                            bad(format!(
                                "\"{name}\" isn't a day; use mon, tue, wed, thu, fri, sat or sun"
                            ))
                        })?;
                    if !days.contains(&day) {
                        days.push(day);
                    }
                }
                days.sort();
                ScheduleRule::Weekly {
                    days,
                    time: time()?,
                }
            }
            "monthly" => ScheduleRule::Monthly {
                day: value
                    .get("day")
                    .and_then(Value::as_u64)
                    .and_then(|d| u8::try_from(d).ok())
                    .ok_or_else(|| bad("rule.day (1 to 31) is needed for a monthly schedule"))?,
                time: time()?,
            },
            _ => {
                return Err(bad(
                    "rule.kind is one of once, daily, weekdays, weekly or monthly (no other repeats)",
                ));
            }
        };
        let rule = Self::parse(&wire)?;
        Ok((rule, rule.to_wire()))
    }

    /// The rule on the wire, tidied (24-hour times, days in order).
    #[must_use]
    pub fn to_wire(&self) -> ScheduleRule {
        let time = self.time.format("%H:%M").to_string();
        match self.days {
            Days::Once(date) => ScheduleRule::Once {
                at: format!("{}T{time}", date.format("%Y-%m-%d")),
            },
            Days::Daily => ScheduleRule::Daily { time },
            Days::Weekdays => ScheduleRule::Weekdays { time },
            Days::Weekly(mask) => ScheduleRule::Weekly {
                days: Day::ALL
                    .into_iter()
                    .filter(|d| mask & bit(weekday(*d)) != 0)
                    .collect(),
                time,
            },
            Days::Monthly(day) => ScheduleRule::Monthly { day, time },
        }
    }

    /// Whether it runs only once.
    #[must_use]
    pub fn is_once(&self) -> bool {
        matches!(self.days, Days::Once(_))
    }

    /// The rule in words: `Every weekday at 8:15 AM`.
    #[must_use]
    pub fn describe(&self) -> String {
        let at = clock(self.time);
        match self.days {
            Days::Once(date) => format!("Once on {} at {at}", date.format("%a, %b %-d, %Y")),
            Days::Daily => format!("Every day at {at}"),
            Days::Weekdays => format!("Every weekday at {at}"),
            Days::Weekly(mask) => {
                let names: Vec<&str> = Day::ALL
                    .into_iter()
                    .filter(|d| mask & bit(weekday(*d)) != 0)
                    .map(day_name)
                    .collect();
                format!("Every {} at {at}", and_list(&names))
            }
            Days::Monthly(day) if day >= 29 => {
                format!(
                    "On the {} of every month (or its last day) at {at}",
                    ordinal(day)
                )
            }
            Days::Monthly(day) => format!("On the {} of every month at {at}", ordinal(day)),
        }
    }

    /// Whether it runs on `date`.
    fn on(&self, date: NaiveDate) -> bool {
        match self.days {
            Days::Once(d) => d == date,
            Days::Daily => true,
            Days::Weekdays => MON_TO_FRI & bit(date.weekday()) != 0,
            Days::Weekly(mask) => mask & bit(date.weekday()) != 0,
            Days::Monthly(day) => u32::from(day).min(last_day(date)) == date.day(),
        }
    }

    /// Its first run after `after_ms` (ms since the Unix epoch) in `zone`,
    /// if it has one.
    #[must_use]
    pub fn next_after(&self, after_ms: i64, zone: &dyn Zone) -> Option<i64> {
        let today = local_date(after_ms, zone);
        let (first, days) = match self.days {
            Days::Once(date) => (date, 1),
            // A day early, in case the zone's offset puts the instant there;
            // 13 months covers a monthly day of 31.
            _ => (today.pred_opt()?, 400),
        };
        let mut date = first;
        for _ in 0..days {
            if self.on(date) {
                let at = instant(zone, date.and_time(self.time));
                if at > after_ms {
                    return Some(at);
                }
            }
            date = date.succ_opt()?;
        }
        None
    }

    /// Its runs after `after_ms` up to and including `until_ms`, oldest
    /// first, at most `cap` of them.
    #[must_use]
    pub fn runs_between(
        &self,
        after_ms: i64,
        until_ms: i64,
        zone: &dyn Zone,
        cap: usize,
    ) -> Vec<i64> {
        let mut out = Vec::new();
        let mut from = after_ms;
        while out.len() < cap {
            match self.next_after(from, zone) {
                Some(at) if at <= until_ms => {
                    out.push(at);
                    from = at;
                }
                _ => break,
            }
        }
        out
    }
}

/// What a check does with a schedule whose run is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatchUp {
    /// The run that runs now, if any: the most recent one due.
    pub run: Option<i64>,
    /// Older runs due meanwhile, noted as missed (oldest first, at most
    /// [`MAX_MISSED`]).
    pub missed: Vec<i64>,
    /// The run after them all, if the rule has one.
    pub next: Option<i64>,
}

impl Rule {
    /// What to do at `now_ms` with a schedule due at `first_ms` (at or
    /// before `now_ms`): its most recent due run runs once, if the run
    /// after it isn't due yet; older ones are missed.
    #[must_use]
    pub fn catch_up(&self, first_ms: i64, now_ms: i64, zone: &dyn Zone) -> CatchUp {
        let mut latest = first_ms;
        let mut missed = Vec::new();
        // A year of minutes is far more than any rule here has; this only
        // keeps a broken zone from looping for ever.
        for _ in 0..400_000 {
            match self.next_after(latest, zone) {
                Some(at) if at <= now_ms => {
                    if missed.len() < MAX_MISSED {
                        missed.push(latest);
                    }
                    latest = at;
                }
                _ => break,
            }
        }
        let next = self.next_after(now_ms.max(latest), zone);
        // The run after the latest isn't due yet: the latest runs.
        let runs = latest <= now_ms && self.next_after(latest, zone).is_none_or(|n| n > now_ms);
        if !runs {
            missed.push(latest);
            missed.truncate(MAX_MISSED);
        }
        CatchUp {
            run: runs.then_some(latest),
            missed,
            next,
        }
    }
}

/// Whether a run due at `due_ms` that starts at `started_ms` is late.
#[must_use]
pub fn is_late(due_ms: i64, started_ms: i64) -> bool {
    started_ms - due_ms > LATE_MS
}

/// What a late run says, e.g. `Ran at 11:04 AM; the home computer was
/// asleep or oxbot was closed at 8 AM.` (with the day when it differs).
#[must_use]
pub fn late_line(due_ms: i64, started_ms: i64, zone: &dyn Zone) -> String {
    let at = |ms: i64| {
        let local = ms + i64::from(zone.offset_at(ms)) * 1000;
        DateTime::from_timestamp_millis(local).map(|t| t.naive_utc())
    };
    let (Some(due), Some(started)) = (at(due_ms), at(started_ms)) else {
        return String::new();
    };
    let was = if due.date() == started.date() {
        format!("at {}", clock(due.time()))
    } else {
        format!("on {}", when(due_ms, zone))
    };
    format!(
        "Ran at {}; the home computer was asleep or oxbot was closed {was}.",
        clock(started.time())
    )
}

/// The date the wall shows at `utc_ms` in `zone`.
fn local_date(utc_ms: i64, zone: &dyn Zone) -> NaiveDate {
    let local = utc_ms + i64::from(zone.offset_at(utc_ms)) * 1000;
    DateTime::from_timestamp_millis(local).map_or(NaiveDate::MIN, |t| t.naive_utc().date())
}

/// When the wall in `zone` shows `local`: the first time if it shows it
/// twice, and as late as the jump if the clocks skip it.
fn instant(zone: &dyn Zone, local: NaiveDateTime) -> i64 {
    let wall = local.and_utc().timestamp_millis();
    match zone
        .offsets_for(local)
        .iter()
        .map(|o| wall - i64::from(*o) * 1000)
        .min()
    {
        Some(at) => at,
        // In a gap: the offset from before it carries the time past it.
        None => wall - i64::from(zone.offset_at(wall - 86_400_000)) * 1000,
    }
}

/// The last day of `date`'s month.
fn last_day(date: NaiveDate) -> u32 {
    let (y, m) = (date.year(), date.month());
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)
    };
    next.and_then(|d| d.pred_opt()).map_or(28, |d| d.day())
}

/// `8:15 AM`, `6 PM`, `12 PM` (noon).
fn clock(time: NaiveTime) -> String {
    let (pm, h) = time.hour12();
    let half = if pm { "PM" } else { "AM" };
    if time.minute() == 0 {
        format!("{h} {half}")
    } else {
        format!("{h}:{:02} {half}", time.minute())
    }
}

/// A run's time in words in `zone`, e.g. `Tue, Nov 4 at 8:15 AM`.
#[must_use]
pub fn when(utc_ms: i64, zone: &dyn Zone) -> String {
    let local = utc_ms + i64::from(zone.offset_at(utc_ms)) * 1000;
    DateTime::from_timestamp_millis(local).map_or_else(String::new, |t| {
        let t = t.naive_utc();
        format!("{} at {}", t.format("%a, %b %-d"), clock(t.time()))
    })
}

fn day_name(day: Day) -> &'static str {
    match day {
        Day::Mon => "Monday",
        Day::Tue => "Tuesday",
        Day::Wed => "Wednesday",
        Day::Thu => "Thursday",
        Day::Fri => "Friday",
        Day::Sat => "Saturday",
        Day::Sun => "Sunday",
    }
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `1st`, `2nd`, `3rd`, `11th`, `22nd`.
fn ordinal(n: u8) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    let mut s = n.to_string();
    let _ = write!(s, "{suffix}");
    s
}

/// A schedule's card line: `Every weekday at 8:15 AM: today's prep plan`.
#[must_use]
pub fn card_text(rule: &Rule, task: &str) -> String {
    format!("{}: {}", rule.describe(), one_line(task, 160))
}

/// `text` on one line, at most `max` characters (with `…` when cut).
#[must_use]
pub fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

/// A task as a schedule keeps it: trimmed, at most [`MAX_TASK`]
/// characters, no control characters but line breaks.
///
/// # Errors
/// An empty or too long task.
pub fn clean_task(task: &str) -> Result<String, RuleError> {
    let task: String = task
        .trim()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect();
    if task.is_empty() {
        return Err(bad(
            "A schedule needs a task: what the bot should do each time.",
        ));
    }
    if task.chars().count() > MAX_TASK {
        return Err(bad(format!(
            "A schedule's task is at most {MAX_TASK} characters."
        )));
    }
    Ok(task)
}

/// Test zones (tests only): fixed ones, and one with North American
/// daylight saving.
#[cfg(any(test, feature = "test-support"))]
pub mod zones {
    use chrono::Duration;

    use super::{Datelike, NaiveDate, NaiveDateTime, Weekday, Zone};

    /// Always `offset` seconds east of UTC.
    #[derive(Debug, Clone)]
    pub struct Fixed(pub &'static str, pub i32);

    impl Zone for Fixed {
        fn name(&self) -> String {
            self.0.to_owned()
        }
        fn offset_at(&self, _: i64) -> i32 {
            self.1
        }
        fn offsets_for(&self, _: NaiveDateTime) -> Vec<i32> {
            vec![self.1]
        }
    }

    /// Toronto's rules: UTC−5, and UTC−4 from 2:00 on March's second
    /// Sunday to 2:00 on November's first.
    #[derive(Debug, Clone, Copy)]
    pub struct Toronto;

    const STD: i32 = -5 * 3600;
    const DST: i32 = -4 * 3600;

    fn nth_sunday(year: i32, month: u32, n: u32) -> NaiveDate {
        let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(NaiveDate::MIN);
        let to_sun = (7 - first.weekday().num_days_from_sunday()) % 7;
        first + Duration::days(i64::from(to_sun + 7 * (n - 1)))
    }

    impl Zone for Toronto {
        fn name(&self) -> String {
            "America/Toronto".to_owned()
        }
        fn offset_at(&self, utc_ms: i64) -> i32 {
            let Some(t) = chrono::DateTime::from_timestamp_millis(utc_ms) else {
                return STD;
            };
            let y = t.year();
            let two = chrono::NaiveTime::from_hms_opt(2, 0, 0).unwrap_or_default();
            let start = nth_sunday(y, 3, 2)
                .and_time(two)
                .and_utc()
                .timestamp_millis()
                - i64::from(STD) * 1000;
            let end = nth_sunday(y, 11, 1)
                .and_time(two)
                .and_utc()
                .timestamp_millis()
                - i64::from(DST) * 1000;
            debug_assert_eq!(nth_sunday(y, 3, 2).weekday(), Weekday::Sun);
            if (start..end).contains(&utc_ms) {
                DST
            } else {
                STD
            }
        }
        fn offsets_for(&self, local: NaiveDateTime) -> Vec<i32> {
            let wall = local.and_utc().timestamp_millis();
            [DST, STD]
                .into_iter()
                .filter(|o| self.offset_at(wall - i64::from(*o) * 1000) == *o)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::zones::{Fixed, Toronto};
    use super::*;

    fn ms(text: &str) -> i64 {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis()
    }

    #[allow(clippy::needless_pass_by_value, reason = "tests build rules inline")]
    fn rule(r: ScheduleRule) -> Rule {
        Rule::parse(&r).unwrap()
    }

    fn weekly(days: &[Day], time: &str) -> Rule {
        rule(ScheduleRule::Weekly {
            days: days.to_vec(),
            time: time.into(),
        })
    }

    #[test]
    fn rules_read_in_plain_words() {
        let t = |time: &str| time.to_owned();
        assert_eq!(
            rule(ScheduleRule::Weekdays { time: t("08:15") }).describe(),
            "Every weekday at 8:15 AM"
        );
        assert_eq!(
            rule(ScheduleRule::Daily { time: t("12:00") }).describe(),
            "Every day at 12 PM"
        );
        assert_eq!(
            rule(ScheduleRule::Daily { time: t("0:05") }).describe(),
            "Every day at 12:05 AM"
        );
        assert_eq!(
            weekly(&[Day::Sat], "18:00").describe(),
            "Every Saturday at 6 PM"
        );
        assert_eq!(
            weekly(&[Day::Fri, Day::Mon, Day::Wed], "7:00").describe(),
            "Every Monday, Wednesday and Friday at 7 AM"
        );
        assert_eq!(weekly(&Day::ALL, "7:00").describe(), "Every day at 7 AM");
        assert_eq!(
            weekly(&[Day::Mon, Day::Tue, Day::Wed, Day::Thu, Day::Fri], "7:00").describe(),
            "Every weekday at 7 AM"
        );
        assert_eq!(
            rule(ScheduleRule::Monthly {
                day: 1,
                time: t("09:00")
            })
            .describe(),
            "On the 1st of every month at 9 AM"
        );
        assert_eq!(
            rule(ScheduleRule::Monthly {
                day: 31,
                time: t("09:30")
            })
            .describe(),
            "On the 31st of every month (or its last day) at 9:30 AM"
        );
        assert_eq!(
            rule(ScheduleRule::Once {
                at: t("2026-10-20T09:00")
            })
            .describe(),
            "Once on Tue, Oct 20, 2026 at 9 AM"
        );
        assert_eq!(
            card_text(
                &rule(ScheduleRule::Weekdays { time: t("08:15") }),
                "today's prep\n plan"
            ),
            "Every weekday at 8:15 AM: today's prep plan"
        );
        assert_eq!(ordinal(22), "22nd");
        assert_eq!(ordinal(13), "13th");
    }

    #[test]
    fn bad_rules_say_whats_wrong() {
        for (r, words) in [
            (
                ScheduleRule::Daily {
                    time: "25:00".into(),
                },
                "isn't a time",
            ),
            (ScheduleRule::Daily { time: "8".into() }, "isn't a time"),
            (
                ScheduleRule::Weekly {
                    days: vec![],
                    time: "08:00".into(),
                },
                "at least one day",
            ),
            (
                ScheduleRule::Monthly {
                    day: 0,
                    time: "08:00".into(),
                },
                "1 to 31",
            ),
            (
                ScheduleRule::Monthly {
                    day: 32,
                    time: "08:00".into(),
                },
                "1 to 31",
            ),
            (
                ScheduleRule::Once {
                    at: "tomorrow".into(),
                },
                "isn't a date",
            ),
            (
                ScheduleRule::Once {
                    at: "2026-02-30T09:00".into(),
                },
                "isn't a date",
            ),
        ] {
            let err = Rule::parse(&r).unwrap_err();
            assert!(err.0.contains(words), "{r:?}: {err}");
        }
        assert_eq!(
            parse_time("6 pm").unwrap(),
            NaiveTime::from_hms_opt(18, 0, 0).unwrap()
        );
        assert_eq!(
            parse_time("12:30am").unwrap(),
            NaiveTime::from_hms_opt(0, 30, 0).unwrap()
        );
        assert!(parse_time("13 pm").is_err());
        assert_eq!(
            parse_at("2026-10-20 09:00:59").unwrap().1,
            NaiveTime::from_hms_opt(9, 0, 0).unwrap()
        );
    }

    #[test]
    fn a_bots_rule_is_read_leniently_and_tidied() {
        let (r, wire) = Rule::from_tool(&serde_json::json!({
            "kind": "Weekly", "days": ["Saturday", "sun", "SAT"], "time": "6 PM"
        }))
        .unwrap();
        assert_eq!(
            wire,
            ScheduleRule::Weekly {
                days: vec![Day::Sat, Day::Sun],
                time: "18:00".into()
            }
        );
        assert_eq!(r.describe(), "Every Saturday and Sunday at 6 PM");
        for (v, words) in [
            (
                serde_json::json!({"kind": "hourly", "time": "08:00"}),
                "no other repeats",
            ),
            (serde_json::json!({"kind": "daily"}), "rule.time"),
            (
                serde_json::json!({"kind": "weekly", "time": "08:00"}),
                "rule.days",
            ),
            (
                serde_json::json!({"kind": "weekly", "days": ["funday"], "time": "08:00"}),
                "isn't a day",
            ),
            (
                serde_json::json!({"kind": "monthly", "time": "08:00"}),
                "rule.day",
            ),
            (serde_json::json!({"kind": "once"}), "rule.at"),
        ] {
            let err = Rule::from_tool(&v).unwrap_err();
            assert!(err.0.contains(words), "{v}: {err}");
        }
    }

    #[test]
    fn weekdays_skip_the_weekend() {
        let utc = Fixed("UTC", 0);
        let r = rule(ScheduleRule::Weekdays {
            time: "08:15".into(),
        });
        // Friday 2026-11-06 at 9:00: next is Monday the 9th.
        assert_eq!(
            r.next_after(ms("2026-11-06T09:00:00Z"), &utc),
            Some(ms("2026-11-09T08:15:00Z"))
        );
        // Friday at 8:00: later that day.
        assert_eq!(
            r.next_after(ms("2026-11-06T08:00:00Z"), &utc),
            Some(ms("2026-11-06T08:15:00Z"))
        );
        // Exactly at the run: the next one.
        assert_eq!(
            r.next_after(ms("2026-11-06T08:15:00Z"), &utc),
            Some(ms("2026-11-09T08:15:00Z"))
        );
        let sat = weekly(&[Day::Sat], "18:00");
        assert_eq!(
            sat.next_after(ms("2026-11-04T12:00:00Z"), &utc),
            Some(ms("2026-11-07T18:00:00Z"))
        );
    }

    #[test]
    fn a_zone_decides_the_day_and_the_hour() {
        let tokyo = Fixed("Asia/Tokyo", 9 * 3600);
        let r = rule(ScheduleRule::Daily {
            time: "07:00".into(),
        });
        // 23:00 UTC is already 8:00 the next day in Tokyo: 7:00 the day after.
        assert_eq!(
            r.next_after(ms("2026-11-04T23:00:00Z"), &tokyo),
            Some(ms("2026-11-05T22:00:00Z"))
        );
        assert_eq!(
            when(ms("2026-11-05T22:00:00Z"), &tokyo),
            "Fri, Nov 6 at 7 AM"
        );
    }

    #[test]
    fn month_ends_and_leap_years() {
        let utc = Fixed("UTC", 0);
        let r = rule(ScheduleRule::Monthly {
            day: 31,
            time: "09:00".into(),
        });
        let runs = r.runs_between(
            ms("2027-01-01T00:00:00Z"),
            ms("2027-05-31T23:00:00Z"),
            &utc,
            10,
        );
        assert_eq!(
            runs,
            [
                ms("2027-01-31T09:00:00Z"),
                ms("2027-02-28T09:00:00Z"),
                ms("2027-03-31T09:00:00Z"),
                ms("2027-04-30T09:00:00Z"),
                ms("2027-05-31T09:00:00Z"),
            ]
        );
        let leap = rule(ScheduleRule::Monthly {
            day: 30,
            time: "09:00".into(),
        });
        assert_eq!(
            leap.next_after(ms("2028-02-01T00:00:00Z"), &utc),
            Some(ms("2028-02-29T09:00:00Z"))
        );
        let first = rule(ScheduleRule::Monthly {
            day: 1,
            time: "00:00".into(),
        });
        assert_eq!(
            first.next_after(ms("2026-12-15T00:00:00Z"), &utc),
            Some(ms("2027-01-01T00:00:00Z"))
        );
    }

    #[test]
    fn once_runs_once() {
        let utc = Fixed("UTC", 0);
        let r = rule(ScheduleRule::Once {
            at: "2026-10-20T09:00".into(),
        });
        assert_eq!(
            r.next_after(ms("2026-10-01T00:00:00Z"), &utc),
            Some(ms("2026-10-20T09:00:00Z"))
        );
        assert_eq!(r.next_after(ms("2026-10-20T09:00:00Z"), &utc), None);
        assert!(r.is_once());
    }

    #[test]
    fn daylight_saving_skips_a_gap_and_runs_once_in_an_overlap() {
        let zone = Toronto;
        // 2026: clocks go forward on March 8, back on November 1.
        let early = rule(ScheduleRule::Daily {
            time: "02:30".into(),
        });
        // 2:30 doesn't exist on March 8: it runs at 3:30 EDT (07:30 UTC).
        assert_eq!(
            early.next_after(ms("2026-03-07T12:00:00Z"), &zone),
            Some(ms("2026-03-08T07:30:00Z"))
        );
        // The day before and after, 2:30 as usual (EST then EDT).
        assert_eq!(
            early.next_after(ms("2026-03-06T12:00:00Z"), &zone),
            Some(ms("2026-03-07T07:30:00Z"))
        );
        assert_eq!(
            early.next_after(ms("2026-03-08T12:00:00Z"), &zone),
            Some(ms("2026-03-09T06:30:00Z"))
        );
        // 1:30 happens twice on November 1: once, the first time (EDT).
        let night = rule(ScheduleRule::Daily {
            time: "01:30".into(),
        });
        let runs = night.runs_between(
            ms("2026-10-31T12:00:00Z"),
            ms("2026-11-02T12:00:00Z"),
            &zone,
            10,
        );
        assert_eq!(
            runs,
            [ms("2026-11-01T05:30:00Z"), ms("2026-11-02T06:30:00Z")]
        );
        // 8:15 keeps its wall time across the change.
        let r = rule(ScheduleRule::Weekdays {
            time: "08:15".into(),
        });
        assert_eq!(
            r.next_after(ms("2026-10-30T13:00:00Z"), &zone),
            Some(ms("2026-11-02T13:15:00Z"))
        );
        assert_eq!(
            r.next_after(ms("2026-10-29T13:00:00Z"), &zone),
            Some(ms("2026-10-30T12:15:00Z"))
        );
        assert_eq!(zone.name(), "America/Toronto");
    }

    #[test]
    fn a_run_three_hours_late_runs_once() {
        let utc = Fixed("UTC", 0);
        let r = rule(ScheduleRule::Daily {
            time: "08:00".into(),
        });
        let due = ms("2026-11-07T08:00:00Z");
        let c = r.catch_up(due, ms("2026-11-07T11:04:00Z"), &utc);
        assert_eq!(c.run, Some(due));
        assert_eq!(c.missed, Vec::<i64>::new());
        assert_eq!(c.next, Some(ms("2026-11-08T08:00:00Z")));
    }

    #[test]
    fn days_of_missed_daily_runs_run_the_newest_once() {
        let utc = Fixed("UTC", 0);
        let r = rule(ScheduleRule::Daily {
            time: "08:00".into(),
        });
        let c = r.catch_up(ms("2026-11-05T08:00:00Z"), ms("2026-11-07T11:00:00Z"), &utc);
        assert_eq!(c.run, Some(ms("2026-11-07T08:00:00Z")));
        assert_eq!(
            c.missed,
            [ms("2026-11-05T08:00:00Z"), ms("2026-11-06T08:00:00Z")]
        );
        assert_eq!(c.next, Some(ms("2026-11-08T08:00:00Z")));
    }

    #[test]
    fn a_weekly_run_a_day_late_still_runs() {
        let utc = Fixed("UTC", 0);
        let r = weekly(&[Day::Sat], "08:00");
        let due = ms("2026-11-07T08:00:00Z");
        let c = r.catch_up(due, ms("2026-11-08T09:30:00Z"), &utc);
        assert_eq!(c.run, Some(due));
        assert_eq!(c.missed, Vec::<i64>::new());
        assert_eq!(c.next, Some(ms("2026-11-14T08:00:00Z")));
    }

    #[test]
    fn an_on_time_run_is_unchanged() {
        let utc = Fixed("UTC", 0);
        let r = rule(ScheduleRule::Daily {
            time: "08:15".into(),
        });
        let due = ms("2026-11-07T08:15:00Z");
        let now = due + 50;
        let c = r.catch_up(due, now, &utc);
        assert_eq!(c.run, Some(due));
        assert_eq!(c.missed, Vec::<i64>::new());
        assert!(!is_late(due, now));
        // A one-time schedule runs late too, then has no next run.
        let once = rule(ScheduleRule::Once {
            at: "2026-11-07T08:00".into(),
        });
        let c = once.catch_up(ms("2026-11-07T08:00:00Z"), ms("2026-11-09T08:00:00Z"), &utc);
        assert_eq!((c.run.is_some(), c.next), (true, None));
    }

    #[test]
    fn a_late_run_says_when_it_ran_and_when_it_was_due() {
        let utc = Fixed("UTC", 0);
        let due = ms("2026-11-07T08:00:00Z");
        assert!(!is_late(due, due + LATE_MS));
        assert!(is_late(due, due + LATE_MS + 1));
        assert_eq!(
            late_line(due, ms("2026-11-07T11:04:00Z"), &utc),
            "Ran at 11:04 AM; the home computer was asleep or oxbot was closed at 8 AM."
        );
        assert_eq!(
            late_line(due, ms("2026-11-08T09:30:00Z"), &utc),
            "Ran at 9:30 AM; the home computer was asleep or oxbot was closed on Sat, Nov 7 at 8 AM."
        );
    }

    #[test]
    fn missed_runs_are_listed_up_to_a_cap() {
        let utc = Fixed("UTC", 0);
        let r = rule(ScheduleRule::Daily {
            time: "08:00".into(),
        });
        let three = r.runs_between(
            ms("2026-11-01T09:00:00Z"),
            ms("2026-11-04T09:00:00Z"),
            &utc,
            10,
        );
        assert_eq!(three.len(), 3);
        assert_eq!(
            r.runs_between(
                ms("2026-01-01T00:00:00Z"),
                ms("2026-12-31T00:00:00Z"),
                &utc,
                5
            )
            .len(),
            5
        );
    }

    #[test]
    fn tasks_are_kept_clean_and_short() {
        assert_eq!(clean_task("  plan\u{7}\n day ").unwrap(), "plan\n day");
        assert!(clean_task("  ").is_err());
        assert!(clean_task(&"x".repeat(MAX_TASK + 1)).is_err());
        assert_eq!(one_line("a\n b", 10), "a b");
        assert_eq!(one_line("abcdef", 4), "abc…");
    }

    #[test]
    fn the_macs_zone_has_a_name_and_an_offset() {
        let mac = MacZone;
        assert_ne!(mac.name(), "");
        let now = chrono::Utc::now().timestamp_millis();
        assert!(mac.offset_at(now).abs() <= 14 * 3600);
    }
}
