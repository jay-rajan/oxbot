//! Plans and the home area: a bot understands the task before it browses.
//!
//! - `plan_task`: before its first web visit, a bot shows the person a
//!   card with the site, the place and its plan, and starts at once: the
//!   card is a notice, not a question. Stop on it ends browsing for the
//!   task (the bot is told when it next browses); a reply in the chat
//!   changes the plan (the card closes and the bot reads the message).
//!   The plan holds for the same person's follow-up messages while they
//!   come within [`PLAN_LASTS`] of each other. With
//!   [`HubConfig::plan_first`] on, the gate refuses browser tools until a
//!   plan was shown. A schedule's own run needs no plan: its card was the
//!   person's yes, and nobody is there to answer.
//! - `save_home_area`: a place the person named becomes the home's area,
//!   which every bot gets with each task (see `Hub::about_line`). The area
//!   is the household's, so only the owner saves it, on a card.
//!
//! [`HubConfig::plan_first`]: super::HubConfig::plan_first

use std::sync::Arc;
use std::time::Duration;

use oxbot_protocol::{Answer, ApprovalKind, EventKind, Outcome};
use tokio::sync::oneshot;

use super::engines::Entry;
use super::profiles::{PREF_HOME_AREA, PREF_HOME_AREA_USE};
use super::{Hub, lock};
use crate::bot::Viewer;
use crate::engine::{Decision, GateMode};
use crate::id::BotId;
use crate::policy;

/// The longest site, place or plan on a card, in characters.
const MAX_CARD_TEXT: usize = 400;
/// The longest home area, in characters (as Settings allows).
const MAX_AREA: usize = 100;
/// How long a plan holds between one message and the next.
pub(super) const PLAN_LASTS: Duration = Duration::from_mins(30);

/// What a bot is told when it browses before showing a plan.
pub(super) const PLAN_FIRST: &str = "Not yet: before your first web visit in a task, make sure you \
     know what it needs (the place, dates, who it's for; ask the person one short question if \
     something is missing), then call plan_task with the site, the place and your plan.";

/// What a bot is told once the person pressed Stop on its plan.
pub(super) const PLAN_STOPPED: &str = "The person pressed Stop on your plan. Don't browse; ask \
     them in a line what they'd like instead, and stop.";

/// What `plan_task` tells the bot once the card is up.
const PLAN_SHOWN: &str = "The person can see your plan. Go ahead now, as planned; don't wait for \
     an answer. If they write with changes, follow them.";

fn card_text(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(MAX_CARD_TEXT).collect()
}

impl Hub {
    /// Why `bot` may not browse yet, if it may not: a live browser call
    /// (`browses`, in `mode`), plans are on, it isn't a schedule's own
    /// run, and the person pressed Stop or no plan was shown.
    pub(super) async fn plan_first_refusal(
        &self,
        bot: &BotId,
        browses: bool,
        mode: GateMode,
    ) -> Option<Decision> {
        if !browses || mode != GateMode::Live || !self.config.plan_first {
            return None;
        }
        let runtimes = self.runtimes.lock().await;
        let runtime = runtimes.get(bot)?;
        if runtime.plan_stopped {
            return Some(Decision::Deny(PLAN_STOPPED.to_owned()));
        }
        if runtime.plan_ok || runtime.origin.entry == Entry::Schedule {
            return None;
        }
        Some(Decision::Deny(PLAN_FIRST.to_owned()))
    }

    /// `plan_task`: shows the person the plan, and lets the bot start.
    pub(super) async fn plan_task(
        self: &Arc<Self>,
        bot_id: &BotId,
        site: &str,
        place: &str,
        plan: &str,
    ) -> (bool, String) {
        let Ok(bot) = self.bot(bot_id.as_str()) else {
            return (false, "oxbot lost track of this bot.".to_owned());
        };
        {
            let mut runtimes = self.runtimes.lock().await;
            let Some(runtime) = runtimes.get_mut(bot_id) else {
                return (false, "oxbot lost track of this bot.".to_owned());
            };
            if runtime.plan_stopped {
                return (false, PLAN_STOPPED.to_owned());
            }
            runtime.plan_ok = true;
        }
        let place = card_text(place);
        let ask = policy::Ask {
            detail: (!place.is_empty()).then_some(place),
            ..policy::Ask::new(
                ApprovalKind::Plan,
                "start",
                card_text(plan),
                card_text(site),
            )
        };
        let (id, rx) = self.open_card(&bot, &ask, None).await;
        tracing::info!(bot = %bot_id, "showed its plan");
        let hub = Arc::clone(self);
        let bot_id = bot_id.clone();
        tokio::spawn(async move { hub.watch_plan(&bot_id, id, rx, ask).await });
        (true, PLAN_SHOWN.to_owned())
    }

