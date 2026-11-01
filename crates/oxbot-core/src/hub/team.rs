//! oxbot's own tools: Herder's team tools (`team`, `give_task`,
//! `read_reply`) and the sign-in tool, run for a bot's Claude.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, PoisonError};

use oxbot_protocol::{EventKind, ServerMsg};
use serde_json::Value;
use tokio::sync::broadcast;

use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::id::BotId;
use crate::tools::{self, TOOL_SERVER_NAME};

impl Hub {
    /// Answers a message for oxbot's own tool server.
    pub(super) async fn tool_message(
        self: Arc<Self>,
        bot: BotId,
        request_id: String,
        message: Value,
    ) {
        let central = self.bot(bot.as_str()).is_ok_and(|b| b.is_central());
        let response = tools::handle(&message, central, |tool, args| {
            let hub = Arc::clone(&self);
            let bot = bot.clone();
            hub.run_tool(bot, tool, args)
        })
        .await;
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(&bot)
            && let Err(err) = runtime.process.answer_mcp(&request_id, response).await
        {
            tracing::warn!(%bot, %err, "couldn't answer Claude's tool call");
        }
    }

    /// Runs one of oxbot's tools for `bot` (`tools::handle` has already
    /// checked the bot may use it). Returns whether it worked and what to
    /// tell the bot.
    ///
    /// Boxed with a named type: giving a task can start another bot, whose
    /// tool calls come back here, and the compiler needs that loop broken.
    fn run_tool(
        self: Arc<Self>,
        bot: BotId,
        tool: String,
        args: Value,
    ) -> std::pin::Pin<Box<dyn Future<Output = (bool, String)> + Send>> {
        Box::pin(async move { self.run_tool_inner(&bot, &tool, &args).await })
    }

    async fn run_tool_inner(
        self: &Arc<Self>,
        bot: &BotId,
        tool: &str,
        args: &Value,
    ) -> (bool, String) {
        let text = |key: &str| tools::text_arg(args, key).trim().to_owned();
        // Herder acts for whoever sent the message it's answering, and sees
        // only what they may see (never the Farm's view).
        let asker = self
            .runtimes
            .lock()
            .await
            .get(bot)
            .and_then(|r| r.asker.clone());
        let asker = Viewer::gate(asker.unwrap_or_default());
        match tool {
            tools::SIGN_IN_TOOL => self.request_sign_in(bot, &text("url"), &text("why")).await,
            tools::LIST_BOTS_TOOL => (true, self.team(bot, &asker).await),
            tools::CREATE_BOT_TOOL => {
                // The user already said yes on the card (see `policy`).
                // The bot belongs to whoever asked Herder for it.
                let private = args.get("private").and_then(Value::as_bool) == Some(true);
                let created_by = Some(asker.name.as_str()).filter(|n| !n.is_empty());
                if private && created_by.is_none() {
                    return (
                        false,
                        "A private bot needs to know who it's for; ask the user to send their \
                         request again."
                            .to_owned(),
                    );
                }
                match self
                    .create_bot_with(&text("name"), &text("instructions"), created_by, private)
                    .await
                {
                    Ok(info) => (
                        true,
                        format!(
                            "Created \"{}\" (id {}). It's in the user's sidebar. Give it tasks with give_task.",
                            info.name, info.id
                        ),
                    ),
                    Err(err) => (false, format!("Couldn't create the bot: {err}")),
                }
            }
            tools::GIVE_TASK_TOOL => {
                let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(true);
                self.give_task(bot, &asker, &text("bot"), &text("message"), wait)
                    .await
            }
            tools::READ_REPLY_TOOL => self.read_reply(&asker, &text("bot")).await,
            _ => (false, "unknown tool".to_owned()),
        }
    }

    /// The bots `asker` may use, for Herder: id, name, maker, what it's
    /// for, and status.
    async fn team(&self, me: &BotId, asker: &Viewer) -> String {
        let busy: HashSet<BotId> = self
            .runtimes
            .lock()
            .await
            .iter()
            .filter(|(_, r)| r.busy)
            .map(|(id, _)| id.clone())
            .collect();
        let mut bots: Vec<Bot> = self
            .bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        bots.sort_by(|a, b| a.name.cmp(&b.name));
        let mut out = String::new();
        for b in bots {
            // Other people's Herders (and the Farm's, for them) aren't team members.
            if b.is_central() && &b.id != me {
                continue;
            }
            if !b.visible_to(asker) {
                let who = b.created_by.as_deref().unwrap_or("someone");
                let _ = writeln!(
                    out,
                    "- {} (id {}): private to {who}, locked. You can't see it or give it tasks.",
                    b.name, b.id
                );
                continue;
            }
            let purpose: String = b
                .instructions
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(160)
                .collect();
            let status = if busy.contains(&b.id) {
                "working"
            } else {
                "free"
            };
            let you = if b.is_central() { " (you)" } else { "" };
            let maker = match (&b.created_by, b.private) {
                (Some(who), true) => {
                    format!(", private to {who} (only they open it; you can't give it tasks)")
                }
                (Some(who), false) => format!(", made by {who}"),
                (None, _) => String::new(),
            };
            let _ = writeln!(
                out,
                "- {} (id {}){you}{maker}: {status}. {}",
                b.name,
                b.id,
                if purpose.is_empty() {
                    "No instructions."
                } else {
                    &purpose
                }
            );
        }
        out
    }

