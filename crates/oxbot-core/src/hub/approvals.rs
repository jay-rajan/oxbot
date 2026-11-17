//! Approval cards ("red cards"): deciding an engine's permission requests
//! by policy, asking the person the turn acts for, and who may answer.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use oxbot_browser::{MCP_SERVER_NAME, UidCheck};
use oxbot_protocol::{Answer, Approval, ApprovalKind, EventKind, Outcome, ScheduleEdit, ServerMsg};
use serde_json::Value;
use tokio::sync::oneshot;

use super::{Hub, HubError, lock};
use crate::bot::{Bot, Viewer};
use crate::engine::trust::HOST_TOOL_PREFIX;
use crate::engine::{Decision, GateMode, PermissionRequest};
use crate::id::BotId;
use crate::policy::{self, PageFacts, Verdict};
use crate::tools::{SIGN_IN_TOOL, TOOL_SERVER_NAME};

impl Hub {
    /// Whether `viewer` may see (and answer) approval card `id`: the home computer,
    /// the person the card's turn acts for, or (on a bot shared with picked
    /// people) its owner and those people.
    #[must_use]
    pub fn approval_visible(&self, id: u64, viewer: &Viewer) -> bool {
        let card = lock(&self.approvals)
            .get(&id)
            .map(|(a, asker, _)| (a.bot.clone(), asker.clone(), a.also.clone()));
        let viewer = self.resolve(viewer);
        card.is_some_and(|(bot, asker, also)| {
            let theirs = asker.is_some() && viewer.id == asker;
            (viewer.home_computer || theirs || viewer.is_member(&also))
                && self.bot_for(&bot, &viewer).is_ok()
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
        self.answer_with(id, answer, None, viewer)
    }

    /// Like [`Hub::answer_as`]; on a schedule card, `edit` is the schedule
    /// as `viewer` changed it before pressing Create. It's checked first:
    /// a rule or task that isn't valid leaves the card open.
    ///
    /// # Errors
    /// As [`Hub::answer_as`], or what's wrong with the changed schedule.
    pub fn answer_with(
        &self,
        id: u64,
        answer: Answer,
        edit: Option<ScheduleEdit>,
        viewer: &Viewer,
    ) -> Result<bool, HubError> {
        if !self.approval_visible(id, viewer) {
            return Ok(false);
        }
        if let Some(edit) = edit {
            let kind = lock(&self.approvals).get(&id).map(|(a, _, _)| a.kind);
            if kind == Some(ApprovalKind::Schedule) && answer == Answer::AllowOnce {
                let edit = super::schedules::checked_edit(&edit)?;
                lock(&self.scheduler.edits).insert(id, edit);
            }
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
            lock(&self.scheduler.edits).remove(&id);
            return false;
        };
        if answer != Answer::AllowOnce {
            lock(&self.scheduler.edits).remove(&id);
        }
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

    /// Says no to every open card for `bot` because its person sent a
    /// message instead (see [`Hub::send_for`]): the bot is told to read it.
    pub(super) fn deny_for_message(&self, bot: &BotId) {
        let ids: Vec<u64> = lock(&self.approvals)
            .iter()
            .filter(|(_, (a, _, _))| a.bot == bot.as_str())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            lock(&self.answered_by_message).insert(id);
            if !self.answer(id, Answer::Deny) {
                lock(&self.answered_by_message).remove(&id);
            }
        }
    }

    /// Denies every open card for `bot` (its turn ended or was cancelled).
    pub(super) fn deny_open_approvals(&self, bot: &BotId) {
        let ids: Vec<(u64, bool)> = lock(&self.approvals)
            .iter()
            .filter(|(_, (a, _, _))| a.bot == bot.as_str())
            .map(|(id, (a, _, _))| (*id, is_notice(a.kind)))
            .collect();
        for (id, notice) in ids {
            // A notice closed by oxbot: nobody pressed Stop.
            if notice {
                lock(&self.notices_closed).insert(id);
            }
            if !self.answer(id, Answer::Deny) && notice {
                lock(&self.notices_closed).remove(&id);
            }
        }
    }

    /// `bot`'s engine asks whether it may run a tool: decide, maybe ask the
    /// user, then answer the engine.
    pub(super) async fn permission(self: Arc<Self>, bot: BotId, request: PermissionRequest) {
        let mode = self
            .runtimes
            .lock()
            .await
            .get(&bot)
            .map_or(GateMode::Live, |r| r.gate_mode);
        // In a practice run oxbot's own tools run (the hub answers them
        // with what it records), so the bot gets a normal result.
        // File tools inside the bot's own folder run. A
        // practice run's folder is its own copy,
        // deleted when it ends, so its files are written as live.
        let inside = self
            .run_bot(bot.as_str())
            .is_ok_and(|b| policy::file_call_inside(&request.tool, &request.input, &b.work_dir()));
        let decision =
            if inside || (mode == GateMode::DryRun && request.tool.starts_with(HOST_TOOL_PREFIX)) {
                Decision::Allow
            } else {
                match self.decide_permission(&bot, &request, mode).await {
                    Ok(decision) => decision,
                    Err(err) => Decision::Deny(format!("oxbot couldn't check this: {err}")),
                }
            };
        request.reply.send(decision);
    }

    /// The gate's decision for `request`. In a practice run (`mode`) a
    /// tool that would need the person's OK is only recorded: no card, and
    /// no "Always allow" rule is used.
    async fn decide_permission(
        self: &Arc<Self>,
        bot_id: &BotId,
        request: &PermissionRequest,
        mode: GateMode,
    ) -> Result<Decision, HubError> {
        let bot = self.run_bot(bot_id.as_str())?;
        let uses_browser = request
            .tool
            .starts_with(&format!("mcp__{MCP_SERVER_NAME}__"))
            || request.tool == format!("mcp__{TOOL_SERVER_NAME}__{SIGN_IN_TOOL}");
        // Fail closed: a practice run browses only behind its guard.
        if mode == GateMode::DryRun
            && let Some(no) = self.trial_browser_refusal(bot_id, &request.tool).await
        {
            return Ok(no);
        }
        if let Some(no) = self.plan_first_refusal(bot_id, uses_browser, mode).await {
            return Ok(no);
        }
        if uses_browser && !self.take_browser(bot_id).await? {
            return Ok(Decision::Deny(
                "Your turn ended while you waited for the browser.".to_owned(),
            ));
        }

        let (page, mut labels) = self.labels_of(bot_id, &request.input).await;
        let page_url = page.as_ref().map(|(_, url)| url.as_str());
        if let Some(no) = self.steps_gate(bot_id, request, mode, page_url)? {
            return Ok(no);
        }
        let browser = self.browser_of(&bot);
        if let Some(shared) = &browser
            && let Some(no) = self
                .check_label_live(&shared.browser, request, page_url, &mut labels)
                .await
        {
            return Ok(Decision::Deny(no));
        }
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
        let account = policy::call_account_action(&request.tool, &request.input, ctx, |uid| {
            labels.get(uid).cloned()
        });
        if mode == GateMode::DryRun {
            let label = click_label(&request.tool, &request.input, &labels);
            return self.trial_gate(&bot, request, &verdict, page_url, label, element.as_ref());
        }
        match verdict {
            Verdict::Allow => {
                let decision = self
                    .run_allowed(&bot, account, page_url, request.quiet)
                    .await?;
                if matches!(decision, Decision::Allow) {
                    self.pace_visit(&request.tool, &request.input, page_url)
                        .await;
                }
                Ok(decision)
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
                let decision = self
                    .ask_verdict(&bot, ask, request, tab.as_deref(), page.clone())
                    .await?;
                if matches!(decision, Decision::Allow) {
                    self.pace_visit(&request.tool, &request.input, page_url)
                        .await;
                }
                Ok(decision)
            }
        }
    }

    /// Waits so a browser call keeps to the site's pace
    /// ([`super::pace`]): the site is the page a load goes to, or the
    /// page the bot is on.
    async fn pace_visit(&self, tool: &str, input: &Value, page_url: Option<&str>) {
        let Some(step) = super::pace::Step::of(tool) else {
            return;
        };
        // read_sites loads several sites at once: each keeps its pace
        // (waiting until each may be visited, so in all about as long as
        // the longest wait).
        if let Some(urls) = input.get("urls").and_then(Value::as_array) {
            let sites: std::collections::BTreeSet<String> = urls
                .iter()
                .filter_map(Value::as_str)
                .filter_map(oxbot_browser::web_host)
                .map(|h| oxbot_browser::site_of(&h))
                .collect();
            for site in &sites {
                self.pacer.wait(site, step, self.config.pace).await;
            }
            return;
        }
        let target = match step {
            super::pace::Step::Load => input.get("url").and_then(Value::as_str),
            super::pace::Step::Act => page_url,
        };
        let Some(host) = target.and_then(oxbot_browser::web_host) else {
            return;
        };
        self.pacer
            .wait(&oxbot_browser::site_of(&host), step, self.config.pace)
            .await;
    }

    /// The page `bot_id` is on, and the labels of the elements `input`
    /// names (the uid it clicks, the fields it fills), from its element
    /// book.
    async fn labels_of(
        &self,
        bot_id: &BotId,
        input: &Value,
    ) -> (Option<(String, String)>, HashMap<String, String>) {
        let runtimes = self.runtimes.lock().await;
        let r = runtimes.get(bot_id);
        let labels: HashMap<String, String> = r
            .map(|r| {
                request_uids(input)
                    .filter_map(|uid| Some((uid.to_owned(), r.elements.describe(uid)?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        (r.and_then(|r| r.page.clone()), labels)
    }

    /// A call the policy lets run. One that acts for the account (adding
    /// to a cart) runs only once the browser is signed in to the site, or
    /// it lands in a guest cart only oxbot's Chrome sees (a card checks the
    /// same in [`Hub::ask_verdict`]).
    async fn run_allowed(
        &self,
        bot: &Bot,
        account: Option<policy::AccountAction>,
        page_url: Option<&str>,
        quiet: bool,
    ) -> Result<Decision, HubError> {
        if let Some(action) = account
            && let Some(no) = self.sign_in_before(bot, action, page_url, quiet).await
        {
            return Ok(no);
        }
        Ok(Decision::Allow)
    }

    /// Checks the element a browser call names before oxbot judges it.
    /// Its label came from text the browser tool returned, which a page
    /// (or the bot's own typing) can shape, so oxbot asks the browser what
    /// the uid really is: one from an earlier snapshot of a page that has
    /// changed is refused (the call would fail; the bot takes a fresh
    /// snapshot), and a label that isn't the element's own role and name,
    /// or that the live page no longer shows, is dropped, so the click asks
    /// as one oxbot can't name. If oxbot can't ask about the uid, a click
    /// on a label that would run is checked by name on the page instead.
    /// Not in the end-to-end test mode, whose stand-in browser tool makes
    /// up its snapshots.
    async fn check_label_live(
        &self,
        browser: &oxbot_browser::Browser,
        request: &PermissionRequest,
        page_url: Option<&str>,
        labels: &mut HashMap<String, String>,
    ) -> Option<String> {
        let (tool, input) = (request.tool.as_str(), &request.input);
        if self.config.e2e {
            return None;
        }
        if tool.starts_with(&format!("mcp__{MCP_SERVER_NAME}__"))
            && let Some(uid) = input.get("uid").and_then(Value::as_str)
        {
            match browser.uid_check(uid).await {
                Some(UidCheck::Gone) => {
                    tracing::info!(uid, "a uid from an earlier snapshot: refused");
                    return Some(STALE_UID.to_owned());
                }
                Some(UidCheck::Live { role, name, shown }) => {
                    // An element oxbot didn't learn from a snapshot: the
                    // page names it, so the click is judged like any other.
                    if shown && !labels.contains_key(uid) {
                        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
                        labels.insert(
                            uid.to_owned(),
                            format!("{role} \"{}\"", name.replace('"', "'")),
                        );
                        return None;
                    }
                    if let Some(label) = labels.get(uid) {
                        let (label_role, label_name) = policy::role_and_name(label);
                        if !shown || label_role != role || !same_name(label_name, &name) {
                            tracing::info!(target = %label, "the page doesn't show what the snapshot named: asking");
                            labels.remove(uid);
                        }
                    }
                    return None;
                }
                None => {}
            }
        }
        let (Some(label), Some(url)) = (click_label(tool, input, labels), page_url) else {
            return None;
        };
        if !matches!(
            policy::classify_at(Some(label), None, page_url),
            policy::Click::Plain | policy::Click::Query
        ) && !policy::wants_element_facts(label)
        {
            return None;
        }
        let (role, name) = policy::role_and_name(label);
        let (role, name) = (role.to_owned(), name.to_owned());
        if browser.count_named(url, &role, &name).await == Some(0) {
            tracing::info!(target = %label, "the page doesn't show what the snapshot named: asking");
            if let Some(uid) = input.get("uid").and_then(Value::as_str) {
                labels.remove(uid);
            }
        }
        None
    }

    /// Several steps at once: anywhere but websites with fine-grained
    /// control. `Some` refusal for a `page_steps` call there.
    fn steps_gate(
        &self,
        bot_id: &BotId,
        request: &PermissionRequest,
        mode: GateMode,
        page_url: Option<&str>,
    ) -> Result<Option<Decision>, HubError> {
        if request.tool != crate::tools::full_name(policy::PAGE_STEPS_TOOL)
            || mode != GateMode::Live
        {
            return Ok(None);
        }
        let Some(no) = self.steps_not_allowed(page_url) else {
            return Ok(None);
        };
        self.record(
            bot_id,
            EventKind::Blocked {
                message: format!("oxbot refused: {no}"),
            },
        )?;
        Ok(Some(Decision::Deny(no)))
    }

    /// A call the policy says needs the person's OK: a program's card; for
    /// an account action on a site the browser isn't signed in to, the
    /// sign-in card first (see [`Hub::sign_in_before`]); an Always rule, or
    /// (asked quietly, one step of a batch) what would be asked; else the
    /// card.
    async fn ask_verdict(
        &self,
        bot: &Bot,
        ask: policy::Ask,
        request: &PermissionRequest,
        tab: Option<&oxbot_browser::Browser>,
        page: Option<(String, String)>,
    ) -> Result<Decision, HubError> {
        let page_url = page.as_ref().map(|(_, url)| url.as_str());
        if ask.kind == ApprovalKind::Script {
            if request.quiet {
                return Ok(Decision::WouldAsk(format!(
                    "{} {} folder",
                    ask.action,
                    policy::possessive(&bot.name)
                )));
            }
            let program = oxbot_loop::tools::program::card_text(&request.input);
            return self.script_permission(bot, &program).await;
        }
        // Acting for the person's account (a cart, an order, a booking):
        // signed in to the site first, even before an Always rule, or it
        // lands in a guest cart only oxbot's Chrome sees.
        if let Some(action) = ask.account
            && let Some(no) = self
                .sign_in_before(bot, action, page_url, request.quiet)
                .await
        {
            return Ok(no);
        }
        // A click oxbot can't name, on a site the bot isn't signed in to:
        // nothing there is the person's (no account, no money), so it
        // runs without a card. Signed in, it still asks.
        if ask.click == Some(policy::Click::Unknown) && !self.signed_in_at(bot, page_url).await {
            return Ok(Decision::Allow);
        }
        let mut ask = ask;
        if let Some(read) = self.rule_held_back(bot, &ask).await? {
            // The card says why an Always rule didn't apply.
            let note = format!(
                "It has read {read} in this conversation, so oxbot asks even if you said Always."
            );
            ask.detail = Some(match ask.detail.take() {
                Some(detail) => format!("{detail}\n\n({note})"),
                None => note,
            });
        } else if self.allowed_by_rule(bot, &ask, tab, page_url).await? {
            return Ok(Decision::Allow);
        }
        // One step of a batch: it stops here, and the bot asks for this
        // step on its own.
        if request.quiet {
            return Ok(Decision::WouldAsk(format!(
                "{} {} on {}",
                ask.action, ask.target, ask.site
            )));
        }
        self.ask_user(bot, ask, page).await
    }

    /// Shows the user a card for `ask` and turns their answer into the engine's.
    pub(super) async fn ask_user(
        &self,
        bot: &Bot,
        ask: policy::Ask,
        page: Option<(String, String)>,
    ) -> Result<Decision, HubError> {
        let ask = self.with_maker(bot, ask).await;
        let (outcome, id) = self.confirm_card(bot, &ask, page).await?;
        if lock(&self.answered_by_message).remove(&id) {
            return Ok(Decision::Deny(format!(
                "The user didn't allow {} {}: they sent a new message instead. Read it and do \
                 what it says.",
                ask.action, ask.target
            )));
        }
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
    /// turn asked for it (private, the owner only), or everyone (shared).
    async fn with_maker(&self, bot: &Bot, mut ask: policy::Ask) -> policy::Ask {
        if ask.kind != ApprovalKind::CreateBot {
            return ask;
        }
        let asker = self
            .runtimes
            .lock()
            .await
            .get(&bot.id)
            .and_then(|r| r.origin.asker().cloned());
        let name = asker.as_ref().and_then(|id| self.person_name(id));
        // Only the owner's bot can be private: anyone else's card says
        // shared, as the bot will be.
        if ask.private && !self.may_keep_private(asker.as_ref()) {
            ask.private = false;
        }
        ask.action = match (ask.private, name) {
            (true, Some(name)) => format!("create a private bot for {name}"),
            (true, None) => "create a private bot".to_owned(),
            (false, _) => "create a shared bot".to_owned(),
        };
        ask
    }

    /// Shows the person `bot`'s turn acts for (and the home computer) a card for
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
        Ok(self.confirm_card(bot, ask, page).await?.0)
    }

    /// Like [`Hub::confirm`], also giving the card's id (a schedule card's
    /// changes are kept by it).
    pub(super) async fn confirm_card(
        &self,
        bot: &Bot,
        ask: &policy::Ask,
        page: Option<(String, String)>,
    ) -> Result<(Outcome, u64), HubError> {
        let bot_id = &bot.id;
        let (id, rx) = self.open_card(bot, ask, page).await;
        let _ = self.updates.send(ServerMsg::Activity {
            bot: bot_id.to_string(),
            text: "Waiting for your OK…".to_owned(),
        });
        tracing::info!(bot = %bot_id, kind = ?ask.kind, "asking the user");
        // A schedule's run waits here: its person hears it needs them.
        self.scheduled_run_waiting(bot_id, "is waiting for your OK");

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
            return Ok((outcome, id));
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
        Ok((outcome, id))
    }

    /// Shows `bot`'s card `ask` to the people who may answer it, without
    /// waiting: its id, and where the answer comes.
    pub(super) async fn open_card(
        &self,
        bot: &Bot,
        ask: &policy::Ask,
        page: Option<(String, String)>,
    ) -> (u64, oneshot::Receiver<Answer>) {
        let bot_id = &bot.id;
        let screenshot = match self.browser_of(bot) {
            // A new bot, a task for another bot or a schedule has nothing
            // to do with the page on screen.
            _ if matches!(
                ask.kind,
                ApprovalKind::CreateBot
                    | ApprovalKind::GiveTask
                    | ApprovalKind::Schedule
                    | ApprovalKind::ChangeSchedule
                    | ApprovalKind::Script
                    | ApprovalKind::Plan
                    | ApprovalKind::HomeArea
                    | ApprovalKind::Checkpoint
            ) =>
            {
                None
            }
            Some(s) => {
                s.browser
                    .screenshot(page.as_ref().map(|(_, u)| u.as_str()))
                    .await
            }
            None => None,
        };
        let asker = match &ask.answerer {
            Some(person) => Some(person.clone()),
            None => self
                .runtimes
                .lock()
                .await
                .get(bot_id)
                .and_then(|r| r.origin.asker().cloned()),
        };
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
            // The Assistant's cards are never remembered.
            rule: ask.rule.clone().filter(|_| {
                matches!(ask.kind, ApprovalKind::Change | ApprovalKind::Script) && !bot.is_central()
            }),
            schedule: ask.schedule.as_deref().cloned(),
            // A bot shared with picked people: any of them, and its owner,
            // may answer what it asks.
            also: if bot.effective_sharing() == crate::bot::Sharing::People {
                bot.owner
                    .iter()
                    .map(ToString::to_string)
                    .chain(bot.member_ids())
                    .collect()
            } else {
                Vec::new()
            },
        };
        let (tx, rx) = oneshot::channel();
        lock(&self.approvals).insert(id, (approval.clone(), asker, tx));
        let _ = self.updates.send(ServerMsg::ApprovalNeeded(approval));
        (id, rx)
    }
}

/// A card that tells the person what happens unless they stop it (a
/// plan, a check-in), rather than asking.
pub(super) fn is_notice(kind: ApprovalKind) -> bool {
    matches!(kind, ApprovalKind::Plan | ApprovalKind::Checkpoint)
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
/// What a bot is told when it uses a uid from a snapshot of a page that
/// has changed since.
const STALE_UID: &str = "That element is from an earlier snapshot, and the page has changed \
     since. Take a fresh snapshot and use a uid from it.";

/// Whether a label's name (as the snapshot text carried it: one line, cut
/// to [`MAX_NAME`] characters) is the element's own name.
fn same_name(label: &str, name: &str) -> bool {
    let words = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
    let (label, name) = (words(label), words(name));
    label == name || (label.chars().count() >= MAX_NAME && name.starts_with(&label))
}

/// Names longer than this may be cut in a label.
const MAX_NAME: usize = 900;

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

/// Whether `viewer` should be shown `card`: the home computer, the person the
/// card's turn acts for, or one of the others who may answer it (a bot
/// shared with picked people), by person id (see [`Viewer::is_person`];
/// answering is checked again in [`Hub::approval_visible`]). A card nobody
/// asked for (no person) is the home computer's.
#[must_use]
pub fn answers(card: &Approval, viewer: &Viewer) -> bool {
    viewer.home_computer
        || viewer.is_person(card.asker_id.as_deref(), card.asker.as_deref())
        || viewer.is_member(&card.also)
}
