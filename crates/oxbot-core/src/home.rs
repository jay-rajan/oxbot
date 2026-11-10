//! [`Home`]: where oxbot keeps its files.

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

/// A folder only this Mac account may open.
pub const PRIVATE_DIR: u32 = 0o700;
/// A file only this Mac account may read or write.
pub const PRIVATE_FILE: u32 = 0o600;

/// oxbot's data folder.
///
/// Default: `~/Library/Application Support/oxbot` on macOS. Set `OXBOT_HOME`
/// to use another folder (tests and experiments do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    /// A home at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `OXBOT_HOME` if set, otherwise the platform's data folder + `oxbot`.
    /// `None` only if the platform has no data folder.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if let Some(dir) = std::env::var_os("OXBOT_HOME") {
            return Some(Self::new(dir));
        }
        dirs::data_dir().map(|d| Self::new(d.join("oxbot")))
    }

    /// The folder itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where bots live: one folder per bot.
    #[must_use]
    pub fn bots_dir(&self) -> PathBuf {
        self.root.join("bots")
    }

    /// The SQLite database with every bot's history.
    #[must_use]
    pub fn db_path(&self) -> PathBuf {
        self.root.join("oxbot.db")
    }

    /// Makes the data folder this account's alone: the folder and the
    /// ones holding bots, the device door's certificate and deleted bots
    /// 0700; the database, its journal files and its copies 0600. Made
    /// looser by an older oxbot, they're tightened; nothing is loosened.
    ///
    /// # Errors
    /// Fails if the folder can't be made or changed.
    pub fn make_private(&self) -> io::Result<()> {
        private_dir(&self.root)?;
        for dir in ["bots", "door", "trash"] {
            restrict(&self.root.join(dir), PRIVATE_DIR)?;
        }
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("oxbot.db") {
                restrict(&entry.path(), PRIVATE_FILE)?;
            }
        }
        Ok(())
    }
}

/// Makes `dir` (and any folders above it) and lets only this account in.
///
/// # Errors
/// Fails if it can't be made or changed.
pub fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    restrict(dir, PRIVATE_DIR)
}

/// Takes away from `path` what `mode` doesn't allow (never adds). Nothing
/// there, or a link (never followed), is left alone.
///
/// # Errors
/// Fails if it can't be changed.
pub fn restrict(path: &Path, mode: u32) -> io::Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    let now = meta.permissions().mode() & 0o7777;
    if now & !mode != 0 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(now & mode))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_data_folder_is_made_private() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::new(tmp.path().join("oxbot"));
        private_dir(home.root()).unwrap();
        // As an older oxbot (or a loose umask) left it.
        for dir in ["bots", "door", "trash"] {
            std::fs::create_dir(home.root().join(dir)).unwrap();
            std::fs::set_permissions(
                home.root().join(dir),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        for file in [
            "oxbot.db",
            "oxbot.db-wal",
            "oxbot.db-shm",
            "oxbot.db.v7.bak",
        ] {
            std::fs::write(home.root().join(file), "x").unwrap();
            std::fs::set_permissions(
                home.root().join(file),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        std::fs::set_permissions(home.root(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // A link is never followed.
        let outside = tmp.path().join("outside");
        std::fs::write(&outside, "x").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&outside, home.root().join("oxbot.db.link")).unwrap();

        home.make_private().unwrap();
        assert_eq!(mode(home.root()), 0o700);
        for dir in ["bots", "door", "trash"] {
            assert_eq!(mode(&home.root().join(dir)), 0o700, "{dir}");
        }
        for file in [
            "oxbot.db",
            "oxbot.db-wal",
            "oxbot.db-shm",
            "oxbot.db.v7.bak",
        ] {
            assert_eq!(mode(&home.root().join(file)), 0o600, "{file}");
        }
        assert_eq!(mode(&outside), 0o644);
        // Nothing is loosened.
        std::fs::set_permissions(
            home.root().join("bots"),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        home.make_private().unwrap();
        assert_eq!(mode(&home.root().join("bots")), 0o500);
    }

    #[test]
    fn paths_hang_off_the_root() {
        let home = Home::new("/x");
        assert_eq!(home.bots_dir(), Path::new("/x/bots"));
        assert_eq!(home.db_path(), Path::new("/x/oxbot.db"));
    }
}
