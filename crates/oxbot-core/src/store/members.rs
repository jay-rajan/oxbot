//! Who a bot is shared with when its maker picked people (`bot_members`,
//! migration 25).

use std::collections::HashMap;

use rusqlite::params;

use super::{Store, StoreError, now_ms};
use crate::id::{BotId, PersonId};

impl Store {
    /// Makes `people` the ones `bot` is shared with (none: nobody).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_bot_members(&self, bot: &BotId, people: &[PersonId]) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM bot_members WHERE bot = ?1", [bot.as_str()])?;
        let now = now_ms();
        for person in people {
            tx.execute(
                "INSERT OR IGNORE INTO bot_members (bot, person, added_ms) VALUES (?1, ?2, ?3)",
                params![bot.as_str(), person.as_str(), now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Everyone each bot is shared with, by bot (oldest first).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn bot_members(&self) -> Result<HashMap<BotId, Vec<PersonId>>, StoreError> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT bot, person FROM bot_members ORDER BY added_ms, rowid")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out: HashMap<BotId, Vec<PersonId>> = HashMap::new();
        for row in rows {
            let (bot, person) = row?;
            // A row oxbot didn't write is skipped rather than trusted.
            if let (Ok(bot), Ok(person)) = (BotId::new(bot), PersonId::new(person)) {
                out.entry(bot).or_default().push(person);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bots_people_are_replaced_whole_and_go_with_it() {
        let store = Store::open_in_memory().unwrap();
        let garden = BotId::new("garden").unwrap();
        let (sam, ann) = (PersonId::random(), PersonId::random());
        store
            .set_bot_members(&garden, &[sam.clone(), ann.clone(), sam.clone()])
            .unwrap();
        assert_eq!(store.bot_members().unwrap()[&garden], [sam.clone(), ann]);
        store
            .set_bot_members(&garden, std::slice::from_ref(&sam))
            .unwrap();
        assert_eq!(store.bot_members().unwrap()[&garden], [sam]);
        store.delete_bot(&garden).unwrap();
        assert!(!store.bot_members().unwrap().contains_key(&garden));
    }
}
