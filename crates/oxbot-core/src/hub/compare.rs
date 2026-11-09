//! Comparing engines and models per bot (#21), for the home computer owner: thumbs
//! on replies, test cases made from finished turns, batches of practice
//! runs on Codex and on oxbot's own loop, the owner's verdicts and the
//! scoreboard.
//!
//! Every call here is the owner's only: the home computer connection, or the home computer's
//! person on their own device. Cases come only from bots the owner may task
//! (shared bots and their own private ones).

use oxbot_protocol::{
    Attachment, BatchEstimate, BotEvent, CaseDraft, CaseResult, ComparePrefs,
    EngineKind as WireEngine, EventKind, Outcome, RunVerdict, ScoreRow, ServerMsg, Target,
    TestCaseInfo, TrialNetwork,
};
use serde_json::{Value, json};

use super::trials::{
    Job, PROVIDER, cases_dir, copy_tree, original_steps, safe_relative, trial_mode,
};
use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::compare::{self, Expected, Fixtures};
use crate::engine::{EngineKind, from_wire};
use crate::id::BotId;
use crate::store::{BatchRecord, TestCaseRecord, TrialRunRecord, now_ms};
use crate::tools;

/// What may run in a batch: each case with each target that may run it,
/// and the targets refused, with why.
type Plan = (Vec<(TestCaseRecord, Target)>, Vec<String>);

/// Free `OpenRouter` requests kept for live bots, as a share of the day's.
pub(super) const PREF_RESERVE: &str = "compare.reserve";
/// Most runs one batch may make.
const MAX_BATCH_RUNS: usize = 60;

fn engine_name(e: WireEngine) -> &'static str {
    from_wire(e).as_str()
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(80) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// A turn's meter totals (requests, input, output and cached tokens,
/// dollars, model) as a run's use.
fn used_of(
    (requests, input, output, cached, cost, _): (u32, u64, u64, u64, f64, String),
) -> TrialRunRecord {
    TrialRunRecord {
        requests,
        input_tokens: input,
        output_tokens: output,
        cached_tokens: cached,
        cost_usd: cost,
        ..TrialRunRecord::default()
    }
}

/// A finished turn read from a bot's chat.
struct Turn {
    user: BotEvent,
    events: Vec<BotEvent>,
    done_seq: u64,
    done_ms: i64,
    duration_ms: u64,
}

impl Turn {
    fn prompt(&self) -> (&str, &[Attachment], Option<&str>) {
        match &self.user.kind {
            EventKind::UserMessage { text, files, from } => (text, files, from.as_deref()),
            _ => ("", &[], None),
        }
    }

