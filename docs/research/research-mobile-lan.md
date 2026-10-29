# oxbot mobile + LAN research (2026-10-27)

Scope: a phone app that discovers and drives oxbot (Rust, Axum + tokio, always on on a Mac laptop) over the LAN.
**Hard constraint (updated):** data may leave the LAN only to Anthropic (Claude) and OpenAI (Codex). No other third party: no cloud relay, no push service carrying content, and no SDK or framework that phones home.

Legend: **[V]** = verified from the cited official doc or crate page in this session. **[GK]** = general knowledge, not re-verified today; check before relying on it.

---

## TL;DR recommendations

1. **Discovery:** Advertise `_oxbot._tcp` from Rust with **`mdns-sd`** (pure Rust, no runtime needed, v0.21.6). On iPhone, browse with **NWBrowser** (Network.framework). That needs `NSLocalNetworkUsageDescription` and `NSBonjourServices = [_oxbot._tcp]` and **does not need the multicast entitlement**. Do **not** run `mdns-sd` *inside* the iOS app: raw multicast on iOS needs Apple's gated entitlement. Keep a **QR code fallback**.
2. **Pairing/security:** Use HTTPS with a **self-signed cert (rcgen) and pinning (rustls)**, LocalSend/Syncthing style. Pair with a QR code that carries `host, port, cert SHA-256 fingerprint, one-time pairing secret (short TTL)`. The phone exchanges the secret for a **per-device token**, stored in Keychain on the phone and hashed on the server, so a device can be revoked. Plain HTTP is acceptable only as a first prototype step.
3. **Transport:** Use **WebSocket** for the interactive control channel (send prompt, cancel, approve tool use) plus a **resumable event log** (every event gets a sequence number; on reconnect the app asks for "events since N"). SSE also works. The resume design matters more than the protocol choice, because iOS suspends the app and drops sockets within seconds of backgrounding.
4. **No push.** APNs means Apple's servers, which this constraint rules out (even a content-free push leaks timing metadata to Apple). Instead: catch up on open, post **local notifications** from whatever background time iOS grants, and optionally send notifications from the Mac to the phone through a self-hosted path (see §3). Accept that the phone is a "check in" device, not a live pager.
5. **App tech:** **SwiftUI native** is the lowest-risk path to a working iPhone app, and the Rust learning stays on the server, where most of oxbot is. **Tauri 2 mobile** fits the "Rust everywhere" goal and is now on 2.12.5, but it has **no mDNS plugin** (you would write a small Swift plugin around NWBrowser), and **WKWebView can't do WebSocket to a self-signed cert**, so networking must live in Rust (reqwest/tokio-tungstenite with a pinned verifier) and be bridged to the UI. That is good Rust practice but adds plumbing. Avoid a PWA (no discovery, no pinning) and Expo (telemetry on by default, so set `EXPO_NO_TELEMETRY=1`, plus heavier JS tooling).
   - My pick: **SwiftUI client + Rust server** for v1. Revisit Tauri if Android becomes a real need.
6. **Distribution:** $99/yr Apple Developer Program for anything beyond tinkering. A free Apple ID works for personal installs from Xcode, but apps expire after **7 days** (3 devices, 10 App IDs). TestFlight needs the paid program.
7. **Away from home:** **Plain WireGuard on the home router** (or on a Pi or the Mac) is the purest fit. **Headscale** (self-hosted Tailscale control) with its **embedded DERP and `urls: []`** is next. Hosted Tailscale fails the new constraint: its coordination server sees metadata, its DERP relays carry (encrypted) packets, and its clients upload logs by default. mDNS does not cross any of these tunnels, so the app must remember the paired host's address or tunnel IP.
8. **Always-on Mac:** Use a per-user **LaunchAgent** (`KeepAlive`, `RunAtLoad`) with a **power assertion held by the process** (`keepawake` crate or IOPMAssertion), and keep it **on AC power**. With the lid closed, a MacBook sleeps unless it is in clamshell mode (power plus external display). A sleeping Mac cannot run agents; "Wake for network access" only helps with a Bonjour Sleep Proxy (Apple TV/HomePod-class device) and only wakes it briefly. macOS 15+ local-network privacy **applies to LaunchAgents** (not daemons), so sign the binary or expect the prompt.

---

## 1. Discovery (mDNS / DNS-SD)

