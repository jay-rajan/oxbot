//! "Always allow" rules: what a bot may do without asking, one action on
//! one element label on one exact host.

use oxbot_protocol::AllowRule;
use rusqlite::{OptionalExtension as _, params};

use super::{Store, StoreError, now_ms};
use crate::id::{BotId, PersonId};

/// What a rule lets through: `action` (`click`) on `label`
/// (`button "Save"`) on `site` (an exact host, `www.example.com`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleKey {
    /// The exact host, lowercase.
    pub site: String,
    /// The action, e.g. `click`.
    pub action: String,
    /// The element's role and name, normalized (see `policy::rule_label`).
    pub label: String,
}

/// A rule as stored: who set it is a person id (`None`: the Farm).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRule {
    /// The rule; its `by` is empty (names are looked up by the caller).
    pub rule: AllowRule,
    /// Who set it.
    pub by_person: Option<PersonId>,
}

impl Store {
    /// Lets `bot` do `key` without asking from now on, set by `by` (`None`:
    /// the Farm). The same rule twice is kept once (the first).
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn add_allow_rule(
        &self,
        bot: &BotId,
        key: &RuleKey,
        by: Option<&PersonId>,
    ) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO allow_rules (bot, site, action, label, by_person, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(bot, site, action, label) DO NOTHING",
            params![
                bot.as_str(),
                key.site,
                key.action,
                key.label,
                by.map(PersonId::as_str),
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Whether one of `bot`'s rules lets `key` through. Exact matches only.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn allow_rule_matches(&self, bot: &BotId, key: &RuleKey) -> Result<bool, StoreError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM allow_rules
                 WHERE bot = ?1 AND site = ?2 AND action = ?3 AND label = ?4",
                params![bot.as_str(), key.site, key.action, key.label],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// `bot`'s rules, newest first.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn allow_rules(&self, bot: &BotId) -> Result<Vec<StoredRule>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, site, action, label, by_person, created_ms FROM allow_rules
             WHERE bot = ?1 ORDER BY created_ms DESC, id DESC",
        )?;
        let rows = stmt.query_map([bot.as_str()], |r| {
            Ok((
                AllowRule {
                    id: r.get(0)?,
                    site: r.get(1)?,
                    action: r.get(2)?,
                    label: r.get(3)?,
                    by: None,
                    created_ms: r.get(5)?,
                },
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (rule, by) = row?;
            Ok(StoredRule {
                rule,
                // An id that doesn't parse (edited by hand) shows as nobody.
                by_person: by.and_then(|id| PersonId::new(id).ok()),
            })
        })
        .collect()
    }

    /// Removes `bot`'s rule `id`. Returns whether it was there.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn remove_allow_rule(&self, bot: &BotId, id: i64) -> Result<bool, StoreError> {
        Ok(self.conn().execute(
            "DELETE FROM allow_rules WHERE bot = ?1 AND id = ?2",
            params![bot.as_str(), id],
        )? > 0)
    }
}
