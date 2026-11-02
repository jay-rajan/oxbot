//! Who may use what: the bot list as each viewer sees it, and making,
//! renaming, hiding and deleting bots.

use std::collections::{HashMap, HashSet};
use std::sync::PoisonError;

use oxbot_protocol::{BotEvent, BotInfo, BrowserState, ServerMsg};

use super::{Hub, HubError, MAX_NAME, lock};
use crate::bot::{Bot, Viewer};
use crate::id::{BotId, PersonId};
use crate::store::{HistoryPage, now_ms};

/// Who made `bot`, by name as it is now: its owner's name, else the name
/// it was made with.
pub(super) fn maker_name(bot: &Bot, names: &HashMap<PersonId, String>) -> Option<String> {
    bot.owner
        .as_ref()
        .and_then(|o| names.get(o).cloned())
        .or_else(|| bot.created_by.clone())
}

impl Hub {
    /// A person's Herder as Claude runs it: with the Farm's Herder's
    /// instructions and model.
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
            bot.model.clone_from(&main.model);
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
        let seqs = lock(&self.seqs).clone();
        let names = self.person_names();
        list.into_iter()
            .map(|b| {
                let seq = seqs.get(&b.id).copied().unwrap_or_default();
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
                    owner: b.owner.as_ref().map(ToString::to_string),
                    id: b.id.to_string(),
                    name: b.name,
                    ..BotInfo::default()
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
            .filter(|b| b.visible_to(viewer))
            .map(|b| b.id.to_string())
            .collect()
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
            .filter(|b| b.visible_to(viewer))
            .ok_or_else(|| HubError::UnknownBot(id.to_owned()))
    }

    /// The bot `id`, if `viewer` may change it.
    fn bot_to_manage(&self, id: &str, viewer: &Viewer) -> Result<Bot, HubError> {
        let bot = self.bot_for(id, viewer)?;
        if !bot.manageable_by(viewer) {
            let who = maker_name(&bot, &self.person_names())
                .unwrap_or_else(|| "the Farm's owner".to_owned());
            return Err(HubError::NotAllowed(format!(
                "Only {who} or the Farm can change {}.",
                bot.name
            )));
        }
        Ok(bot)
    }

    /// Applies `change` to bot `id` as it is now and saves it, all under the
    /// bots lock, so two changes (a rename and a privacy change) can't
    /// overwrite each other.
    fn update_bot(&self, id: &BotId, change: impl FnOnce(&mut Bot)) -> Result<Bot, HubError> {
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
    /// uses its owner's own browser) or shared again. A bot nobody owns
    /// becomes `viewer`'s when it goes private.
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
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "Herder is shared by everyone.".to_owned(),
            ));
        }
        if bot.private == private {
            return Ok(());
        }
        if private && bot.owner.is_none() && viewer.id.is_none() {
            return Err(HubError::NotAllowed(
                "A private bot needs to know whose it is.".to_owned(),
            ));
        }
        // Held until its Claude is stopped, so a waiting message can't start
        // it with the old browser.
        self.claim(&bot.id).await?;
        let changed = self.update_bot(&bot.id, |b| {
            if private && b.owner.is_none() {
                b.owner.clone_from(&viewer.id);
                b.created_by = Some(viewer.name.clone());
            }
            b.private = private;
        });
        if changed.is_ok() {
            tracing::info!(bot = %bot.id, private, "changed who can see it");
            // Its Claude was started with the other browser: start it again next time.
            self.stop_claude(&bot.id).await;
        }
        lock(&self.starting).remove(&bot.id);
        changed?;
        self.announce_bots().await;
        Ok(())
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
            return Err(HubError::NotAllowed("Herder keeps its name.".to_owned()));
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

    /// Deletes a bot: stops it, saves its history as `history.jsonl` in its
    /// folder, forgets the history, and moves the folder to oxbot's trash.
    ///
    /// # Errors
    /// Fails if `viewer` may not change it, it's Herder, it's working, or
    /// its history or folder can't be saved or moved.
    pub async fn delete_bot(&self, id: &str, viewer: &Viewer) -> Result<(), HubError> {
        let viewer = &*self.resolve(viewer);
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed("Herder can't be deleted.".to_owned()));
        }
        // No message may start it while it's being deleted.
        self.claim(&bot.id).await?;
        let result = self.delete_claimed(&bot, viewer).await;
        lock(&self.starting).remove(&bot.id);
        result?;
        self.announce_bots().await;
        Ok(())
    }

    async fn delete_claimed(&self, bot: &Bot, viewer: &Viewer) -> Result<(), HubError> {
        self.deny_open_approvals(&bot.id);
        self.cancel_sign_ins(&bot.id);
        self.stop_claude(&bot.id).await;
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
        // Forget it in the database first: if that fails, nothing has moved
        // and its id can't be reused with old history behind it.
        self.store.delete_bot(&bot.id)?;
        // A new bot can get the same id: it starts from 0.
        lock(&self.seqs).remove(&bot.id);
        let trash = self
            .config
            .bots_dir
            .parent()
            .map_or_else(
                || self.config.bots_dir.join(".."),
                std::path::Path::to_path_buf,
            )
            .join("trash");
        let moved = bot.move_to(&trash, now_ms())?;
        self.bots
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&bot.id);
        let by = viewer
            .id
            .as_ref()
            .map_or_else(|| "the Farm".to_owned(), ToString::to_string);
        tracing::info!(bot = %bot.id, by, to = %moved.display(), "deleted a bot");
        Ok(())
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

    /// Creates a bot, picking its id from `name`, and tells every client.
    /// `created_by` names its maker (made a person if new).
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
        let maker = created_by
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(|c| self.resolve(&Viewer::gate(c)).into_owned())
            .filter(|v| v.id.is_some());
        self.create_bot_as(name, instructions, maker.as_ref(), private)
            .await
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
            bot.private = true;
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
