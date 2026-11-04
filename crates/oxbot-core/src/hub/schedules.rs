//! Schedules in the hub: a bot proposing one (its card), the scheduler
//! that runs them, and who may see, pause and delete them.
//!
//! - A schedule is made only through its card: `schedule_task` shows the
//!   person the turn acts for "Every weekday at 8:15 AM: today's prep plan"
//!   with Create, Change and Don't. A turn that has read something oxbot
//!   doesn't trust may still propose one, and its card says where the
//!   request came from. Resuming one after such text, and deleting one,
//!   ask too; pausing never needs to.
//! - The scheduler wakes each minute. A due run starts a turn by the same
//!   path as a typed task, as the schedule's person ([`Entry::Schedule`]):
//!   their bots, their Chrome, their limits, every approval request. One
//!   run at a time per bot (else `busy`), at least [`MIN_GAP_MS`] apart
//!   (else `too_soon`), and only under the home's limits (else `limit`). A
//!   run missed by at most [`CATCH_UP_MS`] (the Mac asleep, oxbot closed)
//!   runs once; older ones are noted `missed`.
//! - A run's answer lands in the bot's chat, and a line goes to its
//!   person's devices; so does "needs you" when it waits for an OK or a
//!   sign-in.
//!
//! [`Entry::Schedule`]: super::Entry::Schedule

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use oxbot_protocol::{
    ApprovalKind, EventKind, Outcome, ScheduleDraft, ScheduleEdit, ScheduleInfo, ServerMsg,
};
use serde_json::Value;

use super::engines::{ChainOrigin, Entry};
use super::{Hub, HubError, lock};
use crate::bot::{Bot, Viewer};
use crate::engine::text::data_block;
use crate::id::{BotId, PersonId};
use crate::policy;
use crate::schedule::{self, CATCH_UP_MS, MAX_PER_PERSON, MIN_GAP_MS, Rule};
use crate::store::{NewSchedule, ScheduleRecord, now_ms};
use crate::tools;

/// A scheduled run in progress (one per bot at most).
#[derive(Debug, Clone)]
struct ActiveRun {
    /// Who it runs as.
    person: PersonId,
    /// Its row in `schedule_runs`, once its turn started.
    run: Option<i64>,
    /// Its message in the bot's chat, once recorded.
    seq: u64,
    /// The turn ended before the run was written down: whether it was ok.
    done: Option<bool>,
    /// Its person heard it needs them.
    told_waiting: bool,
}

/// The scheduler's state.
#[derive(Debug, Default)]
pub(super) struct Scheduler {
    /// Runs in progress, by bot.
    active: std::sync::Mutex<HashMap<BotId, ActiveRun>>,
    /// When each bot's last scheduled run started.
    last_start: std::sync::Mutex<HashMap<BotId, i64>>,
    /// Held while schedules are checked, so two checks never overlap.
    tick: tokio::sync::Mutex<()>,
    /// End-to-end tests move the clock forward (never otherwise).
    offset_ms: AtomicI64,
    /// A schedule card's changes ("Change"), by card id, until its tool
    /// reads them.
    pub(super) edits: std::sync::Mutex<HashMap<u64, ScheduleEdit>>,
}

/// A schedule as the person changed it on its card, checked.
///
/// # Errors
/// What's wrong with it, in words for them.
pub(super) fn checked_edit(edit: &ScheduleEdit) -> Result<ScheduleEdit, HubError> {
    let rule = Rule::parse(&edit.rule).map_err(|e| HubError::NotAllowed(e.0))?;
    let task = schedule::clean_task(&edit.task).map_err(|e| HubError::NotAllowed(e.0))?;
    Ok(ScheduleEdit {
        task,
        rule: rule.to_wire(),
    })
}

/// Wakes the scheduler at the start of every minute until the hub goes.
pub(super) async fn run_scheduler(hub: Weak<Hub>) {
    loop {
        // Just past the minute, so a run due at 8:15 starts at 8:15.
        let wait = 60_000 - now_ms().rem_euclid(60_000) + 50;
        tokio::time::sleep(Duration::from_millis(u64::try_from(wait).unwrap_or(60_000))).await;
        let Some(hub) = hub.upgrade() else { return };
        hub.check_schedules().await;
    }
}

/// The first line of a reply, plain, for a notification.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '*', '-', '>', ' '])
                .trim()
        })
        .find(|l| !l.is_empty())
        .unwrap_or_default();
    schedule::one_line(&line.replace("**", ""), 120)
}

impl Hub {
    /// Now, by the scheduler's clock: the Mac's (moved forward only in
    /// end-to-end tests).
    pub(super) fn schedule_now(&self) -> i64 {
        now_ms() + self.scheduler.offset_ms.load(Ordering::Relaxed)
    }

    /// End-to-end tests only: moves the scheduler's clock forward by
    /// `minutes`, or (`None`) to just past the soonest schedule's next
    /// run, then checks schedules.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] outside test mode.
    pub async fn advance_test_clock(
        self: &Arc<Self>,
        minutes: Option<u32>,
    ) -> Result<(), HubError> {
        if !(self.config.e2e || cfg!(any(test, feature = "test-people"))) {
            return Err(HubError::NotAllowed(
                "The clock moves only in test mode.".to_owned(),
            ));
        }
        let now = self.schedule_now();
        let by = match minutes {
            Some(m) => i64::from(m) * 60_000,
            None => match self.store.earliest_next_run()? {
                Some(at) if at >= now => at - now + 1000,
                _ => 0,
            },
        };
        self.scheduler.offset_ms.fetch_add(by, Ordering::Relaxed);
        self.check_schedules().await;
        Ok(())
    }

    /// Schedule `id`'s rule in words, e.g. `Every weekday at 8:15 AM`.
    pub(super) fn schedule_label(&self, id: i64) -> Option<String> {
        let s = self.store.schedule(id).ok()??;
        Rule::parse(&s.rule).ok().map(|r| r.describe())
    }

    /// Tells clients showing schedules to ask again.
    fn announce_schedules(&self) {
        let _ = self.updates.send(ServerMsg::SchedulesChanged);
    }

