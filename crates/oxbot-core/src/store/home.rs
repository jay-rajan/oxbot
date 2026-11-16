//! The family Home's calendar (migration 30): events everyone in the home
//! can see and change, and a log of every change so a bot's change can be
//! undone. Deleted events stay for [`KEEP_DELETED_DAYS`] ("Recently
//! deleted") before they're gone for good.
//!
//! Times are the home's local time, as written (`2026-11-17T17:30`, or a
//! day, `2026-11-20`, for an all-day event): a weekly event stays at 5:30
//! across a clock change, and every device in the home shows the same.

use rusqlite::{OptionalExtension as _, Row, Transaction, params};
use serde::{Deserialize, Serialize};

use super::{Store, StoreError, now_ms};
use crate::id::{BotId, PersonId};

/// How long a deleted event can be brought back.
pub const KEEP_DELETED_DAYS: i64 = 30;

/// Who made a change: a person, or a bot (acting for someone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A person, by id.
    Person(PersonId),
    /// A bot, by id.
    Bot(BotId),
}

impl Actor {
    fn to_text(&self) -> String {
        match self {
            Self::Person(p) => format!("p:{p}"),
            Self::Bot(b) => format!("b:{b}"),
        }
    }

    /// `None` for text oxbot didn't write.
    fn from_text(text: &str) -> Option<Self> {
        match text.split_once(':')? {
            ("p", id) => PersonId::new(id.to_owned()).ok().map(Self::Person),
            ("b", id) => BotId::new(id.to_owned()).ok().map(Self::Bot),
            _ => None,
        }
    }
}

/// How an event repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Every {
    /// Every day.
    Day,
    /// Every week, on its weekday.
    Week,
    /// Every month, on its day of the month.
    Month,
    /// Every year, on its date.
    Year,
}

/// An event's repeat: how often, and its last day (`2027-06-30`), if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repeat {
    /// How often.
    pub every: Every,
    /// The last day it happens on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

/// What an event says: everything a person or bot can set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFields {
    /// What it is ("Soccer").
    pub title: String,
    /// When it starts, local time (`2026-11-17T17:30`), or its day
    /// (`2026-11-20`) when it's all day.
    pub starts: String,
    /// When it ends, the same way; none: no set end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends: Option<String>,
    /// Where.
    #[serde(default)]
    pub place: String,
    /// Who it's for.
    #[serde(default)]
    pub people: Vec<PersonId>,
    /// More about it.
    #[serde(default)]
    pub notes: String,
    /// How it repeats, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Repeat>,
    /// Remind its people this many minutes before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remind_min: Option<u32>,
    /// What it came from (`meal:12`, `note:5`); empty for one added by hand.
    #[serde(default)]
    pub source: String,
}

impl EventFields {
    /// Whether it's an all-day event (its start is a day, not a time).
    #[must_use]
    pub fn all_day(&self) -> bool {
        !self.starts.contains('T')
    }

    /// The fields checked: a title, and times written the way the store
    /// keeps them.
    fn checked(mut self) -> Result<Self, StoreError> {
        self.title = self.title.trim().to_owned();
        if self.title.is_empty() {
            return Err(StoreError::Invalid("An event needs a title.".to_owned()));
        }
        let day = |t: &str| {
            t.len() == 10
                && t.as_bytes().iter().enumerate().all(|(i, c)| match i {
                    4 | 7 => *c == b'-',
                    _ => c.is_ascii_digit(),
                })
        };
        let time = |t: &str| {
            t.split_once('T').is_some_and(|(d, hm)| {
                day(d)
                    && hm.len() == 5
                    && hm.as_bytes()[2] == b':'
                    && hm.bytes().filter(u8::is_ascii_digit).count() == 4
            })
        };
        let ok = |t: &str| if self.all_day() { day(t) } else { time(t) };
        if !ok(&self.starts) || self.ends.as_deref().is_some_and(|e| !ok(e)) {
            return Err(StoreError::Invalid(
                "Times are written 2026-11-17T17:30, or 2026-11-20 for all day (start and end alike)."
                    .to_owned(),
            ));
        }
        if self
            .ends
            .as_deref()
            .is_some_and(|e| e < self.starts.as_str())
        {
            return Err(StoreError::Invalid(
                "An event can't end before it starts.".to_owned(),
            ));
        }
        if let Some(until) = self.repeat.as_ref().and_then(|r| r.until.as_deref())
            && !day(until)
        {
            return Err(StoreError::Invalid(
                "A repeat's last day is written 2027-06-30.".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// An event on the Home's calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeEvent {
    /// Its id.
    pub id: i64,
    /// What it says.
    pub fields: EventFields,
    /// Who added it.
    pub made_by: Option<Actor>,
    /// Who changed it last.
    pub changed_by: Option<Actor>,
    /// When it was last changed.
    pub updated_ms: i64,
    /// When it was deleted ("Recently deleted"), if it was.
    pub deleted_ms: Option<i64>,
}

/// What a change did, for its Undo line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Added it.
    Add,
    /// Changed it.
    Change,
    /// Deleted it.
    Delete,
    /// Brought it back from Recently deleted.
    Restore,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Change => "change",
            Self::Delete => "delete",
            Self::Restore => "restore",
        }
    }

    fn from_str(text: &str) -> Option<Self> {
        Some(match text {
            "add" => Self::Add,
            "change" => Self::Change,
            "delete" => Self::Delete,
            "restore" => Self::Restore,
            _ => return None,
        })
    }
}

/// One change to the Home, as its log keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeChange {
    /// Its id (what Undo names).
    pub id: i64,
    /// Who made it.
    pub by: Option<Actor>,
    /// What it did.
    pub action: Action,
    /// The event it changed.
    pub event: i64,
    /// When.
    pub ts_ms: i64,
    /// When it was undone, if it was.
    pub undone_ms: Option<i64>,
}

