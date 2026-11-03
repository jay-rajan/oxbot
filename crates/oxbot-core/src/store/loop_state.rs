//! oxbot's own loop in the database: conversations (`loop_sessions`,
//! `loop_messages`), turns with their checkpoints (`loop_turns`), and every
//! model request (`model_calls`), with the sums the limits and the usage
//! view read.

use oxbot_engine::CallRecord;
use oxbot_llm::Message;
use oxbot_loop::store::{
    CalibrationRow, LoopStore, LoopStoreError, ModelStat, PendingCall, ProgressNote, ResultKind,
    ResultRow, SessionRow, StoredMessage, TurnRow, TurnState, TurnStatus,
};
use rusqlite::{OptionalExtension as _, Row, params};
use serde_json::Value;

use super::{Store, StoreError, now_ms};

/// How long model requests are kept, in days.
pub const KEEP_CALLS_DAYS: i64 = 400;
/// How long the messages of a closed conversation are kept, in days (for
/// replays).
pub const KEEP_CLOSED_DAYS: i64 = 30;

/// How long stored tool results and progress notes are kept after their
/// session's last turn, in days (their files follow the same rule).
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

fn json_or_empty(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

fn turn_row(r: &Row<'_>) -> rusqlite::Result<TurnRow> {
    let status: String = r.get("status")?;
    let pending: Option<String> = r.get("pending")?;
    let origin: String = r.get("origin")?;
    Ok(TurnRow {
        session: r.get("session")?,
        turn: to_u32(r.get("turn")?),
        bot: r.get("bot")?,
        user_seq: to_u64(r.get("user_seq")?),
        origin: serde_json::from_str(&origin).unwrap_or(Value::Null),
        question: r.get("question")?,
        status: TurnStatus::parse(&status).unwrap_or(TurnStatus::Failed),
        step: to_u32(r.get("step")?),
        pending: pending.and_then(|p| serde_json::from_str::<PendingCall>(&p).ok()),
        model: r.get("model")?,
        resumes: to_u32(r.get("resumes")?),
        dry_run: r.get("dry_run")?,
    })
}

impl LoopStore for Store {
    fn open_session(
        &self,
        bot: &str,
        id: &str,
        prompt_hash: &str,
        resume: bool,
    ) -> Result<SessionRow, LoopStoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(err)?;
        let now = now_ms();
        tx.execute(
            "UPDATE loop_sessions SET closed_ms = ?3
             WHERE bot = ?1 AND id != ?2 AND closed_ms IS NULL",
            params![bot, id, now],
        )
        .map_err(err)?;
        let saved: Option<(String, String)> = tx
            .query_row(
                "SELECT prompt_hash, context FROM loop_sessions WHERE id = ?1 AND bot = ?2",
                params![id, bot],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        let row = match saved {
            Some((hash, context)) if resume => {
                tx.execute(
                    "UPDATE loop_sessions SET prompt_hash = ?2, closed_ms = NULL WHERE id = ?1",
                    params![id, prompt_hash],
                )
                .map_err(err)?;
                SessionRow {
                    id: id.to_owned(),
                    context: json_or_empty(&context),
                    existed: true,
                    prefix_changed: hash != prompt_hash,
                }
            }
            _ => {
                // A new conversation (an id reused by mistake starts over).
                tx.execute("DELETE FROM loop_messages WHERE session = ?1", [id])
                    .map_err(err)?;
                tx.execute(
                    "INSERT OR REPLACE INTO loop_sessions (id, bot, prompt_hash, context, created_ms)
                     VALUES (?1, ?2, ?3, '{}', ?4)",
                    params![id, bot, prompt_hash, now],
                )
                .map_err(err)?;
                SessionRow {
                    id: id.to_owned(),
                    context: json_or_empty("{}"),
                    existed: false,
                    prefix_changed: false,
                }
            }
        };
        tx.commit().map_err(err)?;
        Ok(row)
    }

    fn messages(&self, session: &str) -> Result<Vec<StoredMessage>, LoopStoreError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT idx, turn, body, tokens, untrusted, handle, meta FROM loop_messages
                 WHERE session = ?1 ORDER BY idx",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([session], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(err)?;
        let mut out = Vec::new();
        for row in rows {
            let (idx, turn, body, tokens, untrusted, handle, meta) = row.map_err(err)?;
            out.push(StoredMessage {
                idx: to_u32(idx),
                turn: to_u32(turn),
                message: Message::from_stored(&body).map_err(err)?,
                tokens: to_u32(tokens),
                untrusted,
                handle,
                meta: json_or_empty(&meta),
            });
        }
        Ok(out)
    }

    fn begin_turn(&self, t: &TurnRow) -> Result<(), LoopStoreError> {
        let now = now_ms();
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO loop_turns
                   (session, turn, bot, user_seq, origin, question, status, step, pending, model,
                    resumes, dry_run, started_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
                params![
                    t.session,
                    t.turn,
                    t.bot,
                    to_i64(t.user_seq),
                    t.origin.to_string(),
                    t.question,
                    t.status.as_str(),
                    t.step,
                    t.pending
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()
                        .map_err(err)?,
                    t.model,
                    t.resumes,
                    t.dry_run,
                    now,
                ],
            )
            .map_err(err)?;
        Ok(())
    }

    fn checkpoint(
        &self,
        session: &str,
        turn: u32,
        add: &[StoredMessage],
        state: &TurnState,
        context: Option<&Value>,
    ) -> Result<(), LoopStoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(err)?;
        for m in add {
            tx.execute(
                "INSERT INTO loop_messages
                   (session, idx, turn, role, body, tokens, untrusted, handle, meta)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    session,
                    m.idx,
                    m.turn,
                    m.message.role.as_str(),
                    m.message.to_stored().map_err(err)?,
                    m.tokens,
                    m.untrusted,
                    m.handle,
                    m.meta.to_string(),
                ],
            )
            .map_err(err)?;
        }
        tx.execute(
            "UPDATE loop_turns SET step = ?3, model = ?4, pending = ?5, updated_ms = ?6
             WHERE session = ?1 AND turn = ?2",
            params![
                session,
                turn,
                state.step,
                state.model,
                state
                    .pending
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(err)?,
                now_ms(),
            ],
        )
        .map_err(err)?;
        if let Some(context) = context {
            tx.execute(
                "UPDATE loop_sessions SET context = ?2 WHERE id = ?1",
                params![session, context.to_string()],
            )
            .map_err(err)?;
        }
        tx.commit().map_err(err)
    }

    fn end_turn(&self, session: &str, turn: u32, status: TurnStatus) -> Result<(), LoopStoreError> {
        let now = now_ms();
        self.conn()
            .execute(
                "UPDATE loop_turns SET status = ?3, pending = NULL, updated_ms = ?4, ended_ms = ?4
                 WHERE session = ?1 AND turn = ?2",
                params![session, turn, status.as_str(), now],
            )
            .map_err(err)?;
        Ok(())
    }

    fn turn_resumed(&self, session: &str, turn: u32) -> Result<(), LoopStoreError> {
        self.conn()
            .execute(
                "UPDATE loop_turns SET resumes = resumes + 1 WHERE session = ?1 AND turn = ?2",
                params![session, turn],
            )
            .map_err(err)?;
        Ok(())
    }

    fn running_turns(&self) -> Result<Vec<TurnRow>, LoopStoreError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT * FROM loop_turns WHERE status = 'running' ORDER BY started_ms, turn")
            .map_err(err)?;
        let rows = stmt.query_map([], turn_row).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    fn record_call(&self, c: &CallRecord) -> Result<(), LoopStoreError> {
        self.conn()
            .execute(
                "INSERT INTO model_calls
                   (ts_ms, day, bot, person, session, turn, step, provider, requested, model, free,
                    private, generation_id, prompt_tokens, completion_tokens, cached_tokens,
                    cost_usd, latency_ms, outcome, kind)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20)",
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

    fn last_result_number(&self, session: &str) -> Result<u32, LoopStoreError> {
        let n: Option<i64> = self
            .conn()
            .query_row(
                "SELECT MAX(CAST(SUBSTR(handle, 2) AS INTEGER)) FROM loop_results
                 WHERE session = ?1",
                [session],
                |r| r.get(0),
            )
            .map_err(err)?;
        Ok(to_u32(n.unwrap_or(0)))
    }

    fn add_progress(&self, session: &str, note: &ProgressNote) -> Result<(), LoopStoreError> {
        self.conn()
            .execute(
                "INSERT INTO loop_progress (session, turn, n, ms, note, tainted_by)
                 VALUES (?1, ?2,
                         (SELECT COALESCE(MAX(n), 0) + 1 FROM loop_progress
                          WHERE session = ?1 AND turn = ?2),
                         ?3, ?4, ?5)",
                params![session, note.turn, note.ms, note.note, note.tainted_by],
            )
            .map_err(err)?;
        Ok(())
    }

    fn progress(&self, session: &str, n: usize) -> Result<Vec<ProgressNote>, LoopStoreError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT turn, ms, note, tainted_by FROM (
                    SELECT * FROM loop_progress WHERE session = ?1
                    ORDER BY turn DESC, n DESC LIMIT ?2)
                 ORDER BY turn, n",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map(params![session, to_i64(n)], |r| {
                Ok(ProgressNote {
                    turn: to_u32(r.get(0)?),
                    ms: r.get(1)?,
                    note: r.get(2)?,
                    tainted_by: r.get(3)?,
                })
            })
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    fn calibrations(&self) -> Result<Vec<CalibrationRow>, LoopStoreError> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT model, ratio, samples FROM token_calibration ORDER BY model")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CalibrationRow {
                    model: r.get(0)?,
                    ratio: r.get(1)?,
                    samples: to_u32(r.get(2)?),
                })
            })
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    fn save_calibration(&self, row: &CalibrationRow) -> Result<(), LoopStoreError> {
        self.conn()
            .execute(
                "INSERT OR REPLACE INTO token_calibration (model, ratio, samples, updated_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![row.model, row.ratio, row.samples, now_ms()],
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
                     GROUP BY model ORDER BY COUNT(*) DESC, model LIMIT 1)
             FROM model_calls m
             WHERE day = ?1 AND {column} IS NOT NULL AND outcome != 'cancelled'
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

    /// When a turn of loop session `session` last moved, if it has any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn loop_session_last_ms(&self, session: &str) -> Result<Option<i64>, StoreError> {
        Ok(self.conn().query_row(
            "SELECT MAX(updated_ms) FROM loop_turns WHERE session = ?1",
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

    /// The loop turn started by `bot`'s message `user_seq`, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn loop_turn_for(&self, bot: &str, user_seq: u64) -> Result<Option<TurnRow>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT * FROM loop_turns WHERE bot = ?1 AND user_seq = ?2
                 ORDER BY started_ms DESC LIMIT 1",
                params![bot, to_i64(user_seq)],
                turn_row,
            )
            .optional()?)
    }

    /// Deletes model requests older than [`KEEP_CALLS_DAYS`] and the
    /// messages of conversations closed more than [`KEEP_CLOSED_DAYS`]
    /// ago. Returns how many rows went.
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
        // Stored results and notes go with their files: once no turn of
        // their session has moved for the retention time. A bot's open
        // session keeps them (the sweep keeps its files too).
        let mut context = 0;
        for table in ["loop_results", "loop_progress"] {
            context += tx.execute(
                &format!(
                    "DELETE FROM {table}
                     WHERE session NOT IN (
                           SELECT session FROM loop_turns GROUP BY session HAVING MAX(updated_ms) >= ?1)
                       AND session NOT IN (SELECT id FROM loop_sessions WHERE closed_ms IS NULL)"
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
    use serde_json::json;

    fn turn(session: &str, n: u32) -> TurnRow {
        TurnRow {
            session: session.into(),
            turn: n,
            bot: "scout".into(),
            user_seq: 3,
            origin: json!({"person": "p1", "on_farm": true}),
            question: "search jobs".into(),
            status: TurnStatus::Running,
            step: 0,
            pending: None,
            model: None,
            resumes: 0,
            dry_run: false,
        }
    }

    fn stored(idx: u32, message: Message) -> StoredMessage {
        StoredMessage {
            idx,
            turn: 0,
            message,
            tokens: 5,
            untrusted: idx == 1,
            handle: None,
            meta: json!({}),
        }
    }

    #[test]
    fn sessions_turns_and_checkpoints() {
        let store = Store::open_in_memory().unwrap();
        let s = store.open_session("scout", "s1", "h1", false).unwrap();
        assert!(!s.existed);
        store.begin_turn(&turn("s1", 0)).unwrap();
        let pending = PendingCall {
            id: "c1".into(),
            name: "mcp__chrome-devtools__click".into(),
            args: "{}".into(),
            side_effect: true,
        };
        store
            .checkpoint(
                "s1",
                0,
                &[
                    stored(0, Message::user("hi")),
                    stored(1, Message::tool("c0", vec![])),
                ],
                &TurnState {
                    step: 1,
                    model: Some("a:free".into()),
                    pending: Some(pending.clone()),
                },
                Some(&json!({"from_idx": 1})),
            )
            .unwrap();
        let running = store.running_turns().unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].pending, Some(pending));
        assert_eq!(running[0].step, 1);
        assert_eq!(running[0].origin["person"], "p1");
        let msgs = store.messages("s1").unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].message, Message::user("hi"));
        assert!(msgs[1].untrusted);
        // Resumed with another prefix: noted, context kept.
        let s = store.open_session("scout", "s1", "h2", true).unwrap();
        assert!(s.existed && s.prefix_changed);
        assert_eq!(s.context["from_idx"], 1);
        store.turn_resumed("s1", 0).unwrap();
        store.end_turn("s1", 0, TurnStatus::Done).unwrap();
        assert_eq!(store.running_turns().unwrap(), Vec::new());
        let t = store.loop_turn_for("scout", 3).unwrap().unwrap();
        assert_eq!(
            (t.status, t.resumes, t.pending),
            (TurnStatus::Done, 1, None)
        );
        // A new session closes the old one; closed ones are pruned later.
        store.open_session("scout", "s2", "h2", false).unwrap();
        assert_eq!(store.prune_loop(now_ms()).unwrap(), 0);
        assert_eq!(
            store
                .prune_loop(now_ms() + (KEEP_CLOSED_DAYS + 1) * DAY_MS)
                .unwrap(),
            2
        );
    }

    #[test]
    fn stored_results_go_with_their_session_but_not_the_open_one() {
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
        store.open_session("scout", "old", "h", false).unwrap();
        store.begin_turn(&turn("old", 0)).unwrap();
        store.save_result(&row("old")).unwrap();
        // A newer session closes the old one; it stays open itself.
        store.open_session("scout", "open", "h", false).unwrap();
        store.begin_turn(&turn("open", 0)).unwrap();
        store.save_result(&row("open")).unwrap();
        let later = now_ms() + (KEEP_RESULTS_DAYS + 1) * DAY_MS;
        assert_eq!(store.prune_loop(now_ms()).unwrap(), 0);
        store.prune_loop(later).unwrap();
        assert_eq!(store.result("old", "r1").unwrap(), None);
        assert!(store.result("open", "r1").unwrap().is_some());
    }

    #[test]
    fn a_failed_checkpoint_saves_nothing() {
        let store = Store::open_in_memory().unwrap();
        store.open_session("scout", "s1", "h", false).unwrap();
        store.begin_turn(&turn("s1", 0)).unwrap();
        let state = TurnState {
            step: 1,
            model: None,
            pending: None,
        };
        let m = stored(0, Message::user("hi"));
        store
            .checkpoint("s1", 0, std::slice::from_ref(&m), &state, None)
            .unwrap();
        // The same message number again fails, and the step isn't saved.
        let later = TurnState { step: 2, ..state };
        assert!(store.checkpoint("s1", 0, &[m], &later, None).is_err());
        assert_eq!(store.running_turns().unwrap()[0].step, 1);
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
        assert_eq!(store.bot_requests("2026-11-03", "scout").unwrap(), 0);
        let usage = store.day_usage(day).unwrap();
        let scout = &usage.bots.iter().find(|(b, _)| b == "scout").unwrap().1;
        assert_eq!(scout.requests, 3);
        assert_eq!(scout.prompt_tokens, 300);
        assert_eq!(scout.main_model, "a:free");
        let stats = store.model_stats(0).unwrap();
        let a = stats.iter().find(|s| s.model == "a:free").unwrap();
        assert_eq!((a.ok, a.total), (1, 3));
        assert!(
            stats.iter().all(|s| s.model != "llama"),
            "local isn't ranked"
        );
    }
}
