//! Files and dictation through the real server: `POST /upload`, sending
//! with files, opening them, and the Mac's microphone.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use oxbot::server::{self, AppState};
use oxbot_core::testing::{self, FakeBots, On, Tool};
use oxbot_core::{Bot, BotId, Hub, HubConfig, Store};
use oxbot_protocol::{Attachment, AttachmentKind, ClientMsg, EventKind, ServerMsg};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Server {
    addr: std::net::SocketAddr,
    key: String,
    work: PathBuf,
    _dir: tempfile::TempDir,
}

/// A server in test mode (`as=` names a person) with Herder, a shared
/// `notes` bot and Sam's private `diary`, and the stand-in Mac helper.
async fn start() -> Server {
    start_with(false).await
}

/// As [`start`]; with `farm_person`, Jay is chosen as the Farm's person.
async fn start_with(farm_person: bool) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let sam = store.find_or_create_person("Sam").unwrap().0.id;
    let herder = Bot::create(dir.path(), BotId::new("assistant").unwrap(), "Herder", "").unwrap();
    let notes = Bot::create(dir.path(), BotId::new("notes").unwrap(), "Notes", "").unwrap();
    let mut diary = Bot::create(dir.path(), BotId::new("diary").unwrap(), "Diary", "").unwrap();
    diary.owner = Some(sam);
    diary.created_by = Some("Sam".into());
    diary.private = true;
    diary.save().unwrap();
    let work = notes.work_dir();
    let fake = Arc::new(FakeBots::new(dir.path(), Tool::Note));
    let config = HubConfig {
        browser: None,
        ..testing::config(dir.path(), &dir.path().join(".home"), &fake, &On::Loop)
    };
    let hub = Hub::new(vec![herder, notes, diary], store, config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fake-oxbot-mac.sh");
    let state = AppState {
        local_person: "Jay".to_owned(),
        e2e: true,
        dictation: Arc::new(oxbot::dictation::Dictation::new(Some(helper))),
        ..AppState::new(hub, "test-mac")
    };
    state.refresh_setup().await;
    if farm_person {
        // Chooses Jay as the Farm's person.
        let _ = state.farm_viewer();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let key = state.farm_key.clone();
    tokio::spawn(server::serve(listener, state, std::future::pending()));
    Server {
        addr,
        key,
        work,
        _dir: dir,
    }
}

/// A raw HTTP request: (status, body).
async fn request(addr: std::net::SocketAddr, head: &str, body: &[u8]) -> (u16, String) {
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (mut read, mut write) = tcp.into_split();
    write.write_all(head.as_bytes()).await.unwrap();
    // The server may answer (and stop reading) before the body is all sent.
    let body = body.to_vec();
    let writer = tokio::spawn(async move {
        let _ = write.write_all(&body).await;
        write
    });
    let mut reply = Vec::new();
    let _ = read.read_to_end(&mut reply).await;
    drop(writer);
    let reply = String::from_utf8_lossy(&reply).into_owned();
    let status = reply[9..12].parse().unwrap();
    let body = reply
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// Uploads `body` as `name` for `bot`, with `extra` headers.
async fn upload(s: &Server, query: &str, extra: &str, body: &[u8]) -> (u16, serde_json::Value) {
    let head = format!(
        "POST /upload?{query} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n",
        body.len()
    );
    let (status, text) = request(s.addr, &head, body).await;
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)),
    )
}

fn farm(s: &Server) -> String {
    format!(
        "X-Oxbot-Key: {}\r\nOrigin: http://127.0.0.1:{}\r\n",
        s.key,
        s.addr.port()
    )
}

async fn connect(url: &str) -> Ws {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    // Everything sent on connect, up to the sections (always sent).
    loop {
        if matches!(next(&mut ws).await, ServerMsg::Sections { .. }) {
            return ws;
        }
    }
}