    /// Sends `target` a task from Herder (`from`) and, if `wait`,
    /// waits for its reply.
    async fn give_task(
        self: &Arc<Self>,
        from: &BotId,
        asker: &Viewer,
        target: &str,
        message: &str,
        wait: bool,
    ) -> (bool, String) {
        if let Some(locked) = self.locked_message(target, asker) {
            return (false, locked);
        }
        let Ok(to) = self.bot_for(target, asker) else {
            return (
                false,
                format!("There's no bot with id {target}. Use list_bots."),
            );
        };
        if &to.id == from {
            return (false, "That's you. Do the task yourself.".to_owned());
        }
        if let Some(private) = private_message(&to) {
            return (false, private);
        }
        // Give up the browser first, so the other bot can use it.
        if let Some(runtime) = self.runtimes.lock().await.get_mut(from) {
            Self::release_browser(from, runtime);
        }
        let mut updates = self.subscribe();
        let start = self.seqs_of(&to.id).last;
        let sender = self.bot_name(from);
        let person = Some(asker.name.as_str());
        match self
            .send_for(to.id.as_str(), message, Some(&sender), person)
            .await
        {
            Ok(()) => {}
            Err(HubError::Busy(_)) => {
                return (
                    false,
                    format!(
                        "{} is busy with something else. Try again later, or read_reply.",
                        to.name
                    ),
                );
            }
            Err(err) => return (false, format!("Couldn't send the task: {err}")),
        }
        tracing::info!(from = %from, to = %to.id, "gave a task");
        if !wait {
            return (
                true,
                format!("Sent to {}. Use read_reply for its answer.", to.name),
            );
        }
        let finished = tokio::time::timeout(self.config.task_timeout, async {
            loop {
                match updates.recv().await {
                    Ok(ServerMsg::Event(e)) if e.bot == to.id.as_str() => {
                        if let EventKind::TurnDone { ok, .. } = e.kind {
                            return ok;
                        }
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return false,
                }
            }
        })
        .await;
        let reply = self.latest_reply(&to.id, start);
        match finished {
            Err(_) => (
                true,
                format!(
                    "{} is still working after {} minutes. Use read_reply later.",
                    to.name,
                    self.config.task_timeout.as_secs() / 60
                ),
            ),
            Ok(ok) => match reply {
                Some(text) => (true, format!("{} replied:\n{text}", to.name)),
                None if ok => (true, format!("{} finished without a reply.", to.name)),
                None => (false, format!("{} stopped without finishing.", to.name)),
            },
        }
    }

    /// A bot's latest reply and whether it's still working.
    async fn read_reply(&self, asker: &Viewer, target: &str) -> (bool, String) {
        if let Some(locked) = self.locked_message(target, asker) {
            return (false, locked);
        }
        let Ok(bot) = self.bot_for(target, asker) else {
            return (
                false,
                format!("There's no bot with id {target}. Use list_bots."),
            );
        };
        if let Some(private) = private_message(&bot) {
            return (false, private);
        }
        let busy = self
            .runtimes
            .lock()
            .await
            .get(&bot.id)
            .is_some_and(|r| r.busy);
        let status = if busy {
            "still working"
        } else {
            "not working right now"
        };
        match self.latest_reply(&bot.id, 0) {
            Some(text) => (
                true,
                format!("{} ({status}). Latest reply:\n{text}", bot.name),
            ),
            None => (true, format!("{} ({status}) hasn't replied yet.", bot.name)),
        }
    }

    /// What Herder hears about a bot that's private to someone else.
    fn locked_message(&self, target: &str, asker: &Viewer) -> Option<String> {
        let bot = self.bot(target).ok()?;
        (!bot.visible_to(asker)).then(|| {
            format!(
                "{} is private to {}. Only they can use it; tell the user that.",
                bot.name,
                bot.created_by.as_deref().unwrap_or("someone else")
            )
        })
    }

    /// The text of `bot`'s latest reply after event `since`.
    pub(super) fn latest_reply(&self, bot: &BotId, since: u64) -> Option<String> {
        let seq = self.seqs_of(bot).last_reply;
        if seq <= since {
            return None;
        }
        match self.store.event(bot, seq).ok()??.kind {
            EventKind::AssistantMessage { text } => Some(text),
            _ => None,
        }
    }
}

/// Herder doesn't relay private bots: what they do would land in Herder's
/// chat, which everyone can read. `None` for a shared bot.
fn private_message(bot: &Bot) -> Option<String> {
    bot.private.then(|| {
        format!(
            "{} is private. Herder doesn't pass tasks to private bots or read their replies; \
             tell the user to open {} in the sidebar and ask it there.",
            bot.name, bot.name
        )
    })
}

/// Plain names for Herder's team tools.
pub(super) fn team_label(name: &str) -> Option<&'static str> {
    let tool = name.strip_prefix(&format!("mcp__{TOOL_SERVER_NAME}__"))?;
    Some(match tool {
        tools::LIST_BOTS_TOOL => "looking at your bots",
        tools::CREATE_BOT_TOOL => "creating a bot",
        tools::GIVE_TASK_TOOL => "giving a bot a task",
        tools::READ_REPLY_TOOL => "reading a bot's reply",
        _ => return None,
    })
}