### Advertising from Rust on macOS
| Crate | What it is | Version / activity | Notes |
|---|---|---|---|
| **mdns-sd** | Pure Rust responder + querier. Runs its own thread and uses `flume` channels (sync `recv()` or `recv_async()`), so it doesn't need tokio. | 0.21.6, updated 2026-10-19, ~6.8M downloads **[V]** | README: "still beta software". Tested against `dns-sd` on macOS, Avahi on Linux and Bonjour on iOS **[V]**. Handles RFC 6762 name conflicts with a `DaemonEvent::NameChange` **[V]**. Multicast only (no unicast responses) **[V]**. |
| **zeroconf** | Wraps the system stack: Bonjour (`dns_sd`) / Avahi | 0.18.0 (2026-04-07) **[V]** | Uses the OS daemon (mDNSResponder on macOS), so there is no socket sharing with 5353. Needs Avahi dev libs on Linux. |
| **astro-dnssd** | Thin wrapper over the dns-sd API | 0.3.7 (2026-10-26) **[V]** | Register/browse/TXT work; its own TODO list includes "tests, documentation". |

**macOS coexistence:** mDNSResponder always holds UDP 5353. A userland responder like `mdns-sd` has to share the port (SO_REUSEADDR/SO_REUSEPORT). The crate README doesn't discuss this **[V: not mentioned]**, but it says it was tested with macOS `dns-sd` tooling. **Check this on day one:** run oxbot, then `dns-sd -B _oxbot._tcp` on the Mac and NWBrowser on the phone. If it's flaky, switch to `zeroconf` or `astro-dnssd`, which register through mDNSResponder and are the most "native" choice on a Mac.

**macOS local network privacy also applies to the server [V, TN3179]:**
- Since macOS 15, sending multicast and registering Bonjour services are "local network operations". Accepting incoming TCP connections is **not** one.
- Automatically allowed: launchd **daemons**, root processes, and command-line tools run from Terminal/SSH (including their children).
- **"The exception for `launchd` daemons doesn't apply to `launchd` agents."** A LaunchAgent oxbot needs the Local Network privilege (one prompt). If it isn't installed via `SMAppService`, set `AssociatedBundleIdentifiers` in the plist.
- Tracking is tied to the code signature: "sign it with an Apple-issued code-signing identity". An unsigned or ad-hoc binary that changes on every `cargo build` may confuse the privilege state. Also, a short-lived process that exits right after a failed local operation never shows the alert (FB16131937).
- Practical result: **while developing, run from Terminal (exempt).** When it becomes a LaunchAgent, sign it (a Developer ID from the $99 program) or accept re-approving it.

### Browsing on iOS [V, TN3179 + Apple docs]
- **API:** `NWBrowser(for: .bonjourWithTXTRecord(type: "_oxbot._tcp", domain: nil), using: .tcp)` **[GK for exact signature]**. All Bonjour operations (register, browse, resolve) require local network access **[V]**.
- **Info.plist:**
  - `NSLocalNetworkUsageDescription`: "Any app that uses the local network, directly or indirectly, should include this description." **[V]**
  - `NSBonjourServices`: an array like `_oxbot._tcp` listing "all service types that your app expects to use" **[V]**.
- **Multicast entitlement (`com.apple.developer.networking.multicast`):** needed for raw UDP multicast/broadcast send or receive, for "working with arbitrary Bonjour service types", or for browsing all service types. It needs Apple's approval **[V]**. **Not needed** for browsing a declared type with NWBrowser. Consequence: don't use `mdns-sd` (raw multicast sockets) inside the iOS app.
- **Prompt behaviour [V]:** the alert appears on the first local operation, and there is no API to trigger or query it. The first operation may be denied before the user answers, so use Network.framework's waiting/retry behaviour. Bonjour reports `kDNSServiceErr_PolicyDenied (-65570)` when denied. Operations from the background while the privilege is undetermined are denied silently. The Simulator doesn't enforce the privilege, so test on a real phone. Delete the app to reset it.
- **TXT record:** put `id` (stable server UUID), `name`, `v` (protocol version) and `fp` (short cert fingerprint prefix) in TXT. Discovery is a convenience; security still comes from pinning.

