//! Family Notes (migration 31): files the family keeps, each with what
//! makes it easy to find again: a title, a day, a topic, tags and who it's
//! about. Every note has a day: one a person gave, one read from the
//! document, or else the day it was uploaded ([`DayFrom`]).
//!
//! The file is kept in the `family-notes` folder, at [`HomeNote::file`];
//! this only records where (see `Hub::file_notes`).

use rusqlite::{OptionalExtension as _, Row, params};

use super::{Actor, Store, StoreError, now_ms};
use crate::id::PersonId;

/// Where a note's day came from, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayFrom {
    /// A person gave it.
    You,
    /// The AI read it from the document (its date, a bill's, a visit's).
    Document,
    /// None was given: the day it was uploaded.
    Upload,
}

impl DayFrom {
    /// As stored and shown: `you`, `document` or `upload`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::You => "you",
            Self::Document => "document",
            Self::Upload => "upload",
        }
    }

    /// From [`DayFrom::as_str`]; anything else is an upload day.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "you" => Self::You,
            "document" => Self::Document,
            _ => Self::Upload,
        }
    }
}

/// What a note says: what a person or the bot can set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFields {
    /// What it is ("Akhila's blood test").
    pub title: String,
    /// Its day, `2026-11-17`.
    pub day: String,
    /// Where the day came from.
    pub day_from: DayFrom,
    /// Its topic, a folder ("Medical"); empty: Unsorted.
    pub topic: String,
    /// Words to find it by.
    pub tags: Vec<String>,
    /// Who it's about.
    pub people: Vec<PersonId>,
    /// A line about what's in it (only from what the AI read, or a person wrote).
    pub summary: String,
}

impl NoteFields {
    /// The fields checked and tidied: a title, a day written `2026-11-17`,
    /// tags without repeats.
    ///
    /// # Errors
    /// No title, or the day isn't a day.
    pub fn checked(mut self) -> Result<Self, StoreError> {
        self.title = one_line(&self.title);
        self.topic = one_line(&self.topic);
        self.summary = self.summary.trim().to_owned();
        if self.title.is_empty() {
            return Err(StoreError::Invalid("A note needs a title.".to_owned()));
        }
        if !is_day(&self.day) {
            return Err(StoreError::Invalid(
                "A note's day is written 2026-11-17.".to_owned(),
            ));
        }
        let mut tags: Vec<String> = Vec::new();
        for t in &self.tags {
            let t = one_line(t);
            if !t.is_empty() && !tags.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                tags.push(t);
            }
        }
        self.tags = tags;
        Ok(self)
    }
}

/// Whether `t` is a day written `2026-11-17` (a real month and day).
#[must_use]
pub fn is_day(t: &str) -> bool {
    let b = t.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| t.get(r).and_then(|s| s.parse::<u32>().ok());
    matches!((num(0..4), num(5..7), num(8..10)), (Some(_), Some(m), Some(d)) if (1..=12).contains(&m) && (1..=31).contains(&d))
}

