//! Records: files the family keeps, filed so they're easy to get
//! back. A file attached in the Records chat is copied into the
//! `family-notes` folder beside `bots/`, at
//! `<year>/<topic>/<day> <title>.<ext>`, and recorded with what the
//! person said about it as they attached it.
//!
//! Every note has a day: the one the person gave; or, if the AI may read
//! the file, the one the Records bot reads from it (`file_note`);
//! or else the day it was uploaded. A private file is filed with only
//! what the person gave: the AI never sees it.
//!
//! Anyone in the home can find a note again: on the Notes screen, or by
//! asking the bot, which searches what's recorded (`find_notes`), never
//! the files, and shows what it found as cards in the chat (`show_note`).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use oxbot_protocol::{
    Attachment, AttachmentKind, EventKind, HomeNoteInfo, NoteInput, NoteUpload, ServerMsg,
};
use serde_json::Value;

use super::{Hub, HubError};
use crate::bot::{Bot, HOME_LEADS, Viewer};
use crate::id::{BotId, PersonId};
use crate::store::{
    Actor, DayFrom, HomeNote, KEEP_DELETED_DAYS, NewNote, NoteFields, StoreError, is_day,
};
use crate::tools;

/// The Records bot's id.
pub const NOTES_BOT: &str = HOME_LEADS[2].0;

/// Where deleted notes' files wait, in the Records folder.
const DELETED: &str = ".deleted";

/// A note's folder when it has no topic.
const UNSORTED: &str = "Unsorted";

/// The most notes `find_notes` lists.
const MAX_FOUND: usize = 30;

impl Hub {
    /// The Records folder: beside `bots/` in oxbot's data folder.
    #[must_use]
    pub fn notes_root(&self) -> PathBuf {
        self.config.bots_dir.with_file_name("family-notes")
    }

    /// Files claimed uploads for the Records bot (already placed:
    /// private, or in its work folder for the AI), with what the person
    /// said about each (`notes`, by upload id): copies each into the Family
    /// Notes folder and records it. Marks each file `filed`. One filed
    /// already (a send tried again) is left as it is.
    ///
    /// # Errors
    /// A file can't be copied, what was said isn't valid (a day that
    /// isn't one), or the database fails.
    pub(super) fn file_notes(
        &self,
        bot: &Bot,
        files: &mut [Attachment],
        notes: &[NoteUpload],
        viewer: &Viewer,
    ) -> Result<Vec<HomeNote>, HubError> {
        let by = self.home_actor(viewer)?;
        let root = self.notes_root();
        let mut kept = Vec::new();
        for f in files.iter_mut() {
            if f.filed.is_some() {
                continue;
            }
            let said = notes
                .iter()
                .find(|n| n.id == f.id)
                .map(|n| n.note.clone())
                .unwrap_or_default();
            let fields = self.note_fields(&said, &f.name, None)?;
            let Some(from) = super::uploads::upload_file(bot, f) else {
                return Err(not_kept("the file isn't there any more"));
            };
            let file = self.free_note_path(&fields, &f.name, 0)?;
            copy_in(&root, &from, &file)?;
            let note = self.store.add_note(
                NewNote {
                    fields,
                    file: file.clone(),
                    name: f.name.clone(),
                    kind: kind_text(f.kind),
                    size: f.size,
                    private: f.private,
                },
                &by,
            );
            let note = match note {
                Ok(n) => n,
                Err(err) => {
                    let _ = std::fs::remove_file(root.join(&file));
                    return Err(err.into());
                }
            };
            f.filed = Some(note.id);
            self.mark_filed(&f.id, note.id);
            kept.push(note);
        }
        if !kept.is_empty() {
            let _ = self.updates.send(ServerMsg::HomeChanged);
        }
        Ok(kept)
    }

    /// Cards in `bot`'s chat for `notes`.
    pub(super) fn note_cards(&self, bot: &BotId, notes: &[HomeNote]) {
        for n in notes {
            if let Err(err) = self.record(bot, note_card(n)) {
                tracing::warn!(%err, "couldn't show a note in the chat");
            }
        }
    }

    /// Records, latest day first.
    ///
    /// # Errors
    /// `viewer` isn't in the home, or the database fails.
    pub fn home_notes(&self, viewer: &Viewer) -> Result<Vec<HomeNoteInfo>, HubError> {
        self.home_actor(viewer)?;
        self.purge_deleted_notes();
        let names = self.person_names();
        Ok(self
            .store
            .notes()?
            .into_iter()
            .map(|n| note_info(n, &names))
            .collect())
    }

