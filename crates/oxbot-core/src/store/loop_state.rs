//! What Codex bots' tools and requests keep in the database: every model
//! request (`model_calls`) with the sums the limits and the usage view
//! read, stored tool results (`loop_results`) and answer checks
//! (`loop_checks`). The tables are named from oxbot's own loop, which
//! retired; its conversations and turns (`loop_sessions`, `loop_messages`,
//! `loop_turns`) are only pruned.

use oxbot_engine::CallRecord;
use oxbot_loop::store::{CheckRow, LoopStore, LoopStoreError, ModelStat, ResultKind, ResultRow};
use rusqlite::{OptionalExtension as _, params};

#[cfg(test)]
use super::now_ms;
use super::{Store, StoreError};

/// How long model requests are kept, in days.
pub const KEEP_CALLS_DAYS: i64 = 400;
/// How long the messages of a closed conversation of oxbot's retired loop
/// are kept, in days.
pub const KEEP_CLOSED_DAYS: i64 = 30;

/// How long stored tool results and answer checks are kept after their
/// session's last turn, in days (the results' files follow the same rule).
pub const KEEP_RESULTS_DAYS: i64 = oxbot_loop::results::KEEP_DAYS;

const DAY_MS: i64 = 86_400_000;

/// Model requests of one bot (or person) on one day.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallTotals {
    /// Requests sent.
    pub requests: u32,
    /// Input tokens.
    pub prompt_tokens: u64,
    /// Output tokens.
    pub completion_tokens: u64,
    /// US dollars.
    pub cost_usd: f64,
    /// The model that answered most.
    pub main_model: String,
}

/// Requests per bot and per person on one day.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DayUsage {
    /// By bot id.
    pub bots: Vec<(String, CallTotals)>,
    /// By person id.
    pub people: Vec<(String, CallTotals)>,
}

fn err(e: impl std::fmt::Display) -> LoopStoreError {
    LoopStoreError(e.to_string())
}

fn to_i64(n: impl TryInto<i64>) -> i64 {
    n.try_into().unwrap_or(i64::MAX)
}

fn to_u32(n: i64) -> u32 {
    u32::try_from(n.max(0)).unwrap_or(u32::MAX)
}

fn to_u64(n: i64) -> u64 {
    u64::try_from(n.max(0)).unwrap_or(0)
}

