//! The family Home's calendar, as people see and change it: everyone in
//! the home may (a person, or the home computer). People are named on the
//! page and kept by id; each change is announced so every open calendar
//! asks again.

use std::collections::HashMap;
use std::fmt::Write as _;

use oxbot_protocol::{EventInput, EventRepeat, HomeEventInfo, ServerMsg};
use serde_json::Value;

use super::{Hub, HubError};
use crate::bot::Viewer;
use crate::id::{BotId, PersonId};
use crate::store::{Actor, EventFields, Every, HomeEvent, KEEP_DELETED_DAYS, Repeat, StoreError};
use crate::tools;

impl Hub {
    /// Who `viewer` is in the Home: a person (the home computer acts as
    /// its owner). Someone the home doesn't know yet can't change it.
    pub(super) fn home_actor(&self, viewer: &Viewer) -> Result<Actor, HubError> {
        let viewer = self.resolve(viewer);
        if let Some(id) = &viewer.id {
            return Ok(Actor::Person(id.clone()));
        }
        if viewer.home_computer
            && let Some(owner) = self.store.owner_person()?
        {
            return Ok(Actor::Person(owner.id));
        }
        Err(HubError::NotAllowed(
            "Pick your name first: the calendar is for the people in this home.".to_owned(),
        ))
    }

    /// The events that may happen from day `from` to day `to`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, or the database fails.
    pub fn home_events(
        &self,
        viewer: &Viewer,
        from: &str,
        to: &str,
    ) -> Result<Vec<HomeEventInfo>, HubError> {
        self.home_actor(viewer)?;
        let names = self.person_names();
        Ok(self
            .store
            .events_between(from, to)?
            .into_iter()
            .map(|e| self.event_info(e, &names))
            .collect())
    }

    /// Recently deleted events (and anything deleted longer ago is gone
    /// for good).
    ///
    /// # Errors
    /// `viewer` isn't in the home, or the database fails.
    pub fn deleted_home_events(&self, viewer: &Viewer) -> Result<Vec<HomeEventInfo>, HubError> {
        self.home_actor(viewer)?;
        let cutoff = crate::store::now_ms() - KEEP_DELETED_DAYS * 86_400_000;
        if let Err(err) = self.store.purge_deleted_events(cutoff) {
            tracing::warn!(%err, "couldn't clear out old deleted events");
        }
        let names = self.person_names();
        Ok(self
            .store
            .deleted_events()?
            .into_iter()
            .map(|e| self.event_info(e, &names))
            .collect())
    }

    /// Adds an event as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, the event isn't valid, or the database
    /// fails.
    pub fn add_home_event(
        &self,
        viewer: &Viewer,
        input: EventInput,
    ) -> Result<ServerMsg, HubError> {
        let by = self.home_actor(viewer)?;
        let (event, change) = self.store.add_event(self.event_fields(input)?, &by)?;
        Ok(self.home_saved(change, &event, "add"))
    }

    /// Changes event `id` as `viewer`.
    ///
    /// # Errors
    /// As [`Hub::add_home_event`], or the event is gone.
    pub fn change_home_event(
        &self,
        viewer: &Viewer,
        id: i64,
        input: EventInput,
    ) -> Result<ServerMsg, HubError> {
        let by = self.home_actor(viewer)?;
        let fields = EventFields {
            // What it came from stays.
            source: self.store.home_event(id)?.fields.source,
            ..self.event_fields(input)?
        };
        let (event, change) = self.store.change_event(id, fields, &by)?;
        Ok(self.home_saved(change, &event, "change"))
    }

    /// Deletes event `id` into Recently deleted, as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, the event is gone, or the database fails.
    pub fn delete_home_event(&self, viewer: &Viewer, id: i64) -> Result<ServerMsg, HubError> {
        let by = self.home_actor(viewer)?;
        let change = self.store.delete_event(id, &by)?;
        let event = self.store.home_event(id)?;
        Ok(self.home_saved(change, &event, "delete"))
    }

