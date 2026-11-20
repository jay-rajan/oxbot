//! Kid accounts (people under 18): what's different for them.
//!
//! - Every card from a kid's chats goes to the administrators, never the
//!   kid (see `open_card`).
//! - A kid's message with files waits for an administrator: the bot can't
//!   see the files until one presses Allow.
//! - A kid's message is checked first ([`Hub::kid_check`]); one that should
//!   go past a parent waits the same way. Curious questions are fine.
//! - Bots are told, with each kid message, to answer as for a child
//!   ([`KID_RULES`]).

use std::sync::Arc;
use std::time::Duration;

use oxbot_protocol::{Answer, ApprovalKind, Attachment, EventKind, NoteUpload};

use super::Hub;
use super::engines::ChainOrigin;
use crate::bot::{Bot, Viewer};
use crate::id::PersonId;
use crate::policy;
use crate::store::Role;

/// How long a kid's message waits for an administrator.
const KID_WAIT: Duration = Duration::from_hours(12);

/// What a bot is told with every message from a kid.
pub(super) const KID_RULES: &str = "[oxbot] This message is from a child (under 18). Answer \
     as you would for a curious child: kind, honest and simple, and welcome their questions. \
     Keep to what's right for their age: nothing sexual or romantic, no gore, nothing on \
     drugs, alcohol, weapons, gambling or getting around rules, and no ways to hurt \
     themselves or others. For worries about health, safety, feelings or someone bothering \
     them, answer gently and say to talk to a parent or a trusted adult. Never ask for or \
     repeat their address, school, phone, passwords or photos of themselves, and never \
     buy, sign up, sign in or message anyone for them.";

/// What the check model is told: how to judge a kid's message.
const CHECK_RULES: &str = "You check messages a child (under 18) sends to a family's AI \
     assistant, before the assistant sees them. Answer with one line: OK, or HOLD: and a \
     short reason a parent would understand. HOLD anything sexual or romantic, self-harm or \
     suicide, hurting someone, drugs, alcohol, weapons, gambling, meeting or messaging \
     strangers, sharing personal details (address, school, phone, passwords, photos of \
     themselves), buying or signing up for things, getting around a parent's rules, or \
     anything else a parent should see first. Curious questions (science, nature, history, \
     how things work, homework, games, stories) are OK.";

/// The longest check answer kept.
const CHECK_TOKENS: u32 = 120;

impl Hub {
    /// Whether `person` is a kid account.
    pub(super) fn is_kid(&self, person: Option<&PersonId>) -> bool {
        person.is_some_and(|p| self.role_of(p) == Role::Kid)
    }

    /// oxbot's lines after a message for `person`: what they let bots know
    /// about them, and for a kid, [`KID_RULES`].
    pub(super) fn asker_lines(&self, person: Option<&PersonId>) -> Vec<String> {
        let mut lines: Vec<String> = person.and_then(|p| self.about_line(p)).into_iter().collect();
        if self.is_kid(person) {
            lines.push(KID_RULES.to_owned());
        }
        lines
    }