    /// Changes note `id` as `viewer`: what they wrote (a day they give is
    /// theirs; none keeps the note's). Its file moves to match.
    ///
    /// # Errors
    /// `viewer` isn't in the home, the note is gone, what they wrote isn't
    /// valid, or the file can't be moved.
    pub fn change_home_note(
        &self,
        viewer: &Viewer,
        id: i64,
        input: &NoteInput,
    ) -> Result<(), HubError> {
        let by = self.home_actor(viewer)?;
        let note = self.store.note(id)?;
        let mut fields = self.note_fields(input, &note.name, None)?;
        fields.summary.clone_from(&note.fields.summary);
        if input.day.trim().is_empty() {
            fields.day.clone_from(&note.fields.day);
            fields.day_from = note.fields.day_from;
        }
        self.save_note(&note, fields, &by)?;
        Ok(())
    }

    /// Deletes note `id` as `viewer`: its file goes to the Records
    /// folder's `.deleted` folder for [`KEEP_DELETED_DAYS`], when it's gone
    /// for good (see [`Hub::restore_home_note`]).
    ///
    /// # Errors
    /// `viewer` isn't in the home, the note is gone, or the file can't be
    /// moved.
    pub fn delete_home_note(&self, viewer: &Viewer, id: i64) -> Result<(), HubError> {
        let by = self.home_actor(viewer)?;
        let note = self.store.note(id)?;
        if note.deleted_ms.is_some() {
            return Err(StoreError::Invalid(format!("Note {id} is deleted already.")).into());
        }
        let root = self.notes_root();
        let name = Path::new(&note.file)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("note");
        let away = format!("{DELETED}/{id} {name}");
        move_within(&root, &note.file, &away)?;
        if let Err(err) = self.store.set_note_deleted(id, true, &away, &by) {
            let _ = move_within(&root, &away, &note.file);
            return Err(err.into());
        }
        let _ = self.updates.send(ServerMsg::HomeChanged);
        Ok(())
    }

    /// Brings deleted note `id` back, to where its title, day and topic
    /// put it.
    ///
    /// # Errors
    /// `viewer` isn't in the home, the note isn't deleted (or is gone for
    /// good), or the file can't be moved.
    pub fn restore_home_note(&self, viewer: &Viewer, id: i64) -> Result<(), HubError> {
        let by = self.home_actor(viewer)?;
        let note = self.store.note(id)?;
        if note.deleted_ms.is_none() {
            return Err(StoreError::Invalid(format!("Note {id} isn't deleted.")).into());
        }
        let back = self.free_note_path(&note.fields, &note.name, id)?;
        move_within(&self.notes_root(), &note.file, &back)?;
        self.store.set_note_deleted(id, false, &back, &by)?;
        let _ = self.updates.send(ServerMsg::HomeChanged);
        Ok(())
    }

