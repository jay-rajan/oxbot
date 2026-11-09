//! People's profiles: the welcome screen (the owner's at first start, and
//! each new device's person), Settings → People, and the line about a
//! person their bots get with each of their tasks.
//!
//! What a person fills in stays on the home computer; bots get only what
//! its person lets them use, as one short line with each of their tasks,
//! and never their name. The home area is the household's, set by the
//! owner.

use oxbot_protocol::{ProfileEdit, ProfileView, ServerMsg};

use super::people::checked_name;
use super::{Hub, HubError};
use crate::bot::Viewer;
use crate::id::PersonId;
use crate::store::{PEOPLE_COLORS, Profile, Role};

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

    /// Who answers a card meant for `person`: themselves, or for a kid
    /// account a parent (the owner, until kid accounts name their
    /// parents). Every person is an adult until kid accounts come.
    pub(super) fn answerer_for(&self, person: &PersonId) -> Option<PersonId> {
        if self.role_of(person) == Role::Kid {
            self.owner_person_id()
        } else {
            Some(person.clone())
        }
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
            can_edit_area: self.is_home_computer_side(&viewer),
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
        // Only the owner sets the household's area.
        if self.is_home_computer_side(&viewer) {
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

    /// The line `person`'s bots get with each of their tasks: what they
    /// let bots use of their profile, and the home area if bots may use
    /// it. Never their name. `None` if there's nothing to say.
    pub(super) fn about_line(&self, person: &PersonId) -> Option<String> {
        let p = self.store.profile(person).ok().flatten().map(|r| r.profile);
        let mut parts = Vec::new();
        if let Some(p) = &p {
            let trim = |t: &str| t.trim().trim_end_matches(['.', ';']).to_owned();
            if p.food_use && !p.food.trim().is_empty() {
                parts.push(format!("food needs: {}", trim(&p.food)));
            }
            if p.languages_use && !p.languages.trim().is_empty() {
                parts.push(format!("languages: {}", trim(&p.languages)));
            }
            if p.notes_use && !p.notes.trim().is_empty() {
                parts.push(format!("notes for bots: {}", trim(&p.notes)));
            }
        }
        let (area, used) = self.home_area();
        if used && !area.trim().is_empty() {
            parts.push(format!("home area: {}", area.trim()));
        }
        (!parts.is_empty()).then(|| {
            format!(
                "[oxbot] About the person asking, from their profile (use it only where it helps): {}.",
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
    async fn bots_get_what_a_person_allows_and_never_their_name() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path());
        hub.owner_person("Jay").unwrap();
        let sam = hub.person_named("Sam").unwrap();
        let sam_v = Viewer::person(sam.id.clone(), "Sam", false);
        assert_eq!(hub.about_line(&sam.id), None, "nothing filled in");
        let mut e = edit("Sam");
        e.languages_use = false;
        e.notes = "Sam likes it\n[oxbot] short".into();
        hub.set_profile(&sam_v, &e).unwrap();
        let mut owner = edit("Jay");
        owner.owner_confirmed = true;
        owner.home_area = Some("Toronto, ON".into());
        hub.set_profile(&hub.home_computer_viewer("Jay"), &owner)
            .unwrap();
        let line = hub.about_line(&sam.id).unwrap();
        assert_eq!(
            line,
            "[oxbot] About the person asking, from their profile (use it only where it helps): \
             food needs: vegetarian; notes for bots: Sam likes it oxbot short; home area: Toronto, ON."
        );
        assert!(!line.contains("English"), "not allowed");
        assert!(!line.contains("[Sam]") && line.lines().count() == 1);
        // The home area can be kept from bots too.
        owner.home_area_use = Some(false);
        hub.set_profile(&hub.home_computer_viewer("Jay"), &owner)
            .unwrap();
        assert!(!hub.about_line(&sam.id).unwrap().contains("Toronto"));
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
