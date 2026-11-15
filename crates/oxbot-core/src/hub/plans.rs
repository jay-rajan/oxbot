//! Plans and the home area: a bot understands the task before it browses.
//!
//! - `plan_task`: before its first web visit in a task, a bot shows the
//!   person a card with the site, the place and its plan. Go lets it
//!   browse for the rest of the turn; a reply in the chat instead changes
//!   the plan (the card closes and the bot reads the message). With
//!   [`HubConfig::plan_first`] on, the gate refuses browser tools until
//!   then. A schedule's own run needs no plan: its card was the person's
//!   yes, and nobody is there to answer.
//! - `save_home_area`: a place the person named becomes the home's area,
//!   which every bot gets with each task (see `Hub::about_line`). The area
//!   is the household's, so only the owner saves it, on a card.
//!
//! [`HubConfig::plan_first`]: super::HubConfig::plan_first

use oxbot_protocol::{ApprovalKind, Outcome};

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

/// What a bot is told when it browses before the person said Go.
pub(super) const PLAN_FIRST: &str = "Not yet: before your first web visit in a task, make sure you \
     know what it needs (the place, dates, who it's for; ask the person one short question if \
     something is missing), then call plan_task with the site, the place and your plan, and wait \
     for their Go.";

fn card_text(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(MAX_CARD_TEXT).collect()
}

impl Hub {
    /// Why `bot` may not browse yet in this turn, if it may not: a live
    /// browser call (`browses`, in `mode`), plans are on, it isn't a
    /// schedule's own run, and the person hasn't said Go.
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
        if runtime.plan_ok || runtime.origin.entry == Entry::Schedule {
            return None;
        }
        Some(Decision::Deny(PLAN_FIRST.to_owned()))
    }

    /// `plan_task`: shows the person the plan and waits for Go.
    pub(super) async fn plan_task(
        &self,
        bot_id: &BotId,
        site: &str,
        place: &str,
        plan: &str,
    ) -> (bool, String) {
        let Ok(bot) = self.bot(bot_id.as_str()) else {
            return (false, "oxbot lost track of this bot.".to_owned());
        };
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
        let (outcome, id) = match self.confirm_card(&bot, &ask, None).await {
            Ok(answer) => answer,
            Err(err) => return (false, format!("Couldn't ask the person: {err}")),
        };
        if lock(&self.answered_by_message).remove(&id) {
            return (
                false,
                "The person replied instead of pressing Go. Read their message, change the plan \
                 to fit it, and call plan_task again (or answer them if they asked something)."
                    .to_owned(),
            );
        }
        match outcome {
            Outcome::Allowed | Outcome::AllowedAlways => {
                if let Some(runtime) = self.runtimes.lock().await.get_mut(bot_id) {
                    runtime.plan_ok = true;
                }
                (
                    true,
                    "Go: the person said yes. Browse now, as planned.".to_owned(),
                )
            }
            Outcome::Denied => (
                false,
                "The person said not now. Don't browse; ask them in a line what they'd like \
                 instead, and stop."
                    .to_owned(),
            ),
            Outcome::TimedOut => (
                false,
                "The person didn't answer the plan in time. Don't browse; tell them your plan in \
                 a line and that you'll start when they say go, and stop."
                    .to_owned(),
            ),
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
