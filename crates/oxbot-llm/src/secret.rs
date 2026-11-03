//! The OpenRouter key in memory.
//!
//! [`ApiKey`] never shows its contents: no `Display`, no `Serialize`, no
//! `Clone`, and `Debug` prints `ApiKey(…)`. Its memory is wiped when it is
//! dropped. [`KeyCell`] is the one shared place the client reads it from,
//! per request; the server sets and clears it on sign-in and sign-out.

use std::fmt;
use std::sync::{Arc, PoisonError, RwLock};

use reqwest::header::HeaderValue;
use zeroize::Zeroizing;

/// Longest key accepted (OpenRouter's are about 75 characters).
const MAX_KEY: usize = 512;

/// An API key. See the module docs for how it is kept hidden.
pub struct ApiKey(Zeroizing<String>);

/// Why a key was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// Empty.
    #[error("the key is empty")]
    Empty,
    /// Too long to be a key.
    #[error("the key is too long")]
    TooLong,
    /// Has characters a key can't have (spaces, control characters,
    /// anything outside visible ASCII).
    #[error("the key has characters a key can't have")]
    BadCharacters,
}

impl ApiKey {
    /// A key from text. Takes ownership so no copy is left behind; the text
    /// is wiped if it is refused.
    ///
    /// # Errors
    /// Empty, longer than 512 characters, or not visible ASCII.
    pub fn new(key: String) -> Result<Self, KeyError> {
        let key = Zeroizing::new(key);
        if key.is_empty() {
            return Err(KeyError::Empty);
        }
        if key.len() > MAX_KEY {
            return Err(KeyError::TooLong);
        }
        if !key.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(KeyError::BadCharacters);
        }
        Ok(Self(key))
    }

    /// A key from bytes, e.g. read from the Keychain. The bytes are wiped
    /// if they are refused.
    ///
    /// # Errors
    /// As [`ApiKey::new`], or the bytes aren't text.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, KeyError> {
        match String::from_utf8(bytes) {
            Ok(text) => Self::new(text),
            Err(err) => {
                drop(Zeroizing::new(err.into_bytes()));
                Err(KeyError::BadCharacters)
            }
        }
    }

    /// The key's bytes, only for storing it in the Keychain. Never log,
    /// print or send these anywhere else.
    #[must_use]
    pub fn expose_for_storage(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// The `Authorization` header for this key, marked sensitive so HTTP
    /// layers never log it.
    pub(crate) fn bearer(&self) -> Option<HeaderValue> {
        let text = Zeroizing::new(format!("Bearer {}", self.expose()));
        let mut value = HeaderValue::from_str(&text).ok()?;
        value.set_sensitive(true);
        Some(value)
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(…)")
    }
}

/// The current key, shared by the server (which sets and clears it) and
/// every model client (which reads it per request). Cloning shares it.
#[derive(Clone, Default)]
pub struct KeyCell(Arc<RwLock<Option<Arc<ApiKey>>>>);

impl KeyCell {
    /// An empty cell (not signed in).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A cell holding this key.
    #[must_use]
    pub fn with_key(key: ApiKey) -> Self {
        let cell = Self::new();
        cell.set(key);
        cell
    }

    /// Puts in a new key (after sign-in), replacing any old one.
    pub fn set(&self, key: ApiKey) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(key));
    }

    /// Removes the key (sign-out). Requests in flight keep the copy they
    /// already took; new ones fail with "not signed in".
    pub fn clear(&self) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Whether there is a key.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    /// The key now, for one request.
    pub(crate) fn get(&self) -> Option<Arc<ApiKey>> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl fmt::Debug for KeyCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.is_set() {
            "KeyCell(set)"
        } else {
            "KeyCell(empty)"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "sk-or-v1-test-0123456789abcdef";

    #[test]
    fn debug_never_shows_the_key() {
        let key = ApiKey::new(FAKE.to_owned()).unwrap();
        let shown = format!("{key:?} {key:#?}");
        assert!(!shown.contains("0123456789"), "{shown}");
        assert_eq!(format!("{key:?}"), "ApiKey(…)");

        let cell = KeyCell::with_key(key);
        let shown = format!("{cell:?} {cell:#?}");
        assert!(!shown.contains("0123456789"), "{shown}");
        assert_eq!(format!("{cell:?}"), "KeyCell(set)");
        cell.clear();
        assert_eq!(format!("{cell:?}"), "KeyCell(empty)");
    }

    #[test]
    fn bearer_header_is_sensitive_and_hidden_in_debug() {
        let key = ApiKey::new(FAKE.to_owned()).unwrap();
        let header = key.bearer().unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header.to_str().unwrap(), format!("Bearer {FAKE}"));
        assert!(!format!("{header:?}").contains("0123456789"));
    }

    #[test]
    fn bad_keys_are_refused() {
        assert_eq!(ApiKey::new(String::new()).unwrap_err(), KeyError::Empty);
        assert_eq!(
            ApiKey::new("a b".into()).unwrap_err(),
            KeyError::BadCharacters
        );
        assert_eq!(
            ApiKey::new("ab\n".into()).unwrap_err(),
            KeyError::BadCharacters
        );
        assert_eq!(
            ApiKey::new("é".into()).unwrap_err(),
            KeyError::BadCharacters
        );
        assert_eq!(ApiKey::new("a".repeat(513)).unwrap_err(), KeyError::TooLong);
        assert_eq!(
            ApiKey::from_bytes(vec![0xff, 0xfe]).unwrap_err(),
            KeyError::BadCharacters
        );
        assert_eq!(
            ApiKey::from_bytes(FAKE.as_bytes().to_vec())
                .unwrap()
                .expose(),
            FAKE
        );
    }

    #[test]
    fn cell_is_shared_between_clones() {
        let a = KeyCell::new();
        let b = a.clone();
        assert!(!b.is_set());
        a.set(ApiKey::new(FAKE.to_owned()).unwrap());
        assert!(b.is_set());
        let held = b.get().unwrap();
        a.clear();
        assert!(!b.is_set());
        // A request that already took the key keeps it until it ends.
        assert_eq!(held.expose(), FAKE);
        assert_eq!(held.expose_for_storage(), FAKE.as_bytes());
    }
}
