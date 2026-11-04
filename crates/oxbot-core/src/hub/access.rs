//! Who may use what: the bot list as each viewer sees it, and making,
//! renaming, hiding and deleting bots.

use std::collections::{HashMap, HashSet};
use std::sync::PoisonError;

use oxbot_protocol::{BotEvent, BotInfo, BrowserState, ServerMsg};

use super::{Hub, HubError, MAX_NAME, lock};
use crate::bot::{Bot, Sharing, Viewer};
use crate::store::Store;

/// The most people one bot may be shared with, besides its maker.
pub const MAX_MEMBERS: usize = 20;

/// `bots` with the people each is shared with, from the database.
pub(super) fn with_members(mut bots: Vec<Bot>, store: &Store) -> Vec<Bot> {
    match store.bot_members() {
        Ok(mut all) => {
            for bot in &mut bots {
                bot.members = all.remove(&bot.id).unwrap_or_default();
            }
        }
        // Nobody picked rather than the wrong people: such bots are just
        // their maker's until it reads.
        Err(err) => tracing::warn!(%err, "couldn't read who bots are shared with"),
    }
    bots
}
use crate::id::{BotId, PersonId};
use crate::store::{HistoryPage, Replay, now_ms};

/// Who made `bot`, by name as it is now: its owner's name, else the name
/// it was made with.
pub(super) fn maker_name(bot: &Bot, names: &HashMap<PersonId, String>) -> Option<String> {
    bot.owner
        .as_ref()
        .and_then(|o| names.get(o).cloned())
        .or_else(|| bot.created_by.clone())
}