const COLUMNS: &str = "id, title, starts, ends, place, people, notes, repeat, remind_min, \
                       source, made_by, changed_by, updated_ms, deleted_ms";

fn event_row(r: &Row<'_>) -> rusqlite::Result<HomeEvent> {
    let people: String = r.get(5)?;
    let repeat: Option<String> = r.get(7)?;
    let actor = |i: usize| -> rusqlite::Result<Option<Actor>> {
        Ok(Actor::from_text(&r.get::<_, String>(i)?))
    };
    Ok(HomeEvent {
        id: r.get(0)?,
        fields: EventFields {
            title: r.get(1)?,
            starts: r.get(2)?,
            ends: r.get(3)?,
            place: r.get(4)?,
            // Ids oxbot didn't write are dropped rather than trusted.
            people: serde_json::from_str::<Vec<String>>(&people)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|p| PersonId::new(p).ok())
                .collect(),
            notes: r.get(6)?,
            repeat: repeat.and_then(|r| serde_json::from_str(&r).ok()),
            remind_min: r.get(8)?,
            source: r.get(9)?,
        },
        made_by: actor(10)?,
        changed_by: actor(11)?,
        updated_ms: r.get(12)?,
        deleted_ms: r.get(13)?,
    })
}

fn get_event(tx: &Transaction<'_>, id: i64) -> Result<HomeEvent, StoreError> {
    tx.query_row(
        &format!("SELECT {COLUMNS} FROM home_events WHERE id = ?1"),
        [id],
        event_row,
    )
    .optional()?
    .ok_or_else(|| StoreError::Invalid(format!("There's no event {id}.")))
}

/// Writes `fields` over event `id`.
fn write_fields(
    tx: &Transaction<'_>,
    id: i64,
    fields: &EventFields,
    by: &Actor,
    now: i64,
) -> Result<(), StoreError> {
    let people: Vec<&str> = fields.people.iter().map(PersonId::as_str).collect();
    tx.execute(
        "UPDATE home_events SET title = ?2, starts = ?3, ends = ?4, place = ?5, people = ?6,
           notes = ?7, repeat = ?8, remind_min = ?9, source = ?10, changed_by = ?11,
           updated_ms = ?12
         WHERE id = ?1",
        params![
            id,
            fields.title,
            fields.starts,
            fields.ends,
            fields.place,
            serde_json::to_string(&people).unwrap_or_else(|_| "[]".to_owned()),
            fields.notes,
            fields
                .repeat
                .as_ref()
                .and_then(|r| serde_json::to_string(r).ok()),
            fields.remind_min,
            fields.source,
            by.to_text(),
            now,
        ],
    )?;
    Ok(())
}

