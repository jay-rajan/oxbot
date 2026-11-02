//! People: who a viewer is (by person id), the Farm's own person, each
//! person's own Herder, Gates, and giving bots from before person ids an
//! owner.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::PoisonError;

use oxbot_protocol::{GateInfo, Person, ServerMsg};

use super::{Hub, HubError, MAX_PERSON, RESERVED_PEOPLE, lock};
use crate::bot::{Bot, CENTRAL_NAME, DEFAULT_BOT, Viewer, is_personal_herder};
use crate::id::{BotId, PersonId};
use crate::store::{PersonRecord, Store};

/// The Farm's Herder's id.
fn main_herder() -> BotId {
    BotId::new(DEFAULT_BOT).unwrap_or_else(|_| unreachable!("a valid id"))
}

/// Gives each bot from before person ids an owner, and saves it in its
/// `bot.toml`: the person its `created_by` names (made if new), or for a
/// shared bot nobody is recorded as making, the Farm's own person (if
/// chosen yet). Herder (`assistant`) belongs to nobody.
pub(super) fn adopt_bots(bots: Vec<Bot>, store: &Store, farm: Option<&PersonRecord>) -> Vec<Bot> {
    bots.into_iter()
        .map(|mut bot| {
            if let Err(err) = adopt(&mut bot, store, farm) {
                tracing::warn!(bot = %bot.id, %err, "couldn't give a bot its owner");
            }
            bot
        })
        .collect()
}

/// Gives `bot` an owner if it has none (see [`adopt_bots`]).
fn adopt(bot: &mut Bot, store: &Store, farm: Option<&PersonRecord>) -> Result<(), HubError> {
    if bot.owner.is_some() || bot.is_main_herder() {
        return Ok(());
    }
    let maker = bot
        .created_by
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let person = match (maker, farm) {
        (Some(name), _) => store.find_or_create_person(name)?.0,
        (None, Some(farm)) if !bot.is_central() => farm.clone(),
        _ => return Ok(()),
    };
    bot.owner = Some(person.id.clone());
    bot.created_by.get_or_insert(person.name);
    bot.save()?;
    tracing::info!(bot = %bot.id, owner = %person.id, "gave a bot its owner");
    Ok(())
}

