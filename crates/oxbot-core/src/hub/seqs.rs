//! [`LatestSeqs`]: each bot's latest event and reply numbers, kept in
//! memory as events are recorded, so listing bots and reading replies
//! don't search the history.

use std::collections::HashMap;

use oxbot_protocol::{BotEvent, EventKind};

use super::lock;
use crate::id::BotId;
use crate::store::{Seqs, Store};

/// Each bot's latest numbers. Read for every bot at start; if that read
/// fails, each bot's are read from the store the first time they're needed.
#[derive(Debug)]
pub(super) struct LatestSeqs {
    seqs: std::sync::Mutex<HashMap<BotId, Seqs>>,
    /// Whether every bot's numbers were read at start. Without it, a bot
    /// missing from the map hasn't been read yet (rather than having none).
    loaded: bool,
}

impl LatestSeqs {
    /// Every bot's numbers, read from `store` in one go.
    pub(super) fn load(store: &Store) -> Self {
        match store.all_seqs() {
            Ok(all) => Self {
                seqs: std::sync::Mutex::new(
                    all.into_iter()
                        .filter_map(|(id, seqs)| Some((BotId::new(&id).ok()?, seqs)))
                        .collect(),
                ),
                loaded: true,
            },
            Err(err) => {
                tracing::warn!(%err, "couldn't read the bots' latest event numbers; reading each when needed");
                Self {
                    seqs: std::sync::Mutex::default(),
                    loaded: false,
                }
            }
        }
    }

    /// `bot`'s numbers (0 if it has none).
    pub(super) fn of(&self, store: &Store, bot: &BotId) -> Seqs {
        if let Some(seqs) = lock(&self.seqs).get(bot) {
            return *seqs;
        }
        if self.loaded {
            return Seqs::default();
        }
        let read = read_one(store, bot);
        // Another thread may have noted a newer event meanwhile.
        let mut seqs = lock(&self.seqs);
        let entry = seqs.entry(bot.clone()).or_insert(read);
        *entry = max(*entry, read);
        *entry
    }

    /// Every known bot's numbers, for the bot list. Bots not read yet (only
    /// when the start-up read failed) are read now.
    pub(super) fn for_bots<'a>(
        &self,
        store: &Store,
        bots: impl IntoIterator<Item = &'a BotId>,
    ) -> HashMap<BotId, Seqs> {
        bots.into_iter()
            .map(|id| (id.clone(), self.of(store, id)))
            .collect()
    }

    /// Keeps `event`'s bot's numbers up to date (it's already stored).
    pub(super) fn note(&self, store: &Store, event: &BotEvent) {
        let Ok(bot) = BotId::new(&event.bot) else {
            return;
        };
        // Not read yet: the store has this event too, so start from there.
        if !self.loaded && !lock(&self.seqs).contains_key(&bot) {
            self.of(store, &bot);
        }
        let mut seqs = lock(&self.seqs);
        let seq = seqs.entry(bot).or_default();
        // Two turns can record at once; numbers only go up.
        seq.last = seq.last.max(event.seq);
        if matches!(event.kind, EventKind::AssistantMessage { .. }) {
            seq.last_reply = seq.last_reply.max(event.seq);
        }
    }

    /// Forgets a deleted bot: a new bot with its id starts from 0.
    pub(super) fn remove(&self, bot: &BotId) {
        let mut seqs = lock(&self.seqs);
        if self.loaded {
            seqs.remove(bot);
        } else {
            // Its history is gone, so there's nothing to read back.
            seqs.insert(bot.clone(), Seqs::default());
        }
    }
}

/// `bot`'s numbers straight from the store (0s if it can't be read).
fn read_one(store: &Store, bot: &BotId) -> Seqs {
    let read = store
        .last_seq(bot)
        .and_then(|last| Ok((last, store.last_reply_seq(bot)?)));
    match read {
        Ok((last, last_reply)) => Seqs { last, last_reply },
        Err(err) => {
            tracing::warn!(%bot, %err, "couldn't read a bot's latest event numbers");
            Seqs::default()
        }
    }
}

fn max(a: Seqs, b: Seqs) -> Seqs {
    Seqs {
        last: a.last.max(b.last),
        last_reply: a.last_reply.max(b.last_reply),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(bot: &str, seq: u64) -> BotEvent {
        BotEvent {
            bot: bot.into(),
            seq,
            ts_ms: 0,
            kind: EventKind::AssistantMessage { text: "hi".into() },
        }
    }

    #[test]
    fn a_failed_start_up_read_falls_back_to_each_bot() {
        let store = Store::open_in_memory().unwrap();
        let a = BotId::new("a").unwrap();
        store
            .append(&a, EventKind::AssistantMessage { text: "x".into() })
            .unwrap();
        store
            .append(
                &a,
                EventKind::ToolUse {
                    name: "Read".into(),
                    summary: "y".into(),
                },
            )
            .unwrap();
        // As if the start-up read had failed.
        let seqs = LatestSeqs {
            seqs: std::sync::Mutex::default(),
            loaded: false,
        };
        assert_eq!(
            seqs.of(&store, &a),
            Seqs {
                last: 2,
                last_reply: 1
            },
            "read from the store, not 0"
        );
        // A bot not read yet gets its stored numbers before the new one.
        let b = BotId::new("b").unwrap();
        for _ in 0..4 {
            store
                .append(&b, EventKind::AssistantMessage { text: "x".into() })
                .unwrap();
        }
        let ev = store
            .append(
                &b,
                EventKind::ToolUse {
                    name: "Read".into(),
                    summary: "z".into(),
                },
            )
            .unwrap();
        seqs.note(&store, &ev);
        assert_eq!(
            seqs.of(&store, &b),
            Seqs {
                last: 5,
                last_reply: 4
            }
        );
        seqs.remove(&b);
        assert_eq!(seqs.of(&store, &b), Seqs::default());
        let all = seqs.for_bots(&store, [&a, &b]);
        assert_eq!(all[&a].last, 2);
    }

    #[test]
    fn numbers_only_go_up() {
        let store = Store::open_in_memory().unwrap();
        let seqs = LatestSeqs::load(&store);
        seqs.note(&store, &reply("a", 5));
        seqs.note(&store, &reply("a", 3));
        let a = BotId::new("a").unwrap();
        assert_eq!(
            seqs.of(&store, &a),
            Seqs {
                last: 5,
                last_reply: 5
            }
        );
        seqs.remove(&a);
        assert_eq!(seqs.of(&store, &a), Seqs::default());
    }
}
