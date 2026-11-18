//! [`Bot`]s on disk.
//!
//! Each bot is a folder named after its id:
//!
//! ```text
//! bots/assistant/
//!   bot.toml          name = "Assistant", engine, owner, created_by, private,
//!                     sharing (everyone, me or people), [openrouter]
//!                     settings (its model and daily limit)
//!   instructions.md   appended to the bot's system prompt
//!   work/             the bot's working folder: the only place it may touch
//! ```
//!
//! A file from before Codex was the only engine says `engine = "claude"`,
//! no engine at all (every bot made before engines), or `engine =
//! "openrouter"` (oxbot's own loop, retired): that bot runs on Codex now,
//! and [`Bot::moved_from`] says so until the hub upgrades its file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::EngineKind;
use crate::id::{BotId, BotIdError, BrowserOwner, PersonId};
pub use oxbot_protocol::Sharing;

/// The central bot's id. It's the Assistant: created on first run, kept if
/// missing, and the only bot that can create other bots and give them tasks.
/// (The id stays `assistant`; for a while the bot was called Herder.)
pub const DEFAULT_BOT: &str = "assistant";
/// The central bot's name.
pub const CENTRAL_NAME: &str = "Assistant";
/// How a person's own Assistant's id starts: `assistant--sam-1a2b3c`. Ids made
/// from names never have two dashes in a row, so no other bot gets one.
pub const PERSONAL_ASSISTANT_PREFIX: &str = "assistant--";

/// The family Home's three lead bots: id, name and built-in instructions.
/// Every home gets them once (a deleted one stays deleted). The page shows
/// each beside its own screen; the Assistant stays, out of the bot list.
pub const HOME_LEADS: &[(&str, &str, &str)] = &[
    (
        "home-calendar",
        "Calendar",
        "You are the family's Calendar bot. You keep everyone's events: school, \
         practice, appointments, birthdays and trips. When someone tells you about \
         something coming up, add it to the calendar with add_event: the day, \
         time, place and who it's for. Ask only when the day or what it is isn't \
         clear. Look with calendar_events first so nothing is added twice, and to \
         answer what's on. After a change, say in one line what you did. Keep replies short: the family may be reading on \
         a phone. You can make helper bots for jobs like reading a school's calendar.",
    ),
    (
        "home-meals",
        "Meal Plan",
        "You are the family's Meal Plan bot. You plan the week's meals, which \
         everyone sees on the Meal Plan. The first time, ask a few questions: \
         who eats, allergies and dislikes, busy nights, budget and favorite \
         meals. Plan with plan_meal (one dish a call, for a day's breakfast, \
         lunch or dinner; look with meal_plan first so nothing is planned \
         twice), then put what's needed on Groceries, the family's shared \
         shopping list, with add_groceries (with the meal's id and an aisle). \
         After a change, say in one line what you did. Keep replies short: the \
         family may be reading on a phone.",
    ),
    (
        "home-notes",
        "Records",
        "You are the family's Records bot. You keep the family's records (bills, \
         insurance, receipts, warranties, forms) so anyone can find them again, from any device. A file attached here is \
         filed in Records at once, with what the person said about it. When \
         you may read it, read it and file it better with file_note: a short, \
         findable title; the document's own date (a bill's, a visit's, a \
         statement's) unless they gave one; one broad topic, reusing ones already \
         there (find_notes lists them); a few tags; who it's about; a one-line \
         summary. Then show_note it and say in one line where it went. A private \
         file you can't read: file it only by what they tell you, and ask for a \
         title or who it's for if they gave none. When someone asks for a file, \
         find_notes and show_note the best match. Never ask for or repeat \
         passwords, PINs or card numbers: say they're kept on the home computer \
         only. Keep replies short: the family may be reading on a phone.",
    ),
    (
        "home-groceries",
        "Groceries",
        "You are the family's Groceries bot. You keep the family's shared \
         shopping list: food and everyday household things (toilet paper, dish \
         soap, batteries). \"These\", \"the list\", \"everything\" or \"what we \
         need\" mean the items on it: read it with grocery_list first, never ask \
         them to list items. When someone says they need something, add it with \
         add_groceries, with an aisle and an amount if they said one. Tick items \
         off when someone says they got them, and take items off when asked. \
         The Meal Plan bot adds what its meals need to the same list. When \
         someone asks you to order or fill a cart at a store (Walmart, Costco, \
         Instacart...): that's their own account there, so if the site isn't \
         signed in, ask them to sign in with request_sign_in; then search the \
         store for each item not yet got, pick a sensible match for the amount, \
         and add it to the cart, one at a time. Never check out or pay: say the \
         cart is ready, and list anything you couldn't find. After a change, say \
         in one line what you did. Keep replies short: the family may be reading \
         it in the store, on a phone.",
    ),
];

/// The Groceries bot's first instructions: a home that still has them,
/// unchanged, gets the new ones.
const FIRST_GROCERIES_INSTRUCTIONS: &str = "You are the family's Groceries bot. You keep the family's shared \
         shopping list: food and everyday household things (toilet paper, dish \
         soap, batteries). When someone says they need something, add it with \
         add_groceries, with an aisle and an amount if they said one; look with \
         grocery_list to answer what's left. Tick items off when someone says \
         they got them, and take items off when asked. The Meal Plan bot adds \
         what its meals need to the same list. After a change, say in one line \
         what you did. Keep replies short: the family may be reading it in the \
         store, on a phone.";