    /// Brings event `id` back from Recently deleted, as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, the event isn't deleted, or the database
    /// fails.
    pub fn restore_home_event(&self, viewer: &Viewer, id: i64) -> Result<ServerMsg, HubError> {
        let by = self.home_actor(viewer)?;
        let change = self.store.restore_event(id, &by)?;
        let event = self.store.home_event(id)?;
        Ok(self.home_saved(change, &event, "restore"))
    }

    /// Undoes `change`, as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, it can't be undone, or the database fails.
    pub fn undo_home_change(&self, viewer: &Viewer, change: i64) -> Result<ServerMsg, HubError> {
        let by = self.home_actor(viewer)?;
        let event = self.store.undo_change(change, &by)?;
        Ok(self.home_saved(change, &event, "undo"))
    }

    /// Tells everyone the Home changed; the answer to whoever changed it.
    fn home_saved(&self, change: i64, event: &HomeEvent, action: &str) -> ServerMsg {
        let _ = self.updates.send(ServerMsg::HomeChanged);
        ServerMsg::HomeSaved {
            change,
            id: event.id,
            action: action.to_owned(),
            title: event.fields.title.clone(),
            bot: None,
        }
    }

    /// Runs one of the Calendar bot's tools (`tools::CALENDAR_TOOLS`) as
    /// `bot`. A change is shown to everyone who sees the bot, with Undo.
    pub(super) fn calendar_tool(&self, bot: &BotId, tool: &str, args: &Value) -> (bool, String) {
        let by = Actor::Bot(bot.clone());
        let id = || {
            args.get("id")
                .and_then(Value::as_i64)
                .ok_or("needs the event's id")
        };
        let saved = |r: Result<(i64, HomeEvent, &str), HubError>| match r {
            Ok((change, event, action)) => {
                let _ = self.updates.send(ServerMsg::HomeChanged);
                let _ = self.updates.send(ServerMsg::HomeSaved {
                    change,
                    id: event.id,
                    action: action.to_owned(),
                    title: event.fields.title.clone(),
                    bot: Some(bot.to_string()),
                });
                let done = match action {
                    "add" => "Added",
                    "change" => "Changed",
                    _ => "Deleted (it's in Recently deleted for 30 days)",
                };
                (
                    true,
                    format!(
                        "{done}: \"{}\" (id {}). Everyone's calendar shows it now, with Undo.",
                        event.fields.title, event.id
                    ),
                )
            }
            Err(err) => (false, format!("{err} Nothing was changed.")),
        };
        match tool {
            tools::CALENDAR_EVENTS_TOOL => {
                let (from, to) = (tools::text_arg(args, "from"), tools::text_arg(args, "to"));
                match self.store.events_between(from.trim(), to.trim()) {
                    Ok(events) => (true, self.events_text(events)),
                    Err(err) => (false, format!("Couldn't read the calendar: {err}")),
                }
            }
            tools::ADD_EVENT_TOOL => {
                let mut added = Vec::new();
                let (ok, text) = saved(
                    self.tool_event(args, &mut added)
                        .and_then(|f| Ok(self.store.add_event(f, &by)?))
                        .map(|(e, c)| (c, e, "add")),
                );
                (ok, with_added(text, &added))
            }
            tools::CHANGE_EVENT_TOOL => {
                let Ok(id) = id() else {
                    return (false, "change_event needs the event's id".to_owned());
                };
                let mut added = Vec::new();
                let (ok, text) = saved((|| {
                    let source = self.store.home_event(id)?.fields.source;
                    let fields = EventFields {
                        source,
                        ..self.tool_event(args, &mut added)?
                    };
                    let (e, c) = self.store.change_event(id, fields, &by)?;
                    Ok((c, e, "change"))
                })());
                (ok, with_added(text, &added))
            }
            tools::DELETE_EVENT_TOOL => {
                let Ok(id) = id() else {
                    return (false, "delete_event needs the event's id".to_owned());
                };
                saved((|| {
                    let c = self.store.delete_event(id, &by)?;
                    Ok((c, self.store.home_event(id)?, "delete"))
                })())
            }
            _ => (false, "unknown tool".to_owned()),
        }
    }

