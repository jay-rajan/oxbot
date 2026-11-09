//! Settings → Activity's log (`net_log`): one row per exchange with the
//! outside world. Sizes, places and outcomes only, never what was said.
//! Rows go after [`KEEP_NET_DAYS`].

use std::collections::BTreeMap;

use oxbot_engine::{CallKind, CallRecord};
use oxbot_protocol::{NetKind, NetTotals};
use oxbot_relay::AiExchange;
use rusqlite::types::Value as SqlValue;
use rusqlite::{Row, params, params_from_iter};
use serde::{Deserialize, Serialize};

use super::{Store, StoreError};

/// How long rows are kept, in days.
pub const KEEP_NET_DAYS: i64 = 30;

const DAY_MS: i64 = 86_400_000;

/// What a row says beyond its columns (kept as JSON in `detail`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetDetail {
    /// How long it took (a device: how long it stayed connected).
    #[serde(default)]
    pub ms: u64,
    /// `ok`, or a short plain reason.
    #[serde(default)]
    pub outcome: String,
    /// Personal details swapped for stand-ins, by kind → how many.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub swapped: BTreeMap<String, u32>,
    /// AI: the model that answered (its id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// AI: input tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_in: Option<u64>,
    /// AI: output tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_out: Option<u64>,
    /// Websites: pages read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// Websites: other sites the pages pulled from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_hosts: Option<u32>,
    /// Devices: when it disconnected (ms since the Unix epoch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<i64>,
}

/// One stored exchange.
#[derive(Debug, Clone, PartialEq)]
pub struct NetRecord {
    /// Its id (0 before it's stored).
    pub id: i64,
    /// When it happened (ms since the Unix epoch).
    pub ts_ms: i64,
    /// What kind.
    pub kind: NetKind,
    /// The name shown when it was written (a bot's, a device's, `oxbot`).
    pub who: String,
    /// The bot's id.
    pub bot: Option<String>,
    /// The person's id.
    pub person: Option<String>,
    /// The device's id.
    pub device: Option<String>,
    /// Where it went.
    pub host: String,
    /// What it was, in plain words.
    pub label: String,
    /// Bytes sent.
    pub bytes_up: u64,
    /// Bytes received.
    pub bytes_down: u64,
    /// What it cost, in US dollars.
    pub cost_usd: Option<f64>,
    /// The rest.
    pub detail: NetDetail,
}

impl NetRecord {
    /// A row of `kind` at `ts_ms`, everything else empty.
    #[must_use]
    pub fn new(kind: NetKind, ts_ms: i64) -> Self {
        Self {
            id: 0,
            ts_ms,
            kind,
            who: String::new(),
            bot: None,
            person: None,
            device: None,
            host: String::new(),
            label: String::new(),
            bytes_up: 0,
            bytes_down: 0,
            cost_usd: None,
            detail: NetDetail::default(),
        }
    }
}

/// Which rows someone may see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetScope {
    /// Every row (the owner).
    All,
    /// A person: rows of their own bots, of their own devices, and
    /// questions to the AI made for them.
    Person {
        /// Their person id.
        id: String,
        /// The ids of their own bots.
        bots: Vec<String>,
    },
    /// No rows (kid accounts).
    Nothing,
}

impl NetScope {
    /// The `WHERE` condition for this scope, adding its values to `args`.
    /// `None`: no row.
    fn condition(&self, args: &mut Vec<SqlValue>) -> Option<String> {
        match self {
            Self::All => Some("1".to_owned()),
            Self::Nothing => None,
            Self::Person { id, bots } => {
                args.push(SqlValue::Text(id.clone()));
                let me = args.len();
                let mut cond = format!("(person = ?{me} AND kind IN ('ai', 'device'))");
                if !bots.is_empty() {
                    let mut marks = Vec::with_capacity(bots.len());
                    for b in bots {
                        args.push(SqlValue::Text(b.clone()));
                        marks.push(format!("?{}", args.len()));
                    }
                    cond = format!(
                        "({cond} OR (bot IN ({}) AND kind IN ('ai', 'website')))",
                        marks.join(", ")
                    );
                }
                Some(cond)
            }
        }
    }
}

const SELECT: &str = "SELECT id, ts_ms, kind, who, bot, person, device, host, label, bytes_up,
                             bytes_down, cost_usd, detail FROM net_log";

fn to_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn to_u64(n: i64) -> u64 {
    u64::try_from(n.max(0)).unwrap_or(0)
}