### Android [V, developer.android.com]
- `NsdManager.discoverServices("_oxbot._tcp", PROTOCOL_DNS_SD, listener)`, then resolve. Stop discovery in `onPause`.
- **[GK]** Android 13+ may require `NEARBY_WIFI_DEVICES`, and `resolveService` is deprecated in API 34 in favour of `registerServiceInfoCallback`. Some devices need a `MulticastLock`.

### Fallback
A **QR code on the Mac** (CLI prints it, or a local web page at `https://localhost:PORT/pair`) with `oxbot://pair?h=192.168.1.20,mac.local&p=7443&fp=<sha256>&s=<one-time secret>&id=<server uuid>`. This also covers networks where multicast is blocked (guest Wi-Fi, mesh "client isolation", corporate).

---

## 2. Pairing and security

### Prior art
- **LocalSend [V, protocol v2.2]:** multicast UDP **224.0.0.167:53317**. The group is in 224.0.0.0/24 because some Android devices reject other groups. Peers reply by HTTP POST to `/api/localsend/v2/register` (UDP fallback), and there's a legacy fallback that scans all local IPs. HTTPS with self-signed certs, with **fingerprint = SHA-256 of the certificate** (a random string in HTTP mode). An optional PIN is sent as `?pin=`, with 401/403/409/429 semantics.
- **Syncthing [V]:** Device ID = encoded **SHA-256 of the device's TLS certificate**. Connections are accepted only if the fingerprint is on the configured list. TLS 1.2/1.3. Local discovery broadcasts or multicasts the device ID and port every 30 s, and the docs note anyone on the LAN can see them.
- **KDE Connect [secondary source]:** UDP 1716 discovery (or `_kdeconnect._udp` mDNS), TLS on TCP 1716 after an explicit pairing request with a verification key shown on both sides. Several implementations exist, including Swift.
- **Home Assistant [old docs only]:** advertises via zeroconf with a base URL in TXT, and its push uses Apple's service. That doesn't fit our constraint.