    /// Waits on plan card `id`: Stop (the person's own, not a card closed
    /// with its task or by their message) ends browsing for the task.
    async fn watch_plan(
        &self,
        bot_id: &BotId,
        id: u64,
        rx: oneshot::Receiver<Answer>,
        ask: policy::Ask,
    ) {
        let answer = tokio::time::timeout(self.config.approval_timeout, rx).await;
        let by_message = lock(&self.answered_by_message).remove(&id);
        let closed = lock(&self.notices_closed).remove(&id);
        match answer {
            Ok(Ok(Answer::Deny)) if !by_message && !closed => {
                if let Some(runtime) = self.runtimes.lock().await.get_mut(bot_id) {
                    runtime.plan_ok = false;
                    runtime.plan_stopped = true;
                }
                tracing::info!(bot = %bot_id, "the person stopped its plan");
                let _ = self.record(
                    bot_id,
                    EventKind::Approval {
                        action: ask.action,
                        target: ask.target,
                        site: ask.site,
                        outcome: Outcome::Denied,
                    },
                );
            }
            // Nobody answered: the card just goes.
            Err(_) => {
                self.answer(id, Answer::Deny);
            }
            _ => {}
        }
    }

    /// `save_home_area`: the owner's card, then the home's area.
    pub(super) async fn save_home_area_tool(
        &self,
        bot_id: &BotId,
        asker: &Viewer,
        area: &str,
    ) -> (bool, String) {
        let area = area.split_whitespace().collect::<Vec<_>>().join(" ");
        if area.is_empty() || area.chars().count() > MAX_AREA {
            return (
                false,
                format!(
                    "Not saved: the area should be a short place name, up to {MAX_AREA} characters."
                ),
            );
        }
        let owner = self.owner_person_id();
        if owner.is_none() || asker.id != owner {
            return (
                false,
                format!(
                    "Not saved: only the home's owner can set the home area. Use {area} for this \
                     task, and tell the person the owner can save it in Settings → People."
                ),
            );
        }
        let Ok(bot) = self.bot(bot_id.as_str()) else {
            return (false, "oxbot lost track of this bot.".to_owned());
        };
        let ask = policy::Ask {
            answerer: owner,
            ..policy::Ask::new(
                ApprovalKind::HomeArea,
                "save the home area",
                area.clone(),
                "oxbot",
            )
        };
        let (outcome, id) = match self.confirm_card(&bot, &ask, None).await {
            Ok(answer) => answer,
            Err(err) => return (false, format!("Couldn't ask the person: {err}")),
        };
        let by_message = lock(&self.answered_by_message).remove(&id);
        match outcome {
            Outcome::Allowed | Outcome::AllowedAlways => {
                let saved = self
                    .store
                    .set_pref(PREF_HOME_AREA, &area)
                    .and_then(|()| self.store.set_pref(PREF_HOME_AREA_USE, "1"));
                if let Err(err) = saved {
                    return (false, format!("Couldn't save the home area: {err}"));
                }
                tracing::info!(bot = %bot_id, "saved the home area");
                let _ = self
                    .updates
                    .send(oxbot_protocol::ServerMsg::ProfilesChanged);
                (
                    true,
                    format!(
                        "Saved: the home area is {area}. Every bot gets it with each task from now on."
                    ),
                )
            }
            _ if by_message => (
                false,
                format!(
                    "Not saved: the person replied instead. Use {area} for this task and read their message."
                ),
            ),
            _ => (
                false,
                format!("Not saved: the person said no. Use {area} for this task only."),
            ),
        }
    }
}
