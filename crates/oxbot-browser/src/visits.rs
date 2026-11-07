//! A bot's visits to websites, for Settings → Activity: what its browser
//! sent and got, per site.
//!
//! The guard (oxbot's own `DevTools` connection, see `guard.rs`) turns on
//! `Network` events in each tab and feeds them here as [`NetEvent`]s. A
//! visit is one tab's time on one site (the registrable domain of its top
//! page, see [`crate::site_of`]): it counts the pages opened there, the
//! bytes of every request made meanwhile (other sites' scripts, images and
//! trackers are folded in, and counted as other sites), and ends when the
//! tab moves to another site, closes, goes blank, or sits idle, or when
//! someone else starts using the browser.
//!
//! Bytes sent are an estimate (the address, headers and body of each
//! request); bytes received are what Chrome says came over the network
//! (`encodedDataLength`). Requests from frames or workers in their own
//! process, which the guard doesn't attach to, aren't seen.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use crate::browser::site_of;
use crate::web::{is_home_network, web_host};

/// How long a visit may go without a request before it's written.
pub const IDLE_MS: i64 = 120_000;
/// The longest request body `DevTools` is asked to include (a bigger one
/// counts as this much).
pub(crate) const MAX_POST_DATA: u64 = 65_536;

/// Where finished visits go (the hub writes them down).
pub type VisitSink = Arc<dyn Fn(Visit) + Send + Sync>;

/// What a browser's guard needs to count visits: where they go, and who
/// is using the browser now.
#[derive(Clone)]
pub(crate) struct Watch {
    pub(crate) sink: VisitSink,
    pub(crate) user: Arc<Mutex<Option<String>>>,
}

impl Watch {
    pub(crate) fn user(&self) -> Option<String> {
        self.user
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn send(&self, visits: Vec<Visit>) {
        for visit in visits {
            (self.sink)(visit);
        }
    }
}

/// One finished visit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    /// Who was using the browser when it started (the hub's tag: a bot id),
    /// if anyone.
    pub user: Option<String>,
    /// The site, e.g. `loblaws.ca`.
    pub site: String,
    /// When its first page started loading (ms since the Unix epoch).
    pub started_ms: i64,
    /// When its last request was seen.
    pub last_ms: i64,
    /// Pages opened on the site.
    pub pages: u32,
    /// Bytes sent (estimated: addresses, headers and bodies).
    pub bytes_up: u64,
    /// Bytes received.
    pub bytes_down: u64,
    /// Other sites its pages pulled from.
    pub other_sites: u32,
}

/// What the guard saw, as the visit counter needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetEvent<'a> {
    /// A request went out from tab `tab`.
    Request {
        /// The tab (its `DevTools` target id).
        tab: &'a str,
        /// Its address.
        url: &'a str,
        /// It loads the tab's top page (a navigation, or its redirect).
        top: bool,
        /// It's a redirect of the request before it.
        redirect: bool,
        /// Bytes it sent.
        up: u64,
    },
    /// Bytes came in for a request of tab `tab`.
    Received {
        /// The tab.
        tab: &'a str,
        /// How many.
        bytes: u64,
    },
    /// Tab `tab` closed.
    TabGone {
        /// The tab.
        tab: &'a str,
    },
}

#[derive(Debug)]
struct Open {
    visit: Visit,
    others: BTreeSet<String>,
}

impl Open {
    fn done(self) -> Visit {
        let mut visit = self.visit;
        visit.other_sites = u32::try_from(self.others.len()).unwrap_or(u32::MAX);
        visit
    }
}

/// Each tab's visit under way.
#[derive(Debug, Default)]
pub struct Visits {
    open: HashMap<String, Open>,
}