    /// A line for `person`'s devices about `bot`; `waiting`: it waits for
    /// them.
    fn notify_person(&self, person: &PersonId, bot: &BotId, text: String) {
        self.notify_person_as(person, bot, text, false);
    }

    fn notify_person_as(&self, person: &PersonId, bot: &BotId, text: String, waiting: bool) {
        let _ = self.updates.send(ServerMsg::Notify {
            bot: bot.to_string(),
            text,
            waiting,
            person_id: person.to_string(),
        });
    }

    /// Checks every schedule: runs what's due (as its person), notes what
    /// was missed, and works out each next run. One check at a time.
    pub async fn check_schedules(self: &Arc<Self>) {
        let one = self.scheduler.tick.lock().await;
        let now = self.schedule_now();
        let mut changed = self.follow_time_zone(now);
        let due = match self.store.due_schedules(now) {
            Ok(due) => due,
            Err(err) => {
                tracing::warn!(%err, "couldn't read the schedules");
                return;
            }
        };
        let zone = Arc::clone(&self.config.zone);
        for s in due {
            changed = true;
            let Some(first) = s.next_run_ms else { continue };
            let Ok(rule) = Rule::parse(&s.rule) else {
                tracing::warn!(
                    schedule = s.id,
                    "a schedule's rule can't be read; pausing it"
                );
                let _ = self.store.set_schedule_paused(s.id, true, s.next_run_ms);
                continue;
            };
            let mut dues = vec![first];
            dues.extend(rule.runs_between(first, now, &*zone, 60));
            let latest = dues.last().copied().unwrap_or(first);
            // Late by at most two hours: it runs once now.
            let catch_up = now - latest <= CATCH_UP_MS;
            let missed = if catch_up {
                &dues[..dues.len() - 1]
            } else {
                &dues[..]
            };
            for at in missed {
                if let Err(err) = self.store.add_schedule_run(s.id, *at, None, "missed", None) {
                    tracing::warn!(schedule = s.id, %err, "couldn't note a missed run");
                }
            }
            if !missed.is_empty() {
                tracing::info!(
                    schedule = s.id,
                    missed = missed.len(),
                    "scheduled runs were missed"
                );
            }
            let next = rule.next_after(now.max(latest), &*zone);
            if let Err(err) = self
                .store
                .set_schedule_runs(s.id, next, catch_up.then_some(now))
            {
                tracing::warn!(schedule = s.id, %err, "couldn't move a schedule on");
                continue;
            }
            if catch_up {
                self.start_run(&s, &rule, latest, now);
            }
        }
        drop(one);
        if changed {
            self.announce_schedules();
        }
    }

    /// The home's time zone changed (the Mac moved): every schedule's
    /// times are the new zone's from now on. Returns whether any moved.
    fn follow_time_zone(&self, now: i64) -> bool {
        let tz = self.config.zone.name();
        let Ok(all) = self.store.schedules() else {
            return false;
        };
        let mut moved = false;
        for s in all.into_iter().filter(|s| s.tz != tz) {
            let next = Rule::parse(&s.rule)
                .ok()
                .and_then(|r| r.next_after(now, &*self.config.zone));
            // A one-time schedule that ran stays done.
            let next = if s.next_run_ms.is_none() { None } else { next };
            if self.store.set_schedule_tz(s.id, &tz, next).is_ok() {
                tracing::info!(
                    schedule = s.id,
                    from = s.tz,
                    to = tz,
                    "a schedule follows the Mac's time zone"
                );
                moved = true;
            }
        }
        moved
    }

    /// Starts schedule `s`'s run due at `due`, unless its bot is busy or
    /// too recently run, its person may no longer use it, or a limit is
    /// reached (each noted as the run's outcome).
    fn start_run(self: &Arc<Self>, s: &ScheduleRecord, rule: &Rule, due: i64, now: i64) {
        let skip = |outcome: &str| {
            if let Err(err) = self.store.add_schedule_run(s.id, due, None, outcome, None) {
                tracing::warn!(schedule = s.id, %err, "couldn't note a scheduled run");
            }
            tracing::info!(schedule = s.id, outcome, "skipped a scheduled run");
        };
        let Ok(bot) = self.bot(s.bot.as_str()) else {
            skip("no_access");
            return;
        };
        let person = self.person_viewer(Some(&s.person));
        if person.id.is_none() || self.bot_for(bot.id.as_str(), &person).is_err() {
            // They may no longer open the bot: it waits until someone sees to it.
            skip("no_access");
            let _ = self.store.set_schedule_paused(s.id, true, s.next_run_ms);
            let _ = self.updates.send(ServerMsg::Notice {
                message: format!(
                    "A schedule of yours is paused: you can't use {} any more.",
                    bot.name
                ),
                owner: Some(person.name.clone()).filter(|n| !n.is_empty()),
                owner_id: Some(s.person.to_string()),
            });
            return;
        }
        if lock(&self.scheduler.active).contains_key(&bot.id) {
            skip("busy");
            return;
        }
        if lock(&self.scheduler.last_start)
            .get(&bot.id)
            .is_some_and(|last| now - last < MIN_GAP_MS)
        {
            skip("too_soon");
            return;
        }
        if let Some(why) = self.run_limit(&bot.id, &s.person) {
            skip("limit");
            let line = format!("Skipped a scheduled run ({}): {why}", rule.describe());
            let _ = self.record(&bot.id, EventKind::Note { text: line });
            self.notify_person(
                &s.person,
                &bot.id,
                format!("{}: skipped a scheduled run ({why})", bot.name),
            );
            return;
        }
        lock(&self.scheduler.active).insert(
            bot.id.clone(),
            ActiveRun {
                person: s.person.clone(),
                run: None,
                seq: 0,
                done: None,
                told_waiting: false,
            },
        );
        lock(&self.scheduler.last_start).insert(bot.id.clone(), now);
        let hub = Arc::clone(self);
        let s = s.clone();
        tokio::spawn(async move { hub.run_scheduled(bot, s, due, now).await });
    }

