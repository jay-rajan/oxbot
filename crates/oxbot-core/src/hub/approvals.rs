//! Approval cards ("red cards"): deciding Claude's permission requests by
//! policy, asking the person the turn acts for, and who may answer.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use oxbot_browser::MCP_SERVER_NAME;
use oxbot_claude::{Decision, PermissionRequest};
use oxbot_protocol::{Answer, Approval, ApprovalKind, EventKind, Outcome, ServerMsg};
use serde_json::Value;
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
        let card = lock(&self.approvals)
            .get(&id)
            .map(|(a, asker, _)| (a.bot.clone(), asker.clone()));
        let viewer = self.resolve(viewer);
        card.is_some_and(|(bot, asker)| {
            let theirs = asker.is_some() && viewer.id == asker;
            (viewer.farm || theirs) && self.bot_for(&bot, &viewer).is_ok()
        })
    }

    /// `viewer` answered card `id`. "Always allow" also saves the card's
    /// rule first, and only if the card offers it to them (see
    /// [`Hub::can_always`]); otherwise the card stays open. Returns whether
    /// the card was open and theirs to answer.
    ///
    /// # Errors
    /// [`HubError::NotAllowed`] if "Always allow" isn't theirs to give, or
    /// the rule couldn't be saved.
    pub fn answer_as(&self, id: u64, answer: Answer, viewer: &Viewer) -> Result<bool, HubError> {
        if !self.approval_visible(id, viewer) {
            return Ok(false);
        }
        if answer == Answer::AlwaysAllow {
            // The card as oxbot stored it, not as the client describes it.
            let card = lock(&self.approvals).get(&id).map(|(a, _, _)| a.clone());
            let Some(card) = card else {
                return Ok(false);
            };
            self.add_rule_from(&card, viewer)?;
        }
        Ok(self.answer(id, answer))
    }

    /// The user answered an approval card. Returns whether it was still open.
    pub fn answer(&self, id: u64, answer: Answer) -> bool {
        let entry = lock(&self.approvals).remove(&id);
        let Some((_, _, tx)) = entry else {
            return false;
        };
        let _ = tx.send(answer);
        let _ = self.updates.send(ServerMsg::ApprovalResolved { id });
        true
    }

    /// Approval cards still waiting for the user.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<Approval> {
        lock(&self.approvals)
            .values()
            .map(|(a, _, _)| a.clone())
            .collect()
    }

    /// Denies every open card for `bot` (its turn ended or was cancelled).
    pub(super) fn deny_open_approvals(&self, bot: &BotId) {
        let ids: Vec<u64> = lock(&self.approvals)
            .iter()
            .filter(|(_, (a, _, _))| a.bot == bot.as_str())
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

        // The labels of the elements this request names (the uid it clicks,
        // the fields it fills).
        let (page, labels) = {
            let runtimes = self.runtimes.lock().await;
            let r = runtimes.get(bot_id);
            let labels: HashMap<String, String> = r
                .map(|r| {
                    request_uids(&request.input)
                        .filter_map(|uid| {
                            Some((uid.to_owned(), r.elements.describe(uid)?.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            (r.and_then(|r| r.page.clone()), labels)
        };
        let page_url = page.as_ref().map(|(_, url)| url.as_str());
        let browser = self.browser_of(&bot);
        let facts = match &browser {
            Some(shared) if policy::needs_facts(&request.tool) => shared
                .browser
                .inspect(page_url, &policy::facts_script(&request.input))
                .await
                .map(|v| PageFacts::from_value(&v)),
            _ => None,
        };
        let element = match (
            &browser,
            click_label(&request.tool, &request.input, &labels),
        ) {
            (Some(shared), Some(label)) if policy::wants_element_facts(label) => {
                let (role, name) = policy::role_and_name(label);
                let found = shared.browser.describe(page_url, role, name).await;
                policy::strictest(label, &found).cloned()
            }
            _ => None,
        };
        let tab = browser.as_ref().map(|shared| Arc::clone(&shared.browser));
        let ctx = policy::Context {
            work_dir: &bot.work_dir(),
            page_url,
            facts: facts.as_ref(),
            element: element.as_ref(),
        };
        let verdict = policy::decide(&request.tool, &request.input, ctx, |uid| {
            labels.get(uid).cloned()
        });
        match verdict {
            Verdict::Allow => Ok(Decision::Allow),
            Verdict::AllowUnnamed(what) => {
                // A missing label never blocks browsing; the log shows how
                // often it happens.
                let site = page_url.and_then(oxbot_browser::web_host);
                tracing::info!(
                    bot = %bot_id,
                    target = %what,
                    site = site.as_deref().unwrap_or("unknown"),
                    "click on an element oxbot couldn't name: allowed"
                );
                Ok(Decision::Allow)
            }
            Verdict::Deny(message) => {
                self.record(
                    bot_id,
                    EventKind::Blocked {
                        message: format!("oxbot refused: {message}"),
                    },
                )?;
                Ok(Decision::Deny(message))
            }
            Verdict::Ask(ask) => {
                if self
                    .allowed_by_rule(&bot, &ask, tab.as_deref(), page_url)
                    .await?
                {
                    return Ok(Decision::Allow);
                }
                self.ask_user(&bot, ask, page).await
            }
        }
    }

    /// Shows the user a card for `ask` and turns their answer into Claude's.
    async fn ask_user(
        &self,
        bot: &Bot,
        ask: policy::Ask,
        page: Option<(String, String)>,
    ) -> Result<Decision, HubError> {
        let ask = self.with_maker(bot, ask).await;
        let outcome = self.confirm(bot, &ask, page).await?;
        Ok(match outcome {
            Outcome::Allowed | Outcome::AllowedAlways => Decision::Allow,
            Outcome::TimedOut => Decision::Deny(format!(
                "The user didn't answer within {} minutes, so oxbot said no. Tell them what you \
                 wanted to do, and stop.",
                self.config.approval_timeout.as_secs() / 60
            )),
            Outcome::Denied => Decision::Deny(format!(
                "The user said no to: {} {}. Don't try another way; tell them what you wanted to \
                 do, and stop.",
                ask.action, ask.target
            )),
        })
    }

    /// A create-bot card says who the bot will be for: the person whose
    /// turn asked for it (private), or everyone (shared).
    async fn with_maker(&self, bot: &Bot, mut ask: policy::Ask) -> policy::Ask {
        if ask.kind != ApprovalKind::CreateBot {
            return ask;
        }
        let asker = self
            .runtimes
            .lock()
            .await
            .get(&bot.id)
            .and_then(|r| r.asker.clone());
        let name = asker.as_ref().and_then(|id| self.person_name(id));
        ask.action = match (ask.private, name) {
            (true, Some(name)) => format!("create a private bot for {name}"),
            (true, None) => "create a private bot".to_owned(),
            (false, _) => "create a shared bot".to_owned(),
        };
        ask
    }

    /// Shows the person `bot`'s turn acts for (and the Farm) a card for
    /// `ask`, waits for their answer, and records it in `bot`'s chat. No
    /// answer within `approval_timeout` is a no.
    ///
    /// # Errors
    /// Fails if the answer can't be recorded.
    pub(super) async fn confirm(
        &self,
        bot: &Bot,
        ask: &policy::Ask,
        page: Option<(String, String)>,
    ) -> Result<Outcome, HubError> {
        let bot_id = &bot.id;
        let screenshot = match self.browser_of(bot) {
            // A new bot or a task for another bot has nothing to do with
            // the page on screen.
            _ if matches!(ask.kind, ApprovalKind::CreateBot | ApprovalKind::GiveTask) => None,
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
            // Their name, for showing; who gets it is decided by id.
            asker: asker.as_ref().and_then(|id| self.person_name(id)),
            asker_id: asker.as_ref().map(ToString::to_string),
            // Who's offered Always is decided for each client.
            always: false,
            // Herder's cards are never remembered.
            rule: ask
                .rule
                .clone()
                .filter(|_| ask.kind == ApprovalKind::Change && !bot.is_central()),
        };
        let (tx, rx) = oneshot::channel();
        lock(&self.approvals).insert(id, (approval.clone(), asker, tx));
        let _ = self.updates.send(ServerMsg::ApprovalNeeded(approval));
        let _ = self.updates.send(ServerMsg::Activity {
            bot: bot_id.to_string(),
            text: "Waiting for your OK…".to_owned(),
        });
        tracing::info!(bot = %bot_id, kind = ?ask.kind, "asking the user");

        let outcome = match tokio::time::timeout(self.config.approval_timeout, rx).await {
            Ok(Ok(Answer::AllowOnce)) => Outcome::Allowed,
            Ok(Ok(Answer::AlwaysAllow)) => Outcome::AllowedAlways,
            Ok(Ok(Answer::Deny) | Err(_)) => Outcome::Denied,
            Err(_) => {
                self.answer(id, Answer::Deny);
                Outcome::TimedOut
            }
        };
        // Deleting a bot says no to its cards; by the time this one wakes,
        // the bot and its history may be gone. Nothing to add it to then.
        if self.bot(bot_id.as_str()).is_err() {
            return Ok(outcome);
        }
        self.record(
            bot_id,
            EventKind::Approval {
                action: ask.action.clone(),
                target: ask.target.clone(),
                site: ask.site.clone(),
                outcome,
            },
        )?;
        Ok(outcome)
    }
}

/// The element uids a browser request names: `uid`, and each of
/// `fill_form`'s `elements`.
fn request_uids(input: &Value) -> impl Iterator<Item = &str> {
    let one = input.get("uid").and_then(Value::as_str);
    let many = input
        .get("elements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|e| e.get("uid").and_then(Value::as_str));
    one.into_iter().chain(many)
}

/// The label of the element a `click` aims at, if oxbot knows it.
fn click_label<'a>(
    tool: &str,
    input: &Value,
    labels: &'a HashMap<String, String>,
) -> Option<&'a str> {
    if tool != format!("mcp__{MCP_SERVER_NAME}__click") {
        return None;
    }
    let uid = input.get("uid").and_then(Value::as_str)?;
    labels.get(uid).map(String::as_str)
}

/// Whether `viewer` should be shown `card`: the Farm, or the person the
/// card's turn acts for, by person id (see [`Viewer::is_person`]; answering
/// is checked again in [`Hub::approval_visible`]). A card nobody asked for
/// (no person) is the Farm's.
#[must_use]
pub fn answers(card: &Approval, viewer: &Viewer) -> bool {
    viewer.farm || viewer.is_person(card.asker_id.as_deref(), card.asker.as_deref())
}
