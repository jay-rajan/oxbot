//! oxbot on a phone: always a Gate, never the Farm.
//!
//! The window shows `gate.html` (bundled). Unpaired, it offers to scan the
//! Farm's QR code. Paired, it asks this module for the Farm's page and
//! shows it, with `window.oxbotGate` standing in for the WebSocket: the
//! page's messages travel through [`oxbot_gate`]'s TLS link, pinned to the
//! Farm's certificate. The page never holds the key and the phone opens no
//! ports.
//!
//! Away from home, the app shows the last copy of the Farm's page and the
//! phone's own copy of its chats ([`oxbot_gate::copy`]), read-only, until
//! the Farm can be reached again.
//!
//! Web links open in the phone's browser, never in the app's window.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use futures_util::{SinkExt as _, StreamExt as _};
use oxbot_gate::copy::ChatCopy;
use oxbot_gate::{GateError, Message, PairLink, Paired};
use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager as _, State};

use crate::links::{self, Link};
use tokio::sync::{Mutex, mpsc};

/// How often the link checks the Farm is still there.
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(15);
/// How long without a word from the Farm before the link counts as lost.
const SILENCE: std::time::Duration = std::time::Duration::from_secs(35);

/// What the app keeps: the pairing, the open link if any, and the chat copy.
#[derive(Default)]
struct Gate {
    paired: Mutex<Option<Paired>>,
    link: Mutex<Option<mpsc::Sender<String>>>,
    copy: std::sync::Mutex<Option<ChatCopy>>,
}

/// The saved pairing's format. A newer app reads older versions.
const SAVED_VERSION: u32 = 1;

/// `farm.json`: the pairing, with its format's version.
#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    paired: Paired,
}

/// What the page is told about the pairing.
#[derive(Debug, Clone, Serialize)]
struct Status {
    farm: String,
    person: String,
}

/// The Farm's page, and whether it's the copy from last time (the Farm
/// can't be reached).
#[derive(Debug, Clone, Serialize)]
struct Page {
    html: String,
    cached: bool,
}

impl From<&Paired> for Status {
    fn from(p: &Paired) -> Self {
        Self {
            farm: p.farm.name.clone(),
            person: p.person.clone(),
        }
    }
}

/// What the link tells the page.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Wire {
    /// Connected to the Farm.
    Open,
    /// A message from the Farm (JSON text).
    Msg { text: String },
    /// The link closed. `unpaired`: the Farm removed this phone.
    Closed { reason: String, unpaired: bool },
}

/// A file in the app's private folder, which other apps can't read and
/// which isn't backed up (see the Android manifest).
fn data_path(app: &AppHandle, name: &str) -> anyhow::Result<PathBuf> {
    Ok(app
        .path()
        .app_data_dir()
        .context("no app data folder")?
        .join(name))
}

/// Writes all at once: a crash mid-write leaves the old file, never half a
/// new one.
fn write_whole(path: &std::path::Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn save(app: &AppHandle, paired: &Paired) -> anyhow::Result<()> {
    let saved = Saved {
        version: SAVED_VERSION,
        paired: paired.clone(),
    };
    write_whole(&data_path(app, "farm.json")?, &serde_json::to_vec(&saved)?)
}

/// The saved pairing: `Ok(None)` if there's none, an error if it can't be
/// read (so the page can say so instead of quietly asking to pair again).
fn load(app: &AppHandle) -> anyhow::Result<Option<Paired>> {
    let path = data_path(app, "farm.json")?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("couldn't read the saved pairing"),
    };
    if let Ok(saved) = serde_json::from_slice::<Saved>(&bytes) {
        anyhow::ensure!(
            saved.version <= SAVED_VERSION,
            "the saved pairing is from a newer oxbot app; update this one"
        );
        return Ok(Some(saved.paired));
    }
    // Before versions: the pairing on its own. Save it in today's format.
    let paired: Paired =
        serde_json::from_slice(&bytes).context("the saved pairing is damaged; pair again")?;
    let _ = save(app, &paired);
    Ok(Some(paired))
}

