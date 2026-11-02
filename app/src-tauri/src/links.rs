//! Links tapped in the app's window: the app's own page stays in the
//! window; web pages (a report's sources, links in a chat) open in the
//! system browser, so the window never leaves oxbot's page; anything else
//! is blocked.

use tauri::Url;

/// What to do with a navigation or a new window the page asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// The app's own page, or a frame's own document: load it here.
    Stay,
    /// A web page: open it in the system browser, not here.
    Outside(String),
    /// Anything else (`file:`, `javascript:`, other apps' schemes).
    Block,
}

/// Decides for `url`, where `own` is the app page's own address (its
/// scheme, host and port count).
#[must_use]
pub fn decide(url: &Url, own: &Url) -> Link {
    if url.scheme() == "about" {
        return Link::Stay;
    }
    if url.origin() == own.origin() {
        return Link::Stay;
    }
    if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() {
        return Link::Outside(url.as_str().to_owned());
    }
    Link::Block
}

/// Opens `url` (an http(s) address, from [`decide`]) in the Mac's default
/// browser.
#[cfg(target_os = "macos")]
pub fn open_outside(url: &str) {
    // `open` gets the address as one argument, no shell in between.
    if let Err(err) = std::process::Command::new("/usr/bin/open")
        .arg("--")
        .arg(url)
        .spawn()
    {
        tracing::warn!(%err, "couldn't open a link in the browser");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn only_the_apps_own_page_loads_in_the_window() {
        let mac = url("http://127.0.0.1:7878/");
        assert_eq!(decide(&url("http://127.0.0.1:7878/#k=1"), &mac), Link::Stay);
        assert_eq!(decide(&url("about:srcdoc#src-1"), &mac), Link::Stay);
        assert_eq!(decide(&url("about:blank"), &mac), Link::Stay);
        assert_eq!(
            decide(&url("https://cafe-a.example.com/menu"), &mac),
            Link::Outside("https://cafe-a.example.com/menu".into())
        );
        assert_eq!(
            decide(&url("http://127.0.0.1:9999/"), &mac),
            Link::Outside("http://127.0.0.1:9999/".into()),
            "another port is another site"
        );
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "intent://x#Intent;end",
            "tel:123",
        ] {
            assert_eq!(decide(&url(bad), &mac), Link::Block, "{bad}");
        }
        let phone = url("http://tauri.localhost/gate.html");
        assert_eq!(decide(&url("http://tauri.localhost/x"), &phone), Link::Stay);
        assert_eq!(
            decide(&url("https://example.com/"), &phone),
            Link::Outside("https://example.com/".into())
        );
    }
}
