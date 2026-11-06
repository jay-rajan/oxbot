//! `read_pages`' reading, done by the hub on the bot's own browser: the
//! engine opens each page through the browser tool (and the gate), then
//! asks the hub with [`SealedCall`] to copy the page and run the bot's
//! script on the copy in a sealed tab (no network, no cookies, no page
//! scripts). The bot never gets a script into a live page.

use serde_json::Value;

use crate::reply::{Refusal, Reply};

/// What the hub is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealedWork {
    /// Copy the page the bot's browser shows at `url` and run `script` (a
    /// function body that gets `doc` and `url`) on the copy in the sealed
    /// tab. With `follow`, also find the next page's address on the copy:
    /// the first link matching it as a CSS selector, else by its text.
    Read {
        /// The page, as the browser tool last named it.
        url: String,
        /// The bot's extractor.
        script: String,
        /// A selector or link text for the next page, if following.
        follow: Option<String>,
    },
    /// The run is over: close the sealed tab.
    Done,
}

/// What the hub found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SealedReply {
    /// Whether it worked.
    pub ok: bool,
    /// What the script returned (JSON), when it worked.
    pub value: Value,
    /// Why not, in plain words, when it didn't.
    pub error: String,
    /// The next page's address (with `follow`), if the copy has one.
    pub next: Option<String>,
    /// The page was bigger than oxbot copies: only its start was read.
    pub cut: bool,
}

impl SealedReply {
    /// A failure with `why`.
    #[must_use]
    pub fn failed(why: impl Into<String>) -> Self {
        Self {
            error: why.into(),
            ..Self::default()
        }
    }
}

impl Refusal for SealedReply {
    fn refusal() -> Self {
        Self::failed("oxbot couldn't read this page")
    }
}

/// One request to the hub. Dropping `reply` unanswered is a failure.
#[derive(Debug)]
pub struct SealedCall {
    /// The bot's turn's work.
    pub work: SealedWork,
    /// Where the answer goes.
    pub reply: Reply<SealedReply>,
}
