//! People: who oxbot knows, by id, with their color, browser folder and
//! Herder.

use oxbot_protocol::Person;
use rusqlite::{Connection, OptionalExtension, params};

use super::{PEOPLE_COLORS, PersonRecord, Store, StoreError, now_ms};
use crate::bot::{personal_herder_id, personal_herder_id_for};
use crate::id::{BotId, PersonId};

pub(super) fn color_name(color: i64) -> String {
    let i = usize::try_from(color).unwrap_or(0) % PEOPLE_COLORS.len();
    PEOPLE_COLORS[i].to_owned()
}

pub(super) fn person(name: &str, color: i64) -> Person {
    Person {
        name: name.to_owned(),
        color: color_name(color),
        role: String::new(),
        photo: None,
    }
}

/// The folder name for a person's own browser, as oxbot made it before
/// people had ids: their name made safe for a folder, plus a short hash of
/// the exact name, so `Sam` and `sam` never share sign-ins. People from then
/// keep theirs (their sign-ins are in it); newer people's is their id.
pub(crate) fn person_folder(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "person" } else { safe };
    // FNV-1a: stable across runs and Rust versions, unlike `DefaultHasher`.
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!(
        "{}-{:08x}",
        safe.chars().take(32).collect::<String>(),
        hash >> 32
    )
}

pub(super) const PERSON_COLUMNS: &str = "id, name, color, folder, herder, farm";

pub(super) fn person_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PersonRecord> {
    let id: String = r.get(0)?;
    let name: String = r.get(1)?;
    let bad = |e: String| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    };
    let id = PersonId::new(id).map_err(|e| bad(e.to_string()))?;
    // Filled at open; the old derivations are what an older oxbot used.
    let folder: Option<String> = r.get(3)?;
    let herder: Option<String> = r.get(4)?;
    let herder = herder
        .and_then(|h| BotId::new(h).ok())
        .unwrap_or_else(|| personal_herder_id(&name));
    Ok(PersonRecord {
        folder: folder.unwrap_or_else(|| person_folder(&name)),
        herder,
        color: color_name(r.get(2)?),
        farm: r.get::<_, i64>(5)? != 0,
        id,
        name,
    })
}

/// The first color nobody has yet (or, once all are taken, the next in turn).
pub(super) fn next_color(conn: &Connection) -> Result<i64, StoreError> {
    let mut stmt = conn.prepare("SELECT color FROM people")?;
    let taken: Vec<i64> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let n = i64::try_from(PEOPLE_COLORS.len()).unwrap_or(12);
    Ok((0..n)
        .find(|c| !taken.contains(c))
        .unwrap_or_else(|| i64::try_from(taken.len()).unwrap_or(0) % n))
}

pub(super) fn find_person(
    conn: &Connection,
    by: &str,
    key: &str,
) -> Result<Option<PersonRecord>, StoreError> {
    Ok(conn
        .query_row(
            &format!("SELECT {PERSON_COLUMNS} FROM people WHERE {by} = ?1"),
            [key],
            person_row,
        )
        .optional()?)
}

/// The person called exactly `name`, made if there's none. The flag says
/// whether they're new. A new person's browser folder is their id, and
/// their Herder's id comes from their name unless someone has it already.
pub(super) fn find_or_create(
    conn: &Connection,
    name: &str,
) -> Result<(PersonRecord, bool), StoreError> {
    find_or_create_with(conn, name, false)
}

/// Like [`find_or_create`]; with `legacy`, someone new gets the browser
/// folder an older oxbot made for that name (see [`person_folder`]) unless
/// someone has it already, so sign-ins made before person ids are theirs.
pub(super) fn find_or_create_with(
    conn: &Connection,
    name: &str,
    legacy: bool,
) -> Result<(PersonRecord, bool), StoreError> {
    if let Some(found) = find_person(conn, "name", name)? {
        return Ok((found, false));
    }
    let id = PersonId::random();
    let taken = |column: &str, value: &str| -> Result<bool, StoreError> {
        Ok(conn.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM people WHERE {column} = ?1)"),
            [value],
            |r| r.get(0),
        )?)
    };
    let by_name = personal_herder_id(name);
    let herder = if taken("herder", by_name.as_str())? {
        personal_herder_id_for(name, &id)
    } else {
        by_name
    };
    let old_folder = person_folder(name);
    let folder = if legacy && !taken("folder", &old_folder)? {
        old_folder
    } else {
        id.as_str().to_owned()
    };
    conn.execute(
        "INSERT INTO people (id, name, color, first_ms, folder, herder) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id.as_str(), name, next_color(conn)?, now_ms(), folder, herder.as_str()],
    )?;
    let made = find_person(conn, "id", id.as_str())?
        .ok_or(StoreError::Sqlite(rusqlite::Error::QueryReturnedNoRows))?;
    Ok((made, true))
}

