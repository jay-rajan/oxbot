//! The device door: how phones and laptops (devices) reach the home computer.
//!
//! - It listens on port 7879, TLS only, with a certificate the home computer made for
//!   itself ([`identity`]). A device pins that certificate's fingerprint, which
//!   it got from the pairing QR code.
//! - Pairing: Settings → devices shows a one-time code and its QR code for a
//!   chosen person ([`pairing`]). The device sends it to `POST /pair` and gets
//!   a key back. Every other request needs that key ([`http`]); the home computer
//!   keeps only its hash, and a device always acts as its person.
//! - It's open only while the Mac is on a network the home computer listed in
//!   Settings → devices ([`network`]), and only while a device is paired or a
//!   code is showing. It checks every 30 seconds and whenever something
//!   changes, and only devices with local addresses get in ([`listener`]).
//! - While open, it tells the network where it is (mDNS `_oxbot._tcp`), so
//!   a device finds the home computer after its address changes ([`discovery`]).

pub mod discovery;
pub mod http;
pub mod identity;
pub mod listener;
pub mod network;
pub mod pairing;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use anyhow::Context as _;
use axum::Router;
use oxbot_core::{Hub, HubError, Viewer};
use oxbot_protocol::{
    DeviceInfo, DoorNetwork, DoorState, PairReply, PairRequest, PairingCode, ServerMsg,
};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

pub use http::DeviceAuth;
pub use identity::Identity;
pub use network::Probe;

use pairing::{Check, PairingCodes};

/// The device door's usual port.
pub const DEFAULT_DEVICE_PORT: u16 = 7879;

/// The device door's folder in `data_dir` (its certificate). It was `gate`
/// before the rename; that folder is moved here once.
///
/// # Errors
/// Fails if the old folder is there and can't be moved.
pub fn folder(data_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("door");
    oxbot_device::move_old_name(&data_dir.join("gate"), &dir)?;
    Ok(dir)
}
/// How often the door checks which network the Mac is on.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// Sent on the removals channel to close every device's connection (the door
/// closed).
pub const EVERY_DEVICE: &str = "*";

/// Why pairing failed, as the device is told.
#[derive(Debug, PartialEq, Eq)]
pub enum PairError {
    /// No code is showing on the home computer, or it ran out.
    NoCode,
    /// Wrong code.
    WrongCode,
    /// Something went wrong on the home computer.
    HomeComputer(String),
}

impl std::error::Error for PairError {}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCode => f.write_str(
                "No pairing code is showing on the home computer, or it ran out. Make a new one in Settings → Devices.",
            ),
            Self::WrongCode => f.write_str("That code isn't right. Check it and try again."),
            Self::HomeComputer(why) => write!(f, "The home computer couldn't pair just now: {why}"),
        }
    }
}

/// The door while it's open.
struct Open {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
    mdns: Option<mdns_sd::ServiceDaemon>,
}

impl Drop for Open {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(mdns) = self.mdns.take() {
            let _ = mdns.shutdown();
        }
    }
}