fn one_line(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A note in Family Notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeNote {
    /// Its id.
    pub id: i64,
    /// What it says.
    pub fields: NoteFields,
    /// The file, below the `family-notes` folder
    /// (`2026/Medical/2026-11-17 Blood test.pdf`).
    pub file: String,
    /// Its name as uploaded.
    pub name: String,
    /// What kind of file (an `AttachmentKind`, as JSON writes it: `pdf`).
    pub kind: String,
    /// Its size in bytes.
    pub size: u64,
    /// Kept private: the AI never read it.
    pub private: bool,
    /// When it was uploaded.
    pub uploaded_ms: i64,
    /// Who uploaded it.
    pub uploaded_by: Option<Actor>,
    /// Who changed it last.
    pub changed_by: Option<Actor>,
    /// When it was last changed.
    pub updated_ms: i64,
    /// When it was deleted, if it was.
    pub deleted_ms: Option<i64>,
}

/// A note about to be kept: its fields, and the file's.
#[derive(Debug, Clone)]
pub struct NewNote {
    /// What it says.
    pub fields: NoteFields,
    /// Where the file is, below `family-notes`.
    pub file: String,
    /// Its name as uploaded.
    pub name: String,
    /// Its kind.
    pub kind: String,
    /// Its size.
    pub size: u64,
    /// Kept private.
    pub private: bool,
}

const COLUMNS: &str = "id, title, day, day_from, topic, tags, people, summary, file, name, kind, \
                       size, private, uploaded_ms, uploaded_by, changed_by, updated_ms, deleted_ms";

fn note_row(r: &Row<'_>) -> rusqlite::Result<HomeNote> {
    let json = |i: usize| -> rusqlite::Result<Vec<String>> {
        Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or_default())
    };
    let actor = |i: usize| -> rusqlite::Result<Option<Actor>> {
        Ok(Actor::from_text(&r.get::<_, String>(i)?))
    };
    Ok(HomeNote {
        id: r.get(0)?,
        fields: NoteFields {
            title: r.get(1)?,
            day: r.get(2)?,
            day_from: DayFrom::parse(&r.get::<_, String>(3)?),
            topic: r.get(4)?,
            tags: json(5)?,
            people: json(6)?
                .into_iter()
                .filter_map(|p| PersonId::new(p).ok())
                .collect(),
            summary: r.get(7)?,
        },
        file: r.get(8)?,
        name: r.get(9)?,
        kind: r.get(10)?,
        size: u64::try_from(r.get::<_, i64>(11)?).unwrap_or(0),
        private: r.get(12)?,
        uploaded_ms: r.get(13)?,
        uploaded_by: actor(14)?,
        changed_by: actor(15)?,
        updated_ms: r.get(16)?,
        deleted_ms: r.get(17)?,
    })
}

fn people_json(people: &[PersonId]) -> String {
    let ids: Vec<&str> = people.iter().map(PersonId::as_str).collect();
    serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_owned())
}

