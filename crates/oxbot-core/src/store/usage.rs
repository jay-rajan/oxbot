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

    /// Keeps the plan's limits Claude just reported. A window it didn't
    /// mention this time keeps its last numbers.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_limits(&self, windows: &[LimitWindow]) -> Result<(), StoreError> {
        let seen = now_ms();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for w in windows {
            tx.execute(
                "INSERT INTO limits (name, used, resets_at_ms, status, seen_ms) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(name) DO UPDATE SET
                   used = excluded.used,
                   resets_at_ms = excluded.resets_at_ms,
                   status = excluded.status,
                   seen_ms = excluded.seen_ms",
                params![w.name, w.used, w.resets_at.saturating_mul(1000), w.status, seen],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Notes the model `bot`'s Claude said it uses, e.g.
    /// `claude-sonnet-4-5-20250929`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_model(&self, bot: &BotId, model: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO usage (bot, model) VALUES (?1, ?2)
             ON CONFLICT(bot) DO UPDATE SET model = excluded.model",
            params![bot.as_str(), model],
        )?;
        Ok(())
    }

    /// Each bot's last model, for bots whose Claude has said one.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn bot_models(&self) -> Result<Vec<(String, String)>, StoreError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT bot, model FROM usage WHERE model <> '' ORDER BY bot")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
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