impl LoopStore for Store {
    fn record_call(&self, c: &CallRecord) -> Result<(), LoopStoreError> {
        self.conn()
            .execute(
                "INSERT INTO model_calls
                   (ts_ms, day, bot, person, session, turn, step, provider, requested, model, free,
                    private, generation_id, prompt_tokens, completion_tokens, cached_tokens,
                    cost_usd, latency_ms, outcome, kind, engine)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21)",
                params![
                    c.ts_ms,
                    c.day,
                    c.bot,
                    c.person,
                    c.session,
                    c.turn,
                    c.step,
                    c.provider,
                    c.requested,
                    c.model,
                    c.free,
                    c.private,
                    c.generation_id,
                    to_i64(c.prompt_tokens),
                    to_i64(c.completion_tokens),
                    to_i64(c.cached_tokens),
                    c.cost_usd,
                    to_i64(c.latency_ms),
                    c.outcome,
                    c.kind.as_str(),
                    c.engine,
                ],
            )
            .map_err(err)?;
        Ok(())
    }

    fn model_stats(&self, since_ms: i64) -> Result<Vec<ModelStat>, LoopStoreError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT CASE WHEN model = '' THEN requested ELSE model END AS m,
                        SUM(outcome = 'ok'), COUNT(*),
                        COALESCE(AVG(CASE WHEN outcome = 'ok' THEN latency_ms END), 0)
                 FROM model_calls
                 WHERE ts_ms >= ?1 AND provider = 'openrouter'
                 GROUP BY m",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([since_ms], |r| {
                Ok(ModelStat {
                    model: r.get(0)?,
                    ok: to_u32(r.get(1)?),
                    total: to_u32(r.get(2)?),
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    avg_latency_ms: r.get::<_, f64>(3)?.max(0.0) as u64,
                })
            })
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    fn load_pref(&self, key: &str) -> Result<Option<String>, LoopStoreError> {
        self.pref(key).map_err(err)
    }

    fn save_pref(&self, key: &str, value: &str) -> Result<(), LoopStoreError> {
        self.set_pref(key, value).map_err(err)
    }

    fn save_result(&self, r: &ResultRow) -> Result<(), LoopStoreError> {
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO loop_results
                   (session, handle, bot, turn, step, tool, kind, page_url, page_title, bytes,
                    est_tokens, tainted_by, created_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    r.session,
                    r.handle,
                    r.bot,
                    r.turn,
                    r.step,
                    r.tool,
                    r.kind.as_str(),
                    r.page_url,
                    r.page_title,
                    to_i64(r.bytes),
                    to_i64(r.est_tokens),
                    r.tainted_by,
                    r.created_ms,
                ],
            )
            .map_err(err)?;
        Ok(())
    }

    fn result(&self, session: &str, handle: &str) -> Result<Option<ResultRow>, LoopStoreError> {
        self.conn()
            .query_row(
                "SELECT * FROM loop_results WHERE session = ?1 AND handle = ?2",
                params![session, handle],
                |r| {
                    Ok(ResultRow {
                        session: r.get("session")?,
                        handle: r.get("handle")?,
                        bot: r.get("bot")?,
                        turn: to_u32(r.get("turn")?),
                        step: to_u32(r.get("step")?),
                        tool: r.get("tool")?,
                        kind: ResultKind::parse(&r.get::<_, String>("kind")?),
                        page_url: r.get("page_url")?,
                        page_title: r.get("page_title")?,
                        bytes: to_u64(r.get("bytes")?),
                        est_tokens: to_u64(r.get("est_tokens")?),
                        tainted_by: r.get("tainted_by")?,
                        created_ms: r.get("created_ms")?,
                    })
                },
            )
            .optional()
            .map_err(err)
    }

    fn last_result_number(&self, session: &str, prefix: char) -> Result<u32, LoopStoreError> {
        let n: Option<i64> = self
            .conn()
            .query_row(
                "SELECT MAX(CAST(SUBSTR(handle, 2) AS INTEGER)) FROM loop_results
                 WHERE session = ?1 AND SUBSTR(handle, 1, 1) = ?2",
                params![session, prefix.to_string()],
                |r| r.get(0),
            )
            .map_err(err)?;
        Ok(to_u32(n.unwrap_or(0)))
    }

    fn record_check(&self, row: &CheckRow) -> Result<(), LoopStoreError> {
        let list = |v: &[String]| serde_json::to_string(v).unwrap_or_else(|_| "[]".to_owned());
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO loop_checks
                   (session, turn, bounced, ungrounded_before, ungrounded_after, ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    row.session,
                    row.turn,
                    row.bounced,
                    list(&row.ungrounded_before),
                    list(&row.ungrounded_after),
                    row.ms,
                ],
            )
            .map_err(err)?;
        Ok(())
    }
}