impl Store {
    /// Keeps a new note, uploaded by `by`.
    ///
    /// # Errors
    /// The fields aren't valid, or SQLite fails.
    pub fn add_note(&self, note: NewNote, by: &Actor) -> Result<HomeNote, StoreError> {
        let f = note.fields.checked()?;
        let now = now_ms();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO home_notes (title, day, day_from, topic, tags, people, summary, file, name,
                                     kind, size, private, uploaded_ms, uploaded_by, changed_by, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?13)",
            params![
                f.title,
                f.day,
                f.day_from.as_str(),
                f.topic,
                serde_json::to_string(&f.tags).unwrap_or_else(|_| "[]".to_owned()),
                people_json(&f.people),
                f.summary,
                note.file,
                note.name,
                note.kind,
                i64::try_from(note.size).unwrap_or(i64::MAX),
                note.private,
                now,
                by.to_text(),
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.note(id)
    }

    /// Changes note `id`'s fields, and where its file now is.
    ///
    /// # Errors
    /// The fields aren't valid, the note isn't there, or SQLite fails.
    pub fn change_note(
        &self,
        id: i64,
        fields: NoteFields,
        file: &str,
        by: &Actor,
    ) -> Result<HomeNote, StoreError> {
        let f = fields.checked()?;
        let changed = self.conn().execute(
            "UPDATE home_notes SET title = ?2, day = ?3, day_from = ?4, topic = ?5, tags = ?6,
               people = ?7, summary = ?8, file = ?9, changed_by = ?10, updated_ms = ?11
             WHERE id = ?1 AND deleted_ms IS NULL",
            params![
                id,
                f.title,
                f.day,
                f.day_from.as_str(),
                f.topic,
                serde_json::to_string(&f.tags).unwrap_or_else(|_| "[]".to_owned()),
                people_json(&f.people),
                f.summary,
                file,
                by.to_text(),
                now_ms(),
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::Invalid(format!("There's no note {id}.")));
        }
        self.note(id)
    }

    /// Note `id`, deleted or not.
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn note(&self, id: i64) -> Result<HomeNote, StoreError> {
        self.conn()
            .query_row(
                &format!("SELECT {COLUMNS} FROM home_notes WHERE id = ?1"),
                [id],
                note_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid(format!("There's no note {id}.")))
    }

    /// Every note not deleted, latest day first.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn notes(&self) -> Result<Vec<HomeNote>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM home_notes WHERE deleted_ms IS NULL ORDER BY day DESC, id DESC"
        ))?;
        let rows = stmt.query_map([], note_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Deletes note `id` (Recently deleted), or brings it back (`deleted`
    /// false), keeping its file at `file`.
    ///
    /// # Errors
    /// The note isn't there (or is already that way), or SQLite fails.
    pub fn set_note_deleted(
        &self,
        id: i64,
        deleted: bool,
        file: &str,
        by: &Actor,
    ) -> Result<HomeNote, StoreError> {
        let now = now_ms();
        let changed = self.conn().execute(
            "UPDATE home_notes SET deleted_ms = ?2, file = ?3, changed_by = ?4, updated_ms = ?5
             WHERE id = ?1 AND (deleted_ms IS NULL) = ?6",
            params![id, deleted.then_some(now), file, by.to_text(), now, deleted],
        )?;
        if changed == 0 {
            return Err(StoreError::Invalid(format!(
                "Note {id} isn't {}.",
                if deleted { "there" } else { "deleted" }
            )));
        }
        self.note(id)
    }

    /// Notes deleted before `before_ms`: gone for good (their files are the
    /// caller's to remove). Returns them.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn purge_deleted_notes(&self, before_ms: i64) -> Result<Vec<HomeNote>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM home_notes WHERE deleted_ms < ?1"
        ))?;
        let gone: Vec<HomeNote> = stmt
            .query_map([before_ms], note_row)?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        conn.execute("DELETE FROM home_notes WHERE deleted_ms < ?1", [before_ms])?;
        Ok(gone)
    }

    /// Whether some note other than `id` keeps its file at `file`.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn note_file_taken(&self, file: &str, id: i64) -> Result<bool, StoreError> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) > 0 FROM home_notes WHERE file = ?1 AND id != ?2",
            params![file, id],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bill() -> NewNote {
        NewNote {
            fields: NoteFields {
                title: "  Hydro   bill ".into(),
                day: "2026-11-02".into(),
                day_from: DayFrom::Document,
                topic: "Bills".into(),
                tags: vec!["hydro".into(), "Hydro".into(), " ".into()],
                people: vec![],
                summary: String::new(),
            },
            file: "2026/Bills/2026-11-02 Hydro bill.pdf".into(),
            name: "bill.pdf".into(),
            kind: "pdf".into(),
            size: 10,
            private: false,
        }
    }

    #[test]
    fn a_note_is_kept_tidied_and_changed() {
        let store = Store::open_in_memory().unwrap();
        let maya = Actor::Person(PersonId::random());
        let note = store.add_note(bill(), &maya).unwrap();
        assert_eq!(note.fields.title, "Hydro bill");
        assert_eq!(note.fields.tags, ["hydro"]);
        assert_eq!(note.uploaded_by, Some(maya.clone()));
        assert!(store.note_file_taken(&note.file, 0).unwrap());
        assert!(!store.note_file_taken(&note.file, note.id).unwrap());
        let changed = store
            .change_note(
                note.id,
                NoteFields {
                    day: "2026-10-30".into(),
                    day_from: DayFrom::You,
                    ..note.fields.clone()
                },
                "2026/Bills/2026-10-30 Hydro bill.pdf",
                &maya,
            )
            .unwrap();
        assert_eq!(changed.fields.day_from, DayFrom::You);
        assert_eq!(store.notes().unwrap().len(), 1);
    }

    #[test]
    fn a_note_needs_a_title_and_a_real_day() {
        let store = Store::open_in_memory().unwrap();
        let by = Actor::Person(PersonId::random());
        for (title, day) in [
            ("", "2026-11-02"),
            ("Bill", "last week"),
            ("Bill", "2026-13-02"),
        ] {
            let mut n = bill();
            n.fields.title = title.into();
            n.fields.day = day.into();
            assert!(matches!(
                store.add_note(n, &by),
                Err(StoreError::Invalid(_))
            ));
        }
    }
}
