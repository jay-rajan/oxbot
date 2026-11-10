//! Secrets the home computer keeps: random keys, files only this Mac account can
//! read, and the home computer secret.
//!
//! The home computer secret decides who counts as the home computer. It's made once and kept
//! in the data folder (`home-computer-key`, readable only by this account). The oxbot
//! app opens its window with it, `oxbot chat` and `oxbot bots` read it, and
//! `oxbot serve` prints a link that carries it. A connection from this Mac
//! without it is refused, so a web page, a bot's browser or another program
//! that reaches `127.0.0.1` can't act as the home computer.
//!
//! Before oxbot's own programs send the secret to a server on this Mac,
//! they ask it to prove it holds the same secret ([`HELLO_PATH`],
//! [`hello_proof`]): anything else listening on oxbot's port gets nothing.

use std::io::Write as _;
use std::path::Path;

/// The home computer secret's file in the data folder.
pub const HOME_COMPUTER_KEY_FILE: &str = "home-computer-key";
/// Its name before the rename: moved to [`HOME_COMPUTER_KEY_FILE`] once.
pub const OLD_HOME_COMPUTER_KEY_FILE: &str = "farm-key";

/// `N` random bytes as lowercase hex.
#[must_use]
pub fn random_hex<const N: usize>() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; N];
    // The system's random source doesn't fail on any OS oxbot runs on; if it
    // ever did, the all-zero key is refused (see `usable`).
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    hex(&bytes)
}

/// Bytes as lowercase hex.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Whether a key is long enough and not the all-zero fallback.
#[must_use]
pub fn usable(key: &str) -> bool {
    key.len() >= 32 && key.bytes().any(|b| b != b'0')
}

/// Compares without leaking where the first difference is.
#[must_use]
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Writes a file that only this account can read, all at once: a crash
/// mid-write leaves the old file, never half a new one. The file it
/// writes first is always made new (one left by a crash, or a link put
/// there, is removed, never written through: a link put back in between
/// makes it fail).
///
/// # Errors
/// Fails if the file can't be written.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("tmp");
    match std::fs::remove_file(&tmp) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Where a server on this Mac answers [`hello_proof`] for a nonce:
/// `GET /hello?n=<nonce>`. Needs no key.
pub const HELLO_PATH: &str = "/hello";

/// Whether `nonce` is one a hello may carry: 16 to 128 letters and digits.
#[must_use]
pub fn hello_nonce_ok(nonce: &str) -> bool {
    (16..=128).contains(&nonce.len()) && nonce.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The answer to a hello: HMAC-SHA256 of `oxbot-hello:<nonce>` with the
/// home computer secret `key`, as lowercase hex. It shows the server holds
/// the key without showing the key.
#[must_use]
pub fn hello_proof(key: &str, nonce: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key.as_bytes());
    let tag = ring::hmac::sign(&key, format!("oxbot-hello:{nonce}").as_bytes());
    hex(tag.as_ref())
}

/// Whether `answer` is [`hello_proof`] of `nonce` with `key`, compared in
/// constant time.
#[must_use]
pub fn hello_proof_ok(key: &str, nonce: &str, answer: &str) -> bool {
    let Some(tag) = unhex(answer.trim()) else {
        return false;
    };
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key.as_bytes());
    ring::hmac::verify(&key, format!("oxbot-hello:{nonce}").as_bytes(), &tag).is_ok()
}

/// Lowercase or uppercase hex as bytes; `None` if it isn't hex.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The home computer secret in `data_dir`, made the first time.
///
/// # Errors
/// Fails if it can't be read or saved.
pub fn home_computer_key(data_dir: &Path) -> std::io::Result<String> {
    if let Some(key) = read_home_computer_key(data_dir) {
        return Ok(key);
    }
    std::fs::create_dir_all(data_dir)?;
    let key = random_hex::<32>();
    write_private(&data_dir.join(HOME_COMPUTER_KEY_FILE), key.as_bytes())?;
    Ok(key)
}

/// The home computer secret in `data_dir`, if there is a usable one.
#[must_use]
pub fn read_home_computer_key(data_dir: &Path) -> Option<String> {
    let path = data_dir.join(HOME_COMPUTER_KEY_FILE);
    if let Err(err) = oxbot_device::move_old_name(&data_dir.join(OLD_HOME_COMPUTER_KEY_FILE), &path)
    {
        tracing::warn!(%err, "couldn't move the home computer secret to its new name");
    }
    std::fs::read_to_string(path)
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| usable(k))
}

/// The home computer secret of the oxbot data folder in use (`OXBOT_HOME` or the
/// default), if it has one yet.
#[must_use]
pub fn local_home_computer_key() -> Option<String> {
    oxbot_core::Home::from_env().and_then(|home| read_home_computer_key(home.root()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_home_computer_key_is_made_once_and_kept_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let key = home_computer_key(tmp.path()).unwrap();
        assert!(usable(&key));
        assert_eq!(home_computer_key(tmp.path()).unwrap(), key);
        let mode = std::fs::metadata(tmp.path().join(HOME_COMPUTER_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_secret_saved_before_the_rename_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let old = "ab".repeat(32);
        std::fs::write(tmp.path().join(OLD_HOME_COMPUTER_KEY_FILE), &old).unwrap();
        assert_eq!(home_computer_key(tmp.path()).unwrap(), old);
        assert!(tmp.path().join(HOME_COMPUTER_KEY_FILE).exists());
        assert!(!tmp.path().join(OLD_HOME_COMPUTER_KEY_FILE).exists());
    }

    #[test]
    fn a_private_file_is_never_written_through_a_link() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::write(&elsewhere, "theirs").unwrap();
        let path = tmp.path().join(HOME_COMPUTER_KEY_FILE);
        // A link where the file is first written: removed, not followed.
        std::os::unix::fs::symlink(&elsewhere, path.with_extension("tmp")).unwrap();
        write_private(&path, b"ours").unwrap();
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "theirs");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ours");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // One left over by a crash: written afresh.
        std::fs::write(path.with_extension("tmp"), "half").unwrap();
        write_private(&path, b"again").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "again");
    }

    #[test]
    fn a_hello_proves_the_key_without_showing_it() {
        let key = random_hex::<32>();
        let nonce = random_hex::<16>();
        let proof = hello_proof(&key, &nonce);
        assert_eq!(proof.len(), 64);
        assert!(!proof.contains(&key));
        assert!(hello_proof_ok(&key, &nonce, &proof));
        assert!(
            !hello_proof_ok(&key, &random_hex::<16>(), &proof),
            "another nonce"
        );
        assert!(
            !hello_proof_ok(&random_hex::<32>(), &nonce, &proof),
            "another key"
        );
        assert!(!hello_proof_ok(&key, &nonce, "zz"));
        assert!(!hello_proof_ok(&key, &nonce, ""));
        assert!(hello_nonce_ok(&nonce));
        assert!(!hello_nonce_ok("short"));
        assert!(!hello_nonce_ok(&"a/".repeat(10)));
    }

    #[test]
    fn keys_compare_whole() {
        assert!(same("abcd", "abcd"));
        assert!(!same("abcd", "abce"));
        assert!(!same("abc", "abcd"));
        assert!(!usable(&"0".repeat(64)));
        assert!(!usable("abc"));
    }
}
