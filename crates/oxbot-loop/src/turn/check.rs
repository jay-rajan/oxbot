//! Checking a final answer before it's accepted (round 3, changes 2 and
//! 3). When a turn that used tools answers in plain text, oxbot looks for
//! the values in the answer (numbers of 10 or more, decimals, money,
//! percents, temperatures) that nothing the bot read in this task had, and
//! sends the answer back once with a short checklist: the task word for
//! word, those values, and "check each part the task asked for". The next
//! answer without a tool call ends the turn, whatever it says; what was
//! still ungrounded is recorded (`loop_checks`).
//!
//! Not checked: a turn with fewer than two tool calls that ran, or one
//! where oxbot or the person said no to a call (sent back, the model might
//! try another way).
//!
//! What counts as read: every tool result of this turn and of the window
//! (stored results in full: the whole page, not just what was shown), and
//! the window's task state. An `ask_page` answer counts only through the
//! page it came from: a helper's words aren't evidence. Numbers in the
//! person's own messages of this turn aren't checked.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use oxbot_engine::{Activity, EngineEvent};
use oxbot_llm::{Message, Role};
use tokio::time::Instant;

use super::{ToSave, Turn};
use crate::context::state::{EntryKind, EntryMeta, WindowState};
use crate::numbers::{self, Evidence};
use crate::session::{Conv, Core};
use crate::store::{CheckRow, StoredMessage};
use crate::tools::page::ASK_PAGE;

/// Tool calls a turn needs before its answer is checked.
pub(super) const CHECK_AFTER_CALLS: usize = 2;
/// Most ungrounded values named.
const MAX_NAMED: usize = 8;
/// Most characters of the task quoted back.
const MAX_GOAL: usize = 600;
/// What the chat shows while the bot checks.
const CHECKING: &str = "Checking its answer";

/// What to do with a final answer.
pub(super) enum Checked {
    /// Send it back once with this note.
    Bounce(String),
    /// Accept it; the check to record, if there was one.
    Accept(Option<CheckRow>),
}

/// The note that sends an answer back: the task word for word, the values
/// nothing read had (if any), and the checklist. `browser`: name the page
/// tools; else the stored results and files.
#[must_use]
pub(super) fn note(goal: &str, ungrounded: &[String], browser: bool) -> String {
    let goal = goal.trim();
    let goal = match goal.char_indices().nth(MAX_GOAL) {
        Some((cut, _)) => format!("{}…", &goal[..cut]),
        None => goal.to_owned(),
    };
    let mut note = format!("[oxbot] Before you answer, check it. The task was: «{goal}».");
    if !ungrounded.is_empty() {
        let tools = if browser {
            "find_in_page, read_section or query_rows"
        } else {
            "read_result or the file tools"
        };
        let _ = write!(
            note,
            " These values aren't in anything you read in this task: {}. Check them ({tools}) or \
             remove them.",
            ungrounded.join(", ")
        );
    }
    note.push_str(
        " Check each part the task asked for (every filter, the number of items, each field). \
         Then give your final answer.",
    );
    note
}

impl Turn {
    /// Decides about the final answer `text` (already saved).
    pub(super) fn answer_check(
        &mut self,
        core: &Core,
        conv: &mut Conv,
        goal: &str,
        text: &str,
        max_steps: u32,
    ) -> Checked {
        // Tool calls that ran; a turn where oxbot or the person said no
        // ends as the model says (sent back, it might try another way).
        let results: Vec<EntryMeta> = conv
            .messages
            .iter()
            .filter(|m| m.turn == self.number && m.message.role == Role::Tool)
            .filter(|m| m.message.text() != crate::runner::SKIPPED)
            .map(|m| EntryMeta::from_value(&m.meta))
            .collect();
        if results.len() < CHECK_AFTER_CALLS || results.iter().any(|m| m.kind == EntryKind::Refusal)
        {
            return Checked::Accept(None);
        }
        let bounced = self.bounced || has_check(conv, self.number);
        let missing = self.ungrounded(core, conv, text);
        if bounced {
            let before = match self.ungrounded_before.take() {
                Some(before) => before,
                // Picked up after a restart: the draft is still there.
                None => draft(conv, self.number)
                    .map(|d| self.ungrounded(core, conv, &d))
                    .unwrap_or_default(),
            };
            let ms = self.bounced_at.map_or(0, |at| {
                i64::try_from(at.elapsed().as_millis()).unwrap_or(i64::MAX)
            });
            return Checked::Accept(Some(CheckRow {
                session: core.id.clone(),
                turn: self.number,
                bounced: true,
                ungrounded_before: before,
                ungrounded_after: missing,
                ms,
            }));
        }
        // No room for one more step: accepted as it is.
        if self.step >= max_steps {
            return Checked::Accept(Some(CheckRow {
                session: core.id.clone(),
                turn: self.number,
                bounced: false,
                ungrounded_before: missing.clone(),
                ungrounded_after: missing,
                ms: 0,
            }));
        }
        self.bounced = true;
        self.bounced_at = Some(Instant::now());
        let note = note(goal, &missing, core.browser.is_some());
        self.ungrounded_before = Some(missing);
        Checked::Bounce(note)
    }