impl Hub {
    /// A person's Herder as its engine runs it: with the Farm's Herder's
    /// name and instructions.
    pub(super) fn as_herder(&self, bot: &Bot) -> Bot {
        let mut bot = bot.clone();
        if !bot.is_main_herder()
            && bot.is_central()
            && let Some(main) = self
                .bots
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&BotId::new(crate::bot::DEFAULT_BOT).unwrap_or_else(|_| unreachable!()))
        {
            bot.instructions.clone_from(&main.instructions);
            bot.name.clone_from(&main.name);
        }
        bot
    }

    /// Every bot, with whether it's busy, its latest event number and the
    /// browser's state.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub async fn bots(&self) -> Result<Vec<BotInfo>, HubError> {
        let list: Vec<Bot> = self
            .bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        let busy: HashSet<BotId> = self
            .runtimes
            .lock()
            .await
            .iter()
            .filter(|(_, r)| r.busy)
            .map(|(id, _)| id.clone())
            .collect();
        let seqs = self.seqs.for_bots(&self.store, list.iter().map(|b| &b.id));
        let names = self.person_names();
        list.into_iter()
            .map(|b| {
                let seq = seqs.get(&b.id).copied().unwrap_or_default();
                let mut info = BotInfo::default();
                self.fill_engine(&b, &mut info);
                Ok(BotInfo {
                    busy: busy.contains(&b.id),
                    last_seq: seq.last,
                    last_reply_seq: seq.last_reply,
                    central: b.is_central(),
                    browser_state: b
                        .browser_owner()
                        .map_or(BrowserState::Off, |owner| self.owner_state(&owner)),
                    created_by: maker_name(&b, &names),
                    private: b.private,
                    sharing: b.effective_sharing(),
                    members: b
                        .member_ids()
                        .iter()
                        .filter_map(|id| crate::id::PersonId::new(id).ok())
                        .filter_map(|id| names.get(&id).cloned())
                        .collect(),
                    member_ids: b.member_ids(),
                    owner: b.owner.as_ref().map(ToString::to_string),
                    id: b.id.to_string(),
                    name: b.name,
                    ..info
                })
            })
            .collect()
    }

    /// The ids of the bots `viewer` may open, without building the list.
    #[must_use]
    pub fn visible_ids(&self, viewer: &Viewer) -> HashSet<String> {
        let viewer = &*self.resolve(viewer);
        self.bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|b| self.opens(b, viewer))
            .map(|b| b.id.to_string())
            .collect()
    }

    /// Whether `viewer` (already resolved) may open `bot`: as
    /// [`Bot::visible_to`] says, and the Farm's Herder only on the Farm or
    /// for the Farm's person. Everyone else talks to their own Herder.
    pub(super) fn opens(&self, bot: &Bot, viewer: &Viewer) -> bool {
        bot.visible_to(viewer) && (!bot.is_main_herder() || self.is_farm_side(viewer))
    }

    /// Whether `viewer` is the Farm or the Farm's person (by id).
    pub(super) fn is_farm_side(&self, viewer: &Viewer) -> bool {
        viewer.farm
            || viewer
                .id
                .as_ref()
                .is_some_and(|id| self.farm_id().as_ref() == Some(id))
    }

    /// Every bot as `viewer` gets it: a private bot they can't open is
    /// listed (name, maker, lock) with nothing of what it's doing, and each
    /// says whether they may manage it.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub async fn bots_for(&self, viewer: &Viewer) -> Result<Vec<BotInfo>, HubError> {
        let viewer = self.resolve(viewer).into_owned();
        Ok(viewer.redact(self.bots().await?))
    }

    /// The bot `id`, if `viewer` may see it. A bot they can't see answers
    /// as if it didn't exist.
    ///
    /// # Errors
    /// [`HubError::UnknownBot`] if there's no such bot or it's hidden.
    pub fn bot_for(&self, id: &str, viewer: &Viewer) -> Result<Bot, HubError> {
        let viewer = &*self.resolve(viewer);
        self.bot(id)
            .ok()
            .filter(|b| self.opens(b, viewer))
            .ok_or_else(|| HubError::UnknownBot(id.to_owned()))
    }

    /// The bot `id`, if `viewer` may change it.
    pub(super) fn bot_to_manage(&self, id: &str, viewer: &Viewer) -> Result<Bot, HubError> {
        let bot = self.bot_for(id, viewer)?;
        if !bot.manageable_by(viewer) {
            return Err(HubError::NotAllowed(
                match maker_name(&bot, &self.person_names()) {
                    Some(who) => format!("Only {who} or the owner can change {}.", bot.name),
                    None => format!("Only the owner can change {}.", bot.name),
                },
            ));
        }
        Ok(bot)
    }

    /// Applies `change` to bot `id` as it is now and saves it, all under the
    /// bots lock, so two changes (a rename and a privacy change) can't
    /// overwrite each other.
    pub(super) fn update_bot(
        &self,
        id: &BotId,
        change: impl FnOnce(&mut Bot),
    ) -> Result<Bot, HubError> {
        let mut bots = self.bots.write().unwrap_or_else(PoisonError::into_inner);
        let mut bot = bots
            .get(id)
            .cloned()
            .ok_or_else(|| HubError::UnknownBot(id.to_string()))?;
        change(&mut bot);
        bot.save()?;
        bots.insert(id.clone(), bot.clone());
        Ok(bot)
    }

    /// Makes a bot private (only its owner and the Farm see it, and it
    /// uses its owner's own browser) or shared with everyone again. A bot
    /// nobody owns becomes `viewer`'s when it goes private.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's Herder, it's working, or
    /// its file can't be written.
    pub async fn set_private(
        &self,
        id: &str,
        private: bool,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let sharing = if private {
            Sharing::Me
        } else {
            Sharing::Everyone
        };
        self.set_sharing(id, sharing, &[], viewer).await
    }

    /// Chooses who a bot is shared with: everyone in the home, just its
    /// owner, or its owner and `people` (by name, exactly; at most
    /// [`MAX_MEMBERS`]). Shared with anyone but everyone, it uses its
    /// owner's own browser and their sign-ins. A bot nobody owns becomes
    /// `viewer`'s when it stops being everyone's.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's an Assistant, a name isn't
    /// someone oxbot knows, nobody is picked, it's working, or its file
    /// can't be written.
    pub async fn set_sharing(
        &self,
        id: &str,
        sharing: Sharing,
        people: &[String],
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "The Assistant is shared by everyone.".to_owned(),
            ));
        }
        if sharing != Sharing::Everyone && bot.owner.is_none() && viewer.id.is_none() {
            return Err(HubError::NotAllowed(
                "A private bot needs to know whose it is.".to_owned(),
            ));
        }
        let owner = bot.owner.clone().or_else(|| viewer.id.clone());
        let members = if sharing == Sharing::People {
            self.picked_people(people, owner.as_ref())?
        } else {
            Vec::new()
        };
        if bot.effective_sharing() == sharing && bot.members == members {
            return Ok(());
        }
        // Held until its session is stopped, so a waiting message can't start
        // it with the old browser.
        self.claim(&bot.id).await?;
        let browser_changes = bot.private != (sharing != Sharing::Everyone);
        let changed = self
            .store
            .set_bot_members(&bot.id, &members)
            .map_err(HubError::from)
            .and_then(|()| {
                self.update_bot(&bot.id, |b| {
                    if sharing != Sharing::Everyone && b.owner.is_none() {
                        b.owner.clone_from(&viewer.id);
                        b.created_by = Some(viewer.name.clone());
                    }
                    b.set_sharing(sharing, members.clone());
                })
            });
        if changed.is_ok() {
            tracing::info!(bot = %bot.id, ?sharing, people = members.len(), "changed who can see it");
            // Its session was started with the other browser: start it again next time.
            if browser_changes {
                self.stop_session(&bot.id).await;
            }
        }
        lock(&self.starting).remove(&bot.id);
        changed?;
        self.announce_bots().await;
        Ok(())
    }

    /// The people picked to share a bot with, by their exact names: known
    /// people only, never its owner (who always may), each once.
    fn picked_people(
        &self,
        names: &[String],
        owner: Option<&PersonId>,
    ) -> Result<Vec<PersonId>, HubError> {
        let mut picked = Vec::new();
        for name in names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            let person = self.store.person_by_name(name)?.ok_or_else(|| {
                HubError::NotAllowed(format!("oxbot doesn't know anyone called {name}."))
            })?;
            if Some(&person.id) != owner && !picked.contains(&person.id) {
                picked.push(person.id);
            }
        }
        if picked.is_empty() {
            return Err(HubError::NotAllowed(
                "Pick at least one person to share it with, or choose Just me.".to_owned(),
            ));
        }
        if picked.len() > MAX_MEMBERS {
            return Err(HubError::NotAllowed(format!(
                "A bot can be shared with at most {MAX_MEMBERS} people."
            )));
        }
        Ok(picked)
    }

    /// Gives a bot a new name.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's Herder, the name is empty,
    /// too long, on more than one line or taken, or its file can't be
    /// written.
    pub async fn rename_bot(&self, id: &str, name: &str, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "The Assistant keeps its name.".to_owned(),
            ));
        }
        let name = name.trim();
        if name.is_empty() {
            return Err(HubError::NotAllowed("A bot needs a name.".to_owned()));
        }
        if name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
            return Err(HubError::NotAllowed(format!(
                "A bot's name is one line of at most {MAX_NAME} characters."
            )));
        }
        let taken = self
            .bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .any(|b| b.id != bot.id && b.name.eq_ignore_ascii_case(name))
            || name.eq_ignore_ascii_case(crate::bot::CENTRAL_NAME);
        if taken {
            return Err(HubError::NotAllowed(format!(
                "There's already a bot called {name}."
            )));
        }
        self.update_bot(&bot.id, |b| name.clone_into(&mut b.name))?;
        self.announce_bots().await;
        Ok(())
    }

    /// Deletes a bot: stops it, saves its history as `history.jsonl` (and
    /// its reports as `reports.jsonl`) in its folder, forgets them, and
    /// moves the folder to oxbot's trash.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's Herder, it's working, or
    /// its history or folder can't be saved or moved.
    pub async fn delete_bot(&self, id: &str, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "The Assistant can't be deleted.".to_owned(),
            ));
        }
        // No message may start it while it's being deleted.
        self.claim(&bot.id).await?;
        let result = self.delete_claimed(&bot, viewer).await;
        lock(&self.starting).remove(&bot.id);
        let filed_by = result?;
        self.announce_bots().await;
        // Whoever had it in a section gets their sections without it.
        for person in &filed_by {
            self.announce_sections(person);
        }
        Ok(())
    }

    /// Deletes a claimed bot; returns the people who had it in a section.
    async fn delete_claimed(&self, bot: &Bot, viewer: &Viewer) -> Result<Vec<PersonId>, HubError> {
        self.deny_open_approvals(&bot.id);
        self.cancel_sign_ins(&bot.id);
        self.stop_session(&bot.id).await;
        self.forget_scheduled_run(&bot.id);
        // The trash keeps the whole bot, history included.
        let history: String = self
            .store
            .history(&bot.id, 0)?
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .map(|line| line + "\n")
            .collect();
        let path = bot.dir.join("history.jsonl");
        std::fs::write(&path, history).map_err(|source| crate::bot::BotError::Io {
            path: path.clone(),
            source,
        })?;
        // Its reports too, as the bot wrote them.
        let reports: String = self
            .store
            .all_reports(&bot.id)?
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "created_ms": r.created_ms,
                    "question": r.question,
                    "title": r.title,
                    "summary": r.summary,
                    "markdown": r.markdown,
                    "sources": r.sources,
                })
                .to_string()
                    + "\n"
            })
            .collect();
        if !reports.is_empty() {
            let path = bot.dir.join("reports.jsonl");
            std::fs::write(&path, reports).map_err(|source| crate::bot::BotError::Io {
                path: path.clone(),
                source,
            })?;
        }
        // Out of the list before its history goes, so a card answered late
        // (deleting said no to it) finds no bot and records nothing.
        let listed = self
            .bots
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&bot.id);
        let put_back = || {
            if let Some(listed) = listed.clone() {
                self.bots
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(bot.id.clone(), listed);
            }
        };
        // Forget it in the database first: if that fails, nothing has moved
        // and its id can't be reused with old history behind it.
        let filed_by = match self.store.delete_bot(&bot.id) {
            Ok(filed_by) => filed_by,
            Err(err) => {
                put_back();
                return Err(err.into());
            }
        };
        // A new bot can get the same id: it starts from 0.
        self.seqs.remove(&bot.id);
        let trash = self
            .config
            .bots_dir
            .parent()
            .map_or_else(
                || self.config.bots_dir.join(".."),
                std::path::Path::to_path_buf,
            )
            .join("trash");
        if let Err(err) = bot.move_to(&trash, now_ms()) {
            put_back();
            return Err(err.into());
        }
        let by = viewer
            .id
            .as_ref()
            .map_or_else(|| "the Farm".to_owned(), ToString::to_string);
        tracing::info!(bot = %bot.id, by, "deleted a bot; its folder is in the trash");
        Ok(filed_by)
    }

    pub(super) async fn announce_bots(&self) {
        if let Ok(bots) = self.bots().await {
            let _ = self.updates.send(ServerMsg::Bots { bots });
        }
    }

    /// A bot's stored events after `since`.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist or the database fails.
    pub fn history(&self, bot: &str, since: u64) -> Result<Vec<BotEvent>, HubError> {
        let bot = self.bot(bot)?;
        Ok(self.store.history(&bot.id, since)?)
    }

    /// Like [`Hub::history`], for a bot `viewer` may see.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist, is hidden, or the database fails.
    pub fn history_for(
        &self,
        bot: &str,
        since: u64,
        viewer: &Viewer,
    ) -> Result<Vec<BotEvent>, HubError> {
        let bot = self.bot_for(bot, viewer)?;
        Ok(self.store.history(&bot.id, since)?)
    }

    /// A page of a bot `viewer` may see: at most `limit` events just before
    /// event `before`, or its latest with `None`.
    ///
    /// # Errors
    /// Fails if the bot doesn't exist, is hidden, or the database fails.
    pub fn history_page_for(
        &self,
        bot: &str,
        before: Option<u64>,
        limit: usize,
        viewer: &Viewer,
    ) -> Result<HistoryPage, HubError> {
        let bot = self.bot_for(bot, viewer)?;
        Ok(self.store.history_page(&bot.id, before, limit)?)
    }

    /// What a client that has a bot's events up to `since` needs to catch
    /// up, for a bot `viewer` may see: the rest, if there are at most
    /// `limit`, or else the latest page (see [`crate::Store::replay`]).
    ///
    /// # Errors
    /// Fails if the bot doesn't exist, is hidden, or the database fails.
    pub fn replay_for(
        &self,
        bot: &str,
        since: u64,
        limit: usize,
        viewer: &Viewer,
    ) -> Result<Replay, HubError> {
        let bot = self.bot_for(bot, viewer)?;
        Ok(self.store.replay(&bot.id, since, limit)?)
    }

    /// Creates a bot, picking its id from `name`, and tells every client.
    /// `created_by` names its maker (made a person if new, see
    /// [`Hub::person_named`]). For `oxbot chat` and tests: connections use
    /// [`Hub::create_bot_as`].
    ///
    /// # Errors
    /// Fails if its files can't be written.
    pub async fn create_bot(
        &self,
        name: &str,
        instructions: &str,
        created_by: Option<&str>,
    ) -> Result<BotInfo, HubError> {
        self.create_bot_with(name, instructions, created_by, false)
            .await
    }

    /// Like [`Hub::create_bot`]; a `private` bot (which needs a creator) is
    /// seen only by its creator and the Farm.
    ///
    /// # Errors
    /// Fails if its files can't be written.
    pub async fn create_bot_with(
        &self,
        name: &str,
        instructions: &str,
        created_by: Option<&str>,
        private: bool,
    ) -> Result<BotInfo, HubError> {
        let (maker, new) = match created_by.map(str::trim).filter(|c| !c.is_empty()) {
            Some(name) => {
                let (person, new) = self.person_named_quietly(name)?;
                (Some(Viewer::person(person.id, person.name, false)), new)
            }
            None => (None, false),
        };
        let made = self
            .create_bot_as(name, instructions, maker.as_ref(), private)
            .await;
        // After the new bot list, as before people were told about.
        if new {
            self.announce_people();
        }
        made
    }

    /// Creates a bot owned by `maker` (a viewer with a person id; `None`
    /// for nobody). A `private` bot needs a maker.
    ///
    /// # Errors
    /// Fails if its files can't be written.
    pub async fn create_bot_as(
        &self,
        name: &str,
        instructions: &str,
        maker: Option<&Viewer>,
        private: bool,
    ) -> Result<BotInfo, HubError> {
        // One line, at most MAX_NAME characters.
        let name: String = name
            .split(char::is_control)
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_NAME)
            .collect();
        let name = if name.is_empty() {
            "New bot".to_owned()
        } else {
            name
        };
        let id = {
            let bots = self.bots.read().unwrap_or_else(PoisonError::into_inner);
            BotId::from_name(&name, |id| {
                bots.contains_key(id) || self.config.bots_dir.join(id.as_str()).exists()
            })
        };
        let owner = maker.and_then(|m| m.id.as_ref());
        let created_by = maker
            .filter(|m| m.id.is_some())
            .map(|m| m.name.as_str())
            .filter(|n| !n.is_empty());
        let mut bot = Bot::create_owned(
            &self.config.bots_dir,
            id.clone(),
            &name,
            instructions.trim(),
            owner,
            created_by,
        )?;
        if private && owner.is_some() {
            bot.set_sharing(Sharing::Me, Vec::new());
            bot.save()?;
        }
        self.bots
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(bot.id.clone(), bot);
        self.announce_bots().await;
        self.bots()
            .await?
            .into_iter()
            .find(|b| b.id == id.as_str())
            .ok_or_else(|| HubError::UnknownBot(id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::HubConfig;
    use crate::hub::answers;
    use crate::id::PersonId;
    use crate::store::Store;
    use oxbot_protocol::{Approval, ApprovalKind, SignInRequest};

    /// Jay (the Farm's person) has a bot, Garden; Sam and Ann are people.
    async fn hub(
        db: &std::path::Path,
        dir: &std::path::Path,
    ) -> (Arc<Hub>, Viewer, Viewer, Viewer) {
        let hub = Hub::new(
            crate::bot::load_all(dir).unwrap_or_default(),
            Store::open(db).unwrap(),
            HubConfig::new(dir),
        );
        let jay = hub.farm_person("Jay").unwrap();
        let sam = hub.person_named("Sam").unwrap();
        let ann = hub.person_named("Ann").unwrap();
        if hub.bot("garden").is_err() {
            let maker = Viewer::person(jay.id.clone(), "Jay", false);
            hub.create_bot_as("Garden", "", Some(&maker), false)
                .await
                .unwrap();
        }
        (
            hub,
            Viewer::person(jay.id, "Jay", false),
            Viewer::person(sam.id, "Sam", false),
            Viewer::person(ann.id, "Ann", false),
        )
    }

    fn listed<'a>(list: &'a [BotInfo], id: &str) -> &'a BotInfo {
        list.iter().find(|b| b.id == id).unwrap()
    }

    #[tokio::test]
    async fn picked_people_use_a_bot_and_only_its_maker_and_the_owner_change_it() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, sam, ann) = hub(&tmp.path().join("oxbot.db"), tmp.path()).await;
        hub.set_sharing("garden", Sharing::People, &["Sam".into()], &jay)
            .await
            .unwrap();
        let bot = hub.bot("garden").unwrap();
        assert_eq!(
            (bot.effective_sharing(), bot.private),
            (Sharing::People, true)
        );
        assert_eq!(
            bot.browser_owner(),
            bot.owner.clone().map(crate::id::BrowserOwner::Person),
            "the maker's sign-ins"
        );

        // Sam sees it, its chat, and may give it tasks (here no AI model is
        // set up, so it says that, not that it's hidden).
        assert!(hub.bot_for("garden", &sam).is_ok());
        assert!(hub.history_for("garden", 0, &sam).is_ok());
        assert!(matches!(
            hub.may_send("garden", &sam),
            Err(HubError::EngineNotReady(_))
        ));
        assert!(hub.visible_ids(&sam).contains("garden"));
        let for_sam = hub.bots_for(&sam).await.unwrap();
        let g = listed(&for_sam, "garden");
        assert!(!g.locked && g.can_task && !g.can_manage);
        assert_eq!(
            (g.sharing, g.members.as_slice()),
            (Sharing::People, &["Sam".to_owned()][..])
        );
        // ... but never changes it.
        assert!(hub.rename_bot("garden", "Mine", &sam).await.is_err());
        assert!(
            hub.set_sharing("garden", Sharing::Everyone, &[], &sam)
                .await
                .is_err()
        );
        assert!(hub.delete_bot("garden", &sam).await.is_err());

        // Ann doesn't, and isn't told who it's shared with.
        assert!(hub.bot_for("garden", &ann).is_err());
        assert!(matches!(
            hub.may_send("garden", &ann),
            Err(HubError::UnknownBot(_))
        ));
        let for_ann = hub.bots_for(&ann).await.unwrap();
        let g = listed(&for_ann, "garden");
        assert!(g.locked && !g.can_task && g.members.is_empty());

        // Its maker and the owner always may.
        assert!(hub.bot_for("garden", &jay).is_ok() && hub.bot_to_manage("garden", &jay).is_ok());
        assert!(hub.bot_for("garden", &hub.farm_viewer("Jay")).is_ok());

        // Back to just Jay: Sam is out.
        hub.set_private("garden", true, &jay).await.unwrap();
        assert_eq!(hub.bot("garden").unwrap().effective_sharing(), Sharing::Me);
        assert!(hub.bot_for("garden", &sam).is_err());
        assert!(
            !hub.store
                .bot_members()
                .unwrap()
                .contains_key(&BotId::new("garden").unwrap())
        );
    }

    #[tokio::test]
    async fn only_people_oxbot_knows_are_picked_and_the_maker_isnt_one_of_them() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, _, _) = hub(&tmp.path().join("oxbot.db"), tmp.path()).await;
        for (names, words) in [
            (vec!["Ghost".to_owned()], "doesn't know anyone called Ghost"),
            (vec![], "at least one person"),
            (vec!["Jay".to_owned()], "at least one person"),
            (vec!["sam".to_owned()], "doesn't know anyone called sam"),
        ] {
            let err = hub
                .set_sharing("garden", Sharing::People, &names, &jay)
                .await
                .unwrap_err();
            assert!(err.to_string().contains(words), "{names:?}: {err}");
        }
        hub.set_sharing(
            "garden",
            Sharing::People,
            &["Sam".into(), "Sam".into(), "Jay".into(), "Ann".into()],
            &jay,
        )
        .await
        .unwrap();
        assert_eq!(
            hub.bot("garden").unwrap().members.len(),
            2,
            "each once, never the maker"
        );
        let herder = hub.herder_for("Sam").unwrap();
        assert!(
            hub.set_sharing(
                herder.as_str(),
                Sharing::People,
                &["Ann".into()],
                &hub.farm_viewer("Jay")
            )
            .await
            .is_err(),
            "an Assistant is never shared"
        );
    }

    #[tokio::test]
    async fn who_its_shared_with_survives_a_restart() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("oxbot.db");
        let bots = tmp.path().join("bots");
        std::fs::create_dir_all(&bots).unwrap();
        let (hub, jay, sam, _) = hub(&db, &bots).await;
        hub.set_sharing("garden", Sharing::People, &["Sam".into()], &jay)
            .await
            .unwrap();
        hub.shutdown().await;
        drop(hub);
        let (hub, _, _, ann) = hub_again(&db, &bots).await;
        assert!(hub.bot_for("garden", &sam).is_ok());
        assert!(hub.bot_for("garden", &ann).is_err());
        let text = std::fs::read_to_string(bots.join("garden/bot.toml")).unwrap();
        assert!(
            text.contains("sharing = \"people\"") && text.contains("private = true"),
            "{text}"
        );
    }

    async fn hub_again(
        db: &std::path::Path,
        dir: &std::path::Path,
    ) -> (Arc<Hub>, Viewer, Viewer, Viewer) {
        hub(db, dir).await
    }

    #[tokio::test]
    async fn picked_people_answer_its_cards_but_only_its_maker_signs_its_browser_in() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, sam, ann) = hub(&tmp.path().join("oxbot.db"), tmp.path()).await;
        hub.set_sharing("garden", Sharing::People, &["Sam".into()], &jay)
            .await
            .unwrap();
        let bot = hub.bot("garden").unwrap();
        let mut card = Approval {
            id: 1,
            bot: "garden".into(),
            kind: ApprovalKind::Change,
            action: "click".into(),
            target: "Post".into(),
            site: "x.example".into(),
            page: None,
            detail: None,
            screenshot: None,
            asker: Some("Ann".into()),
            asker_id: ann.id.as_ref().map(ToString::to_string),
            always: false,
            rule: None,
            schedule: None,
            also: Vec::new(),
        };
        assert!(!answers(&card, &sam));
        card.also = bot
            .owner
            .iter()
            .map(ToString::to_string)
            .chain(bot.member_ids())
            .collect();
        assert!(answers(&card, &sam) && answers(&card, &jay));
        assert!(!answers(
            &card,
            &Viewer::person(PersonId::random(), "Sam", false)
        ));
        let sign_in = SignInRequest {
            id: 1,
            bot: "garden".into(),
            url: "https://x.example/login".into(),
            site: "x.example".into(),
            why: String::new(),
            window_open: false,
            only: bot.owner.as_ref().map(ToString::to_string),
        };
        assert!(super::super::signs_in(&sign_in, &jay));
        assert!(
            !super::super::signs_in(&sign_in, &sam),
            "Sam never signs Jay's browser in"
        );
        assert!(super::super::signs_in(&sign_in, &hub.farm_viewer("Jay")));
    }

    #[tokio::test]
    async fn the_assistant_lists_a_shared_bot_to_its_people_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let (hub, jay, sam, ann) = hub(&tmp.path().join("oxbot.db"), tmp.path()).await;
        hub.set_sharing("garden", Sharing::People, &["Sam".into()], &jay)
            .await
            .unwrap();
        let sams = hub.herder_for("Sam").unwrap();
        let anns = hub.herder_for("Ann").unwrap();
        let (list, _) = hub.team(&sams, &sam).await;
        let line = list.lines().find(|l| l.contains("(id garden)")).unwrap();
        assert!(
            line.contains("shared by Jay with picked people, the person asking among them"),
            "{line}"
        );
        let (list, _) = hub.team(&anns, &ann).await;
        let line = list.lines().find(|l| l.contains("(id garden)")).unwrap();
        assert!(line.contains("locked"), "{line}");
        assert!(hub.locked_message("garden", &ann).is_some());
        assert!(hub.locked_message("garden", &sam).is_none());
    }
}
