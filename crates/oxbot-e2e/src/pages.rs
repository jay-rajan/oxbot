//! Big fake pages for the stand-ins' scripts (`big = { … }` in a reply):
//! a results page as chrome-devtools-mcp 1.10.1 prints it, with the noise
//! real pages have (unnamed wrappers, text boxes under text, focusable
//! companions), so the raw snapshot is several times what oxbot shows a
//! model once trimmed. Items come in up to seven shapes in turn, so they
//! don't all fold into "more like the above". Example.com sites only.

use std::fmt::Write as _;

use serde::Deserialize;

/// What big pages a reply reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BigPages {
    /// How many pages (`?page=1` … when more than one).
    #[serde(default = "one")]
    pub pages: usize,
    /// Results on each page.
    pub items: usize,
    /// Item shapes, 1 to 7 (1: every item alike, so they fold).
    #[serde(default = "seven")]
    pub shapes: usize,
    /// The address.
    #[serde(default = "default_url")]
    pub url: String,
    /// The title.
    #[serde(default = "default_title")]
    pub title: String,
}

fn one() -> usize {
    1
}

fn seven() -> usize {
    7
}

fn default_url() -> String {
    "https://shop.example.com/list".to_owned()
}

fn default_title() -> String {
    "Lamps".to_owned()
}

impl BigPages {
    /// Every page's snapshot, in order.
    #[must_use]
    pub fn snapshots(&self) -> Vec<String> {
        (1..=self.pages.max(1)).map(|k| self.page(k)).collect()
    }

    /// Page `k`'s address.
    #[must_use]
    pub fn url_of(&self, k: usize) -> String {
        if self.pages > 1 {
            format!("{}?page={k}", self.url)
        } else {
            self.url.clone()
        }
    }

    fn page(&self, k: usize) -> String {
        let url = self.url_of(k);
        let title = if self.pages > 1 {
            format!("{} {k}", self.title)
        } else {
            self.title.clone()
        };
        let snap = k + 1;
        let mut n = 0usize;
        let mut uid = || {
            n += 1;
            format!("{snap}_{n}")
        };
        let mut s = format!(
            "## Pages\n1: {title} ({url}) [selected]\n## Latest page snapshot\nuid={snap}_0 RootWebArea \"{title}\" url=\"{url}\"\n"
        );
        let _ = writeln!(s, "  uid={} banner", uid());
        let _ = writeln!(
            s,
            "    uid={} link \"Shop home\" focusable url=\"https://shop.example.com/\"",
            uid()
        );
        let _ = writeln!(s, "    uid={} searchbox \"Search\" focusable", uid());
        let _ = writeln!(s, "  uid={} main", uid());
        let _ = writeln!(s, "    uid={} heading \"{title}\" level=\"1\"", uid());
        let _ = writeln!(s, "    uid={} list \"Results\"", uid());
        let shapes = self.shapes.clamp(1, 7);
        for i in 0..self.items {
            item(&mut s, k * 1000 + i, i, i % shapes, &mut uid);
        }
        let _ = writeln!(s, "  uid={} contentinfo", uid());
        let _ = writeln!(
            s,
            "    uid={} link \"Help\" focusable url=\"https://shop.example.com/help\"",
            uid()
        );
        s
    }
}

/// One result item of `shape` (0 to 6), with uids from `uid`.
fn item(s: &mut String, item: usize, i: usize, shape: usize, uid: &mut impl FnMut() -> String) {
    let name = format!("Lamp {item} with a brass arm and a linen shade");
    let price = format!("${}.{:02}", 10 + item % 90, item % 100);
    let _ = writeln!(s, "      uid={} listitem", uid());
    let _ = writeln!(s, "        uid={} generic", uid());
    // A link's name shows again as its text, as real pages print it.
    let link = |s: &mut String, uid: String| {
        let _ = writeln!(
            s,
            "          uid={uid} link \"{name}\" focusable url=\"https://shop.example.com/p/{item}?ref=list&pos={i}\""
        );
        let _ = writeln!(s, "            uid={uid}_t StaticText \"{name}\"");
        let _ = writeln!(s, "              uid={uid}_b InlineTextBox \"{name}\"");
    };
    let text = |s: &mut String, uid: String, box_uid: String, t: &str| {
        let _ = writeln!(s, "          uid={uid} StaticText \"{t}\"");
        let _ = writeln!(s, "            uid={box_uid} InlineTextBox \"{t}\"");
    };
    match shape {
        0 => {
            link(s, uid());
            text(s, uid(), uid(), &price);
        }
        1 => {
            link(s, uid());
            let _ = writeln!(
                s,
                "          uid={} button \"Add lamp {item}\" focusable",
                uid()
            );
        }
        2 => {
            let _ = writeln!(
                s,
                "          uid={} heading \"Lamp {item}\" level=\"3\"",
                uid()
            );
            link(s, uid());
        }
        3 => {
            link(s, uid());
            let _ = writeln!(s, "          uid={} image \"Photo of lamp {item}\"", uid());
            text(s, uid(), uid(), &price);
        }
        4 => {
            link(s, uid());
            text(
                s,
                uid(),
                uid(),
                &format!("{price}, {}.5 out of 5 stars", item % 5),
            );
            let _ = writeln!(
                s,
                "          uid={} button \"Compare lamp {item}\" focusable",
                uid()
            );
        }
        5 => {
            text(s, uid(), uid(), &format!("New: {price}"));
            link(s, uid());
        }
        _ => {
            let _ = writeln!(
                s,
                "          uid={} checkbox \"Keep lamp {item}\" checkable focusable",
                uid()
            );
            link(s, uid());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_big_distinct_and_stable() {
        let big = BigPages {
            pages: 2,
            items: 80,
            shapes: 7,
            url: default_url(),
            title: default_title(),
        };
        let pages = big.snapshots();
        assert_eq!(pages.len(), 2);
        assert!(pages[0].starts_with(
            "## Pages\n1: Lamps 1 (https://shop.example.com/list?page=1) [selected]\n"
        ));
        assert!(pages[1].contains("uid=3_0 RootWebArea \"Lamps 2\""));
        assert_eq!(big.snapshots(), pages);
    }

    /// What the long-task test needs: a page of about 10k tokens raw that
    /// oxbot shows whole once trimmed (under its 4,000-token cap); and a
    /// results page so big that it comes as a map.
    #[test]
    fn the_pages_trim_as_the_tests_need() {
        let est = |t: &str| t.len().div_ceil(3);
        let long = BigPages {
            pages: 1,
            items: 64,
            shapes: 7,
            url: default_url(),
            title: default_title(),
        };
        let raw = &long.snapshots()[0];
        let snap = oxbot_loop::page::Snapshot::parse(raw, &|_| 0).unwrap();
        let trimmed = snap.trim().est_tokens();
        assert!(
            (9_000..14_000).contains(&est(raw)),
            "raw {} trimmed {}",
            est(raw),
            snap.trim().est_tokens()
        );
        assert!((2_800..4_000).contains(&trimmed), "trimmed {trimmed}");
        let search = BigPages { items: 300, ..long };
        let raw = &search.snapshots()[0];
        let snap = oxbot_loop::page::Snapshot::parse(raw, &|_| 0).unwrap();
        assert!(snap.trim().est_tokens() > 4_000);
    }
}