    /// A tool call's event, as the page would send it.
    fn tool_event(&self, args: &Value, added: &mut Vec<String>) -> Result<EventFields, HubError> {
        let text = |k: &str| tools::text_arg(args, k).trim().to_owned();
        let opt = |k: &str| Some(text(k)).filter(|t| !t.is_empty());
        let input = EventInput {
            title: text("title"),
            starts: text("starts"),
            ends: opt("ends"),
            place: text("place"),
            people: args
                .get("people")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            notes: text("notes"),
            repeat: opt("repeat").map(|every| EventRepeat {
                every,
                until: opt("until"),
            }),
            remind_min: args
                .get("remind_min")
                .and_then(Value::as_i64)
                .and_then(|m| u32::try_from(m).ok()),
        };
        self.event_fields_adding(input, Some(added))
    }

    /// Events as the Calendar bot reads them, one per line, after who's in
    /// the home.
    fn events_text(&self, events: Vec<HomeEvent>) -> String {
        let names = self.person_names();
        let mut people: Vec<_> = names.values().cloned().collect();
        people.sort();
        let mut out = format!("People in this home: {}.\n", people.join(", "));
        if events.is_empty() {
            out.push_str("No events then.");
        }
        for e in events {
            let i = self.event_info(e, &names);
            let ev = &i.event;
            let mut line = format!("- id {}: {} · {}", i.id, ev.title, ev.starts);
            if let Some(end) = &ev.ends {
                let _ = write!(line, " to {end}");
            }
            if !ev.people.is_empty() {
                let _ = write!(line, " · for {}", ev.people.join(", "));
            }
            if !ev.place.is_empty() {
                let _ = write!(line, " · at {}", ev.place);
            }
            if let Some(r) = &ev.repeat {
                let _ = write!(line, " · every {}", r.every);
                if let Some(u) = &r.until {
                    let _ = write!(line, " until {u}");
                }
            }
            if !ev.notes.is_empty() {
                let _ = write!(line, " · notes: {}", ev.notes);
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// `input` as the store keeps it: people by id (a name the home doesn't
    /// know is refused).
    fn event_fields(&self, input: EventInput) -> Result<EventFields, HubError> {
        self.event_fields_adding(input, None)
    }

    /// Like [`Hub::event_fields`], adding a name the home doesn't know yet
    /// to its people when `added` is given (the Calendar bot, told "only
    /// Akhila is going"); their names go in `added`.
    fn event_fields_adding(
        &self,
        input: EventInput,
        mut added: Option<&mut Vec<String>>,
    ) -> Result<EventFields, HubError> {
        let names = self.person_names();
        let mut people = Vec::new();
        for name in &input.people {
            let known = names
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(name.trim()))
                .map(|(id, _)| id.clone());
            let id = match known {
                Some(id) => id,
                None if added.is_some() && !name.trim().is_empty() => {
                    let person = self.person_named(name)?;
                    if let Some(added) = added.as_deref_mut() {
                        added.push(person.name);
                    }
                    person.id
                }
                None => {
                    return Err(HubError::Store(StoreError::Invalid(format!(
                        "There's nobody called {name} in this home."
                    ))));
                }
            };
            if !people.contains(&id) {
                people.push(id);
            }
        }
        let repeat = match input.repeat {
            None => None,
            Some(EventRepeat { every, until }) => Some(Repeat {
                every: match every.as_str() {
                    "day" => Every::Day,
                    "week" => Every::Week,
                    "month" => Every::Month,
                    "year" => Every::Year,
                    _ => {
                        return Err(HubError::Store(StoreError::Invalid(
                            "An event repeats every day, week, month or year.".to_owned(),
                        )));
                    }
                },
                until: until.filter(|u| !u.trim().is_empty()),
            }),
        };
        Ok(EventFields {
            title: input.title,
            starts: input.starts.trim().to_owned(),
            ends: input
                .ends
                .map(|e| e.trim().to_owned())
                .filter(|e| !e.is_empty()),
            place: input.place.trim().to_owned(),
            people,
            notes: input.notes.trim().to_owned(),
            repeat,
            remind_min: input.remind_min,
            source: String::new(),
        })
    }

    fn event_info(&self, e: HomeEvent, names: &HashMap<PersonId, String>) -> HomeEventInfo {
        let who = |a: Option<&Actor>| match a {
            Some(Actor::Person(p)) => names.get(p).cloned().unwrap_or_default(),
            Some(Actor::Bot(b)) => self.bot_name(b),
            None => String::new(),
        };
        HomeEventInfo {
            id: e.id,
            all_day: e.fields.all_day(),
            made_by: who(e.made_by.as_ref()),
            changed_by: who(e.changed_by.as_ref()),
            updated_ms: e.updated_ms,
            deleted_ms: e.deleted_ms,
            source: e.fields.source.clone(),
            event: EventInput {
                title: e.fields.title,
                starts: e.fields.starts,
                ends: e.fields.ends,
                place: e.fields.place,
                people: e
                    .fields
                    .people
                    .iter()
                    .filter_map(|p| names.get(p).cloned())
                    .collect(),
                notes: e.fields.notes,
                repeat: e.fields.repeat.map(|r| EventRepeat {
                    every: match r.every {
                        Every::Day => "day",
                        Every::Week => "week",
                        Every::Month => "month",
                        Every::Year => "year",
                    }
                    .to_owned(),
                    until: r.until,
                }),
                remind_min: e.fields.remind_min,
            },
        }
    }
}

/// A tool's answer, saying who joined the home's people for it.
fn with_added(text: String, added: &[String]) -> String {
    if added.is_empty() {
        return text;
    }
    format!(
        "{text} New in this home's people (so the family can pick them on events): {}.",
        added.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bot, BotId, HubConfig, Store};
    use std::sync::Arc;

    fn plain_hub(dir: &std::path::Path) -> Arc<Hub> {
        let bots =
            vec![Bot::create(dir, BotId::new("assistant").unwrap(), "Assistant", "").unwrap()];
        let hub = Hub::new(bots, Store::open_in_memory().unwrap(), HubConfig::new(dir));
        hub.owner_person("Jay").unwrap();
        hub
    }

    fn input(title: &str, starts: &str, people: &[&str]) -> EventInput {
        EventInput {
            title: title.into(),
            starts: starts.into(),
            people: people.iter().map(|p| (*p).to_owned()).collect(),
            ..EventInput::default()
        }
    }

    #[tokio::test]
    async fn people_in_the_home_share_one_calendar() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = plain_hub(tmp.path());
        let jay = hub.home_computer_viewer("Jay");
        let ann = hub.person_named("Ann").unwrap();
        let ann = Viewer::person(ann.id, "Ann", false);
        let mut updates = hub.subscribe();

        let ServerMsg::HomeSaved { id, change, .. } = hub
            .add_home_event(&ann, input("Soccer", "2026-11-17T17:30", &["ann", "Jay"]))
            .unwrap()
        else {
            panic!("not saved")
        };
        assert!(matches!(
            updates.recv().await.unwrap(),
            ServerMsg::HomeChanged
        ));
        // Jay sees Ann's event, with names as the home knows them.
        let seen = hub.home_events(&jay, "2026-11-16", "2026-11-22").unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].event.people, ["Ann", "Jay"]);
        assert_eq!(seen[0].made_by, "Ann");
        // Jay moves it; Ann undoes that.
        let ServerMsg::HomeSaved { change: moved, .. } = hub
            .change_home_event(&jay, id, input("Soccer", "2026-11-17T18:00", &[]))
            .unwrap()
        else {
            panic!("not saved")
        };
        assert!(
            hub.undo_home_change(&ann, change).is_err(),
            "not the latest"
        );
        hub.undo_home_change(&ann, moved).unwrap();
        let back = hub.home_events(&jay, "2026-11-17", "2026-11-17").unwrap();
        assert_eq!(back[0].event.starts, "2026-11-17T17:30");
        assert_eq!(back[0].changed_by, "Ann");
        // Deleted, it waits in Recently deleted.
        hub.delete_home_event(&jay, id).unwrap();
        assert_eq!(
            hub.home_events(&jay, "2026-11-17", "2026-11-17").unwrap(),
            []
        );
        assert_eq!(hub.deleted_home_events(&ann).unwrap().len(), 1);
        hub.restore_home_event(&ann, id).unwrap();
        assert_eq!(
            hub.home_events(&jay, "2026-11-17", "2026-11-17")
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn the_calendar_refuses_strangers_and_unknown_names() {
        let tmp = tempfile::tempdir().unwrap();
        let hub = plain_hub(tmp.path());
        let stranger = Viewer {
            id: None,
            name: String::new(),
            home_computer: false,
            admin: false,
        };
        assert!(
            hub.home_events(&stranger, "2026-11-16", "2026-11-22")
                .is_err()
        );
        assert!(
            hub.add_home_event(&stranger, input("Party", "2026-11-21", &[]))
                .is_err()
        );
        let jay = hub.home_computer_viewer("Jay");
        let err = hub
            .add_home_event(&jay, input("Party", "2026-11-21", &["Zed"]))
            .unwrap_err();
        assert!(err.to_string().contains("nobody called Zed"), "{err}");
        let err = hub
            .add_home_event(
                &jay,
                EventInput {
                    repeat: Some(EventRepeat {
                        every: "fortnight".into(),
                        until: None,
                    }),
                    ..input("Party", "2026-11-21", &[])
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("every day, week"), "{err}");
    }

    #[tokio::test]
    async fn the_calendar_bot_adds_events_from_words_with_undo() {
        use serde_json::json;
        let tmp = tempfile::tempdir().unwrap();
        let hub = plain_hub(tmp.path());
        hub.person_named("Leo").unwrap();
        let cal = BotId::new("home-calendar").unwrap();
        let mut updates = hub.subscribe();

        let (ok, text) = hub.calendar_tool(
            &cal,
            tools::ADD_EVENT_TOOL,
            &json!({"title": "Soccer", "starts": "2026-11-24T17:30", "people": ["leo"], "repeat": "week"}),
        );
        assert!(ok, "{text}");
        assert!(text.starts_with("Added: \"Soccer\""), "{text}");
        // Everyone's calendar asks again, and shows the bot's change with Undo.
        assert!(matches!(
            updates.recv().await.unwrap(),
            ServerMsg::HomeChanged
        ));
        let ServerMsg::HomeSaved {
            change, id, bot, ..
        } = updates.recv().await.unwrap()
        else {
            panic!("saved")
        };
        assert_eq!(bot.as_deref(), Some("home-calendar"));

        let (_, list) = hub.calendar_tool(
            &cal,
            tools::CALENDAR_EVENTS_TOOL,
            &json!({"from": "2026-12-01", "to": "2026-12-07"}),
        );
        assert!(list.contains("People in this home: Jay, Leo."), "{list}");
        assert!(
            list.contains(&format!(
                "id {id}: Soccer · 2026-11-24T17:30 · for Leo · every week"
            )),
            "{list}"
        );

        // Someone the home doesn't know yet joins its people.
        let (ok, text) = hub.calendar_tool(
            &cal,
            tools::ADD_EVENT_TOOL,
            &json!({"title": "King concert", "starts": "2026-11-21T19:00", "people": ["Akhila"]}),
        );
        assert!(ok, "{text}");
        assert!(
            text.contains("New in this home's people") && text.contains("Akhila"),
            "{text}"
        );
        assert!(hub.person_names().values().any(|n| n == "Akhila"));

        let (ok, _) = hub.calendar_tool(
            &cal,
            tools::CHANGE_EVENT_TOOL,
            &json!({"id": id, "title": "Soccer", "starts": "2026-11-24T18:00", "repeat": "week"}),
        );
        assert!(ok);
        let (ok, text) = hub.calendar_tool(&cal, tools::DELETE_EVENT_TOOL, &json!({"id": id}));
        assert!(ok && text.contains("Recently deleted"), "{text}");
        let jay = hub.home_computer_viewer("Jay");
        let gone = hub.deleted_home_events(&jay).unwrap();
        assert_eq!(gone[0].changed_by, "home-calendar");
        // Adding it can't be undone once it was changed after.
        assert!(hub.undo_home_change(&jay, change).is_err());
    }
}