    /// Removes for good what was deleted over [`KEEP_DELETED_DAYS`] ago.
    fn purge_deleted_notes(&self) {
        let cutoff = crate::store::now_ms() - KEEP_DELETED_DAYS * 86_400_000;
        match self.store.purge_deleted_notes(cutoff) {
            Ok(gone) => {
                let root = self.notes_root();
                for n in gone {
                    let path = root.join(&n.file);
                    if inside(&root, &path) {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "couldn't clear out old deleted notes"),
        }
    }

    /// Note `id`'s file, for `viewer` to open on the home computer.
    ///
    /// # Errors
    /// `viewer` isn't in the home, or the note or its file is gone.
    pub fn note_file(&self, viewer: &Viewer, id: i64) -> Result<(HomeNote, PathBuf), HubError> {
        self.home_actor(viewer)?;
        let note = self.store.note(id)?;
        let path = self.notes_root().join(&note.file);
        if note.deleted_ms.is_some() || !inside(&self.notes_root(), &path) || !path.is_file() {
            return Err(HubError::NotAllowed(
                "That note's file isn't there any more.".to_owned(),
            ));
        }
        Ok((note, path))
    }

    /// Writes `fields` over `note`, moving its file to where they put it.
    fn save_note(
        &self,
        note: &HomeNote,
        fields: NoteFields,
        by: &Actor,
    ) -> Result<HomeNote, HubError> {
        let fields = fields.checked()?;
        let root = self.notes_root();
        let mut file = note.file.clone();
        if note_path(&fields, &note.name, 0) != note.file {
            let to = self.free_note_path(&fields, &note.name, note.id)?;
            let (old, new) = (root.join(&note.file), root.join(&to));
            if old.is_file() && inside(&root, &old) {
                if let Some(parent) = new.parent() {
                    crate::home::private_dir(parent).map_err(|e| not_kept(&e.to_string()))?;
                }
                std::fs::rename(&old, &new).map_err(|e| not_kept(&e.to_string()))?;
                remove_empty_up(&root, old.parent());
            }
            file = to;
        }
        let saved = self.store.change_note(note.id, fields, &file, by)?;
        let _ = self.updates.send(ServerMsg::HomeChanged);
        Ok(saved)
    }

    /// The first free path for a note with `fields` (" (2)" and so on
    /// after a taken one), not counting note `id`'s own.
    fn free_note_path(&self, fields: &NoteFields, name: &str, id: i64) -> Result<String, HubError> {
        let root = self.notes_root();
        for n in 0..1000 {
            let path = note_path(fields, name, n);
            if !root.join(&path).exists() && !self.store.note_file_taken(&path, id)? {
                return Ok(path);
            }
        }
        Err(not_kept("too many notes with that name"))
    }

    /// What a person wrote as a note's fields: the file's name if no
    /// title; their day, or else today (an upload's day); people by name
    /// (a name the home doesn't know is added when `added` is given, as
    /// for the bot).
    fn note_fields(
        &self,
        input: &NoteInput,
        file_name: &str,
        mut added: Option<&mut Vec<String>>,
    ) -> Result<NoteFields, HubError> {
        let title = match input.title.trim() {
            "" => title_of(file_name),
            t => t.to_owned(),
        };
        let (day, day_from) = match input.day.trim() {
            "" => (today(), DayFrom::Upload),
            d if is_day(d) => (d.to_owned(), DayFrom::You),
            _ => {
                return Err(
                    StoreError::Invalid("A note's day is written 2026-11-17.".to_owned()).into(),
                );
            }
        };
        let names = self.person_names();
        let mut people: Vec<PersonId> = Vec::new();
        for name in input
            .people
            .iter()
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
        {
            let known = names
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(name))
                .map(|(id, _)| id.clone());
            let id = match (known, added.as_deref_mut()) {
                (Some(id), _) => id,
                (None, Some(added)) => {
                    let person = self.person_named(name)?;
                    added.push(person.name);
                    person.id
                }
                (None, None) => {
                    return Err(StoreError::Invalid(format!(
                        "There's nobody called {name} in this home."
                    ))
                    .into());
                }
            };
            if !people.contains(&id) {
                people.push(id);
            }
        }
        Ok(NoteFields {
            title,
            day,
            day_from,
            topic: input.topic.trim().to_owned(),
            tags: input.tags.clone(),
            people,
            summary: String::new(),
        })
    }

    /// Runs one of the Records bot's tools (`tools::NOTES_TOOLS`) as
    /// `bot`.
    pub(super) fn notes_tool(&self, bot: &BotId, tool: &str, args: &Value) -> (bool, String) {
        let by = Actor::Bot(bot.clone());
        let id = args.get("id").and_then(Value::as_i64);
        let names = self.person_names();
        match tool {
            tools::FIND_NOTES_TOOL => match self.store.notes() {
                Ok(notes) => (true, found_text(notes, args, &names)),
                Err(err) => (false, format!("Couldn't read Records: {err}")),
            },
            tools::SHOW_NOTE_TOOL => {
                let Some(id) = id else {
                    return (false, "show_note needs the note's id".to_owned());
                };
                match self.store.note(id) {
                    Ok(n) if n.deleted_ms.is_none() => {
                        self.note_cards(bot, std::slice::from_ref(&n));
                        (
                            true,
                            format!(
                                "Shown in the chat: \"{}\". They can open it from there.",
                                n.fields.title
                            ),
                        )
                    }
                    _ => (false, format!("There's no note {id}.")),
                }
            }
            tools::FILE_NOTE_TOOL => {
                let Some(id) = id else {
                    return (false, "file_note needs the note's id".to_owned());
                };
                let mut added = Vec::new();
                match self
                    .tool_note(id, args, &mut added)
                    .and_then(|(note, f)| self.save_note(&note, f, &by))
                {
                    Ok(n) => {
                        let mut text = format!(
                            "Filed: note {} \"{}\", {} ({}), at Records/{}.",
                            n.id,
                            n.fields.title,
                            n.fields.day,
                            day_from_words(n.fields.day_from),
                            n.file
                        );
                        if !added.is_empty() {
                            let _ =
                                write!(text, " New in this home's people: {}.", added.join(", "));
                        }
                        (true, text)
                    }
                    Err(err) => (false, format!("{err} Nothing was changed.")),
                }
            }
            _ => (false, "unknown tool".to_owned()),
        }
    }

    /// `file_note`'s arguments over note `id`: only what it gives changes.
    /// A day read from the document never replaces one a person gave.
    fn tool_note(
        &self,
        id: i64,
        args: &Value,
        added: &mut Vec<String>,
    ) -> Result<(HomeNote, NoteFields), HubError> {
        let note = self.store.note(id)?;
        if note.deleted_ms.is_some() {
            return Err(StoreError::Invalid(format!("There's no note {id}.")).into());
        }
        let text = |k: &str| tools::text_arg(args, k).trim().to_owned();
        let list = |k: &str| {
            args.get(k).and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
        };
        let mut f = note.fields.clone();
        if !text("title").is_empty() {
            f.title = text("title");
        }
        if !text("topic").is_empty() {
            f.topic = text("topic");
        }
        if !text("summary").is_empty() {
            f.summary = text("summary");
        }
        if let Some(tags) = list("tags") {
            f.tags = tags;
        }
        if let Some(people) = list("people") {
            let input = NoteInput {
                people,
                ..NoteInput::default()
            };
            f.people = self.note_fields(&input, &note.name, Some(added))?.people;
        }
        let day = text("day");
        if !day.is_empty() {
            if !is_day(&day) {
                return Err(
                    StoreError::Invalid("A note's day is written 2026-11-17.".to_owned()).into(),
                );
            }
            let from = if text("day_from") == "you" {
                DayFrom::You
            } else {
                DayFrom::Document
            };
            // A person's own day stays unless they changed it.
            if f.day_from != DayFrom::You || from == DayFrom::You {
                f.day = day;
                f.day_from = from;
            }
        }
        Ok((note, f))
    }
}

/// `find_notes`' answer: the notes that match, best first, one a line.
fn found_text(notes: Vec<HomeNote>, args: &Value, names: &HashMap<PersonId, String>) -> String {
    let text = |k: &str| tools::text_arg(args, k).trim().to_lowercase();
    let (from, to, person, topic) = (text("from"), text("to"), text("person"), text("topic"));
    let words = query_words(&text("query"));
    let mut found: Vec<(usize, HomeNote)> = notes
        .into_iter()
        .filter(|n| from.is_empty() || n.fields.day.as_str() >= from.as_str())
        .filter(|n| to.is_empty() || n.fields.day.as_str() <= to.as_str())
        .filter(|n| topic.is_empty() || n.fields.topic.to_lowercase().contains(&topic))
        .filter(|n| {
            person.is_empty()
                || n.fields.people.iter().any(|p| {
                    names
                        .get(p)
                        .is_some_and(|name| name.to_lowercase() == person)
                })
        })
        .filter_map(|n| {
            let score = score(&n, &words, names);
            (words.is_empty() || score > 0).then_some((score, n))
        })
        .collect();
    // Best match first, then the latest.
    found.sort_by(|(a, x), (b, y)| b.cmp(a).then(y.fields.day.cmp(&x.fields.day)));
    let total = found.len();
    let mut people: Vec<_> = names.values().cloned().collect();
    people.sort();
    let mut out = format!("People in this home: {}.\n", people.join(", "));
    if total == 0 {
        out.push_str("No notes match.");
        return out;
    }
    let _ = writeln!(
        out,
        "{total} note{} found:",
        if total == 1 { "" } else { "s" }
    );
    for (_, n) in found.into_iter().take(MAX_FOUND) {
        let f = &n.fields;
        let _ = write!(
            out,
            "- note {}: {} · {} ({}) · {}",
            n.id,
            f.title,
            f.day,
            day_from_words(f.day_from),
            if f.topic.is_empty() {
                UNSORTED
            } else {
                &f.topic
            }
        );
        if !f.tags.is_empty() {
            let _ = write!(out, " · tags: {}", f.tags.join(", "));
        }
        let about: Vec<&str> = f
            .people
            .iter()
            .filter_map(|p| names.get(p).map(String::as_str))
            .collect();
        if !about.is_empty() {
            let _ = write!(out, " · about {}", about.join(", "));
        }
        let _ = write!(
            out,
            " · {}",
            if n.private {
                "private (you can't open it)"
            } else {
                "the AI read it"
            }
        );
        if !f.summary.is_empty() {
            let _ = write!(out, " · {}", f.summary);
        }
        out.push('\n');
    }
    if total > MAX_FOUND {
        let _ = writeln!(out, "(and {} more: narrow it down)", total - MAX_FOUND);
    }
    out
}

/// How well `n` matches `words`: how many of them it has somewhere.
fn score(n: &HomeNote, words: &[String], names: &HashMap<PersonId, String>) -> usize {
    let f = &n.fields;
    let mut hay = format!(
        "{} {} {} {} {} {} {}",
        f.title,
        f.topic,
        f.tags.join(" "),
        f.summary,
        n.name,
        f.day,
        month_name(&f.day)
    );
    for p in &f.people {
        if let Some(name) = names.get(p) {
            hay.push(' ');
            hay.push_str(name);
        }
    }
    let hay = hay.to_lowercase();
    words.iter().filter(|w| hay.contains(w.as_str())).count()
}

/// A question's words worth looking for: no "the", "my" and the like,
/// and "bills" looks for "bill" too.
fn query_words(query: &str) -> Vec<String> {
    const SKIP: &[&str] = &[
        "a",
        "an",
        "and",
        "the",
        "my",
        "our",
        "of",
        "for",
        "to",
        "in",
        "on",
        "at",
        "from",
        "with",
        "is",
        "it",
        "me",
        "find",
        "show",
        "get",
        "last",
        "latest",
        "recent",
        "file",
        "files",
        "note",
        "notes",
        "document",
        "documents",
        "doc",
        "docs",
        "please",
        "where",
        "what",
        "s",
    ];
    let mut out: Vec<String> = Vec::new();
    for w in query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty() && !SKIP.contains(w))
    {
        let w = w.strip_suffix("'s").unwrap_or(w);
        let w = if w.len() > 3 {
            w.strip_suffix('s').unwrap_or(w)
        } else {
            w
        };
        if !out.iter().any(|x| x == w) {
            out.push(w.to_owned());
        }
    }
    out
}

