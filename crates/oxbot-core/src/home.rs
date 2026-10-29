//! [`Home`]: where oxbot keeps its files.

use std::path::{Path, PathBuf};

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_hang_off_the_root() {
        let home = Home::new("/x");
        assert_eq!(home.bots_dir(), Path::new("/x/bots"));
        assert_eq!(home.db_path(), Path::new("/x/oxbot.db"));
    }
}
