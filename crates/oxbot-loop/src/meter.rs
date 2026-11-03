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
    let days = ms.div_euclid(86_400_000);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
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
