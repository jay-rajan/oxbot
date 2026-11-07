//! Comparing engines and models (#21): test cases, practice runs and what
//! they did, the oxbot-tool calls of every turn (a case's fixtures), and
//! people's thumbs on replies.
//!
//! JSON columns are kept as [`Value`]s here; the hub gives them their types.

use oxbot_protocol::{BotEvent, EventKind};
use rusqlite::{OptionalExtension as _, Row, params};
use serde_json::Value;

use super::events::{from_sql, to_sql};
use super::{Store, StoreError, now_ms};
use crate::id::BotId;

/// The most text kept of one oxbot-tool call's arguments or result.
pub const MAX_TOOL_TEXT: usize = 8 * 1024;

/// One oxbot-tool call in a turn, as kept for fixtures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnToolRecord {
    /// 1, 2… in the order they ran.
    pub n: u32,
    /// The tool, e.g. `give_task`.
    pub tool: String,
    /// Its arguments, as JSON text.
    pub args: String,
    /// What the bot was told.
    pub result: String,
    /// Whether it worked.
    pub ok: bool,
}

/// A test case: a finished turn frozen for running again.
#[derive(Debug, Clone, PartialEq)]
pub struct TestCaseRecord {
    /// Its id.
    pub id: String,
    /// The bot it came from.
    pub bot: String,
    /// When it was made.
    pub created_ms: i64,
    /// Who made it, by person id.
    pub created_by: Option<String>,
    /// The original turn's `UserMessage` event.
    pub seq_from: u64,
    /// The original turn's `TurnDone` event.
    pub seq_to: u64,
    /// Its title.
    pub title: String,
    /// The message.
    pub prompt: String,
    /// The files sent with it (JSON `[Attachment]`).
    pub files: Value,
    /// The bot's instructions then.
    pub instructions: String,
    /// What the bot knew then, once there is knowledge.
    pub knowledge: Option<Value>,
    /// `reads` or `acts`.
    pub kind: String,
    /// The owner's checks, one per line.
    pub rubric: String,
    /// What it should and shouldn't do (JSON).
    pub expected: Value,
    /// oxbot-tool results of the original turn (JSON).
    pub fixtures: Value,
    /// The sites it opened (JSON `[host]`).
    pub sites: Value,
    /// Whether it needs the source browser's sign-ins.
    pub uses_sign_ins: bool,
    /// Whose browser: '' home, else a person id.
    pub browser_owner: String,
    /// `strict` or `xhr`.
    pub network: String,
    /// The bot keeps its data on this Mac (local models only).
    pub local_only: bool,
    /// Who started the original turn (JSON `{person, on_farm, tainted_by}`).
    pub origin: Value,
    /// Hidden from the list.
    pub archived: bool,
}

/// A batch of practice runs.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRecord {
    /// Its id.
    pub id: String,
    /// The bot.
    pub bot: String,
    /// `replay` (`shadow` for runs of a feature since removed).
    pub kind: String,
    /// When it was started.
    pub created_ms: i64,
    /// JSON `[{engine, model}]`.
    pub targets: Value,
    /// JSON `[case id]`.
    pub cases: Value,
    /// Runs per case and target.
    pub repeats: u32,
    /// Model requests it was expected to take.
    pub est_requests: u32,
    /// `queued`, `running`, `paused`, `done` or `stopped`.
    pub status: String,
}

/// One run of a case on one engine and model (or the original turn).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrialRunRecord {
    /// Its id.
    pub id: String,
    /// Its batch.
    pub batch: Option<String>,
    /// Its case.
    pub case_id: Option<String>,
    /// The bot.
    pub bot: String,
    /// `original` or `replay` (`shadow` for runs of a feature since
    /// removed).
    pub kind: String,
    /// `codex`, `openrouter` (`claude` for runs from before Claude left
    /// oxbot).
    pub engine: String,
    /// `anthropic`, `openrouter`, …
    pub provider: String,
    /// The model.
    pub model: String,
    /// When it started.
    pub started_ms: Option<i64>,
    /// When it ended.
    pub ended_ms: Option<i64>,
    /// See the `trial_runs` table.
    pub status: String,
    /// Tool calls.
    pub steps: u32,
    /// Model requests.
    pub requests: u32,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Cached input tokens.
    pub cached_tokens: u64,
    /// US dollars.
    pub cost_usd: f64,
    /// Steps a live run would have asked about.
    pub intents: u32,
    /// Steps refused.
    pub refusals: u32,
    /// Errors.
    pub errors: u32,
    /// Requests the network guard stopped.
    pub net_blocked: u32,
    /// The final answer.
    pub answer: String,
    /// A report it would have saved (JSON).
    pub report: Option<Value>,
    /// What it recorded instead of doing (JSON `[Intent]`).
    pub intent_list: Value,
    /// How that matched the case (JSON).
    pub intent_match: Option<Value>,
    /// `pass`, `partial` or `fail`: the owner's, else the judge's.
    pub verdict: Option<String>,
    /// `owner`, `judge` or `local_judge`.
    pub verdict_by: Option<String>,
    /// The judge's answer (JSON).
    pub judge: Option<Value>,
    /// The owner's own verdict.
    pub owner_verdict: Option<String>,
    /// Days between the original turn and this run.
    pub web_age_days: Option<i64>,
}