    /// Gives `bot` schedule `s`'s task, as its person, and notes the run.
    async fn run_scheduled(self: Arc<Self>, bot: Bot, s: ScheduleRecord, due: i64, started: i64) {
        let name = self.person_name(&s.person);
        let origin = ChainOrigin::scheduled(s.person.clone(), s.id);
        let sent = self
            .send_for(
                bot.id.as_str(),
                &s.task,
                &[],
                name.as_deref(),
                &origin,
                false,
            )
            .await;
        let outcome = match sent {
            Ok(seq) => {
                let run =
                    self.store
                        .add_schedule_run(s.id, due, Some(started), "running", Some(seq));
                let finished = {
                    let mut active = lock(&self.scheduler.active);
                    match (active.get_mut(&bot.id), run) {
                        (Some(a), Ok(run)) => {
                            a.run = Some(run);
                            a.seq = seq;
                            // Its turn may be over already.
                            a.done.map(|ok| (ok, a.clone()))
                        }
                        (_, Err(err)) => {
                            tracing::warn!(schedule = s.id, %err, "couldn't note a scheduled run");
                            active.remove(&bot.id);
                            None
                        }
                        (None, _) => None,
                    }
                };
                if let Some((ok, run)) = finished {
                    lock(&self.scheduler.active).remove(&bot.id);
                    self.finish_run(&bot.id, &run, ok);
                }
                tracing::info!(schedule = s.id, bot = %bot.id, "started a scheduled run");
                self.announce_schedules();
                return;
            }
            Err(HubError::Busy(_)) => "busy",
            Err(HubError::EngineNotReady(why)) => {
                // Not set up yet (oxbot just started, say): tried again
                // at the next check, while it's still within the catch-up.
                tracing::info!(schedule = s.id, %why, "a scheduled run waits for the AI model");
                lock(&self.scheduler.active).remove(&bot.id);
                lock(&self.scheduler.last_start).remove(&bot.id);
                let _ = self.store.set_schedule_runs(s.id, Some(due), None);
                return;
            }
            Err(err) => {
                tracing::warn!(schedule = s.id, %err, "a scheduled run couldn't start");
                if let Some(person) = Some(&s.person) {
                    self.notify_person(
                        person,
                        &bot.id,
                        format!("{}: a scheduled run couldn't start ({err})", bot.name),
                    );
                }
                "failed"
            }
        };
        lock(&self.scheduler.active).remove(&bot.id);
        if outcome == "busy" {
            lock(&self.scheduler.last_start).remove(&bot.id);
        }
        let _ = self.store.add_schedule_run(s.id, due, None, outcome, None);
        self.announce_schedules();
    }

    /// `bot`'s turn ended: if it was a scheduled run, notes how it went and
    /// tells its person.
    pub(super) fn scheduled_turn_done(&self, bot: &BotId, ok: bool) {
        let finished = {
            let mut active = lock(&self.scheduler.active);
            match active.get_mut(bot) {
                // Not written down yet: its run reads this when it is.
                Some(a) if a.run.is_none() => {
                    a.done = Some(ok);
                    None
                }
                Some(_) => active.remove(bot),
                None => None,
            }
        };
        if let Some(run) = finished {
            self.finish_run(bot, &run, ok);
            self.announce_schedules();
        }
    }

    /// Notes `run`'s end and sends its person the answer's first line.
    fn finish_run(&self, bot: &BotId, run: &ActiveRun, ok: bool) {
        if let Some(id) = run.run
            && let Err(err) = self.store.end_schedule_run(
                id,
                if ok { "ok" } else { "failed" },
                self.schedule_now(),
            )
        {
            tracing::warn!(%bot, %err, "couldn't note a scheduled run's end");
        }
        let name = self.bot_name(bot);
        let text = match self.latest_reply(bot, run.seq) {
            Some(reply) if !first_line(&reply).is_empty() => {
                format!("{name}: {}", first_line(&reply))
            }
            _ if ok => format!("{name}: finished its scheduled task"),
            _ => format!("{name}: its scheduled task stopped; open its chat to see why"),
        };
        self.notify_person(&run.person, bot, text);
    }

    /// A scheduled run of `bot` is waiting (`what`, e.g. "is waiting for
    /// your OK"): its person hears it once.
    pub(super) fn scheduled_run_waiting(&self, bot: &BotId, what: &str) {
        let person = {
            let mut active = lock(&self.scheduler.active);
            match active.get_mut(bot) {
                Some(a) if !a.told_waiting => {
                    a.told_waiting = true;
                    Some(a.person.clone())
                }
                _ => None,
            }
        };
        if let Some(person) = person {
            let text = format!("{} needs you: it {what}", self.bot_name(bot));
            self.notify_person_as(&person, bot, text, true);
        }
    }

    /// Forgets `bot`'s run in progress (the bot is being deleted).
    pub(super) fn forget_scheduled_run(&self, bot: &BotId) {
        lock(&self.scheduler.active).remove(bot);
        lock(&self.scheduler.last_start).remove(bot);
    }
}

/// The schedule tools, run for a bot's turn.
impl Hub {
    /// What a schedule tool's turn has read that oxbot doesn't trust (or
    /// a task passed on after such text), and whether it's a scheduled run.
    async fn provenance(&self, bot: &BotId, origin: &ChainOrigin) -> Option<String> {
        let read = self
            .untrusted(bot)
            .await
            .or_else(|| origin.tainted_by.clone());
        match (origin.entry == Entry::Schedule, read) {
            (true, Some(why)) => Some(format!("a scheduled run, after reading {why}")),
            (true, None) => Some("a scheduled run, not a message of theirs".to_owned()),
            (false, Some(why)) => Some(format!("after reading {why}")),
            (false, None) => None,
        }
    }