/// Logs a change to event `id`, with what it was before (none for an add).
fn log(
    tx: &Transaction<'_>,
    by: &Actor,
    action: Action,
    id: i64,
    before: Option<&EventFields>,
    now: i64,
) -> Result<i64, StoreError> {
    tx.execute(
        "INSERT INTO home_changes (ts_ms, by, action, event, before) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            now,
            by.to_text(),
            action.as_str(),
            id,
            before.and_then(|b| serde_json::to_string(b).ok())
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

impl Store {
    /// Adds an event. Returns it and the change (for an Undo line).
    ///
    /// # Errors
    /// The fields aren't valid, or SQLite fails.
    pub fn add_event(
        &self,
        fields: EventFields,
        by: &Actor,
    ) -> Result<(HomeEvent, i64), StoreError> {
        let fields = fields.checked()?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let now = now_ms();
        tx.execute(
            "INSERT INTO home_events (title, starts, made_by, changed_by, created_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?3, ?4, ?4)",
            params![fields.title, fields.starts, by.to_text(), now],
        )?;
        let id = tx.last_insert_rowid();
        write_fields(&tx, id, &fields, by, now)?;
        let change = log(&tx, by, Action::Add, id, None, now)?;
        let event = get_event(&tx, id)?;
        tx.commit()?;
        Ok((event, change))
    }

    /// Changes event `id` to `fields`. Returns it and the change.
    ///
    /// # Errors
    /// The fields aren't valid, the event isn't there (or is deleted), or
    /// SQLite fails.
    pub fn change_event(
        &self,
        id: i64,
        fields: EventFields,
        by: &Actor,
    ) -> Result<(HomeEvent, i64), StoreError> {
        let fields = fields.checked()?;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let before = live(get_event(&tx, id)?)?;
        let now = now_ms();
        write_fields(&tx, id, &fields, by, now)?;
        let change = log(&tx, by, Action::Change, id, Some(&before.fields), now)?;
        let event = get_event(&tx, id)?;
        tx.commit()?;
        Ok((event, change))
    }

    /// Deletes event `id` into Recently deleted. Returns the change.
    ///
    /// # Errors
    /// The event isn't there (or is deleted already), or SQLite fails.
    pub fn delete_event(&self, id: i64, by: &Actor) -> Result<i64, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        live(get_event(&tx, id)?)?;
        let now = now_ms();
        tx.execute(
            "UPDATE home_events SET deleted_ms = ?2, changed_by = ?3, updated_ms = ?2 WHERE id = ?1",
            params![id, now, by.to_text()],
        )?;
        let change = log(&tx, by, Action::Delete, id, None, now)?;
        tx.commit()?;
        Ok(change)
    }

    /// Brings event `id` back from Recently deleted. Returns the change.
    ///
    /// # Errors
    /// The event isn't in Recently deleted, or SQLite fails.
    pub fn restore_event(&self, id: i64, by: &Actor) -> Result<i64, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if get_event(&tx, id)?.deleted_ms.is_none() {
            return Err(StoreError::Invalid(format!("Event {id} isn't deleted.")));
        }
        let now = now_ms();
        tx.execute(
            "UPDATE home_events SET deleted_ms = NULL, changed_by = ?3, updated_ms = ?2 WHERE id = ?1",
            params![id, now, by.to_text()],
        )?;
        let change = log(&tx, by, Action::Restore, id, None, now)?;
        tx.commit()?;
        Ok(change)
    }

    /// Undoes change `change`: an add is deleted, a change put back, a
    /// delete brought back, a restore deleted again. Once only, and only
    /// while it's the event's latest change (a later one would be lost).
    ///
    /// # Errors
    /// It was undone already, the event changed since, or SQLite fails.
    pub fn undo_change(&self, change: i64, by: &Actor) -> Result<HomeEvent, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let found: Option<(String, i64, Option<String>, Option<i64>)> = tx
            .query_row(
                "SELECT action, event, before, undone_ms FROM home_changes WHERE id = ?1",
                [change],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((action, id, before, undone)) = found else {
            return Err(StoreError::Invalid(format!("There's no change {change}.")));
        };
        if undone.is_some() {
            return Err(StoreError::Invalid("That was undone already.".to_owned()));
        }
        let latest: i64 = tx.query_row(
            "SELECT MAX(id) FROM home_changes WHERE event = ?1 AND undone_ms IS NULL",
            [id],
            |r| r.get(0),
        )?;
        if latest != change {
            return Err(StoreError::Invalid(
                "It's been changed since, so this can't be undone.".to_owned(),
            ));
        }
        let now = now_ms();
        let deleted = match Action::from_str(&action) {
            Some(Action::Add | Action::Restore) => Some(now),
            Some(Action::Delete) => None,
            Some(Action::Change) => {
                let before: EventFields = before
                    .and_then(|b| serde_json::from_str(&b).ok())
                    .ok_or_else(|| {
                        StoreError::Invalid("That change can't be undone.".to_owned())
                    })?;
                write_fields(&tx, id, &before, by, now)?;
                get_event(&tx, id)?.deleted_ms
            }
            None => {
                return Err(StoreError::Invalid(
                    "That change can't be undone.".to_owned(),
                ));
            }
        };
        tx.execute(
            "UPDATE home_events SET deleted_ms = ?2, changed_by = ?3, updated_ms = ?4 WHERE id = ?1",
            params![id, deleted, by.to_text(), now],
        )?;
        tx.execute(
            "UPDATE home_changes SET undone_ms = ?2 WHERE id = ?1",
            params![change, now],
        )?;
        let event = get_event(&tx, id)?;
        tx.commit()?;
        Ok(event)
    }

    /// Event `id`, deleted or not.
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn home_event(&self, id: i64) -> Result<HomeEvent, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        get_event(&tx, id)
    }

    /// The events that may happen from day `from` to day `to` (both
    /// `2026-11-16`, `to` included), earliest first: those starting in
    /// that time, those still going, and repeating ones that started
    /// before and haven't ended (the calendar works out their days).
    ///
    /// # Errors
    /// SQLite fails.
    pub fn events_between(&self, from: &str, to: &str) -> Result<Vec<HomeEvent>, StoreError> {
        let conn = self.conn();
        // `to` + "~" sorts after every time on that day.
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM home_events
             WHERE deleted_ms IS NULL AND starts <= ?2 || '~'
               AND (starts >= ?1 OR ends >= ?1
                    OR (repeat IS NOT NULL
                        AND IFNULL(json_extract(repeat, '$.until'), '9999') >= ?1))
             ORDER BY starts, id"
        ))?;
        let rows = stmt.query_map(params![from, to], event_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Recently deleted: events deleted in the last [`KEEP_DELETED_DAYS`],
    /// latest first.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn deleted_events(&self) -> Result<Vec<HomeEvent>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM home_events WHERE deleted_ms >= ?1 ORDER BY deleted_ms DESC, id"
        ))?;
        let since = now_ms() - KEEP_DELETED_DAYS * 86_400_000;
        let rows = stmt.query_map([since], event_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Removes for good what was deleted before `before_ms`, and the log of
    /// changes to it. Returns how many events went.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn purge_deleted_events(&self, before_ms: i64) -> Result<usize, StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM home_changes WHERE event IN
               (SELECT id FROM home_events WHERE deleted_ms < ?1)",
            [before_ms],
        )?;
        let gone = tx.execute("DELETE FROM home_events WHERE deleted_ms < ?1", [before_ms])?;
        tx.commit()?;
        Ok(gone)
    }

    /// Change `id`, as the log keeps it.
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn home_change(&self, id: i64) -> Result<HomeChange, StoreError> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id, by, action, event, ts_ms, undone_ms FROM home_changes WHERE id = ?1",
            [id],
            |r| {
                Ok(HomeChange {
                    id: r.get(0)?,
                    by: Actor::from_text(&r.get::<_, String>(1)?),
                    action: Action::from_str(&r.get::<_, String>(2)?).unwrap_or(Action::Change),
                    event: r.get(3)?,
                    ts_ms: r.get(4)?,
                    undone_ms: r.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::Invalid(format!("There's no change {id}.")))
    }
}

