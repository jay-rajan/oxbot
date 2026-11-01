//! Approval cards ("red cards"): deciding Claude's permission requests by
//! policy, asking the person the turn acts for, and who may answer.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use oxbot_browser::MCP_SERVER_NAME;
use oxbot_claude::{Decision, PermissionRequest};
use oxbot_protocol::{Answer, Approval, ApprovalKind, EventKind, Outcome, ServerMsg};
use tokio::sync::oneshot;

use super::{Hub, HubError, lock};
use crate::bot::{Bot, Viewer};
use crate::id::BotId;
use crate::policy::{self, PageFacts, Verdict};
use crate::tools::{SIGN_IN_TOOL, TOOL_SERVER_NAME};

impl Hub {
    /// Whether `viewer` may see (and answer) approval card `id`: the Farm,
    /// or the person the card's turn acts for.
    #[must_use]
    pub fn approval_visible(&self, id: u64, viewer: &Viewer) -> bool {
        let card = lock(&self.approvals).get(&id).map(|(a, _)| a.clone());
        card.is_some_and(|a| answers(&a, viewer) && self.bot_for(&a.bot, viewer).is_ok())
    }

    /// The user answered an approval card. Returns whether it was still open.
    pub fn answer(&self, id: u64, answer: Answer) -> bool {
        let entry = lock(&self.approvals).remove(&id);
        let Some((_, tx)) = entry else { return false };
        let _ = tx.send(answer);
        let _ = self.updates.send(ServerMsg::ApprovalResolved { id });
        true
    }

    /// Approval cards still waiting for the user.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<Approval> {
        lock(&self.approvals)
            .values()
            .map(|(a, _)| a.clone())
            .collect()
    }

    /// Denies every open card for `bot` (its turn ended or was cancelled).
    pub(super) fn deny_open_approvals(&self, bot: &BotId) {
        let ids: Vec<u64> = lock(&self.approvals)
            .iter()
            .filter(|(_, (a, _))| a.bot == bot.as_str())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.answer(id, Answer::Deny);
        }
    }

    /// Claude asks whether `bot` may run a tool: decide, maybe ask the user,
    /// then answer Claude.
    pub(super) async fn permission(self: Arc<Self>, bot: BotId, request: PermissionRequest) {
        let decision = match self.decide_permission(&bot, &request).await {
            Ok(decision) => decision,
            Err(err) => Decision::Deny(format!("oxbot couldn't check this: {err}")),
        };
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(&bot)
            && let Err(err) = runtime
                .process
                .answer(&request.request_id, &request.input, &decision)
                .await
        {
            tracing::warn!(%bot, %err, "couldn't answer Claude's permission request");
        }
    }

    async fn decide_permission(
        self: &Arc<Self>,
        bot_id: &BotId,
        request: &PermissionRequest,
    ) -> Result<Decision, HubError> {
        let bot = self.bot(bot_id.as_str())?;
        let uses_browser = request
            .tool
            .starts_with(&format!("mcp__{MCP_SERVER_NAME}__"))
            || request.tool == format!("mcp__{TOOL_SERVER_NAME}__{SIGN_IN_TOOL}");
        if uses_browser && !self.take_browser(bot_id).await? {
            return Ok(Decision::Deny(
                "Your turn ended while you waited for the browser.".to_owned(),
            ));
        }

        let (page, elements) = {
            let runtimes = self.runtimes.lock().await;
            let r = runtimes.get(bot_id);
            (
                r.and_then(|r| r.page.clone()),
                r.map(|r| r.elements.clone()).unwrap_or_default(),
            )
        };
        let page_url = page.as_ref().map(|(_, url)| url.as_str());
        let facts = match self.browser_of(&bot) {
            Some(shared) if policy::needs_facts(&request.tool) => shared
                .browser
                .inspect(page_url, &policy::facts_script(&request.input))
                .await
                .map(|v| PageFacts::from_value(&v)),
            _ => None,
        };
        let ctx = policy::Context {
            work_dir: &bot.work_dir(),
            page_url,
            facts: facts.as_ref(),
        };
        let verdict = policy::decide(&request.tool, &request.input, ctx, |uid| {
            elements.get(uid).cloned()
        });
        match verdict {
            Verdict::Allow => Ok(Decision::Allow),
            Verdict::Deny(message) => {
                self.record(
                    bot_id,
                    EventKind::Blocked {
                        message: format!("oxbot refused: {message}"),
                    },
                )?;
                Ok(Decision::Deny(message))
            }
            Verdict::Ask(ask) => self.ask_user(&bot, ask, page).await,
        }
    }

    /// Shows the user a card for `ask` and turns their answer into Claude's.
    async fn ask_user(
        &self,
        bot: &Bot,
        ask: policy::Ask,
        page: Option<(String, String)>,
    ) -> Result<Decision, HubError> {
        let bot_id = &bot.id;
        let screenshot = match self.browser_of(bot) {
            // A new bot has nothing to do with the page on screen.
            _ if ask.kind == ApprovalKind::CreateBot => None,
            Some(s) => {
                s.browser
                    .screenshot(page.as_ref().map(|(_, u)| u.as_str()))
                    .await
            }
            None => None,
        };
        let asker = self
            .runtimes
            .lock()
            .await
            .get(bot_id)
            .and_then(|r| r.asker.clone());
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let approval = Approval {
            id,
            bot: bot_id.to_string(),
            kind: ask.kind,
            action: ask.action.clone(),
            target: ask.target.clone(),
            site: ask.site.clone(),
            page: page.map(|(title, url)| if title.is_empty() { url } else { title }),
            detail: ask.detail.clone(),
            screenshot,
            asker,
        };
        let (tx, rx) = oneshot::channel();
        lock(&self.approvals).insert(id, (approval.clone(), tx));
        let _ = self.updates.send(ServerMsg::ApprovalNeeded(approval));
        let _ = self.updates.send(ServerMsg::Activity {
            bot: bot_id.to_string(),
            text: "Waiting for your OK…".to_owned(),
        });
        tracing::info!(bot = %bot_id, action = ask.action, site = ask.site, "asking the user");

        let answer = match tokio::time::timeout(self.config.approval_timeout, rx).await {
            Ok(Ok(answer)) => Some(answer),
            Ok(Err(_)) => Some(Answer::Deny),
            Err(_) => {
                self.answer(id, Answer::Deny);
                None
            }
        };
        let (outcome, decision) = match answer {
            None => (
                Outcome::TimedOut,
                Decision::Deny(format!(
                    "The user didn't answer within {} minutes, so oxbot said no. Tell them what you \
                     wanted to do, and stop.",
                    self.config.approval_timeout.as_secs() / 60
                )),
            ),
            Some(Answer::Deny) => (
                Outcome::Denied,
                Decision::Deny(format!(
                    "The user said no to: {} {}. Don't try another way; tell them what you wanted to \
                     do, and stop.",
                    ask.action, ask.target
                )),
            ),
            Some(Answer::AllowOnce) => (Outcome::Allowed, Decision::Allow),
        };
        self.record(
            bot_id,
            EventKind::Approval {
                action: ask.action,
                target: ask.target,
                site: ask.site,
                outcome,
            },
        )?;
        Ok(decision)
    }
}

/// Whether `viewer` may see and answer `card`: the Farm, or the person the
/// card's turn acts for. A card nobody asked for (no person) is the Farm's.
#[must_use]
pub fn answers(card: &Approval, viewer: &Viewer) -> bool {
    viewer.farm
        || card
            .asker
            .as_deref()
            .is_some_and(|a| !a.is_empty() && a == viewer.name)
}