    /// `schedule_task`: shows the person the turn acts for a schedule card
    /// and makes the schedule only if they press Create (as they changed
    /// it, if they did).
    pub(super) async fn schedule_task(
        self: &Arc<Self>,
        bot: &BotId,
        asker: &Viewer,
        origin: &ChainOrigin,
        args: &Value,
    ) -> (bool, String) {
        let proposal = match self.propose_schedule(bot, asker, args) {
            Ok(p) => p,
            Err(why) => return (false, why),
        };
        let zone = Arc::clone(&self.config.zone);
        let draft = ScheduleDraft {
            text: schedule::card_text(&proposal.rule, &proposal.task),
            task: proposal.task.clone(),
            rule: proposal.rule.to_wire(),
            tz: zone.name(),
            bot_name: proposal.target.name.clone(),
            from: self.provenance(bot, origin).await,
        };
        let ask = policy::Ask {
            detail: Some(format!(
                "First run: {}",
                schedule::when(proposal.next, &*zone)
            )),
            private: proposal.target.private,
            schedule: Some(Box::new(draft.clone())),
            ..policy::Ask::new(
                ApprovalKind::Schedule,
                "set up a schedule",
                draft.text,
                "oxbot",
            )
        };
        let (outcome, card) = match self.confirm_card(&proposal.me, &ask, None).await {
            Ok(answer) => answer,
            Err(err) => return (false, format!("Couldn't ask the person: {err}")),
        };
        let edit = lock(&self.scheduler.edits).remove(&card);
        match outcome {
            Outcome::Allowed | Outcome::AllowedAlways => self.save_schedule(&proposal, edit),
            Outcome::Denied => (
                false,
                "The person said no to this schedule. Don't try another way; tell them, and stop."
                    .to_owned(),
            ),
            Outcome::TimedOut => (
                false,
                "The person didn't answer the schedule card in time, so it wasn't made. Tell them, \
                 and stop."
                    .to_owned(),
            ),
        }
    }

    /// Checks a `schedule_task` call: who it's for, the bot it runs in, its
    /// rule and task, its first run, and the person's limit.
    fn propose_schedule(
        &self,
        bot: &BotId,
        asker: &Viewer,
        args: &Value,
    ) -> Result<Proposal, String> {
        let person = asker.id.clone().ok_or_else(|| {
            "A schedule needs to know who it's for; ask the person to send their request again."
                .to_owned()
        })?;
        let me = self
            .bot(bot.as_str())
            .map_err(|_| "Not scheduled: oxbot lost track of this bot.".to_owned())?;
        // The Assistant may name another of the person's bots.
        let target = match tools::text_arg(args, "bot").trim() {
            "" => me.clone(),
            id if id == me.id.as_str() => me.clone(),
            id if me.is_central() => match self.bot_for(id, asker) {
                Ok(b) if !b.is_central() => b,
                _ => {
                    return Err(format!(
                        "There's no bot with id {id} they can use. Use list_bots."
                    ));
                }
            },
            _ => return Err("You can only set up schedules for yourself.".to_owned()),
        };
        let (rule, _) = match args.get("rule").map(Rule::from_tool) {
            Some(Ok(parsed)) => parsed,
            Some(Err(why)) => return Err(format!("Not scheduled: {why}")),
            None => return Err("Not scheduled: schedule_task needs a rule.".to_owned()),
        };
        let task = schedule::clean_task(tools::text_arg(args, "task"))
            .map_err(|why| format!("Not scheduled: {why}"))?;
        let next = rule
            .next_after(self.schedule_now(), &*self.config.zone)
            .ok_or_else(|| {
                "Not scheduled: that time has passed. Ask the person for a later one.".to_owned()
            })?;
        if self.schedules_of(&person) >= MAX_PER_PERSON {
            return Err(too_many());
        }
        Ok(Proposal {
            person,
            me,
            target,
            rule,
            task,
            next,
        })
    }

    /// Saves a schedule the person said yes to, as they changed it on the
    /// card if they did (checked when they answered).
    fn save_schedule(&self, proposal: &Proposal, edit: Option<ScheduleEdit>) -> (bool, String) {
        let zone = &*self.config.zone;
        let changed = edit.is_some();
        let (task, rule) = match edit {
            Some(edit) => match Rule::parse(&edit.rule) {
                Ok(rule) => (edit.task, rule),
                Err(why) => return (false, format!("Not scheduled: {why}")),
            },
            None => (proposal.task.clone(), proposal.rule),
        };
        let Some(next) = rule.next_after(self.schedule_now(), zone) else {
            return (false, "Not scheduled: that time has passed.".to_owned());
        };
        let new = NewSchedule {
            bot: proposal.target.id.clone(),
            person: proposal.person.clone(),
            task: task.clone(),
            rule: rule.to_wire(),
            tz: zone.name(),
            next_run_ms: Some(next),
        };
        match self.store.add_schedule(&new, MAX_PER_PERSON) {
            Ok(Some(made)) => {
                tracing::info!(schedule = made.id, bot = %new.bot, person = %new.person, "made a schedule");
                self.announce_schedules();
                let how = if changed {
                    " (the person changed it on the card)"
                } else {
                    ""
                };
                (
                    true,
                    format!(
                        "Scheduled{how}, id {}: {} First run: {}. Tell the person in a line.",
                        made.id,
                        sentence(&schedule::card_text(&rule, &task)),
                        schedule::when(next, zone)
                    ),
                )
            }
            Ok(None) => (false, too_many()),
            Err(err) => (false, format!("Couldn't save the schedule: {err}")),
        }
    }

    /// How many schedules `person` has.
    fn schedules_of(&self, person: &PersonId) -> usize {
        self.store
            .schedules()
            .map_or(0, |all| all.iter().filter(|s| &s.person == person).count())
    }

    /// The schedule `id`, if it's `asker`'s and this bot's to touch (the
    /// Assistant: any of theirs; another bot: theirs in it).
    fn own_schedule(
        &self,
        bot: &BotId,
        asker: &Viewer,
        id: Option<i64>,
    ) -> Result<ScheduleRecord, String> {
        let central = self.bot(bot.as_str()).is_ok_and(|b| b.is_central());
        let missing = || match id {
            Some(id) => {
                format!("There's no schedule with id {id} for them here. Use list_schedules.")
            }
            None => "Which schedule? Give its id, from list_schedules.".to_owned(),
        };
        let s = id
            .and_then(|id| self.store.schedule(id).ok().flatten())
            .ok_or_else(missing)?;
        let theirs = asker.id.as_ref() == Some(&s.person) && (central || &s.bot == bot);
        if theirs { Ok(s) } else { Err(missing()) }
    }

