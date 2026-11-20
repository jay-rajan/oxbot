//! People's profiles: the welcome screen (the owner's at first start, and
//! each new device's person), Settings → People, and the line about a
//! person their bots get with each of their tasks.
//!
//! What a person fills in stays on the home computer; bots get only what
//! its person lets them use, as one short line with each of their tasks,
//! and never their name. The home area is the household's, set by the
//! owner.

use oxbot_protocol::{MemoryItem, ProfileEdit, ProfileView, ServerMsg};

use super::people::checked_name;
use super::{Hub, HubError};
use crate::bot::Viewer;
use crate::id::PersonId;
use crate::store::{PEOPLE_COLORS, PersonRecord, Profile, Role};

/// The home's area, e.g. `Toronto, ON` (the owner sets it).
pub const PREF_HOME_AREA: &str = "home.area";
/// Whether bots may use the home area (`1`, the default, or `0`).
pub const PREF_HOME_AREA_USE: &str = "home.area_use";

/// The longest food needs or languages, in characters.
const MAX_SHORT: usize = 200;
/// The longest notes for bots, in characters.
const MAX_NOTES: usize = 500;
/// The longest home area, in characters.
const MAX_AREA: usize = 100;
/// The longest one thing in a person's memory.
const MAX_MEMORY_TEXT: usize = 200;
/// The most things in one person's memory.
const MAX_MEMORY: usize = 40;
/// The longest photo, as a data URL (a small picture the page shrinks).
const MAX_PHOTO: usize = 64 * 1024;

/// `text` on one line, without brackets (the line bots get is oxbot's
/// own), at most `max` characters.
///
/// # Errors
/// Too long, saying which `field`.
fn field(text: &str, max: usize, field: &str) -> Result<String, HubError> {
    let text: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '[' | ']'))
        .collect();
    if text.chars().count() > max {
        return Err(HubError::NotAllowed(format!(
            "{field} is at most {max} characters."
        )));
    }
    Ok(text)
}

/// A photo the page made: a small JPEG or PNG as a base64 data URL.
///
/// # Errors
/// Anything else, or too big.
fn photo(url: &str) -> Result<String, HubError> {
    let bad = || HubError::NotAllowed("That photo can't be used; pick another.".to_owned());
    let data = url
        .strip_prefix("data:image/jpeg;base64,")
        .or_else(|| url.strip_prefix("data:image/png;base64,"))
        .ok_or_else(bad)?;
    if url.len() > MAX_PHOTO {
        return Err(HubError::NotAllowed(
            "That photo is too big; pick a smaller one.".to_owned(),
        ));
    }
    if data.is_empty()
        || !data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
    {
        return Err(bad());
    }
    Ok(url.to_owned())
}

impl Hub {
    /// `person`'s role (adult if they have no profile yet).
    pub(super) fn role_of(&self, person: &PersonId) -> Role {
        match self.store.profile(person) {
            Ok(Some(p)) => p.role,
            Ok(None) => Role::Adult,
            Err(err) => {
                tracing::warn!(%err, "couldn't read a profile");
                Role::Adult
            }
        }
    }

    /// Whether `person` chose their name themselves.
    pub(super) fn name_chosen(&self, person: &PersonId) -> bool {
        self.store
            .profile(person)
            .ok()
            .flatten()
            .is_some_and(|p| p.name_set)
    }

    /// A device was paired for `person`: if they have no profile yet, their
    /// welcome screen shows on it.
    pub(super) fn welcome_new_device(&self, person: &PersonId) -> Result<(), HubError> {
        if self.store.start_profile(person, Role::Adult, true)? {
            tracing::info!(%person, "a new device's person gets the welcome screen");
        }
        Ok(())
    }

    /// The home's area as Settings → General shows it ("" when unset).
    #[must_use]
    pub fn home_area_text(&self) -> String {
        self.home_area().0
    }

    /// Sets the home's area for everyone (bots then use it); empty clears it.
    ///
    /// # Errors
    /// The area is too long, or the database failed.
    pub fn set_home_area(&self, area: &str) -> Result<(), HubError> {
        let area = field(area, MAX_AREA, "The home area")?;
        self.store.set_pref(PREF_HOME_AREA, &area)?;
        self.store.set_pref(PREF_HOME_AREA_USE, "1")?;
        tracing::info!("set the home area");
        let _ = self.updates.send(ServerMsg::ProfilesChanged);
        Ok(())
    }

