//! Sidebar sections: each person files bots into sections of their own.
//!
//! Sections are a person's view, not bot data: filing a bot changes nothing
//! about it (its name, privacy or who sees it), so it needs no right to
//! manage the bot, and nobody else (the Farm included) gets someone's
//! sections. They never reach a model either.

use oxbot_protocol::{SectionInfo, ServerMsg};

use super::{Hub, HubError};
use crate::bot::Viewer;
use crate::id::{BotId, PersonId};
use crate::store::SectionError;

/// The longest section name, in characters.
pub const MAX_SECTION_NAME: usize = 40;
/// The most sections one person may have.
pub const MAX_SECTIONS: usize = 50;

impl From<SectionError> for HubError {
    fn from(e: SectionError) -> Self {
        match e {
            SectionError::NameTaken => {
                Self::NotAllowed("You already have a section called that.".to_owned())
            }
            SectionError::TooMany => Self::NotAllowed(format!(
                "That's the most sections you can have ({MAX_SECTIONS})."
            )),
            SectionError::NoSuchSection => Self::NotAllowed("No such section.".to_owned()),
            SectionError::Store(e) => Self::Store(e),
        }
    }
}

/// A section name as kept: trimmed, one line, not empty, not too long.
fn section_name(name: &str) -> Result<&str, HubError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(HubError::NotAllowed("A section needs a name.".to_owned()));
    }
    if name.chars().count() > MAX_SECTION_NAME || name.chars().any(char::is_control) {
        return Err(HubError::NotAllowed(format!(
            "A section's name is one line of at most {MAX_SECTION_NAME} characters."
        )));
    }
    Ok(name)
}

impl Hub {
    /// Who `viewer` is, for their sections.
    fn section_owner(&self, viewer: &Viewer) -> Result<PersonId, HubError> {
        self.resolve(viewer)
            .id
            .clone()
            .ok_or_else(|| HubError::NotAllowed("Sections need to know who you are.".to_owned()))
    }

    /// `person`'s sections and where their bots are, as sent to them.
    fn sections_msg(&self, person: &PersonId) -> Result<ServerMsg, HubError> {
        let sections = self
            .store
            .sections(person)?
            .into_iter()
            .map(|s| SectionInfo {
                id: s.id,
                name: s.name,
            })
            .collect();
        let placed = self.store.placements(person)?.into_iter().collect();
        Ok(ServerMsg::Sections {
            sections,
            placed,
            owner_id: Some(person.to_string()),
        })
    }

    /// The sections `viewer` gets when they connect: their own, or `None`
    /// for someone oxbot can't place (who has none).
    #[must_use]
    pub fn sections_for(&self, viewer: &Viewer) -> Option<ServerMsg> {
        let person = self.section_owner(viewer).ok()?;
        match self.sections_msg(&person) {
            Ok(msg) => Some(msg),
            Err(err) => {
                tracing::warn!(%err, "couldn't read a person's sections");
                None
            }
        }
    }

    /// Sends `person`'s sections to every connection of theirs.
    pub(super) fn announce_sections(&self, person: &PersonId) {
        match self.sections_msg(person) {
            Ok(msg) => self.announce(msg),
            Err(err) => tracing::warn!(%err, "couldn't read a person's sections"),
        }
    }

    /// The bot `id` as a person may file it: one in the list that isn't
    /// Herder. Hidden ones (another person's Herder) answer as unknown.
    fn bot_to_file(&self, id: &str, viewer: &Viewer) -> Result<BotId, HubError> {
        let bot = self.bot(id)?;
        if !bot.is_central() {
            return Ok(bot.id);
        }
        // Someone else's Herder isn't in their list at all.
        if !self.opens(&bot, &self.resolve(viewer)) {
            return Err(HubError::UnknownBot(id.to_owned()));
        }
        Err(HubError::NotAllowed(
            "The Assistant stays at the top.".to_owned(),
        ))
    }

