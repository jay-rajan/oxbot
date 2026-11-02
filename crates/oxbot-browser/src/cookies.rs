//! [`CookieMarks`]: which cookies a browser holds, per site, to tell
//! whether a sign-in window left new ones behind.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::site_of;

/// The cookies a browser holds, by site (main domain): each one's name,
/// domain and path, never its value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CookieMarks(BTreeMap<String, BTreeSet<String>>);

impl CookieMarks {
    /// From Chrome's `Storage.getCookies` answer.
    #[must_use]
    pub fn from_cookies(cookies: &[Value]) -> Self {
        let mut marks: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for cookie in cookies {
            let field = |name: &str| cookie.get(name).and_then(Value::as_str).unwrap_or("");
            let site = site_of(field("domain"));
            if site.is_empty() {
                continue;
            }
            let mark = format!("{}\t{}\t{}", field("name"), field("domain"), field("path"));
            marks.entry(site).or_default().insert(mark);
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
            Some(then) => now.difference(then).next().is_some(),
            None => !now.is_empty(),
        }
    }
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
}