/// `event`, unless it's deleted.
fn live(event: HomeEvent) -> Result<HomeEvent, StoreError> {
    if event.deleted_ms.is_some() {
        return Err(StoreError::Invalid(format!(
            "Event {} is deleted; bring it back first.",
            event.id
        )));
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soccer() -> EventFields {
        EventFields {
            title: " Soccer ".into(),
            starts: "2026-11-17T17:30".into(),
            ends: Some("2026-11-17T18:30".into()),
            repeat: Some(Repeat {
                every: Every::Week,
                until: None,
            }),
            ..EventFields::default()
        }
    }

    #[test]
    fn an_event_is_added_changed_and_found_by_its_days() {
        let store = Store::open_in_memory().unwrap();
        let maya = Actor::Person(PersonId::random());
        let leo = PersonId::random();
        let (event, _) = store
            .add_event(
                EventFields {
                    people: vec![leo.clone()],
                    ..soccer()
                },
                &maya,
            )
            .unwrap();
        assert_eq!(event.fields.title, "Soccer");
        assert_eq!(event.fields.people, [leo]);
        assert_eq!(event.made_by, Some(maya.clone()));
        let (trip, _) = store
            .add_event(
                EventFields {
                    title: "Zoo trip".into(),
                    starts: "2026-11-20".into(),
                    ..EventFields::default()
                },
                &maya,
            )
            .unwrap();
        assert!(trip.fields.all_day());
        // Soccer repeats weekly from the 17th, so it's in the next week too.
        let week = store.events_between("2026-11-23", "2026-11-29").unwrap();
        assert_eq!(week.iter().map(|e| e.id).collect::<Vec<_>>(), [event.id]);
        let this = store.events_between("2026-11-16", "2026-11-22").unwrap();
        assert_eq!(this.len(), 2);
        let day = store.events_between("2026-11-20", "2026-11-20").unwrap();
        assert!(day.iter().any(|e| e.id == trip.id));

        let bot = Actor::Bot(BotId::new("calendar").unwrap());
        let (moved, _) = store
            .change_event(
                event.id,
                EventFields {
                    starts: "2026-11-17T18:00".into(),
                    ends: None,
                    ..event.fields.clone()
                },
                &bot,
            )
            .unwrap();
        assert_eq!(moved.fields.starts, "2026-11-17T18:00");
        assert_eq!(moved.changed_by, Some(bot));
        assert_eq!(moved.made_by, Some(maya));
    }

    #[test]
    fn bad_times_and_titles_are_refused() {
        let store = Store::open_in_memory().unwrap();
        let by = Actor::Person(PersonId::random());
        for fields in [
            EventFields {
                title: "  ".into(),
                ..soccer()
            },
            EventFields {
                starts: "next tuesday".into(),
                ..soccer()
            },
            EventFields {
                ends: Some("2026-11-17".into()),
                ..soccer()
            },
            EventFields {
                ends: Some("2026-11-17T09:00".into()),
                ..soccer()
            },
            EventFields {
                repeat: Some(Repeat {
                    every: Every::Week,
                    until: Some("June".into()),
                }),
                ..soccer()
            },
        ] {
            assert!(
                matches!(
                    store.add_event(fields.clone(), &by),
                    Err(StoreError::Invalid(_))
                ),
                "{fields:?}"
            );
        }
    }

    #[test]
    fn a_bots_change_can_be_undone_while_its_the_latest() {
        let store = Store::open_in_memory().unwrap();
        let bot = Actor::Bot(BotId::new("calendar").unwrap());
        let maya = Actor::Person(PersonId::random());
        let (event, added) = store.add_event(soccer(), &bot).unwrap();
        let (_, moved) = store
            .change_event(
                event.id,
                EventFields {
                    title: "Soccer practice".into(),
                    ..event.fields.clone()
                },
                &bot,
            )
            .unwrap();
        // The add isn't the latest change any more.
        assert!(store.undo_change(added, &maya).is_err());
        let back = store.undo_change(moved, &maya).unwrap();
        assert_eq!(back.fields.title, "Soccer");
        assert!(store.undo_change(moved, &maya).is_err(), "once only");
        assert!(store.home_change(moved).unwrap().undone_ms.is_some());
        // Now the add is the latest live change: undoing it deletes the event.
        let gone = store.undo_change(added, &maya).unwrap();
        assert!(gone.deleted_ms.is_some());
        assert_eq!(
            store.events_between("2026-11-16", "2026-11-22").unwrap(),
            []
        );
    }

    #[test]
    fn a_deleted_event_waits_in_recently_deleted_then_goes() {
        let store = Store::open_in_memory().unwrap();
        let maya = Actor::Person(PersonId::random());
        let (event, _) = store.add_event(soccer(), &maya).unwrap();
        let deleted = store.delete_event(event.id, &maya).unwrap();
        assert!(store.delete_event(event.id, &maya).is_err());
        assert!(store.change_event(event.id, soccer(), &maya).is_err());
        assert_eq!(store.deleted_events().unwrap().len(), 1);
        // Undoing the delete brings it back; so does Restore.
        store.undo_change(deleted, &maya).unwrap();
        assert_eq!(
            store
                .events_between("2026-11-17", "2026-11-17")
                .unwrap()
                .len(),
            1
        );
        store.delete_event(event.id, &maya).unwrap();
        store.restore_event(event.id, &maya).unwrap();
        assert!(store.restore_event(event.id, &maya).is_err());
        store.delete_event(event.id, &maya).unwrap();
        // Only what was deleted before the cutoff goes.
        assert_eq!(store.purge_deleted_events(0).unwrap(), 0);
        assert_eq!(store.purge_deleted_events(now_ms() + 1).unwrap(), 1);
        assert!(store.home_event(event.id).is_err());
        assert_eq!(store.deleted_events().unwrap(), []);
    }
}