fn month_name(day: &str) -> &'static str {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    day.get(5..7)
        .and_then(|m| m.parse::<usize>().ok())
        .and_then(|m| MONTHS.get(m.wrapping_sub(1)))
        .copied()
        .unwrap_or("")
}

fn day_from_words(from: DayFrom) -> &'static str {
    match from {
        DayFrom::You => "day given",
        DayFrom::Document => "from the document",
        DayFrom::Upload => "upload day",
    }
}

/// Where a note with `fields` goes: `<year>/<topic>/<day> <title>.<ext>`,
/// with " (n+1)" after the title for `n` > 0.
fn note_path(fields: &NoteFields, name: &str, n: usize) -> String {
    let year = fields.day.get(..4).unwrap_or("Undated");
    let topic = match clean_part(&fields.topic, 40) {
        t if t.is_empty() => UNSORTED.to_owned(),
        t => t,
    };
    let title = match clean_part(&fields.title, 80) {
        t if t.is_empty() => "Note".to_owned(),
        t => t,
    };
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| clean_part(e, 8).to_lowercase())
        .filter(|e| !e.is_empty())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let again = if n == 0 {
        String::new()
    } else {
        format!(" ({})", n + 1)
    };
    format!("{year}/{topic}/{} {title}{again}{ext}", fields.day)
}