    /// Saves the note that sends the answer back, and says so in the chat.
    pub(super) fn bounce(
        &self,
        core: &Core,
        conv: &mut Conv,
        note: String,
    ) -> Result<(), crate::runner::Stop> {
        let _ = core
            .events
            .try_send(EngineEvent::Activity(Activity::Other(CHECKING.to_owned())));
        let save = ToSave {
            message: Message::user(note),
            untrusted: false,
            handle: None,
            meta: EntryMeta {
                kind: EntryKind::Check,
                ..EntryMeta::default()
            },
        };
        self.save(core, conv, vec![save], None)
    }

    /// The values in `text` that nothing read in this task had (at most
    /// 8), leaving out those the person wrote this turn.
    fn ungrounded(&self, core: &Core, conv: &Conv, text: &str) -> Vec<String> {
        let mut said = Evidence::new();
        for m in conv
            .messages
            .iter()
            .filter(|m| m.turn == self.number && m.message.role == Role::User && !is_check(m))
        {
            said.add(&m.message.text());
        }
        let claims: Vec<String> = numbers::claims(text)
            .into_iter()
            .filter(|c| !said.has(c))
            .collect();
        if claims.is_empty() {
            return Vec::new();
        }
        evidence(core, conv, self.number).missing(&claims, MAX_NAMED)
    }
}

/// Everything read in this task: this turn's and the window's tool results
/// (stored ones in full) and the window's task state.
fn evidence(core: &Core, conv: &Conv, turn: u32) -> Evidence {
    let state = WindowState::from_value(&conv.context);
    let mut read = Evidence::new();
    read.add(&state.header);
    let mut stored: BTreeSet<&str> = BTreeSet::new();
    for m in conv
        .messages
        .iter()
        .filter(|m| m.message.role == Role::Tool && (m.turn == turn || m.idx >= state.start_idx))
    {
        let meta = EntryMeta::from_value(&m.meta);
        // A helper's answer is no evidence; the page it read is.
        if meta.tool.as_deref() != Some(ASK_PAGE) {
            read.add(&m.message.text());
        }
        if let Some(handle) = m.handle.as_deref()
            && crate::results::valid_handle(handle)
            && stored.insert(handle)
            && let Ok((_, text)) = core.results.get(handle)
        {
            read.add(&text);
        }
    }
    read
}

/// Whether `m` is oxbot's note asking to check an answer.
fn is_check(m: &StoredMessage) -> bool {
    EntryMeta::from_value(&m.meta).kind == EntryKind::Check
}

/// Whether turn `turn` already sent an answer back.
fn has_check(conv: &Conv, turn: u32) -> bool {
    conv.messages
        .iter()
        .any(|m| m.turn == turn && m.message.role == Role::User && is_check(m))
}

/// The answer that was sent back in turn `turn`: the assistant message
/// just before the note.
fn draft(conv: &Conv, turn: u32) -> Option<String> {
    let at = conv
        .messages
        .iter()
        .position(|m| m.turn == turn && m.message.role == Role::User && is_check(m))?;
    conv.messages[..at]
        .iter()
        .rev()
        .find(|m| m.message.role == Role::Assistant)
        .map(|m| m.message.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_quotes_the_task_and_names_the_values() {
        assert_eq!(
            note(
                "[Jay] the 5 tallest finished towers",
                &["351.8".into(), "13".into()],
                true
            ),
            "[oxbot] Before you answer, check it. The task was: «[Jay] the 5 tallest finished \
             towers». These values aren't in anything you read in this task: 351.8, 13. Check \
             them (find_in_page, read_section or query_rows) or remove them. Check each part the \
             task asked for (every filter, the number of items, each field). Then give your \
             final answer."
        );
        assert_eq!(
            note(" [Sam] sum the file ", &[], false),
            "[oxbot] Before you answer, check it. The task was: «[Sam] sum the file». Check each \
             part the task asked for (every filter, the number of items, each field). Then give \
             your final answer."
        );
        assert!(
            note("[Sam] go", &["12".into()], false).contains("(read_result or the file tools)")
        );
        let long = "x".repeat(700);
        let n = note(&long, &[], true);
        assert!(n.contains(&format!("«{}…»", "x".repeat(600))), "{n}");
    }
}
