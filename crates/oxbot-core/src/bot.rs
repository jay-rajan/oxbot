//! [`Bot`]s on disk.
//!
//! Each bot is a folder named after its id:
//!
//! ```text
//! bots/assistant/
//!   bot.toml          name = "Herder", optional engine, model, owner,
//!                     created_by, private, [openrouter] settings
//!   instructions.md   appended to Claude's system prompt
//!   work/             Claude's working folder: the only place it may touch
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::EngineKind;
use crate::id::{BotId, BotIdError, BrowserOwner, PersonId};

/// The central bot's id. It's Herder: created on first run, kept if
/// missing, and the only bot that can create other bots and give them tasks.
/// (The id stays `assistant`, its name before it was called Herder.)
pub const DEFAULT_BOT: &str = "assistant";
/// The central bot's name.
pub const CENTRAL_NAME: &str = "Herder";
/// How a person's own Herder's id starts: `assistant--sam-1a2b3c`. Ids made
/// from names never have two dashes in a row, so no other bot gets one.
pub const PERSONAL_HERDER_PREFIX: &str = "assistant--";

/// Whether `id` is a person's own Herder (not the Farm's, `assistant`).
#[must_use]
pub fn is_personal_herder(id: &str) -> bool {
    id.starts_with(PERSONAL_HERDER_PREFIX)
}

/// The id of `person`'s own Herder: their name made safe, plus a short hash
/// of it (ignoring case), so different names never share one.
#[must_use]
pub fn personal_herder_id(person: &str) -> BotId {
    let key = person.trim().to_lowercase();
    let slug: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(20)
        .collect();
    // FNV-1a: stable across versions and machines, unlike the std hasher.
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    let id = if slug.is_empty() {
        format!("{PERSONAL_HERDER_PREFIX}{:06x}", hash & 0xff_ffff)
    } else {
        format!("{PERSONAL_HERDER_PREFIX}{slug}-{:06x}", hash & 0xff_ffff)
    };
    BotId::new(id).unwrap_or_else(|_| unreachable!("only a-z, 0-9 and dashes, at most 38 long"))
}

/// A Herder id for `person` made from their id, for when the one made from
/// their name is someone else's (`Sam` and `sam` share it). It ends in 8
/// hex digits where [`personal_herder_id`] has 6, so the two never meet.
#[must_use]
pub fn personal_herder_id_for(person: &str, id: &PersonId) -> BotId {
    let slug: String = person
        .trim()
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(20)
        .collect();
    let hex = id.as_str().trim_start_matches("p_");
    let hex = hex.get(..8).unwrap_or(hex);
    let id = if slug.is_empty() {
        format!("{PERSONAL_HERDER_PREFIX}{hex}")
    } else {
        format!("{PERSONAL_HERDER_PREFIX}{slug}-{hex}")
    };
    BotId::new(id).unwrap_or_else(|_| unreachable!("only a-z, 0-9 and dashes, at most 40 long"))
}
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
/// Anyone else sees shared bots and their own private ones. These are the
/// only rules for that: the page and Herder get their answers from here.
///
/// A viewer tied to a person (every viewer the server and hub make) is
/// matched with bots by person id; names are only for showing. One with
/// only a name (tests, or before the hub looks the name up) is matched by
/// name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    /// Their person id, once known.
    pub id: Option<PersonId>,
    /// Their name, e.g. `Jayadev`.
    pub name: String,
    /// On the Farm itself. Only the Farm secret makes a connection this,
    /// never a name.
    pub farm: bool,
}