/// The device door. One per home computer.
pub struct Door {
    dir: PathBuf,
    port: u16,
    /// The Mac's name: what phones see unless the home computer picked another.
    mac_name: String,
    hub: Arc<Hub>,
    probe: Probe,
    identity: Mutex<Option<Arc<Identity>>>,
    codes: Mutex<PairingCodes>,
    settings: Mutex<network::Settings>,
    /// The network the Mac is on, as last checked.
    here: Mutex<Option<DoorNetwork>>,
    /// Held while opening or closing, so two at once can't race.
    open: tokio::sync::Mutex<Option<Open>>,
    /// Where it listens while open (readable without waiting).
    addr: Mutex<Option<SocketAddr>>,
    /// The page and WebSocket routes it serves, from [`Door::start`].
    router: Mutex<Option<Router>>,
    /// Device id → open connections.
    online: Mutex<HashMap<String, usize>>,
    /// Device ids that were just removed (or [`EVERY_DEVICE`]); their
    /// connections close.
    removed: broadcast::Sender<String>,
    /// What the home computer was last told, to tell it only about changes.
    told: Mutex<Option<DoorState>>,
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for Door {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Door")
            .field("dir", &self.dir)
            .field("port", &self.port)
            .field("addr", &*lock(&self.addr))
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Door {
    /// A closed door keeping its certificate and settings in `dir`, that
    /// listens on `port` (0 picks a free one) when open. `probe` says which
    /// network the Mac is on ([`network::mac_probe`] for the real one).
    #[must_use]
    pub fn new(
        dir: PathBuf,
        port: u16,
        mac_name: impl Into<String>,
        hub: Arc<Hub>,
        probe: Probe,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings: Mutex::new(network::Settings::load(&dir)),
            dir,
            port,
            mac_name: mac_name.into(),
            hub,
            probe,
            identity: Mutex::new(None),
            codes: Mutex::new(PairingCodes::default()),
            here: Mutex::new(None),
            open: tokio::sync::Mutex::new(None),
            addr: Mutex::new(None),
            router: Mutex::new(None),
            online: Mutex::new(HashMap::new()),
            removed: broadcast::channel(16).0,
            told: Mutex::new(None),
            watcher: Mutex::new(None),
        })
    }

    /// Starts watching the network and opens the door when it should be:
    /// serving `router` (the page and WebSocket) to paired devices.
    pub async fn start(self: &Arc<Self>, router: Router) {
        *lock(&self.router) = Some(router);
        self.refresh().await;
        let door: Weak<Self> = Arc::downgrade(self);
        let watcher = tokio::spawn(async move {
            loop {
                tokio::time::sleep(CHECK_EVERY).await;
                let Some(door) = door.upgrade() else { return };
                door.refresh().await;
            }
        });
        if let Some(old) = lock(&self.watcher).replace(watcher) {
            old.abort();
        }
    }

    /// Closes the door for good (on shutdown).
    pub async fn stop(&self) {
        if let Some(watcher) = lock(&self.watcher).take() {
            watcher.abort();
        }
        lock(&self.router).take();
        self.close_now().await;
    }

    fn identity(&self) -> anyhow::Result<Arc<Identity>> {
        let mut id = lock(&self.identity);
        if let Some(id) = id.as_ref() {
            return Ok(Arc::clone(id));
        }
        let made = Arc::new(Identity::load_or_create(&self.dir)?);
        *id = Some(Arc::clone(&made));
        Ok(made)
    }

    /// The certificate fingerprint devices pin.
    ///
    /// # Errors
    /// Fails if the certificate can't be loaded or made.
    pub fn fingerprint(&self) -> anyhow::Result<String> {
        Ok(self.identity()?.fingerprint.clone())
    }

    /// Where the door listens, while open.
    #[must_use]
    pub fn addr(&self) -> Option<SocketAddr> {
        *lock(&self.addr)
    }

    /// The name phones see.
    #[must_use]
    pub fn name(&self) -> String {
        lock(&self.settings)
            .name()
            .map_or_else(|| self.mac_name.clone(), str::to_owned)
    }

    /// The door as Settings → devices shows it.
    #[must_use]
    pub fn state(&self) -> DoorState {
        let why = if self.addr().is_some() {
            String::new()
        } else {
            self.wanted().1
        };
        self.state_with(why)
    }

    fn state_with(&self, why: String) -> DoorState {
        let here = lock(&self.here).clone();
        let networks = lock(&self.settings).networks(here.as_ref());
        let name = self.name();
        DoorState {
            open: self.addr().is_some(),
            why,
            networks,
            current: here.map(|h| DoorNetwork { here: true, ..h }),
            name,
        }
    }