impl Store {
    /// `OpenRouter` requests `bot` sent on UTC `day`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn bot_requests(&self, day: &str, bot: &str) -> Result<u32, StoreError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM model_calls
             WHERE day = ?1 AND bot = ?2 AND provider = 'openrouter' AND outcome != 'cancelled'",
            params![day, bot],
            |r| r.get(0),
        )?;
        Ok(to_u32(n))
    }

    /// `OpenRouter` requests made for `person` on UTC `day`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_requests(&self, day: &str, person: &str) -> Result<u32, StoreError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM model_calls
             WHERE day = ?1 AND person = ?2 AND provider = 'openrouter' AND outcome != 'cancelled'",
            params![day, person],
            |r| r.get(0),
        )?;
        Ok(to_u32(n))
    }

    /// Free `OpenRouter` requests on UTC `day`, household-wide.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn free_requests(&self, day: &str) -> Result<u32, StoreError> {
        let n: i64 = self.conn().query_row(
            "SELECT COUNT(*) FROM model_calls
             WHERE day = ?1 AND free = 1 AND provider = 'openrouter' AND outcome != 'cancelled'",
            [day],
            |r| r.get(0),
        )?;
        Ok(to_u32(n))
    }

    /// US dollars spent on `OpenRouter` on UTC `day`, household-wide.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn spend(&self, day: &str) -> Result<f64, StoreError> {
        Ok(self.conn().query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM model_calls
             WHERE day = ?1 AND provider = 'openrouter'",
            [day],
            |r| r.get(0),
        )?)
    }

    /// US dollars spent on `OpenRouter` on UTC days from `first_day`
    /// (`YYYY-MM-DD`) on, household-wide: this month's, from its first day.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn spend_since(&self, first_day: &str) -> Result<f64, StoreError> {
        Ok(self.conn().query_row(
            "SELECT COALESCE(SUM(cost_usd), 0) FROM model_calls
             WHERE day >= ?1 AND provider = 'openrouter'",
            [first_day],
            |r| r.get(0),
        )?)
    }

    /// Requests, tokens, money and main model per bot and per person on
    /// UTC `day`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn day_usage(&self, day: &str) -> Result<DayUsage, StoreError> {
        Ok(DayUsage {
            bots: self.totals_by(day, "bot")?,
            people: self.totals_by(day, "person")?,
        })
    }

    fn totals_by(&self, day: &str, column: &str) -> Result<Vec<(String, CallTotals)>, StoreError> {
        let column = if column == "bot" { "bot" } else { "person" };
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {column}, COUNT(*), SUM(prompt_tokens), SUM(completion_tokens), SUM(cost_usd),
                    (SELECT model FROM model_calls m2
                     WHERE m2.day = m.day AND m2.{column} = m.{column} AND m2.model != ''
                       AND m2.provider != 'anthropic'
                     GROUP BY model ORDER BY COUNT(*) DESC, model LIMIT 1)
             FROM model_calls m
             WHERE day = ?1 AND {column} IS NOT NULL AND outcome != 'cancelled'
               AND provider != 'anthropic'
             GROUP BY {column} ORDER BY {column}"
        ))?;
        let rows = stmt.query_map([day], |r| {
            Ok((
                r.get::<_, String>(0)?,
                CallTotals {
                    requests: to_u32(r.get(1)?),
                    prompt_tokens: to_u64(r.get(2)?),
                    completion_tokens: to_u64(r.get(3)?),
                    cost_usd: r.get(4)?,
                    main_model: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                },
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// When `session` (a bot's Codex session) last had a turn, if it has
    /// any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn session_last_ms(&self, session: &str) -> Result<Option<i64>, StoreError> {
        Ok(self.conn().query_row(
            "SELECT MAX(updated_ms) FROM codex_threads WHERE session = ?1",
            [session],
            |r| r.get(0),
        )?)
    }

    /// Forgets the stored results of `bot`'s sessions whose folders were
    /// deleted (`folders`: session ids as folder names).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn forget_loop_results(&self, bot: &str, folders: &[String]) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let sessions: Vec<String> = {
            let mut stmt =
                tx.prepare("SELECT DISTINCT session FROM loop_results WHERE bot = ?1")?;
            let rows = stmt.query_map([bot], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut n = 0;
        for session in sessions {
            if folders.contains(&oxbot_loop::results::folder_name(&session)) {
                n += tx.execute(
                    "DELETE FROM loop_results WHERE session = ?1 AND bot = ?2",
                    params![session, bot],
                )?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Deletes model requests older than [`KEEP_CALLS_DAYS`], the messages
    /// of the retired loop's conversations closed more than
    /// [`KEEP_CLOSED_DAYS`] ago, and stored results and answer checks of
    /// sessions with no turn for [`KEEP_RESULTS_DAYS`] (a bot's current
    /// session keeps its own). Returns how many rows went.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn prune_loop(&self, now_ms: i64) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let calls = tx.execute(
            "DELETE FROM model_calls WHERE ts_ms < ?1",
            [now_ms - KEEP_CALLS_DAYS * DAY_MS],
        )?;
        let messages = tx.execute(
            "DELETE FROM loop_messages WHERE session IN (
                SELECT id FROM loop_sessions WHERE closed_ms IS NOT NULL AND closed_ms < ?1)",
            [now_ms - KEEP_CLOSED_DAYS * DAY_MS],
        )?;
        // Stored results and checks go with their files: once no turn of
        // their session has moved for the retention time. A bot's current
        // session keeps them (the sweep keeps its files too).
        let mut context = 0;
        for table in ["loop_results", "loop_checks"] {
            context += tx.execute(
                &format!(
                    "DELETE FROM {table}
                     WHERE session NOT IN (
                           SELECT session FROM codex_threads GROUP BY session
                           HAVING MAX(updated_ms) >= ?1)
                       AND session NOT IN (SELECT claude_session FROM sessions)"
                ),
                [now_ms - KEEP_RESULTS_DAYS * DAY_MS],
            )?;
        }
        tx.commit()?;
        Ok(calls + messages + context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_engine::CallKind;

    #[test]
    fn stored_results_go_with_their_session_but_not_the_current_one() {
        use oxbot_codex::{CodexStore as _, ThreadRow};
        let store = Store::open_in_memory().unwrap();
        let row = |session: &str| ResultRow {
            session: session.into(),
            handle: "r1".into(),
            bot: "scout".into(),
            turn: 0,
            step: 1,
            tool: "Read".into(),
            kind: ResultKind::Text,
            page_url: None,
            page_title: None,
            bytes: 3,
            est_tokens: 1,
            tainted_by: None,
            created_ms: 1,
        };
        let thread = |session: &str| ThreadRow {
            bot: "scout".into(),
            session: session.into(),
            thread_id: format!("t-{session}"),
            model: "a:free".into(),
            window: 64_000,
            turns: 1,
            created_ms: now_ms(),
            updated_ms: now_ms(),
        };
        for session in ["old", "current"] {
            store.save_thread(&thread(session)).unwrap();
            store.save_result(&row(session)).unwrap();
            store
                .record_check(&CheckRow {
                    session: session.into(),
                    turn: 0,
                    bounced: false,
                    ungrounded_before: Vec::new(),
                    ungrounded_after: Vec::new(),
                    ms: 0,
                })
                .unwrap();
        }
        store
            .set_session(
                &crate::BotId::new("scout").unwrap(),
                Some(&super::super::SavedSession {
                    id: "current".into(),
                    prompt: "h".into(),
                }),
            )
            .unwrap();
        let checks = |s: &Store| -> i64 {
            s.conn()
                .query_row("SELECT COUNT(*) FROM loop_checks", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(store.prune_loop(now_ms()).unwrap(), 0);
        assert!(store.session_last_ms("old").unwrap().is_some());
        let later = now_ms() + (KEEP_RESULTS_DAYS + 1) * DAY_MS;
        store.prune_loop(later).unwrap();
        assert_eq!(store.result("old", "r1").unwrap(), None);
        assert!(store.result("current", "r1").unwrap().is_some());
        assert_eq!(checks(&store), 1, "the current session's check stays");
        assert_eq!(store.session_last_ms("never").unwrap(), None);
    }

    fn call(bot: &str, person: Option<&str>, model: &str, outcome: &str, cost: f64) -> CallRecord {
        CallRecord {
            ts_ms: now_ms(),
            day: "2026-11-02".into(),
            bot: bot.into(),
            person: person.map(Into::into),
            provider: "openrouter".into(),
            requested: model.into(),
            model: if outcome == "ok" {
                model.into()
            } else {
                String::new()
            },
            free: cost == 0.0,
            outcome: outcome.into(),
            cost_usd: cost,
            prompt_tokens: 100,
            completion_tokens: 10,
            latency_ms: 200,
            kind: CallKind::Chat,
            ..CallRecord::default()
        }
    }

    #[test]
    fn a_codex_turns_use_is_its_meter_rows_in_its_time() {
        let store = Store::open_in_memory().unwrap();
        let at = |ts_ms, model: &str, engine: &str| CallRecord {
            ts_ms,
            engine: engine.into(),
            ..call("scout", None, model, "ok", 0.0)
        };
        for c in [
            at(1_000, "a:free", "codex"),
            at(2_000, "a:free", "codex"),
            at(2_500, "b:free", "codex"),
            at(2_600, "b:free", "openrouter"),
            at(9_000, "b:free", "codex"),
        ] {
            store.record_call(&c).unwrap();
        }
        let (requests, input, output, _, _, model) = store
            .engine_calls_totals("scout", "codex", 1_000, 3_000)
            .unwrap();
        assert_eq!((requests, input, output), (3, 300, 30));
        assert_eq!(model, "a:free", "the model that answered most");
        assert_eq!(
            store
                .engine_calls_totals("news", "codex", 0, 9_000)
                .unwrap()
                .0,
            0
        );
    }

    #[test]
    fn sums_for_limits_and_usage() {
        let store = Store::open_in_memory().unwrap();
        for c in [
            call("scout", Some("p1"), "a:free", "ok", 0.0),
            call("scout", Some("p1"), "a:free", "rate_limited", 0.0),
            call("scout", Some("p1"), "b:free", "ok", 0.0),
            call("news", Some("p2"), "paid", "ok", 0.25),
            call("news", None, "a:free", "cancelled", 0.0),
            CallRecord {
                provider: "local".into(),
                ..call("news", Some("p2"), "llama", "ok", 0.0)
            },
            // A Claude bot's own request, kept for comparisons only.
            CallRecord {
                provider: "anthropic".into(),
                ..call("scout", Some("p1"), "claude-sonnet-4-5", "ok", 0.0)
            },
        ] {
            store.record_call(&c).unwrap();
        }
        let day = "2026-11-02";
        assert_eq!(store.bot_requests(day, "scout").unwrap(), 3);
        assert_eq!(
            store.bot_requests(day, "news").unwrap(),
            1,
            "local and cancelled don't count"
        );
        assert_eq!(store.person_requests(day, "p1").unwrap(), 3);
        assert_eq!(store.free_requests(day).unwrap(), 3);
        assert!((store.spend(day).unwrap() - 0.25).abs() < 1e-9);
        assert!((store.spend_since("2026-11-01").unwrap() - 0.25).abs() < 1e-9);
        assert!((store.spend_since("2026-10-01").unwrap() - 0.25).abs() < 1e-9);
        assert!(store.spend_since("2026-11-03").unwrap().abs() < 1e-9);
        assert_eq!(store.bot_requests("2026-11-03", "scout").unwrap(), 0);
        let usage = store.day_usage(day).unwrap();
        let scout = &usage.bots.iter().find(|(b, _)| b == "scout").unwrap().1;
        assert_eq!(scout.requests, 3);
        assert_eq!(scout.prompt_tokens, 300);
        assert_eq!(scout.main_model, "a:free");
        assert!(
            usage
                .bots
                .iter()
                .all(|(_, t)| t.main_model != "claude-sonnet-4-5"),
            "Claude's requests aren't OpenRouter usage"
        );
        let stats = store.model_stats(0).unwrap();
        assert!(stats.iter().all(|s| !s.model.starts_with("claude")));
        let a = stats.iter().find(|s| s.model == "a:free").unwrap();
        assert_eq!((a.ok, a.total), (1, 3));
        assert!(
            stats.iter().all(|s| s.model != "llama"),
            "local isn't ranked"
        );
    }
}
