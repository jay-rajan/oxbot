//! [`CookieMarks`]: which cookies a browser holds, per site, to tell
//! whether a sign-in window left new or changed ones behind; and
//! [`site_cookie_params`]: one site's cookies as another browser takes
//! them, to share a sign-in.

use std::collections::BTreeMap;
use std::hash::{BuildHasher, RandomState};
use std::sync::OnceLock;

use serde_json::Value;

use crate::site_of;

/// The cookies a browser holds, by site (main domain): each one's name,
/// domain and path, with a keyed hash of its value and expiry. Never the
/// value itself: the hash only tells whether it changed, and its key
/// lives only in this process's memory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CookieMarks(BTreeMap<String, BTreeMap<String, u64>>);

/// A random key, the same for every comparison in this process, so a
/// hash can't be matched against guessed values elsewhere.
fn value_key() -> &'static RandomState {
    static KEY: OnceLock<RandomState> = OnceLock::new();
    KEY.get_or_init(RandomState::new)
}

/// The keyed hash of a cookie's value and expiry.
fn value_hash(cookie: &Value) -> u64 {
    let value = cookie.get("value").and_then(Value::as_str).unwrap_or("");
    let expires = cookie
        .get("expires")
        .map(Value::to_string)
        .unwrap_or_default();
    value_key().hash_one((value, expires))
}

impl CookieMarks {
    /// From Chrome's `Storage.getCookies` answer.
    #[must_use]
    pub fn from_cookies(cookies: &[Value]) -> Self {
        let mut marks: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for cookie in cookies {
            let field = |name: &str| cookie.get(name).and_then(Value::as_str).unwrap_or("");
            let site = site_of(field("domain"));
            if site.is_empty() {
                continue;
            }
            let mark = format!("{}\t{}\t{}", field("name"), field("domain"), field("path"));
            marks
                .entry(site)
                .or_default()
                .insert(mark, value_hash(cookie));
        }
        Self(marks)
    }