/// The Meal Plan bot's instructions from before Groceries was its own
/// Home lead (Nov 2026): a home that still has them gets the new ones.
const MEALS_WITH_GROCERIES_INSTRUCTIONS: &str = "You are the family's Meal Plan bot. You plan the week's meals and keep \
         the grocery list, both of which everyone sees on the Meal Plan. The \
         first time, ask a few questions: who eats, allergies and dislikes, busy \
         nights, budget and favorite meals. Plan with plan_meal (one dish a call, \
         for a day's breakfast, lunch or dinner; look with meal_plan first so \
         nothing is planned twice), then add what's needed with add_groceries, \
         with the meal's id and an aisle. Tick items off when someone says they \
         got them. After a change, say in one line what you did. Keep replies \
         short: the family may be reading on a phone. You can suggest helper \
         bots, such as one that orders groceries from a store.";

/// The Meal Plan bot's first instructions, before it had the Meal Plan's
/// tools: a home that still has them, unchanged, gets the new ones.
const OLD_MEALS_INSTRUCTIONS: &str = "You are the family's Meal Plan bot. You plan the week's dinners and keep \
         the grocery list. The first time, ask a few questions: who eats, \
         allergies and dislikes, busy nights, budget and favorite meals. Then plan \
         a week and add what's needed to the grocery list. Keep replies short. You \
         can suggest helper bots, such as one that orders groceries from a store.";

/// The Records bot's instructions from when it was Family Notes (Nov
/// 2026): a home that still has them, unchanged, gets the new ones.
const FAMILY_NOTES_INSTRUCTIONS: &str = "You are the family's Notes bot. You keep the family's files and notes so \
         anyone can find them again, from any device. A file attached here is \
         filed in Family Notes at once, with what the person said about it. When \
         you may read it, read it and file it better with file_note: a short, \
         findable title; the document's own date (a bill's, a visit's, a \
         statement's) unless they gave one; one broad topic, reusing ones already \
         there (find_notes lists them); a few tags; who it's about; a one-line \
         summary. Then show_note it and say in one line where it went. A private \
         file you can't read: file it only by what they tell you, and ask for a \
         title or who it's for if they gave none. When someone asks for a file, \
         find_notes and show_note the best match. Never ask for or repeat \
         passwords, PINs or card numbers: say they're kept on the home computer \
         only. Keep replies short: the family may be reading on a phone.";

/// The Family Notes bot's first instructions, before it filed files:
/// a home that still has them gets the new ones.
const OLD_NOTES_INSTRUCTIONS: &str = "You are the family's Notes bot. Anyone can drop anything here: a Wi-Fi \
         name, a doctor's number, a recipe, a warranty, a school form. File each \
         note with a short title and tags so it's easy to find, and answer \
         questions from the notes. Never ask for or repeat passwords, PINs or card \
         numbers: say they're kept on the home computer only. Keep replies short.";

/// Whether `id` is a Home bot that works without the web: the Calendar and
/// Family Notes (Meal Plan may shop).
#[must_use]
pub fn needs_no_browser(id: &str) -> bool {
    id == HOME_LEADS[0].0 || id == HOME_LEADS[2].0
}

/// Whether `id` is one of the Home's lead bots.
#[must_use]
pub fn is_home_lead(id: &str) -> bool {
    HOME_LEADS.iter().any(|(i, ..)| *i == id)
}

/// Whether `id` is a person's own Assistant (not the home computer's, `assistant`).
#[must_use]
pub fn is_personal_assistant(id: &str) -> bool {
    id.starts_with(PERSONAL_ASSISTANT_PREFIX)
}

/// The id of `person`'s own Assistant: their name made safe, plus a short hash
/// of it (ignoring case), so different names never share one.
#[must_use]
pub fn personal_assistant_id(person: &str) -> BotId {
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
        format!("{PERSONAL_ASSISTANT_PREFIX}{:06x}", hash & 0xff_ffff)
    } else {
        format!("{PERSONAL_ASSISTANT_PREFIX}{slug}-{:06x}", hash & 0xff_ffff)
    };
    BotId::new(id).unwrap_or_else(|_| unreachable!("only a-z, 0-9 and dashes, at most 38 long"))
}

/// An Assistant id for `person` made from their id, for when the one made from
/// their name is someone else's (`Sam` and `sam` share it). It ends in 8
/// hex digits where [`personal_assistant_id`] has 6, so the two never meet.
#[must_use]
pub fn personal_assistant_id_for(person: &str, id: &PersonId) -> BotId {
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
        format!("{PERSONAL_ASSISTANT_PREFIX}{hex}")
    } else {
        format!("{PERSONAL_ASSISTANT_PREFIX}{slug}-{hex}")
    };
    BotId::new(id).unwrap_or_else(|_| unreachable!("only a-z, 0-9 and dashes, at most 40 long"))
}
/// Names and default instructions the central bot had before: it was
/// "Assistant", then "Herder", and is the Assistant again.
const OLD_NAMES: &[&str] = &["Herder"];
const OLD_INSTRUCTIONS: &[&str] = &[
    "You are Herder, oxbot's lead bot on the user's Mac. The user comes to you\nfirst: answer what you can, and guide their herd of bots for the rest.\nKeep replies short: the user may be reading on a phone.",
    "You are oxbot's assistant, running on the user's Mac.\nKeep replies short: the user may be reading on a phone.\nYou can read and write files in your working folder.",
    "You are oxbot's Assistant, running on the user's Mac. You are the user's\nfirst stop: answer what you can, and run the team of bots for the rest.\nKeep replies short: the user may be reading on a phone.",
];

const DEFAULT_INSTRUCTIONS: &str = "\
You are the Assistant, oxbot's lead bot on the user's home computer. The
user comes to you first: answer what you can, and hand the rest to their
other bots. Keep replies short: the user may be reading on a phone.";

/// Who is asking: a person, and whether they're on the home computer itself.
///
/// The home computer (the Mac that runs the bots) can see and change every bot.
/// Anyone else sees shared bots and their own private ones. These are the
/// only rules for that: the page and the Assistant get their answers from here.
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
    /// On the home computer itself. Only the home computer secret makes a connection this,
    /// never a name.
    pub home_computer: bool,
}

