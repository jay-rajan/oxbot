//! The meter: one [`CallRecord`] per model request (success or not), and
//! the host's limits ([`UsageGuard`]), checked before every request.

use std::time::{SystemTime, UNIX_EPOCH};

use oxbot_llm::{ErrorKind, LlmError};

/// What a limit check is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardQuery<'a> {
    /// The bot.
    pub bot: &'a str,
    /// The person its task chain acts for.
    pub person: Option<&'a str>,
    /// Whether the next request goes to a free model.
    pub free: bool,
}

/// A limit was reached; the turn ends with this message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LimitHit(pub String);

/// The host's limits on model requests (per bot, per person, money).
pub trait UsageGuard: Send + Sync {
    /// Whether one more request is allowed.
    ///
    /// # Errors
    /// [`LimitHit`] with what to tell the person.
    fn check(&self, query: &GuardQuery<'_>) -> Result<(), LimitHit>;
}

/// No limits (tests, and hosts that keep none).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLimits;

impl UsageGuard for NoLimits {
    fn check(&self, _: &GuardQuery<'_>) -> Result<(), LimitHit> {
        Ok(())
    }
}

/// Milliseconds since the Unix epoch, now.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The UTC day of `ms`, `YYYY-MM-DD`.
#[must_use]
pub fn utc_day(ms: i64) -> String {
    let (year, month, day) = civil(ms.div_euclid(86_400_000));
    format!("{year:04}-{month:02}-{day:02}")
}

/// The first day of `ms`'s UTC month, `YYYY-MM-01` (spending is counted
/// by the month from it).
#[must_use]
pub fn utc_month_start(ms: i64) -> String {
    let (year, month, _) = civil(ms.div_euclid(86_400_000));
    format!("{year:04}-{month:02}-01")
}

/// Milliseconds since the Unix epoch of 00:00 UTC on the first day of the
/// month after `ms`'s.
#[must_use]
pub fn next_utc_month_ms(ms: i64) -> i64 {
    let (year, month, _) = civil(ms.div_euclid(86_400_000));
    let (year, month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    days_from_civil(year, month, 1) * 86_400_000
}

/// Year, month and day of `days` since 1970-01-01 (Howard Hinnant's
/// civil-from-days).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days since 1970-01-01 of a date (Howard Hinnant's days-from-civil).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The meter's outcome for a request that failed with `err`.
#[must_use]
pub fn outcome_of(err: &LlmError) -> String {
    match err.kind {
        ErrorKind::RateLimited => "rate_limited".to_owned(),
        ErrorKind::Overloaded => "busy".to_owned(),
        ErrorKind::FreeDailyLimit => "daily_limit".to_owned(),
        ErrorKind::Cancelled => "cancelled".to_owned(),
        kind => format!("error:{}", kind.as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_days() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(951_782_400_000), "2000-02-29");
        assert_eq!(utc_day(1_793_577_599_999), "2026-11-01");
        assert_eq!(utc_day(1_793_577_600_000), "2026-11-02");
        assert_eq!(utc_day(-1), "1969-12-31");
    }

    #[test]
    fn utc_months() {
        // 2026-11-01 00:00 UTC, and the last moment of October.
        let nov = 1_793_491_200_000;
        assert_eq!(utc_month_start(nov), "2026-11-01");
        assert_eq!(utc_month_start(nov - 1), "2026-10-01");
        assert_eq!(utc_month_start(1_793_577_600_000), "2026-11-01");
        assert_eq!(next_utc_month_ms(nov - 1), nov);
        assert_eq!(next_utc_month_ms(nov), nov + 30 * 86_400_000);
        // December rolls into the next year; February in a leap year.
        assert_eq!(
            utc_day(next_utc_month_ms(nov + 31 * 86_400_000)),
            "2027-01-01"
        );
        assert_eq!(utc_day(next_utc_month_ms(951_782_400_000)), "2000-03-01");
        assert_eq!(next_utc_month_ms(0), 31 * 86_400_000);
        assert_eq!(next_utc_month_ms(-1), 0);
    }

    #[test]
    fn outcomes() {
        assert_eq!(
            outcome_of(&LlmError::new(ErrorKind::RateLimited, "x")),
            "rate_limited"
        );
        assert_eq!(
            outcome_of(&LlmError::new(ErrorKind::Overloaded, "x")),
            "busy"
        );
        assert_eq!(
            outcome_of(&LlmError::new(ErrorKind::Server, "x")),
            "error:server"
        );
        assert_eq!(outcome_of(&LlmError::cancelled()), "cancelled");
    }
}