    /// `list_schedules`: the asker's schedules here, their tasks as data.
    pub(super) fn list_schedules_tool(&self, bot: &BotId, asker: &Viewer) -> (bool, String) {
        let central = self.bot(bot.as_str()).is_ok_and(|b| b.is_central());
        let Ok(all) = self.store.schedules() else {
            return (false, "Couldn't read the schedules.".to_owned());
        };
        let last = self.store.latest_schedule_runs().unwrap_or_default();
        let zone = &*self.config.zone;
        let mut out = String::new();
        for s in all
            .iter()
            .filter(|s| asker.id.as_ref() == Some(&s.person) && (central || &s.bot == bot))
        {
            let rule = Rule::parse(&s.rule).map_or_else(|_| "?".to_owned(), |r| r.describe());
            let _ = write!(out, "- id {}", s.id);
            if central {
                let _ = write!(out, " (in {}, id {})", self.bot_name(&s.bot), s.bot);
            }
            let _ = write!(out, ": {rule}. ");
            if s.paused {
                out.push_str("Paused. ");
            } else if let Some(next) = s.next_run_ms {
                let _ = write!(out, "Next: {}. ", schedule::when(next, zone));
            } else {
                out.push_str("Done (it ran once). ");
            }
            if let Some(run) = last.get(&s.id) {
                let _ = write!(out, "Last: {}. ", run.outcome);
            }
            let _ = writeln!(out, "{}", data_block("schedule_task", "", &s.task));
        }
        if out.is_empty() {
            out.push_str("They have no schedules here yet.");
        }
        (true, out)
    }

    /// `pause_schedule`: pausing runs at once; resuming after reading
    /// something oxbot doesn't trust asks the person first.
    pub(super) async fn pause_schedule_tool(
        self: &Arc<Self>,
        bot: &BotId,
        asker: &Viewer,
        origin: &ChainOrigin,
        args: &Value,
    ) -> (bool, String) {
        let s = match self.own_schedule(bot, asker, args.get("id").and_then(Value::as_i64)) {
            Ok(s) => s,
            Err(why) => return (false, why),
        };
        let paused = args.get("paused").and_then(Value::as_bool).unwrap_or(true);
        if !paused
            && s.paused
            && let Some(from) = self.provenance(bot, origin).await
            && let Some(no) = self
                .ask_schedule_change(bot, &s, "resume the schedule", from)
                .await
        {
            return (false, no);
        }
        match self.set_paused(&s, paused) {
            Ok(()) if paused => (true, format!("Paused schedule {}.", s.id)),
            Ok(()) => (true, format!("Resumed schedule {}.", s.id)),
            Err(err) => (false, format!("Couldn't change it: {err}")),
        }
    }

    /// `delete_schedule`: after the person's OK on a card.
    pub(super) async fn delete_schedule_tool(
        self: &Arc<Self>,
        bot: &BotId,
        asker: &Viewer,
        origin: &ChainOrigin,
        args: &Value,
    ) -> (bool, String) {
        let s = match self.own_schedule(bot, asker, args.get("id").and_then(Value::as_i64)) {
            Ok(s) => s,
            Err(why) => return (false, why),
        };
        let from = self.provenance(bot, origin).await.unwrap_or_default();
        if let Some(no) = self
            .ask_schedule_change(bot, &s, "delete the schedule", from)
            .await
        {
            return (false, no);
        }
        match self.store.delete_schedule(s.id) {
            Ok(_) => {
                self.announce_schedules();
                (true, format!("Deleted schedule {}.", s.id))
            }
            Err(err) => (false, format!("Couldn't delete it: {err}")),
        }
    }

    /// Asks the person on a card before `action` (e.g. `delete the
    /// schedule`) on `s`; `from` says what the turn had read, if anything.
    /// `None` if they said yes, else what to tell the bot.
    async fn ask_schedule_change(
        &self,
        bot: &BotId,
        s: &ScheduleRecord,
        action: &str,
        from: String,
    ) -> Option<String> {
        let me = self.bot(bot.as_str()).ok()?;
        let rule = Rule::parse(&s.rule).ok()?;
        let draft = ScheduleDraft {
            text: schedule::card_text(&rule, &s.task),
            task: s.task.clone(),
            rule: s.rule.clone(),
            tz: s.tz.clone(),
            bot_name: self.bot_name(&s.bot),
            from: Some(from).filter(|f| !f.is_empty()),
        };
        let ask = policy::Ask {
            schedule: Some(Box::new(draft.clone())),
            ..policy::Ask::new(ApprovalKind::ChangeSchedule, action, draft.text, "oxbot")
        };
        match self.confirm(&me, &ask, None).await {
            Ok(Outcome::Allowed | Outcome::AllowedAlways) => None,
            Ok(Outcome::Denied) => Some(format!(
                "The person said no to: {action}. Don't try another way; tell them, and stop."
            )),
            Ok(Outcome::TimedOut) => Some(format!(
                "The person didn't answer in time, so oxbot didn't {action}. Tell them, and stop."
            )),
            Err(err) => Some(format!("Couldn't ask the person: {err}")),
        }
    }

    /// Pauses `s`, or resumes it from now (never catching up).
    fn set_paused(&self, s: &ScheduleRecord, paused: bool) -> Result<(), HubError> {
        let next = if paused {
            s.next_run_ms
        } else {
            Rule::parse(&s.rule)
                .ok()
                .and_then(|r| r.next_after(self.schedule_now(), &*self.config.zone))
        };
        self.store.set_schedule_paused(s.id, paused, next)?;
        tracing::info!(schedule = s.id, paused, "changed whether a schedule runs");
        self.announce_schedules();
        Ok(())
    }
}

/// Who may see, pause and delete schedules, for Settings and bot
/// settings.
impl Hub {
    /// Whether `viewer` (resolved) may see `s`: the owner sees every one;
    /// anyone else their own and those in bots they may open.
    fn sees_schedule(&self, s: &ScheduleRecord, viewer: &Viewer) -> bool {
        self.is_farm_side(viewer)
            || viewer.id.as_ref() == Some(&s.person)
            || self
                .bot(s.bot.as_str())
                .is_ok_and(|b| self.opens(&b, viewer))
    }

