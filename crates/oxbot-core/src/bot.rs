//! [`Bot`]s on disk.
//!
//! Each bot is a folder named after its id:
//!
//! ```text
//! bots/assistant/
//!   bot.toml          name = "Herder", optional model, created_by, private
//!   instructions.md   appended to Claude's system prompt
//!   work/             Claude's working folder: the only place it may touch
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::id::{BotId, BotIdError};

/// The central bot's id. It's Herder: created on first run, kept if
/// missing, and the only bot that can create other bots and give them tasks.
/// (The id stays `assistant`, its name before it was called Herder.)
pub const DEFAULT_BOT: &str = "assistant";
/// The central bot's name.
pub const CENTRAL_NAME: &str = "Herder";
/// Names and instructions the central bot had before it was Herder.
const OLD_NAMES: &[&str] = &["Assistant"];
const OLD_INSTRUCTIONS: &[&str] = &[
    "You are oxbot's assistant, running on the user's Mac.\nKeep replies short: the user may be reading on a phone.\nYou can read and write files in your working folder.",
    "You are oxbot's Assistant, running on the user's Mac. You are the user's\nfirst stop: answer what you can, and run the team of bots for the rest.\nKeep replies short: the user may be reading on a phone.",
];

const DEFAULT_INSTRUCTIONS: &str = "\
You are Herder, oxbot's lead bot on the user's Mac. The user comes to you
first: answer what you can, and guide their herd of bots for the rest.
Keep replies short: the user may be reading on a phone.";

/// Who is asking: a person, and whether they're on the Farm itself.
///
/// The Farm (the Mac that runs the bots) can see and change every bot.
/// Anyone else sees shared bots and their own private ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// Their name, e.g. `Jayadev`.
    pub name: String,
    /// On the Farm itself.
    pub farm: bool,
}

impl Viewer {
    /// Someone on the Farm.
    #[must_use]
    pub fn farm(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            farm: true,
        }
    }

    /// Someone on a Gate (a laptop or phone), or a bot acting for them.
    #[must_use]
    pub fn gate(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            farm: false,
        }
    }

    /// Whether they may see a bot made by `created_by` that is `private`.
    #[must_use]
    pub fn can_see(&self, created_by: Option<&str>, private: bool) -> bool {
        self.farm || !private || self.made(created_by)
    }

    /// Whether they may rename, delete, or change who sees a bot made by
    /// `created_by`.
    #[must_use]
    pub fn can_manage(&self, created_by: Option<&str>) -> bool {
        self.farm || self.made(created_by)
    }

    /// The bot list as they get it: every bot is listed (name, maker,
    /// lock), but a private bot they can't open shows nothing of what it's
    /// doing (no activity, no message counts).
    #[must_use]
    pub fn redact(&self, mut bots: Vec<oxbot_protocol::BotInfo>) -> Vec<oxbot_protocol::BotInfo> {
        for b in &mut bots {
            if !self.can_see(b.created_by.as_deref(), b.private) {
                b.busy = false;
                b.last_seq = 0;
                b.last_reply_seq = 0;
                b.browser_state = oxbot_protocol::BrowserState::Off;
            }
        }
        bots
    }

    fn made(&self, created_by: Option<&str>) -> bool {
        !self.name.is_empty() && created_by == Some(self.name.as_str())
    }
}

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
    /// Who created it (a person's name), if known.
    pub created_by: Option<String>,
    /// Only its creator (and the Farm) can see and use it, and it uses
    /// only its creator's own browser.
    pub private: bool,
    /// The bot's folder.
    pub dir: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct BotFile {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_by: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    private: bool,
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
    /// Whether this is the central bot, Herder.
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
            created_by: file.created_by,
            private: file.private,
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
        Self::create_by(bots_dir, id, name, instructions, None)
    }

    /// Like [`Bot::create`], recording who created it.
    ///
    /// # Errors
    /// Fails if the bot exists or its files can't be written.
    pub fn create_by(
        bots_dir: &Path,
        id: BotId,
        name: &str,
        instructions: &str,
        created_by: Option<&str>,
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
            created_by: created_by.map(str::to_owned),
            private: false,
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

    /// Whether `viewer` may see and use it.
    #[must_use]
    pub fn visible_to(&self, viewer: &Viewer) -> bool {
        viewer.can_see(self.created_by.as_deref(), self.private)
    }

    /// Whose browser it uses: its creator's own if it's private, else the
    /// home browser ([`crate::store::HOME`]). `None` for a private bot with
    /// no creator: it gets no browser rather than the home one.
    #[must_use]
    pub fn browser_owner(&self) -> Option<String> {
        if self.private {
            self.created_by.clone().filter(|c| !c.trim().is_empty())
        } else {
            Some(crate::store::HOME.to_owned())
        }
    }

    /// Writes its name, model, creator and privacy back to `bot.toml`.
    ///
    /// # Errors
    /// Fails if the file can't be written.
    pub fn save(&self) -> Result<(), BotError> {
        write_file(
            &self.dir,
            &BotFile {
                name: self.name.clone(),
                model: self.model.clone(),
                created_by: self.created_by.clone(),
                private: self.private,
            },
        )
    }

    /// Moves the bot's folder into `trash` (as `<id>-<when>`), so a deleted
    /// bot's files can still be found by hand. Returns where it went.
    ///
    /// # Errors
    /// Fails if the folder can't be moved.
    pub fn move_to(&self, trash: &Path, when_ms: i64) -> Result<PathBuf, BotError> {
        fs::create_dir_all(trash).map_err(io_err(trash))?;
        let to = trash.join(format!("{}-{when_ms}", self.id));
        fs::rename(&self.dir, &to).map_err(io_err(&self.dir))?;
        Ok(to)
    }
}