    /// The home area, and whether bots may use it.
    fn home_area(&self) -> (String, bool) {
        let area = self
            .store
            .pref(PREF_HOME_AREA)
            .ok()
            .flatten()
            .unwrap_or_default();
        let used = self
            .store
            .pref(PREF_HOME_AREA_USE)
            .ok()
            .flatten()
            .as_deref()
            != Some("0");
        (area, used)
    }

    /// `viewer`'s own profile, if they're someone oxbot knows.
    ///
    /// # Errors
    /// Fails if the database does.
    pub fn profile_for(&self, viewer: &Viewer) -> Result<Option<ProfileView>, HubError> {
        let viewer = self.resolve(viewer);
        let Some(id) = viewer.id.as_ref() else {
            return Ok(None);
        };
        let Some(person) = self.store.person_by_id(id)? else {
            return Ok(None);
        };
        let rec = self.store.profile(id)?;
        let role = rec.as_ref().map_or(Role::Adult, |r| r.role);
        let p = rec.as_ref().map_or_else(
            || Profile {
                food_use: true,
                languages_use: true,
                notes_use: true,
                ..Profile::default()
            },
            |r| r.profile.clone(),
        );
        let (home_area, home_area_use) = self.home_area();
        Ok(Some(ProfileView {
            name: person.name,
            color: person.color,
            colors: PEOPLE_COLORS.iter().map(|c| (*c).to_owned()).collect(),
            photo: p.photo,
            role: role.as_str().to_owned(),
            food: p.food,
            food_use: p.food_use,
            languages: p.languages,
            languages_use: p.languages_use,
            notes: p.notes,
            notes_use: p.notes_use,
            home_area,
            home_area_use,
            can_edit_area: self.resolve(&viewer).sees_all(),
            welcome: rec.as_ref().is_some_and(|r| r.welcome_due),
            owner_confirm: role == Role::Owner
                && rec.as_ref().is_none_or(|r| r.owner_ok_ms.is_none()),
        }))
    }

    /// Saves `viewer`'s own profile (and, from the owner, the home area);
    /// their welcome is done. The owner confirms once that they're 18 or
    /// older and accept `OpenRouter`'s terms.
    ///
    /// # Errors
    /// They're nobody oxbot knows, a field isn't valid, the name is
    /// someone else's, the owner didn't confirm, or the database fails.
    pub fn set_profile(
        &self,
        viewer: &Viewer,
        edit: &ProfileEdit,
    ) -> Result<ProfileView, HubError> {
        let viewer = self.resolve(viewer).into_owned();
        let id = viewer.id.clone().ok_or_else(|| {
            HubError::NotAllowed("oxbot needs to know who you are first.".to_owned())
        })?;
        let person = self.store.person_by_id(&id)?.ok_or_else(|| {
            HubError::NotAllowed("oxbot needs to know who you are first.".to_owned())
        })?;
        let role = self.role_of(&id);
        if role == Role::Owner
            && !edit.owner_confirmed
            && self
                .store
                .profile(&id)?
                .is_none_or(|r| r.owner_ok_ms.is_none())
        {
            return Err(HubError::NotAllowed(
                "To finish, confirm you're 18 or older and accept OpenRouter's terms.".to_owned(),
            ));
        }
        let name = checked_name(&edit.name, "Say what to call you.")?;
        let color = PEOPLE_COLORS
            .iter()
            .position(|c| c.eq_ignore_ascii_case(edit.color.trim()))
            .ok_or_else(|| HubError::NotAllowed("Pick one of the colors.".to_owned()))?;
        let profile = Profile {
            photo: match edit.photo.as_deref().map(str::trim) {
                None | Some("") => None,
                Some(url) => Some(photo(url)?),
            },
            food: field(&edit.food, MAX_SHORT, "Food needs")?,
            food_use: edit.food_use,
            languages: field(&edit.languages, MAX_SHORT, "Languages")?,
            languages_use: edit.languages_use,
            notes: field(&edit.notes, MAX_NOTES, "Notes for bots")?,
            notes_use: edit.notes_use,
        };
        let area = match &edit.home_area {
            Some(area) => Some(field(area, MAX_AREA, "The home area")?),
            None => None,
        };
        if name != person.name {
            if !self.store.rename_person(&id, &name)? {
                return Err(HubError::NotAllowed(format!(
                    "Someone else in the home is called {name}."
                )));
            }
            self.store.set_name_chosen(&id)?;
            tracing::info!(person = %id, "a person changed their name");
        }
        self.store.set_person_color(&id, color)?;
        // Only the administrators set the household's area.
        if self.resolve(&viewer).sees_all() {
            if let Some(area) = area {
                self.store.set_pref(PREF_HOME_AREA, &area)?;
            }
            if let Some(used) = edit.home_area_use {
                self.store
                    .set_pref(PREF_HOME_AREA_USE, if used { "1" } else { "0" })?;
            }
        }
        self.store
            .save_profile(&id, &profile, role == Role::Owner && edit.owner_confirmed)?;
        tracing::info!(person = %id, "saved a profile");
        self.announce_people();
        let _ = self.updates.send(ServerMsg::ProfilesChanged);
        self.profile_for(&Viewer::person(id, name, viewer.home_computer))?
            .ok_or_else(|| HubError::NotAllowed("oxbot lost track of you.".to_owned()))
    }

