//! People's profiles (`profiles`, migration 26): their role, a photo, and
//! what bots may know about them, each with whether bots may use it.

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError, now_ms};
use crate::id::PersonId;

/// Who someone is in the home.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Role {
    /// Installed oxbot and connected the home's AI account (18 or older).
    Owner,
    /// Anyone else grown up.
    #[default]
    Adult,
    /// A child: a parent answers their requests (kid accounts come later).
    Kid,
}

impl Role {
    /// As stored: `owner`, `adult`, `kid`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Adult => "adult",
            Self::Kid => "kid",
        }
    }

    fn parse(text: &str) -> Self {
        match text {
            "owner" => Self::Owner,
            "kid" => Self::Kid,
            _ => Self::Adult,
        }
    }
}

/// What a person filled in. Every field may be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    /// A small picture, as a `data:image/jpeg;base64,…` URL.
    pub photo: Option<String>,
    /// Food needs, e.g. `vegetarian; no nuts`.
    pub food: String,
    /// Bots may use `food`.
    pub food_use: bool,
    /// Languages, e.g. `English, Hindi`.
    pub languages: String,
    /// Bots may use `languages`.
    pub languages_use: bool,
    /// Notes for bots, e.g. `I prefer metric`.
    pub notes: String,
    /// Bots may use `notes`.
    pub notes_use: bool,
}

/// A profile as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRecord {
    /// Whose.
    pub person: PersonId,
    /// Their role.
    pub role: Role,
    /// Their age, for a kid account.
    pub age: Option<u32>,
    /// What they filled in.
    pub profile: Profile,
    /// They chose their name themselves (the Mac account's name no longer
    /// renames the Farm's person).
    pub name_set: bool,
    /// The welcome screen shows until they save it.
    pub welcome_due: bool,
    /// When the owner confirmed they're 18 or older and accept
    /// `OpenRouter`'s terms.
    pub owner_ok_ms: Option<i64>,
}

const COLUMNS: &str = "person, role, age, photo, food, food_use, languages, languages_use, notes, notes_use, name_set, welcome_due, owner_ok_ms";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProfileRecord> {
    let person: String = r.get(0)?;
    let person = PersonId::new(person).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            e.to_string().into(),
        )
    })?;
    Ok(ProfileRecord {
        person,
        role: Role::parse(&r.get::<_, String>(1)?),
        age: r
            .get::<_, Option<i64>>(2)?
            .and_then(|a| u32::try_from(a).ok()),
        profile: Profile {
            photo: r.get(3)?,
            food: r.get(4)?,
            food_use: r.get::<_, i64>(5)? != 0,
            languages: r.get(6)?,
            languages_use: r.get::<_, i64>(7)? != 0,
            notes: r.get(8)?,
            notes_use: r.get::<_, i64>(9)? != 0,
        },
        name_set: r.get::<_, i64>(10)? != 0,
        welcome_due: r.get::<_, i64>(11)? != 0,
        owner_ok_ms: r.get(12)?,
    })
}

