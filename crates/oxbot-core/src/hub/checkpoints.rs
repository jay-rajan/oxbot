//! Check-ins on long tasks: after [`HubConfig::max_steps`] model requests
//! a bot stops to write where it is (the engine asks it to), and the
//! person gets a card that counts down: it carries on by itself unless
//! they press Stop. Carrying on is a new turn in the same conversation,
//! so a task has no step limit, and the chat stays up to date.
//!
//! [`HubConfig::max_steps`]: super::HubConfig::max_steps

use std::sync::Arc;

use oxbot_protocol::{Answer, ApprovalKind, EventKind};

use super::engines::ChainOrigin;
use super::{Hub, lock};
use crate::id::BotId;
use crate::policy;

/// What a bot is told when it carries on.
pub(super) const CARRY_ON: &str = "(oxbot: the person let you carry on. Go on with the task \
     where you left off, and when it's done, give your full answer, briefly.)";

/// What a bot is told when the person's next message came while a
/// check-in was still counting down: the task it paused isn't done
/// (added after the message).
pub(super) const LEFT_OFF: &str = "\n\n(oxbot: your last task paused at a check-in before it was \
     done, and this message came before it carried on. Answer this message, then finish what was \
     left of that task, unless this message changes or cancels it.)";

impl Hub {
    /// `bot_id` stopped to check in after `steps` steps of a task for
    /// `origin`: shows the card and, unless the person stops it, carries
    /// on.
    pub(super) fn offer_more(self: &Arc<Self>, bot_id: &BotId, steps: u32, origin: ChainOrigin) {
        let hub = Arc::clone(self);
        let bot_id = bot_id.clone();
        tokio::spawn(async move { hub.check_in(&bot_id, steps, origin).await });
    }

    async fn check_in(self: &Arc<Self>, bot_id: &BotId, steps: u32, origin: ChainOrigin) {
        let Ok(bot) = self.bot(bot_id.as_str()) else {
            return;
        };
        let wait = self.config.checkpoint_wait;
        let ask = policy::Ask {
            detail: Some(wait.as_secs().to_string()),
            ..policy::Ask::new(
                ApprovalKind::Checkpoint,
                "keep going",
                format!("{steps} steps"),
                "oxbot",
            )
        };
        let (id, rx) = self.open_card(&bot, &ask, None).await;
        let answer = tokio::time::timeout(wait, rx).await;
        let by_message = lock(&self.answered_by_message).remove(&id);
        let closed = lock(&self.notices_closed).remove(&id);
        match answer {
            // Nobody answered: it carries on.
            Err(_) => {
                self.answer(id, Answer::AllowOnce);
            }
            Ok(Ok(Answer::AllowOnce | Answer::AlwaysAllow)) => {}
            // Stop; or the card went with a new message (which is the
            // next turn) or the bot.
            Ok(Ok(Answer::Deny) | Err(_)) => {
                if !by_message && !closed {
                    tracing::info!(bot = %bot_id, steps, "stopped at a check-in");
                    let _ = self.record(
                        bot_id,
                        EventKind::Note {
                            text: format!("Stopped after {steps} steps, as you asked."),
                        },
                    );
                }
                return;
            }
        }
        let Ok(note) = self.record(
            bot_id,
            EventKind::Note {
                text: format!("Keeping going after {steps} steps."),
            },
        ) else {
            return;
        };
        tracing::info!(bot = %bot_id, steps, "carrying on after a check-in");
        if let Err(err) = self.continue_task(bot_id, origin, note.seq).await {
            tracing::info!(bot = %bot_id, %err, "couldn't carry on after a check-in");
        }
    }

    /// Closes `bot`'s open check-in cards: a new turn started. Whether
    /// there was one (a task left unfinished).
    pub(super) fn close_checkpoints(&self, bot: &BotId) -> bool {
        let ids: Vec<u64> = lock(&self.approvals)
            .iter()
            .filter(|(_, (a, _, _))| a.bot == bot.as_str() && a.kind == ApprovalKind::Checkpoint)
            .map(|(id, _)| *id)
            .collect();
        let mut closed = false;
        for id in ids {
            lock(&self.notices_closed).insert(id);
            if self.answer(id, Answer::Deny) {
                closed = true;
            } else {
                lock(&self.notices_closed).remove(&id);
            }
        }
        closed
    }
}
