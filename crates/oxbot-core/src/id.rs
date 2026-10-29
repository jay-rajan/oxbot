//! [`BotId`]: a validated bot id.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Longest allowed bot id, in bytes.
pub const MAX_BOT_ID_LEN: usize = 40;

/// A bot's id: lowercase letters, digits and dashes, like `substack-daily`.
///
/// It doubles as the bot's folder name on disk, which is why it is strict.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BotId(String);

/// Why a string isn't a valid [`BotId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BotIdError {
    /// The id was empty.
    #[error("bot id is empty")]
    Empty,
    /// The id was longer than [`MAX_BOT_ID_LEN`].
    #[error("bot id is longer than {MAX_BOT_ID_LEN} characters")]
    TooLong,
    /// The id contained something other than `a-z`, `0-9` or `-`.
    #[error("bot id may only contain a-z, 0-9 and '-', found {0:?}")]
    BadChar(char),
    /// The id started or ended with a dash.
    #[error("bot id can't start or end with '-'")]
    EdgeDash,
}

impl BotId {
    /// Checks `id` and wraps it.
    ///
    /// # Errors
    /// Returns a [`BotIdError`] saying what's wrong with `id`.
    pub fn new(id: impl Into<String>) -> Result<Self, BotIdError> {
        let id = id.into();
        if id.is_empty() {
            return Err(BotIdError::Empty);
        }
        if id.len() > MAX_BOT_ID_LEN {
            return Err(BotIdError::TooLong);
        }
        if let Some(c) = id
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
        {
            return Err(BotIdError::BadChar(c));
        }
        if id.starts_with('-') || id.ends_with('-') {
            return Err(BotIdError::EdgeDash);
        }
        Ok(Self(id))
    }

    /// An id made from a display name: "Search Gmail" becomes
    /// `search-gmail`. If `taken` says that's used, `-2`, `-3`… is added.
    /// Names with nothing usable in them become `bot`.
    #[must_use]
    pub fn from_name(name: &str, taken: impl Fn(&Self) -> bool) -> Self {
        let mut slug = String::new();
        for c in name.trim().chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.ends_with('-') {
                slug.push('-');
            }
        }
        // Leave room for a "-NN" suffix.
        slug.truncate(MAX_BOT_ID_LEN - 4);
        let base = slug.trim_matches('-');
        let base = if base.is_empty() { "bot" } else { base };
        let mut n = 1u32;
        loop {
            let candidate = if n == 1 {
                base.to_owned()
            } else {
                format!("{base}-{n}")
            };
            // The slug only has a-z, 0-9 and inner dashes, so it's valid.
            let id = Self(candidate);
            if !taken(&id) {
                return id;
            }
            n += 1;
        }
    }

    /// The id as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for BotId {
    type Error = BotIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BotId> for String {
    fn from(id: BotId) -> Self {
        id.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_made_from_names() {
        let none = |_: &BotId| false;
        assert_eq!(
            BotId::from_name("Search Gmail", none).as_str(),
            "search-gmail"
        );
        assert_eq!(
            BotId::from_name("  Research on Substack! ", none).as_str(),
            "research-on-substack"
        );
        assert_eq!(
            BotId::from_name("C++ & Rust / Notes", none).as_str(),
            "c-rust-notes"
        );
        assert_eq!(BotId::from_name("日本", none).as_str(), "bot");
        let long = BotId::from_name(&"a".repeat(100), none);
        assert!(long.as_str().len() <= MAX_BOT_ID_LEN - 4);
        assert!(BotId::new(long.as_str()).is_ok());
    }

    #[test]
    fn taken_ids_get_a_number() {
        let taken = |id: &BotId| matches!(id.as_str(), "gmail" | "gmail-2");
        assert_eq!(BotId::from_name("Gmail", taken).as_str(), "gmail-3");
    }

    #[test]
    fn accepts_simple_ids() {
        assert_eq!(
            BotId::new("substack-daily").unwrap().as_str(),
            "substack-daily"
        );
        assert_eq!(BotId::new("bot2").unwrap().to_string(), "bot2");
    }

    #[test]
    fn rejects_bad_ids() {
        assert_eq!(BotId::new(""), Err(BotIdError::Empty));
        assert_eq!(BotId::new("Has Space"), Err(BotIdError::BadChar('H')));
        assert_eq!(BotId::new("../etc"), Err(BotIdError::BadChar('.')));
        assert_eq!(BotId::new("-x"), Err(BotIdError::EdgeDash));
        assert_eq!(BotId::new("a".repeat(41)), Err(BotIdError::TooLong));
    }
}