async fn next(ws: &mut Ws) -> ServerMsg {
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap();
    match frame.unwrap().unwrap() {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("unexpected frame {other:?}"),
    }
}

/// The next message `pick` takes.
async fn wait<T>(ws: &mut Ws, mut pick: impl FnMut(ServerMsg) -> Option<T>) -> T {
    loop {
        if let Some(t) = pick(next(ws).await) {
            return t;
        }
    }
}

async fn send(ws: &mut Ws, msg: &ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(msg).unwrap().into()))
        .await
        .unwrap();
}

fn files_in(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |d| {
        d.flatten()
            .map(|e| {
                if e.path().is_dir() {
                    files_in(&e.path())
                } else {
                    1
                }
            })
            .sum()
    })
}

#[tokio::test]
async fn uploads_need_the_farm_or_a_test_person_and_this_page() {
    let s = start().await;
    let (status, _) = upload(&s, "bot=notes&name=a.txt", "", b"hi").await;
    assert_eq!(status, 401, "no Farm secret");
    let foreign = format!("X-Oxbot-Key: {}\r\nOrigin: https://evil.example\r\n", s.key);
    let (status, _) = upload(&s, "bot=notes&name=a.txt", &foreign, b"hi").await;
    assert_eq!(status, 403, "another website's page");
    let (status, body) = upload(&s, "bot=diary&name=a.txt&as=Ann", "", b"hi").await;
    assert_eq!(status, 404, "Sam's private bot is hidden from Ann: {body}");
    let (status, body) = upload(&s, "bot=diary&name=a.txt&as=Sam", "", b"hi").await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = upload(&s, "bot=notes&name=a.txt", &farm(&s), b"hi").await;
    assert_eq!(status, 200, "{body}");
    let file: Attachment = serde_json::from_value(body).unwrap();
    assert_eq!(
        (file.name.as_str(), file.kind, file.size),
        ("a.txt", AttachmentKind::Text, 2)
    );
}

#[tokio::test]
async fn anyone_who_may_task_a_shared_bot_sends_it_files() {
    let s = start_with(true).await;
    let (status, body) = upload(&s, "bot=notes&name=a.txt&as=Ann", "", b"hi").await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = upload(&s, "bot=notes&name=b.txt", &farm(&s), b"hi").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(files_in(&s.work), 2);
}

#[tokio::test]
async fn too_big_or_unreadable_files_get_a_plain_message_and_leave_nothing() {
    let s = start().await;
    let head = format!(
        "POST /upload?bot=notes&name=clip.mov HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n",
        640 * 1024 * 1024,
        farm(&s)
    );
    let (status, body) = request(s.addr, &head, b"").await;
    assert_eq!(status, 413);
    assert!(
        body.contains("“clip.mov” is 640 MB; oxbot takes videos up to 200 MB."),
        "{body}"
    );

    let big = vec![b'a'; 26 * 1024 * 1024];
    let (status, body) = upload(&s, "bot=notes&name=big.bin", &farm(&s), &big).await;
    assert_eq!(status, 413, "{body}");
    assert_eq!(
        body["error"],
        "“big.bin” is 26 MB; oxbot takes files up to 25 MB."
    );

    // Sent without saying its size: counted as it comes, then refused.
    let mut chunked = Vec::new();
    for _ in 0..26 {
        chunked.extend_from_slice(b"100000\r\n");
        chunked.extend_from_slice(&vec![b'a'; 1024 * 1024]);
        chunked.extend_from_slice(b"\r\n");
    }
    chunked.extend_from_slice(b"0\r\n\r\n");
    let head = format!(
        "POST /upload?bot=notes&name=notes HTTP/1.1\r\nHost: x\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n{}\r\n",
        farm(&s)
    );
    let (status, body) = request(s.addr, &head, &chunked).await;
    assert_eq!(status, 413, "{body}");
    assert!(body.contains("oxbot takes text files up to 2 MB"), "{body}");

    let (status, body) = upload(
        &s,
        "bot=notes&name=photos.zip",
        &farm(&s),
        b"PK\x03\x04junk",
    )
    .await;
    assert_eq!(status, 415);
    assert!(
        body["error"].as_str().unwrap().contains("Unzip it"),
        "{body}"
    );
    assert_eq!(files_in(&s.work.join("inbox")), 0, "nothing left behind");
}

