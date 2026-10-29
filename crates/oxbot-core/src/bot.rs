//! [`Bot`]s on disk.
//!
//! Each bot is a folder named after its id:
//!
//! ```text
//! bots/assistant/
//!   bot.toml          name = "Assistant", optional model = "sonnet"
//!   instructions.md   appended to Claude's system prompt
//!   work/             Claude's working folder: the only place it may touch
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::id::{BotId, BotIdError};

/// The central bot, the Assistant: created on first run, kept if missing,
/// and the only bot that can create other bots and give them tasks.
pub const DEFAULT_BOT: &str = "assistant";

const DEFAULT_INSTRUCTIONS: &str = "\
You are oxbot's Assistant, running on the user's Mac. You are the user's
first stop: answer what you can, and run the team of bots for the rest.
Keep replies short: the user may be reading on a phone.";

/// A bot: who it is and how it should behave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    /// Its id, which is also its folder name.
    pub id: BotId,
    /// Display name.
    pub name: String,
    /// Model alias for Claude (e.g. `sonnet`); `None` uses the default.
    pub model: Option<String>,
    /// Extra instructions for Claude.
    pub instructions: String,
    /// The bot's folder.
    pub dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct BotFile {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

/// Errors reading or writing bots.
#[derive(Debug, thiserror::Error)]
pub enum BotError {
    /// A file or folder couldn't be read or written.
    #[error("{path}: {source}")]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// `bot.toml` isn't valid.
    #[error("{path}: {source}")]
    Toml {
        /// The file.
        path: PathBuf,
        /// What's wrong with it.
        source: toml::de::Error,
    },
    /// A folder name isn't a valid bot id.
    #[error("{path}: {source}")]
    BadId {
        /// The folder.
        path: PathBuf,
        /// Why the name isn't valid.
        source: BotIdError,
    },
    /// A bot with that id already exists.
    #[error("bot {0} already exists")]
    Exists(BotId),
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> BotError + '_ {
    move |source| BotError::Io {
        path: path.to_owned(),
        source,
    }
}

impl Bot {
    /// Whether this is the central bot, the Assistant.
    #[must_use]
    pub fn is_central(&self) -> bool {
        self.id.as_str() == DEFAULT_BOT
    }

    /// Reads the bot in `dir`.
    ///
    /// # Errors
    /// Fails if the folder name isn't a valid id or `bot.toml` can't be read.
    pub fn load(dir: &Path) -> Result<Self, BotError> {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = BotId::new(name).map_err(|source| BotError::BadId {
            path: dir.to_owned(),
            source,
        })?;
        let toml_path = dir.join("bot.toml");
        let text = fs::read_to_string(&toml_path).map_err(io_err(&toml_path))?;
        let file: BotFile = toml::from_str(&text).map_err(|source| BotError::Toml {
            path: toml_path.clone(),
            source,
        })?;
        // A missing instructions file just means no extra instructions.
        let instructions = fs::read_to_string(dir.join("instructions.md")).unwrap_or_default();
        Ok(Self {
            id,
            name: file.name,
            model: file.model,
            instructions,
            dir: dir.to_owned(),
        })
    }

    /// Creates a new bot folder under `bots_dir`.
    ///
    /// # Errors
    /// Fails if the bot exists or its files can't be written.
    pub fn create(
        bots_dir: &Path,
        id: BotId,
        name: &str,
        instructions: &str,
    ) -> Result<Self, BotError> {
        let dir = bots_dir.join(id.as_str());
        if dir.exists() {
            return Err(BotError::Exists(id));
        }
        let work = dir.join("work");
        fs::create_dir_all(&work).map_err(io_err(&work))?;
        let file = BotFile {
            name: name.to_owned(),
            model: None,
        };
        write_file(&dir, &file)?;
        let md = dir.join("instructions.md");
        fs::write(&md, instructions).map_err(io_err(&md))?;
        Self::load(&dir)
    }

    /// Claude's working folder for this bot.
    #[must_use]
    pub fn work_dir(&self) -> PathBuf {
        self.dir.join("work")
    }
}

fn write_file(dir: &Path, file: &BotFile) -> Result<(), BotError> {
    let toml_path = dir.join("bot.toml");
    // Plain strings and lists always serialize.
    let text = toml::to_string(file).unwrap_or_default();
    fs::write(&toml_path, text).map_err(io_err(&toml_path))
}

/// Reads every bot under `bots_dir`, sorted by id. Folders that aren't valid
/// bots are skipped with a warning, so one broken bot can't stop the rest.
///
/// # Errors
/// Fails only if `bots_dir` itself can't be read.
pub fn load_all(bots_dir: &Path) -> Result<Vec<Bot>, BotError> {
    let mut bots = Vec::new();
    for entry in fs::read_dir(bots_dir).map_err(io_err(bots_dir))? {
        let path = entry.map_err(io_err(bots_dir))?.path();
        if !path.is_dir() {
            continue;
        }
        match Bot::load(&path) {
            Ok(bot) => bots.push(bot),
            Err(err) => tracing::warn!(%err, "skipping bot"),
        }
    }
    bots.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(bots)
}

/// Creates `bots_dir` and the default `assistant` bot if there are no bots yet.
///
/// # Errors
/// Fails if the folders or files can't be written.
pub fn ensure_default(bots_dir: &Path) -> Result<(), BotError> {
    fs::create_dir_all(bots_dir).map_err(io_err(bots_dir))?;
    if !load_all(bots_dir)?.iter().any(Bot::is_central) {
        let id = BotId::new(DEFAULT_BOT).map_err(|source| BotError::BadId {
            path: bots_dir.join(DEFAULT_BOT),
            source,
        })?;
        Bot::create(bots_dir, id, "Assistant", DEFAULT_INSTRUCTIONS)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bot_is_created_once() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_default(tmp.path()).unwrap();
        ensure_default(tmp.path()).unwrap();
        let bots = load_all(tmp.path()).unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].id.as_str(), "assistant");
        assert!(bots[0].instructions.contains("phone"));
        assert!(bots[0].work_dir().is_dir());
    }

    #[test]
    fn create_refuses_duplicates_and_load_skips_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let id = BotId::new("digest").unwrap();
        Bot::create(tmp.path(), id.clone(), "Digest", "").unwrap();
        assert!(matches!(
            Bot::create(tmp.path(), id, "Digest", ""),
            Err(BotError::Exists(_))
        ));
        fs::create_dir(tmp.path().join("Not A Bot")).unwrap();
        let bots = load_all(tmp.path()).unwrap();
        assert_eq!(bots.len(), 1);
        assert_eq!(bots[0].name, "Digest");
    }

    #[test]
    fn old_files_with_browser_settings_still_load() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old");
        fs::create_dir_all(&old).unwrap();
        fs::write(
            old.join("bot.toml"),
            "name = \"Old\"\nbrowser = \"accounts\"\nsites = [\"mail.google.com\"]\n",
        )
        .unwrap();
        assert_eq!(Bot::load(&old).unwrap().name, "Old");
    }
}
