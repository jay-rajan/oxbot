//! The phone's own copy of the chats it can see, so they can be read away
//! from home.
//!
//! It's filled from what the Farm sends over the link anyway: no extra
//! requests. Limits: the latest [`KEEP_MESSAGES`] messages per bot (with
//! the steps between them), nothing older than [`KEEP_DAYS`] days, only
//! bots this phone's person may open, and no file contents (events carry
//! only short summaries of what bots did). Forgetting the Farm deletes it.

use std::path::{Path, PathBuf};

use oxbot_protocol::{BotEvent, BotInfo, EventKind, ServerMsg};
use rusqlite::{Connection, OptionalExtension as _, params};

/// Messages kept per bot.
pub const KEEP_MESSAGES: i64 = 300;
/// Days kept.
pub const KEEP_DAYS: i64 = 30;
/// How many new events between tidy-ups.
const TIDY_EVERY: u32 = 50;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    bot     TEXT NOT NULL,
    seq     INTEGER NOT NULL,
    ts_ms   INTEGER NOT NULL,
    message INTEGER NOT NULL,
    json    TEXT NOT NULL,
    PRIMARY KEY (bot, seq)
);
CREATE TABLE IF NOT EXISTS latest (
    kind TEXT PRIMARY KEY,
    json TEXT NOT NULL
);
";

/// What can go wrong with the copy (it's never worth losing the link over).
pub type CopyError = rusqlite::Error;

/// The phone's copy of its chats, in a small SQLite file.
#[derive(Debug)]
pub struct ChatCopy {
    conn: Connection,
    path: PathBuf,
    /// Who this phone is for: only their bots are kept.
    person: String,
    since_tidy: u32,
}

