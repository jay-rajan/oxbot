//! Where the household's `OpenRouter` key is kept: the macOS Keychain, and
//! nowhere else on disk.
//!
//! [`secret_store`] picks the store. The app and `oxbot serve` use the
//! [`Keychain`] (one item per data folder, so a dev `OXBOT_HOME` never
//! shares the app's key). End-to-end tests (`OXBOT_E2E=1`) never touch
//! the Keychain: they get the stand-in's canary key from the file named
//! by `OXBOT_E2E_OPENROUTER_KEY_FILE` ([`TestKeyFile`]), or nothing. Unit
//! tests always get [`MemorySecrets`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use oxbot_llm::{ApiKey, MemorySecrets, SecretError, SecretStore};

/// The Keychain item's service name.
pub const SERVICE: &str = "oxbot OpenRouter key";
/// The file holding the e2e stand-in's key (test mode only).
pub const E2E_KEY_FILE: &str = "OXBOT_E2E_OPENROUTER_KEY_FILE";

/// The store for the data folder `data_dir`. `e2e`: test mode, which
/// never uses the Keychain.
#[must_use]
pub fn secret_store(data_dir: &Path, e2e: bool) -> Arc<dyn SecretStore> {
    // This crate's own tests never reach the Keychain either.
    if cfg!(test) {
        return Arc::new(MemorySecrets::new());
    }
    if e2e {
        return match std::env::var_os(E2E_KEY_FILE) {
            Some(path) => Arc::new(TestKeyFile::new(PathBuf::from(path))),
            None => Arc::new(MemorySecrets::new()),
        };
    }
    Arc::new(Keychain::for_data_dir(data_dir))
}

/// The Keychain item for one data folder: service [`SERVICE`], account
/// `household-` and the first 12 hex digits of the folder path's SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keychain {
    account: String,
}

impl Keychain {
    /// The item for `data_dir`.
    #[must_use]
    pub fn for_data_dir(data_dir: &Path) -> Self {
        Self {
            account: account_for(data_dir),
        }
    }

    /// Its account name.
    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }
}

fn account_for(data_dir: &Path) -> String {
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        data_dir.as_os_str().as_encoded_bytes(),
    );
    let hex = crate::secret::hex(digest.as_ref());
    format!("household-{}", &hex[..12])
}

#[cfg(target_os = "macos")]
mod mac {
    use security_framework::passwords::{
        PasswordOptions, delete_generic_password, generic_password, set_generic_password,
    };

    use super::{ApiKey, Keychain, SERVICE, SecretError, SecretStore};

    /// `errSecItemNotFound`.
    const NOT_FOUND: i32 = -25300;

    fn failed(what: &str, err: security_framework::base::Error) -> SecretError {
        // The error says what failed (a status code), never the item's data.
        SecretError(format!(
            "The Keychain couldn't {what} the OpenRouter key ({}).",
            err.code()
        ))
    }

    impl SecretStore for Keychain {
        fn load(&self) -> Result<Option<ApiKey>, SecretError> {
            match generic_password(PasswordOptions::new_generic_password(
                SERVICE,
                &self.account,
            )) {
                Ok(bytes) => ApiKey::from_bytes(bytes)
                    .map(Some)
                    .map_err(|e| SecretError(format!("The kept OpenRouter key is unusable: {e}."))),
                Err(err) if err.code() == NOT_FOUND => Ok(None),
                Err(err) => Err(failed("read", err)),
            }
        }

        fn save(&self, key: &ApiKey) -> Result<(), SecretError> {
            set_generic_password(SERVICE, &self.account, key.expose_for_storage())
                .map_err(|e| failed("save", e))
        }

        fn delete(&self) -> Result<(), SecretError> {
            match delete_generic_password(SERVICE, &self.account) {
                Err(err) if err.code() != NOT_FOUND => Err(failed("delete", err)),
                _ => Ok(()),
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl SecretStore for Keychain {
    fn load(&self) -> Result<Option<ApiKey>, SecretError> {
        Ok(None)
    }

    fn save(&self, _key: &ApiKey) -> Result<(), SecretError> {
        Err(SecretError(
            "There is no Keychain on this system.".to_owned(),
        ))
    }

    fn delete(&self) -> Result<(), SecretError> {
        Ok(())
    }
}

/// Test mode's store: starts with the key in a file the e2e runner made
/// (outside oxbot's data folder), then keeps changes in memory only. The
/// file is never written or deleted.
pub struct TestKeyFile {
    path: PathBuf,
    /// `None`: still the file's key. `Some`: what sign-in or sign-out left.
    changed: Mutex<Option<MemorySecrets>>,
}

impl TestKeyFile {
    /// The store starting from `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            changed: Mutex::new(None),
        }
    }
}

impl std::fmt::Debug for TestKeyFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TestKeyFile")
    }
}

impl SecretStore for TestKeyFile {
    fn load(&self) -> Result<Option<ApiKey>, SecretError> {
        if let Some(memory) = &*self.changed.lock().unwrap_or_else(PoisonError::into_inner) {
            return memory.load();
        }
        match std::fs::read(&self.path) {
            Ok(mut bytes) => {
                while bytes.last().is_some_and(u8::is_ascii_whitespace) {
                    bytes.pop();
                }
                ApiKey::from_bytes(bytes)
                    .map(Some)
                    .map_err(|e| SecretError(format!("The test key file is unusable: {e}.")))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(SecretError(format!(
                "The test key file couldn't be read: {}.",
                err.kind()
            ))),
        }
    }

    fn save(&self, key: &ApiKey) -> Result<(), SecretError> {
        let memory = MemorySecrets::new();
        memory.save(key)?;
        *self.changed.lock().unwrap_or_else(PoisonError::into_inner) = Some(memory);
        Ok(())
    }

    fn delete(&self) -> Result<(), SecretError> {
        *self.changed.lock().unwrap_or_else(PoisonError::into_inner) = Some(MemorySecrets::new());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE: &str = "sk-or-v1-test-00112233445566778899";

    #[test]
    fn each_data_folder_has_its_own_item() {
        let app = Keychain::for_data_dir(Path::new("/Users/jay/Library/Application Support/oxbot"));
        let dev = Keychain::for_data_dir(Path::new("/tmp/oxbot-dev"));
        assert_ne!(app, dev);
        assert!(app.account().starts_with("household-"));
        assert_eq!(app.account().len(), "household-".len() + 12);
        assert_eq!(
            app,
            Keychain::for_data_dir(Path::new("/Users/jay/Library/Application Support/oxbot"))
        );
    }

    #[test]
    fn tests_never_get_the_keychain() {
        let store = secret_store(Path::new("/tmp/x"), false);
        assert_eq!(format!("{store:?}"), "MemorySecrets");
    }

    #[test]
    fn the_test_key_file_is_read_and_never_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("canary");
        std::fs::write(&path, format!("{FAKE}\n")).unwrap();
        let store = TestKeyFile::new(path.clone());
        assert!(!format!("{store:?}").contains(FAKE));
        assert!(store.load().unwrap().is_some());
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none(), "signed out");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{FAKE}\n"));
        store.save(&ApiKey::new(FAKE.to_owned()).unwrap()).unwrap();
        assert!(store.load().unwrap().is_some());
        let missing = TestKeyFile::new(tmp.path().join("none"));
        assert!(missing.load().unwrap().is_none());
    }
}
