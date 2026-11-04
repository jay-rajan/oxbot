//! Where the household's `OpenRouter` key is kept: the macOS Keychain.
//!
//! [`secret_store`] picks the store. The app and `oxbot serve` use the
//! [`Keychain`] (one item per data folder, so a dev `OXBOT_HOME` never
//! shares the app's key). A dev or test copy started with
//! `OXBOT_KEY_STORE=file` and its own `OXBOT_HOME` keeps its key in a file
//! only the person can read ([`KeyFile`]) instead: an unsigned dev build
//! is a new app to the Keychain after every rebuild, so it would ask
//! again each time. The app's own data folder always uses the Keychain.
//! End-to-end tests (`OXBOT_E2E=1`) never touch the Keychain: they get the
//! stand-in's canary key from the file named by
//! `OXBOT_E2E_OPENROUTER_KEY_FILE` ([`TestKeyFile`]), or nothing. Unit
//! tests always get [`MemorySecrets`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use oxbot_llm::{ApiKey, MemorySecrets, SecretError, SecretStore};

/// The Keychain item's service name.
pub const SERVICE: &str = "oxbot OpenRouter key";
/// The file holding the e2e stand-in's key (test mode only).
pub const E2E_KEY_FILE: &str = "OXBOT_E2E_OPENROUTER_KEY_FILE";
/// Test mode only: how long the first read of the test key takes, in ms,
/// as a Keychain waiting for the person's OK would.
pub const E2E_KEYCHAIN_DELAY: &str = "OXBOT_E2E_KEYCHAIN_DELAY_MS";

/// Set to `file` for a dev or test copy's key file ([`KeyFile`]).
pub const KEY_STORE: &str = "OXBOT_KEY_STORE";

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
            Some(path) => {
                let delay = std::env::var(E2E_KEYCHAIN_DELAY)
                    .ok()
                    .and_then(|ms| ms.parse().ok())
                    .map_or(std::time::Duration::ZERO, std::time::Duration::from_millis);
                Arc::new(TestKeyFile::new(PathBuf::from(path)).with_delay(delay))
            }
            None => Arc::new(MemorySecrets::new()),
        };
    }
    let app_dir = dirs::data_dir().map(|d| d.join("oxbot"));
    let wanted = std::env::var(KEY_STORE).ok();
    match pick(data_dir, wanted.as_deref(), app_dir.as_deref()) {
        Pick::File => {
            tracing::info!(
                "the OpenRouter key is kept in this data folder's key file (OXBOT_KEY_STORE=file)"
            );
            Arc::new(KeyFile::in_data_dir(data_dir))
        }
        Pick::KeychainAppFolder => {
            tracing::warn!(
                "OXBOT_KEY_STORE=file is ignored for the app's own data folder; using the Keychain"
            );
            Arc::new(Keychain::for_data_dir(data_dir))
        }
        Pick::Keychain => Arc::new(Keychain::for_data_dir(data_dir)),
    }
}

/// Which store a non-test run gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Keychain,
    /// A key file was asked for, but this is the app's own folder.
    KeychainAppFolder,
    File,
}

/// `wanted`: `OXBOT_KEY_STORE`. `app_dir`: the app's default data folder.
/// A key file only for a folder that isn't the app's (or when the app's
/// folder is unknown, never).
fn pick(data_dir: &Path, wanted: Option<&str>, app_dir: Option<&Path>) -> Pick {
    if wanted != Some("file") {
        return Pick::Keychain;
    }
    let same = |a: &Path, b: &Path| {
        a == b
            || std::fs::canonicalize(a)
                .ok()
                .zip(std::fs::canonicalize(b).ok())
                .is_some_and(|(a, b)| a == b)
    };
    match app_dir {
        Some(app) if !same(data_dir, app) => Pick::File,
        _ => Pick::KeychainAppFolder,
    }
}

/// A dev or test copy's key: `secrets/openrouter-key` in its data folder,
/// readable by the person only (folder 0700, file 0600), written whole
/// through a temporary file. Bots' work folders are elsewhere.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyFile {
    path: PathBuf,
}

impl KeyFile {
    /// The file for `data_dir`.
    #[must_use]
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("secrets").join("openrouter-key"),
        }
    }

    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Debug for KeyFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeyFile")
    }
}

