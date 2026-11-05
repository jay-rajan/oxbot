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
/// `bot.toml` (see [`adopt`]). Returns the bots, and whether anyone new
/// was made for them.
pub(super) fn adopt_bots(
    bots: Vec<Bot>,
    store: &Store,
    farm: Option<&PersonRecord>,
) -> (Vec<Bot>, bool) {
    let mut anyone_new = false;
    let bots = bots
        .into_iter()
        .map(|mut bot| {
            match adopt(&mut bot, store, farm) {
                Ok(new) => anyone_new |= new,
                Err(err) => tracing::warn!(bot = %bot.id, %err, "couldn't give a bot its owner"),
            }
            bot
        })
        .collect();
    (bots, anyone_new)
}

/// Gives `bot` an owner if it has none: a personal Herder belongs to the
/// person it is kept with; any other bot to the person its `created_by`
/// names (made if new, with the browser folder and Herder an older oxbot
/// gave them, so their sign-ins carry over). A private bot nobody is
/// recorded as making stays ownerless (only the Farm manages it); a shared
/// one becomes the Farm's own person's (if chosen yet). Herder
/// (`assistant`) belongs to nobody. Returns whether anyone new was made.
fn adopt(bot: &mut Bot, store: &Store, farm: Option<&PersonRecord>) -> Result<bool, HubError> {
    if bot.owner.is_some() || bot.is_main_herder() {
        return Ok(false);
    }
    let kept_with = if is_personal_herder(bot.id.as_str()) {
        store.person_by_herder(&bot.id)?
    } else {
        None
    };
    let maker = bot
        .created_by
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let (person, new) = match (kept_with, maker, farm) {
        (Some(person), _, _) => (person, false),
        (None, Some(name), _) => store.find_or_create_legacy_person(name)?,
        // A shared bot from before oxbot kept makers: the Mac owner's.
        (None, None, Some(farm)) if !bot.is_central() && !bot.private => (farm.clone(), false),
        _ => return Ok(false),
    };
    bot.owner = Some(person.id.clone());
    bot.created_by.get_or_insert(person.name);
    bot.save()?;
    tracing::info!(bot = %bot.id, owner = %person.id, "gave a bot its owner");
    Ok(new)
}

/// A person's name made safe: trimmed, one space between words, at most
/// [`MAX_PERSON`] characters, no brackets or line breaks (names go into
/// bots' messages as `[Name]`), and not a name oxbot uses itself. `empty`
/// says what to do when there's none.
///
/// # Errors
/// What's wrong with it.
pub(super) fn checked_name(name: &str, empty: &str) -> Result<String, HubError> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let refuse = |why: &str| Err(HubError::NotAllowed(why.to_owned()));
    if name.is_empty() {
        return refuse(empty);
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
    Ok(name)
}