impl ChatCopy {
    /// Opens (or makes) the copy at `path` for `person`.
    ///
    /// # Errors
    /// The file can't be opened.
    pub fn open(path: &Path, person: &str) -> Result<Self, CopyError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            path: path.to_owned(),
            person: person.to_owned(),
            since_tidy: 0,
        })
    }

    /// Keeps what's worth keeping from one message from the Farm (JSON
    /// text): bot events, and the latest hello, settings, people, sections
    /// and bot list. Anything else, or anything unreadable, is skipped.
    ///
    /// # Errors
    /// The file can't be written.
    pub fn record(&mut self, text: &str, now_ms: i64) -> Result<(), CopyError> {
        let Ok(msg) = serde_json::from_str::<ServerMsg>(text) else {
            return Ok(());
        };
        match msg {
            ServerMsg::Event(e) => self.keep_event(&e, text, now_ms)?,
            ServerMsg::History { events, .. } => self.keep_page(&events, now_ms)?,
            ServerMsg::Bots { bots } => {
                self.keep_latest("bots", text)?;
                self.drop_hidden(&bots)?;
            }
            ServerMsg::Hello(_) => self.keep_latest("hello", text)?,
            ServerMsg::Settings(_) => self.keep_latest("settings", text)?,
            ServerMsg::People { .. } => self.keep_latest("people", text)?,
            // Only this phone's person's own sections ever reach it.
            ServerMsg::Sections { .. } => self.keep_latest("sections", text)?,
            _ => {}
        }
        Ok(())
    }

    /// Keeps one event; `json` is it as a [`ServerMsg::Event`].
    fn keep_event(&mut self, e: &BotEvent, json: &str, now_ms: i64) -> Result<(), CopyError> {
        insert_event(&self.conn, e, json)?;
        self.since_tidy += 1;
        if self.since_tidy >= TIDY_EVERY {
            self.tidy(now_ms)?;
        }
        Ok(())
    }

    /// Keeps a page of history as the events it holds (so the copy replays
    /// the same way however the chat arrived), in one write, then tidies:
    /// a page can hold hundreds. Events older than [`KEEP_DAYS`] days are
    /// skipped rather than written and deleted.
    fn keep_page(&mut self, events: &[BotEvent], now_ms: i64) -> Result<(), CopyError> {
        let oldest = oldest_kept(now_ms);
        let tx = self.conn.transaction()?;
        for e in events.iter().filter(|e| e.ts_ms >= oldest) {
            if let Ok(json) = serde_json::to_string(&ServerMsg::Event(e.clone())) {
                insert_event(&tx, e, &json)?;
            }
        }
        tx.commit()?;
        self.tidy(now_ms)
    }

    fn keep_latest(&self, kind: &str, text: &str) -> Result<(), CopyError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO latest (kind, json) VALUES (?1, ?2)",
            params![kind, text],
        )?;
        Ok(())
    }

    /// Deletes chats of bots that are gone or this person may no longer open.
    /// The Farm says which: a bot `locked` to this phone's person goes. A
    /// private one also needs to be theirs (they may manage it), or, from a
    /// Farm too old to say, made under their name.
    fn drop_hidden(&self, bots: &[BotInfo]) -> Result<(), CopyError> {
        let open: Vec<&str> = bots
            .iter()
            .filter(|b| {
                !b.locked
                    && (!b.private
                        || b.can_manage
                        || b.created_by.as_deref() == Some(self.person.as_str()))
            })
            .map(|b| b.id.as_str())
            .collect();
        let mut stmt = self.conn.prepare("SELECT DISTINCT bot FROM events")?;
        let kept: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for bot in kept.iter().filter(|b| !open.contains(&b.as_str())) {
            self.conn
                .execute("DELETE FROM events WHERE bot = ?1", params![bot])?;
        }
        Ok(())
    }

    /// Applies the limits: [`KEEP_DAYS`] days, [`KEEP_MESSAGES`] messages
    /// per bot.
    ///
    /// # Errors
    /// The file can't be written.
    pub fn tidy(&mut self, now_ms: i64) -> Result<(), CopyError> {
        self.since_tidy = 0;
        let oldest = oldest_kept(now_ms);
        self.conn
            .execute("DELETE FROM events WHERE ts_ms < ?1", params![oldest])?;
        let mut stmt = self.conn.prepare("SELECT DISTINCT bot FROM events")?;
        let bots: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        for bot in bots {
            // The seq of the oldest message to keep; everything before it goes.
            let cut: Option<i64> = self
                .conn
                .query_row(
                    "SELECT seq FROM events WHERE bot = ?1 AND message = 1
                     ORDER BY seq DESC LIMIT 1 OFFSET ?2",
                    params![bot, KEEP_MESSAGES - 1],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(cut) = cut {
                self.conn.execute(
                    "DELETE FROM events WHERE bot = ?1 AND seq < ?2",
                    params![bot, cut],
                )?;
            }
        }
        Ok(())
    }

    /// What to show while away from the Farm, in the order the page expects:
    /// hello, settings, people, sections, bots, then each bot's events.
    ///
    /// # Errors
    /// The file can't be read.
    pub fn replay(&self) -> Result<Vec<String>, CopyError> {
        let mut out = Vec::new();
        for kind in ["hello", "settings", "people", "sections", "bots"] {
            let json: Option<String> = self
                .conn
                .query_row("SELECT json FROM latest WHERE kind = ?1", [kind], |r| {
                    r.get(0)
                })
                .optional()?;
            out.extend(json);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT json FROM events ORDER BY bot, seq")?;
        let events = stmt.query_map([], |r| r.get::<_, String>(0))?;
        for json in events {
            out.push(json?);
        }
        Ok(out)
    }

    /// Deletes the copy and its file.
    pub fn wipe(self) {
        let path = self.path.clone();
        drop(self);
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}

/// The time of the oldest event kept at `now_ms`.
fn oldest_kept(now_ms: i64) -> i64 {
    now_ms - KEEP_DAYS * 24 * 60 * 60 * 1000
}

/// Writes one event; `json` is it as a [`ServerMsg::Event`].
fn insert_event(conn: &Connection, e: &BotEvent, json: &str) -> Result<(), CopyError> {
    let message = matches!(
        e.kind,
        EventKind::UserMessage { .. } | EventKind::AssistantMessage { .. }
    );
    conn.execute(
        "INSERT OR REPLACE INTO events (bot, seq, ts_ms, message, json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![e.bot, i64::try_from(e.seq).unwrap_or(i64::MAX), e.ts_ms, message, json],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxbot_protocol::BrowserState;

    const DAY: i64 = 24 * 60 * 60 * 1000;
    const NOW: i64 = 100 * DAY;

    fn event(bot: &str, seq: u64, ts_ms: i64, message: bool) -> String {
        let kind = if message {
            EventKind::AssistantMessage {
                text: format!("reply {seq}"),
            }
        } else {
            EventKind::ToolUse {
                name: "Read".into(),
                summary: "notes.md".into(),
            }
        };
        serde_json::to_string(&ServerMsg::Event(BotEvent {
            bot: bot.into(),
            seq,
            ts_ms,
            kind,
        }))
        .unwrap()
    }

    fn bots(list: &[(&str, bool, Option<&str>)]) -> String {
        serde_json::to_string(&ServerMsg::Bots {
            bots: list
                .iter()
                .map(|(id, private, by)| BotInfo {
                    id: (*id).into(),
                    name: (*id).into(),
                    busy: false,
                    last_seq: 0,
                    last_reply_seq: 0,
                    browser_state: BrowserState::Off,
                    central: false,
                    created_by: by.map(Into::into),
                    private: *private,
                    ..BotInfo::default()
                })
                .collect(),
        })
        .unwrap()
    }

    fn copy() -> (tempfile::TempDir, ChatCopy) {
        let dir = tempfile::tempdir().unwrap();
        let copy = ChatCopy::open(&dir.path().join("chats.db"), "Ann").unwrap();
        (dir, copy)
    }

    #[test]
    fn replays_what_it_kept_in_order() {
        let (_dir, mut copy) = copy();
        copy.record(&bots(&[("recipes", false, None)]), NOW)
            .unwrap();
        copy.record(&event("recipes", 2, NOW, true), NOW).unwrap();
        copy.record(&event("recipes", 1, NOW, false), NOW).unwrap();
        copy.record(
            "{\"type\":\"delta\",\"bot\":\"recipes\",\"text\":\"x\"}",
            NOW,
        )
        .unwrap();
        copy.record("not json", NOW).unwrap();
        let replay = copy.replay().unwrap();
        assert_eq!(replay.len(), 3);
        assert!(replay[0].contains("\"bots\""));
        assert!(replay[1].contains("\"seq\":1"));
        assert!(replay[2].contains("\"seq\":2"));
    }

    #[test]
    fn sections_are_kept_and_replayed_after_people_before_bots() {
        let (_dir, mut copy) = copy();
        let sections = |name: &str| {
            format!(
                "{{\"type\":\"sections\",\"sections\":[{{\"id\":\"s_1\",\"name\":\"{name}\"}}],\"placed\":{{\"recipes\":\"s_1\"}}}}"
            )
        };
        copy.record(&bots(&[("recipes", false, None)]), NOW)
            .unwrap();
        copy.record(&sections("Kitchen"), NOW).unwrap();
        copy.record(&sections("Food"), NOW).unwrap();
        copy.record("{\"type\":\"people\",\"people\":[]}", NOW)
            .unwrap();
        let replay = copy.replay().unwrap();
        assert_eq!(replay.len(), 3, "{replay:?}");
        assert!(replay[0].contains("\"people\""));
        assert!(
            replay[1].contains("\"sections\"") && replay[1].contains("Food"),
            "the latest sections"
        );
        assert!(replay[2].contains("\"bots\""));
    }

    #[test]
    fn a_page_of_history_is_kept_as_its_events() {
        let (_dir, mut copy) = copy();
        let page = |seqs: std::ops::RangeInclusive<u64>| {
            serde_json::to_string(&ServerMsg::History {
                bot: "recipes".into(),
                events: seqs
                    .map(|seq| BotEvent {
                        bot: "recipes".into(),
                        seq,
                        ts_ms: NOW,
                        kind: EventKind::AssistantMessage {
                            text: format!("reply {seq}"),
                        },
                    })
                    .collect(),
                first_seq: 1,
                has_older: false,
            })
            .unwrap()
        };
        copy.record(&page(3..=4), NOW).unwrap();
        copy.record(&page(1..=2), NOW).unwrap();
        let replay = copy.replay().unwrap();
        assert_eq!(replay.len(), 4);
        for (i, json) in replay.iter().enumerate() {
            let Ok(ServerMsg::Event(e)) = serde_json::from_str(json) else {
                panic!("an event: {json}")
            };
            assert_eq!(e.seq, u64::try_from(i).unwrap() + 1);
        }
    }

    #[test]
    fn a_page_skips_old_events_and_is_tidied_at_once() {
        let (_dir, mut copy) = copy();
        let total = u64::try_from(KEEP_MESSAGES).unwrap() + 5;
        let page = serde_json::to_string(&ServerMsg::History {
            bot: "recipes".into(),
            events: (1..=total)
                .map(|seq| BotEvent {
                    bot: "recipes".into(),
                    seq,
                    ts_ms: if seq == 1 { NOW - 31 * DAY } else { NOW },
                    kind: EventKind::AssistantMessage {
                        text: format!("reply {seq}"),
                    },
                })
                .collect(),
            first_seq: 1,
            has_older: false,
        })
        .unwrap();
        copy.record(&page, NOW).unwrap();
        let replay = copy.replay().unwrap();
        assert_eq!(
            replay.len(),
            usize::try_from(KEEP_MESSAGES).unwrap(),
            "tidied right after the page"
        );
        assert!(replay.iter().all(|j| !j.contains("\"seq\":1,")));
    }

    #[test]
    fn the_farm_says_which_bots_this_person_cant_open() {
        let (_dir, mut copy) = copy();
        for bot in ["locked", "renamed", "shared"] {
            copy.record(&event(bot, 1, NOW, true), NOW).unwrap();
        }
        let list = |bots: Vec<BotInfo>| serde_json::to_string(&ServerMsg::Bots { bots }).unwrap();
        let bot = |id: &str, private, by: &str, locked, can_manage| BotInfo {
            id: id.into(),
            name: id.into(),
            created_by: Some(by.into()),
            private,
            locked,
            can_manage,
            ..BotInfo::default()
        };
        copy.record(
            &list(vec![
                // Made under Ann's name, but someone else's now.
                bot("locked", true, "Ann", true, false),
                // Ann's, though shown under her new name.
                bot("renamed", true, "Annie", false, true),
                bot("shared", false, "Sam", false, false),
            ]),
            NOW,
        )
        .unwrap();
        let replay = copy.replay().unwrap();
        let kept = |id: &str| {
            replay
                .iter()
                .any(|j| j.contains(&format!("\"bot\":\"{id}\"")))
        };
        assert!(!kept("locked") && kept("renamed") && kept("shared"));
    }

    #[test]
    fn keeps_the_latest_messages_and_thirty_days() {
        let (_dir, mut copy) = copy();
        let total = u64::try_from(KEEP_MESSAGES).unwrap() + 20;
        for seq in 1..=total {
            copy.record(&event("recipes", seq * 2 - 1, NOW, false), NOW)
                .unwrap();
            copy.record(&event("recipes", seq * 2, NOW, true), NOW)
                .unwrap();
        }
        copy.record(&event("old", 1, NOW - 31 * DAY, true), NOW)
            .unwrap();
        copy.tidy(NOW).unwrap();
        let replay = copy.replay().unwrap();
        let messages = replay.iter().filter(|j| j.contains("reply")).count();
        assert_eq!(messages, usize::try_from(KEEP_MESSAGES).unwrap());
        assert!(replay.iter().all(|j| !j.contains("\"bot\":\"old\"")));
    }

    #[test]
    fn drops_bots_this_person_cant_open_and_wipes_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chats.db");
        let mut copy = ChatCopy::open(&path, "Ann").unwrap();
        copy.record(&event("bank", 1, NOW, true), NOW).unwrap();
        copy.record(&event("mine", 1, NOW, true), NOW).unwrap();
        copy.record(
            &bots(&[("bank", true, Some("Sam")), ("mine", true, Some("Ann"))]),
            NOW,
        )
        .unwrap();
        let replay = copy.replay().unwrap();
        assert!(replay.iter().all(|j| !j.contains("\"bot\":\"bank\"")));
        assert!(replay.iter().any(|j| j.contains("\"bot\":\"mine\"")));
        copy.wipe();
        assert!(!path.exists());
    }
}