    /// Makes a section for `viewer`, last in their list, with `bot` in it
    /// if given.
    ///
    /// # Errors
    /// Fails for someone oxbot can't place, a bad or taken name, too many
    /// sections, a bot that can't be filed, or the database.
    pub fn create_section(
        &self,
        name: &str,
        bot: Option<&str>,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let person = self.section_owner(viewer)?;
        let name = section_name(name)?;
        let bot = bot.map(|b| self.bot_to_file(b, viewer)).transpose()?;
        let id = self
            .store
            .create_section(&person, name, bot.as_ref(), MAX_SECTIONS)?;
        tracing::info!(section = %id, person = %person, "made a section");
        self.announce_sections(&person);
        Ok(())
    }

    /// Renames one of `viewer`'s sections.
    ///
    /// # Errors
    /// Fails for someone oxbot can't place, a section that isn't theirs,
    /// a bad or taken name, or the database.
    pub fn rename_section(&self, id: &str, name: &str, viewer: &Viewer) -> Result<(), HubError> {
        let person = self.section_owner(viewer)?;
        let name = section_name(name)?;
        self.store.rename_section(&person, id, name)?;
        tracing::info!(section = %id, "renamed a section");
        self.announce_sections(&person);
        Ok(())
    }

    /// Deletes one of `viewer`'s sections; its bots go back to not being in
    /// a section.
    ///
    /// # Errors
    /// Fails for someone oxbot can't place, a section that isn't theirs,
    /// or the database.
    pub fn delete_section(&self, id: &str, viewer: &Viewer) -> Result<(), HubError> {
        let person = self.section_owner(viewer)?;
        self.store.delete_section(&person, id)?;
        tracing::info!(section = %id, "deleted a section");
        self.announce_sections(&person);
        Ok(())
    }

    /// Files `bot` in one of `viewer`'s sections, or takes it out with
    /// `None`. Any listed bot but Herder: it's their own view, so it needs
    /// no right to manage the bot.
    ///
    /// # Errors
    /// Fails for someone oxbot can't place, Herder, an unknown bot, a
    /// section that isn't theirs, or the database.
    pub fn move_bot(
        &self,
        bot: &str,
        section: Option<&str>,
        viewer: &Viewer,
    ) -> Result<(), HubError> {
        let person = self.section_owner(viewer)?;
        let bot = self.bot_to_file(bot, viewer)?;
        self.store.place_bot(&person, &bot, section)?;
        self.announce_sections(&person);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use oxbot_protocol::ServerMsg;

    use super::*;
    use crate::bot::Bot;
    use crate::hub::HubConfig;
    use crate::store::Store;

    /// Herder, Jay's shared Recipes and private Bank (Jay is the owner:
    /// only the owner has private bots), and Sam.
    async fn hub(dir: &std::path::Path) -> Arc<Hub> {
        let herder = Bot::create(dir, BotId::new("assistant").unwrap(), "Herder", "").unwrap();
        let hub = Hub::new(
            vec![herder],
            Store::open_in_memory().unwrap(),
            HubConfig::new(dir),
        );
        hub.farm_person("Jay").unwrap();
        hub.create_bot_with("Recipes", "", Some("Jay"), false)
            .await
            .unwrap();
        hub.create_bot_with("Bank", "", Some("Jay"), true)
            .await
            .unwrap();
        hub.person_named("Sam").unwrap();
        hub
    }

    /// `viewer`'s sections as (names, placed bot → section name).
    fn view(hub: &Hub, viewer: &Viewer) -> (Vec<String>, Vec<(String, String)>) {
        let Some(ServerMsg::Sections {
            sections, placed, ..
        }) = hub.sections_for(viewer)
        else {
            return (Vec::new(), Vec::new());
        };
        let name = |id: &str| {
            sections
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.clone())
                .unwrap_or_default()
        };
        let placed = placed.iter().map(|(b, s)| (b.clone(), name(s))).collect();
        (sections.into_iter().map(|s| s.name).collect(), placed)
    }

    fn section_id(hub: &Hub, viewer: &Viewer, name: &str) -> String {
        let Some(ServerMsg::Sections { sections, .. }) = hub.sections_for(viewer) else {
            panic!("no sections");
        };
        sections.into_iter().find(|s| s.name == name).unwrap().id
    }