    /// The steps it asked about: done (allowed, or a rule let it through)
    /// or not (said no, or no answer).
    fn expected(&self) -> Vec<Expected> {
        self.events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::Approval {
                    action,
                    target,
                    site,
                    outcome,
                } => Some(Expected {
                    action: action.clone(),
                    target: target.clone(),
                    site: site.clone(),
                    expect: matches!(outcome, Outcome::Allowed | Outcome::AllowedAlways),
                }),
                EventKind::AllowedByRule {
                    action,
                    target,
                    site,
                } => Some(Expected {
                    action: action.clone(),
                    target: target.clone(),
                    site: site.clone(),
                    expect: true,
                }),
                _ => None,
            })
            .collect()
    }

    fn count(&self, f: impl Fn(&EventKind) -> bool) -> u32 {
        u32::try_from(self.events.iter().filter(|e| f(&e.kind)).count()).unwrap_or(0)
    }

    fn answer(&self) -> String {
        self.events
            .iter()
            .rev()
            .find_map(|e| match &e.kind {
                EventKind::AssistantMessage { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn ok(&self) -> bool {
        self.events
            .iter()
            .any(|e| matches!(e.kind, EventKind::TurnDone { ok: true, .. }))
    }
}

/// The rubric the "Test case…" sheet starts with: one line per step the
/// original asked about.
fn draft_rubric(expected: &[Expected]) -> String {
    expected
        .iter()
        .map(|e| {
            if e.expect {
                format!("Would {}", e.describe())
            } else {
                format!("Wouldn't {}", e.describe())
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Hub {
    fn owner_only(&self, viewer: &Viewer) -> Result<(), HubError> {
        if self.is_owner_viewer(viewer) {
            Ok(())
        } else {
            Err(HubError::NotAllowed(format!(
                "Only {} compares AI models.",
                self.owner_name()
            )))
        }
    }

    /// Bot `id`, if the owner may compare it: one they may task, not
    /// another person's private bot, not an Assistant.
    fn compared_bot(&self, id: &str, viewer: &Viewer) -> Result<Bot, HubError> {
        self.owner_only(viewer)?;
        let bot = self.bot_for(id, viewer)?;
        if bot.private && bot.owner != self.owner_person_id() {
            return Err(HubError::NotAllowed(format!(
                "{} is someone else's private bot: its tasks stay theirs.",
                bot.name
            )));
        }
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "The Assistant's tasks pass work to other bots, so they can't be compared yet."
                    .to_owned(),
            ));
        }
        Ok(bot)
    }

    /// The turn of `bot` that ended at event `done_seq` (its `TurnDone`).
    fn finished_turn(&self, bot: &BotId, done_seq: u64) -> Result<Turn, HubError> {
        let page = self.store.history_page(bot, Some(done_seq + 1), 500)?;
        let Some(done) = page.events.last().filter(|e| e.seq == done_seq) else {
            return Err(HubError::NotAllowed(
                "That reply isn't there any more.".to_owned(),
            ));
        };
        let EventKind::TurnDone { duration_ms, .. } = done.kind else {
            return Err(HubError::NotAllowed(
                "That isn't the end of a reply.".to_owned(),
            ));
        };
        let (done_ms, duration_ms) = (done.ts_ms, duration_ms);
        let start = page
            .events
            .iter()
            .rposition(|e| matches!(e.kind, EventKind::UserMessage { .. }))
            .ok_or_else(|| {
                HubError::NotAllowed("That reply's message is too far back.".to_owned())
            })?;
        let mut events = page.events;
        let rest = events.split_off(start);
        let user = rest[0].clone();
        Ok(Turn {
            user,
            events: rest,
            done_seq,
            done_ms,
            duration_ms,
        })
    }

    /// The hosts `bot`'s session opened during `turn`, and whether any is
    /// signed in on the bot's browser.
    fn turn_sites(&self, bot: &Bot, turn: &Turn) -> (Vec<String>, bool) {
        let mut sites: Vec<String> = self
            .store
            .session_pages(&bot.id)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, v)| v.first_ms >= turn.user.ts_ms && v.first_ms <= turn.done_ms)
            .filter_map(|(url, _)| oxbot_browser::web_host(&url))
            .collect();
        sites.sort();
        sites.dedup();
        let signed_in: Vec<String> = self
            .browser_owner_of(bot)
            .and_then(|o| self.store.sites(o).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.site)
            .collect();
        let uses = sites
            .iter()
            .any(|h| signed_in.contains(&oxbot_browser::site_of(h)));
        (sites, uses)
    }

    /// The engine and model that ran a live turn, as the scoreboard names
    /// them (a turn on an engine oxbot no longer has keeps its name, which
    /// the scoreboard leaves out), with what it used: the meter's rows
    /// from the message to the reply's end.
    fn turn_model(&self, bot: &Bot, turn: &Turn) -> (String, String, Option<TrialRunRecord>) {
        let recorded = self
            .store
            .turn_origin(&bot.id, turn.user.seq)
            .ok()
            .flatten()
            .map(|o| o.engine);
        let engine = match recorded.as_deref().map(EngineKind::parse) {
            Some(Some(kind)) => kind,
            // An engine oxbot no longer has: named as it was.
            Some(None) => {
                let name = recorded.unwrap_or_default();
                return (name.clone(), name, None);
            }
            None => self.engine_of(bot),
        };
        let totals = self
            .store
            .engine_calls_totals(
                bot.id.as_str(),
                engine.as_str(),
                turn.user.ts_ms,
                turn.done_ms,
            )
            .ok()
            .filter(|t| t.0 > 0);
        let model = totals
            .as_ref()
            .map(|t| t.5.clone())
            .filter(|m| !m.is_empty())
            .or_else(|| bot.loop_settings.models.first().cloned())
            .or_else(|| bot.loop_settings.model.clone())
            .unwrap_or_else(|| "auto".to_owned());
        (engine.as_str().to_owned(), model, totals.map(used_of))
    }

    /// A live turn as a run of kind `original`, for the scoreboard.
    fn original_run(
        &self,
        bot: &Bot,
        turn: &Turn,
        case: Option<&str>,
        batch: Option<&str>,
    ) -> TrialRunRecord {
        let (engine, model, used) = self.turn_model(bot, turn);
        let used = used.unwrap_or_default();
        let is = |k: &'static str| {
            move |e: &EventKind| match e {
                EventKind::ToolUse { .. } => k == "tool",
                EventKind::Approval { .. } | EventKind::AllowedByRule { .. } => k == "card",
                EventKind::Blocked { .. } => k == "blocked",
                EventKind::Error { .. } => k == "error",
                _ => false,
            }
        };
        TrialRunRecord {
            id: format!("r_{}", uuid::Uuid::new_v4().simple()),
            batch: batch.map(str::to_owned),
            case_id: case.map(str::to_owned),
            bot: bot.id.to_string(),
            kind: "original".to_owned(),
            engine,
            provider: PROVIDER.to_owned(),
            model,
            started_ms: Some(turn.done_ms - i64::try_from(turn.duration_ms).unwrap_or(0)),
            ended_ms: Some(turn.done_ms),
            status: if turn.ok() { "done" } else { "failed" }.to_owned(),
            steps: turn.count(is("tool")),
            intents: turn.count(is("card")),
            refusals: turn.count(is("blocked")),
            errors: turn.count(is("error")),
            answer: turn.answer(),
            intent_list: json!([]),
            ..used
        }
    }

    /// `viewer`'s thumbs-up (or down, or neither) on `bot`'s reply that
    /// ended at `seq`.
    ///
    /// # Errors
    /// Not the owner, no such reply, or the database failed.
    pub fn rate_turn(
        &self,
        id: &str,
        seq: u64,
        up: Option<bool>,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        self.owner_only(viewer)?;
        let bot = self.bot_for(id, viewer)?;
        let turn = self.finished_turn(&bot.id, seq)?;
        let (engine, model, _) = self.turn_model(&bot, &turn);
        self.store.set_rating(&bot.id, seq, up, &engine, &model)?;
        Ok(())
    }

    /// `bot`'s thumbs and the replies kept as test cases.
    ///
    /// # Errors
    /// Not the owner, or the database failed.
    pub fn ratings_for(&self, id: &str, viewer: &Viewer) -> Result<ServerMsg, HubError> {
        self.owner_only(viewer)?;
        let bot = self.bot_for(id, viewer)?;
        let cases = self
            .store
            .test_cases(&bot.id)?
            .into_iter()
            .map(|c| c.seq_to)
            .collect();
        let ratings = self.store.rating_list(&bot.id)?.into_iter().collect();
        Ok(ServerMsg::Ratings {
            bot: bot.id.to_string(),
            ratings,
            cases,
        })
    }

    /// What the "Test case…" sheet starts with for the turn that ended at
    /// `seq`.
    ///
    /// # Errors
    /// Not the owner, not a bot they may compare, or no such turn.
    pub fn draft_test_case(
        &self,
        id: &str,
        seq: u64,
        viewer: &Viewer,
    ) -> Result<CaseDraft, HubError> {
        let bot = self.compared_bot(id, viewer)?;
        let turn = self.finished_turn(&bot.id, seq)?;
        let expected = turn.expected();
        let (sites, uses_sign_ins) = self.turn_sites(&bot, &turn);
        let (prompt, files, _) = turn.prompt();
        let title = if prompt.trim().is_empty() {
            files
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            first_line(prompt)
        };
        Ok(CaseDraft {
            bot: bot.id.to_string(),
            seq,
            title,
            rubric: draft_rubric(&expected),
            kind: if expected.is_empty() { "reads" } else { "acts" }.to_owned(),
            sites,
            uses_sign_ins,
        })
    }

    /// Keeps `bot`'s turn that ended at `seq` as a test case: its message
    /// and files, the bot's instructions now, what it asked about, what
    /// oxbot's own tools answered, and the turn itself as the first run.
    ///
    /// # Errors
    /// Not the owner, not a bot they may compare, no such turn, or the
    /// files or database failed.
    #[allow(clippy::too_many_arguments, reason = "one per field of the sheet")]
    pub async fn mark_test_case(
        &self,
        id: &str,
        seq: u64,
        title: &str,
        rubric: &str,
        network: TrialNetwork,
        uses_sign_ins: Option<bool>,
        viewer: &Viewer,
    ) -> Result<TestCaseInfo, HubError> {
        let bot = self.compared_bot(id, viewer)?;
        let turn = self.finished_turn(&bot.id, seq)?;
        let (prompt, files, _) = turn.prompt();
        let (prompt, files) = (prompt.to_owned(), files.to_vec());
        let expected = turn.expected();
        let (sites, signed_in) = self.turn_sites(&bot, &turn);
        let viewer = self.resolve(viewer).into_owned();
        let case_id = format!("c_{}", uuid::Uuid::new_v4().simple());

        // The files, copied so deleting the chat's upload keeps the case.
        if !files.is_empty() {
            let (from, to, list) = (
                bot.work_dir(),
                cases_dir(&self.config.data_dir)
                    .join(&case_id)
                    .join("files"),
                files.clone(),
            );
            tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                for f in &list {
                    for rel in std::iter::once(&f.path).chain(f.readable.as_ref()) {
                        if safe_relative(rel) && from.join(rel).exists() {
                            copy_tree(&from.join(rel), &to.join(rel))?;
                        }
                    }
                }
                Ok(())
            })
            .await
            .map_err(|e| HubError::NotAllowed(e.to_string()))?
            .map_err(|e| HubError::NotAllowed(format!("Couldn't keep the files: {e}")))?;
        }

        // What oxbot's own tools answered, given back in the same order.
        let mut fixtures = Fixtures::default();
        for t in self.store.turn_tools(&bot.id, turn.user.seq)? {
            match t.tool.as_str() {
                tools::GIVE_TASK_TOOL => fixtures.give_task.push(t.result),
                tools::READ_REPLY_TOOL => fixtures.read_reply.push(t.result),
                "ask_user" => fixtures.ask_user.push(t.result),
                _ => {}
            }
        }
        let origin = self.store.turn_origin(&bot.id, turn.user.seq)?;
        let original = self.original_run(&bot, &turn, Some(&case_id), None);
        let case = TestCaseRecord {
            id: case_id.clone(),
            bot: bot.id.to_string(),
            created_ms: now_ms(),
            created_by: viewer.id.as_ref().map(ToString::to_string),
            seq_from: turn.user.seq,
            seq_to: turn.done_seq,
            title: if title.trim().is_empty() {
                first_line(&prompt)
            } else {
                title.trim().to_owned()
            },
            prompt,
            files: serde_json::to_value(&files).unwrap_or(Value::Null),
            instructions: bot.instructions.clone(),
            knowledge: self.knowledge.snapshot(&bot.id, turn.user.seq),
            kind: if expected.is_empty() { "reads" } else { "acts" }.to_owned(),
            rubric: rubric.trim().to_owned(),
            expected: serde_json::to_value(&expected).unwrap_or(Value::Null),
            fixtures: serde_json::to_value(&fixtures).unwrap_or(Value::Null),
            sites: json!(sites),
            uses_sign_ins: uses_sign_ins.unwrap_or(signed_in),
            browser_owner: self
                .browser_owner_of(&bot)
                .map(|o| o.as_ref().to_owned())
                .unwrap_or_default(),
            network: trial_mode(network).as_str().to_owned(),
            local_only: false,
            origin: json!({
                "person": origin.as_ref().and_then(|o| o.person.clone()),
                "tainted_by": origin.as_ref().and_then(|o| o.tainted_by.clone()),
                "ms": turn.user.ts_ms,
                "steps": original.steps,
            }),
            archived: false,
        };
        self.store.add_test_case(&case)?;
        self.store.save_run(&original)?;
        tracing::info!(bot = %bot.id, case = case_id, "kept a turn as a test case");
        Ok(self.case_info(&case))
    }

    fn case_info(&self, c: &TestCaseRecord) -> TestCaseInfo {
        let runs = BotId::new(&c.bot)
            .ok()
            .and_then(|b| self.store.runs(&b, Some(&c.id)).ok())
            .unwrap_or_default();
        let mut last: Vec<CaseResult> = Vec::new();
        for r in runs.iter().filter(|r| r.kind != "original") {
            let Some(engine) = compare::engine_of(&r.engine) else {
                continue;
            };
            if last
                .iter()
                .any(|l| l.engine == engine && l.model == r.model)
            {
                continue;
            }
            last.push(CaseResult {
                engine,
                model: r.model.clone(),
                status: r.status.clone(),
                verdict: compare::run_info(r, "").and_then(|i| i.verdict),
            });
        }
        TestCaseInfo {
            id: c.id.clone(),
            bot: c.bot.clone(),
            title: c.title.clone(),
            prompt: c.prompt.clone(),
            kind: c.kind.clone(),
            rubric: c.rubric.clone(),
            sites: serde_json::from_value(c.sites.clone()).unwrap_or_default(),
            uses_sign_ins: c.uses_sign_ins,
            network: if c.network == "xhr" {
                TrialNetwork::Xhr
            } else {
                TrialNetwork::Strict
            },
            created_ms: c.created_ms,
            archived: c.archived,
            last,
        }
    }

    /// Test case `id`, if `viewer` (the owner) may compare its bot.
    fn case_for(&self, id: &str, viewer: &Viewer) -> Result<TestCaseRecord, HubError> {
        self.owner_only(viewer)?;
        let case = self
            .store
            .test_case(id)?
            .ok_or_else(|| HubError::NotAllowed("That test case was deleted.".to_owned()))?;
        self.bot_for(&case.bot, viewer)?;
        Ok(case)
    }

    /// `bot`'s test cases.
    ///
    /// # Errors
    /// Not the owner, or the database failed.
    pub fn test_cases_for(&self, id: &str, viewer: &Viewer) -> Result<Vec<TestCaseInfo>, HubError> {
        self.owner_only(viewer)?;
        let bot = self.bot_for(id, viewer)?;
        Ok(self
            .store
            .test_cases(&bot.id)?
            .iter()
            .map(|c| self.case_info(c))
            .collect())
    }

    /// Changes a test case. Returns its bot's cases.
    ///
    /// # Errors
    /// Not the owner, no such case, or the database failed.
    pub fn update_test_case(
        &self,
        id: &str,
        title: Option<&str>,
        rubric: Option<&str>,
        network: Option<TrialNetwork>,
        archived: Option<bool>,
        viewer: &Viewer,
    ) -> Result<(String, Vec<TestCaseInfo>), HubError> {
        let case = self.case_for(id, viewer)?;
        let title = title.map(str::trim).filter(|t| !t.is_empty());
        self.store.update_test_case(
            id,
            title,
            rubric.map(str::trim),
            network.map(|n| trial_mode(n).as_str()),
            archived,
        )?;
        let cases = self.test_cases_for(&case.bot, viewer)?;
        Ok((case.bot, cases))
    }

    /// Deletes a test case, its files and its runs. Returns its bot's cases.
    ///
    /// # Errors
    /// Not the owner, no such case, or the database failed.
    pub fn delete_test_case(
        &self,
        id: &str,
        viewer: &Viewer,
    ) -> Result<(String, Vec<TestCaseInfo>), HubError> {
        let case = self.case_for(id, viewer)?;
        self.store.delete_test_case(id)?;
        let dir = cases_dir(&self.config.data_dir).join(&case.id);
        if dir.exists()
            && let Err(err) = std::fs::remove_dir_all(&dir)
        {
            tracing::warn!(%err, "couldn't delete a test case's files");
        }
        let cases = self.test_cases_for(&case.bot, viewer)?;
        Ok((case.bot, cases))
    }

    /// Checks a batch: the cases are this bot's, and each target may run
    /// them. Returns the cases and the targets refused, with why.
    fn batch_plan(
        &self,
        bot: &Bot,
        cases: &[String],
        targets: &[Target],
        repeats: u32,
    ) -> Result<Plan, HubError> {
        if !(1..=3).contains(&repeats) {
            return Err(HubError::NotAllowed(
                "Run each case 1 to 3 times.".to_owned(),
            ));
        }
        if cases.is_empty() || targets.is_empty() {
            return Err(HubError::NotAllowed(
                "Pick at least one case and one model.".to_owned(),
            ));
        }
        let mut found = Vec::new();
        for id in cases {
            let case = self
                .store
                .test_case(id)?
                .filter(|c| c.bot == bot.id.as_str())
                .ok_or_else(|| HubError::NotAllowed("A test case was deleted.".to_owned()))?;
            found.push(case);
        }
        let mut pairs = Vec::new();
        let mut refused = Vec::new();
        for target in targets {
            let model = target.model.trim();
            let name = format!("{}/{model}", engine_name(target.engine));
            if model.is_empty() || model.len() > 120 || model.chars().any(char::is_whitespace) {
                refused.push(format!("{name}: not a model name"));
                continue;
            }
            if let Err(err) = self.engine(from_wire(target.engine)) {
                refused.push(format!("{name}: {err}"));
                continue;
            }
            for case in &found {
                if case.local_only {
                    refused.push(format!(
                        "{name} for \"{}\": the bot keeps its data on the home computer",
                        case.title
                    ));
                    continue;
                }
                pairs.push((
                    case.clone(),
                    Target {
                        engine: target.engine,
                        model: model.to_owned(),
                    },
                ));
            }
        }
        Ok((pairs, refused))
    }

    /// What running `cases` on `targets` `repeats` times would take.
    ///
    /// # Errors
    /// Not the owner, not a bot they may compare, or a case isn't there.
    pub fn estimate_batch(
        &self,
        id: &str,
        cases: &[String],
        targets: &[Target],
        repeats: u32,
        viewer: &Viewer,
    ) -> Result<BatchEstimate, HubError> {
        let bot = self.compared_bot(id, viewer)?;
        let (pairs, refused) = self.batch_plan(&bot, cases, targets, repeats)?;
        // Both engines' requests go to OpenRouter.
        let openrouter: u32 = pairs
            .iter()
            .map(|(case, _)| repeats * compare::requests_per_run(original_steps(case)))
            .sum();
        let paid = pairs.iter().any(|(_, t)| !t.model.ends_with(":free"));
        let (left, reserve) = self.openrouter_room();
        Ok(BatchEstimate {
            bot: bot.id.to_string(),
            runs: u32::try_from(pairs.len()).unwrap_or(0) * repeats,
            openrouter_requests: openrouter,
            openrouter_left: left,
            reserve,
            paid,
            fits: !pairs.is_empty()
                && pairs.len() * repeats as usize <= MAX_BATCH_RUNS
                && left.is_none_or(|l| openrouter <= l.saturating_sub(reserve)),
            refused,
        })
    }

    /// Free `OpenRouter` requests left today (if known) and how many are
    /// kept for live bots.
    pub(super) fn openrouter_room(&self) -> (Option<u32>, u32) {
        let pacer = self.loop_engine.as_ref().map(|e| e.pacer());
        let left = pacer.as_ref().and_then(|p| p.free_left());
        let daily = pacer
            .and_then(|p| p.key_info())
            .and_then(|k| k.free_daily)
            .map_or(1000, |q| q.limit);
        let share = self
            .store
            .pref(PREF_RESERVE)
            .ok()
            .flatten()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| *v <= 90)
            .unwrap_or(30);
        (left, daily * share / 100)
    }

    /// Starts running `cases` on `targets`, one run after another. Returns
    /// the batch's id.
    ///
    /// # Errors
    /// Not the owner, not a bot they may compare, nothing that may run, or
    /// it doesn't fit in what's left today.
    pub fn start_batch(
        &self,
        id: &str,
        cases: &[String],
        targets: &[Target],
        repeats: u32,
        viewer: &Viewer,
    ) -> Result<String, HubError> {
        let estimate = self.estimate_batch(id, cases, targets, repeats, viewer)?;
        if !estimate.fits {
            return Err(HubError::NotAllowed(if estimate.runs == 0 {
                format!("Nothing can run: {}", estimate.refused.join("; "))
            } else {
                "That's more than today's free AI questions allow (some are kept for live bots), or \
                 more than 60 runs. Pick fewer cases or models."
                    .to_owned()
            }));
        }
        let bot = self.compared_bot(id, viewer)?;
        let (pairs, _) = self.batch_plan(&bot, cases, targets, repeats)?;
        let batch_id = format!("b_{}", uuid::Uuid::new_v4().simple());
        self.store.add_batch(&BatchRecord {
            id: batch_id.clone(),
            bot: bot.id.to_string(),
            kind: "replay".to_owned(),
            created_ms: now_ms(),
            targets: serde_json::to_value(targets).unwrap_or(Value::Null),
            cases: json!(cases),
            repeats,
            est_requests: estimate.openrouter_requests,
            status: "queued".to_owned(),
        })?;
        for (case, target) in &pairs {
            for _ in 0..repeats {
                self.store.save_run(&TrialRunRecord {
                    id: format!("r_{}", uuid::Uuid::new_v4().simple()),
                    batch: Some(batch_id.clone()),
                    case_id: Some(case.id.clone()),
                    bot: bot.id.to_string(),
                    kind: "replay".to_owned(),
                    engine: engine_name(target.engine).to_owned(),
                    provider: PROVIDER.to_owned(),
                    model: target.model.clone(),
                    status: "queued".to_owned(),
                    intent_list: json!([]),
                    ..TrialRunRecord::default()
                })?;
            }
        }
        tracing::info!(bot = %bot.id, batch = batch_id, runs = estimate.runs, "started a batch of practice runs");
        self.queue_trial(Job::Batch(batch_id.clone()));
        Ok(batch_id)
    }

    /// Stops batch `id`.
    ///
    /// # Errors
    /// Not the owner, or no such batch.
    pub async fn stop_batch(&self, id: &str, viewer: &Viewer) -> Result<(), HubError> {
        self.owner_only(viewer)?;
        let batch = self
            .store
            .batch(id)?
            .ok_or_else(|| HubError::NotAllowed("That batch is gone.".to_owned()))?;
        self.bot_for(&batch.bot, viewer)?;
        self.stop_trials_of(id).await;
        Ok(())
    }

    /// `bot`'s runs (of `case` only, if given), newest first.
    ///
    /// # Errors
    /// Not the owner, or the database failed.
    pub fn runs_for(
        &self,
        id: &str,
        case: Option<&str>,
        viewer: &Viewer,
    ) -> Result<Vec<oxbot_protocol::RunInfo>, HubError> {
        self.owner_only(viewer)?;
        let bot = self.bot_for(id, viewer)?;
        let titles: std::collections::HashMap<String, String> = self
            .store
            .test_cases(&bot.id)?
            .into_iter()
            .map(|c| (c.id, c.title))
            .collect();
        Ok(self
            .store
            .runs(&bot.id, case)?
            .iter()
            .filter_map(|r| {
                let title = r
                    .case_id
                    .as_ref()
                    .and_then(|c| titles.get(c))
                    .map_or("", String::as_str);
                compare::run_info(r, title)
            })
            .collect())
    }

    fn run_for(&self, id: &str, viewer: &Viewer) -> Result<TrialRunRecord, HubError> {
        self.owner_only(viewer)?;
        let run = self
            .store
            .run(id)?
            .ok_or_else(|| HubError::NotAllowed("That run was deleted.".to_owned()))?;
        self.bot_for(&run.bot, viewer)?;
        if compare::engine_of(&run.engine).is_none() {
            return Err(HubError::NotAllowed(
                "That run was on Claude, which oxbot no longer runs.".to_owned(),
            ));
        }
        Ok(run)
    }

    /// Run `id` with its transcript, next to the original turn.
    ///
    /// # Errors
    /// Not the owner, no such run, or the database failed.
    pub fn run_detail(&self, id: &str, viewer: &Viewer) -> Result<ServerMsg, HubError> {
        let run = self.run_for(id, viewer)?;
        let case = run
            .case_id
            .as_deref()
            .and_then(|c| self.store.test_case(c).ok().flatten());
        let title = case.as_ref().map(|c| c.title.clone()).unwrap_or_default();
        let original = case.as_ref().and_then(|c| {
            let bot = BotId::new(&c.bot).ok()?;
            self.store
                .runs(&bot, Some(&c.id))
                .ok()?
                .into_iter()
                .find(|r| r.kind == "original")
        });
        let original_events = match &case {
            Some(c) => BotId::new(&c.bot)
                .ok()
                .and_then(|b| self.store.history(&b, c.seq_from.saturating_sub(1)).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.seq <= c.seq_to)
                .collect(),
            None => Vec::new(),
        };
        let info = compare::run_info(&run, &title)
            .ok_or_else(|| HubError::NotAllowed("That run can't be shown.".to_owned()))?;
        Ok(ServerMsg::RunDetail {
            events: if run.kind == "original" {
                Vec::new()
            } else {
                self.store.trial_events(&run.id)?
            },
            run: Box::new(info),
            original: original
                .and_then(|o| compare::run_info(&o, &title))
                .map(Box::new),
            original_events,
        })
    }

    /// The owner's own verdict on run `id`; it wins over a judge's (runs
    /// judged before Claude left oxbot).
    ///
    /// # Errors
    /// Not the owner, no such run, or the database failed.
    pub fn set_run_verdict(
        &self,
        id: &str,
        verdict: Option<RunVerdict>,
        viewer: &Viewer,
    ) -> Result<oxbot_protocol::RunInfo, HubError> {
        let mut run = self.run_for(id, viewer)?;
        run.owner_verdict = verdict.map(|v| compare::verdict_name(v).to_owned());
        let judged = run
            .judge
            .as_ref()
            .and_then(|j| j.get("verdict"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        (run.verdict, run.verdict_by) = match (&run.owner_verdict, judged) {
            (Some(v), _) => (Some(v.clone()), Some("owner".to_owned())),
            (None, Some(j)) => {
                let local = run
                    .judge
                    .as_ref()
                    .and_then(|j| j.get("local"))
                    .and_then(Value::as_bool)
                    == Some(true);
                (
                    Some(j),
                    Some(if local { "local_judge" } else { "judge" }.to_owned()),
                )
            }
            (None, None) => (None, None),
        };
        self.store.save_run(&run)?;
        self.announce_run(&run);
        let title = run
            .case_id
            .as_deref()
            .and_then(|c| self.store.test_case(c).ok().flatten())
            .map(|c| c.title)
            .unwrap_or_default();
        compare::run_info(&run, &title)
            .ok_or_else(|| HubError::NotAllowed("That run can't be shown.".to_owned()))
    }

    /// `bot`'s scoreboard.
    ///
    /// # Errors
    /// Not the owner, or the database failed.
    pub fn scoreboard_for(&self, id: &str, viewer: &Viewer) -> Result<Vec<ScoreRow>, HubError> {
        self.owner_only(viewer)?;
        let bot = self.bot_for(id, viewer)?;
        let runs = self.store.runs(&bot.id, None)?;
        let ratings = self.store.rating_counts(&bot.id)?;
        Ok(compare::scoreboard(&runs, &ratings))
    }

    /// The comparison settings.
    ///
    /// # Errors
    /// Not the owner.
    pub fn compare_prefs(&self, viewer: &Viewer) -> Result<ComparePrefs, HubError> {
        self.owner_only(viewer)?;
        let d = ComparePrefs::default();
        let read = |key: &str, default: u32| {
            self.store
                .pref(key)
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        Ok(ComparePrefs {
            reserve_pct: read(PREF_RESERVE, d.reserve_pct),
        })
    }

    /// Changes the comparison settings (each kept in its range).
    ///
    /// # Errors
    /// Not the owner, or the database failed.
    pub fn set_compare_prefs(
        &self,
        p: ComparePrefs,
        viewer: &Viewer,
    ) -> Result<ComparePrefs, HubError> {
        self.owner_only(viewer)?;
        self.store
            .set_pref(PREF_RESERVE, &p.reserve_pct.min(90).to_string())?;
        self.compare_prefs(viewer)
    }

    /// Deletes every practice run (cases stay).
    ///
    /// # Errors
    /// Not the owner, or the database failed.
    pub fn delete_all_runs(&self, viewer: &Viewer) -> Result<usize, HubError> {
        self.owner_only(viewer)?;
        Ok(self.store.delete_all_runs()?)
    }

    /// Keeps one of oxbot's own tool calls of a live turn for test-case
    /// fixtures: the Assistant's team tools only (never web content).
    pub(super) fn keep_turn_tool(
        &self,
        bot: &BotId,
        tool: &str,
        args: &Value,
        ok: bool,
        text: &str,
    ) {
        if !matches!(
            tool,
            tools::GIVE_TASK_TOOL | tools::READ_REPLY_TOOL | tools::CREATE_BOT_TOOL
        ) {
            return;
        }
        let seq = self.store.last_user_seq(bot).unwrap_or(0);
        if seq == 0 {
            return;
        }
        if let Err(err) = self.store.add_turn_tool(bot, seq, tool, args, text, ok) {
            tracing::warn!(%bot, %err, "couldn't keep an oxbot tool call");
        }
    }
}

/// What a case knew then, once there is knowledge (#20): a snapshot of the
/// items its turn used. Until then, nothing.
pub trait CaseKnowledge: Send + Sync {
    /// The snapshot for `bot`'s turn of message `user_seq`, as JSON.
    fn snapshot(&self, bot: &BotId, user_seq: u64) -> Option<Value>;
}

/// No knowledge yet.
#[derive(Debug, Default)]
pub struct NoKnowledge;

impl CaseKnowledge for NoKnowledge {
    fn snapshot(&self, _bot: &BotId, _user_seq: u64) -> Option<Value> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_name_each_step_the_original_asked_about() {
        let rubric = draft_rubric(&[
            Expected {
                action: "click".into(),
                target: "button \"Post\"".into(),
                site: "jobs.example.test".into(),
                expect: true,
            },
            Expected {
                action: "click".into(),
                target: "button \"Delete\"".into(),
                site: "jobs.example.test".into(),
                expect: false,
            },
        ]);
        assert_eq!(
            rubric,
            "Would click button \"Post\" on jobs.example.test\nWouldn't click button \"Delete\" on jobs.example.test"
        );
        assert_eq!(first_line("\n  post the job\nthen tell me"), "post the job");
        assert_eq!(first_line(&"x".repeat(90)).chars().count(), 81);
    }
}