impl Visits {
    /// Takes one event seen at `now_ms` while `user` uses the browser;
    /// returns the visits it ended.
    pub fn on(&mut self, event: &NetEvent<'_>, now_ms: i64, user: Option<&str>) -> Vec<Visit> {
        let mut done = Vec::new();
        match *event {
            NetEvent::Request {
                tab,
                url,
                top,
                redirect,
                up,
            } => {
                // This Mac and the home network aren't the outside world.
                let host = web_host(url).filter(|_| !is_home_network(url));
                if host.is_none() && !top {
                    return done;
                }
                if top {
                    let site = host.as_deref().map(site_of);
                    let same = self
                        .open
                        .get(tab)
                        .is_some_and(|o| Some(&o.visit.site) == site.as_ref());
                    if !same {
                        done.extend(self.open.remove(tab).map(Open::done));
                        // A blank or local page ends the visit and starts none.
                        let Some(site) = site else {
                            return done;
                        };
                        self.open
                            .insert(tab.to_owned(), Open::new(site, now_ms, user));
                    }
                }
                let Some(open) = self.open.get_mut(tab) else {
                    return done;
                };
                if top && (!redirect || open.visit.pages == 0) {
                    open.visit.pages += 1;
                }
                open.visit.bytes_up += up;
                open.visit.last_ms = now_ms;
                if let Some(host) = host {
                    let site = site_of(&host);
                    if site != open.visit.site {
                        open.others.insert(site);
                    }
                }
            }
            NetEvent::Received { tab, bytes } => {
                if let Some(open) = self.open.get_mut(tab) {
                    open.visit.bytes_down += bytes;
                    open.visit.last_ms = now_ms;
                }
            }
            NetEvent::TabGone { tab } => done.extend(self.open.remove(tab).map(Open::done)),
        }
        done
    }

    /// Ends the visits idle for [`IDLE_MS`], and those started by someone
    /// other than `user` (the browser changed hands).
    pub fn tick(&mut self, now_ms: i64, user: Option<&str>) -> Vec<Visit> {
        let ended: Vec<String> = self
            .open
            .iter()
            .filter(|(_, o)| now_ms - o.visit.last_ms >= IDLE_MS || o.visit.user.as_deref() != user)
            .map(|(tab, _)| tab.clone())
            .collect();
        ended
            .into_iter()
            .filter_map(|tab| self.open.remove(&tab).map(Open::done))
            .collect()
    }

    /// Ends every visit (the browser is closing).
    pub fn end_all(&mut self) -> Vec<Visit> {
        self.open.drain().map(|(_, o)| o.done()).collect()
    }
}

impl Open {
    fn new(site: String, now_ms: i64, user: Option<&str>) -> Self {
        Self {
            visit: Visit {
                user: user.map(str::to_owned),
                site,
                started_ms: now_ms,
                last_ms: now_ms,
                pages: 0,
                bytes_up: 0,
                bytes_down: 0,
                other_sites: 0,
            },
            others: BTreeSet::new(),
        }
    }
}