    /// Whether the door should be open now, and if not, why.
    fn wanted(&self) -> (bool, String) {
        let here = lock(&self.here).clone();
        let Some(here) = here else {
            return (
                false,
                "oxbot can't tell which network the home computer is on, so phones can't connect."
                    .to_owned(),
            );
        };
        if !lock(&self.settings).allows(&here.id) {
            return (
                false,
                format!(
                    "Phones can't connect: the home computer is on {}, which isn't in the list below.",
                    here.label
                ),
            );
        }
        // If the database can't say, keep phones in rather than shut them out.
        let paired = self.hub.devices().map_or(true, |g| !g.is_empty());
        if !paired && !lock(&self.codes).is_showing(crate::runtime::now_ms()) {
            return (false, "No phones are paired yet.".to_owned());
        }
        (true, String::new())
    }

    /// Checks which network the Mac is on, then opens or closes the door to
    /// match. Returns the door's state.
    pub async fn refresh(&self) -> DoorState {
        let probe = Arc::clone(&self.probe);
        let here = tokio::task::spawn_blocking(move || probe())
            .await
            .ok()
            .flatten();
        *lock(&self.here) = here;
        self.apply().await
    }

    /// Opens or closes the door to match the settings and the network last
    /// checked, and tells the home computer if anything changed.
    async fn apply(&self) -> DoorState {
        let (want, mut why) = self.wanted();
        if want {
            if let Err(err) = self.open_now().await {
                tracing::warn!("{err:#}");
                why = format!("The door couldn't open: {err:#}");
            }
        } else {
            self.close_now().await;
        }
        let state = self.state_with(why);
        let changed = lock(&self.told).as_ref() != Some(&state);
        if changed {
            *lock(&self.told) = Some(state.clone());
            self.hub.announce(ServerMsg::Door(state.clone()));
        }
        state
    }