    #[tokio::test]
    async fn sections_are_each_persons_own() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path()).await;
        let (jay, sam) = (Viewer::gate("Jay"), Viewer::gate("Sam"));
        hub.create_section("Kitchen", Some("recipes"), &jay)
            .unwrap();
        hub.create_section("Money", None, &jay).unwrap();
        assert_eq!(
            view(&hub, &jay),
            (
                vec!["Kitchen".to_owned(), "Money".to_owned()],
                vec![("recipes".to_owned(), "Kitchen".to_owned())]
            ),
            "in the order made, with the bot filed in one step"
        );
        assert_eq!(view(&hub, &sam), (vec![], vec![]), "Sam has none");
        // The same name is fine for someone else.
        hub.create_section("Kitchen", None, &sam).unwrap();

        // Jay's section ids do nothing for Sam.
        let kitchen = section_id(&hub, &jay, "Kitchen");
        for result in [
            hub.rename_section(&kitchen, "Mine now", &sam),
            hub.delete_section(&kitchen, &sam),
            hub.move_bot("bank", Some(&kitchen), &sam),
        ] {
            assert!(
                matches!(result, Err(HubError::NotAllowed(ref m)) if m == "No such section."),
                "{result:?}"
            );
        }
        assert_eq!(view(&hub, &jay).0, ["Kitchen", "Money"]);
        assert_eq!(view(&hub, &sam), (vec!["Kitchen".to_owned()], vec![]));

        // Renaming keeps the bots; deleting puts them back, never deletes them.
        hub.rename_section(&kitchen, "Food", &jay).unwrap();
        assert_eq!(
            view(&hub, &jay).1,
            [("recipes".to_owned(), "Food".to_owned())]
        );
        hub.delete_section(&kitchen, &jay).unwrap();
        assert_eq!(view(&hub, &jay), (vec!["Money".to_owned()], vec![]));
        assert!(hub.bot("recipes").is_ok());
        assert_eq!(view(&hub, &sam).0, ["Kitchen"], "Sam's is untouched");
    }

    #[tokio::test]
    async fn any_listed_bot_but_herder_can_be_filed_without_managing_it() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path()).await;
        let (jay, sam) = (Viewer::gate("Jay"), Viewer::gate("Sam"));
        hub.create_section("Mine", None, &sam).unwrap();
        let mine = section_id(&hub, &sam, "Mine");
        // Jay's shared bot: Sam may not change it, but may file it.
        let recipes = hub
            .bots_for(&sam)
            .await
            .unwrap()
            .into_iter()
            .find(|b| b.id == "recipes")
            .unwrap();
        assert!(!recipes.can_manage);
        hub.move_bot("recipes", Some(&mine), &sam).unwrap();
        // The ··· rules still hold for everything that changes the bot.
        assert!(hub.rename_bot("recipes", "Sam's", &sam).await.is_err());
        assert!(hub.set_private("recipes", true, &sam).await.is_err());
        assert!(hub.delete_bot("recipes", &sam).await.is_err());
        let after = hub.bot("recipes").unwrap();
        assert_eq!((after.name.as_str(), after.private), ("Recipes", false));

        // Jay's private bot is locked for Sam, but listed: Sam may file it.
        hub.create_section("Other", Some("bank"), &sam).unwrap();
        assert_eq!(
            view(&hub, &sam).1,
            [
                ("bank".to_owned(), "Other".to_owned()),
                ("recipes".to_owned(), "Mine".to_owned())
            ]
        );
        assert!(hub.bot_for("bank", &sam).is_err(), "still locked");

        // Out again.
        hub.move_bot("recipes", None, &sam).unwrap();
        hub.move_bot("bank", None, &sam).unwrap();
        assert_eq!(view(&hub, &sam).1, []);

        // Herder stays at the top; unknown bots aren't filed.
        let herder = hub.move_bot("assistant", Some(&mine), &Viewer::farm("Jay"));
        assert!(matches!(herder, Err(HubError::NotAllowed(ref m)) if m.contains("Assistant")));
        let sams_herder = hub.herder_for("Sam").unwrap();
        assert!(
            hub.move_bot(sams_herder.as_str(), Some(&mine), &sam)
                .is_err()
        );
        assert!(matches!(
            hub.move_bot(sams_herder.as_str(), None, &jay),
            Err(HubError::UnknownBot(_))
        ));
        assert!(matches!(
            hub.move_bot("nobody", Some(&mine), &sam),
            Err(HubError::UnknownBot(_))
        ));
        assert!(hub.create_section("H", Some("assistant"), &sam).is_err());
        assert_eq!(
            view(&hub, &sam).0,
            ["Mine", "Other"],
            "nothing made on a refusal"
        );
    }

    #[tokio::test]
    async fn names_are_checked_and_sections_are_limited() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path()).await;
        let jay = Viewer::gate("Jay");
        for bad in ["", "   ", "two\nlines", "tab\there", &"x".repeat(41)] {
            assert!(hub.create_section(bad, None, &jay).is_err(), "{bad:?}");
        }
        hub.create_section(&format!("  {}  ", "é".repeat(40)), None, &jay)
            .unwrap();
        hub.create_section("Kitchen", None, &jay).unwrap();
        let taken = hub.create_section("KITCHEN", None, &jay);
        assert!(
            matches!(taken, Err(HubError::NotAllowed(ref m)) if m.contains("already")),
            "{taken:?}"
        );
        let caps = hub.create_section(&"É".repeat(40), None, &jay);
        assert!(caps.is_err(), "case is ignored beyond ASCII too");
        let kitchen = section_id(&hub, &jay, "Kitchen");
        hub.rename_section(&kitchen, "kitchen", &jay).unwrap();
        assert!(hub.rename_section(&kitchen, " ", &jay).is_err());
        for i in 2..MAX_SECTIONS {
            hub.create_section(&format!("S{i}"), None, &jay).unwrap();
        }
        let more = hub.create_section("One more", None, &jay);
        assert!(
            matches!(more, Err(HubError::NotAllowed(ref m)) if m.contains("most")),
            "{more:?}"
        );
        assert_eq!(view(&hub, &jay).0.len(), MAX_SECTIONS);
    }

    #[tokio::test]
    async fn someone_oxbot_cant_place_has_no_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path()).await;
        let nobody = Viewer::gate("");
        assert!(hub.sections_for(&nobody).is_none());
        let err = hub.create_section("Kitchen", None, &nobody);
        assert!(
            matches!(err, Err(HubError::NotAllowed(ref m)) if m.contains("know who you are")),
            "{err:?}"
        );
        assert!(hub.move_bot("recipes", None, &nobody).is_err());
    }

    #[tokio::test]
    async fn each_change_goes_to_its_person_and_a_deleted_bot_leaves_every_section() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = hub(tmp.path()).await;
        let (jay, sam) = (Viewer::gate("Jay"), Viewer::gate("Sam"));
        let jays_id = hub.resolve(&jay).id.clone().unwrap().to_string();
        let sams_id = hub.resolve(&sam).id.clone().unwrap().to_string();
        let mut updates = hub.subscribe();
        hub.create_section("Kitchen", Some("recipes"), &jay)
            .unwrap();
        let Ok(ServerMsg::Sections {
            owner_id, placed, ..
        }) = updates.recv().await
        else {
            panic!("expected sections");
        };
        assert_eq!(owner_id.as_deref(), Some(jays_id.as_str()));
        assert_eq!(placed.len(), 1);
        hub.create_section("Food", Some("recipes"), &sam).unwrap();
        let _ = updates.recv().await;

        // Deleting Recipes takes it out of both people's sections, and each
        // of them is told.
        hub.delete_bot("recipes", &Viewer::farm("Jay"))
            .await
            .unwrap();
        let mut told = Vec::new();
        while let Ok(msg) = updates.try_recv() {
            if let ServerMsg::Sections {
                owner_id, placed, ..
            } = msg
            {
                assert!(placed.is_empty());
                told.extend(owner_id);
            }
        }
        told.sort();
        let mut want = vec![jays_id, sams_id];
        want.sort();
        assert_eq!(told, want);
        assert_eq!(view(&hub, &jay), (vec!["Kitchen".to_owned()], vec![]));

        // A new bot with the same id lands in nobody's section.
        hub.create_bot_with("Recipes", "", Some("Ann"), true)
            .await
            .unwrap();
        assert!(view(&hub, &jay).1.is_empty() && view(&hub, &sam).1.is_empty());
    }
}