impl Store {
    /// `person`'s profile, if they have one (people from before profiles
    /// have an empty one; some made since have none until they need it).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn profile(&self, person: &PersonId) -> Result<Option<ProfileRecord>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM profiles WHERE person = ?1"),
                [person.as_str()],
                row,
            )
            .optional()?)
    }

    /// Everyone's profiles.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn profiles(&self) -> Result<Vec<ProfileRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM profiles"))?;
        let rows = stmt.query_map([], row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Gives `person` an empty profile with `role`, if they have none;
    /// `welcome`: their welcome screen is due. Returns whether it was made.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn start_profile(
        &self,
        person: &PersonId,
        role: Role,
        welcome: bool,
    ) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "INSERT OR IGNORE INTO profiles (person, role, welcome_due, updated_ms) VALUES (?1, ?2, ?3, ?4)",
            params![person.as_str(), role.as_str(), welcome, now_ms()],
        )? > 0)
    }

    /// Sets `person`'s role (making their profile if they have none).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_role(&self, person: &PersonId, role: Role) -> Result<(), StoreError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO profiles (person, role, updated_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT(person) DO UPDATE SET role = excluded.role, updated_ms = excluded.updated_ms",
            params![person.as_str(), role.as_str(), now_ms()],
        )?;
        Ok(())
    }

    /// Saves what `person` filled in, and that their welcome is done
    /// (making their profile if they have none). `owner_ok`: they confirmed
    /// what the owner must (kept from the first time).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn save_profile(
        &self,
        person: &PersonId,
        p: &Profile,
        owner_ok: bool,
    ) -> Result<(), StoreError> {
        let now = now_ms();
        let ok = owner_ok.then_some(now);
        self.conn().execute(
            "INSERT INTO profiles (person, photo, food, food_use, languages, languages_use, notes, notes_use, welcome_due, owner_ok_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10)
             ON CONFLICT(person) DO UPDATE SET
               photo = excluded.photo, food = excluded.food, food_use = excluded.food_use,
               languages = excluded.languages, languages_use = excluded.languages_use,
               notes = excluded.notes, notes_use = excluded.notes_use, welcome_due = 0,
               owner_ok_ms = COALESCE(profiles.owner_ok_ms, excluded.owner_ok_ms),
               updated_ms = excluded.updated_ms",
            params![
                person.as_str(),
                p.photo,
                p.food,
                p.food_use,
                p.languages,
                p.languages_use,
                p.notes,
                p.notes_use,
                ok,
                now
            ],
        )?;
        Ok(())
    }

    /// Notes that `person` chose their name themselves.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_name_chosen(&self, person: &PersonId) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO profiles (person, name_set, updated_ms) VALUES (?1, 1, ?2)
             ON CONFLICT(person) DO UPDATE SET name_set = 1, updated_ms = excluded.updated_ms",
            params![person.as_str(), now_ms()],
        )?;
        Ok(())
    }

    /// Gives `person` the color numbered `color` in oxbot's list.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_person_color(&self, person: &PersonId, color: usize) -> Result<(), StoreError> {
        self.conn().execute(
            "UPDATE people SET color = ?2 WHERE id = ?1",
            params![person.as_str(), i64::try_from(color).unwrap_or(0)],
        )?;
        Ok(())
    }

    /// Whether oxbot knows nobody yet (a first start).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn nobody_yet(&self) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT NOT EXISTS (SELECT 1 FROM people)", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_starts_empty_and_saving_it_ends_the_welcome() {
        let store = Store::open_in_memory().unwrap();
        let (sam, _) = store.find_or_create_person("Sam").unwrap();
        assert_eq!(
            store.profile(&sam.id).unwrap(),
            None,
            "made after profiles: none yet"
        );
        assert!(store.start_profile(&sam.id, Role::Adult, true).unwrap());
        assert!(
            !store.start_profile(&sam.id, Role::Owner, false).unwrap(),
            "only once"
        );
        let p = store.profile(&sam.id).unwrap().unwrap();
        assert!(p.welcome_due && p.role == Role::Adult && p.profile.food_use);
        let filled = Profile {
            food: "vegetarian".into(),
            food_use: true,
            notes: "metric".into(),
            notes_use: false,
            ..Profile::default()
        };
        store.save_profile(&sam.id, &filled, true).unwrap();
        let p = store.profile(&sam.id).unwrap().unwrap();
        assert!(!p.welcome_due && p.profile == filled);
        let first = p.owner_ok_ms.unwrap();
        store.save_profile(&sam.id, &filled, true).unwrap();
        assert_eq!(
            store.profile(&sam.id).unwrap().unwrap().owner_ok_ms,
            Some(first),
            "the first time is kept"
        );
        store.set_role(&sam.id, Role::Kid).unwrap();
        store.set_name_chosen(&sam.id).unwrap();
        let p = store.profile(&sam.id).unwrap().unwrap();
        assert!(p.role == Role::Kid && p.name_set);
        assert!(!store.nobody_yet().unwrap());
        assert!(Store::open_in_memory().unwrap().nobody_yet().unwrap());
    }

    #[test]
    fn people_from_before_profiles_have_one_with_nothing_due() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("oxbot.db");
        Store::open_at_version(&db, 25).unwrap();
        {
            let store = Store::open_at_version(&db, 25);
            assert!(store.is_ok());
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "INSERT INTO people (id, name, color, first_ms, folder, herder, farm) VALUES ('p_00000000000000a1', 'Jay', 0, 1, 'jay', 'assistant--jay-1', 1), ('p_00000000000000b2', 'Sam', 1, 2, 'sam', 'assistant--sam-2', 0)",
                [],
            )
            .unwrap();
        }
        let store = Store::open(&db).unwrap();
        let jay = store
            .profile(&PersonId::new("p_00000000000000a1").unwrap())
            .unwrap()
            .unwrap();
        let sam = store
            .profile(&PersonId::new("p_00000000000000b2").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!((jay.role, jay.welcome_due), (Role::Owner, false));
        assert_eq!((sam.role, sam.welcome_due), (Role::Adult, false));
    }
}