fn io_failed(what: &str, err: &std::io::Error) -> SecretError {
    // The error names what failed, never the key.
    SecretError(format!(
        "The key file couldn't {what} the OpenRouter key ({}).",
        err.kind()
    ))
}

impl SecretStore for KeyFile {
    fn load(&self) -> Result<Option<ApiKey>, SecretError> {
        match std::fs::read(&self.path) {
            Ok(mut bytes) => {
                while bytes.last().is_some_and(u8::is_ascii_whitespace) {
                    bytes.pop();
                }
                ApiKey::from_bytes(bytes)
                    .map(Some)
                    .map_err(|e| SecretError(format!("The kept OpenRouter key is unusable: {e}.")))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(io_failed("read", &err)),
        }
    }

    fn save(&self, key: &ApiKey) -> Result<(), SecretError> {
        use std::io::Write as _;
        let dir = self
            .path
            .parent()
            .ok_or_else(|| SecretError("The key file has no folder.".to_owned()))?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir).map_err(|e| io_failed("save", &e))?;
        let tmp = dir.join(".openrouter-key.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let written = options.open(&tmp).and_then(|mut file| {
            file.write_all(key.expose_for_storage())?;
            file.sync_all()
        });
        if let Err(err) = written.and_then(|()| std::fs::rename(&tmp, &self.path)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(io_failed("save", &err));
        }
        Ok(())
    }

    fn in_file(&self) -> bool {
        true
    }

    fn delete(&self) -> Result<(), SecretError> {
        match std::fs::remove_file(&self.path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                Err(io_failed("delete", &err))
            }
            _ => Ok(()),
        }
    }
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
    /// How long the first read waits.
    delay: Mutex<std::time::Duration>,
}

impl TestKeyFile {
    /// The store starting from `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            changed: Mutex::new(None),
            delay: Mutex::new(std::time::Duration::ZERO),
        }
    }

    /// The same, whose first read takes `delay`.
    #[must_use]
    pub fn with_delay(self, delay: std::time::Duration) -> Self {
        *self.delay.lock().unwrap_or_else(PoisonError::into_inner) = delay;
        self
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
        let delay = std::mem::take(&mut *self.delay.lock().unwrap_or_else(PoisonError::into_inner));
        if !delay.is_zero() {
            std::thread::sleep(delay);
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
    fn a_key_file_only_when_asked_and_never_for_the_app_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("app");
        let dev = tmp.path().join("dev");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&dev).unwrap();
        assert_eq!(pick(&dev, None, Some(&app)), Pick::Keychain);
        assert_eq!(pick(&dev, Some("keychain"), Some(&app)), Pick::Keychain);
        assert_eq!(pick(&dev, Some("file"), Some(&app)), Pick::File);
        assert_eq!(
            pick(&app, Some("file"), Some(&app)),
            Pick::KeychainAppFolder
        );
        // The same folder spelled differently is still the app's.
        let odd = dev.join("..").join("app");
        assert_eq!(
            pick(&odd, Some("file"), Some(&app)),
            Pick::KeychainAppFolder
        );
        // App folder unknown: never a file.
        assert_eq!(pick(&dev, Some("file"), None), Pick::KeychainAppFolder);
    }

    #[test]
    fn the_key_file_round_trips_privately() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KeyFile::in_data_dir(tmp.path());
        assert!(!format!("{store:?}").contains("secrets"));
        // Settings says where the key is; the Keychain's stores say no file.
        assert!(store.in_file());
        assert!(!Keychain::for_data_dir(tmp.path()).in_file());
        assert!(store.load().unwrap().is_none());
        store.save(&ApiKey::new(FAKE.to_owned()).unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), FAKE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(store.path()), 0o600);
            assert_eq!(mode(store.path().parent().unwrap()), 0o700);
        }
        assert!(store.load().unwrap().is_some());
        store
            .save(&ApiKey::new(format!("{FAKE}-2")).unwrap())
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(store.path()).unwrap(),
            format!("{FAKE}-2")
        );
        assert!(!store.path().with_file_name(".openrouter-key.tmp").exists());
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
        store.delete().unwrap();
        let err = SecretStore::load(&KeyFile {
            path: tmp.path().to_owned(),
        })
        .unwrap_err();
        assert!(!err.0.contains(FAKE));
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