    /// The sites with cookies, sorted.
    #[must_use]
    pub fn sites(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    /// Whether there are cookies for `site` (a main domain).
    #[must_use]
    pub fn has(&self, site: &str) -> bool {
        self.0.get(site).is_some_and(|c| !c.is_empty())
    }

    /// Whether `site` has a cookie now that it didn't have in `before`.
    #[must_use]
    pub fn gained_since(&self, before: &Self, site: &str) -> bool {
        let Some(now) = self.0.get(site) else {
            return false;
        };
        match before.0.get(site) {
            Some(then) => now.keys().any(|mark| !then.contains_key(mark)),
            None => !now.is_empty(),
        }
    }

    /// Whether `site` has a cookie now that it didn't have in `before`, or
    /// one whose value or expiry changed (signing in often replaces a
    /// session cookie the site set on an earlier visit).
    #[must_use]
    pub fn changed_since(&self, before: &Self, site: &str) -> bool {
        if self.gained_since(before, site) {
            return true;
        }
        let (Some(now), Some(then)) = (self.0.get(site), before.0.get(site)) else {
            return false;
        };
        now.iter()
            .any(|(mark, hash)| then.get(mark).is_some_and(|old| old != hash))
    }
}

/// The fields of a cookie Chrome's `Storage.setCookies` takes
/// (`CookieParam`); the rest of what `Storage.getCookies` gives (size,
/// session, …) it doesn't.
const PARAM_FIELDS: [&str; 12] = [
    "name",
    "value",
    "domain",
    "path",
    "secure",
    "httpOnly",
    "sameSite",
    "priority",
    "sameParty",
    "sourceScheme",
    "sourcePort",
    "partitionKey",
];

/// The cookies of `site` (a main domain) and its subdomains from
/// `cookies` (Chrome's `Storage.getCookies` answer), as
/// `Storage.setCookies` takes them, to sign another browser in to the
/// same site. A session cookie stays one (no expiry). Values are copied,
/// never logged.
#[must_use]
pub fn site_cookie_params(cookies: &[Value], site: &str) -> Vec<Value> {
    let site = site_of(site);
    cookies
        .iter()
        .filter(|c| {
            let domain = c.get("domain").and_then(Value::as_str).unwrap_or("");
            !domain.is_empty() && site_of(domain) == site
        })
        .map(|c| {
            let mut param = serde_json::Map::new();
            for field in PARAM_FIELDS {
                if let Some(v) = c.get(field) {
                    param.insert(field.to_owned(), v.clone());
                }
            }
            let session = c.get("session").and_then(Value::as_bool).unwrap_or(false);
            if let Some(expires) = c.get("expires").and_then(Value::as_f64)
                && !session
                && expires > 0.0
            {
                param.insert("expires".to_owned(), expires.into());
            }
            Value::Object(param)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn marks(cookies: &[(&str, &str)]) -> CookieMarks {
        let cookies: Vec<Value> = cookies
            .iter()
            .map(|(name, domain)| json!({ "name": name, "domain": domain, "path": "/", "value": "v" }))
            .collect();
        CookieMarks::from_cookies(&cookies)
    }

    #[test]
    fn cookies_are_grouped_by_main_domain() {
        let m = marks(&[
            ("sid", ".google.com"),
            ("x", "accounts.google.com"),
            ("a", "bank.example"),
        ]);
        assert_eq!(m.sites(), ["bank.example", "google.com"]);
        assert!(m.has("google.com") && !m.has("example.com"));
    }

    #[test]
    fn only_a_new_cookie_counts_as_gained() {
        let before = marks(&[("tracker", ".news.example")]);
        assert!(
            !before.gained_since(&before, "news.example"),
            "the same cookies"
        );
        let same_name_new_value = CookieMarks::from_cookies(&[
            json!({ "name": "tracker", "domain": ".news.example", "path": "/", "value": "other" }),
        ]);
        assert!(!same_name_new_value.gained_since(&before, "news.example"));
        let signed_in = marks(&[("tracker", ".news.example"), ("session", "news.example")]);
        assert!(signed_in.gained_since(&before, "news.example"));
        assert!(signed_in.gained_since(&CookieMarks::default(), "news.example"));
        assert!(!CookieMarks::default().gained_since(&before, "news.example"));
        assert!(!signed_in.gained_since(&before, "other.example"));
    }

    #[test]
    fn a_changed_value_or_expiry_counts_as_changed_but_never_keeps_the_value() {
        let cookie = |value: &str, expires: f64| {
            CookieMarks::from_cookies(&[
                json!({ "name": "sid", "domain": ".bank.example", "path": "/", "value": value, "expires": expires }),
            ])
        };
        let before = cookie("guest", 100.0);
        assert!(!cookie("guest", 100.0).changed_since(&before, "bank.example"));
        assert!(cookie("signed-in", 100.0).changed_since(&before, "bank.example"));
        assert!(cookie("guest", 200.0).changed_since(&before, "bank.example"));
        assert!(!cookie("signed-in", 100.0).changed_since(&before, "other.example"));
        assert!(!CookieMarks::default().changed_since(&before, "bank.example"));
        let added = marks(&[("sid", ".bank.example"), ("new", ".bank.example")]);
        assert!(added.changed_since(&marks(&[("sid", ".bank.example")]), "bank.example"));
        assert!(!format!("{before:?}").contains("guest"), "no value kept");
    }

    #[test]
    fn a_sites_cookies_are_copied_as_chrome_takes_them() {
        let cookies = [
            json!({ "name": "sid", "domain": ".bank.example", "path": "/", "value": "s3cret",
                    "expires": 2.0e9, "size": 9, "httpOnly": true, "secure": true,
                    "session": false, "sameSite": "Lax", "priority": "Medium" }),
            json!({ "name": "tmp", "domain": "login.bank.example", "path": "/a", "value": "1",
                    "expires": -1, "size": 4, "session": true }),
            json!({ "name": "g", "domain": ".google.com", "path": "/", "value": "x" }),
        ];
        let params = site_cookie_params(&cookies, "www.bank.example");
        assert_eq!(params.len(), 2, "only the site's");
        assert_eq!(
            params[0],
            json!({ "name": "sid", "domain": ".bank.example", "path": "/", "value": "s3cret",
                    "expires": 2.0e9, "httpOnly": true, "secure": true, "sameSite": "Lax",
                    "priority": "Medium" })
        );
        assert_eq!(
            params[1],
            json!({ "name": "tmp", "domain": "login.bank.example", "path": "/a", "value": "1" }),
            "a session cookie stays one"
        );
        assert_eq!(
            site_cookie_params(&cookies, "other.example"),
            Vec::<Value>::new()
        );
    }
}