/// Gives people added without a folder or Herder (by an older oxbot, or a
/// test seed) the ones that older oxbot would have used.
pub(super) fn fill_people(conn: &Connection) -> Result<(), StoreError> {
    let mut stmt =
        conn.prepare("SELECT id, name FROM people WHERE folder IS NULL OR herder IS NULL")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (id, name) in rows {
        // Each folder and Herder belongs to one person (migration 11): if
        // the old one is someone else's, they get one from their id.
        let taken = |column: &str, value: &str| -> Result<bool, StoreError> {
            Ok(conn.query_row(
                &format!("SELECT EXISTS (SELECT 1 FROM people WHERE {column} = ?1 AND id <> ?2)"),
                params![value, id],
                |r| r.get(0),
            )?)
        };
        let folder = person_folder(&name);
        let folder = if taken("folder", &folder)? {
            id.clone()
        } else {
            folder
        };
        let herder = personal_herder_id(&name);
        let herder = match PersonId::new(&id) {
            Ok(pid) if taken("herder", herder.as_str())? => personal_herder_id_for(&name, &pid),
            _ => herder,
        };
        conn.execute(
            "UPDATE people SET folder = COALESCE(folder, ?2), herder = COALESCE(herder, ?3) WHERE id = ?1",
            params![id, folder, herder.as_str()],
        )?;
    }
    Ok(())
}

impl Store {
    /// The person called `name`, giving them the first color nobody has yet
    /// if they're new. The flag says whether they're new.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person(&self, name: &str) -> Result<(Person, bool), StoreError> {
        let (found, new) = self.find_or_create_person(name)?;
        Ok((found.person(), new))
    }

    /// The person called exactly `name`, made if there's none. The flag says
    /// whether they're new.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn find_or_create_person(&self, name: &str) -> Result<(PersonRecord, bool), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let found = find_or_create(&tx, name)?;
        tx.commit()?;
        Ok(found)
    }

    /// Like [`Store::find_or_create_person`], for a name found in a bot
    /// made before person ids: someone new keeps the browser folder an older
    /// oxbot used for that name (if nobody has it), so their sign-ins carry
    /// over, and the Herder id it used (if free).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn find_or_create_legacy_person(
        &self,
        name: &str,
    ) -> Result<(PersonRecord, bool), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let found = find_or_create_with(&tx, name, true)?;
        tx.commit()?;
        Ok(found)
    }

    /// The person whose own Herder is `herder`, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_by_herder(&self, herder: &BotId) -> Result<Option<PersonRecord>, StoreError> {
        find_person(&self.conn(), "herder", herder.as_str())
    }

    /// The person with this id, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_by_id(&self, id: &PersonId) -> Result<Option<PersonRecord>, StoreError> {
        find_person(&self.conn(), "id", id.as_str())
    }

    /// The person called exactly `name`, if any.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_by_name(&self, name: &str) -> Result<Option<PersonRecord>, StoreError> {
        find_person(&self.conn(), "name", name)
    }

    /// Everyone oxbot knows, in the order it met them.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn person_records(&self) -> Result<Vec<PersonRecord>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PERSON_COLUMNS} FROM people ORDER BY first_ms, rowid"
        ))?;
        let rows = stmt.query_map([], person_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The Farm's own person, once chosen.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn farm_person(&self) -> Result<Option<PersonRecord>, StoreError> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {PERSON_COLUMNS} FROM people WHERE farm = 1 ORDER BY rowid LIMIT 1"
                ),
                [],
                person_row,
            )
            .optional()?)
    }

    /// Makes `id` the Farm's own person (and nobody else).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_farm_person(&self, id: &PersonId) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        // Two steps: SQLite checks the one-Farm index row by row.
        tx.execute(
            "UPDATE people SET farm = 0 WHERE farm <> 0 AND id <> ?1",
            [id.as_str()],
        )?;
        tx.execute("UPDATE people SET farm = 1 WHERE id = ?1", [id.as_str()])?;
        tx.commit()?;
        Ok(())
    }

    /// Changes a person's name. Returns `false` (and changes nothing) if
    /// someone else has that name.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn rename_person(&self, id: &PersonId, name: &str) -> Result<bool, StoreError> {
        let conn = self.conn();
        if find_person(&conn, "name", name)?.is_some_and(|p| &p.id != id) {
            return Ok(false);
        }
        conn.execute(
            "UPDATE people SET name = ?2 WHERE id = ?1",
            params![id.as_str(), name],
        )?;
        Ok(true)
    }

    /// Everyone who has sent a message, in the order oxbot first saw them.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn people(&self) -> Result<Vec<Person>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.name, p.color, COALESCE(f.role, CASE WHEN p.farm = 1 THEN 'owner' ELSE 'adult' END), f.photo
             FROM people p LEFT JOIN profiles f ON f.person = p.id
             ORDER BY p.first_ms, p.rowid",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Person {
                role: r.get(2)?,
                photo: r.get(3)?,
                ..person(&r.get::<_, String>(0)?, r.get(1)?)
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