    async fn open_now(&self) -> anyhow::Result<SocketAddr> {
        let mut open = self.open.lock().await;
        if let Some(open) = open.as_ref() {
            return Ok(open.addr);
        }
        let router = lock(&self.router)
            .clone()
            .context("the device door isn't started")?;
        let identity = self.identity()?;
        let tls = identity.tls()?;
        let tcp = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, self.port)))
            .await
            .with_context(|| format!("couldn't open the device door on port {}", self.port))?;
        let (stop, stopped) = watch::channel(false);
        let listener = listener::TlsListener::start(tcp, tls, stopped.clone())?;
        let addr = axum::serve::Listener::local_addr(&listener)?;
        tokio::spawn(async move {
            let mut stopped = stopped;
            let served = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<listener::ConnBytes>(),
            )
            .with_graceful_shutdown(async move {
                let _ = stopped.wait_for(|s| *s).await;
            })
            .await;
            if let Err(err) = served {
                tracing::warn!(%err, "the device door stopped with an error");
            }
        });
        // Only once it's really listening, so a failed open leaves nothing behind.
        let mdns = discovery::advertise(&self.name(), addr.port(), &identity.fingerprint);
        *open = Some(Open { addr, stop, mdns });
        *lock(&self.addr) = Some(addr);
        tracing::info!(%addr, "the device door is open");
        Ok(addr)
    }

    async fn close_now(&self) {
        let mut open = self.open.lock().await;
        if open.take().is_some() {
            *lock(&self.addr) = None;
            // Phones connected through it are on their own now.
            let _ = self.removed.send(EVERY_DEVICE.to_owned());
            tracing::info!("the device door is closed");
        }
    }

    /// Lets phones use the network the Mac is on now.
    ///
    /// # Errors
    /// The Mac's network can't be told, or the setting can't be saved.
    pub async fn add_network(&self) -> anyhow::Result<DoorState> {
        self.refresh().await;
        let here = lock(&self.here)
            .clone()
            .context("oxbot can't tell which network the home computer is on. Check that Wi-Fi or Ethernet is connected.")?;
        if lock(&self.settings).add(&here)? {
            tracing::info!(network = here.label, "phones may use a network");
        }
        Ok(self.apply().await)
    }

    /// Stops letting phones use network `id`.
    ///
    /// # Errors
    /// The setting can't be saved.
    pub async fn remove_network(&self, id: &str) -> anyhow::Result<DoorState> {
        if lock(&self.settings).remove(id)? {
            tracing::info!("phones may no longer use a network");
        }
        Ok(self.apply().await)
    }

    /// Sets the name phones see (empty: the Mac's name).
    ///
    /// # Errors
    /// The setting can't be saved.
    pub async fn set_name(&self, name: &str) -> anyhow::Result<DoorState> {
        lock(&self.settings).set_name(Some(name))?;
        // Announce the new name, without dropping anyone.
        if let Some(open) = self.open.lock().await.as_mut() {
            if let Some(old) = open.mdns.take() {
                let _ = old.shutdown();
            }
            open.mdns = discovery::advertise(&self.name(), open.addr.port(), &self.fingerprint()?);
        }
        Ok(self.apply().await)
    }

    /// Shows a new pairing code for `person` (made safe, see
    /// [`Hub::device_person`]), opening the door.
    ///
    /// # Errors
    /// The name isn't usable, phones may not use this network, or the door
    /// can't open.
    pub async fn start_pairing(&self, person: &str) -> anyhow::Result<PairingCode> {
        let (person, person_id) = self.hub.device_pick(person)?;
        self.refresh().await;
        let here = lock(&self.here).clone();
        anyhow::ensure!(
            here.is_some_and(|h| lock(&self.settings).allows(&h.id)),
            "Phones can't use the network the home computer is on yet. Add it under “Networks phones can use” first."
        );
        let (code, expires_ms) =
            lock(&self.codes).show_for(&person, person_id, crate::runtime::now_ms());
        let state = self.apply().await;
        let Some(addr) = self.addr() else {
            lock(&self.codes).cancel();
            anyhow::bail!("{}", state.why);
        };
        let link = oxbot_device::PairLink {
            home_computer: oxbot_device::HomeComputer {
                hosts: discovery::lan_hosts(addr.port()),
                fingerprint: self.fingerprint()?,
                name: self.name(),
            },
            code: code.clone(),
        }
        .to_link();
        let qr_svg = pairing::qr_svg(&link)?;
        tracing::info!("showing a device pairing code");
        Ok(PairingCode {
            person,
            code: pairing::display_code(&code),
            link,
            qr_svg,
            expires_ms,
        })
    }

    /// Stops showing the pairing code (and closes the door if no device is
    /// paired).
    pub async fn cancel_pairing(&self) {
        lock(&self.codes).cancel();
        self.apply().await;
    }

    /// Checks a device's code and pairs it: a new device record and its key.
    ///
    /// # Errors
    /// No code showing, the wrong code, or the database failing.
    pub fn pair(&self, req: &PairRequest) -> Result<PairReply, PairError> {
        let (check, person_id) = lock(&self.codes).check_for(&req.code, crate::runtime::now_ms());
        let person = match check {
            Check::Paired(person) => person,
            Check::NoCode => return Err(PairError::NoCode),
            Check::Wrong { closed } => {
                if closed {
                    tracing::warn!(
                        "a pairing code was guessed wrong too often; it stopped working"
                    );
                    self.hub.announce(ServerMsg::PairingClosed);
                }
                return Err(PairError::WrongCode);
            }
        };
        let key = crate::secret::random_hex::<32>();
        let id = format!("g-{}", uuid::Uuid::new_v4().simple());
        let device: String = req
            .device
            .split(char::is_control)
            .collect::<String>()
            .trim()
            .chars()
            .take(60)
            .collect();
        let device = if device.is_empty() {
            "Device".to_owned()
        } else {
            device
        };
        let key_hash = oxbot_device::fingerprint(key.as_bytes());
        // The person picked when the code was shown, by id; someone new is
        // made now.
        let device = match &person_id {
            Some(person) => self.hub.add_device_for(&id, person, &device, &key_hash),
            None => self.hub.add_device(&id, &person, &device, &key_hash),
        }
        .map_err(|e| PairError::HomeComputer(e.to_string()))?;
        tracing::info!(device = device.id, "a device paired");
        self.hub.announce(ServerMsg::PairingClosed);
        self.announce_devices();
        Ok(PairReply {
            id,
            // Their name as it is now.
            person: device.person,
            home_computer: self.name(),
            key,
        })
    }

    /// The paired device holding `key`, if any.
    ///
    /// # Errors
    /// The database failed: that's "try again", not "not paired".
    pub fn check_key(&self, key: &str) -> Result<Option<(DeviceInfo, Viewer)>, HubError> {
        if !crate::secret::usable(key) {
            return Ok(None);
        }
        self.hub
            .device_viewer_by_key(&oxbot_device::fingerprint(key.as_bytes()))
    }

    /// Every paired device, with who's connected now.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceInfo> {
        let online = lock(&self.online);
        let mut devices = self.hub.devices().unwrap_or_default();
        for device in &mut devices {
            device.online = online.get(&device.id).is_some_and(|n| *n > 0);
        }
        devices
    }

    /// Tells the home computer's clients the devices list.
    pub fn announce_devices(&self) {
        self.hub.announce(ServerMsg::Devices {
            devices: self.devices(),
        });
    }

    /// Unpairs a device and closes its connections (and the door, if it was
    /// the last one).
    ///
    /// # Errors
    /// Fails if the database does.
    pub async fn remove(&self, id: &str) -> anyhow::Result<()> {
        let removed = self.hub.remove_device(id)?;
        anyhow::ensure!(removed, "That device isn't paired.");
        let _ = self.removed.send(id.to_owned());
        tracing::info!(device = id, "a device was removed");
        self.announce_devices();
        self.apply().await;
        Ok(())
    }

    /// Hears about removed devices (each connection listens for its own, and
    /// for [`EVERY_DEVICE`]).
    #[must_use]
    pub fn removals(&self) -> broadcast::Receiver<String> {
        self.removed.subscribe()
    }

    /// Marks device `id` as connected until the guard is dropped, however the
    /// connection ends. With its connection's byte counts, the connection
    /// is also noted in Settings → Activity (for `person`, the device's).
    #[must_use]
    pub fn online(
        self: &Arc<Self>,
        id: &str,
        person: Option<&oxbot_core::id::PersonId>,
        bytes: Option<listener::ConnBytes>,
    ) -> Online {
        self.set_online(id, true);
        let noted = bytes.and_then(|bytes| {
            let device = self.devices().into_iter().find(|g| g.id == id)?;
            let network = lock(&self.here)
                .as_ref()
                .map(|h| h.label.clone())
                .unwrap_or_default();
            let visit = self.hub.device_connected(&device, person, &network)?;
            let start = (bytes.written(), bytes.read());
            Some(Noted {
                visit,
                bytes,
                start,
            })
        });
        Online {
            door: Arc::clone(self),
            id: id.to_owned(),
            noted,
        }
    }

    fn set_online(&self, id: &str, connected: bool) {
        {
            let mut online = lock(&self.online);
            let n = online.entry(id.to_owned()).or_default();
            if connected {
                *n += 1;
            } else {
                *n = n.saturating_sub(1);
            }
        }
        self.announce_devices();
    }
}

/// A device's connection, counted as online while this lives.
#[derive(Debug)]
pub struct Online {
    door: Arc<Door>,
    id: String,
    noted: Option<Noted>,
}

/// A connection's row in Settings → Activity, and its byte counts when it
/// began.
#[derive(Debug)]
struct Noted {
    visit: oxbot_core::DeviceVisit,
    bytes: listener::ConnBytes,
    /// (written, read) when the row was made.
    start: (u64, u64),
}

impl Drop for Online {
    fn drop(&mut self) {
        self.door.set_online(&self.id, false);
        if let Some(n) = self.noted.take() {
            // Up: what the home computer sent the device.
            let up = n.bytes.written().saturating_sub(n.start.0);
            let down = n.bytes.read().saturating_sub(n.start.1);
            self.door.hub.device_disconnected(&n.visit, up, down);
        }
    }
}
