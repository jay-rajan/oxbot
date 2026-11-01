//! Which networks phones may use, and which one this Mac is on.
//!
//! A network is known by its router's hardware address (MAC): another
//! Wi-Fi can copy a network's name, not this. The list is a setting the
//! Farm changes in Settings → Gates ("Add this network"), kept in
//! `gate/door.json` with the name phones see.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use oxbot_protocol::DoorNetwork;
use serde::{Deserialize, Serialize};

/// Says which network the Mac is on now, if it can tell. Slow (it runs
/// system commands): call it off the async threads.
pub type Probe = Arc<dyn Fn() -> Option<DoorNetwork> + Send + Sync>;

/// A network phones may use, as saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    id: String,
    label: String,
}

/// The door's settings, saved in `door.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    networks: Vec<Saved>,
    /// The name phones see; `None` uses the Mac's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip)]
    file: PathBuf,
}

impl Settings {
    /// The settings in `dir`, or none yet.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let file = dir.join("door.json");
        let mut settings: Self = std::fs::read(&file)
            .ok()
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .map_err(|err| tracing::warn!(%err, "door.json is unreadable; starting over"))
                    .ok()
            })
            .unwrap_or_default();
        settings.file = file;
        settings
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::secret::write_private(&self.file, &json)
    }

    /// Whether phones may use the network with this id.
    #[must_use]
    pub fn allows(&self, id: &str) -> bool {
        self.networks.iter().any(|n| n.id == id)
    }

    /// Lets phones use `network`. Returns whether it was new.
    ///
    /// # Errors
    /// Fails if the settings can't be saved.
    pub fn add(&mut self, network: &DoorNetwork) -> std::io::Result<bool> {
        if self.allows(&network.id) {
            return Ok(false);
        }
        self.networks.push(Saved {
            id: network.id.clone(),
            label: network.label.clone(),
        });
        self.save()?;
        Ok(true)
    }

    /// Stops letting phones use network `id`. Returns whether it was listed.
    ///
    /// # Errors
    /// Fails if the settings can't be saved.
    pub fn remove(&mut self, id: &str) -> std::io::Result<bool> {
        let before = self.networks.len();
        self.networks.retain(|n| n.id != id);
        if self.networks.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    /// The name phones see, if the Farm picked one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Sets the name phones see (`None` or empty: the Mac's name).
    ///
    /// # Errors
    /// Fails if the settings can't be saved.
    pub fn set_name(&mut self, name: Option<&str>) -> std::io::Result<()> {
        self.name = name
            .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
            .map(|n| n.chars().take(60).collect::<String>())
            .filter(|n| !n.is_empty());
        self.save()
    }

    /// The listed networks, marking the one the Mac is on.
    #[must_use]
    pub fn networks(&self, here: Option<&DoorNetwork>) -> Vec<DoorNetwork> {
        self.networks
            .iter()
            .map(|n| DoorNetwork {
                id: n.id.clone(),
                label: n.label.clone(),
                here: here.is_some_and(|h| h.id == n.id),
            })
            .collect()
    }
}

/// The real probe: asks macOS for the default router and its MAC.
#[must_use]
pub fn mac_probe() -> Probe {
    Arc::new(current_network)
}

/// A probe that always says `id` (for tests: no real network involved).
#[must_use]
pub fn fixed_probe(id: &str, label: &str) -> Probe {
    let network = DoorNetwork {
        id: id.to_owned(),
        label: label.to_owned(),
        here: true,
    };
    Arc::new(move || Some(network.clone()))
}

