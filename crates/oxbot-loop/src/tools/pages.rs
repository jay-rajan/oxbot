//! What a session knows about its browser's tabs: the page each tab id
//! shows (from every browser result, whatever form it names the page in),
//! which tab is selected, the last snapshot of each tab (the default for
//! the page tools) and what the model was last shown of it (the baseline a
//! new snapshot is compared with).

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use crate::page::{self, Baseline, Snapshot, Trimmed};

/// Parsed pages kept for the page tools.
const CACHE: usize = 4;

/// One tab's page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabPage {
    /// Its address.
    pub url: String,
    /// Its title (may be empty).
    pub title: String,
}

/// A stored page, parsed and trimmed once.
#[derive(Debug)]
pub struct ParsedPage {
    /// The snapshot.
    pub snapshot: Snapshot,
    /// Its trimmed tree.
    pub trimmed: Trimmed,
}

/// A session's tabs.
#[derive(Debug, Default)]
pub struct PageTracker {
    tabs: BTreeMap<u32, TabPage>,
    selected: Option<u32>,
    /// The last page named when no tab id was given.
    last_url: Option<String>,
    /// Tab key → what the model was last shown of it.
    baselines: BTreeMap<String, Baseline>,
    /// The latest snapshot of the selected tab.
    latest: Option<String>,
    cache: VecDeque<(String, Arc<ParsedPage>)>,
}

impl PageTracker {
    /// Learns what `text` (a browser tool's result) says about the pages.
    pub fn learn(&mut self, text: &str) {
        let news = page::page_news(text);
        if let Some((id, url, title)) = news.selected {
            self.selected = Some(id);
            self.tabs.insert(
                id,
                TabPage {
                    url: url.clone(),
                    title,
                },
            );
            self.last_url = Some(url);
        }
        if let Some(url) = news.navigated_to {
            if let Some(id) = self.selected {
                let tab = self.tabs.entry(id).or_default();
                if tab.url != url {
                    tab.url.clone_from(&url);
                    tab.title.clear();
                }
            }
            self.last_url = Some(url);
        }
    }

    /// The selected tab's page, as last known.
    #[must_use]
    pub fn current(&self) -> Option<TabPage> {
        self.selected
            .and_then(|id| self.tabs.get(&id).cloned())
            .or_else(|| {
                self.last_url.clone().map(|url| TabPage {
                    url,
                    title: String::new(),
                })
            })
    }

    /// The key of the selected tab showing `url`: its id and the address
    /// without a fragment.
    #[must_use]
    pub fn tab_key(&self, url: &str) -> String {
        let url = url.split('#').next().unwrap_or(url);
        format!("{} {url}", self.selected.unwrap_or(0))
    }

    /// What the model was last shown of tab `key`.
    #[must_use]
    pub fn baseline(&self, key: &str) -> Option<&Baseline> {
        self.baselines.get(key)
    }

    /// Notes what the model was shown in full (or as a map) of tab `key`.
    pub fn shown(&mut self, key: &str, baseline: Baseline) {
        self.baselines.insert(key.to_owned(), baseline);
    }

    /// Notes the newest snapshot, the page tools' default.
    pub fn latest_is(&mut self, handle: &str) {
        self.latest = Some(handle.to_owned());
    }

    /// The latest snapshot of the selected tab (the page tools' default).
    #[must_use]
    pub fn latest(&self) -> Option<&str> {
        self.latest.as_deref()
    }

    /// A parsed page, if it's one of the last few used.
    #[must_use]
    pub fn cached(&self, handle: &str) -> Option<Arc<ParsedPage>> {
        self.cache
            .iter()
            .find(|(h, _)| h == handle)
            .map(|(_, p)| Arc::clone(p))
    }

    /// Keeps a parsed page for the next page-tool call.
    pub fn keep(&mut self, handle: &str, page: Arc<ParsedPage>) {
        self.cache.retain(|(h, _)| h != handle);
        self.cache.push_back((handle.to_owned(), page));
        while self.cache.len() > CACHE {
            self.cache.pop_front();
        }
    }
}
