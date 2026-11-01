//! Small helpers for web addresses.

use url::Url;

/// The host of an `http(s)` address, lowercase, or `None` for anything else
/// (`file:`, `chrome:`, `javascript:`, `data:`, garbage).
#[must_use]
pub fn web_host(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.host_str().map(str::to_ascii_lowercase)
}

/// Name endings used only inside a home network (compared lowercased).
const LOCAL_NAME_ENDINGS: [&str; 5] = [".localhost", ".local", ".home.arpa", ".internal", ".lan"];

/// Whether `url` points at this Mac or the home network rather than the
/// web: `localhost`, `*.local`, names without a dot, and loopback, private,
/// link-local or unspecified addresses (IPv4 and IPv6). Bots never open
/// these: the oxbot page and Gate door live there, and so do routers and
/// other people's devices.
#[must_use]
pub fn is_home_network(url: &str) -> bool {
    use std::net::{IpAddr, Ipv6Addr};
    use url::Host;
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    let v6_local = |ip: Ipv6Addr| {
        ip.is_loopback()
            || ip.is_unspecified()
            || ip.is_unique_local()
            || ip.is_unicast_link_local()
            || ip
                .to_ipv4_mapped()
                .is_some_and(|v4| is_local_ip(IpAddr::V4(v4)))
    };
    match parsed.host() {
        Some(Host::Ipv4(ip)) => is_local_ip(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => v6_local(ip),
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost"
                || LOCAL_NAME_ENDINGS.iter().any(|end| name.ends_with(end))
                || !name.contains('.')
        }
        None => false,
    }
}

fn is_local_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                // 100.64.0.0/10: carrier-grade NAT and VPN overlays.
                || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xC0) == 64)
        }
        std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unspecified(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_mac_and_the_home_network_are_not_the_web() {
        for local in [
            "http://127.0.0.1:7878/",
            "http://localhost:7878/",
            "http://LOCALHOST./",
            "http://127.1/",
            "http://0x7f000001/",
            "http://0.0.0.0:7879/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://192.168.2.10:7879/",
            "http://10.0.0.1/",
            "http://172.16.5.4/",
            "http://169.254.1.1/",
            "http://jays-mac.local/",
            "http://router/",
            "http://printer.home.arpa/",
            "http://100.100.1.1/",
        ] {
            assert!(is_home_network(local), "{local}");
        }
        for web in [
            "https://news.ycombinator.com/",
            "https://8.8.8.8/",
            "https://example.com:7878/",
            "about:blank",
        ] {
            assert!(!is_home_network(web), "{web}");
        }
    }

    #[test]
    fn only_web_pages_have_a_host() {
        assert_eq!(
            web_host("https://Mail.Google.com/x").as_deref(),
            Some("mail.google.com")
        );
        assert_eq!(
            web_host("http://localhost:8790/").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            web_host("https://a.com@evil.io/").as_deref(),
            Some("evil.io")
        );
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "chrome://settings",
            "data:text/html,hi",
            "nope",
        ] {
            assert_eq!(web_host(bad), None, "{bad}");
        }
    }
}