fn current_network() -> Option<DoorNetwork> {
    let route = run("route", &["-n", "get", "default"])?;
    let (gateway, interface) = parse_route(&route)?;
    let mac = run("arp", &["-n", &gateway])
        .and_then(|out| parse_arp(&out))
        .or_else(|| {
            // Not in the cache yet: say hello to the router once, then look again.
            let _ = Command::new("ping")
                .args(["-c", "1", "-t", "1", &gateway])
                .output();
            run("arp", &["-n", &gateway]).and_then(|out| parse_arp(&out))
        })?;
    let wifi = run("ipconfig", &["getsummary", &interface]).and_then(|out| parse_ssid(&out));
    let label = wifi.unwrap_or_else(|| range_label(&gateway));
    Some(DoorNetwork {
        id: mac,
        label,
        here: true,
    })
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The router's address and the interface to it, from `route -n get default`.
fn parse_route(out: &str) -> Option<(String, String)> {
    let field = |name: &str| {
        out.lines()
            .filter_map(|l| l.trim().split_once(':'))
            .find(|(k, _)| k.trim() == name)
            .map(|(_, v)| v.trim().to_owned())
    };
    let gateway = field("gateway")?;
    gateway.parse::<IpAddr>().ok()?;
    Some((gateway, field("interface")?))
}

/// The hardware address in `arp -n` output, written in full
/// (`a4:2b:b0:02:04:05`; arp drops leading zeros).
fn parse_arp(out: &str) -> Option<String> {
    let after = out.split(" at ").nth(1)?;
    let mac = after.split_whitespace().next()?;
    let parts: Vec<&str> = mac.split(':').collect();
    if parts.len() != 6 || parts.iter().any(|p| p.is_empty() || p.len() > 2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = parts
        .iter()
        .map(|p| u8::from_str_radix(p, 16).ok())
        .collect();
    let bytes = bytes?;
    if bytes.iter().all(|b| *b == 0) {
        return None;
    }
    Some(
        bytes
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// The Wi-Fi name from `ipconfig getsummary`, unless macOS hides it.
fn parse_ssid(out: &str) -> Option<String> {
    out.lines()
        .filter_map(|l| l.trim().split_once(':'))
        .find(|(k, _)| k.trim() == "SSID")
        .map(|(_, v)| v.trim().to_owned())
        .filter(|s| !s.is_empty() && !s.contains("redacted"))
}

/// `192.168.1.1` → `Network 192.168.1.x`.
fn range_label(gateway: &str) -> String {
    match gateway.rsplit_once('.') {
        Some((net, _)) if !gateway.contains(':') => format!("Network {net}.x"),
        _ => format!("Network {gateway}"),
    }
}

/// Whether a device at `ip` is on a local network: phones at home have
/// private addresses; anything from the internet doesn't.
#[must_use]
pub fn is_private_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_private_peer(IpAddr::V4(v4)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_router_and_its_hardware_address() {
        let route = "   route to: default\ndestination: default\n       mask: default\n    gateway: 192.168.1.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>\n";
        assert_eq!(
            parse_route(route),
            Some(("192.168.1.1".into(), "en0".into()))
        );
        assert_eq!(parse_route("gateway: utun4\ninterface: utun4\n"), None);
        assert_eq!(
            parse_arp("? (192.168.1.1) at a4:2b:b0:2:4:5 on en0 ifscope [ethernet]\n").as_deref(),
            Some("a4:2b:b0:02:04:05")
        );
        assert_eq!(
            parse_arp("? (192.168.1.1) at (incomplete) on en0 ifscope [ethernet]"),
            None
        );
        assert_eq!(
            parse_ssid("  BSSID : <redacted>\n  SSID : <redacted>\n"),
            None
        );
        assert_eq!(
            parse_ssid("  SSID : Home Wi-Fi\n").as_deref(),
            Some("Home Wi-Fi")
        );
        assert_eq!(range_label("192.168.1.1"), "Network 192.168.1.x");
    }

    #[test]
    fn only_local_devices_may_knock() {
        for ip in [
            "127.0.0.1",
            "192.168.1.20",
            "10.0.0.5",
            "172.16.0.9",
            "169.254.3.3",
            "fd00::1",
            "fe80::1",
            "::1",
            "::ffff:192.168.1.20",
        ] {
            assert!(is_private_peer(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.64.1.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!is_private_peer(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn the_list_and_name_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let home = DoorNetwork {
            id: "a4:2b:b0:02:04:05".into(),
            label: "Home Wi-Fi".into(),
            here: true,
        };
        let mut settings = Settings::load(tmp.path());
        assert!(!settings.allows(&home.id));
        assert!(settings.add(&home).unwrap());
        assert!(!settings.add(&home).unwrap());
        settings.set_name(Some("  Jay's   Mac ")).unwrap();
        let again = Settings::load(tmp.path());
        assert!(again.allows(&home.id));
        assert_eq!(again.name(), Some("Jay's Mac"));
        assert!(again.networks(Some(&home))[0].here);
        let mut again = again;
        assert!(again.remove(&home.id).unwrap());
        assert!(!Settings::load(tmp.path()).allows(&home.id));
    }
}