### Recommended oxbot pairing flow
1. On first run, oxbot generates a long-lived **ECDSA P-256 key + self-signed cert** with `rcgen` (default key type) and stores it in `~/Library/Application Support/oxbot/`. Fingerprint = SHA-256(DER cert), the same scheme as LocalSend and Syncthing.
2. `oxbot pair` (or a button in a local web UI) creates a **one-time pairing secret** (≥128-bit, 5-minute TTL, single use) and shows a QR code with host(s), port, fingerprint and secret.
3. The phone connects to `https://host:port` with **pinning**: it accepts only a cert whose SHA-256 matches the QR value, then sends `POST /pair {secret, device_name, device_pubkey?}`.
4. The server returns a **per-device bearer token** (random 256-bit). It stores only `hash(token)` plus device name, created and last-seen, and keeps the token in the phone's Keychain. Later: `Authorization: Bearer` on every request and on the WebSocket upgrade.
5. **Revocation:** `oxbot devices list | revoke <id>` deletes the hash and closes any live sockets for that device. Also support **rotating the server cert**: the phone must accept old and new fingerprints during the switch (rustls-pin's guidance: upgrade clients to accept both before switching).
6. **Discovered-but-unpaired servers** only show the "Pair" action. Discovery itself grants nothing.
7. **Multiple bots ("add any bot"):** the phone keeps a list of `{server_id, name, last_known_hosts[], fingerprint, token}`. Bonjour results are matched by `server_id` from TXT, so a Mac whose IP changes is still recognised. Bot *profiles* (Claude Code / Codex / OpenRouter personas) are server-side resources under one pairing, so one Mac means one pairing and N profiles.

### TLS vs plain HTTP on the LAN
- Plain HTTP means anyone on the Wi-Fi (guests, IoT) can read transcripts and steal tokens. Since the whole point is privacy, **use TLS from the first networked milestone**. Plain HTTP on `127.0.0.1` is fine for step 0.
- **Rust server:** `rustls` 0.23.46 + `axum-server` 0.8.0 (`tls-rustls` feature) **[V versions]**, or `tokio-rustls` directly.
- **Rust client (if Tauri):** rustls `dangerous()` custom `ServerCertVerifier` that compares SHA-256(end-entity cert) with the pin. Also verify the handshake signature, don't skip it (`rustls-pin` crate exists) **[V: rustls-pin docs]**.
- **Swift client:** `URLSessionDelegate.urlSession(_:didReceive:completionHandler:)` server-trust challenge: hash the leaf cert and compare. This covers `URLSessionWebSocketTask` too **[GK]**.

### iOS App Transport Security [V, Apple doc]
- `NSAllowsLocalNetworking` lets ATS connect to "unqualified domains, `.local` domains, and IP addresses". Since iOS 17, ATS no longer allows IP addresses by default, so set this key (Apple suggests setting it "as a declaration of intent").
- With pinned HTTPS you still have a self-signed cert: ATS rules don't override your own trust evaluation in the URLSession delegate **[GK]**.
- **WKWebView caveat [Apple DTS forum answers]:** you can override server trust for page navigations, but **"this does not extend to WebSockets"**, and ATS exceptions are ignored for WebSockets. There's also a report of self-signed loopback subresources failing on iOS 26.4. → Any web-view-based client (Tauri, PWA, Capacitor) should **not** open the socket from JavaScript. Do it natively (Swift or Rust) and bridge events.

---

## 3. Transport and backgrounding

### SSE vs WebSocket
| | SSE | WebSocket |
|---|---|---|
| Direction | server→client only; commands go via separate POSTs | bidirectional |
| Resume | built-in `Last-Event-ID` | you implement `since=seq` |
| Axum | `axum::response::sse` | `axum::extract::ws` |
| iOS native | no EventSource in Foundation; parse `URLSession.bytes(for:)` lines **[GK]** | `URLSessionWebSocketTask` **[GK]** |
| Rust client | `reqwest` streaming (0.13.5) | `tokio-tungstenite` 0.30.0 |

**Recommendation:** one **WebSocket** per paired server, carrying JSON frames `{seq, session_id, kind, payload}`. The server keeps an **append-only per-session event log** (SQLite) so the phone can reconnect with `?since=<last_seq>` and replay. That is how the "connection drops when backgrounded" problem goes away. If you prefer simpler code to start with, SSE with `Last-Event-ID` gives the same resume semantics.

### iOS background limits [V, Apple UIKit doc]
- When an app is backgrounded, the delegate method "has five seconds to perform any tasks and return. Shortly after that method returns, the system puts your app into the suspended state." `beginBackgroundTask` buys a little more time (the limit is `backgroundTimeRemaining`, typically around 30 s **[GK]**).
- In practice, sockets die when the app suspends. `BGAppRefreshTask` is "a request to launch your app in the background to execute a short refresh task" **[V]**. Timing is at the system's discretion (could be hours) **[GK]**, and it's only useful for opportunistic polling.
- Local network privacy: background operations with an undetermined privilege are silently denied **[V]**.

### No-cloud notification options
- **APNs** (even "content-free / silent"): **rejected by the constraint.** Apple would learn device token, timing and frequency, and that the phone talks to your server. It also needs a server-side APNs key and the $99 program. It's possible, but it breaks the "only Anthropic/OpenAI" rule, so leave it off.
- **What works without any third party:**
  1. **Catch-up on foreground:** when the app opens, reconnect, replay events since the last seq, show a badge or list of "finished while you were away" and "needs approval".
  2. **Local notifications** (`UNUserNotificationCenter`, no server involved) posted from the app during a background-task window or `BGAppRefreshTask` run, e.g. "Agent finished: 3 files changed". Best-effort only.
  3. **Long-running agent approvals:** design so the agent **pauses and waits** for approval (no timeout), so a late phone check-in is fine.
  4. **[GK] iOS 17+ Live Activities** need APNs for remote updates, so they're excluded. Local Live Activity updates only happen while the app runs.
  5. If real alerts become essential: the Mac sends an **iMessage/email to yourself** (still Apple/Google infrastructure, which fails the rule), or a **self-hosted ntfy server on the LAN**. Note: ntfy on iOS still relies on APNs via the upstream ntfy.sh server for instant delivery **[GK, verify]**, so it also fails. **Conclusion: no instant push under this constraint. Accept a "pull" model.**

---

## 4. Mobile app technology

| Option | Discovery | Pinned TLS + WS | Rust learning | Path to TestFlight | Telemetry | Verdict |
|---|---|---|---|---|---|---|
| **(b) SwiftUI native** | NWBrowser, first-class | URLSession delegate, easy | none on the phone (all Rust on server) | Xcode → TestFlight | none | **Recommended v1** |
| **(a) Tauri 2 mobile** | **no plugin found**; write a Swift plugin around NWBrowser (Tauri supports Swift/Kotlin plugins with `@objc` commands and `trigger()` events **[V]**) | must do in **Rust** (reqwest/tungstenite with custom verifier); WKWebView WS can't trust self-signed | high (shared Rust crate for protocol types) | Xcode project generated; doable | no telemetry in core found **[searched, nothing found; verify in tauri repo]**; avoid optional analytics plugins (e.g. Aptabase) | **Good v2 / Android path** |
| **(d) React Native/Expo + zeroconf** | community zeroconf libs | pinning needs native module | none | EAS (cloud build: code leaves the LAN!) or local `expo run:ios` | **Expo CLI collects anonymous usage; opt out with `EXPO_NO_TELEMETRY=1` [V]**; EAS Build is cloud | Not recommended |
| **(c) PWA from the Mac** | none (must type IP) | browser rejects self-signed unless the cert is installed as a trusted profile; WS fails | none | n/a | none | Only as a debug UI on the Mac |

Tauri state **[V]**: `tauri` crate 2.12.5 (2026-10-21, ~39.6M downloads). Mobile plugins: Swift package with a `Plugin` subclass. Rust→native via `run_mobile_plugin`, native→JS via `trigger` + `addPluginListener`. `tauri-plugin-http` re-exports `reqwest`; its docs list no invalid-cert or custom-TLS option, so pinning means your own reqwest/rustls client in a Rust command.

**Why SwiftUI first:** the hard parts (Local Network prompt, NWBrowser, cert pinning, Keychain, local notifications, background tasks) are all first-class Swift APIs. In Tauri each one is a bridge you write. Jay's Rust learning is better spent on the server (Axum, tokio, rustls, rcgen, mdns-sd, SQLite event log, launchd), which is most of the code. **Shared contract:** define the protocol as JSON Schema or OpenAPI generated from Rust types (`schemars`/`utoipa`) and generate Swift `Codable` types, so the Rust types stay the source of truth.

### Sideloading / distribution [V]
- **Free Apple Account (Personal Team):** Xcode install on your own phone; max 3 devices, 10 App IDs, 3 apps per device; **apps and profiles expire after 7 days** (rebuild and reinstall). Advanced capabilities are not available, but **none are needed** for NWBrowser with a declared service type. The multicast entitlement would need the paid program and Apple approval anyway.
- **Apple Developer Program $99/yr:** 1-year profiles, TestFlight (up to 10,000 external testers), App Store, and Developer ID signing for the Mac binary (helps the macOS local-network and firewall identity).
- **App Review [GK]:** a "controls your own Mac agent" app with local networking is reviewable. Have a clear purpose string, a demo video or notes, and reviewers may need a test server. TestFlight internal testing skips review.

---

## 5. Leaving the house

- **mDNS does not cross VPNs/tunnels** (UDP multicast doesn't go over the tunnel interface) **[community + gateway docs; consistent with TN3179, which defines "local network" as broadcast-capable interfaces and explicitly not VPN]**. The app must fall back to **last-known address** (tunnel IP or a fixed hostname). Unicast DNS-SD ("wide-area Bonjour") via split DNS is possible but overkill.
- **Plain WireGuard (best fit):** a WireGuard server on the home router (many ASUS/OpenWrt/UniFi routers support it), a Pi, or the Mac. The official WireGuard iOS app supports **on-demand activation** with an "except these SSIDs" list (VPN only when not on home Wi-Fi) **[ASUS FAQ]**. No third party, but you need port-forwarding of one UDP port and dynamic DNS (DDNS is a third party that learns your home IP, which is metadata, not content).
- **Headscale (self-hosted Tailscale control) [V]:** single tailnet, for personal use. The official Tailscale iOS app supports "Use custom coordination server". **By default clients still use Tailscale Inc.'s DERP relays**: enable the embedded DERP (`derp.server.enabled: true`) and set `urls: []` to drop Tailscale's servers (single point of failure warning). Needs a publicly reachable server (VPS = another provider, or home with port-forward). Headscale's `logtail` is off by default **[secondary source]**.
- **Hosted Tailscale:** WireGuard end-to-end; "private keys never leave the device"; DERP "impossible for a DERP server to decrypt your traffic" **[V]**. But the coordination server sees metadata (public IPs, device names, OS, versions, who connects to whom) **[V]**, and **clients upload logs to Tailscale by default**. Opt-out is `--no-logs-no-support` / `TS_NO_LOGS_NO_SUPPORT`, and on macOS only with the open-source tailscaled **[V Tailscale logging docs]**. **Fails the "no other third party" rule.**
- **Recommendation:** v1 LAN-only. v2 WireGuard on the router with on-demand on the phone. The app just stores `{lan_hosts[], wg_ip}` per server and tries them in order, using the **same pinned TLS + token** over the tunnel (defence in depth).

---

## 6. Always-on Mac

- **LaunchAgent** at `~/Library/LaunchAgents/com.jay.oxbot.plist` **[V launchd.plist(5)]**:
  - `KeepAlive` (default false means start only on demand; true restarts it continuously)
  - `RunAtLoad` (launch once when loaded)
  - `ThrottleInterval` (default: no respawn more often than every 10 s)
  - `ProcessType` (Background/Standard/Adaptive/Interactive; use Standard or Interactive so it isn't throttled)
  - `AssociatedBundleIdentifiers` (ties it to an app in Login Items, and to local network privacy attribution **[V TN3179]**)
  - An agent (not a daemon) is needed because Claude Code/Codex CLIs use the user's keychain, home directory and auth.
- **Preventing sleep:**
  - `caffeinate -i` prevents idle sleep. `-s` prevents system sleep but "is valid only when system is running on AC power" **[V caffeinate(8)]**.
  - From Rust: `keepawake` crate 0.6.1 (`Builder::default().idle(true).sleep(true).create()`, held until dropped) **[V]**, or IOPMAssertionCreateWithName ("No special privileges are necessary… any process may activate a power assertion") **[V]**.
  - Best: **hold the assertion only while an agent job is running**, and let the Mac sleep otherwise (battery, heat).
  - `pmset -c sleep 0` (charger-only system sleep off) is the system-wide alternative. `womp` is the same as "Wake for network access" **[V pmset(1)]**. Changing settings is up to Jay; this research changed nothing.
- **Lid closed:** closing a MacBook lid sleeps it regardless of `caffeinate -i` **[GK]**. Closed-display (clamshell) mode needs the **power adapter plus an external display** (and keyboard/mouse per older Apple guidance) **[secondary: Apple Community citing Apple's article]**. Third-party "no-sleep with lid closed" hacks (e.g. `pmset disablesleep 1`, undocumented) risk heat in a bag. **Practical answer: keep the lid open (screen can be off/dimmed) on power, or use clamshell with a display.**
- **Can it answer while asleep?** No. A sleeping Mac runs no agents. **Wake for network access + Bonjour Sleep Proxy:** a sleeping Mac registers its Bonjour services with a sleep proxy (historically AirPort/Time Capsule/Apple TV) that wakes it when a client connects to the service. A laptop needs AC power and a display available (lid open or external display), Wi-Fi support depends on the base station, and ping doesn't wake it **[Apple HT201960 archive; Stuart Cheshire notes]**. Unreliable for this use. Treat oxbot as "available while awake on power".
- **Firewall:** the macOS application firewall is **off by default** **[GK]**. When on, signed downloaded apps can be auto-allowed **[V Apple support]**, and unsigned binaries trigger "accept incoming connections?" prompts that can recur after each rebuild because identity is per-signature **[GK]**. Sign the release binary (Developer ID) or add it once via Firewall Options. **Separate from the firewall:** macOS 15+ Local Network privilege for the LaunchAgent's Bonjour advertising (see §1).

---

## Risks
1. **mdns-sd vs mDNSResponder on the same Mac:** beta crate, coexistence undocumented. Mitigation: test on day 1; fallback is `zeroconf`/`astro-dnssd` (system Bonjour).
2. **macOS local-network privilege for an unsigned, rebuilt LaunchAgent:** can silently block advertising. Mitigation: run from Terminal during dev, sign for "production", keep the process alive after a failure so the alert appears.
3. **No real push** under the constraint. Mitigation: design for pull, catch-up and agents that pause for approval.
4. **Mac sleep / lid / battery:** the bot simply isn't there. Mitigation: AC + lid open or clamshell; power assertion per job; show "last seen" in the app.
5. **Self-signed + web views:** WebSockets in WKWebView can't trust a pinned self-signed cert. Mitigation: native or Rust networking only.
6. **Client-isolated Wi-Fi** (hotels, mesh guest networks) blocks both mDNS and direct connections. Mitigation: QR plus manual host; WireGuard.
7. **Expo/EAS and Tailscale-hosted** quietly add third parties (telemetry, cloud builds, coordination, logs, DERP). Excluded by design.
8. **7-day free provisioning** gets old fast. Budget the $99 if this becomes daily-use.
9. **Token or cert theft on the Mac** (anyone with Jay's user account can read the token hashes and key). Acceptable for a personal tool. Keep files 0600, and consider storing the key in the macOS Keychain later.

---

## Sources
- Apple TN3179 Understanding local network privacy: https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy (rev. 2026-10-06)
- Apple Local Network Privacy FAQ (superseded by TN3179): https://developer.apple.com/forums/thread/663858
- NSBonjourServices: https://developer.apple.com/documentation/bundleresources/information-property-list/nsbonjourservices
- NSLocalNetworkUsageDescription: https://developer.apple.com/documentation/bundleresources/information-property-list/nslocalnetworkusagedescription
- Multicast entitlement: https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.networking.multicast
- NSAllowsLocalNetworking: https://developer.apple.com/documentation/bundleresources/information-property-list/nsapptransportsecurity/nsallowslocalnetworking
- NWBrowser: https://developer.apple.com/documentation/network/nwbrowser
- Extending background execution time: https://developer.apple.com/documentation/uikit/extending-your-app-s-background-execution-time
- BGAppRefreshTaskRequest: https://developer.apple.com/documentation/backgroundtasks/bgapprefreshtaskrequest
- IOPMAssertionCreateWithName: https://developer.apple.com/documentation/iokit/1557134-iopmassertioncreatewithname
- Apple membership comparison: https://developer.apple.com/support/compare-memberships/ ; Program ($99, TestFlight): https://developer.apple.com/programs/
- Apple DTS on WKWebView + WebSocket + self-signed: https://developer.apple.com/forums/thread/676413 , https://developer.apple.com/forums/thread/120869 , iOS 26.4 report https://developer.apple.com/forums/thread/820730
- macOS Firewall settings: https://support.apple.com/guide/mac-help/change-firewall-settings-on-mac-mh11783/mac
- launchd.plist(5): https://keith.github.io/xcode-man-pages/launchd.plist.5.html ; caffeinate(8): https://keith.github.io/xcode-man-pages/caffeinate.8.html ; pmset(1): https://keith.github.io/xcode-man-pages/pmset.1.html
- Bonjour Sleep Proxy: archived Apple HT201960 https://nrs.preservation.mirrorweb.com/public/20170610164455mp_/https://support.apple.com/el-gr/HT201960 ; https://stuartcheshire.org/sleepproxy/
- Android NSD: https://developer.android.com/develop/connectivity/wifi/use-nsd
- mdns-sd: https://docs.rs/mdns-sd/latest/mdns_sd/ , https://github.com/keepsimple1/mdns-sd ; zeroconf: https://docs.rs/crate/zeroconf/latest ; astro-dnssd: https://docs.rs/crate/astro-dnssd/latest ; keepawake: https://docs.rs/keepawake/latest/keepawake/ ; rustls-pin: https://docs.rs/rustls-pin/latest/rustls_pin/ ; versions from crates.io API (2026-10-27)
- LocalSend protocol: https://github.com/localsend/protocol
- Syncthing security: https://docs.syncthing.net/users/security.html
- KDE Connect (secondary): https://pkg.go.dev/github.com/bethropolis/kcd , https://nlnet.nl/project/KDE-Connect/
- Tauri mobile plugins: https://v2.tauri.app/develop/plugins/develop-mobile/ ; HTTP plugin: https://v2.tauri.app/plugin/http-client/
- Expo CLI telemetry: https://docs.expo.dev/more/expo-cli/
- Tailscale DERP: https://tailscale.com/kb/1232/derp-servers ; control/data planes: https://tailscale.com/docs/concepts/control-data-planes ; security: https://tailscale.com/security ; logging opt-out: https://tailscale.com/docs/features/logging
- Headscale: https://headscale.net/stable/ ; DERP: https://headscale.net/stable/ref/derp/ ; Apple clients: https://headscale.net/stable/usage/connect/apple/
- mDNS over Tailscale (community): https://community.getchannels.com/t/tailscale-discovery/39284
- WireGuard on-demand (ASUS): https://www.asus.com/us/support/faq/1048916/