/// Thumbs on one engine and model's replies, for the scoreboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatingCount {
    /// The engine.
    pub engine: String,
    /// The model.
    pub model: String,
    /// Thumbs up.
    pub up: u32,
    /// Thumbs down.
    pub down: u32,
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn opt_json(text: Option<String>) -> Option<Value> {
    text.map(|t| json(&t))
}

fn cut(text: &str) -> String {
    if text.len() <= MAX_TOOL_TEXT {
        return text.to_owned();
    }
    let mut end = MAX_TOOL_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn u32_of(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(0)
}

const CASE_COLUMNS: &str = "id, bot, created_ms, created_by, seq_from, seq_to, title, prompt, files, \
    instructions, knowledge, kind, rubric, expected, fixtures, sites, uses_sign_ins, browser_owner, \
    network, local_only, origin, archived";

fn case_row(r: &Row<'_>) -> rusqlite::Result<TestCaseRecord> {
    Ok(TestCaseRecord {
        id: r.get(0)?,
        bot: r.get(1)?,
        created_ms: r.get(2)?,
        created_by: r.get(3)?,
        seq_from: from_sql(r.get(4)?),
        seq_to: from_sql(r.get(5)?),
        title: r.get(6)?,
        prompt: r.get(7)?,
        files: json(&r.get::<_, String>(8)?),
        instructions: r.get(9)?,
        knowledge: opt_json(r.get(10)?),
        kind: r.get(11)?,
        rubric: r.get(12)?,
        expected: json(&r.get::<_, String>(13)?),
        fixtures: json(&r.get::<_, String>(14)?),
        sites: json(&r.get::<_, String>(15)?),
        uses_sign_ins: r.get(16)?,
        browser_owner: r.get(17)?,
        network: r.get(18)?,
        local_only: r.get(19)?,
        origin: json(&r.get::<_, String>(20)?),
        archived: r.get(21)?,
    })
}

const RUN_COLUMNS: &str = "id, batch, case_id, bot, kind, engine, provider, model, started_ms, ended_ms, \
    status, steps, requests, input_tokens, output_tokens, cached_tokens, cost_usd, intents, refusals, \
    errors, net_blocked, answer, report, intent_list, intent_match, verdict, verdict_by, judge, \
    owner_verdict, web_age_days";

fn run_row(r: &Row<'_>) -> rusqlite::Result<TrialRunRecord> {
    Ok(TrialRunRecord {
        id: r.get(0)?,
        batch: r.get(1)?,
        case_id: r.get(2)?,
        bot: r.get(3)?,
        kind: r.get(4)?,
        engine: r.get(5)?,
        provider: r.get(6)?,
        model: r.get(7)?,
        started_ms: r.get(8)?,
        ended_ms: r.get(9)?,
        status: r.get(10)?,
        steps: u32_of(r.get(11)?),
        requests: u32_of(r.get(12)?),
        input_tokens: from_sql(r.get(13)?),
        output_tokens: from_sql(r.get(14)?),
        cached_tokens: from_sql(r.get(15)?),
        cost_usd: r.get(16)?,
        intents: u32_of(r.get(17)?),
        refusals: u32_of(r.get(18)?),
        errors: u32_of(r.get(19)?),
        net_blocked: u32_of(r.get(20)?),
        answer: r.get(21)?,
        report: opt_json(r.get(22)?),
        intent_list: json(&r.get::<_, String>(23)?),
        intent_match: opt_json(r.get(24)?),
        verdict: r.get(25)?,
        verdict_by: r.get(26)?,
        judge: opt_json(r.get(27)?),
        owner_verdict: r.get(28)?,
        web_age_days: r.get(29)?,
    })
}

const BATCH_COLUMNS: &str =
    "id, bot, kind, created_ms, targets, cases, repeats, est_requests, status";

fn batch_row(r: &Row<'_>) -> rusqlite::Result<BatchRecord> {
    Ok(BatchRecord {
        id: r.get(0)?,
        bot: r.get(1)?,
        kind: r.get(2)?,
        created_ms: r.get(3)?,
        targets: json(&r.get::<_, String>(4)?),
        cases: json(&r.get::<_, String>(5)?),
        repeats: u32_of(r.get(6)?),
        est_requests: u32_of(r.get(7)?),
        status: r.get(8)?,
    })
}

impl Store {
    /// Keeps one of oxbot's own tool calls in the turn of `bot`'s message
    /// `seq` (arguments and result cut to [`MAX_TOOL_TEXT`]).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_turn_tool(
        &self,
        bot: &BotId,
        seq: u64,
        tool: &str,
        args: &Value,
        result: &str,
        ok: bool,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let n: i64 = tx.query_row(
            "SELECT COALESCE(MAX(n), 0) + 1 FROM turn_tools WHERE bot = ?1 AND seq = ?2",
            params![bot.as_str(), to_sql(seq)],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO turn_tools (bot, seq, n, tool, args, result, ok) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                bot.as_str(),
                to_sql(seq),
                n,
                tool,
                cut(&args.to_string()),
                cut(result),
                ok
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// oxbot's own tool calls in the turn of `bot`'s message `seq`, in
    /// order.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn turn_tools(&self, bot: &BotId, seq: u64) -> Result<Vec<TurnToolRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT n, tool, args, result, ok FROM turn_tools WHERE bot = ?1 AND seq = ?2 ORDER BY n",
        )?;
        let rows = stmt.query_map(params![bot.as_str(), to_sql(seq)], |r| {
            Ok(TurnToolRecord {
                n: u32_of(r.get(0)?),
                tool: r.get(1)?,
                args: r.get(2)?,
                result: r.get(3)?,
                ok: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The number of `bot`'s latest `UserMessage` (0 if none): the message
    /// its running turn answers.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn last_user_seq(&self, bot: &BotId) -> Result<u64, StoreError> {
        let seq: Option<i64> = self.conn().query_row(
            "SELECT MAX(seq) FROM events WHERE bot = ?1 AND json_extract(body, '$.kind') = 'user_message'",
            [bot.as_str()],
            |r| r.get(0),
        )?;
        Ok(seq.map_or(0, from_sql))
    }

    /// Saves a new test case.
    ///
    /// # Errors
    /// Fails if SQLite fails (or the id is taken).
    pub fn add_test_case(&self, case: &TestCaseRecord) -> Result<(), StoreError> {
        self.conn().execute(
            &format!(
                "INSERT INTO test_cases ({CASE_COLUMNS}) VALUES \
                 (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)"
            ),
            params![
                case.id,
                case.bot,
                case.created_ms,
                case.created_by,
                to_sql(case.seq_from),
                to_sql(case.seq_to),
                case.title,
                case.prompt,
                case.files.to_string(),
                case.instructions,
                case.knowledge.as_ref().map(Value::to_string),
                case.kind,
                case.rubric,
                case.expected.to_string(),
                case.fixtures.to_string(),
                case.sites.to_string(),
                case.uses_sign_ins,
                case.browser_owner,
                case.network,
                case.local_only,
                case.origin.to_string(),
                case.archived,
            ],
        )?;
        Ok(())
    }

    /// Test case `id`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn test_case(&self, id: &str) -> Result<Option<TestCaseRecord>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {CASE_COLUMNS} FROM test_cases WHERE id = ?1"),
                [id],
                case_row,
            )
            .optional()?)
    }

    /// `bot`'s test cases, newest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn test_cases(&self, bot: &BotId) -> Result<Vec<TestCaseRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CASE_COLUMNS} FROM test_cases WHERE bot = ?1 ORDER BY created_ms DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map([bot.as_str()], case_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Changes what the owner may change on test case `id`. Returns whether
    /// it was there.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn update_test_case(
        &self,
        id: &str,
        title: Option<&str>,
        rubric: Option<&str>,
        network: Option<&str>,
        archived: Option<bool>,
    ) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "UPDATE test_cases SET title = COALESCE(?2, title), rubric = COALESCE(?3, rubric),
                network = COALESCE(?4, network), archived = COALESCE(?5, archived)
             WHERE id = ?1",
            params![id, title, rubric, network, archived],
        )? > 0)
    }

    /// Deletes test case `id`, its runs and what they did. Returns whether
    /// it was there.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn delete_test_case(&self, id: &str) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM trial_events WHERE run IN (SELECT id FROM trial_runs WHERE case_id = ?1)",
            [id],
        )?;
        tx.execute("DELETE FROM trial_runs WHERE case_id = ?1", [id])?;
        let gone = tx.execute("DELETE FROM test_cases WHERE id = ?1", [id])? > 0;
        tx.commit()?;
        Ok(gone)
    }

    /// Saves a new batch.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_batch(&self, batch: &BatchRecord) -> Result<(), StoreError> {
        self.conn().execute(
            &format!("INSERT INTO trial_batches ({BATCH_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"),
            params![
                batch.id,
                batch.bot,
                batch.kind,
                batch.created_ms,
                batch.targets.to_string(),
                batch.cases.to_string(),
                batch.repeats,
                batch.est_requests,
                batch.status,
            ],
        )?;
        Ok(())
    }

    /// Batch `id`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn batch(&self, id: &str) -> Result<Option<BatchRecord>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {BATCH_COLUMNS} FROM trial_batches WHERE id = ?1"),
                [id],
                batch_row,
            )
            .optional()?)
    }

    /// Sets batch `id`'s status.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_batch_status(&self, id: &str, status: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE trial_batches SET status = ?2 WHERE id = ?1",
            params![id, status],
        )?;
        Ok(())
    }

    /// Saves a run, new or changed.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn save_run(&self, run: &TrialRunRecord) -> Result<(), StoreError> {
        let opt = |v: &Option<Value>| v.as_ref().map(Value::to_string);
        self.conn().execute(
            &format!(
                "INSERT OR REPLACE INTO trial_runs ({RUN_COLUMNS}) VALUES \
                 (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, \
                  ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30)"
            ),
            params![
                run.id,
                run.batch,
                run.case_id,
                run.bot,
                run.kind,
                run.engine,
                run.provider,
                run.model,
                run.started_ms,
                run.ended_ms,
                run.status,
                run.steps,
                run.requests,
                to_sql(run.input_tokens),
                to_sql(run.output_tokens),
                to_sql(run.cached_tokens),
                run.cost_usd,
                run.intents,
                run.refusals,
                run.errors,
                run.net_blocked,
                run.answer,
                opt(&run.report),
                run.intent_list.to_string(),
                opt(&run.intent_match),
                run.verdict,
                run.verdict_by,
                opt(&run.judge),
                run.owner_verdict,
                run.web_age_days,
            ],
        )?;
        Ok(())
    }

    /// Run `id`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn run(&self, id: &str) -> Result<Option<TrialRunRecord>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM trial_runs WHERE id = ?1"),
                [id],
                run_row,
            )
            .optional()?)
    }

    /// `bot`'s runs (of `case` only, if given), newest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn runs(&self, bot: &BotId, case: Option<&str>) -> Result<Vec<TrialRunRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM trial_runs WHERE bot = ?1 AND (?2 IS NULL OR case_id = ?2)
             ORDER BY COALESCE(started_ms, 0) DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map(params![bot.as_str(), case], run_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The runs of batch `id`, in the order they were made.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn batch_runs(&self, id: &str) -> Result<Vec<TrialRunRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM trial_runs WHERE batch = ?1 ORDER BY rowid"
        ))?;
        let rows = stmt.query_map([id], run_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Ends every run and batch an earlier oxbot left queued, running or
    /// paused (practice runs never go on after a restart). Returns how
    /// many runs.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn stop_unfinished_runs(&self) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let runs = tx.execute(
            "UPDATE trial_runs SET status = 'stopped', ended_ms = COALESCE(ended_ms, ?1)
             WHERE status IN ('queued', 'running')",
            [now_ms()],
        )?;
        tx.execute(
            "UPDATE trial_batches SET status = 'stopped' WHERE status IN ('queued', 'running', 'paused')",
            [],
        )?;
        tx.commit()?;
        Ok(runs)
    }

    /// Adds an event to run `run`'s own history (never a bot's chat).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_trial_event(&self, run: &str, kind: EventKind) -> Result<BotEvent, StoreError> {
        let body = serde_json::to_string(&kind)?;
        let ts_ms = now_ms();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM trial_events WHERE run = ?1",
            [run],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO trial_events (run, seq, ts_ms, body) VALUES (?1, ?2, ?3, ?4)",
            params![run, seq, ts_ms, body],
        )?;
        tx.commit()?;
        Ok(BotEvent {
            bot: run.to_owned(),
            seq: from_sql(seq),
            ts_ms,
            kind,
        })
    }

    /// Run `run`'s events, oldest first (unreadable ones skipped).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn trial_events(&self, run: &str) -> Result<Vec<BotEvent>, StoreError> {
        let rows: Vec<(i64, i64, String)> = {
            let conn = self.conn();
            let mut stmt = conn
                .prepare("SELECT seq, ts_ms, body FROM trial_events WHERE run = ?1 ORDER BY seq")?;
            let rows = stmt.query_map([run], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        Ok(rows
            .into_iter()
            .filter_map(|(seq, ts_ms, body)| {
                Some(BotEvent {
                    bot: run.to_owned(),
                    seq: from_sql(seq),
                    ts_ms,
                    kind: serde_json::from_str(&body).ok()?,
                })
            })
            .collect())
    }

    /// Sets (or with `None` clears) someone's thumb on `bot`'s reply that
    /// ended at event `seq`, made by `engine` and `model`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_rating(
        &self,
        bot: &BotId,
        seq: u64,
        up: Option<bool>,
        engine: &str,
        model: &str,
    ) -> Result<(), StoreError> {
        let conn = self.conn();
        match up {
            Some(up) => conn.execute(
                "INSERT INTO turn_ratings (bot, seq, up, engine, model, ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(bot, seq) DO UPDATE SET up = excluded.up, ms = excluded.ms",
                params![bot.as_str(), to_sql(seq), up, engine, model, now_ms()],
            )?,
            None => conn.execute(
                "DELETE FROM turn_ratings WHERE bot = ?1 AND seq = ?2",
                params![bot.as_str(), to_sql(seq)],
            )?,
        };
        Ok(())
    }

    /// The thumb on `bot`'s reply that ended at `seq`, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn rating(&self, bot: &BotId, seq: u64) -> Result<Option<bool>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT up FROM turn_ratings WHERE bot = ?1 AND seq = ?2",
                params![bot.as_str(), to_sql(seq)],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// `bot`'s thumbs: the reply's `TurnDone` event → up (true) or down.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn rating_list(&self, bot: &BotId) -> Result<Vec<(u64, bool)>, StoreError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT seq, up FROM turn_ratings WHERE bot = ?1 ORDER BY seq")?;
        let rows = stmt.query_map([bot.as_str()], |r| Ok((from_sql(r.get(0)?), r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// `bot`'s thumbs, per engine and model.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn rating_counts(&self, bot: &BotId) -> Result<Vec<RatingCount>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT engine, model, SUM(up), SUM(1 - up) FROM turn_ratings WHERE bot = ?1
             GROUP BY engine, model ORDER BY engine, model",
        )?;
        let rows = stmt.query_map([bot.as_str()], |r| {
            Ok(RatingCount {
                engine: r.get(0)?,
                model: r.get(1)?,
                up: u32_of(r.get(2)?),
                down: u32_of(r.get(3)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Deletes every practice run and batch (test cases stay). Returns how
    /// many runs went.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn delete_all_runs(&self) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM trial_events", [])?;
        let runs = tx.execute("DELETE FROM trial_runs WHERE kind <> 'original'", [])?;
        tx.execute(
            "DELETE FROM trial_batches WHERE status NOT IN ('queued', 'running', 'paused')",
            [],
        )?;
        tx.commit()?;
        Ok(runs)
    }

    /// What `bot`'s requests on `engine` between `from_ms` and `to_ms`
    /// used: requests, input, output and cached tokens, dollars, and the
    /// model that answered most (a turn's use, from the relay's meter).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn engine_calls_totals(
        &self,
        bot: &str,
        engine: &str,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<(u32, u64, u64, u64, f64, String), StoreError> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*), COALESCE(SUM(prompt_tokens), 0), COALESCE(SUM(completion_tokens), 0),
                    COALESCE(SUM(cached_tokens), 0), COALESCE(SUM(cost_usd), 0),
                    COALESCE((SELECT model FROM model_calls m2 WHERE m2.bot = ?1 AND m2.engine = ?2
                        AND m2.ts_ms BETWEEN ?3 AND ?4 AND m2.model != ''
                        GROUP BY model ORDER BY COUNT(*) DESC, model LIMIT 1), '')
             FROM model_calls WHERE bot = ?1 AND engine = ?2 AND ts_ms BETWEEN ?3 AND ?4
                AND outcome != 'cancelled'",
            params![bot, engine, from_ms, to_ms],
            |r| {
                Ok((
                    u32_of(r.get(0)?),
                    from_sql(r.get(1)?),
                    from_sql(r.get(2)?),
                    from_sql(r.get(3)?),
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?)
    }

    /// Forgets everything stored under a practice run's own key (its
    /// session, the pages it opened, its taint): nothing of a practice run
    /// stays under a bot id once it ends.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn forget_trial_key(&self, key: &BotId) -> Result<(), StoreError> {
        self.delete_bot(key).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn case(id: &str, bot: &str) -> TestCaseRecord {
        TestCaseRecord {
            id: id.into(),
            bot: bot.into(),
            created_ms: 1,
            created_by: None,
            seq_from: 3,
            seq_to: 9,
            title: "Post the job".into(),
            prompt: "post it".into(),
            files: json!([]),
            instructions: "Post jobs".into(),
            knowledge: None,
            kind: "acts".into(),
            rubric: "Would click Post".into(),
            expected: json!([]),
            fixtures: json!({}),
            sites: json!(["jobs.example.test"]),
            uses_sign_ins: false,
            browser_owner: String::new(),
            network: "strict".into(),
            local_only: false,
            origin: json!({"on_farm": true}),
            archived: false,
        }
    }

    #[test]
    fn turn_tools_are_kept_in_order_and_cut_short() {
        let store = Store::open_in_memory().unwrap();
        let bot = BotId::new("jobs").unwrap();
        store
            .add_turn_tool(&bot, 4, "give_task", &json!({"bot": "news"}), "done", true)
            .unwrap();
        store
            .add_turn_tool(&bot, 4, "read_reply", &json!({}), &"x".repeat(9000), false)
            .unwrap();
        let tools = store.turn_tools(&bot, 4).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!((tools[0].n, tools[0].tool.as_str()), (1, "give_task"));
        assert!(tools[1].result.len() <= MAX_TOOL_TEXT + 3 && !tools[1].ok);
        assert_eq!(store.turn_tools(&bot, 5).unwrap().len(), 0);
    }

    #[test]
    fn cases_runs_and_events_round_trip_and_go_with_the_bot() {
        let store = Store::open_in_memory().unwrap();
        let bot = BotId::new("jobs").unwrap();
        store.add_test_case(&case("c1", "jobs")).unwrap();
        assert_eq!(store.test_case("c1").unwrap().unwrap(), case("c1", "jobs"));
        assert!(
            store
                .update_test_case("c1", Some("New"), None, Some("xhr"), Some(true))
                .unwrap()
        );
        let c = store.test_case("c1").unwrap().unwrap();
        assert_eq!(
            (c.title.as_str(), c.network.as_str(), c.archived),
            ("New", "xhr", true)
        );
        assert_eq!(c.rubric, "Would click Post", "unchanged");
        assert!(
            !store
                .update_test_case("nope", Some("x"), None, None, None)
                .unwrap()
        );

        let run = TrialRunRecord {
            id: "r1".into(),
            case_id: Some("c1".into()),
            bot: "jobs".into(),
            kind: "replay".into(),
            engine: "claude".into(),
            provider: "anthropic".into(),
            model: "sonnet".into(),
            status: "done".into(),
            steps: 4,
            intent_list: json!([{"kind": "change"}]),
            judge: Some(json!({"verdict": "pass"})),
            ..TrialRunRecord::default()
        };
        store.save_run(&run).unwrap();
        assert_eq!(store.run("r1").unwrap().unwrap(), run);
        let event = store
            .add_trial_event("r1", EventKind::AssistantMessage { text: "hi".into() })
            .unwrap();
        assert_eq!((event.bot.as_str(), event.seq), ("r1", 1));
        assert_eq!(store.trial_events("r1").unwrap().len(), 1);
        assert!(store.history(&bot, 0).unwrap().is_empty(), "never the chat");
        assert_eq!(store.runs(&bot, Some("c1")).unwrap().len(), 1);
        assert_eq!(store.runs(&bot, Some("c2")).unwrap().len(), 0);

        store
            .set_rating(&bot, 9, Some(true), "claude", "sonnet")
            .unwrap();
        store
            .set_rating(&bot, 12, Some(false), "claude", "sonnet")
            .unwrap();
        assert_eq!(store.rating(&bot, 9).unwrap(), Some(true));
        assert_eq!(
            store.rating_counts(&bot).unwrap(),
            vec![RatingCount {
                engine: "claude".into(),
                model: "sonnet".into(),
                up: 1,
                down: 1
            }]
        );
        store
            .set_rating(&bot, 12, None, "claude", "sonnet")
            .unwrap();
        assert_eq!(store.rating(&bot, 12).unwrap(), None);
        store
            .add_turn_tool(&bot, 3, "give_task", &json!({}), "ok", true)
            .unwrap();

        store.delete_bot(&bot).unwrap();
        assert!(store.test_case("c1").unwrap().is_none());
        assert!(store.run("r1").unwrap().is_none());
        assert_eq!(store.trial_events("r1").unwrap().len(), 0);
        assert_eq!(store.turn_tools(&bot, 3).unwrap().len(), 0);
        assert_eq!(store.rating_counts(&bot).unwrap().len(), 0);
    }

    #[test]
    fn a_restart_stops_what_was_running() {
        let store = Store::open_in_memory().unwrap();
        let mut run = TrialRunRecord {
            id: "r1".into(),
            bot: "jobs".into(),
            kind: "replay".into(),
            status: "running".into(),
            ..TrialRunRecord::default()
        };
        store.save_run(&run).unwrap();
        run.id = "r2".into();
        run.status = "done".into();
        store.save_run(&run).unwrap();
        store
            .add_batch(&BatchRecord {
                id: "b1".into(),
                bot: "jobs".into(),
                kind: "replay".into(),
                created_ms: 1,
                targets: json!([]),
                cases: json!([]),
                repeats: 1,
                est_requests: 0,
                status: "running".into(),
            })
            .unwrap();
        assert_eq!(store.stop_unfinished_runs().unwrap(), 1);
        assert_eq!(store.run("r1").unwrap().unwrap().status, "stopped");
        assert_eq!(store.run("r2").unwrap().unwrap().status, "done");
        assert_eq!(store.batch("b1").unwrap().unwrap().status, "stopped");
    }

    #[test]
    fn the_upgrade_adds_the_tables_and_keeps_what_was_there() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("oxbot.db");
        // The version before migration 18 (comparisons).
        let before = 17;
        Store::open_at_version(&path, before).unwrap();
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "INSERT INTO events (bot, seq, ts_ms, body) VALUES ('jobs', 1, 1, '{\"kind\":\"error\",\"message\":\"x\"}');
                 DROP TABLE IF EXISTS test_cases;",
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            usize::try_from(version).unwrap(),
            super::super::migrations::MIGRATIONS.len()
        );
        let bot = BotId::new("jobs").unwrap();
        assert_eq!(store.history(&bot, 0).unwrap().len(), 1);
        store.add_test_case(&case("c1", "jobs")).unwrap();
        assert_eq!(store.test_cases(&bot).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_case_takes_its_runs() {
        let store = Store::open_in_memory().unwrap();
        store.add_test_case(&case("c1", "jobs")).unwrap();
        store
            .save_run(&TrialRunRecord {
                id: "r1".into(),
                case_id: Some("c1".into()),
                bot: "jobs".into(),
                kind: "original".into(),
                status: "done".into(),
                ..TrialRunRecord::default()
            })
            .unwrap();
        store
            .add_trial_event(
                "r1",
                EventKind::Error {
                    message: "x".into(),
                },
            )
            .unwrap();
        assert!(store.delete_test_case("c1").unwrap());
        assert!(store.run("r1").unwrap().is_none());
        assert_eq!(store.trial_events("r1").unwrap().len(), 0);
        assert!(!store.delete_test_case("c1").unwrap());
    }
}