#[tokio::test]
async fn a_file_goes_with_a_message_and_opens_on_the_farm_only() {
    let s = start().await;
    let (_, body) = upload(
        &s,
        "bot=notes&name=list.csv",
        &farm(&s),
        b"name,qty\nflour,2\n",
    )
    .await;
    let file: Attachment = serde_json::from_value(body).unwrap();
    let mut ws = connect(&format!("ws://{}/ws?k={}", s.addr, s.key)).await;
    send(
        &mut ws,
        &ClientMsg::OpenBot {
            bot: "notes".into(),
            limit: None,
        },
    )
    .await;
    send(
        &mut ws,
        &ClientMsg::Send {
            bot: "notes".into(),
            text: String::new(),
            files: vec![file.id.clone()],
        },
    )
    .await;
    let (seq, files) = wait(&mut ws, |m| match m {
        ServerMsg::Event(e) => match e.kind {
            EventKind::UserMessage { files, .. } => Some((e.seq, files)),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(files, std::slice::from_ref(&file));

    let open = ClientMsg::OpenFile {
        bot: "notes".into(),
        seq,
        id: file.id.clone(),
    };
    send(&mut ws, &open).await;
    let said = wait(&mut ws, |m| match m {
        ServerMsg::Notice { message, .. } => Some(message),
        _ => None,
    })
    .await;
    assert_eq!(said, "Opening list.csv on the home computer.");

    let mut ann = connect(&format!("ws://{}/ws?as=Ann", s.addr)).await;
    for msg in [open, ClientMsg::Dictate { on: true }] {
        send(&mut ann, &msg).await;
        let said = wait(&mut ann, |m| match m {
            ServerMsg::Error { message } => Some(message),
            _ => None,
        })
        .await;
        assert_eq!(said, "That can only be done on the home computer.");
    }
}

#[tokio::test]
async fn dictation_text_goes_only_to_the_connection_that_asked() {
    let s = start().await;
    let url = format!("ws://{}/ws?k={}", s.addr, s.key);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let settings = wait(&mut ws, |m| match m {
        ServerMsg::Settings(s) => Some(s),
        _ => None,
    })
    .await;
    assert!(settings.farm && settings.dictation, "the Farm gets the mic");
    let (mut ann, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws?as=Ann", s.addr))
        .await
        .unwrap();
    let theirs = wait(&mut ann, |m| match m {
        ServerMsg::Settings(s) => Some(s),
        _ => None,
    })
    .await;
    assert!(!theirs.dictation, "a Gate never gets the Mac's mic");

    let mut other = connect(&url).await;
    send(&mut ws, &ClientMsg::Dictate { on: true }).await;
    let heard = wait(&mut ws, |m| match m {
        ServerMsg::Dictation {
            text, done: false, ..
        } if text.contains("call") => Some(text),
        _ => None,
    })
    .await;
    assert_eq!(heard, "Remind me to call");
    send(&mut ws, &ClientMsg::Dictate { on: false }).await;
    let (text, error) = wait(&mut ws, |m| match m {
        ServerMsg::Dictation {
            text,
            done: true,
            error,
        } => Some((text, error)),
        _ => None,
    })
    .await;
    assert_eq!(
        (text.as_str(), error),
        ("Remind me to call the plumber", None)
    );
    // The other Farm window heard nothing.
    send(&mut other, &ClientMsg::ListBots).await;
    let first = next(&mut other).await;
    assert!(matches!(first, ServerMsg::Bots { .. }), "{first:?}");
}