/// Writes `bot.toml` all at once: a crash mid-write leaves the old file, never
/// a cut-off one (which could drop `private` and turn the bot shared).
fn write_file(dir: &Path, file: &BotFile) -> Result<(), BotError> {
    let toml_path = dir.join("bot.toml");
    let text = toml::to_string(file)
        .map_err(|e| io_err(&toml_path)(io::Error::new(io::ErrorKind::InvalidData, e)))?;
    let tmp = dir.join("bot.toml.tmp");
    let write = || -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        io::Write::write_all(&mut f, text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, &toml_path)
    };
    write().map_err(io_err(&toml_path))
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
        Bot::create(bots_dir, id, CENTRAL_NAME, DEFAULT_INSTRUCTIONS)?;
    }
    rename_old_central(bots_dir)
}

/// A central bot still called "Assistant" (with its old default
/// instructions) becomes Herder, once: a marker file in its folder means it
/// was renamed already, so a name the user put back by hand stays.
/// Instructions the user wrote are kept.
fn rename_old_central(bots_dir: &Path) -> Result<(), BotError> {
    let Some(mut bot) = load_all(bots_dir)?.into_iter().find(Bot::is_central) else {
        return Ok(());
    };
    let marker = bot.dir.join(".renamed-to-herder");
    if marker.exists() || !OLD_NAMES.contains(&bot.name.as_str()) {
        return Ok(());
    }
    CENTRAL_NAME.clone_into(&mut bot.name);
    bot.save()?;
    if OLD_INSTRUCTIONS.contains(&bot.instructions.trim()) {
        let path = bot.dir.join("instructions.md");
        fs::write(&path, DEFAULT_INSTRUCTIONS).map_err(io_err(&path))?;
    }
    fs::write(&marker, "").map_err(io_err(&marker))?;
    tracing::info!("the Assistant is now called {CENTRAL_NAME}");
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
        assert_eq!(bots[0].name, CENTRAL_NAME);
    }

    #[test]
    fn the_old_assistant_becomes_herder_and_keeps_its_own_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let id = BotId::new(DEFAULT_BOT).unwrap();
        let old = Bot::create(tmp.path(), id, "Assistant", OLD_INSTRUCTIONS[1]).unwrap();
        fs::write(
            old.dir.join("bot.toml"),
            "name = \"Assistant\"\nmodel = \"haiku\"\n",
        )
        .unwrap();
        ensure_default(tmp.path()).unwrap();
        let bot = Bot::load(&old.dir).unwrap();
        assert_eq!(bot.name, "Herder");
        assert_eq!(bot.model.as_deref(), Some("haiku"), "the model is kept");
        assert!(bot.instructions.contains("You are Herder"));

        // It's renamed once: a name put back by hand stays.
        fs::write(old.dir.join("bot.toml"), "name = \"Assistant\"\n").unwrap();
        ensure_default(tmp.path()).unwrap();
        assert_eq!(Bot::load(&old.dir).unwrap().name, "Assistant");

        // Instructions the user wrote stay (an install never renamed yet).
        fs::remove_file(old.dir.join(".renamed-to-herder")).unwrap();
        fs::write(old.dir.join("instructions.md"), "Mine.").unwrap();
        ensure_default(tmp.path()).unwrap();
        let bot = Bot::load(&old.dir).unwrap();
        assert_eq!(
            (bot.name.as_str(), bot.instructions.as_str()),
            ("Herder", "Mine.")
        );
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
    fn the_farm_sees_everything_and_others_see_shared_bots_and_their_own() {
        let farm = Viewer::farm("Jay");
        let sam = Viewer::gate("Sam");
        let ann = Viewer::gate("Ann");
        assert!(farm.can_see(Some("Sam"), true));
        assert!(sam.can_see(Some("Sam"), true));
        assert!(!ann.can_see(Some("Sam"), true));
        assert!(ann.can_see(Some("Sam"), false));
        assert!(!ann.can_see(None, true));
        assert!(
            !Viewer::gate("").can_see(None, true),
            "no name owns nothing"
        );
        assert!(farm.can_manage(None));
        assert!(sam.can_manage(Some("Sam")));
        assert!(!ann.can_manage(Some("Sam")));
    }

    #[test]
    fn privacy_and_names_are_saved_and_deleted_bots_go_to_the_trash() {
        let tmp = tempfile::tempdir().unwrap();
        let bots = tmp.path().join("bots");
        let id = BotId::new("bank").unwrap();
        let mut bot = Bot::create_by(&bots, id, "Bank", "", Some("Sam")).unwrap();
        assert!(!bot.private);
        assert!(
            !fs::read_to_string(bot.dir.join("bot.toml"))
                .unwrap()
                .contains("private")
        );
        bot.private = true;
        bot.name = "Bank Check".to_owned();
        bot.save().unwrap();
        let again = Bot::load(&bot.dir).unwrap();
        assert!(again.private);
        assert_eq!(again.name, "Bank Check");
        assert_eq!(again.created_by.as_deref(), Some("Sam"));

        let moved = again.move_to(&tmp.path().join("trash"), 42).unwrap();
        assert!(moved.ends_with("bank-42"));
        assert!(moved.join("bot.toml").is_file());
        assert_eq!(load_all(&bots).unwrap().len(), 0);
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
