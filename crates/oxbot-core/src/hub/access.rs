//! Who may use what: each person's Herder, Gates and the people oxbot
//! knows, the bot list as each viewer sees it, and making, renaming,
//! hiding and deleting bots.

use std::collections::HashSet;
use std::sync::PoisonError;

use oxbot_protocol::{BotEvent, BotInfo, BrowserState, GateInfo, Person, ServerMsg};

use super::{Hub, HubError, MAX_NAME, MAX_PERSON, RESERVED_PEOPLE, lock};
use crate::bot::{Bot, Viewer};
use crate::id::BotId;
use crate::store::{HistoryPage, now_ms};

impl Hub {
    /// `person`'s own Herder, made the first time they need it. It shares
    /// the Farm's Herder's instructions and model, but has its own Claude
    /// session and history, so nobody can ask it what someone else asked.
    /// (The Farm's own person uses the Farm's Herder, `assistant`.)
    ///
    /// # Errors
    /// Fails if `person` is empty or its folder can't be written.
    pub fn herder_for(&self, person: &str) -> Result<BotId, HubError> {
        let person = person.trim();
        if person.is_empty() {
            return Err(HubError::NotAllowed(
                "Herder needs to know who it's for.".to_owned(),
            ));
        }
        let id = crate::bot::personal_herder_id(person);
        let mut bots = self.bots.write().unwrap_or_else(PoisonError::into_inner);
        if bots.contains_key(&id) {
            return Ok(id);
        }
        let dir = self.config.bots_dir.join(id.as_str());
        let mut bot = if dir.exists() {
            Bot::load(&dir)?
        } else {
            Bot::create_by(
                &self.config.bots_dir,
                id.clone(),
                crate::bot::CENTRAL_NAME,
                "",
                Some(person),
            )?
        };
        if !bot.private || bot.created_by.as_deref() != Some(person) {
            bot.private = true;
            bot.created_by = Some(person.to_owned());
            bot.save()?;
        }
        tracing::info!(bot = %id, person, "made a Herder for a person");
        bots.insert(id.clone(), bot);
        Ok(id)
    }

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