impl Hub {
    /// `viewer` tied to a person: one with only a name gets the id of the
    /// person called exactly that (made if new), so access is decided by
    /// id from here on. A nameless one stays nobody.
    #[must_use]
    pub fn resolve<'a>(&self, viewer: &'a Viewer) -> Cow<'a, Viewer> {
        if viewer.id.is_some() || viewer.name.trim().is_empty() {
            return Cow::Borrowed(viewer);
        }
        match self.store.find_or_create_person(&viewer.name) {
            Ok((person, _)) => Cow::Owned(Viewer::person(person.id, person.name, viewer.farm)),
            Err(err) => {
                // Nobody rather than matched by name.
                tracing::warn!(%err, "couldn't look up a person");
                Cow::Owned(Viewer {
                    id: None,
                    name: String::new(),
                    farm: viewer.farm,
                })
            }
        }
    }

    /// The person `id` as a viewer who isn't on the Farm (Herder acting for
    /// them); nobody if they're unknown.
    pub(super) fn person_viewer(&self, id: Option<&PersonId>) -> Viewer {
        match id.map(|id| self.store.person_by_id(id)) {
            Some(Ok(Some(person))) => Viewer::person(person.id, person.name, false),
            Some(Err(err)) => {
                tracing::warn!(%err, "couldn't look up a person");
                Viewer::gate("")
            }
            _ => Viewer::gate(""),
        }
    }

    /// A person's name as it is now, if known.
    pub(super) fn person_name(&self, id: &PersonId) -> Option<String> {
        self.store.person_by_id(id).ok().flatten().map(|p| p.name)
    }

    /// Everyone's name as it is now, by id.
    pub(super) fn person_names(&self) -> HashMap<PersonId, String> {
        match self.store.person_records() {
            Ok(people) => people.into_iter().map(|p| (p.id, p.name)).collect(),
            Err(err) => {
                tracing::warn!(%err, "couldn't read people");
                HashMap::new()
            }
        }
    }

    /// The Farm's own person. Chosen once, the first time: the person called
    /// `local_name` (the Mac account's first name), made if new. From then
    /// on it's the same person, found by id; their name follows
    /// `local_name` if nobody else has it. Being the Farm's person is never
    /// what makes a connection the Farm: only the Farm secret does.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn farm_person(&self, local_name: &str) -> Result<PersonRecord, HubError> {
        let mut farm = lock(&self.farm);
        if let Some(id) = farm.as_ref()
            && let Some(person) = self.store.person_by_id(id)?
        {
            return Ok(person);
        }
        let local_name = local_name.trim();
        let local_name = if local_name.is_empty() {
            "Me"
        } else {
            local_name
        };
        let person = match self.store.farm_person()? {
            Some(person) if person.name != local_name => {
                if self.store.rename_person(&person.id, local_name)? {
                    tracing::info!(person = %person.id, "the Farm's person has a new name");
                }
                self.store.person_by_id(&person.id)?.unwrap_or(person)
            }
            Some(person) => person,
            None => {
                let (person, _) = self.store.find_or_create_person(local_name)?;
                self.store.set_farm_person(&person.id)?;
                tracing::info!(person = %person.id, "chose the Farm's person");
                person
            }
        };
        *farm = Some(person.id.clone());
        drop(farm);
        self.adopt_ownerless(&person);
        Ok(person)
    }

    /// The Farm's own window as a viewer: the Farm's person, on the Farm.
    #[must_use]
    pub fn farm_viewer(&self, local_name: &str) -> Viewer {
        match self.farm_person(local_name) {
            Ok(person) => Viewer::person(person.id, person.name, true),
            Err(err) => {
                tracing::warn!(%err, "couldn't find the Farm's person");
                Viewer::farm(local_name)
            }
        }
    }

    /// The Farm's person's id, if chosen.
    pub(super) fn farm_id(&self) -> Option<PersonId> {
        if let Some(id) = lock(&self.farm).clone() {
            return Some(id);
        }
        self.store.farm_person().ok().flatten().map(|p| p.id)
    }

    /// Bots nobody is recorded as making become the Farm's person's.
    fn adopt_ownerless(&self, farm: &PersonRecord) {
        let mut bots = self.bots.write().unwrap_or_else(PoisonError::into_inner);
        for bot in bots.values_mut().filter(|b| b.owner.is_none()) {
            if let Err(err) = adopt(bot, &self.store, Some(farm)) {
                tracing::warn!(bot = %bot.id, %err, "couldn't give a bot its owner");
            }
        }
    }

    /// Whether `herder` is `asker`'s own Herder: their personal one, or the
    /// Farm's (`assistant`) when they are the Farm's person. Compared by
    /// person id.
    pub(super) fn is_own_herder(&self, herder: &BotId, asker: &Viewer) -> bool {
        let Some(person) = asker.id.as_ref() else {
            return false;
        };
        if herder.as_str() == DEFAULT_BOT {
            return self.farm_id().as_ref() == Some(person);
        }
        self.bot(herder.as_str()).is_ok_and(|b| {
            is_personal_herder(b.id.as_str()) && b.private && b.owner.as_ref() == Some(person)
        })
    }

    /// Which Herder `viewer` talks to: the Farm and the Farm's own person
    /// (by id, on the Mac or a phone) use the Farm's; anyone else gets their
    /// own. A name alone never gets the Farm's.
    ///
    /// # Errors
    /// Fails if `viewer` is nobody or their Herder can't be made.
    pub fn herder_of(&self, viewer: &Viewer) -> Result<BotId, HubError> {
        let viewer = self.resolve(viewer);
        if viewer.farm {
            return Ok(main_herder());
        }
        match &viewer.id {
            Some(id) if self.farm_id().as_ref() == Some(id) => Ok(main_herder()),
            Some(id) => self.herder_for_person(id),
            None => Err(HubError::NotAllowed(
                "Herder needs to know who it's for.".to_owned(),
            )),
        }
    }

    /// The Herder of the person called exactly `person` (made if new); see
    /// [`Hub::herder_for_person`].
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
        let (person, _) = self.store.find_or_create_person(person)?;
        self.herder_for_person(&person.id)
    }

    /// Person `id`'s own Herder, made the first time they need it. It shares
    /// the Farm's Herder's instructions and model, but has its own Claude
    /// session and history, so nobody can ask it what someone else asked.
    /// Its id is kept with the person, so it stays theirs if their name
    /// changes. (The Farm's own person uses the Farm's Herder, `assistant`.)
    ///
    /// # Errors
    /// Fails if there's no such person, its folder belongs to someone else,
    /// or it can't be written.
    pub fn herder_for_person(&self, id: &PersonId) -> Result<BotId, HubError> {
        let person = self
            .store
            .person_by_id(id)?
            .ok_or_else(|| HubError::NotAllowed("Herder needs to know who it's for.".to_owned()))?;
        let herder = person.herder.clone();
        let mut bots = self.bots.write().unwrap_or_else(PoisonError::into_inner);
        let known = bots.get(&herder).cloned();
        if let Some(bot) = &known
            && bot.private
            && bot.owner.as_ref() == Some(id)
        {
            return Ok(herder);
        }
        let dir = self.config.bots_dir.join(herder.as_str());
        let mut bot = match known {
            Some(bot) => bot,
            None if dir.exists() => Bot::load(&dir)?,
            None => Bot::create_owned(
                &self.config.bots_dir,
                herder.clone(),
                CENTRAL_NAME,
                "",
                Some(id),
                Some(&person.name),
            )?,
        };
        if bot.owner.as_ref().is_some_and(|owner| owner != id) {
            return Err(HubError::NotAllowed(
                "That Herder belongs to someone else.".to_owned(),
            ));
        }
        if !bot.private || bot.owner.is_none() {
            bot.private = true;
            bot.owner = Some(id.clone());
            bot.created_by = Some(person.name);
            bot.save()?;
        }
        tracing::info!(bot = %herder, person = %id, "made a Herder for a person");
        bots.insert(herder.clone(), bot);
        Ok(herder)
    }

    /// Who a new Gate is for, made safe: trimmed, at most
    /// [`MAX_PERSON`] characters, no brackets or line breaks (names go into
    /// bots' messages as `[Name]`), not a name oxbot uses itself, and spelled
    /// like a person oxbot already knows if it matches exactly one ignoring
    /// case ("sam" pairs as "Sam"). A name that is someone's exactly stays
    /// theirs: "sam" never becomes "Sam" when there is a "sam".
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
        let known: Vec<String> = self.store.people()?.into_iter().map(|p| p.name).collect();
        if known.contains(&name) {
            return Ok(name);
        }
        let lower = name.to_lowercase();
        let mut alike = known.into_iter().filter(|k| k.to_lowercase() == lower);
        match (alike.next(), alike.next()) {
            (Some(one), None) => Ok(one),
            _ => Ok(name),
        }
    }

    /// Remembers a newly paired Gate for the person called exactly `person`
    /// (see [`Hub::gate_person`]; made if new). `key_hash` is the hash of
    /// its key.
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
        let new = self.store.person_by_name(person)?.is_none();
        let gate = self.store.add_gate(id, person, device, key_hash)?;
        if new {
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

    /// Like [`Hub::gate_by_key`], with the Gate's person as a viewer (never
    /// the Farm).
    ///
    /// # Errors
    /// Fails if the database does (which is not the same as "not paired").
    pub fn gate_viewer_by_key(
        &self,
        key_hash: &str,
    ) -> Result<Option<(GateInfo, Viewer)>, HubError> {
        Ok(self
            .store
            .gate_and_person_by_key(key_hash)?
            .map(|(gate, id)| {
                let viewer = Viewer::person(id, gate.person.clone(), false);
                (gate, viewer)
            }))
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
}
