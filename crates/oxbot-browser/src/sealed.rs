//! Sealed script reads: where a bot's `read_pages` script ran on a copy of
//! a page in a network-less tab. That tab was built on Chrome's `DevTools`
//! and has no Camoufox equivalent yet, so [`crate::Browser::read_sealed`]
//! refuses (see [`crate::NO_SEALED_READS`]). These are the shapes the hub
//! keeps using.

use serde_json::Value;

/// A page as oxbot copied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copied {
    /// Its HTML.
    pub html: String,
    /// It was longer: only the start was copied.
    pub cut: bool,
    /// The address the browser says the tab shows.
    pub url: String,
}

/// What the script made of one page.
#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    /// What it returned.
    pub value: Value,
    /// The next page's address, when following and the copy has one.
    pub next: Option<String>,
}
