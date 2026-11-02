//! Turns: delivering a message to a bot's Claude, cancelling, stopping
//! idle bots and shutting down.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oxbot_browser::BrowserStatus;
use oxbot_protocol::{EventKind, ServerMsg};
use tokio::sync::OwnedSemaphorePermit;

use super::{Hub, HubError, lock};
use crate::bot::{Bot, Viewer};
use crate::id::{BotId, PersonId};

impl Hub {
    /// Sends a message to a bot from the person called `from`, if known
    /// (made if new). Returns once Claude has it; the reply arrives through
    /// [`Hub::subscribe`]. Waits if `max_concurrent_turns` turns are already
    /// running.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist, is busy, or Claude can't be started.
    pub async fn send(
        self: &Arc<Self>,
        bot: &str,
        text: &str,
        from: Option<&str>,
    ) -> Result<(), HubError> {
        let from = from.map(str::trim).filter(|f| !f.is_empty());
        let person = match from {
            Some(name) => Some(self.sender(name)?),
            None => None,
        };
        self.send_for(bot, text, from, person.as_ref()).await
    }

    /// Like [`Hub::send`], from `viewer` (by their person id).
    ///
    /// # Errors
    /// Fails if the bot doesn't exist, is busy, or Claude can't be started.
    pub async fn send_as(
        self: &Arc<Self>,
        bot: &str,
        text: &str,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let viewer = self.resolve(viewer).into_owned();
        let from = Some(viewer.name.as_str()).filter(|n| !n.is_empty());
        self.send_for(bot, text, from, viewer.id.as_ref()).await
    }

    /// The person called `name`, telling every client if they're new.
    fn sender(&self, name: &str) -> Result<PersonId, HubError> {
        let (person, new) = self.store.find_or_create_person(name)?;
        if new {
            let _ = self.updates.send(ServerMsg::People {
                people: self.people()?,
            });
        }
        Ok(person.id)
    }

    /// Delivers a message shown as from `from`, for a turn that acts for
    /// `person` (who may not be the sender: Herder passes tasks on for
    /// people).
    pub(super) async fn send_for(
        self: &Arc<Self>,
        bot: &str,
        text: &str,
        from: Option<&str>,
        person: Option<&PersonId>,
    ) -> Result<(), HubError> {
        let bot = self.bot(bot)?;
        let from = from.map(str::trim).filter(|f| !f.is_empty());
        let permit = Arc::clone(&self.turns)
            .acquire_owned()
            .await
            .map_err(|_| HubError::Busy(bot.id.clone()))?;

        // Claim the bot, so a second message can't slip in while Claude starts.
        self.claim(&bot.id).await?;
        // It may have been made private, renamed or deleted while this
        // message waited for a turn: use it as it is now.
        let result = match self.bot(bot.id.as_str()) {
            Ok(bot) => self.deliver(&bot, text, from, person, permit).await,
            Err(err) => Err(err),
        };
        lock(&self.starting).remove(&bot.id);
        result
    }

    /// Marks `bot` as starting, so no message, privacy change or delete can
    /// run on it at the same time. Undo with `lock(&self.starting).remove`.
    pub(super) async fn claim(&self, bot: &BotId) -> Result<(), HubError> {
        let runtimes = self.runtimes.lock().await;
        let mut starting = lock(&self.starting);
        if runtimes.get(bot).is_some_and(|r| r.busy) || starting.contains(bot) {
            return Err(HubError::Busy(bot.clone()));
        }
        starting.insert(bot.clone());
        Ok(())
    }

    async fn deliver(
        self: &Arc<Self>,
        bot: &Bot,
        text: &str,
        from: Option<&str>,
        person: Option<&PersonId>,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), HubError> {
        self.record(
            &bot.id,
            EventKind::UserMessage {
                text: text.to_owned(),
                from: from.map(str::to_owned),
            },
        )?;

        let running = self.runtimes.lock().await.contains_key(&bot.id);
        if !running {
            // Started outside the lock: the first start can take a while.
            let runtime = match self.start_claude(bot).await {
                Ok(r) => r,
                Err(err) => {
                    self.turn_failed(&bot.id, &err.to_string())?;
                    return Err(err);
                }
            };
            self.runtimes.lock().await.insert(bot.id.clone(), runtime);
        }

        let message = sender_line(from, text);
        let mut runtimes = self.runtimes.lock().await;
        let Some(runtime) = runtimes.get_mut(&bot.id) else {
            unreachable!("inserted above, and only this call may touch the bot meanwhile");
        };
        if let Err(err) = runtime.process.send(&message).await {
            runtimes.remove(&bot.id);
            drop(runtimes);
            self.turn_failed(&bot.id, &err.to_string())?;
            return Err(err.into());
        }
        runtime.busy = true;
        runtime.permit = Some(permit);
        runtime.last_active = Instant::now();
        runtime.asker = person.cloned();
        drop(runtimes);
        self.announce_bots().await;
        Ok(())
    }