impl Viewer {
    /// Someone on the Farm.
    #[must_use]
    pub fn farm(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            farm: true,
        }
    }

    /// Someone on a Gate (a laptop or phone), or a bot acting for them.
    #[must_use]
    pub fn gate(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            farm: false,
        }
    }

    /// The person `id` called `name`, on the Farm (`farm`) or not.
    #[must_use]
    pub fn person(id: PersonId, name: impl Into<String>, farm: bool) -> Self {
        Self {
            id: Some(id),
            name: name.into(),
            farm,
        }
    }

    /// Whether they may see a bot made by `made_by` that is `private`.
    /// `made_by` is the maker's id for a viewer with an id, else their name
    /// (see [`Viewer::maker`]).
    #[must_use]
    pub fn can_see(&self, made_by: Option<&str>, private: bool) -> bool {
        self.farm || !private || self.made(made_by)
    }

    /// Whether they may rename, delete, or change who sees a bot made by
    /// `made_by` (as for [`Viewer::can_see`]).
    #[must_use]
    pub fn can_manage(&self, made_by: Option<&str>) -> bool {
        self.farm || self.made(made_by)
    }

    /// Who made a bot, as this viewer is matched against it: its owner's
    /// id if the viewer has an id, else the maker's name.
    #[must_use]
    pub fn maker<'a>(
        &self,
        owner: Option<&'a str>,
        created_by: Option<&'a str>,
    ) -> Option<&'a str> {
        if self.id.is_some() { owner } else { created_by }
    }

    /// Whether they may open the listed bot `b`.
    #[must_use]
    pub fn can_see_info(&self, b: &oxbot_protocol::BotInfo) -> bool {
        self.can_see(
            self.maker(b.owner.as_deref(), b.created_by.as_deref()),
            b.private,
        )
    }

    /// The bot list as they get it: every bot is listed (name, maker,
    /// lock), but a private bot they can't open shows nothing of what it's
    /// doing (no activity, no message counts). Each says whether they may
    /// manage it and whether it's locked to them, so clients don't decide.
    #[must_use]
    pub fn redact(&self, mut bots: Vec<oxbot_protocol::BotInfo>) -> Vec<oxbot_protocol::BotInfo> {
        for b in &mut bots {
            let maker = self.maker(b.owner.as_deref(), b.created_by.as_deref());
            b.locked = !self.can_see(maker, b.private);
            b.can_manage = !b.central && self.can_manage(maker);
            // Claude bots take tasks only from the Farm's owner.
            b.can_task = !b.locked
                && b.task_owner.as_deref().is_none_or(|owner| {
                    self.farm
                        || (!owner.is_empty()
                            && self.id.as_ref().is_some_and(|id| id.as_str() == owner))
                });
            if b.locked {
                b.busy = false;
                b.last_seq = 0;
                b.last_reply_seq = 0;
                b.browser_state = oxbot_protocol::BrowserState::Off;
            }
        }
        bots
    }

    /// Whether they're the person a message or card is for: `id` (a person
    /// id) decides when both sides have one. Only when neither has an id
    /// (a viewer the hub hasn't looked up, as in tests) is `name` compared.
    /// One with an id is never matched by name alone, nor one without by an
    /// id.
    #[must_use]
    pub fn is_person(&self, id: Option<&str>, name: Option<&str>) -> bool {
        match (id, &self.id) {
            (Some(id), Some(mine)) => id == mine.as_str(),
            (None, None) => name.is_some_and(|n| !n.is_empty() && n == self.name),
            _ => false,
        }
    }

    fn made(&self, made_by: Option<&str>) -> bool {
        match &self.id {
            Some(id) => made_by == Some(id.as_str()),
            None => !self.name.is_empty() && made_by == Some(self.name.as_str()),
        }
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
    /// Who it belongs to: the person who made it (or the Farm's own person,
    /// for bots from before oxbot kept track). Access is decided by this.
    pub owner: Option<PersonId>,
    /// Its maker's name when it was made, for showing.
    pub created_by: Option<String>,
    /// Only its owner (and the Farm) can see and use it, and it uses
    /// only its owner's own browser.
    pub private: bool,
    /// The engine that runs it, as its file says (Herders' engines are
    /// fixed by the hub). Bots made before engines run on Claude.
    pub engine: EngineKind,
    /// Its settings for oxbot's own loop (the `[openrouter]` table).
    pub loop_settings: LoopSettings,
    /// The bot's folder.
    pub dir: PathBuf,
}

/// A bot's settings for oxbot's own loop. Every one is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopSettings {
    /// `auto`, or a model id the owner pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Its own cap on model requests a day; none: the household's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_requests: Option<u32>,
}