    /// Whether `viewer` (resolved) may pause or delete `s`: its person,
    /// the Farm, or whoever may manage its bot.
    fn changes_schedule(&self, s: &ScheduleRecord, viewer: &Viewer) -> bool {
        viewer.farm
            || viewer.id.as_ref() == Some(&s.person)
            || self
                .bot(s.bot.as_str())
                .is_ok_and(|b| !b.is_central() && self.opens(&b, viewer) && b.manageable_by(viewer))
    }

    /// The schedules `viewer` may see: one bot's (`bot`, which they must
    /// be able to open), or all of them, soonest first.
    ///
    /// # Errors
    /// The bot is unknown or hidden from them, or the database fails.
    pub fn schedules_for(
        &self,
        viewer: &Viewer,
        bot: Option<&str>,
    ) -> Result<Vec<ScheduleInfo>, HubError> {
        let viewer = &*self.resolve(viewer);
        let only = match bot {
            Some(id) => Some(self.bot_for(id, viewer)?.id),
            None => None,
        };
        let names = self.person_names();
        let last = self.store.latest_schedule_runs()?;
        Ok(self
            .store
            .schedules()?
            .into_iter()
            .filter(|s| only.as_ref().is_none_or(|b| &s.bot == b))
            .filter(|s| self.sees_schedule(s, viewer))
            .map(|s| {
                let text = Rule::parse(&s.rule)
                    .map_or_else(|_| s.task.clone(), |r| schedule::card_text(&r, &s.task));
                ScheduleInfo {
                    can_change: self.changes_schedule(&s, viewer),
                    mine: viewer.id.as_ref() == Some(&s.person),
                    bot_name: self.bot_name(&s.bot),
                    person: names.get(&s.person).cloned().unwrap_or_default(),
                    last_outcome: last.get(&s.id).map(|r| r.outcome.clone()),
                    last_run_ms: last.get(&s.id).map(|r| r.started_ms.unwrap_or(r.due_ms)),
                    id: s.id,
                    bot: s.bot.to_string(),
                    task: s.task,
                    rule: s.rule,
                    text,
                    tz: s.tz,
                    next_run_ms: s.next_run_ms,
                    paused: s.paused,
                    created_ms: s.created_ms,
                    person_id: s.person.to_string(),
                }
            })
            .collect())
    }

    /// The schedule `id`, if `viewer` may change it.
    fn schedule_to_change(&self, id: i64, viewer: &Viewer) -> Result<ScheduleRecord, HubError> {
        let s = self
            .store
            .schedule(id)?
            .filter(|s| self.sees_schedule(s, viewer))
            .ok_or_else(|| HubError::NotAllowed("There's no such schedule.".to_owned()))?;
        if !self.changes_schedule(&s, viewer) {
            return Err(HubError::NotAllowed(
                "Only the person who made a schedule, the bot's maker or the owner can change it."
                    .to_owned(),
            ));
        }
        Ok(s)
    }

    /// Pauses or resumes schedule `id` for `viewer`.
    ///
    /// # Errors
    /// They may not, or the database fails.
    pub fn pause_schedule_as(
        &self,
        id: i64,
        paused: bool,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let s = self.schedule_to_change(id, viewer)?;
        self.set_paused(&s, paused)
    }

    /// Deletes schedule `id` for `viewer`.
    ///
    /// # Errors
    /// They may not, or the database fails.
    pub fn delete_schedule_as(&self, id: i64, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let s = self.schedule_to_change(id, viewer)?;
        self.store.delete_schedule(s.id)?;
        tracing::info!(schedule = s.id, "deleted a schedule");
        self.announce_schedules();
        Ok(())
    }
}

/// A schedule a bot proposes, checked.
struct Proposal {
    /// Who it runs as.
    person: PersonId,
    /// The bot whose turn proposes it (its card shows in that chat).
    me: Bot,
    /// The bot it runs in.
    target: Bot,
    rule: Rule,
    task: String,
    /// Its first run.
    next: i64,
}

