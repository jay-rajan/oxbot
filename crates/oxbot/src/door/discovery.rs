//! How devices find the door: this Mac's home-network addresses for the
//! pairing link, and an mDNS announcement (`_oxbot._tcp`) so a device finds
//! the home computer again after its address changes.

/// The mDNS service devices look for.
pub const MDNS_SERVICE: &str = "_oxbot._tcp.local.";

/// Where devices can reach the door: this Mac's private addresses, then its
/// `.local` name.
#[must_use]
pub fn lan_hosts(port: u16) -> Vec<String> {
    let mut hosts: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            std::net::IpAddr::V4(ip) if ip.is_private() => Some(format!("{ip}:{port}")),
            _ => None,
        })
        .collect();
    hosts.sort();
    hosts.dedup();
    if let Some(name) = local_host_name() {
        hosts.push(format!("{name}.local:{port}"));
    }
    hosts
}

/// This Mac's Bonjour name, e.g. `Jays-Mac-mini`.
fn local_host_name() -> Option<String> {
    std::process::Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Announces the door as `name` on the local network, with the certificate
/// fingerprint. Best effort: pairing still works by address without it.
#[must_use]
pub fn advertise(name: &str, port: u16, fingerprint: &str) -> Option<mdns_sd::ServiceDaemon> {
    let host = format!("{}.local.", local_host_name()?);
    let daemon = mdns_sd::ServiceDaemon::new()
        .map_err(|err| tracing::warn!(%err, "couldn't announce the device door on Wi-Fi"))
        .ok()?;
    let props = [("fp", fingerprint)];
    let info = mdns_sd::ServiceInfo::new(MDNS_SERVICE, name, &host, "", port, &props[..])
        .map(mdns_sd::ServiceInfo::enable_addr_auto)
        .map_err(|err| tracing::warn!(%err, "couldn't describe the device door for Wi-Fi"))
        .ok()?;
    daemon
        .register(info)
        .map_err(|err| tracing::warn!(%err, "couldn't announce the device door on Wi-Fi"))
        .ok()?;
    Some(daemon)
}