impl Viewer {
    /// Someone on the home computer.
    #[must_use]
    pub fn home_computer(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            home_computer: true,
        }
    }

    /// Someone on a device (a laptop or phone), or a bot acting for them.
    #[must_use]
    pub fn device(name: impl Into<String>) -> Self {
        Self {
            id: None,
            name: name.into(),
            home_computer: false,
        }
    }

    /// The person `id` called `name`, on the home computer (`home_computer`) or not.
    #[must_use]
    pub fn person(id: PersonId, name: impl Into<String>, home_computer: bool) -> Self {
        Self {
            id: Some(id),
            name: name.into(),
            home_computer,
        }
    }

    /// Whether they may see a bot made by `made_by` that is `private`.
    /// `made_by` is the maker's id for a viewer with an id, else their name
    /// (see [`Viewer::maker`]).
    #[must_use]
    pub fn can_see(&self, made_by: Option<&str>, private: bool) -> bool {
        self.can_see_shared(made_by, private, &[])
    }

    /// Like [`Viewer::can_see`], for a private bot shared with `members`
    /// (person ids; picked people). A member is matched by id only.
    #[must_use]
    pub fn can_see_shared(&self, made_by: Option<&str>, private: bool, members: &[String]) -> bool {
        self.home_computer || !private || self.made(made_by) || self.is_member(members)
    }

    /// Whether they're one of `members` (person ids), by id.
    #[must_use]
    pub fn is_member(&self, members: &[String]) -> bool {
        self.id
            .as_ref()
            .is_some_and(|id| members.iter().any(|m| m == id.as_str()))
    }

    /// Whether they may rename, delete, or change who sees a bot made by
    /// `made_by` (as for [`Viewer::can_see`]).
    #[must_use]
    pub fn can_manage(&self, made_by: Option<&str>) -> bool {
        self.home_computer || self.made(made_by)
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
        self.can_see_shared(
            self.maker(b.owner.as_deref(), b.created_by.as_deref()),
            b.private,
            &b.member_ids,
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
            b.locked = !self.can_see_info(b);
            b.can_manage = !b.central && self.can_manage(maker);
            b.can_task = !b.locked;
            if b.locked {
                b.busy = false;
                b.last_seq = 0;
                b.last_reply_seq = 0;
                b.browser_state = oxbot_protocol::BrowserState::Off;
                // Who else it's shared with, and what it's for, are theirs to know.
                b.members.clear();
                b.instructions.clear();
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
    /// Extra instructions for the bot.
    pub instructions: String,
    /// Who it belongs to: the person who made it (or the home computer's own person,
    /// for bots from before oxbot kept track). Access is decided by this.
    pub owner: Option<PersonId>,
    /// Its maker's name when it was made, for showing.
    pub created_by: Option<String>,
    /// Not shared with everyone: only its owner (and the home computer), and the
    /// people it's shared with if [`Bot::sharing`] says so, can see and use
    /// it, and it uses only its owner's own browser.
    pub private: bool,
    /// Who it's shared with, as its file says (see
    /// [`Bot::effective_sharing`]: `private` wins if the two disagree).
    pub sharing: Sharing,
    /// The people picked ([`Sharing::People`]), kept in the database
    /// (`bot_members`); the hub fills them in.
    pub members: Vec<PersonId>,
    /// The engine that runs it, as its file says (Assistants' engines are
    /// fixed by the hub).
    pub engine: EngineKind,
    /// The engine its file still names, when that's one oxbot no longer
    /// has ([`CLAUDE`], also for no engine: made before engines; or
    /// [`LOOP`]): it runs on Codex now, and the hub rewrites the file once
    /// and tells its chat (see `Hub::upgrade_from_claude`).
    pub moved_from: Option<&'static str>,
    /// Its model settings (the `[openrouter]` table, named from oxbot's
    /// retired loop).
    pub loop_settings: LoopSettings,
    /// Runs the browser hidden (no window) while it browses. A sign-in or
    /// robot check still brings the browser forward.
    pub browser_hidden: bool,
    /// A lead bot: like the Assistant, it can list the home's bots, give
    /// them tasks and create helpers (see [`Bot::is_lead`]).
    pub lead: bool,
    /// The lead bot that created it, if one did: it's listed under that
    /// lead as its helper.
    pub helper_of: Option<BotId>,
    /// The bot's folder.
    pub dir: PathBuf,
}

/// A bot's model settings. Every one is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopSettings {
    /// `auto`, or a model id the owner pinned (from before bots had their
    /// own lists; [`LoopSettings::models`] wins when it has any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The bot's own models, tried in this order (free and paid ids
    /// mixed): its free ones first, then other free models, then its paid
    /// ones if the owner allows paid models. Empty: automatic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Its own cap on model requests a day; none: the household's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_requests: Option<u32>,
}

impl LoopSettings {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Gives the bot its own ordered list (empty: automatic). A list
    /// replaces a pinned model.
    pub fn set_models(&mut self, models: Vec<String>) {
        if !models.is_empty() {
            self.model = None;
        }
        self.models = models;
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BotFile {
    name: String,
    /// Absent: `claude` (every bot made before engines).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engine: Option<String>,
    /// The owner's person id (`p_…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_by: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    private: bool,
    /// `everyone`, `me` or `people`; absent in files from before sharing
    /// (then `private` says: me, else everyone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sharing: Option<Sharing>,
    /// Before the `[openrouter]` table: TOML writes plain values first.
    /// Hidden unless the file says `browser_hidden = false` (Nov 2026: every
    /// bot's browser is hidden by default, shown only when the user is
    /// needed).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    browser_hidden: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    lead: bool,
    /// The id of the lead bot that created it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    helper_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    openrouter: Option<LoopSettings>,
}

impl BotFile {
    /// Who the bot is shared with. A file that says private is never
    /// shared with everyone, whatever else it says.
    fn sharing(&self) -> Sharing {
        match (self.sharing, self.private) {
            (Some(Sharing::Everyone) | None, true) => Sharing::Me,
            (Some(s), _) => s,
            (None, false) => Sharing::Everyone,
        }
    }
}

/// The engine files from before Claude left oxbot name.
pub const CLAUDE: &str = "claude";
/// The engine files of bots on oxbot's own loop (retired) name.
pub const LOOP: &str = "openrouter";

impl BotFile {
    /// The engine it names, and the retired one it moves from (Claude,
    /// also for no engine; or oxbot's own loop), which runs on Codex now.
    /// One this oxbot doesn't know (a newer oxbot's) runs on Codex too,
    /// under the same rules, and its file is left as it is.
    fn engine(&self, id: &BotId) -> (EngineKind, Option<&'static str>) {
        match self.engine.as_deref() {
            None | Some(CLAUDE) => (EngineKind::Codex, Some(CLAUDE)),
            Some(LOOP) => (EngineKind::Codex, Some(LOOP)),
            Some(name) => {
                if let Some(kind) = EngineKind::parse(name) {
                    (kind, None)
                } else {
                    tracing::warn!(bot = %id, engine = name, "unknown engine in bot.toml; using codex");
                    (EngineKind::Codex, None)
                }
            }
        }
    }
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

#[allow(clippy::unnecessary_wraps, reason = "serde's default takes a function")]
const fn yes() -> bool {
    true
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes a reference"
)]
const fn is_true(b: &bool) -> bool {
    *b
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> BotError + '_ {
    move |source| BotError::Io {
        path: path.to_owned(),
        source,
    }
}

impl Bot {
    /// Whether this is the Assistant: the home computer's (`assistant`) or a person's own.
    #[must_use]
    pub fn is_central(&self) -> bool {
        self.is_main_assistant() || is_personal_assistant(self.id.as_str())
    }

    /// Whether it can run a team: list the home's bots, give them tasks and
    /// create helpers. The Assistant always can; any other bot when it's
    /// made a lead.
    #[must_use]
    pub fn is_lead(&self) -> bool {
        self.lead || self.is_central()
    }

    /// Whether this is the home computer's Assistant, whose instructions every person's
    /// Assistant shares.
    #[must_use]
    pub fn is_main_assistant(&self) -> bool {
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
        let (engine, moved_from) = file.engine(&id);
        let sharing = file.sharing();
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
            owner,
            created_by: file.created_by,
            private: sharing != Sharing::Everyone,
            sharing,
            members: Vec::new(),
            engine,
            moved_from,
            loop_settings: file.openrouter.unwrap_or_default(),
            browser_hidden: file.browser_hidden,
            lead: file.lead,
            helper_of: file.helper_of.and_then(|h| BotId::new(h).ok()),
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
            engine: Some(EngineKind::default().as_str().to_owned()),
            owner: owner.map(ToString::to_string),
            created_by: created_by.map(str::to_owned),
            private: false,
            sharing: Some(Sharing::Everyone),
            openrouter: None,
            browser_hidden: true,
            lead: false,
            helper_of: None,
        };
        write_file(&dir, &file)?;
        let md = dir.join("instructions.md");
        fs::write(&md, instructions).map_err(io_err(&md))?;
        Self::load(&dir)
    }

    /// The bot's working folder.
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

    /// Who it's shared with: its file's choice, except that a bot marked
    /// private is never everyone's, and a personal Assistant is always just
    /// its owner's.
    #[must_use]
    pub fn effective_sharing(&self) -> Sharing {
        if is_personal_assistant(self.id.as_str()) {
            return Sharing::Me;
        }
        match (self.private, self.sharing) {
            (false, _) => Sharing::Everyone,
            (true, Sharing::Everyone) => Sharing::Me,
            (true, s) => s,
        }
    }

    /// The people it's shared with besides its owner, by id: its picked
    /// people when it's shared with them, else nobody.
    #[must_use]
    pub fn member_ids(&self) -> Vec<String> {
        if self.effective_sharing() == Sharing::People {
            self.members.iter().map(ToString::to_string).collect()
        } else {
            Vec::new()
        }
    }

    /// Sets who it's shared with (`members` only count for
    /// [`Sharing::People`]).
    pub fn set_sharing(&mut self, sharing: Sharing, members: Vec<PersonId>) {
        self.sharing = sharing;
        self.private = sharing != Sharing::Everyone;
        self.members = if sharing == Sharing::People {
            members
        } else {
            Vec::new()
        };
    }

    /// Whether `viewer` may see and use it: everyone for a shared bot; its
    /// owner, the home computer and its picked people for one shared with them; its
    /// owner and the home computer otherwise. A personal Assistant is only its owner's
    /// (and the home computer's), even if an old file says it's shared. The home computer's
    /// Assistant needs the hub to know who the home computer's person is: see
    /// `Hub::bot_for`.
    #[must_use]
    pub fn visible_to(&self, viewer: &Viewer) -> bool {
        let private = self.private || is_personal_assistant(self.id.as_str());
        viewer.can_see_shared(self.maker_for(viewer), private, &self.member_ids())
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

    /// Writes its name, engine, owner, creator, privacy and loop settings
    /// back to `bot.toml` (a moved bot gets Codex written out once the hub
    /// moved it, and loses the Claude model it named).
    ///
    /// # Errors
    /// Fails if the file can't be written.
    pub fn save(&self) -> Result<(), BotError> {
        write_file(
            &self.dir,
            &BotFile {
                name: self.name.clone(),
                // Until the hub has moved it (and told its chat), a moved bot
                // keeps its file's old engine, whoever saves it.
                engine: match self.moved_from {
                    None => Some(self.engine.as_str().to_owned()),
                    Some(CLAUDE) => None,
                    Some(from) => Some(from.to_owned()),
                },
                owner: self.owner.as_ref().map(ToString::to_string),
                created_by: self.created_by.clone(),
                // Both, so an older oxbot (which reads only `private`) keeps
                // a bot shared with picked people private.
                private: self.effective_sharing() != Sharing::Everyone,
                sharing: Some(self.effective_sharing()),
                openrouter: (!self.loop_settings.is_empty()).then(|| self.loop_settings.clone()),
                browser_hidden: self.browser_hidden,
                lead: self.lead,
                helper_of: self.helper_of.as_ref().map(ToString::to_string),
            },
        )
    }

    /// Writes `instructions.md` all at once: a crash mid-write leaves the
    /// old file, never half of the new one.
    ///
    /// # Errors
    /// Fails if the file can't be written.
    pub fn save_instructions(&self) -> Result<(), BotError> {
        let path = self.dir.join("instructions.md");
        let tmp = self.dir.join("instructions.md.tmp");
        let write = || -> io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            io::Write::write_all(&mut f, self.instructions.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &path)
        };
        write().map_err(io_err(&path))
    }

    /// Moves the bot's folder into `trash` (as `<id>-<when>`), so a deleted
    /// bot's files can still be found by hand. Returns where it went.
    ///
    /// # Errors
    /// Fails if the folder can't be moved.
    pub fn move_to(&self, trash: &Path, when_ms: i64) -> Result<PathBuf, BotError> {
        crate::home::private_dir(trash).map_err(io_err(trash))?;
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

/// Bots whose files are from before sharing (no `sharing` line) get one
/// written, as their `private` said: me, else everyone. Once each; a file
/// that can't be written is tried again next start (it reads the same
/// meanwhile).
pub fn write_sharing_once(bots: &[Bot]) {
    for bot in bots {
        let path = bot.dir.join("bot.toml");
        let has = fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
            .is_some_and(|t| t.contains_key("sharing"));
        if has {
            continue;
        }
        match bot.save() {
            Ok(()) => {
                tracing::info!(bot = %bot.id, sharing = ?bot.effective_sharing(), "wrote who a bot is shared with");
            }
            Err(err) => {
                tracing::warn!(bot = %bot.id, %err, "couldn't write who a bot is shared with");
            }
        }
    }
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

/// Creates `bots_dir` (this account's only: 0700) and the default `assistant`
/// bot if there are no bots yet.
///
/// # Errors
/// Fails if the folders or files can't be written.
pub fn ensure_default(bots_dir: &Path) -> Result<(), BotError> {
    crate::home::private_dir(bots_dir).map_err(io_err(bots_dir))?;
    if !load_all(bots_dir)?.iter().any(Bot::is_main_assistant) {
        let id = BotId::new(DEFAULT_BOT).map_err(|source| BotError::BadId {
            path: bots_dir.join(DEFAULT_BOT),
            source,
        })?;
        Bot::create(bots_dir, id, CENTRAL_NAME, DEFAULT_INSTRUCTIONS)?;
    }
    rename_old_central(bots_dir)?;
    ensure_home_leads(bots_dir)
}

/// Makes the Home's lead bots ([`HOME_LEADS`]) once per home: a marker in
/// `bots_dir` means it was done, so one the family deletes stays deleted.
fn ensure_home_leads(bots_dir: &Path) -> Result<(), BotError> {
    let marker = bots_dir.join(".home-leads");
    if marker.exists() {
        // Family Notes' first instructions, never changed: the new ones.
        let path = bots_dir.join(HOME_LEADS[2].0).join("instructions.md");
        if fs::read_to_string(&path)
            .is_ok_and(|t| [OLD_NOTES_INSTRUCTIONS, FAMILY_NOTES_INSTRUCTIONS].contains(&t.trim()))
        {
            fs::write(&path, HOME_LEADS[2].2).map_err(io_err(&path))?;
        }
        let meals = bots_dir.join(HOME_LEADS[1].0).join("instructions.md");
        if fs::read_to_string(&meals).is_ok_and(|t| {
            [OLD_MEALS_INSTRUCTIONS, MEALS_WITH_GROCERIES_INSTRUCTIONS].contains(&t.trim())
        }) {
            fs::write(&meals, HOME_LEADS[1].2).map_err(io_err(&meals))?;
        }
        let groceries = bots_dir.join(HOME_LEADS[3].0).join("instructions.md");
        if fs::read_to_string(&groceries).is_ok_and(|t| t.trim() == FIRST_GROCERIES_INSTRUCTIONS) {
            fs::write(&groceries, HOME_LEADS[3].2).map_err(io_err(&groceries))?;
        }
        // Family Notes is called Records now (a name the person chose stays).
        if let Ok(mut notes) = Bot::load(&bots_dir.join(HOME_LEADS[2].0))
            && notes.name == "Family Notes"
        {
            HOME_LEADS[2].1.clone_into(&mut notes.name);
            notes.save()?;
        }
        return ensure_new_leads(bots_dir);
    }
    for (id, name, instructions) in HOME_LEADS {
        if bots_dir.join(id).exists() {
            continue;
        }
        let id = BotId::new(*id).map_err(|source| BotError::BadId {
            path: bots_dir.join(id),
            source,
        })?;
        let mut bot = Bot::create(bots_dir, id, name, instructions)?;
        bot.lead = true;
        bot.save()?;
    }
    fs::write(&marker, "").map_err(io_err(&marker))?;
    ensure_new_leads(bots_dir)
}

/// Home leads added after a home was made (Groceries, Nov 2026): made once,
/// each with its own marker, so one the person deleted stays deleted.
fn ensure_new_leads(bots_dir: &Path) -> Result<(), BotError> {
    for (id, name, instructions) in &HOME_LEADS[3..] {
        let marker = bots_dir.join(format!(".home-lead-{id}"));
        if marker.exists() {
            continue;
        }
        if !bots_dir.join(id).exists() {
            let bot_id = BotId::new(*id).map_err(|source| BotError::BadId {
                path: bots_dir.join(id),
                source,
            })?;
            let mut bot = Bot::create(bots_dir, bot_id, name, instructions)?;
            bot.lead = true;
            bot.save()?;
        }
        fs::write(&marker, "").map_err(io_err(&marker))?;
    }
    Ok(())
}

/// A central bot (the main one or a person's own) still called "Herder"
/// becomes the Assistant, and old default instructions become the new
/// ones, once: a marker file in its folder means it was done already, so a
/// name the user picked by hand stays. Instructions the user wrote are kept.
/// The marker is the second one: an older oxbot (Nov 2026) could rename the
/// Assistant back to Herder after the first, so homes it reached are fixed
/// once more.
fn rename_old_central(bots_dir: &Path) -> Result<(), BotError> {
    for mut bot in load_all(bots_dir)?.into_iter().filter(Bot::is_central) {
        let marker = bot.dir.join(".renamed-to-assistant-2");
        if marker.exists() {
            continue;
        }
        if OLD_NAMES.contains(&bot.name.as_str()) {
            CENTRAL_NAME.clone_into(&mut bot.name);
            bot.save()?;
            tracing::info!(bot = %bot.id, "Herder is now called {CENTRAL_NAME}");
        }
        if bot.is_main_assistant() && OLD_INSTRUCTIONS.contains(&bot.instructions.trim()) {
            let path = bot.dir.join("instructions.md");
            fs::write(&path, DEFAULT_INSTRUCTIONS).map_err(io_err(&path))?;
        }
        fs::write(&marker, "").map_err(io_err(&marker))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_persons_assistant_id_is_theirs_alone() {
        let ann = personal_assistant_id("Ann");
        assert!(is_personal_assistant(ann.as_str()));
        assert_eq!(ann, personal_assistant_id(" ann "));
        assert_ne!(ann, personal_assistant_id("Anne"));
        assert_ne!(personal_assistant_id("José"), personal_assistant_id("Jos"));
        assert!(is_personal_assistant(
            personal_assistant_id("王芳").as_str()
        ));
        assert!(personal_assistant_id(&"x".repeat(80)).as_str().len() <= crate::MAX_BOT_ID_LEN);
        // A bot named by a person never gets an Assistant's id.
        let id = BotId::from_name("Assistant -- Ann", |_| false);
        assert!(!is_personal_assistant(id.as_str()));
    }

    #[test]
    fn family_notes_becomes_records_unless_renamed() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_default(tmp.path()).unwrap();
        let dir = tmp.path().join(HOME_LEADS[2].0);
        // As a home made before the rename has it.
        let mut notes = Bot::load(&dir).unwrap();
        "Family Notes".clone_into(&mut notes.name);
        notes.save().unwrap();
        fs::write(dir.join("instructions.md"), FAMILY_NOTES_INSTRUCTIONS).unwrap();
        ensure_default(tmp.path()).unwrap();
        let notes = Bot::load(&dir).unwrap();
        assert_eq!(notes.name, "Records");
        assert_eq!(notes.instructions.trim(), HOME_LEADS[2].2);
        // A name and instructions the person wrote stay.
        let mut notes = notes;
        "Our papers".clone_into(&mut notes.name);
        notes.save().unwrap();
        fs::write(dir.join("instructions.md"), "Keep it short.").unwrap();
        ensure_default(tmp.path()).unwrap();
        let notes = Bot::load(&dir).unwrap();
        assert_eq!(notes.name, "Our papers");
        assert_eq!(notes.instructions.trim(), "Keep it short.");
    }

    #[test]
    fn default_bot_is_created_once() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_default(tmp.path()).unwrap();
        ensure_default(tmp.path()).unwrap();
        let bots = load_all(tmp.path()).unwrap();
        let ids: Vec<_> = bots.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "assistant",
                "home-calendar",
                "home-groceries",
                "home-meals",
                "home-notes"
            ]
        );
        assert!(bots[0].instructions.contains("phone"));
        assert!(bots[0].work_dir().is_dir());
        assert_eq!(bots[0].name, CENTRAL_NAME);
        assert!(bots[1..].iter().all(|b| b.lead && !b.is_central()));
        assert_eq!(bots[2].name, "Groceries");

        // A Home lead the family deletes stays deleted.
        fs::remove_dir_all(tmp.path().join("home-notes")).unwrap();
        fs::remove_dir_all(tmp.path().join("home-groceries")).unwrap();
        ensure_default(tmp.path()).unwrap();
        assert_eq!(load_all(tmp.path()).unwrap().len(), 3);
    }

    #[test]
    fn a_home_made_before_groceries_gets_it_once() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_default(tmp.path()).unwrap();
        // As a home from before Groceries was a Home lead.
        fs::remove_dir_all(tmp.path().join("home-groceries")).unwrap();
        fs::remove_file(tmp.path().join(".home-lead-home-groceries")).unwrap();
        ensure_default(tmp.path()).unwrap();
        let groceries = Bot::load(&tmp.path().join("home-groceries")).unwrap();
        assert!(groceries.lead);
        assert_eq!(groceries.name, "Groceries");
    }

    #[test]
    fn assistant_becomes_the_assistant_and_keeps_its_own_instructions() {
        let tmp = tempfile::tempdir().unwrap();
        let id = BotId::new(DEFAULT_BOT).unwrap();
        let old = Bot::create(tmp.path(), id, "Herder", OLD_INSTRUCTIONS[0]).unwrap();
        fs::write(
            old.dir.join("bot.toml"),
            "name = \"Herder\"\nmodel = \"haiku\"\n",
        )
        .unwrap();
        let theirs = Bot::create(tmp.path(), personal_assistant_id("Ann"), "Herder", "").unwrap();
        ensure_default(tmp.path()).unwrap();
        let bot = Bot::load(&old.dir).unwrap();
        assert_eq!(bot.name, "Assistant");
        assert!(bot.instructions.contains("You are the Assistant"));
        assert_eq!(Bot::load(&theirs.dir).unwrap().name, "Assistant");

        // It's renamed once: a name put back by hand stays.
        fs::write(old.dir.join("bot.toml"), "name = \"Herder\"\n").unwrap();
        ensure_default(tmp.path()).unwrap();
        assert_eq!(Bot::load(&old.dir).unwrap().name, "Herder");

        // Instructions the user wrote stay (an install never renamed yet).
        fs::remove_file(old.dir.join(".renamed-to-assistant-2")).unwrap();
        fs::write(old.dir.join("instructions.md"), "Mine.").unwrap();
        ensure_default(tmp.path()).unwrap();
        let bot = Bot::load(&old.dir).unwrap();
        assert_eq!(
            (bot.name.as_str(), bot.instructions.as_str()),
            ("Assistant", "Mine.")
        );

        // A home an older oxbot renamed back to Herder after the first rename
        // (only the first marker there) is fixed once more.
        let tmp2 = tempfile::tempdir().unwrap();
        let id = BotId::new(DEFAULT_BOT).unwrap();
        let back = Bot::create(tmp2.path(), id, "Herder", "Mine.").unwrap();
        fs::write(back.dir.join(".renamed-to-assistant"), "").unwrap();
        fs::write(back.dir.join(".renamed-to-herder"), "").unwrap();
        ensure_default(tmp2.path()).unwrap();
        let bot = Bot::load(&back.dir).unwrap();
        assert_eq!(
            (bot.name.as_str(), bot.instructions.as_str()),
            ("Assistant", "Mine.")
        );

        // An install from before the Assistant, still on an old default, gets the new one.
        let tmp = tempfile::tempdir().unwrap();
        let id = BotId::new(DEFAULT_BOT).unwrap();
        let older = Bot::create(tmp.path(), id, "Assistant", OLD_INSTRUCTIONS[2]).unwrap();
        ensure_default(tmp.path()).unwrap();
        let bot = Bot::load(&older.dir).unwrap();
        assert_eq!(bot.name, "Assistant");
        assert!(bot.instructions.contains("You are the Assistant"));
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
    fn the_home_computer_sees_everything_and_others_see_shared_bots_and_their_own() {
        let home_computer = Viewer::home_computer("Jay");
        let sam = Viewer::device("Sam");
        let ann = Viewer::device("Ann");
        assert!(home_computer.can_see(Some("Sam"), true));
        assert!(sam.can_see(Some("Sam"), true));
        assert!(!ann.can_see(Some("Sam"), true));
        assert!(ann.can_see(Some("Sam"), false));
        assert!(!ann.can_see(None, true));
        assert!(
            !Viewer::device("").can_see(None, true),
            "no name owns nothing"
        );
        assert!(home_computer.can_manage(None));
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
    fn a_new_bot_runs_on_codex_and_its_engine_is_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let id = BotId::new("scout").unwrap();
        let mut bot = Bot::create(tmp.path(), id, "Scout", "").unwrap();
        assert_eq!((bot.engine, bot.moved_from), (EngineKind::Codex, None));
        assert_eq!(bot.loop_settings, LoopSettings::default());
        let text = fs::read_to_string(bot.dir.join("bot.toml")).unwrap();
        assert!(text.contains("engine = \"codex\""), "{text}");

        bot.loop_settings.model = Some("auto".into());
        bot.loop_settings.daily_requests = Some(200);
        // Hidden by default; only a shown browser is written down.
        assert!(bot.browser_hidden);
        assert!(!text.contains("browser_hidden"), "{text}");
        bot.browser_hidden = false;
        bot.save().unwrap();
        let shown = fs::read_to_string(bot.dir.join("bot.toml")).unwrap();
        assert!(shown.contains("browser_hidden = false"), "{shown}");
        assert!(!Bot::load(&bot.dir).unwrap().browser_hidden);
        bot.browser_hidden = true;
        bot.lead = true;
        bot.helper_of = Some(BotId::new("meals").unwrap());
        bot.save().unwrap();
        let text = fs::read_to_string(bot.dir.join("bot.toml")).unwrap();
        assert!(text.contains("engine = \"codex\""), "{text}");
        assert!(text.contains("[openrouter]") && text.contains("daily_requests = 200"));
        let again = Bot::load(&bot.dir).unwrap();
        assert!(again.browser_hidden);
        assert!(again.lead && again.is_lead() && !again.is_central());
        assert_eq!(again.helper_of.as_ref().map(BotId::as_str), Some("meals"));
        assert_eq!(again.engine, EngineKind::Codex);
        assert_eq!(again.loop_settings.model.as_deref(), Some("auto"));
        assert_eq!(again.loop_settings.daily_requests, Some(200));

        // Its own list: kept in order, and it replaces the pinned model.
        bot.loop_settings
            .set_models(vec!["e2e/b:free".into(), "e2e/paid".into()]);
        bot.save().unwrap();
        let text = fs::read_to_string(bot.dir.join("bot.toml")).unwrap();
        assert!(
            text.contains("models = [\"e2e/b:free\", \"e2e/paid\"]") && !text.contains("model ="),
            "{text}"
        );
        let again = Bot::load(&bot.dir).unwrap();
        assert_eq!(again.loop_settings.models, ["e2e/b:free", "e2e/paid"]);
        assert_eq!(again.loop_settings.model, None);
        // Back to automatic: no list in the file.
        bot.loop_settings.set_models(Vec::new());
        bot.save().unwrap();
        let text = fs::read_to_string(bot.dir.join("bot.toml")).unwrap();
        assert!(!text.contains("models"), "{text}");
    }

    #[test]
    fn a_claude_bots_file_runs_on_codex_and_saves_without_claude() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("news");
        fs::create_dir_all(&dir).unwrap();
        let old = "name = \"News\"\nmodel = \"haiku\"\nowner = \"p_0123456789abcdef\"\ncreated_by = \"Sam\"\nprivate = true\n";
        fs::write(dir.join("bot.toml"), old).unwrap();
        let mut bot = Bot::load(&dir).unwrap();
        assert_eq!(
            (bot.engine, bot.moved_from),
            (EngineKind::Codex, Some(CLAUDE))
        );
        // Saved for another reason first (a rename, say): still Claude's
        // until the hub moves it.
        bot.save().unwrap();
        assert_eq!(
            Bot::load(&dir).unwrap().moved_from,
            Some(CLAUDE),
            "still to be moved"
        );
        bot.moved_from = None;
        bot.save().unwrap();
        let text = fs::read_to_string(dir.join("bot.toml")).unwrap();
        assert_eq!(
            text,
            "name = \"News\"\nengine = \"codex\"\nowner = \"p_0123456789abcdef\"\ncreated_by = \"Sam\"\nprivate = true\nsharing = \"me\"\n",
            "everything but Claude's model is kept (and who it's shared with is written out)"
        );
        let again = Bot::load(&dir).unwrap();
        assert!(again.moved_from.is_none() && again.private);

        fs::write(
            dir.join("bot.toml"),
            "name = \"News\"\nengine = \"claude\"\n",
        )
        .unwrap();
        assert_eq!(
            (
                Bot::load(&dir).unwrap().engine,
                Bot::load(&dir).unwrap().moved_from
            ),
            (EngineKind::Codex, Some(CLAUDE))
        );
        // A newer oxbot's engine: Codex's rules, and not called an upgrade.
        fs::write(dir.join("bot.toml"), "name = \"News\"\nengine = \"gpt\"\n").unwrap();
        let unknown = Bot::load(&dir).unwrap();
        assert_eq!(
            (unknown.engine, unknown.moved_from),
            (EngineKind::Codex, None)
        );
        // oxbot's own loop retired: Codex, moved once, and until then the
        // file keeps saying so.
        fs::write(
            dir.join("bot.toml"),
            "name = \"News\"\nengine = \"openrouter\"\n[openrouter]\nmodel = \"e2e/a:free\"\n",
        )
        .unwrap();
        let mut looped = Bot::load(&dir).unwrap();
        assert_eq!(
            (looped.engine, looped.moved_from),
            (EngineKind::Codex, Some(LOOP))
        );
        looped.save().unwrap();
        assert_eq!(Bot::load(&dir).unwrap().moved_from, Some(LOOP));
        looped.moved_from = None;
        looped.save().unwrap();
        let text = fs::read_to_string(dir.join("bot.toml")).unwrap();
        assert!(text.contains("engine = \"codex\""), "{text}");
        assert!(
            text.contains("model = \"e2e/a:free\""),
            "its model stays: {text}"
        );
        // A file from before bots had lists: none, and its pin stays.
        let old = Bot::load(&dir).unwrap();
        assert_eq!(old.loop_settings.models, Vec::<String>::new());
        assert_eq!(old.loop_settings.model.as_deref(), Some("e2e/a:free"));
    }

    #[test]
    fn sharing_comes_from_private_in_old_files_and_private_always_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("garden");
        fs::create_dir_all(&dir).unwrap();
        let read = |text: &str| {
            fs::write(dir.join("bot.toml"), text).unwrap();
            Bot::load(&dir).unwrap()
        };
        assert_eq!(
            read("name = \"G\"\n").effective_sharing(),
            Sharing::Everyone
        );
        assert_eq!(
            read("name = \"G\"\nprivate = true\n").effective_sharing(),
            Sharing::Me
        );
        assert_eq!(
            read("name = \"G\"\nprivate = true\nsharing = \"everyone\"\n").effective_sharing(),
            Sharing::Me,
            "a hand-edited file that says both stays private"
        );
        let mut b = read("name = \"G\"\nprivate = true\nsharing = \"people\"\n");
        assert_eq!((b.effective_sharing(), b.private), (Sharing::People, true));
        // Picked people count only while it's shared with them.
        let sam = PersonId::random();
        b.members = vec![sam.clone()];
        let viewer = Viewer::person(sam.clone(), "Sam", false);
        assert!(
            b.visible_to(&viewer) && !b.manageable_by(&viewer),
            "they use it, never change it"
        );
        b.set_sharing(Sharing::Me, vec![sam.clone()]);
        assert!(!b.visible_to(&viewer) && b.members.is_empty());
        b.set_sharing(Sharing::People, vec![sam.clone()]);
        b.save().unwrap();
        let text = fs::read_to_string(dir.join("bot.toml")).unwrap();
        assert!(
            text.contains("private = true") && text.contains("sharing = \"people\""),
            "{text}"
        );
        assert_eq!(
            Bot::load(&dir).unwrap().members,
            [],
            "members live in the database"
        );
        b.set_sharing(Sharing::Everyone, vec![sam]);
        assert!(!b.private && b.member_ids().is_empty());
        // A personal Assistant is its owner's alone, whatever its file says.
        let h = Bot::create(tmp.path(), personal_assistant_id("Ann"), "Assistant", "").unwrap();
        assert_eq!(h.effective_sharing(), Sharing::Me);
    }

    #[test]
    fn a_file_from_before_sharing_gets_it_written_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("diary");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("bot.toml"),
            "name = \"Diary\"\nengine = \"codex\"\nprivate = true\n",
        )
        .unwrap();
        let bots = load_all(tmp.path()).unwrap();
        write_sharing_once(&bots);
        let text = fs::read_to_string(dir.join("bot.toml")).unwrap();
        assert!(
            text.contains("sharing = \"me\"") && text.contains("private = true"),
            "{text}"
        );
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