/// Forgets the Farm: the pairing, the link, the chat copy and the page.
async fn forget(app: &AppHandle, gate: &Gate) {
    gate.paired.lock().await.take();
    gate.link.lock().await.take();
    if let Some(copy) = lock(&gate.copy).take() {
        copy.wipe();
    } else if let Ok(path) = data_path(app, "chats.db") {
        let _ = std::fs::remove_file(path);
    }
    for name in ["farm.json", "page.html"] {
        if let Ok(path) = data_path(app, name) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The pairing, loading it the first time.
async fn paired(app: &AppHandle, gate: &Gate) -> Result<Option<Paired>, String> {
    let mut paired = gate.paired.lock().await;
    if paired.is_none() {
        *paired = load(app).map_err(|e| format!("{e:#}"))?;
    }
    Ok(paired.clone())
}

/// Runs `f` on the chat copy, opening it the first time. The copy is a
/// convenience: if it fails, the link carries on without it.
fn with_copy(
    app: &AppHandle,
    gate: &Gate,
    person: &str,
    f: impl FnOnce(&mut ChatCopy) -> Result<(), oxbot_gate::copy::CopyError>,
) {
    let mut copy = lock(&gate.copy);
    if copy.is_none() {
        match data_path(app, "chats.db").map(|p| ChatCopy::open(&p, person)) {
            Ok(Ok(opened)) => *copy = Some(opened),
            Ok(Err(e)) => tracing::warn!(%e, "couldn't open the chat copy"),
            Err(e) => tracing::warn!("{e:#}"),
        }
    }
    if let Some(copy) = copy.as_mut()
        && let Err(e) = f(copy)
    {
        tracing::warn!(%e, "the chat copy failed");
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Puts the address that answered first, so it's tried first next time.
async fn remember_host(app: &AppHandle, gate: &Gate, host: &str) {
    let mut paired = gate.paired.lock().await;
    if let Some(p) = paired.as_mut()
        && p.farm.hosts.first().map(String::as_str) != Some(host)
    {
        p.farm.hosts.retain(|h| h != host);
        p.farm.hosts.insert(0, host.to_owned());
        let _ = save(app, p);
    }
}

/// This phone's model, e.g. `Pixel 10 Pro XL`.
fn device_name() -> String {
    std::process::Command::new("getprop")
        .arg("ro.product.model")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Phone".to_owned())
}

#[tauri::command]
async fn gate_status(app: AppHandle, gate: State<'_, Arc<Gate>>) -> Result<Option<Status>, String> {
    Ok(paired(&app, &gate).await?.as_ref().map(Status::from))
}

#[tauri::command]
async fn gate_pair(
    app: AppHandle,
    gate: State<'_, Arc<Gate>>,
    link: String,
) -> Result<Status, String> {
    let link = PairLink::parse(&link).map_err(|e| e.to_string())?;
    let paired = oxbot_gate::pair(&link, &device_name())
        .await
        .map_err(|e| e.to_string())?;
    save(&app, &paired).map_err(|e| format!("couldn't save the pairing: {e:#}"))?;
    let status = Status::from(&paired);
    // A new pairing starts a new copy (it may be another person's).
    if let Some(copy) = lock(&gate.copy).take() {
        copy.wipe();
    }
    *gate.paired.lock().await = Some(paired);
    Ok(status)
}

/// Looks for a QR code in one camera frame. `frame` is the frame's
/// brightness, one character per pixel (code point 0x100 + brightness, so
/// JSON never has to escape it), `width` by
/// `height`. Text, not raw bytes, because Android's bridge between the page
/// and Rust carries only text. Runs on the phone; nothing is sent anywhere.
#[tauri::command]
fn gate_scan(frame: &str, width: usize, height: usize) -> Result<Option<String>, String> {
    let luma = frame_bytes(frame, width, height)?;
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| luma[y * width + x]);
    Ok(image
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .find(|text| text.starts_with("oxbot://pair")))
}

/// What the page adds to each brightness byte (see `gate_scan`).
const FRAME_SHIFT: u32 = 0x100;

/// Turns the page's frame text back into one brightness byte per pixel.
fn frame_bytes(frame: &str, width: usize, height: usize) -> Result<Vec<u8>, String> {
    if !(1..=4096).contains(&width) || !(1..=4096).contains(&height) {
        return Err("bad frame size".to_owned());
    }
    let luma = frame
        .chars()
        .map(|c| {
            u32::from(c)
                .checked_sub(FRAME_SHIFT)
                .and_then(|b| u8::try_from(b).ok())
                .ok_or_else(|| "the frame isn't brightness bytes".to_owned())
        })
        .collect::<Result<Vec<u8>, String>>()?;
    if luma.len() != width * height {
        return Err("the frame's size doesn't match".to_owned());
    }
    Ok(luma)
}

#[tauri::command]
async fn gate_forget(app: AppHandle, gate: State<'_, Arc<Gate>>) -> Result<(), String> {
    forget(&app, &gate).await;
    Ok(())
}

/// The Farm's page, or the copy from last time if the Farm can't be
/// reached. `"unpaired"` if the Farm removed this phone.
#[tauri::command]
async fn gate_page(app: AppHandle, gate: State<'_, Arc<Gate>>) -> Result<Page, String> {
    let Some(paired) = paired(&app, &gate).await? else {
        return Err("unpaired".to_owned());
    };
    match oxbot_gate::fetch(&paired, "/").await {
        Ok((host, page)) => {
            remember_host(&app, &gate, &host).await;
            if let Ok(path) = data_path(&app, "page.html") {
                let _ = write_whole(&path, &page);
            }
            Ok(Page {
                html: String::from_utf8_lossy(&page).into_owned(),
                cached: false,
            })
        }
        Err(GateError::Unpaired) => {
            forget(&app, &gate).await;
            Err("unpaired".to_owned())
        }
        Err(e) => match data_path(&app, "page.html").map(std::fs::read_to_string) {
            Ok(Ok(html)) => Ok(Page { html, cached: true }),
            _ => Err(e.to_string()),
        },
    }
}

/// Opens the link to the Farm; what arrives goes to `events`. Replaces any
/// link already open.
#[tauri::command]
async fn gate_open(
    app: AppHandle,
    gate: State<'_, Arc<Gate>>,
    events: Channel<Wire>,
) -> Result<(), String> {
    let gate = Arc::clone(&gate);
    let Some(paired) = paired(&app, &gate).await? else {
        let _ = events.send(Wire::Closed {
            reason: "not paired".to_owned(),
            unpaired: true,
        });
        return Ok(());
    };
    let (tx, mut outgoing) = mpsc::channel::<String>(64);
    *gate.link.lock().await = Some(tx.clone());
    tauri::async_runtime::spawn(async move {
        let socket = match oxbot_gate::connect(&paired).await {
            Ok((host, socket)) => {
                remember_host(&app, &gate, &host).await;
                socket
            }
            Err(e) => {
                let unpaired = matches!(e, GateError::Unpaired);
                if unpaired {
                    forget(&app, &gate).await;
                } else {
                    // Away from the Farm: show the chats from the phone's copy.
                    let mut kept = Vec::new();
                    with_copy(&app, &gate, &paired.person, |copy| {
                        kept = copy.replay()?;
                        Ok(())
                    });
                    for text in kept {
                        let _ = events.send(Wire::Msg { text });
                    }
                }
                let _ = events.send(Wire::Closed {
                    reason: e.to_string(),
                    unpaired,
                });
                return;
            }
        };
        let _ = events.send(Wire::Open);
        let (mut sink, mut stream) = socket.split();
        // A phone that walks out of Wi-Fi range often gets no error, just
        // silence: ping every 15 s and give up after 35 s without a word.
        let mut beat = tokio::time::interval(HEARTBEAT);
        let mut heard = tokio::time::Instant::now();
        let reason = loop {
            tokio::select! {
                frame = stream.next() => {
                    heard = tokio::time::Instant::now();
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            with_copy(&app, &gate, &paired.person, |copy| copy.record(&text, now_ms()));
                            if events.send(Wire::Msg { text: text.to_string() }).is_err() {
                                break "the page went away".to_owned();
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break "the home computer closed the connection".to_owned(),
                        Some(Err(e)) => break e.to_string(),
                        Some(Ok(_)) => {}
                    }
                }
                out = outgoing.recv() => match out {
                    Some(text) => {
                        if let Err(e) = sink.send(Message::Text(text.into())).await {
                            break e.to_string();
                        }
                    }
                    // Replaced by a newer link, or closed by the page.
                    None => break "closed".to_owned(),
                },
                _ = beat.tick() => {
                    if heard.elapsed() > SILENCE {
                        break "the home computer stopped answering".to_owned();
                    }
                    if let Err(e) = sink.send(Message::Ping(Vec::new().into())).await {
                        break e.to_string();
                    }
                }
            }
        };
        let _ = sink.close().await;
        let _ = events.send(Wire::Closed {
            reason,
            unpaired: false,
        });
    });
    // Only this function's copy and the stored one hold the sender; drop
    // ours so replacing the stored one ends the old link.
    drop(tx);
    Ok(())
}

#[tauri::command]
async fn gate_send(gate: State<'_, Arc<Gate>>, text: String) -> Result<(), String> {
    let link = gate.link.lock().await.clone();
    match link {
        Some(tx) => tx.send(text).await.map_err(|_| "not connected".to_owned()),
        None => Err("not connected".to_owned()),
    }
}

#[tauri::command]
async fn gate_close(gate: State<'_, Arc<Gate>>) -> Result<(), String> {
    gate.link.lock().await.take();
    Ok(())
}

/// Opens `url` (an http(s) address) in the phone's browser, with an
/// Android "view" intent, so the app's window stays on oxbot's page.
fn open_outside(app: &AppHandle, url: String) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.with_webview(move |webview| {
        webview.jni_handle().exec(move |env, activity, _| {
            if let Err(err) = view_intent(env, activity, &url) {
                tracing::warn!(%err, "couldn't open a link in the browser");
            }
        });
    });
}

/// `activity.startActivity(new Intent(ACTION_VIEW, Uri.parse(url)))`, in a
/// new task.
fn view_intent(
    env: &mut jni::JNIEnv<'_>,
    activity: &jni::objects::JObject<'_>,
    url: &str,
) -> jni::errors::Result<()> {
    use jni::objects::JValue;
    /// `Intent.FLAG_ACTIVITY_NEW_TASK`.
    const NEW_TASK: i32 = 0x1000_0000;
    let text = env.new_string(url)?;
    let uri = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValue::Object(&text)],
        )?
        .l()?;
    let action = env.new_string("android.intent.action.VIEW")?;
    let intent = env.new_object(
        "android/content/Intent",
        "(Ljava/lang/String;Landroid/net/Uri;)V",
        &[JValue::Object(&action), JValue::Object(&uri)],
    )?;
    env.call_method(
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[JValue::Int(NEW_TASK)],
    )?;
    env.call_method(
        activity,
        "startActivity",
        "(Landroid/content/Intent;)V",
        &[JValue::Object(&intent)],
    )?;
    Ok(())
}