/// `text`, ending as a sentence does.
fn sentence(text: &str) -> String {
    if text.ends_with(['.', '!', '?', '…']) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

/// What a bot hears when its person has as many schedules as oxbot keeps.
fn too_many() -> String {
    format!(
        "Not scheduled: they have {MAX_PER_PERSON} schedules, the most oxbot keeps for one person. Ask \
         them to delete one first (bot settings → Schedules)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HubConfig;
    use crate::schedule::zones::Fixed;
    use crate::store::Store;
    use oxbot_protocol::ScheduleRule;

    /// A hub in UTC with `recipes` (Jay's, shared) and `bank` (Sam's,
    /// private); Jay is the Farm's person. No engine: a run that starts
    /// waits for one.
    async fn hub(dir: &std::path::Path) -> (Arc<Hub>, PersonId, PersonId) {
        let hub = Hub::new(
            vec![],
            Store::open_in_memory().unwrap(),
            HubConfig {
                zone: Arc::new(Fixed("UTC", 0)),
                ..HubConfig::new(dir)
            },
        );
        let jay = hub.farm_person("Jay").unwrap().id;
        hub.create_bot_with("Recipes", "", Some("Jay"), false)
            .await
            .unwrap();
        hub.create_bot_with("Bank", "", Some("Sam"), true)
            .await
            .unwrap();
        let sam = hub.store.person_by_name("Sam").unwrap().unwrap().id;
        // Wednesday, November 4, 2026 at noon (UTC), give or take the
        // test's own milliseconds.
        let noon = chrono::DateTime::parse_from_rfc3339("2026-11-04T12:00:00Z")
            .unwrap()
            .timestamp_millis();
        hub.scheduler
            .offset_ms
            .store(noon - now_ms(), Ordering::Relaxed);
        (hub, jay, sam)
    }

    fn add(hub: &Hub, bot: &str, person: &PersonId, next: i64) -> ScheduleRecord {
        hub.store
            .add_schedule(
                &NewSchedule {
                    bot: BotId::new(bot).unwrap(),
                    person: person.clone(),
                    task: "today's prep plan".into(),
                    rule: ScheduleRule::Daily {
                        time: "08:15".into(),
                    },
                    tz: "UTC".into(),
                    next_run_ms: Some(next),
                },
                MAX_PER_PERSON,
            )
            .unwrap()
            .unwrap()
    }

    fn outcomes(hub: &Hub, id: i64) -> Vec<String> {
        hub.store
            .schedule_runs(id)
            .unwrap()
            .into_iter()
            .map(|r| r.outcome)
            .collect()
    }

    #[tokio::test]
    async fn a_run_missed_by_more_than_two_hours_is_noted_and_the_next_one_set() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, _) = hub(tmp.path()).await;
        let now = hub.schedule_now();
        let late = add(&hub, "recipes", &jay, now - 3 * 3_600_000);
        hub.check_schedules().await;
        assert_eq!(outcomes(&hub, late.id), ["missed"]);
        let after = hub.store.schedule(late.id).unwrap().unwrap();
        assert!(after.next_run_ms.unwrap() > now, "on to the next 8:15");
        // Days off: each missed run is noted once.
        // Due Sunday 9:00; then Monday's, Tuesday's and today's 8:15 (all
        // over two hours ago at noon).
        let days = add(&hub, "recipes", &jay, now - 3 * 86_400_000 - 3 * 3_600_000);
        hub.check_schedules().await;
        assert_eq!(outcomes(&hub, days.id), ["missed"; 4]);
        let next = hub
            .store
            .schedule(days.id)
            .unwrap()
            .unwrap()
            .next_run_ms
            .unwrap();
        assert_eq!(
            crate::schedule::when(next, &Fixed("UTC", 0)),
            "Thu, Nov 5 at 8:15 AM"
        );
    }

    #[tokio::test]
    async fn a_run_missed_by_less_waits_for_the_ai_model_and_runs_once() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, _) = hub(tmp.path()).await;
        let now = hub.schedule_now();
        let due = now - 3_600_000;
        let s = add(&hub, "recipes", &jay, due);
        hub.check_schedules().await;
        // No AI model here: it waits, still due, and nothing is noted yet.
        for _ in 0..50 {
            if hub.store.schedule(s.id).unwrap().unwrap().next_run_ms == Some(due) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            hub.store.schedule(s.id).unwrap().unwrap().next_run_ms,
            Some(due)
        );
        assert_eq!(outcomes(&hub, s.id), Vec::<String>::new());
        assert!(!lock(&hub.scheduler.active).contains_key(&s.bot));
    }

    #[tokio::test]
    async fn one_run_at_a_time_per_bot_and_not_too_often() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, _) = hub(tmp.path()).await;
        let now = hub.schedule_now();
        let bot = BotId::new("recipes").unwrap();
        lock(&hub.scheduler.active).insert(
            bot.clone(),
            ActiveRun {
                person: jay.clone(),
                run: Some(1),
                seq: 1,
                done: None,
                told_waiting: false,
            },
        );
        let busy = add(&hub, "recipes", &jay, now - 60_000);
        hub.check_schedules().await;
        assert_eq!(outcomes(&hub, busy.id), ["busy"]);
        lock(&hub.scheduler.active).clear();
        lock(&hub.scheduler.last_start).insert(bot, now - 5 * 60_000);
        let soon = add(&hub, "recipes", &jay, now - 60_000);
        hub.check_schedules().await;
        assert_eq!(outcomes(&hub, soon.id), ["too_soon"]);
    }

    #[tokio::test]
    async fn a_run_over_a_limit_is_skipped_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, _) = hub(tmp.path()).await;
        let bot = BotId::new("recipes").unwrap();
        hub.update_bot(&bot, |b| b.loop_settings.daily_requests = Some(0))
            .unwrap();
        let mut updates = hub.subscribe();
        let s = add(&hub, "recipes", &jay, hub.schedule_now() - 60_000);
        hub.check_schedules().await;
        assert_eq!(outcomes(&hub, s.id), ["limit"]);
        let events = hub.store.history(&bot, 0).unwrap();
        assert!(events.iter().any(|e| matches!(&e.kind, EventKind::Note { text } if text.starts_with("Skipped a scheduled run (Every day at 8:15 AM)"))));
        let mut told = false;
        while let Ok(msg) = updates.try_recv() {
            if let ServerMsg::Notify {
                text, person_id, ..
            } = msg
            {
                told |= person_id == jay.as_str() && text.starts_with("Recipes: skipped");
            }
        }
        assert!(told, "Jay's devices hear it");
    }

    #[tokio::test]
    async fn a_run_for_someone_who_lost_the_bot_pauses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, sam) = hub(tmp.path()).await;
        // Jay's schedule in Sam's private bot (it was shared once).
        let s = add(&hub, "bank", &jay, hub.schedule_now() - 60_000);
        hub.check_schedules().await;
        assert_eq!(outcomes(&hub, s.id), ["no_access"]);
        assert!(hub.store.schedule(s.id).unwrap().unwrap().paused);
        // Sam's own runs on.
        let own = add(&hub, "bank", &sam, hub.schedule_now() + 60_000);
        assert!(!hub.store.schedule(own.id).unwrap().unwrap().paused);
    }

    #[tokio::test]
    async fn who_sees_pauses_and_deletes_which_schedule() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, sam) = hub(tmp.path()).await;
        let ann = hub.person_named("Ann").unwrap().id;
        let later = hub.schedule_now() + 3_600_000;
        let jays = add(&hub, "recipes", &jay, later);
        let sams = add(&hub, "bank", &sam, later);
        let anns = add(&hub, "recipes", &ann, later);
        let ids = |v: &Viewer| -> Vec<i64> {
            hub.schedules_for(v, None)
                .unwrap()
                .into_iter()
                .map(|s| s.id)
                .collect()
        };
        let farm = hub.farm_viewer("Jay");
        let ann_v = Viewer::person(ann.clone(), "Ann", false);
        let sam_v = Viewer::person(sam.clone(), "Sam", false);
        // The owner sees all; Ann her own and the shared bot's; Sam his
        // own and the shared bot's.
        assert_eq!(ids(&farm), [jays.id, sams.id, anns.id]);
        assert_eq!(ids(&ann_v), [jays.id, anns.id]);
        assert_eq!(ids(&sam_v), [jays.id, sams.id, anns.id]);
        assert!(
            hub.schedules_for(&ann_v, Some("bank")).is_err(),
            "Sam's private bot"
        );
        let list = hub.schedules_for(&ann_v, Some("recipes")).unwrap();
        let theirs = list.iter().find(|s| s.id == anns.id).unwrap();
        assert!(theirs.mine && theirs.can_change && theirs.person == "Ann");
        assert!(!list.iter().find(|s| s.id == jays.id).unwrap().can_change);
        assert_eq!(theirs.text, "Every day at 8:15 AM: today's prep plan");
        // Ann can't pause Jay's; Jay (the bot's maker) can pause Ann's.
        assert!(hub.pause_schedule_as(jays.id, true, &ann_v).is_err());
        assert!(
            hub.delete_schedule_as(sams.id, &ann_v).is_err(),
            "can't even see it"
        );
        let jay_phone = Viewer::person(jay.clone(), "Jay", false);
        hub.pause_schedule_as(anns.id, true, &jay_phone).unwrap();
        assert!(hub.store.schedule(anns.id).unwrap().unwrap().paused);
        // Resuming starts from now: no catching up.
        hub.pause_schedule_as(anns.id, false, &ann_v).unwrap();
        let back = hub.store.schedule(anns.id).unwrap().unwrap();
        assert!(!back.paused && back.next_run_ms.unwrap() > hub.schedule_now());
        hub.delete_schedule_as(anns.id, &ann_v).unwrap();
        hub.delete_schedule_as(sams.id, &farm).unwrap();
        assert_eq!(ids(&farm), [jays.id]);
        // Deleting the bot deletes its schedules.
        hub.delete_bot("recipes", &farm).await.unwrap();
        assert_eq!(ids(&farm), Vec::<i64>::new());
    }

    #[tokio::test]
    async fn the_clock_moves_only_in_tests_and_a_new_zone_moves_schedules() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, _) = hub(tmp.path()).await;
        let s = add(&hub, "recipes", &jay, hub.schedule_now() + 10 * 3_600_000);
        let before = hub.schedule_now();
        hub.advance_test_clock(Some(90)).await.unwrap();
        assert!(hub.schedule_now() - before >= 90 * 60_000);
        // A schedule kept in another zone follows the Mac's (UTC here).
        hub.store
            .set_schedule_tz(s.id, "Asia/Tokyo", s.next_run_ms)
            .unwrap();
        hub.check_schedules().await;
        assert_eq!(hub.store.schedule(s.id).unwrap().unwrap().tz, "UTC");
    }

    #[tokio::test]
    async fn a_finished_run_tells_its_person_its_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, _) = hub(tmp.path()).await;
        let bot = BotId::new("recipes").unwrap();
        let s = add(&hub, "recipes", &jay, hub.schedule_now() + 3_600_000);
        let run = hub
            .store
            .add_schedule_run(s.id, 1, Some(1), "running", Some(1))
            .unwrap();
        lock(&hub.scheduler.active).insert(
            bot.clone(),
            ActiveRun {
                person: jay.clone(),
                run: Some(run),
                seq: 0,
                done: None,
                told_waiting: false,
            },
        );
        let mut updates = hub.subscribe();
        hub.scheduled_run_waiting(&bot, "is waiting for your OK");
        hub.scheduled_run_waiting(&bot, "is waiting for your OK");
        hub.record(
            &bot,
            EventKind::AssistantMessage {
                text: "## Day 3 plan is ready.
Details"
                    .into(),
            },
        )
        .unwrap();
        hub.record(
            &bot,
            EventKind::TurnDone {
                ok: true,
                duration_ms: 1,
            },
        )
        .unwrap();
        let mut lines = Vec::new();
        while let Ok(msg) = updates.try_recv() {
            if let ServerMsg::Notify {
                text, person_id, ..
            } = msg
            {
                assert_eq!(person_id, jay.as_str());
                lines.push(text);
            }
        }
        assert_eq!(
            lines,
            [
                "Recipes needs you: it is waiting for your OK",
                "Recipes: Day 3 plan is ready."
            ],
            "told once that it waits, then the answer"
        );
        assert_eq!(outcomes(&hub, s.id), ["ok"]);
    }

    #[test]
    fn a_line_ends_once() {
        assert_eq!(sentence("plan."), "plan.");
        assert_eq!(sentence("plan"), "plan.");
    }

    #[test]
    fn a_notification_takes_the_replys_first_plain_line() {
        assert_eq!(
            first_line("\n## Day 3 plan is ready.\nMore"),
            "Day 3 plan is ready."
        );
        assert_eq!(first_line("**Done:** two things"), "Done: two things");
        assert_eq!(first_line("   \n"), "");
        assert!(first_line(&"x".repeat(300)).chars().count() <= 120);
    }

    #[test]
    fn a_changed_schedule_is_checked() {
        let ok = checked_edit(&ScheduleEdit {
            task: " plan ".into(),
            rule: oxbot_protocol::ScheduleRule::Daily {
                time: "8:05".into(),
            },
        })
        .unwrap();
        assert_eq!(ok.task, "plan");
        assert_eq!(
            ok.rule,
            oxbot_protocol::ScheduleRule::Daily {
                time: "08:05".into()
            }
        );
        assert!(
            checked_edit(&ScheduleEdit {
                task: "x".into(),
                rule: oxbot_protocol::ScheduleRule::Daily {
                    time: "99:00".into()
                },
            })
            .is_err()
        );
        assert!(
            checked_edit(&ScheduleEdit {
                task: " ".into(),
                rule: oxbot_protocol::ScheduleRule::Daily {
                    time: "08:00".into()
                },
            })
            .is_err()
        );
    }
}