    /// Whose memory `name` names, if `viewer` may see and change it: their
    /// own, or anyone's from the home computer's side.
    fn memory_owner(&self, viewer: &Viewer, name: &str) -> Result<PersonRecord, HubError> {
        let viewer = self.resolve(viewer).into_owned();
        let person = self
            .store
            .person_by_name(name.trim())?
            .filter(|p| !self.store.is_removed(&p.id).unwrap_or(true))
            .ok_or_else(|| HubError::NotAllowed(format!("{} isn't in the home.", name.trim())))?;
        if viewer.id.as_ref() == Some(&person.id)
            || viewer.sees_all()
            || self.is_home_computer_side(&viewer)
        {
            Ok(person)
        } else {
            Err(HubError::NotAllowed(
                "Only they, or an administrator, can see what bots remember about them.".to_owned(),
            ))
        }
    }

    /// What bots should remember about `name`, oldest first.
    ///
    /// # Errors
    /// They aren't in the home, `viewer` may not see it, or the database failed.
    pub fn memory_for(&self, viewer: &Viewer, name: &str) -> Result<Vec<MemoryItem>, HubError> {
        let person = self.memory_owner(viewer, name)?;
        Ok(self
            .store
            .memory(&person.id)?
            .into_iter()
            .map(|(id, text)| MemoryItem { id, text })
            .collect())
    }

    /// Adds one thing to `name`'s memory; the memory now.
    ///
    /// # Errors
    /// As [`Hub::memory_for`], or the text is empty, too long, or one too many.
    pub fn add_memory(
        &self,
        viewer: &Viewer,
        name: &str,
        text: &str,
    ) -> Result<Vec<MemoryItem>, HubError> {
        let person = self.memory_owner(viewer, name)?;
        // One plain line: no brackets, so it can't pass for oxbot's own notes.
        let text = field(text, MAX_MEMORY_TEXT, "Each thing")?;
        if text.is_empty() {
            return Err(HubError::NotAllowed(
                "Say what bots should remember.".to_owned(),
            ));
        }
        let now = self.store.memory(&person.id)?;
        if now.len() >= MAX_MEMORY {
            return Err(HubError::NotAllowed(format!(
                "Up to {MAX_MEMORY} things each: remove one first."
            )));
        }
        if !now.iter().any(|(_, t)| t.eq_ignore_ascii_case(&text)) {
            self.store.add_memory(&person.id, &text)?;
            tracing::info!(person = %person.id, "added to someone's memory");
        }
        self.memory_for(viewer, name)
    }

    /// Removes one thing from `name`'s memory; the memory now.
    ///
    /// # Errors
    /// As [`Hub::memory_for`].
    pub fn remove_memory(
        &self,
        viewer: &Viewer,
        name: &str,
        id: i64,
    ) -> Result<Vec<MemoryItem>, HubError> {
        let person = self.memory_owner(viewer, name)?;
        if self.store.remove_memory(&person.id, id)? {
            tracing::info!(person = %person.id, "removed from someone's memory");
        }
        self.memory_for(viewer, name)
    }