    /// Who a new Gate is for, made safe: trimmed, at most
    /// [`MAX_PERSON`] characters, no brackets or line breaks (names go into
    /// bots' messages as `[Name]`), not a name oxbot uses itself, and spelled
    /// like a person oxbot already knows if it matches one ignoring case
    /// ("sam" pairs as "Sam").
    ///
    /// # Errors
    /// Says what's wrong with the name, or the database failed.
    pub fn gate_person(&self, name: &str) -> Result<String, HubError> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        let refuse = |why: &str| Err(HubError::NotAllowed(why.to_owned()));
        if name.is_empty() {
            return refuse("Say who the Gate is for.");
        }
        if name.chars().count() > MAX_PERSON {
            return refuse("That name is too long (40 characters at most).");
        }
        if name
            .chars()
            .any(|c| matches!(c, '[' | ']' | '\\') || c.is_control())
        {
            return refuse("Names can't have brackets or backslashes in them.");
        }
        if RESERVED_PEOPLE
            .iter()
            .any(|r| r.eq_ignore_ascii_case(&name))
        {
            return refuse("oxbot uses that name itself. Pick the person's own name.");
        }
        let known = self
            .store
            .people()?
            .into_iter()
            .map(|p| p.name)
            .chain(self.store.gates()?.into_iter().map(|g| g.person));
        for person in known {
            if person.to_lowercase() == name.to_lowercase() {
                return Ok(person);
            }
        }
        Ok(name)
    }

    /// Remembers a newly paired Gate for `person` (see
    /// [`Hub::gate_person`]). `key_hash` is the hash of its key.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn add_gate(
        &self,
        id: &str,
        person: &str,
        device: &str,
        key_hash: &str,
    ) -> Result<GateInfo, HubError> {
        let gate = self.store.add_gate(id, person, device, key_hash)?;
        if self.store.person(person)?.1 {
            let _ = self.updates.send(ServerMsg::People {
                people: self.people()?,
            });
        }
        Ok(gate)
    }

    /// The paired Gate whose key has this hash; marks it as seen now.
    ///
    /// # Errors
    /// Fails if the database does (which is not the same as "not paired").
    pub fn gate_by_key(&self, key_hash: &str) -> Result<Option<GateInfo>, HubError> {
        Ok(self.store.gate_by_key(key_hash)?)
    }

    /// Every paired Gate, oldest first.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn gates(&self) -> Result<Vec<GateInfo>, HubError> {
        Ok(self.store.gates()?)
    }

    /// Forgets a Gate. Returns whether it was paired.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn remove_gate(&self, id: &str) -> Result<bool, HubError> {
        Ok(self.store.remove_gate(id)?)
    }

    /// Everyone who has sent a message, each with their own color.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub fn people(&self) -> Result<Vec<Person>, HubError> {
        Ok(self.store.people()?)
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
                    created_by: b.created_by.clone(),
                    private: b.private,
                    id: b.id.to_string(),
                    name: b.name,
                })
            })
            .collect()
    }

    /// The ids of the bots `viewer` may open, without building the list.
    #[must_use]
    pub fn visible_ids(&self, viewer: &Viewer) -> HashSet<String> {
        self.bots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|b| b.visible_to(viewer))
            .map(|b| b.id.to_string())
            .collect()
    }

    /// Every bot as `viewer` gets it: a private bot they can't open is
    /// listed (name, maker, lock) with nothing of what it's doing.
    ///
    /// # Errors
    /// Fails if the database fails.
    pub async fn bots_for(&self, viewer: &Viewer) -> Result<Vec<BotInfo>, HubError> {
        Ok(viewer.redact(self.bots().await?))
    }

    /// The bot `id`, if `viewer` may see it. A bot they can't see answers
    /// as if it didn't exist.
    ///
    /// # Errors
    /// [`HubError::UnknownBot`] if there's no such bot or it's hidden.
    pub fn bot_for(&self, id: &str, viewer: &Viewer) -> Result<Bot, HubError> {
        self.bot(id)
            .ok()
            .filter(|b| b.visible_to(viewer))
            .ok_or_else(|| HubError::UnknownBot(id.to_owned()))
    }

    /// The bot `id`, if `viewer` may change it.
    fn bot_to_manage(&self, id: &str, viewer: &Viewer) -> Result<Bot, HubError> {
        let bot = self.bot_for(id, viewer)?;
        if !viewer.can_manage(bot.created_by.as_deref()) {
            let who = bot.created_by.as_deref().unwrap_or("the Farm's owner");
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

    /// Makes a bot private (only its creator and the Farm see it, and it
    /// uses its creator's own browser) or shared again. A bot nobody is
    /// recorded as making becomes `viewer`'s when it goes private.
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
        let bot = self.bot_to_manage(id, viewer)?;
        if bot.is_central() {
            return Err(HubError::NotAllowed(
                "Herder is shared by everyone.".to_owned(),
            ));
        }
        if bot.private == private {
            return Ok(());
        }
        if private && bot.created_by.is_none() && viewer.name.trim().is_empty() {
            return Err(HubError::NotAllowed(
                "A private bot needs to know whose it is.".to_owned(),
            ));
        }
        // Held until its Claude is stopped, so a waiting message can't start
        // it with the old browser.
        self.claim(&bot.id).await?;
        let changed = self.update_bot(&bot.id, |b| {
            if private && b.created_by.is_none() {
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
        tracing::info!(bot = %bot.id, by = viewer.name, to = %moved.display(), "deleted a bot");
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
        let created_by = created_by.map(str::trim).filter(|c| !c.is_empty());
        let mut bot = Bot::create_by(
            &self.config.bots_dir,
            id.clone(),
            &name,
            instructions.trim(),
            created_by,
        )?;
        if private && created_by.is_some() {
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
