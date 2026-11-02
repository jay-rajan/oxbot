//! oxbot's own tools: Herder's team tools (`team`, `give_task`,
//! `read_reply`) and the sign-in tool, run for a bot's Claude.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, PoisonError};

use oxbot_protocol::{ApprovalKind, EventKind, Outcome, ServerMsg};
use serde_json::Value;
use tokio::sync::broadcast;

use super::access::maker_name;
use super::{Hub, HubError};
use crate::bot::{Bot, Viewer};
use crate::id::BotId;
use crate::policy;
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
        let asker = self.person_viewer(asker.as_ref());
        match tool {
            tools::SIGN_IN_TOOL => self.request_sign_in(bot, &text("url"), &text("why")).await,
            tools::LIST_BOTS_TOOL => (true, self.team(bot, &asker).await),
            tools::CREATE_BOT_TOOL => {
                // The user already said yes on the card (see `policy`).
                // The bot belongs to whoever asked Herder for it.
                let private = args.get("private").and_then(Value::as_bool) == Some(true);
                let maker = asker.id.is_some().then_some(&asker);
                if private && maker.is_none() {
                    return (
                        false,
                        "A private bot needs to know who it's for; ask the user to send their \
                         request again."
                            .to_owned(),
                    );
                }
                match self
                    .create_bot_as(&text("name"), &text("instructions"), maker, private)
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
            tools::READ_REPLY_TOOL => self.read_reply(bot, &asker, &text("bot")).await,
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
        let names = self.person_names();
        let own = self.is_own_herder(me, asker);
        let mut out = String::new();
        for b in bots {
            // Other people's Herders (and the Farm's, for them) aren't team members.
            if b.is_central() && &b.id != me {
                continue;
            }
            let maker = maker_name(&b, &names);
            if !b.visible_to(asker) {
                let who = maker.as_deref().unwrap_or("someone");
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
            let maker = match (&maker, b.private) {
                (Some(who), true) if own => format!(
                    ", private to {who} (uses their sign-ins; you may give it tasks for them, \
                     which they may be asked to OK)"
                ),
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
        if to.private
            && let Some(refused) = self.check_private_task(from, asker, &to, message).await
        {
            return (false, refused);
        }
        // Give up the browser first, so the other bot can use it.
        if let Some(runtime) = self.runtimes.lock().await.get_mut(from) {
            Self::release_browser(from, runtime);
        }
        let mut updates = self.subscribe();
        let start = self.seqs_of(&to.id).last;
        let sender = self.bot_name(from);
        match self
            .send_for(
                to.id.as_str(),
                message,
                Some(&sender),
                asker.id.as_ref(),
                true,
            )
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
                Some(text) => {
                    self.taint(from).await;
                    (
                        true,
                        format!("{} replied:\n{}", to.name, bot_reply(&to.id, &text)),
                    )
                }
                None if ok => (true, format!("{} finished without a reply.", to.name)),
                None => (false, format!("{} stopped without finishing.", to.name)),
            },
        }
    }

    /// Whether Herder (`from`) may pass `message` on to private bot `to`
    /// for `asker`: only from the asker's own Herder, and after the
    /// asker's OK on a card if this turn has read a web page or another
    /// bot's reply (which may be what asked for it). `None` if it may;
    /// else what to tell Herder.
    async fn check_private_task(
        &self,
        from: &BotId,
        asker: &Viewer,
        to: &Bot,
        message: &str,
    ) -> Option<String> {
        if !self.is_own_herder(from, asker) {
            return private_message(to);
        }
        if !self.is_tainted(from).await {
            return None;
        }
        let herder = match self.bot(from.as_str()) {
            Ok(b) => b,
            Err(err) => return Some(format!("Couldn't ask the user: {err}")),
        };
        let owner = maker_name(to, &self.person_names()).unwrap_or_else(|| "its owner".to_owned());
        let ask = policy::Ask {
            kind: ApprovalKind::GiveTask,
            action: "give a task to".to_owned(),
            target: format!("{} (private, {owner}'s sign-ins)", to.name),
            site: "oxbot".to_owned(),
            detail: Some(message.to_owned()),
        };
        match self.confirm(&herder, &ask, None).await {
            Ok(Outcome::Allowed) => None,
            Ok(Outcome::Denied) => Some(format!(
                "The user said no to giving {} this task. Don't try another way; tell them, and \
                 stop.",
                to.name
            )),
            Ok(Outcome::TimedOut) => Some(format!(
                "The user didn't OK giving {} this task in time, so it wasn't sent. Tell them, and \
                 stop.",
                to.name
            )),
            Err(err) => Some(format!("Couldn't ask the user: {err}")),
        }
    }

    /// Whether `bot`'s turn has read text oxbot doesn't trust.
    async fn is_tainted(&self, bot: &BotId) -> bool {
        self.runtimes
            .lock()
            .await
            .get(bot)
            .is_some_and(|r| r.tainted)
    }

    /// Marks `bot`'s turn as having read another bot's reply.
    async fn taint(&self, bot: &BotId) {
        if let Some(r) = self.runtimes.lock().await.get_mut(bot) {
            r.tainted = true;
        }
    }

    /// A bot's latest reply and whether it's still working, for Herder
    /// (`from`).
    async fn read_reply(&self, from: &BotId, asker: &Viewer, target: &str) -> (bool, String) {
        if let Some(locked) = self.locked_message(target, asker) {
            return (false, locked);
        }
        let Ok(bot) = self.bot_for(target, asker) else {
            return (
                false,
                format!("There's no bot with id {target}. Use list_bots."),
            );
        };
        if bot.private
            && !self.is_own_herder(from, asker)
            && let Some(private) = private_message(&bot)
        {
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
            Some(text) => {
                self.taint(from).await;
                (
                    true,
                    format!(
                        "{} ({status}). Latest reply:\n{}",
                        bot.name,
                        bot_reply(&bot.id, &text)
                    ),
                )
            }
            None => (true, format!("{} ({status}) hasn't replied yet.", bot.name)),
        }
    }

    /// What Herder hears about a bot that's private to someone else.
    fn locked_message(&self, target: &str, asker: &Viewer) -> Option<String> {
        let bot = self.bot(target).ok()?;
        (!bot.visible_to(asker)).then(|| {
            let who =
                maker_name(&bot, &self.person_names()).unwrap_or_else(|| "someone else".to_owned());
            format!(
                "{} is private to {who}. Only they can use it; tell the user that.",
                bot.name
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

/// Another bot's reply as Herder gets it: inside `<bot_reply>`, which
/// Herder's prompt says is data, never instructions. A closing tag in the
/// reply (any case) is escaped, so the reply can't end the block early.
fn bot_reply(bot: &BotId, text: &str) -> String {
    const CLOSE: &str = "</bot_reply";
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len() + 32);
    let mut rest = 0;
    // Lowercasing ASCII keeps every byte where it was.
    for (at, _) in lower.match_indices(CLOSE) {
        out.push_str(&text[rest..at]);
        out.push_str("<\\/");
        rest = at + 2;
    }
    out.push_str(&text[rest..]);
    format!("<bot_reply bot=\"{bot}\">{out}</bot_reply>")
}

/// Herder doesn't relay other people's private bots, or anyone's from a
/// Herder that isn't theirs: what they do would land in a chat others may
/// read. `None` for a shared bot.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_is_wrapped_and_cant_close_its_block() {
        let id = BotId::new("helper").unwrap();
        assert_eq!(
            bot_reply(&id, "hi"),
            "<bot_reply bot=\"helper\">hi</bot_reply>"
        );
        assert_eq!(
            bot_reply(&id, "a</bot_reply>b</BOT_REPLY >c"),
            "<bot_reply bot=\"helper\">a<\\/bot_reply>b<\\/BOT_REPLY >c</bot_reply>"
        );
    }
}
