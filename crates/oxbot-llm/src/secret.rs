//! The OpenRouter key in memory.
//!
//! [`ApiKey`] never shows its contents: no `Display`, no `Serialize`, no
//! `Clone`, and `Debug` prints `ApiKey(…)`. Its memory is wiped when it is
//! dropped. [`KeyCell`] is the one shared place the client reads it from,
//! per request; the server sets and clears it on sign-in and sign-out.
//! [`SecretStore`] is where it is kept between runs (the macOS Keychain in
//! the app, [`MemorySecrets`] in tests).

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

/// What a bot and Settings → Engines say while the kept key is still
/// being read: the macOS Keychain may be asking the person first.
pub const KEYCHAIN_WAITING: &str = "Waiting for Keychain access: allow oxbot in the dialog.";

/// What a [`KeyCell`] holds.
#[derive(Default)]
enum KeyState {
    /// Not signed in.
    #[default]
    Empty,
    /// The kept key is still being read (see [`KeyCell::loading`]).
    Loading,
    /// A key.
    Set(Arc<ApiKey>),
}

/// The current key, shared by the server (which sets and clears it) and
/// every model client (which reads it per request). Cloning shares it.
#[derive(Clone, Default)]
pub struct KeyCell(Arc<RwLock<KeyState>>);

impl KeyCell {
    /// An empty cell (not signed in).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A cell whose kept key is still being read, in the background: it
    /// holds no key until [`KeyCell::finish_loading`] (or a sign-in or
    /// sign-out) settles it.
    #[must_use]
    pub fn loading() -> Self {
        Self(Arc::new(RwLock::new(KeyState::Loading)))
    }

    /// The kept key was read (`None`: there was none, or it couldn't be
    /// read). Ignored if a sign-in or sign-out already settled the cell.
    pub fn finish_loading(&self, key: Option<ApiKey>) {
        let mut state = self.0.write().unwrap_or_else(PoisonError::into_inner);
        if matches!(*state, KeyState::Loading) {
            *state = key.map_or(KeyState::Empty, |k| KeyState::Set(Arc::new(k)));
        }
    }

    /// Whether the kept key is still being read.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        matches!(
            *self.0.read().unwrap_or_else(PoisonError::into_inner),
            KeyState::Loading
        )
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
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = KeyState::Set(Arc::new(key));
    }

    /// Removes the key (sign-out). Requests in flight keep the copy they
    /// already took; new ones fail with "not signed in".
    pub fn clear(&self) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = KeyState::Empty;
    }

    /// Whether there is a key.
    #[must_use]
    pub fn is_set(&self) -> bool {
        matches!(
            *self.0.read().unwrap_or_else(PoisonError::into_inner),
            KeyState::Set(_)
        )
    }

    /// The key now, for one request.
    pub(crate) fn get(&self) -> Option<Arc<ApiKey>> {
        match &*self.0.read().unwrap_or_else(PoisonError::into_inner) {
            KeyState::Set(key) => Some(Arc::clone(key)),
            KeyState::Empty | KeyState::Loading => None,
        }
    }
}

impl fmt::Debug for KeyCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            match *self.0.read().unwrap_or_else(PoisonError::into_inner) {
                KeyState::Set(_) => "KeyCell(set)",
                KeyState::Loading => "KeyCell(loading)",
                KeyState::Empty => "KeyCell(empty)",
            },
        )
    }
}

/// Where the key is kept between runs: the macOS Keychain in the app,
/// memory (or a test file) in tests. Never a plain file, a log or the
/// database.
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// The key kept, if any.
    ///
    /// # Errors
    /// The store couldn't be read, or what it holds isn't a key.
    fn load(&self) -> Result<Option<ApiKey>, SecretError>;
    /// Keeps `key`, replacing any old one.
    ///
    /// # Errors
    /// The store couldn't be written.
    fn save(&self, key: &ApiKey) -> Result<(), SecretError>;
    /// Forgets the key (nothing kept is fine).
    ///
    /// # Errors
    /// The store couldn't be changed.
    fn delete(&self) -> Result<(), SecretError>;
}

/// A secret store failed. The text never holds the key.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SecretError(pub String);

/// A [`SecretStore`] in memory only: for tests, and when no other store
/// may be used.
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<Option<Zeroizing<Vec<u8>>>>);

impl MemorySecrets {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl fmt::Debug for MemorySecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemorySecrets")
    }
}

impl SecretStore for MemorySecrets {
    fn load(&self) -> Result<Option<ApiKey>, SecretError> {
        let held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        held.as_ref()
            .map(|bytes| ApiKey::from_bytes(bytes.to_vec()))
            .transpose()
            .map_err(|e| SecretError(format!("The kept key is unusable: {e}.")))
    }

    fn save(&self, key: &ApiKey) -> Result<(), SecretError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Zeroizing::new(key.expose_for_storage().to_vec()));
        Ok(())
    }

    fn delete(&self) -> Result<(), SecretError> {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
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
    fn memory_secrets_keep_one_key_and_never_show_it() {
        let store = MemorySecrets::new();
        assert!(store.load().unwrap().is_none());
        store.save(&ApiKey::new(FAKE.to_owned()).unwrap()).unwrap();
        assert_eq!(store.load().unwrap().unwrap().expose(), FAKE);
        assert_eq!(format!("{store:?}"), "MemorySecrets");
        store.delete().unwrap();
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
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

    #[test]
    fn a_loading_cell_has_no_key_until_it_is_settled() {
        let cell = KeyCell::loading();
        assert!(cell.is_loading() && !cell.is_set() && cell.get().is_none());
        assert_eq!(format!("{cell:?}"), "KeyCell(loading)");
        cell.finish_loading(Some(ApiKey::new(FAKE.to_owned()).unwrap()));
        assert!(!cell.is_loading() && cell.is_set());

        let none = KeyCell::loading();
        none.finish_loading(None);
        assert!(!none.is_loading() && !none.is_set());
    }

    #[test]
    fn a_sign_in_or_out_while_loading_wins() {
        let cell = KeyCell::loading();
        cell.set(ApiKey::new("sk-or-v1-new".to_owned()).unwrap());
        cell.finish_loading(Some(ApiKey::new(FAKE.to_owned()).unwrap()));
        assert_eq!(cell.get().unwrap().expose(), "sk-or-v1-new");

        let cell = KeyCell::loading();
        cell.clear();
        cell.finish_loading(Some(ApiKey::new(FAKE.to_owned()).unwrap()));
        assert!(!cell.is_set(), "signed out stays out");
    }
}