/// Bytes a request sends, as `DevTools` describes it in
/// `Network.requestWillBeSent`'s `request`: its first line, headers and
/// body. A body too big for `DevTools` to include counts as `max_body`.
#[must_use]
pub fn request_bytes(request: &serde_json::Value, max_body: u64) -> u64 {
    let len = |v: &serde_json::Value| v.as_str().map_or(0, str::len) as u64;
    let line = len(&request["method"]) + len(&request["url"]) + 12;
    let headers: u64 = request["headers"].as_object().map_or(0, |h| {
        h.iter().map(|(k, v)| k.len() as u64 + len(v) + 4).sum()
    });
    let body = match request.get("postData") {
        Some(data) => len(data),
        None if request["hasPostData"].as_bool() == Some(true) => max_body,
        None => 0,
    };
    line + headers + body
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Vec<Visit> = Vec::new();

    fn request<'a>(tab: &'a str, url: &'a str, top: bool, up: u64) -> NetEvent<'a> {
        NetEvent::Request {
            tab,
            url,
            top,
            redirect: false,
            up,
        }
    }

    #[test]
    fn a_visit_sums_its_pages_and_every_request_while_it_lasts() {
        let mut v = Visits::default();
        let bot = Some("scout");
        assert_eq!(
            v.on(
                &request("t1", "https://www.loblaws.ca/", true, 100),
                1_000,
                bot
            ),
            NONE
        );
        v.on(
            &request("t1", "https://cdn.loblaws.ca/app.js", false, 50),
            1_100,
            bot,
        );
        v.on(
            &request("t1", "https://www.googletagmanager.com/gtm.js", false, 60),
            1_200,
            bot,
        );
        v.on(
            &request("t1", "https://px.ads.example.com/p", false, 10),
            1_250,
            bot,
        );
        v.on(
            &NetEvent::Received {
                tab: "t1",
                bytes: 5_000,
            },
            1_300,
            bot,
        );
        // Another page on the same site is the same visit.
        v.on(
            &request("t1", "https://www.loblaws.ca/deals", true, 120),
            2_000,
            bot,
        );
        v.on(
            &NetEvent::Received {
                tab: "t1",
                bytes: 7_000,
            },
            2_100,
            bot,
        );
        // Moving to another site ends it.
        let done = v.on(&request("t1", "https://example.org/", true, 90), 3_000, bot);
        assert_eq!(
            done,
            [Visit {
                user: Some("scout".into()),
                site: "loblaws.ca".into(),
                started_ms: 1_000,
                last_ms: 2_100,
                pages: 2,
                bytes_up: 340,
                bytes_down: 12_000,
                other_sites: 2,
            }]
        );
        let rest = v.end_all();
        assert_eq!(rest.len(), 1);
        assert_eq!(
            (rest[0].site.as_str(), rest[0].pages, rest[0].bytes_up),
            ("example.org", 1, 90)
        );
    }

    #[test]
    fn redirects_on_one_site_are_one_page_and_across_sites_move_the_visit() {
        let mut v = Visits::default();
        v.on(&request("t1", "http://loblaws.ca/", true, 10), 0, None);
        let redirect = |url| NetEvent::Request {
            tab: "t1",
            url,
            top: true,
            redirect: true,
            up: 10,
        };
        assert_eq!(v.on(&redirect("https://www.loblaws.ca/"), 1, None), NONE);
        let done = v.on(&redirect("https://accounts.example.com/login"), 2, None);
        assert_eq!((done[0].pages, done[0].bytes_up), (1, 20));
        let rest = v.end_all();
        assert_eq!((rest[0].site.as_str(), rest[0].pages), ("example.com", 1));
    }

    #[test]
    fn tabs_are_counted_apart_and_end_when_closed_blank_idle_or_handed_over() {
        let mut v = Visits::default();
        let bot = Some("scout");
        v.on(&request("t1", "https://a.example.com/", true, 1), 0, bot);
        v.on(&request("t2", "https://b.example.net/", true, 1), 0, bot);
        v.on(&request("t3", "https://c.example.org/", true, 1), 0, bot);
        v.on(&request("t4", "https://d.example.edu/", true, 1), 0, bot);
        let gone = v.on(&NetEvent::TabGone { tab: "t1" }, 10, bot);
        assert_eq!(gone[0].site, "example.com");
        let blank = v.on(&request("t2", "about:blank", true, 0), 10, bot);
        assert_eq!(blank[0].site, "example.net");
        // The home network isn't a visit either.
        assert_eq!(
            v.on(&request("t2", "http://192.168.1.1/", true, 9), 10, bot),
            NONE
        );
        // Nothing open in t2 now: its requests count nowhere.
        assert_eq!(
            v.on(&request("t2", "https://x.test/a.js", false, 5), 11, bot),
            NONE
        );
        v.on(
            &NetEvent::Received {
                tab: "t3",
                bytes: 1,
            },
            IDLE_MS - 1,
            bot,
        );
        let idle = v.tick(IDLE_MS, bot);
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].site, "example.edu");
        // Someone else has the browser now.
        let handed = v.tick(IDLE_MS + 1, Some("other"));
        assert_eq!(handed[0].site, "example.org");
        assert_eq!(v.end_all(), NONE);
    }

    #[test]
    fn request_sizes_count_the_line_headers_and_body() {
        let req = serde_json::json!({
            "method": "POST", "url": "https://a.test/x",
            "headers": {"Accept": "*/*"}, "postData": "hello"
        });
        assert_eq!(request_bytes(&req, 1_000), (4 + 16 + 12) + (6 + 3 + 4) + 5);
        let big = serde_json::json!({"method": "POST", "url": "", "hasPostData": true});
        assert_eq!(request_bytes(&big, 1_000), 4 + 12 + 1_000);
    }
}
