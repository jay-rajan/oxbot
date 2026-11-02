//! App-wide preferences that outlive the process: small text values by key.

use rusqlite::{OptionalExtension as _, params};

use super::{Store, StoreError};

impl Store {
    /// The preference `key`, if it was ever set.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn pref(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM prefs WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Sets the preference `key`.
    ///
    /// # Errors
    /// Fails if SQLite fails.
    pub fn set_pref(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn().execute(
            "INSERT INTO prefs (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}