/// `text` safe as one folder or file name: no slashes, colons, control
/// characters or leading dots, at most `max` characters.
fn clean_part(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' || c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let one: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    // Never `.` or `..`, nor hidden: no leading dots (or the spaces between).
    let one = one.trim_start_matches(['.', ' ']);
    let cut: String = one.chars().take(max).collect();
    cut.trim().to_owned()
}

/// A title from a file's name: `blood-test_2026.pdf` → `blood test 2026`.
fn title_of(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    let t: String = stem
        .chars()
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .collect();
    match t.split_whitespace().collect::<Vec<_>>().join(" ") {
        t if t.is_empty() => "Note".to_owned(),
        t => t,
    }
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Copies `from` to `file` below `root`, making its folders (this Mac
/// account's alone).
fn copy_in(root: &Path, from: &Path, file: &str) -> Result<(), HubError> {
    let to = root.join(file);
    if let Some(parent) = to.parent() {
        crate::home::private_dir(parent).map_err(|e| not_kept(&e.to_string()))?;
    }
    std::fs::copy(from, &to).map_err(|e| not_kept(&e.to_string()))?;
    Ok(())
}

/// Moves `from` to `to`, both below `root`, making `to`'s folders; the
/// folder `from` leaves goes if it's empty.
fn move_within(root: &Path, from: &str, to: &str) -> Result<(), HubError> {
    let (old, new) = (root.join(from), root.join(to));
    if !inside(root, &old) || !inside(root, &new) {
        return Err(not_kept("that isn't in Records"));
    }
    if let Some(parent) = new.parent() {
        crate::home::private_dir(parent).map_err(|e| not_kept(&e.to_string()))?;
    }
    if old.is_file() {
        std::fs::rename(&old, &new).map_err(|e| not_kept(&e.to_string()))?;
        remove_empty_up(root, old.parent());
    }
    Ok(())
}

/// Removes `dir` and the folders above it while they're empty, up to (not
/// including) `root`.
fn remove_empty_up(root: &Path, mut dir: Option<&Path>) {
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Whether `path` is below `root`, with no `..` on the way.
fn inside(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        && path.strip_prefix(root).is_ok_and(|p| {
            p.components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
        })
}

fn not_kept(why: &str) -> HubError {
    HubError::NotAllowed(format!("Couldn't file it in Records: {why}."))
}

/// An attachment kind as the store keeps it (`pdf`).
fn kind_text(kind: AttachmentKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn kind_of(text: &str) -> AttachmentKind {
    serde_json::from_value(Value::String(text.to_owned())).unwrap_or(AttachmentKind::Document)
}

fn note_card(n: &HomeNote) -> EventKind {
    EventKind::HomeNote {
        id: n.id,
        title: n.fields.title.clone(),
        day: n.fields.day.clone(),
        file: n.file.clone(),
        private: n.private,
    }
}

fn note_info(n: HomeNote, names: &HashMap<PersonId, String>) -> HomeNoteInfo {
    let who = |a: &Option<Actor>| match a {
        Some(Actor::Person(p)) => names.get(p).cloned().unwrap_or_default(),
        Some(Actor::Bot(b)) => b.to_string(),
        None => String::new(),
    };
    HomeNoteInfo {
        id: n.id,
        uploaded_by: who(&n.uploaded_by),
        people: n
            .fields
            .people
            .iter()
            .filter_map(|p| names.get(p).cloned())
            .collect(),
        title: n.fields.title,
        day: n.fields.day,
        day_from: n.fields.day_from.as_str().to_owned(),
        topic: n.fields.topic,
        tags: n.fields.tags,
        summary: n.fields.summary,
        file: n.file,
        name: n.name,
        kind: kind_of(&n.kind),
        size: n.size,
        private: n.private,
        uploaded_ms: n.uploaded_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(title: &str, topic: &str) -> NoteFields {
        NoteFields {
            title: title.into(),
            day: "2026-11-17".into(),
            day_from: DayFrom::Upload,
            topic: topic.into(),
            tags: vec![],
            people: vec![],
            summary: String::new(),
        }
    }

    #[test]
    fn a_note_goes_by_year_topic_day_and_title() {
        assert_eq!(
            note_path(&fields("Blood test", "Medical"), "scan.PDF", 0),
            "2026/Medical/2026-11-17 Blood test.pdf"
        );
        assert_eq!(
            note_path(&fields("a/b: ../c", ""), "x", 1),
            "2026/Unsorted/2026-11-17 a b .. c (2)"
        );
        assert_eq!(
            note_path(&fields("..", "../.."), "x.jpg", 0),
            "2026/Unsorted/2026-11-17 Note.jpg"
        );
        assert_eq!(
            note_path(&fields(". ./x", ". . /.."), "y", 0),
            "2026/Unsorted/2026-11-17 x"
        );
        assert_eq!(title_of("blood-test_2026.pdf"), "blood test 2026");
    }

    #[test]
    fn a_question_looks_for_its_words() {
        assert_eq!(
            query_words("Find Akhila's last blood tests"),
            ["akhila", "blood", "test"]
        );
        assert_eq!(month_name("2026-03-01"), "march");
    }
}