    /// The line `person`'s bots get with each of their tasks: what bots
    /// should remember about them, and the home area if bots may use it.
    /// Never their name. `None` if there's nothing to say.
    pub(super) fn about_line(&self, person: &PersonId) -> Option<String> {
        let mut parts: Vec<String> = self
            .store
            .memory(person)
            .unwrap_or_default()
            .into_iter()
            .map(|(_, t)| t.trim().trim_end_matches(['.', ';']).to_owned())
            .filter(|t| !t.is_empty())
            .collect();
        // The area is the whole household's. Not set yet: the bot asks
        // where when a task depends on it, and may offer to save it.
        let (area, used) = self.home_area();
        if used && !area.trim().is_empty() {
            parts.push(format!("home area: {}", area.trim()));
        } else if used {
            parts.push("home area: not set yet".to_owned());
        }
        (!parts.is_empty()).then(|| {
            format!(
                "[oxbot] About the person asking, from what they asked bots to remember (use it only where it helps): {}.",
                parts.join("; ")
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::HubConfig;
    use crate::store::Store;

    fn edit(name: &str) -> ProfileEdit {
        ProfileEdit {
            name: name.into(),
            color: PEOPLE_COLORS[3].into(),
            food: "vegetarian".into(),
            food_use: true,
            languages: "English, Hindi".into(),
            languages_use: true,
            notes: "I prefer metric.".into(),
            notes_use: true,
            ..ProfileEdit::default()
        }
    }

    fn hub(dir: &std::path::Path) -> Arc<Hub> {
        Hub::new(
            vec![],
            Store::open_in_memory().unwrap(),
            HubConfig::new(dir),
        )
    }

    #[tokio::test]
    async fn the_first_start_welcomes_the_owner_who_confirms_once() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let home_computer = hub.home_computer_viewer("Jay");
        let view = hub.profile_for(&home_computer).unwrap().unwrap();
        assert!(view.welcome && view.owner_confirm && view.can_edit_area);
        assert_eq!((view.name.as_str(), view.role.as_str()), ("Jay", "owner"));
        // Not without confirming.
        let err = hub.set_profile(&home_computer, &edit("Jay")).unwrap_err();
        assert!(err.to_string().contains("18 or older"), "{err}");
        let mut e = edit("Jayadev");
        e.owner_confirmed = true;
        e.home_area = Some("Toronto, ON".into());
        e.home_area_use = Some(true);
        let view = hub.set_profile(&home_computer, &e).unwrap();
        assert!(!view.welcome && !view.owner_confirm);
        assert_eq!(
            (view.name.as_str(), view.home_area.as_str()),
            ("Jayadev", "Toronto, ON")
        );
        assert_eq!(view.color, PEOPLE_COLORS[3]);
        // From then on, saving needs no confirming again.
        let home_computer = hub.home_computer_viewer("Jay");
        assert!(hub.set_profile(&home_computer, &edit("Jayadev")).is_ok());
    }

    #[tokio::test]
    async fn the_home_area_is_one_setting_for_the_whole_home() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        let jay = hub.home_computer_viewer("Jay");
        assert_eq!(hub.home_area_text(), "");
        hub.set_home_area("  Lorne Park,   Mississauga ").unwrap();
        assert_eq!(hub.home_area_text(), "Lorne Park, Mississauga");
        let view = hub.profile_for(&jay).unwrap().unwrap();
        assert_eq!(view.home_area, "Lorne Park, Mississauga");
        assert!(hub.set_home_area(&"x".repeat(101)).is_err());
        hub.set_home_area("").unwrap();
        assert_eq!(hub.home_area_text(), "");
    }

    #[tokio::test]
    async fn the_owners_chosen_name_stays_whatever_the_mac_account_says() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("oxbot.db");
        let start = || {
            Hub::new(
                vec![],
                Store::open(&db).unwrap(),
                HubConfig::new(tmp.path()),
            )
        };
        let hub = start();
        let jay = hub.owner_person_from("Jay", true).unwrap();
        let mut e = edit("J. R.");
        e.owner_confirmed = true;
        hub.set_profile(&hub.home_computer_viewer("Jay"), &e)
            .unwrap();
        hub.shutdown().await;
        drop(hub);
        let hub = start();
        let again = hub.owner_person_from("Jay", true).unwrap();
        assert_eq!((again.id, again.name.as_str()), (jay.id, "J. R."));
    }

    #[tokio::test]
    async fn people_from_before_and_by_name_get_no_welcome_but_a_paired_device_does() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        hub.owner_person("Jay").unwrap();
        let sam = hub.person_named("Sam").unwrap();
        let sam_v = Viewer::person(sam.id.clone(), "Sam", false);
        assert!(!hub.profile_for(&sam_v).unwrap().unwrap().welcome);
        // Sam's phone pairs: Sam is welcomed there.
        hub.add_device_for("g1", &sam.id, "Phone", "hash-1")
            .unwrap();
        let view = hub.profile_for(&sam_v).unwrap().unwrap();
        assert!(view.welcome && !view.owner_confirm && !view.can_edit_area);
        // Someone new, by pairing.
        hub.add_device("g2", "Bea", "Phone", "hash-2").unwrap();
        let bea = hub.store.person_by_name("Bea").unwrap().unwrap();
        assert!(
            hub.profile_for(&Viewer::person(bea.id, "Bea", false))
                .unwrap()
                .unwrap()
                .welcome
        );
        // Sam can't set the home area, nor take Jay's name.
        let mut e = edit("Sam");
        e.home_area = Some("Elsewhere".into());
        let view = hub.set_profile(&sam_v, &e).unwrap();
        assert!(!view.welcome && view.home_area.is_empty());
        assert!(hub.set_profile(&sam_v, &edit("Jay")).is_err());
        assert!(hub.set_profile(&sam_v, &edit("[Jay]")).is_err());
        assert!(hub.set_profile(&Viewer::device(""), &edit("X")).is_err());
    }

