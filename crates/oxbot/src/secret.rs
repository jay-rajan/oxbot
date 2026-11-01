//! Secrets the Farm keeps: random keys, files only this Mac account can
//! read, and the Farm secret.
//!
//! The Farm secret decides who counts as the Farm. It's made once and kept
//! in the data folder (`farm-key`, readable only by this account). The oxbot
//! app opens its window with it, `oxbot chat` and `oxbot bots` read it, and
//! `oxbot serve` prints a link that carries it. A connection from this Mac
//! without it is refused, so a web page, a bot's browser or another program
//! that reaches `127.0.0.1` can't act as the Farm.

use std::io::Write as _;
use std::path::Path;

/// The Farm secret's file in the data folder.
pub const FARM_KEY_FILE: &str = "farm-key";

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
/// mid-write leaves the old file, never half a new one.
///
/// # Errors
/// Fails if the file can't be written.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// The Farm secret in `data_dir`, made the first time.
///
/// # Errors
/// Fails if it can't be read or saved.
pub fn farm_key(data_dir: &Path) -> std::io::Result<String> {
    if let Some(key) = read_farm_key(data_dir) {
        return Ok(key);
    }
    std::fs::create_dir_all(data_dir)?;
    let key = random_hex::<32>();
    write_private(&data_dir.join(FARM_KEY_FILE), key.as_bytes())?;
    Ok(key)
}

/// The Farm secret in `data_dir`, if there is a usable one.
#[must_use]
pub fn read_farm_key(data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(data_dir.join(FARM_KEY_FILE))
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| usable(k))
}

/// The Farm secret of the oxbot data folder in use (`OXBOT_HOME` or the
/// default), if it has one yet.
#[must_use]
pub fn local_farm_key() -> Option<String> {
    oxbot_core::Home::from_env().and_then(|home| read_farm_key(home.root()))
}

/// The WebSocket address of the oxbot on this Mac at `port`, with the Farm
/// secret from the data folder (for `oxbot chat` and `oxbot bots`).
#[must_use]
pub fn local_ws_url(port: u16) -> String {
    match local_farm_key() {
        Some(key) => format!("ws://127.0.0.1:{port}/ws?k={key}"),
        None => format!("ws://127.0.0.1:{port}/ws"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_farm_key_is_made_once_and_kept_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let key = farm_key(tmp.path()).unwrap();
        assert!(usable(&key));
        assert_eq!(farm_key(tmp.path()).unwrap(), key);
        let mode = std::fs::metadata(tmp.path().join(FARM_KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
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