fn record(r: &Row<'_>) -> rusqlite::Result<NetRecord> {
    let kind: String = r.get(2)?;
    let detail: String = r.get(12)?;
    Ok(NetRecord {
        id: r.get(0)?,
        ts_ms: r.get(1)?,
        kind: NetKind::from_name(&kind).unwrap_or(NetKind::Oxbot),
        who: r.get(3)?,
        bot: r.get(4)?,
        person: r.get(5)?,
        device: r.get(6)?,
        host: r.get(7)?,
        label: r.get(8)?,
        bytes_up: to_u64(r.get(9)?),
        bytes_down: to_u64(r.get(10)?),
        cost_usd: r.get(11)?,
        detail: serde_json::from_str(&detail).unwrap_or_default(),
    })
}

fn detail_json(detail: &NetDetail) -> String {
    serde_json::to_string(detail).unwrap_or_else(|_| "{}".to_owned())
}

/// `scope`'s condition, and `kind`'s when given; `None`: no row.
fn conditions(
    scope: &NetScope,
    kind: Option<NetKind>,
    args: &mut Vec<SqlValue>,
) -> Option<Vec<String>> {
    let mut conds = vec![scope.condition(args)?];
    if let Some(kind) = kind {
        args.push(SqlValue::Text(kind.as_str().to_owned()));
        conds.push(format!("kind = ?{}", args.len()));
    }
    Some(conds)
}

/// Where every question to the AI goes.
pub const AI_HOST: &str = "openrouter.ai";

/// A model request's outcome (the meter's, or oxbot's own words) in
/// plain words.
#[must_use]
pub fn plain_outcome(outcome: &str) -> String {
    match outcome {
        "ok" => "ok",
        "cancelled" => "stopped",
        "rate_limited" | "busy" => "the model was busy",
        "daily_limit" => "the daily limit was reached",
        o if o.starts_with("error:") => "failed",
        o => o,
    }
    .to_owned()
}

/// A question to the AI as the relay noted it.
fn ai_record(row: &AiExchange) -> NetRecord {
    NetRecord {
        who: row.bot.clone(),
        bot: Some(row.bot.clone()).filter(|b| !b.is_empty()),
        person: row.person.clone(),
        host: AI_HOST.to_owned(),
        label: row.model.clone(),
        bytes_up: row.bytes_up,
        bytes_down: row.bytes_down,
        cost_usd: Some(row.cost_usd),
        detail: NetDetail {
            ms: row.ms,
            outcome: plain_outcome(&row.outcome),
            swapped: row.swapped.clone(),
            model: Some(row.model.clone()).filter(|m| !m.is_empty()),
            tokens_in: Some(row.tokens_in),
            tokens_out: Some(row.tokens_out),
            ..NetDetail::default()
        },
        ..NetRecord::new(NetKind::Ai, row.ts_ms)
    }
}

/// A helper request (a page question, a judge, a summary) that didn't go
/// through the relay: its sizes aren't measured.
fn call_record(c: &CallRecord) -> NetRecord {
    let model = if c.model.is_empty() {
        &c.requested
    } else {
        &c.model
    };
    NetRecord {
        who: c.bot.clone(),
        bot: Some(c.bot.clone()).filter(|b| !b.is_empty()),
        person: c.person.clone(),
        host: AI_HOST.to_owned(),
        label: model.clone(),
        cost_usd: Some(c.cost_usd),
        detail: NetDetail {
            ms: c.latency_ms,
            outcome: plain_outcome(&c.outcome),
            model: Some(model.clone()).filter(|m| !m.is_empty()),
            tokens_in: Some(c.prompt_tokens),
            tokens_out: Some(c.completion_tokens),
            ..NetDetail::default()
        },
        ..NetRecord::new(NetKind::Ai, c.ts_ms)
    }
}

impl Store {
    /// Notes a question to the AI the relay sent. Only logged if it fails.
    pub fn note_ai(&self, row: &AiExchange) {
        if let Err(err) = self.add_net(&ai_record(row)) {
            tracing::warn!(%err, "couldn't note a question to the AI");
        }
    }

    /// Notes a model request that didn't go through the relay (a helper's:
    /// page questions, judging, summaries); the relay notes its own (a
    /// chat step). Only logged if it fails.
    pub fn note_call(&self, c: &CallRecord) {
        if c.kind == CallKind::Chat {
            return;
        }
        if let Err(err) = self.add_net(&call_record(c)) {
            tracing::warn!(%err, "couldn't note a question to the AI");
        }
    }