impl LoopSettings {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BotFile {
    name: String,
    /// Absent: `claude` (every bot made before engines), so old files keep
    /// their exact bytes when saved again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    /// The owner's person id (`p_…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_by: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    private: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    openrouter: Option<LoopSettings>,
}

impl BotFile {
    /// The engine it names. One this oxbot doesn't know runs on Claude: the
    /// strictest (only the owner may give it tasks), never open to anyone.
    fn engine(&self, id: &BotId) -> EngineKind {
        match self.engine.as_deref() {
            None => EngineKind::Claude,
            Some(name) => EngineKind::parse(name).unwrap_or_else(|| {
                tracing::warn!(bot = %id, engine = name, "unknown engine in bot.toml; using claude");
                EngineKind::Claude
            }),
        }
    }
}

/// How `engine` is written to `bot.toml`: not at all for Claude.
fn engine_entry(engine: EngineKind) -> Option<String> {
    (engine != EngineKind::Claude).then(|| engine.as_str().to_owned())
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
    /// Whether this is Herder: the Farm's (`assistant`) or a person's own.
    #[must_use]
    pub fn is_central(&self) -> bool {
        self.is_main_herder() || is_personal_herder(self.id.as_str())
    }

    /// Whether this is the Farm's Herder, whose instructions every person's
    /// Herder shares.
    #[must_use]
    pub fn is_main_herder(&self) -> bool {
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
        // That shape is a practice run's key: never a bot on disk.
        if id.as_str().starts_with("t--") {
            return Err(BotError::BadId {
                path: dir.to_owned(),
                source: BotIdError::Reserved,
            });
        }
        let toml_path = dir.join("bot.toml");
        let text = fs::read_to_string(&toml_path).map_err(io_err(&toml_path))?;
        let file: BotFile = toml::from_str(&text).map_err(|source| BotError::Toml {
            path: toml_path.clone(),
            source,
        })?;
        // A missing instructions file just means no extra instructions.
        let instructions = fs::read_to_string(dir.join("instructions.md")).unwrap_or_default();
        let engine = file.engine(&id);
        let owner = file.owner.and_then(|o| match PersonId::new(o) {
            Ok(id) => Some(id),
            Err(err) => {
                // No owner rather than a wrong one: private bots then lock.
                tracing::warn!(bot = %id, %err, "ignoring a bad owner in bot.toml");
                None
            }
        });
        Ok(Self {
            id,
            name: file.name,
            model: file.model,
            owner,
            created_by: file.created_by,
            private: file.private,
            engine,
            loop_settings: file.openrouter.unwrap_or_default(),
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

    /// Like [`Bot::create`], recording who created it (by name; the hub
    /// gives it an owner when it loads it).
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
        Self::create_owned(bots_dir, id, name, instructions, None, created_by)
    }

    /// Like [`Bot::create`], owned by `owner` (whose name is `created_by`).
    ///
    /// # Errors
    /// Fails if the bot exists or its files can't be written.
    pub fn create_owned(
        bots_dir: &Path,
        id: BotId,
        name: &str,
        instructions: &str,
        owner: Option<&PersonId>,
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
            engine: None,
            model: None,
            owner: owner.map(ToString::to_string),
            created_by: created_by.map(str::to_owned),
            private: false,
            openrouter: None,
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

    /// Who made it, as `viewer` is matched against it (see [`Viewer::maker`]).
    fn maker_for(&self, viewer: &Viewer) -> Option<&str> {
        viewer.maker(
            self.owner.as_ref().map(PersonId::as_str),
            self.created_by.as_deref(),
        )
    }

    /// Whether `viewer` may see and use it. A personal Herder is only its
    /// owner's (and the Farm's), even if an old file says it's shared. The
    /// Farm's Herder needs the hub to know who the Farm's person is: see
    /// `Hub::bot_for`.
    #[must_use]
    pub fn visible_to(&self, viewer: &Viewer) -> bool {
        let private = self.private || is_personal_herder(self.id.as_str());
        viewer.can_see(self.maker_for(viewer), private)
    }

    /// Whether `viewer` may rename it, delete it or change who sees it.
    #[must_use]
    pub fn manageable_by(&self, viewer: &Viewer) -> bool {
        viewer.can_manage(self.maker_for(viewer))
    }

    /// Whose browser it uses: its owner's own if it's private, else the
    /// home browser. `None` for a private bot with no owner: it gets no
    /// browser rather than the home one.
    #[must_use]
    pub fn browser_owner(&self) -> Option<BrowserOwner> {
        if self.private {
            self.owner.clone().map(BrowserOwner::Person)
        } else {
            Some(BrowserOwner::Home)
        }
    }

    /// Writes its name, engine, model, owner, creator, privacy and loop
    /// settings back to `bot.toml`.
    ///
    /// # Errors
    /// Fails if the file can't be written.
    pub fn save(&self) -> Result<(), BotError> {
        write_file(
            &self.dir,
            &BotFile {
                name: self.name.clone(),
                engine: engine_entry(self.engine),
                model: self.model.clone(),
                owner: self.owner.as_ref().map(ToString::to_string),
                created_by: self.created_by.clone(),
                private: self.private,
                openrouter: (!self.loop_settings.is_empty()).then(|| self.loop_settings.clone()),
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
    if !load_all(bots_dir)?.iter().any(Bot::is_main_herder) {
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
    let Some(mut bot) = load_all(bots_dir)?.into_iter().find(Bot::is_main_herder) else {
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
    fn a_persons_herder_id_is_theirs_alone() {
        let ann = personal_herder_id("Ann");
        assert!(is_personal_herder(ann.as_str()));
        assert_eq!(ann, personal_herder_id(" ann "));
        assert_ne!(ann, personal_herder_id("Anne"));
        assert_ne!(personal_herder_id("José"), personal_herder_id("Jos"));
        assert!(is_personal_herder(personal_herder_id("王芳").as_str()));
        assert!(personal_herder_id(&"x".repeat(80)).as_str().len() <= crate::MAX_BOT_ID_LEN);
        // A bot named by a person never gets a Herder's id.
        let id = BotId::from_name("Assistant -- Ann", |_| false);
        assert!(!is_personal_herder(id.as_str()));
    }

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
    fn an_owner_is_saved_and_a_bad_one_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let sam = PersonId::random();
        let id = BotId::new("bank").unwrap();
        let bot = Bot::create_owned(tmp.path(), id, "Bank", "", Some(&sam), Some("Sam")).unwrap();
        assert_eq!(Bot::load(&bot.dir).unwrap().owner, Some(sam.clone()));
        let other = PersonId::random();
        let mut viewer = Viewer::person(sam, "Someone else's name", false);
        let mut private = Bot::load(&bot.dir).unwrap();
        private.private = true;
        assert!(private.visible_to(&viewer) && private.manageable_by(&viewer));
        viewer.id = Some(other);
        viewer.name = "Sam".into();
        assert!(
            !private.visible_to(&viewer) && !private.manageable_by(&viewer),
            "the same name with another id owns nothing"
        );
        fs::write(
            bot.dir.join("bot.toml"),
            "name = \"Bank\"\nowner = \"Sam\"\n",
        )
        .unwrap();
        assert_eq!(Bot::load(&bot.dir).unwrap().owner, None);
    }

    #[test]
    fn a_bots_engine_is_saved_and_absent_means_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let id = BotId::new("scout").unwrap();
        let mut bot = Bot::create(tmp.path(), id, "Scout", "").unwrap();
        assert_eq!(bot.engine, EngineKind::Claude);
        assert_eq!(bot.loop_settings, LoopSettings::default());

        bot.engine = EngineKind::OpenRouter;
        bot.loop_settings.model = Some("auto".into());
        bot.loop_settings.daily_requests = Some(200);
        bot.save().unwrap();
        let text = fs::read_to_string(bot.dir.join("bot.toml")).unwrap();
        assert!(text.contains("engine = \"openrouter\""), "{text}");
        assert!(text.contains("[openrouter]") && text.contains("daily_requests = 200"));
        let again = Bot::load(&bot.dir).unwrap();
        assert_eq!(again.engine, EngineKind::OpenRouter);
        assert_eq!(again.loop_settings.model.as_deref(), Some("auto"));
        assert_eq!(again.loop_settings.daily_requests, Some(200));

        // Back to Claude: the file says nothing about engines again.
        let mut again = again;
        again.engine = EngineKind::Claude;
        again.loop_settings = LoopSettings::default();
        again.save().unwrap();
        let text = fs::read_to_string(bot.dir.join("bot.toml")).unwrap();
        assert!(
            !text.contains("engine") && !text.contains("openrouter"),
            "{text}"
        );
    }

    #[test]
    fn an_old_file_keeps_its_bytes_and_an_unknown_engine_is_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("news");
        fs::create_dir_all(&dir).unwrap();
        let old = "name = \"News\"\nmodel = \"haiku\"\nowner = \"p_0123456789abcdef\"\ncreated_by = \"Sam\"\nprivate = true\n";
        fs::write(dir.join("bot.toml"), old).unwrap();
        let bot = Bot::load(&dir).unwrap();
        assert_eq!(bot.engine, EngineKind::Claude);
        bot.save().unwrap();
        assert_eq!(fs::read_to_string(dir.join("bot.toml")).unwrap(), old);

        fs::write(dir.join("bot.toml"), "name = \"News\"\nengine = \"gpt\"\n").unwrap();
        assert_eq!(Bot::load(&dir).unwrap().engine, EngineKind::Claude);
        fs::write(
            dir.join("bot.toml"),
            "name = \"News\"\nengine = \"codex\"\n",
        )
        .unwrap();
        assert_eq!(Bot::load(&dir).unwrap().engine, EngineKind::Codex);
        fs::write(
            dir.join("bot.toml"),
            "name = \"News\"\nengine = \"claude\"\n",
        )
        .unwrap();
        assert_eq!(Bot::load(&dir).unwrap().engine, EngineKind::Claude);
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
