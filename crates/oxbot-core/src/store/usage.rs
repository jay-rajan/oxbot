//! Usage: Claude's tokens and cost per bot, and the plan's limits.

use oxbot_claude::{LimitWindow, TurnUsage};
use oxbot_protocol::{BotUsage, PlanLimit, Usage};
use rusqlite::params;

use super::{Store, StoreError, now_ms};
use crate::id::BotId;

impl Store {
    /// Adds one finished turn's tokens and cost to `bot`'s totals.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_usage(&self, bot: &BotId, turn: &TurnUsage) -> Result<(), StoreError> {
        let n = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
        self.conn().execute(
            "INSERT INTO usage (bot, turns, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd)
             VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(bot) DO UPDATE SET
               turns = turns + 1,
               input_tokens = input_tokens + excluded.input_tokens,
               output_tokens = output_tokens + excluded.output_tokens,
               cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens,
               cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens,
               cost_usd = cost_usd + excluded.cost_usd",
            params![
                bot.as_str(),
                n(turn.input_tokens),
                n(turn.output_tokens),
                n(turn.cache_read_tokens),
                n(turn.cache_write_tokens),
                turn.cost_usd
            ],
        )?;
        Ok(())
    }

    /// Replaces the plan's limits with what Claude just reported.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_limits(&self, windows: &[LimitWindow]) -> Result<(), StoreError> {
        let seen = now_ms();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM limits", [])?;
        for w in windows {
            tx.execute(
                "INSERT INTO limits (name, used, resets_at_ms, status, seen_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![w.name, w.used, w.resets_at.saturating_mul(1000), w.status, seen],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The plan's limits and every bot's totals, most used bot first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn usage(&self) -> Result<Usage, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT name, used, resets_at_ms, status, seen_ms FROM limits ORDER BY resets_at_ms",
        )?;
        let mut limits_seen_ms = 0;
        let limits = stmt
            .query_map([], |r| {
                Ok((
                    PlanLimit {
                        name: r.get(0)?,
                        used: r.get(1)?,
                        resets_at_ms: r.get(2)?,
                        status: r.get(3)?,
                    },
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(limit, seen)| {
                limits_seen_ms = limits_seen_ms.max(seen);
                limit
            })
            .collect();
        let mut stmt = conn.prepare(
            "SELECT bot, turns, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd
             FROM usage ORDER BY input_tokens + output_tokens + cache_read_tokens + cache_write_tokens DESC",
        )?;
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        let bots = stmt
            .query_map([], |r| {
                Ok(BotUsage {
                    bot: r.get(0)?,
                    turns: n(r.get(1)?),
                    input_tokens: n(r.get(2)?),
                    output_tokens: n(r.get(3)?),
                    cache_read_tokens: n(r.get(4)?),
                    cache_write_tokens: n(r.get(5)?),
                    cost_usd: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Usage {
            limits,
            limits_seen_ms,
            bots,
        })
    }
}