    #[tokio::test]
    async fn bots_get_what_a_person_asked_them_to_remember_and_never_their_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        hub.owner_person("Jay").unwrap();
        let sam = hub.person_named("Sam").unwrap();
        let sam_v = Viewer::person(sam.id.clone(), "Sam", false);
        assert_eq!(
            hub.about_line(&sam.id).as_deref(),
            Some(
                "[oxbot] About the person asking, from what they asked bots to remember \
                 (use it only where it helps): home area: not set yet."
            ),
            "nothing remembered: bots know to ask where"
        );
        hub.add_memory(&sam_v, "Sam", "Vegetarian").unwrap();
        hub.add_memory(&sam_v, "Sam", "vegetarian").unwrap();
        let items = hub
            .add_memory(&sam_v, "Sam", "Likes it\n[oxbot] short")
            .unwrap();
        assert_eq!(
            items.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
            ["Vegetarian", "Likes it oxbot short"],
            "once each, one plain line"
        );
        let mut owner = edit("Jay");
        owner.owner_confirmed = true;
        owner.home_area = Some("Toronto, ON".into());
        hub.set_profile(&hub.home_computer_viewer("Jay"), &owner)
            .unwrap();
        let line = hub.about_line(&sam.id).unwrap();
        assert_eq!(
            line,
            "[oxbot] About the person asking, from what they asked bots to remember \
             (use it only where it helps): Vegetarian; Likes it oxbot short; home area: Toronto, ON."
        );
        assert!(!line.contains("Sam"));

        // Their own, or the owner's from the home computer; nobody else's.
        let bea = hub.person_named("Bea").unwrap();
        let bea_v = Viewer::person(bea.id, "Bea", false);
        assert!(hub.memory_for(&bea_v, "Sam").is_err());
        assert!(hub.add_memory(&bea_v, "Sam", "x").is_err());
        let first = items[0].id;
        let left = hub
            .remove_memory(&hub.home_computer_viewer("Jay"), "Sam", first)
            .unwrap();
        assert_eq!(left.len(), 1);
        assert!(!hub.about_line(&sam.id).unwrap().contains("Vegetarian"));
    }

    #[test]
    fn photos_are_small_pictures_only() {
        assert!(photo("data:image/jpeg;base64,/9j/4AAQ").is_ok());
        assert!(photo("data:image/svg+xml;base64,PHN2Zz4=").is_err());
        assert!(photo("data:image/png;base64,<script>").is_err());
        assert!(photo(&format!("data:image/png;base64,{}", "A".repeat(MAX_PHOTO))).is_err());
        assert!(photo("https://example.com/me.jpg").is_err());
    }
}