    /// Why a kid's message `text` to `bot` should wait for an
    /// administrator, or `None` if it's fine. If it can't be checked, it
    /// waits.
    pub(super) async fn kid_check(
        &self,
        bot: &Bot,
        person: Option<&PersonId>,
        text: &str,
    ) -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }
        let Some(engine) = self.loop_engine.as_ref() else {
            return Some("oxbot couldn't check it: no AI is set up".to_owned());
        };
        let messages = vec![
            oxbot_llm::Message::system(CHECK_RULES),
            oxbot_llm::Message::user(format!("The child's message:\n<message>\n{text}\n</message>")),
        ];
        match engine
            .ask(
                bot.id.as_str(),
                person.map(PersonId::as_str),
                messages,
                CHECK_TOKENS,
            )
            .await
        {
            Ok(answer) => check_verdict(&answer),
            Err(err) => {
                tracing::warn!(bot = %bot.id, %err, "couldn't check a kid's message");
                Some("oxbot couldn't check it just now".to_owned())
            }
        }
    }

    /// Holds a kid's message (and its claimed files, not yet where the
    /// bot can read them) until an administrator answers its card: Allow
    /// sends it as it would have gone; Don't (or no answer in 12 hours)
    /// drops it. The chat says it's waiting, and how it ended.
    #[allow(
        clippy::too_many_arguments,
        reason = "a held message is all of a send's parts"
    )]
    pub(super) fn hold_kid_message(
        self: &Arc<Self>,
        target: Bot,
        text: String,
        files: Vec<Attachment>,
        ai: Vec<String>,
        notes: Vec<NoteUpload>,
        viewer: Viewer,
        origin: ChainOrigin,
        why: String,
    ) {
        let hub = Arc::clone(self);
        let name = if viewer.name.is_empty() {
            "This kid".to_owned()
        } else {
            viewer.name.clone()
        };
        let _ = self.record(
            &target.id,
            EventKind::Note {
                text: format!("{name}'s message is waiting for a parent's OK."),
            },
        );
        tracing::info!(bot = %target.id, files = files.len(), "a kid's message waits for an administrator");
        tokio::spawn(async move {
            let mut detail = text.trim().to_owned();
            if !files.is_empty() {
                let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
                if !detail.is_empty() {
                    detail.push_str("\n\n");
                }
                detail.push_str("Files: ");
                detail.push_str(&names.join(", "));
            }
            let ask = policy::Ask {
                answerer: viewer.id.clone(),
                detail: Some(detail),
                ..policy::Ask::new(
                    ApprovalKind::KidMessage,
                    format!("{name} wants to send a message to"),
                    why,
                    "oxbot",
                )
            };
            let (id, rx) = hub.open_card(&target, &ask, None).await;
            let allowed = match tokio::time::timeout(KID_WAIT, rx).await {
                Ok(Ok(Answer::AllowOnce | Answer::AlwaysAllow)) => true,
                Ok(_) => false,
                Err(_) => {
                    hub.answer(id, Answer::Deny);
                    false
                }
            };
            if hub.bot(target.id.as_str()).is_err() {
                hub.uploads_not_sent(&files);
                return;
            }
            if !allowed {
                hub.uploads_not_sent(&files);
                let _ = hub.record(
                    &target.id,
                    EventKind::Note {
                        text: format!("A parent said no to {name}'s message."),
                    },
                );
                return;
            }
            let _ = hub.record(
                &target.id,
                EventKind::Note {
                    text: format!("A parent said OK to {name}'s message."),
                },
            );
            if let Err(err) = hub
                .finish_send(&target, &text, files, &ai, &notes, &viewer, &origin)
                .await
            {
                tracing::warn!(bot = %target.id, %err, "a kid's allowed message couldn't be sent");
                let _ = hub.record(
                    &target.id,
                    EventKind::Note {
                        text: format!("{name}'s message couldn't be sent: {err}"),
                    },
                );
            }
        });
    }
}

/// What the check model's answer means: `None` to go ahead, or why it
/// waits. Anything but a plain OK waits.
fn check_verdict(answer: &str) -> Option<String> {
    let line = answer.trim().lines().next().unwrap_or_default().trim();
    let upper = line.to_ascii_uppercase();
    if upper == "OK" || upper == "OK." {
        return None;
    }
    let why = line
        .split_once(':')
        .map_or(line, |(_, why)| why)
        .trim()
        .trim_end_matches('.');
    Some(if why.is_empty() || upper.starts_with("OK") {
        "oxbot's check wasn't sure about it".to_owned()
    } else {
        why.to_owned()
    })
}

/// Why a kid's message with files waits.
pub(super) fn files_wait(count: usize) -> String {
    if count == 1 {
        "It has a photo or file: a parent OKs it before the bot sees it".to_owned()
    } else {
        format!("It has {count} photos or files: a parent OKs them before the bot sees them")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_ok_goes_ahead() {
        assert_eq!(check_verdict("OK"), None);
        assert_eq!(check_verdict(" ok.\n"), None);
        assert_eq!(
            check_verdict("HOLD: asks how to buy a vape"),
            Some("asks how to buy a vape".to_owned())
        );
        assert!(check_verdict("OK, but maybe check").is_some());
        assert!(check_verdict("I'm not sure").is_some());
    }
}
