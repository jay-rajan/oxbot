//! Settings → Activity: what left the home computer. One row per exchange
//! with the outside world: a question to the AI, a bot's visit to a
//! website, a device connecting, or oxbot's own calls (the models list,
//! the AI account). Never what was said: only who, where, how much and how
//! it went.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Most rows one [`crate::ServerMsg::NetLog`] holds.
pub const MAX_NET_ROWS: u32 = 500;
/// Rows per [`crate::ServerMsg::NetLog`] when the client doesn't say.
pub const DEFAULT_NET_ROWS: u32 = 100;

/// What kind of exchange a row is.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum NetKind {
    /// A question a bot sent to the AI (`OpenRouter`).
    Ai,
    /// A bot's browser visiting a website.
    Website,
    /// A phone, tablet or laptop connecting to the home computer.
    Device,
    /// oxbot's own calls: the models list, the AI account's sign-in and
    /// limits.
    Oxbot,
}

impl NetKind {
    /// Its name as stored and sent, e.g. `website`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Website => "website",
            Self::Device => "device",
            Self::Oxbot => "oxbot",
        }
    }

    /// The kind called `name` (see [`NetKind::as_str`]).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "ai" => Self::Ai,
            "website" => Self::Website,
            "device" => Self::Device,
            "oxbot" => Self::Oxbot,
            _ => return None,
        })
    }
}

/// One exchange with the outside world, as Settings → Activity shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetRow {
    /// The row's id (newer rows have bigger ids).
    pub id: i64,
    /// When it happened (a device: when it connected), ms since the Unix
    /// epoch.
    pub ts_ms: i64,
    /// What kind of exchange.
    pub kind: NetKind,
    /// The name shown: the bot's, the device's, or `oxbot`.
    pub who: String,
    /// The bot's id, if a bot did it.
    #[serde(default)]
    pub bot: Option<String>,
    /// The person's display name, if it was for someone.
    #[serde(default)]
    pub person: Option<String>,
    /// Where it went, e.g. `openrouter.ai`, `loblaws.ca`; for a device,
    /// the network it was on.
    pub host: String,
    /// What it was, in plain words, e.g. `DeepSeek V4.1 Flash`,
    /// `read 2 pages`, `connected on Home Wi-Fi`.
    pub label: String,
    /// Bytes sent.
    pub bytes_up: u64,
    /// Bytes received.
    pub bytes_down: u64,
    /// What it cost, in US dollars, if it costs anything.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// Personal details swapped for stand-ins before it left, by kind
    /// (`email`, `phone`, `person`…) → how many; empty if none. Never the
    /// details themselves.
    #[serde(default)]
    pub swapped: BTreeMap<String, u32>,
    /// How long it took (a device: how long it stayed connected).
    #[serde(default)]
    pub ms: u64,
    /// `ok`, or a short plain reason it didn't go well (a device still
    /// connected: `connected`).
    #[serde(default)]
    pub outcome: String,
    /// Websites: how many other sites the pages pulled from.
    #[serde(default)]
    pub other_hosts: u32,
}

/// Today's sums (since local midnight), over the rows the viewer may see
/// of the kind asked for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetTotals {
    /// Rows today.
    pub today_rows: u32,
    /// Bytes sent today.
    pub today_up: u64,
    /// Bytes received today.
    pub today_down: u64,
    /// Spent today, in US dollars.
    pub today_cost_usd: f64,
}