impl Hub {
    /// `viewer` tied to a person, by looking them up: one with only a name
    /// gets the id of the person called exactly that, so access is decided
    /// by id from here on. Nobody is ever made here: a name oxbot doesn't
    /// know (or a nameless viewer) stays nobody, who matches no bot's
    /// owner. (Tests can turn on [`HubConfig::enrol_named_viewers`].)
    ///
    /// [`HubConfig::enrol_named_viewers`]: super::HubConfig
    #[must_use]
    pub fn resolve<'a>(&self, viewer: &'a Viewer) -> Cow<'a, Viewer> {
        if viewer.id.is_some() {
            return Cow::Borrowed(viewer);
        }
        let nobody = Viewer {
            id: None,
            name: String::new(),
            farm: viewer.farm,
        };
        if viewer.name.trim().is_empty() {
            return Cow::Owned(nobody);
        }
        let found = if self.enrols_named_viewers() {
            self.person_named(&viewer.name).map(Some)
        } else {
            self.store
                .person_by_name(&viewer.name)
                .map_err(HubError::from)
        };
        match found {
            Ok(Some(person)) => Cow::Owned(Viewer::person(person.id, person.name, viewer.farm)),
            Ok(None) => Cow::Owned(nobody),
            Err(err) => {
                // Nobody rather than matched by name.
                tracing::warn!(%err, "couldn't look up a person");
                Cow::Owned(nobody)
            }
        }
    }

    /// Whether a viewer known only by name is made a person (tests only).
    #[cfg_attr(
        not(any(test, feature = "test-people")),
        expect(clippy::unused_self, reason = "never, outside tests")
    )]
    fn enrols_named_viewers(&self) -> bool {
        #[cfg(any(test, feature = "test-people"))]
        {
            self.config.enrol_named_viewers
        }
        #[cfg(not(any(test, feature = "test-people")))]
        {
            false
        }
    }

    /// The person called exactly `name` (trimmed), made if there's none;
    /// every client is told when someone new joins. The one way oxbot makes
    /// a person on purpose: a message sent by name (`oxbot chat`, tests), a
    /// bot made for someone by name, a Herder for someone by name, the
    /// Farm's own person, an e2e page's `?as=`.
    ///
    /// # Errors
    /// Fails if `name` is empty or the database fails.
    pub fn person_named(&self, name: &str) -> Result<PersonRecord, HubError> {
        let (person, new) = self.person_named_quietly(name)?;
        if new {
            self.announce_people();
        }
        Ok(person)
    }

    /// Like [`Hub::person_named`], leaving telling clients to the caller
    /// (with [`Hub::announce_people`], when the flag says they're new).
    pub(super) fn person_named_quietly(
        &self,
        name: &str,
    ) -> Result<(PersonRecord, bool), HubError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(HubError::NotAllowed("A person needs a name.".to_owned()));
        }
        let (person, new) = self.store.find_or_create_person(name)?;
        if new {
            tracing::info!(person = %person.id, "someone new");
        }
        Ok((person, new))
    }

    /// Tells every client who everyone is now.
    pub(super) fn announce_people(&self) {
        match self.people() {
            Ok(people) => {
                let _ = self.updates.send(ServerMsg::People { people });
            }
            Err(err) => tracing::warn!(%err, "couldn't read people"),
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
        self.farm_person_from(local_name, true)
    }

    /// Like [`Hub::farm_person`]. `from_account`: whether `local_name` is
    /// the Mac account's own name. Only that renames the Farm's person; a
    /// stand-in (the device's name, `Me`) is used only to choose them the
    /// first time.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn farm_person_from(
        &self,
        local_name: &str,
        from_account: bool,
    ) -> Result<PersonRecord, HubError> {
        let mut farm = lock(&self.farm);
        if let Some(id) = farm.as_ref()
            && let Some(person) = self.store.person_by_id(id)?
        {
            return Ok(person);
        }
        let local_name = local_name.trim();
        let (local_name, from_account) = if local_name.is_empty() {
            ("Me", false)
        } else {
            (local_name, from_account)
        };
        let person = match self.store.farm_person()? {
            Some(person)
                if from_account && person.name != local_name && !self.name_chosen(&person.id) =>
            {
                self.rename_farm_person(&person, local_name)?
            }
            Some(person) => person,
            None => {
                // A first start: the welcome screen asks the owner the rest.
                let first = self.store.nobody_yet()?;
                let person = self.person_named(local_name)?;
                self.store.set_farm_person(&person.id)?;
                self.store
                    .start_profile(&person.id, crate::store::Role::Owner, first)?;
                tracing::info!(person = %person.id, first, "chose the Farm's person");
                person
            }
        };
        // The Farm's person is the owner.
        if self.role_of(&person.id) != crate::store::Role::Owner {
            self.store.set_role(&person.id, crate::store::Role::Owner)?;
        }
        *farm = Some(person.id.clone());
        drop(farm);
        self.adopt_ownerless(&person);
        Ok(person)
    }

    /// Gives the Farm's person the Mac account's name, unless someone else
    /// has it (then they keep theirs, and the log says why).
    fn rename_farm_person(
        &self,
        person: &PersonRecord,
        name: &str,
    ) -> Result<PersonRecord, HubError> {
        if !self.store.rename_person(&person.id, name)? {
            tracing::warn!(
                person = %person.id,
                "the Farm's person keeps their name: someone else has the Mac account's"
            );
            return Ok(person.clone());
        }
        tracing::info!(person = %person.id, "the Farm's person has a new name");
        self.announce_people();
        Ok(self
            .store
            .person_by_id(&person.id)?
            .unwrap_or_else(|| person.clone()))
    }

    /// The Farm's own window as a viewer: the Farm's person, on the Farm.
    /// (Never renames them: oxbot's start does that, see
    /// [`Hub::farm_person_from`].)
    #[must_use]
    pub fn farm_viewer(&self, local_name: &str) -> Viewer {
        match self.farm_person_from(local_name, false) {
            Ok(person) => Viewer::person(person.id, person.name, true),
            Err(err) => {
                tracing::warn!(%err, "couldn't find the Farm's person");
                Viewer::farm(local_name)
            }
        }
    }

    /// Whether `id` (a person id) is the Farm's own person's.
    #[must_use]
    pub fn is_farm_person(&self, id: &str) -> bool {
        self.farm_id().is_some_and(|f| f.as_str() == id)
    }

    /// The Farm's person's id, if chosen.
    pub(super) fn farm_id(&self) -> Option<PersonId> {
        if let Some(id) = lock(&self.farm).clone() {
            return Some(id);
        }
        self.store.farm_person().ok().flatten().map(|p| p.id)
    }

    /// Shared bots nobody is recorded as making become the Farm's
    /// person's (see [`adopt`]).
    fn adopt_ownerless(&self, farm: &PersonRecord) {
        let mut anyone_new = false;
        {
            let mut bots = self.bots.write().unwrap_or_else(PoisonError::into_inner);
            for bot in bots.values_mut().filter(|b| b.owner.is_none()) {
                match adopt(bot, &self.store, Some(farm)) {
                    Ok(new) => anyone_new |= new,
                    Err(err) => {
                        tracing::warn!(bot = %bot.id, %err, "couldn't give a bot its owner");
                    }
                }
            }
        }
        if anyone_new {
            self.announce_people();
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
                "The Assistant needs to know who it's for.".to_owned(),
            )),
        }
    }

    /// The Herder of the person called exactly `person` (made if new, see
    /// [`Hub::person_named`]); see [`Hub::herder_for_person`]. For `oxbot
    /// chat` and tests: connections use [`Hub::herder_of`].
    ///
    /// # Errors
    /// Fails if `person` is empty or its folder can't be written.
    pub fn herder_for(&self, person: &str) -> Result<BotId, HubError> {
        let person = person.trim();
        if person.is_empty() {
            return Err(HubError::NotAllowed(
                "The Assistant needs to know who it's for.".to_owned(),
            ));
        }
        let person = self.person_named(person)?;
        self.herder_for_person(&person.id)
    }

    /// Person `id`'s own Herder, made the first time they need it. It shares
    /// the Farm's Herder's instructions, but has its own engine
    /// session and history, so nobody can ask it what someone else asked.
    /// Its id is kept with the person, so it stays theirs if their name
    /// changes. (The Farm's own person uses the Farm's Herder, `assistant`.)
    ///
    /// # Errors
    /// Fails if there's no such person, its folder belongs to someone else,
    /// or it can't be written.
    pub fn herder_for_person(&self, id: &PersonId) -> Result<BotId, HubError> {
        let person = self.store.person_by_id(id)?.ok_or_else(|| {
            HubError::NotAllowed("The Assistant needs to know who it's for.".to_owned())
        })?;
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
                "That Assistant belongs to someone else.".to_owned(),
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
        Ok(self.gate_pick(name)?.0)
    }

    /// Like [`Hub::gate_person`], with the id of the person it picked if
    /// they're known already (`None`: someone new, made when the Gate
    /// pairs). The Farm's own person is picked only by their exact name:
    /// "jay" never pairs as the Mac owner "Jay".
    ///
    /// # Errors
    /// Says what's wrong with the name, or the database failed.
    pub fn gate_pick(&self, name: &str) -> Result<(String, Option<PersonId>), HubError> {
        let name = checked_name(name, "Say who the device is for.")?;
        let known = self.store.person_records()?;
        if let Some(person) = known.iter().find(|p| p.name == name) {
            return Ok((name, Some(person.id.clone())));
        }
        let lower = name.to_lowercase();
        let mut alike = known
            .into_iter()
            .filter(|p| !p.farm && p.name.to_lowercase() == lower);
        match (alike.next(), alike.next()) {
            (Some(one), None) => Ok((one.name, Some(one.id))),
            _ => Ok((name, None)),
        }
    }

    /// Remembers a newly paired Gate for the person called exactly `person`
    /// (made if new, and every client told; see [`Hub::gate_person`]).
    /// `key_hash` is the hash of its key.
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
        let (gate, new) = self.store.add_gate(id, person, device, key_hash)?;
        if let Some(found) = self.store.person_by_name(person)? {
            self.welcome_new_device(&found.id)?;
        }
        if new {
            self.announce_people();
        }
        Ok(gate)
    }

    /// Remembers a newly paired Gate for the person `person` (picked when
    /// its pairing code was shown, see [`Hub::gate_pick`]).
    ///
    /// # Errors
    /// Fails if there's no such person or the database fails.
    pub fn add_gate_for(
        &self,
        id: &str,
        person: &PersonId,
        device: &str,
        key_hash: &str,
    ) -> Result<GateInfo, HubError> {
        let gate = self.store.add_gate_for(id, person, device, key_hash)?;
        self.welcome_new_device(person)?;
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