    pub(super) fn turn_failed(&self, bot: &BotId, message: &str) -> Result<(), HubError> {
        self.record(
            bot,
            EventKind::Error {
                message: message.to_owned(),
            },
        )?;
        self.record(
            bot,
            EventKind::TurnDone {
                ok: false,
                duration_ms: 0,
            },
        )?;
        Ok(())
    }

    /// Asks a busy bot to stop its current turn, and says no to any card or
    /// sign-in it's waiting on. Does nothing if it's idle.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist or Claude has stopped.
    pub async fn cancel(&self, bot: &str) -> Result<(), HubError> {
        let id = self.bot(bot)?.id;
        self.deny_open_approvals(&id);
        self.cancel_sign_ins(&id);
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(&id).filter(|r| r.busy) {
            runtime.process.interrupt().await?;
        }
        Ok(())
    }

    /// Stops every `claude` child and the browser (sign-ins stay).
    pub async fn shutdown(&self) {
        let runtimes: Vec<_> = self.runtimes.lock().await.drain().collect();
        for (_, runtime) in runtimes {
            runtime.process.shutdown().await;
        }
        for shared in self.all_browsers() {
            shared.browser.shutdown().await;
        }
    }

    pub(super) async fn stop_claude(&self, bot: &BotId) {
        let runtime = self.runtimes.lock().await.remove(bot);
        if let Some(r) = runtime {
            r.process.shutdown().await;
        }
    }

    /// Stops bots that have been idle for `idle_stop`, and the browser once
    /// nobody has used it for that long.
    async fn stop_idle(&self) {
        let waiting: HashSet<String> = self
            .pending_approvals()
            .into_iter()
            .map(|a| a.bot)
            .chain(self.pending_sign_ins().into_iter().map(|r| r.bot))
            .collect();
        let idle: Vec<BotId> = self
            .runtimes
            .lock()
            .await
            .iter()
            .filter(|(id, r)| {
                !r.busy
                    && r.last_active.elapsed() >= self.config.idle_stop
                    && !waiting.contains(id.as_str())
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in idle {
            tracing::info!(bot = %id, "idle; stopping Claude until the next message");
            self.stop_claude(&id).await;
        }
        if !waiting.is_empty() {
            return;
        }
        for shared in self.all_browsers() {
            if lock(&shared.last_used).elapsed() >= self.config.idle_stop
                && *shared.browser.status().borrow() == BrowserStatus::Ready
                && let Ok(_guard) = Arc::clone(&shared.lease).try_lock_owned()
            {
                tracing::info!(
                    browser = %shared.owner,
                    "browser idle; stopping Chrome until a bot needs it (sign-ins stay)"
                );
                shared.browser.shutdown().await;
                lock(&shared.last).take();
            }
        }
    }
}

/// Every minute, stops bots that have been idle for a while.
pub(super) async fn stop_idle_bots(hub: std::sync::Weak<Hub>) {
    loop {
        tokio::time::sleep(Duration::from_mins(1)).await;
        let Some(hub) = hub.upgrade() else { return };
        hub.stop_idle().await;
    }
}

/// What Claude gets for a message: `[Name] text`. Only that first bracket
/// names the sender, so a line in the text that starts with `[` gets a
/// backslash and can't pass for another person's message.
pub(super) fn sender_line(from: Option<&str>, text: &str) -> String {
    let body = text
        .split('\n')
        .map(|line| {
            if line.trim_start().starts_with('[') {
                format!("\\{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    match from {
        Some(name) => format!("[{name}] {body}"),
        None => body,
    }
}