    /// Stores one exchange; returns its id.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_net(&self, rec: &NetRecord) -> Result<i64, StoreError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO net_log (ts_ms, kind, who, bot, person, device, host, label, bytes_up,
                                  bytes_down, cost_usd, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                rec.ts_ms,
                rec.kind.as_str(),
                rec.who,
                rec.bot,
                rec.person,
                rec.device,
                rec.host,
                rec.label,
                to_i64(rec.bytes_up),
                to_i64(rec.bytes_down),
                rec.cost_usd,
                detail_json(&rec.detail),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Finishes row `id` (a device that disconnected): its sizes and
    /// detail.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn finish_net(
        &self,
        id: i64,
        bytes_up: u64,
        bytes_down: u64,
        detail: &NetDetail,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE net_log SET bytes_up = ?2, bytes_down = ?3, detail = ?4 WHERE id = ?1",
            params![
                id,
                to_i64(bytes_up),
                to_i64(bytes_down),
                detail_json(detail)
            ],
        )?;
        Ok(())
    }

    /// Rows still marked `outcome` (devices left `connected` by an oxbot
    /// that stopped) are marked `ended`. Returns how many.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn end_open_net(&self, outcome: &str) -> Result<usize, StoreError> {
        Ok(self.conn().execute(
            "UPDATE net_log SET detail = json_set(detail, '$.outcome', 'ended')
             WHERE kind = 'device' AND json_extract(detail, '$.outcome') = ?1",
            [outcome],
        )?)
    }

    /// The newest rows `scope` may see, of `kind` if given, older than
    /// `before_ms` if given, at most `limit`; and whether older ones exist.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn net_log(
        &self,
        scope: &NetScope,
        kind: Option<NetKind>,
        before_ms: Option<i64>,
        limit: u32,
    ) -> Result<(Vec<NetRecord>, bool), StoreError> {
        let mut args = Vec::new();
        let Some(mut conds) = conditions(scope, kind, &mut args) else {
            return Ok((Vec::new(), false));
        };
        if let Some(before) = before_ms {
            args.push(SqlValue::Integer(before));
            conds.push(format!("ts_ms < ?{}", args.len()));
        }
        args.push(SqlValue::Integer(i64::from(limit) + 1));
        let sql = format!(
            "{SELECT} WHERE {} ORDER BY ts_ms DESC, id DESC LIMIT ?{}",
            conds.join(" AND "),
            args.len()
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(params_from_iter(args), record)?
            .collect::<Result<Vec<_>, _>>()?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        Ok((rows, more))
    }

    /// Sums of the rows `scope` may see since `since_ms`, of `kind` if
    /// given.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn net_totals(
        &self,
        scope: &NetScope,
        kind: Option<NetKind>,
        since_ms: i64,
    ) -> Result<NetTotals, StoreError> {
        let mut args = Vec::new();
        let Some(mut conds) = conditions(scope, kind, &mut args) else {
            return Ok(NetTotals::default());
        };
        args.push(SqlValue::Integer(since_ms));
        conds.push(format!("ts_ms >= ?{}", args.len()));
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(bytes_up), 0), COALESCE(SUM(bytes_down), 0),
                    COALESCE(SUM(cost_usd), 0) FROM net_log WHERE {}",
            conds.join(" AND ")
        );
        Ok(self.conn().query_row(&sql, params_from_iter(args), |r| {
            Ok(NetTotals {
                today_rows: u32::try_from(r.get::<_, i64>(0)?.max(0)).unwrap_or(u32::MAX),
                today_up: to_u64(r.get(1)?),
                today_down: to_u64(r.get(2)?),
                today_cost_usd: r.get(3)?,
            })
        })?)
    }

    /// Deletes rows older than [`KEEP_NET_DAYS`]. Returns how many went.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn prune_net_log(&self, now_ms: i64) -> Result<usize, StoreError> {
        Ok(self.conn().execute(
            "DELETE FROM net_log WHERE ts_ms < ?1",
            [now_ms - KEEP_NET_DAYS * DAY_MS],
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: NetKind, ts_ms: i64, bot: Option<&str>, person: Option<&str>) -> NetRecord {
        NetRecord {
            who: bot.unwrap_or("oxbot").to_owned(),
            bot: bot.map(str::to_owned),
            person: person.map(str::to_owned),
            host: "example.com".into(),
            label: "read 1 page".into(),
            bytes_up: 10,
            bytes_down: 100,
            ..NetRecord::new(kind, ts_ms)
        }
    }

    fn ids(rows: &[NetRecord]) -> Vec<i64> {
        rows.iter().map(|r| r.ts_ms).collect()
    }

    #[test]
    fn rows_come_back_newest_first_with_their_detail() {
        let store = Store::open_in_memory().unwrap();
        let mut ai = row(NetKind::Ai, 2_000, Some("scout"), Some("p1"));
        ai.cost_usd = Some(0.25);
        ai.detail.swapped = BTreeMap::from([("email".to_owned(), 2), ("person".to_owned(), 1)]);
        ai.detail.model = Some("deepseek/v4".into());
        ai.detail.outcome = "ok".into();
        store.add_net(&ai).unwrap();
        store
            .add_net(&row(NetKind::Website, 1_000, Some("scout"), None))
            .unwrap();
        let (rows, more) = store.net_log(&NetScope::All, None, None, 10).unwrap();
        assert!(!more);
        assert_eq!(ids(&rows), [2_000, 1_000]);
        assert_eq!(rows[0].kind, NetKind::Ai);
        assert_eq!(rows[0].cost_usd, Some(0.25));
        assert_eq!(rows[0].detail.swapped["email"], 2);
        assert_eq!(rows[0].detail.model.as_deref(), Some("deepseek/v4"));
        assert_eq!(rows[1].bytes_down, 100);
    }

    #[test]
    fn pages_go_back_by_time_and_say_when_more_are_left() {
        let store = Store::open_in_memory().unwrap();
        for ts in 1..=5 {
            store
                .add_net(&row(NetKind::Oxbot, ts * 1_000, None, None))
                .unwrap();
        }
        let (rows, more) = store.net_log(&NetScope::All, None, None, 2).unwrap();
        assert_eq!(ids(&rows), [5_000, 4_000]);
        assert!(more);
        let (rows, more) = store.net_log(&NetScope::All, None, Some(4_000), 2).unwrap();
        assert_eq!(ids(&rows), [3_000, 2_000]);
        assert!(more);
        let (rows, more) = store.net_log(&NetScope::All, None, Some(2_000), 2).unwrap();
        assert_eq!(ids(&rows), [1_000]);
        assert!(!more);
        let (rows, _) = store
            .net_log(&NetScope::All, Some(NetKind::Ai), None, 10)
            .unwrap();
        assert_eq!(rows, Vec::<NetRecord>::new());
    }

    #[test]
    fn a_person_sees_their_bots_their_devices_and_their_questions_only() {
        let store = Store::open_in_memory().unwrap();
        // Theirs: their bot's visit, their bot's question for anyone,
        // a question for them by a shared bot, their device.
        store
            .add_net(&row(NetKind::Website, 1, Some("mine"), None))
            .unwrap();
        store
            .add_net(&row(NetKind::Ai, 2, Some("mine"), Some("other")))
            .unwrap();
        store
            .add_net(&row(NetKind::Ai, 3, Some("shared"), Some("me")))
            .unwrap();
        let mut phone = row(NetKind::Device, 4, None, Some("me"));
        phone.device = Some("g1".into());
        store.add_net(&phone).unwrap();
        // Not theirs: a shared bot's visit, someone else's question and
        // device, oxbot's own calls.
        store
            .add_net(&row(NetKind::Website, 5, Some("shared"), Some("me")))
            .unwrap();
        store
            .add_net(&row(NetKind::Ai, 6, Some("shared"), Some("other")))
            .unwrap();
        store
            .add_net(&row(NetKind::Device, 7, None, Some("other")))
            .unwrap();
        store
            .add_net(&row(NetKind::Oxbot, 8, None, Some("me")))
            .unwrap();
        let me = NetScope::Person {
            id: "me".into(),
            bots: vec!["mine".into()],
        };
        let (rows, _) = store.net_log(&me, None, None, 100).unwrap();
        assert_eq!(ids(&rows), [4, 3, 2, 1]);
        let totals = store.net_totals(&me, None, 0).unwrap();
        assert_eq!(totals.today_rows, 4);
        assert_eq!(totals.today_up, 40);
        // With no bots of their own.
        let bare = NetScope::Person {
            id: "me".into(),
            bots: Vec::new(),
        };
        let (rows, _) = store.net_log(&bare, None, None, 100).unwrap();
        assert_eq!(ids(&rows), [4, 3]);
        let (rows, more) = store.net_log(&NetScope::Nothing, None, None, 100).unwrap();
        assert!(rows.is_empty() && !more);
        assert_eq!(
            store.net_totals(&NetScope::Nothing, None, 0).unwrap(),
            NetTotals::default()
        );
        let (all, _) = store.net_log(&NetScope::All, None, None, 100).unwrap();
        assert_eq!(all.len(), 8);
    }

    #[test]
    fn questions_to_the_ai_are_rows_and_relay_chat_calls_arent_counted_twice() {
        use oxbot_loop::store::LoopStore as _;
        let store = Store::open_in_memory().unwrap();
        store.note_ai(&AiExchange {
            ts_ms: 5,
            bot: "scout".into(),
            person: Some("p1".into()),
            model: "deepseek/v4".into(),
            cost_usd: 0.01,
            tokens_in: 900,
            tokens_out: 40,
            ms: 1_200,
            outcome: "rate_limited".into(),
            bytes_up: 4_000,
            bytes_down: 800,
            swapped: BTreeMap::from([("email".to_owned(), 1)]),
        });
        // The relay's own chat steps are metered too, but noted above.
        let chat = CallRecord {
            ts_ms: 6,
            bot: "scout".into(),
            model: "a/b".into(),
            outcome: "ok".into(),
            kind: CallKind::Chat,
            ..CallRecord::default()
        };
        store.record_call(&chat).unwrap();
        // A helper's page question isn't: it's noted here.
        store
            .record_call(&CallRecord {
                ts_ms: 7,
                kind: CallKind::PageQuestion,
                ..chat
            })
            .unwrap();
        let (rows, _) = store.net_log(&NetScope::All, None, None, 10).unwrap();
        assert_eq!(rows.len(), 2);
        let (helper, relayed) = (&rows[0], &rows[1]);
        assert_eq!(helper.ts_ms, 7);
        assert_eq!(helper.label, "a/b");
        assert_eq!(helper.detail.outcome, "ok");
        assert_eq!(relayed.kind, NetKind::Ai);
        assert_eq!(relayed.host, AI_HOST);
        assert_eq!((relayed.bytes_up, relayed.bytes_down), (4_000, 800));
        assert_eq!(relayed.cost_usd, Some(0.01));
        assert_eq!(relayed.detail.outcome, "the model was busy");
        assert_eq!(relayed.detail.swapped["email"], 1);
        assert_eq!(relayed.detail.tokens_in, Some(900));
        assert_eq!(relayed.person.as_deref(), Some("p1"));
    }

    #[test]
    fn totals_count_today_only() {
        let store = Store::open_in_memory().unwrap();
        let mut old = row(NetKind::Ai, 500, Some("b"), None);
        old.cost_usd = Some(1.0);
        store.add_net(&old).unwrap();
        let mut new = row(NetKind::Ai, 1_500, Some("b"), None);
        new.cost_usd = Some(0.5);
        store.add_net(&new).unwrap();
        store
            .add_net(&row(NetKind::Website, 1_600, Some("b"), None))
            .unwrap();
        let t = store.net_totals(&NetScope::All, None, 1_000).unwrap();
        assert_eq!((t.today_rows, t.today_up, t.today_down), (2, 20, 200));
        assert!((t.today_cost_usd - 0.5).abs() < 1e-9);
        let t = store
            .net_totals(&NetScope::All, Some(NetKind::Website), 1_000)
            .unwrap();
        assert_eq!(t.today_rows, 1);
    }

    #[test]
    fn a_device_row_is_finished_and_old_rows_go() {
        let store = Store::open_in_memory().unwrap();
        let now = 100 * DAY_MS;
        let mut phone = row(NetKind::Device, now - DAY_MS, None, Some("me"));
        phone.detail.outcome = "connected".into();
        let id = store.add_net(&phone).unwrap();
        let mut left = row(NetKind::Device, now - 2 * DAY_MS, None, Some("me"));
        left.detail.outcome = "connected".into();
        let left = store.add_net(&left).unwrap();
        let done = NetDetail {
            ms: 60_000,
            outcome: "ok".into(),
            ended_ms: Some(now),
            ..NetDetail::default()
        };
        store.finish_net(id, 1_000, 2_000, &done).unwrap();
        // An oxbot that stopped left the other one open.
        assert_eq!(store.end_open_net("connected").unwrap(), 1);
        store
            .add_net(&row(NetKind::Oxbot, now - 31 * DAY_MS, None, None))
            .unwrap();
        assert_eq!(store.prune_net_log(now).unwrap(), 1);
        let (rows, _) = store.net_log(&NetScope::All, None, None, 10).unwrap();
        assert_eq!(rows.len(), 2);
        let finished = rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!((finished.bytes_up, finished.bytes_down), (1_000, 2_000));
        assert_eq!(finished.detail, done);
        let ended = rows.iter().find(|r| r.id == left).unwrap();
        assert_eq!(ended.detail.outcome, "ended");
    }
}
