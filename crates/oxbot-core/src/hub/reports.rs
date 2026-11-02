//! Research reports in the hub: the `save_report` tool, and opening,
//! listing and deleting reports for the people who may.
//!
//! A report belongs to its bot: whoever may open the bot (as its chat)
//! may open its reports, now, by the bot's rules as they are at the time.
//! Deleting one is for those who may manage the bot.

use std::sync::Arc;

use oxbot_protocol::{ReportInfo, ReportView, ServerMsg};
use serde_json::Value;

use super::{Hub, HubError};
use crate::bot::Viewer;
use crate::id::BotId;
use crate::report::{self, Report};

/// What a viewer hears about a report that isn't there (deleted, or never
/// this bot's).
const DELETED: &str = "That report was deleted.";

impl Hub {
    /// `save_report` for `bot`'s Claude: checks the report against the
    /// pages its session opened, then saves it with its card in the chat.
    /// Returns whether it worked and what to tell the bot.
    pub(super) async fn save_report(self: &Arc<Self>, bot: &BotId, args: &Value) -> (bool, String) {
        let (asker, question, checked) = {
            let runtimes = self.runtimes.lock().await;
            let Some(runtime) = runtimes.get(bot) else {
                return (
                    false,
                    "Not saved: oxbot lost track of this turn.".to_owned(),
                );
            };
            (
                runtime.asker.clone(),
                runtime.question.clone().unwrap_or_default(),
                report::check(args, &runtime.visited),
            )
        };
        let draft = match checked {
            Ok(draft) => draft,
            Err(why) => return (false, why),
        };
        let mut report = Report {
            id: report::new_id(),
            created_ms: 0,
            asked_by: asker,
            question,
            title: draft.title,
            summary: draft.summary,
            markdown: draft.markdown,
            sources: draft.sources,
            links: draft.links,
        };
        match self.store.add_report(bot, &mut report) {
            Ok(event) => {
                self.note_seq(&event);
                let _ = self.updates.send(ServerMsg::Event(event));
                tracing::info!(%bot, report = report.id, sources = report.sources.len(), "saved a report");
                (
                    true,
                    format!(
                        "Saved \"{}\". The user sees a card for it in this chat. Now reply in 2 to 4 \
                         lines with the answer in brief; don't repeat the report.",
                        report.title
                    ),
                )
            }
            Err(err) => (false, format!("Not saved: {err}")),
        }
    }

    /// Bot `id`'s report `report` as a page, for `viewer`, with dates at
    /// `utc_offset_min` minutes east of UTC. Nothing is read before the
    /// viewer is known to open the bot.
    ///
    /// # Errors
    /// [`HubError::UnknownBot`] if the bot doesn't exist or is hidden from
    /// them; [`HubError::NotAllowed`] if there's no such report.
    pub fn report_for(
        &self,
        id: &str,
        report: &str,
        viewer: &Viewer,
        utc_offset_min: i32,
    ) -> Result<ReportView, HubError> {
        let bot = self.bot_for(id, viewer)?;
        let found = if report::is_id(report) {
            self.store.report(&bot.id, report)?
        } else {
            None
        };
        let found = found.ok_or_else(|| HubError::NotAllowed(DELETED.to_owned()))?;
        let name = self.as_herder(&bot).name;
        Ok(ReportView {
            bot: bot.id.to_string(),
            id: found.id.clone(),
            title: found.title.clone(),
            created_ms: found.created_ms,
            html: report::render(&found, &name, utc_offset_min),
        })
    }

    /// Bot `id`'s reports, newest first, for `viewer`.
    ///
    /// # Errors
    /// [`HubError::UnknownBot`] if the bot doesn't exist or is hidden from
    /// them, or the database fails.
    pub fn reports_for(&self, id: &str, viewer: &Viewer) -> Result<Vec<ReportInfo>, HubError> {
        let bot = self.bot_for(id, viewer)?;
        Ok(self.store.reports(&bot.id)?)
    }

    /// Deletes bot `id`'s report `report` (its card stays and says so).
    /// Returns the reports left.
    ///
    /// # Errors
    /// Fails if `viewer` may not manage the bot, or the database fails.
    pub fn delete_report(
        &self,
        id: &str,
        report: &str,
        viewer: &Viewer,
    ) -> Result<Vec<ReportInfo>, HubError> {
        let viewer = self.resolve(viewer);
        let bot = self.bot_to_manage(id, &viewer)?;
        if self.store.delete_report(&bot.id, report)? {
            tracing::info!(bot = %bot.id, report, "deleted a report");
        }
        Ok(self.store.reports(&bot.id)?)
    }

    /// Titles of the reports `bot` saved after its event `since`, oldest
    /// first (for Herder, after a task).
    pub(super) fn reports_since(&self, bot: &BotId, since: u64) -> Vec<String> {
        self.store
            .report_titles_since(bot, since)
            .unwrap_or_else(|err| {
                tracing::warn!(%bot, %err, "couldn't read a bot's new reports");
                Vec::new()
            })
    }
}