/// Opens the Gate window.
pub fn run() -> anyhow::Result<()> {
    tauri::Builder::default()
        .manage(Arc::new(Gate::default()))
        // The window is made here, not in the config, to keep it on
        // oxbot's page: web links (a report's sources, links in a chat)
        // open in the phone's browser instead.
        .setup(|app| {
            let handle = app.handle().clone();
            let own = tauri::Url::parse("http://tauri.localhost/")?;
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("gate.html".into()),
            )
            .title("oxbot")
            .on_navigation(move |to| {
                // The app's own pages come from tauri.localhost (http or https).
                if to.host_str() == Some("tauri.localhost") {
                    return true;
                }
                match links::decide(to, &own) {
                    Link::Stay => true,
                    Link::Outside(url) => {
                        open_outside(&handle, url);
                        false
                    }
                    Link::Block => false,
                }
            })
            .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            gate_status,
            gate_pair,
            gate_scan,
            gate_forget,
            gate_page,
            gate_open,
            gate_send,
            gate_close
        ])
        .run(tauri::generate_context!())
        .context("couldn't run the app")
}

#[cfg(test)]
mod tests {
    use super::frame_bytes;

    fn text(bytes: &[u8]) -> String {
        bytes
            .iter()
            .map(|b| char::from_u32(u32::from(*b) + 0x100).unwrap())
            .collect()
    }

    #[test]
    fn a_frame_reads_back_as_its_brightness() {
        assert_eq!(
            frame_bytes(&text(&[0, 31, 128, 255]), 2, 2),
            Ok(vec![0, 31, 128, 255])
        );
    }

    #[test]
    fn a_frame_of_the_wrong_size_or_text_is_refused() {
        assert!(frame_bytes(&text(&[1, 2, 3]), 2, 2).is_err());
        assert!(frame_bytes("ab", 2, 1).is_err());
        assert!(frame_bytes("", 0, 0).is_err());
    }
}
