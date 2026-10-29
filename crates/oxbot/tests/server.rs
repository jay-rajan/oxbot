//! Starts the real server on a free port with the fake `claude`, and talks to
//! it over a WebSocket like the apps do.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use oxbot::server::{self, AppControls, AppState};
use oxbot_core::{Bot, BotId, Hub, HubConfig, Store};
use oxbot_protocol::{ClientMsg, EventKind, ServerHello, ServerMsg};
use tokio_tungstenite::tungstenite::Message;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A stand-in for the desktop app's Launch at login switch.
#[derive(Debug, Default)]
struct FakeApp(Mutex<bool>);

impl AppControls for FakeApp {
    fn launch_at_login(&self) -> Option<bool> {
        Some(*self.0.lock().unwrap())
    }
    fn set_launch_at_login(&self, on: bool) -> Result<(), String> {
        *self.0.lock().unwrap() = on;
        Ok(())
    }
}

async fn start_with(dir: &std::path::Path, app: Option<Arc<dyn AppControls>>) -> String {
    let claude_tests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../oxbot-claude/tests");
    let bot = Bot::create(dir, BotId::new("assistant").unwrap(), "Assistant", "").unwrap();
    let config = HubConfig {
        claude_program: claude_tests.join("fake-claude.sh"),
        extra_env: vec![(
            "FAKE_CLAUDE_FIXTURE".into(),
            claude_tests
                .join("fixtures/tool-use.jsonl")
                .display()
                .to_string(),
        )],
        ..HubConfig::new(dir)
    };
    let hub = Hub::new(vec![bot], Store::open_in_memory().unwrap(), config);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let state = AppState {
        data_dir: dir.to_owned(),
        app,
        local_person: "Jay".to_owned(),
        ..AppState::new(hub, "test-mac")
    };
    tokio::spawn(server::serve(listener, state, std::future::pending()));
    url
}

async fn start(dir: &std::path::Path) -> String {
    start_with(dir, None).await
}

async fn send(ws: &mut Ws, msg: &ClientMsg) {
    ws.send(Message::Text(serde_json::to_string(msg).unwrap().into()))
        .await
        .unwrap();
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

/// Connects and reads the opening messages: hello, settings, setup, usage,
/// people.
async fn connect(url: &str) -> (Ws, ServerHello, oxbot_protocol::AppSettings) {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let ServerMsg::Hello(hello) = next(&mut ws).await else {
        panic!("expected hello")
    };
    let ServerMsg::Settings(settings) = next(&mut ws).await else {
        panic!("expected settings")
    };
    assert!(matches!(next(&mut ws).await, ServerMsg::Setup(_)));
    assert!(matches!(next(&mut ws).await, ServerMsg::Usage(_)));
    assert!(matches!(next(&mut ws).await, ServerMsg::People { .. }));
    assert!(matches!(next(&mut ws).await, ServerMsg::Sites { .. }));
    (ws, hello, settings)
}

/// Collects stored events until the turn is done (deltas are skipped).
async fn turn(ws: &mut Ws) -> Vec<EventKind> {
    let mut kinds = Vec::new();
    loop {
        if let ServerMsg::Event(e) = next(ws).await {
            let done = matches!(e.kind, EventKind::TurnDone { .. });
            kinds.push(e.kind);
            if done {
                return kinds;
            }
        }
    }
}

#[tokio::test]
async fn chat_over_websocket_and_catch_up_later() {
    let tmp = tempfile::tempdir().unwrap();
    let url = start(tmp.path()).await;

    let (mut ws, hello, settings) = connect(&url).await;
    assert_eq!(hello.server_name, "test-mac");
    assert!(hello.is_compatible());
    assert_eq!(settings.launch_at_login, None, "not the desktop app");
    assert_eq!(settings.you, "Jay", "messages are sent as the Mac's person");

    send(&mut ws, &ClientMsg::ListBots).await;
    let ServerMsg::Bots { bots } = next(&mut ws).await else {
        panic!("expected bots")
    };
    assert_eq!(bots[0].id, "assistant");
    assert_eq!(
        bots[0].browser_state,
        oxbot_protocol::BrowserState::Disabled
    );

    send(
        &mut ws,
        &ClientMsg::Subscribe {
            bot: "assistant".into(),
            since: 0,
        },
    )
    .await;
    send(
        &mut ws,
        &ClientMsg::Send {
            bot: "assistant".into(),
            text: "save a note".into(),
        },
    )
    .await;
    let kinds = turn(&mut ws).await;
    assert_eq!(kinds.len(), 5);
    assert_eq!(
        kinds[0],
        EventKind::UserMessage {
            text: "save a note".into(),
            from: Some("Jay".into()),
        }
    );

    // A second client (the phone, later) catches up from seq 2.
    let (mut phone, _, _) = connect(&url).await;
    send(
        &mut phone,
        &ClientMsg::Subscribe {
            bot: "assistant".into(),
            since: 2,
        },
    )
    .await;
    let ServerMsg::Event(first) = next(&mut phone).await else {
        panic!("expected an event")
    };
    assert_eq!(first.seq, 3);
}

#[tokio::test]
async fn bad_requests_get_errors_not_disconnects() {
    let tmp = tempfile::tempdir().unwrap();
    let url = start(tmp.path()).await;
    let (mut ws, _, _) = connect(&url).await;

    ws.send(Message::Text("{\"type\":\"nope\"}".into()))
        .await
        .unwrap();
    assert!(matches!(next(&mut ws).await, ServerMsg::Error { .. }));

    send(
        &mut ws,
        &ClientMsg::Subscribe {
            bot: "ghost".into(),
            since: 0,
        },
    )
    .await;
    let ServerMsg::Error { message } = next(&mut ws).await else {
        panic!("expected an error")
    };
    assert!(message.contains("ghost"));

    send(&mut ws, &ClientMsg::SetLaunchAtLogin { on: true }).await;
    let ServerMsg::Error { message } = next(&mut ws).await else {
        panic!("expected an error")
    };
    assert!(message.contains("oxbot app"));
}

#[tokio::test]
async fn the_page_is_served_and_other_websites_are_refused() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let tmp = tempfile::tempdir().unwrap();
    let url = start(tmp.path()).await;
    let addr = url
        .trim_start_matches("ws://")
        .trim_end_matches("/ws")
        .to_owned();

    let mut tcp = tokio::net::TcpStream::connect(&addr).await.unwrap();
    tcp.write_all(
        format!("GET / HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut page = String::new();
    tcp.read_to_string(&mut page).await.unwrap();
    assert!(page.starts_with("HTTP/1.1 200"));
    assert!(page.contains("<title>oxbot</title>"));

    let with_origin = |origin: &str| {
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert("Origin", origin.parse().unwrap());
        req
    };
    assert!(
        tokio_tungstenite::connect_async(with_origin("https://evil.example"))
            .await
            .is_err(),
        "a web page from another site must not reach the bots"
    );
    assert!(
        tokio_tungstenite::connect_async(with_origin(&format!("http://{addr}")))
            .await
            .is_ok()
    );
}

/// The reply to a create and the list for everyone, which can arrive in
/// either order.
async fn created(ws: &mut Ws) -> (String, Vec<String>) {
    let (mut id, mut list) = (None, None);
    while id.is_none() || list.is_none() {
        match next(ws).await {
            ServerMsg::BotCreated(info) => id = Some(info.id),
            ServerMsg::Bots { bots } => list = Some(bots.into_iter().map(|b| b.id).collect()),
            other => panic!("unexpected {other:?}"),
        }
    }
    (id.unwrap(), list.unwrap())
}

#[tokio::test]
async fn bots_are_created_from_a_name() {
    let tmp = tempfile::tempdir().unwrap();
    let url = start(tmp.path()).await;
    let (mut ws, _, _) = connect(&url).await;

    let create = ClientMsg::CreateBot {
        name: "Daily News".into(),
        instructions: "Summarize the news.".into(),
    };
    send(&mut ws, &create).await;
    let (id, list) = created(&mut ws).await;
    assert_eq!(id, "daily-news");
    assert_eq!(list, ["assistant", "daily-news"]);

    send(&mut ws, &create).await;
    let (id, _) = created(&mut ws).await;
    assert_eq!(
        id, "daily-news-2",
        "a second bot with the same name gets its own id"
    );
}

#[tokio::test]
async fn the_app_switches_launch_at_login() {
    let tmp = tempfile::tempdir().unwrap();
    let url = start_with(tmp.path(), Some(Arc::new(FakeApp::default()))).await;
    let (mut ws, _, settings) = connect(&url).await;
    assert_eq!(settings.launch_at_login, Some(false));
    assert_eq!(settings.data_dir, tmp.path().display().to_string());

    send(&mut ws, &ClientMsg::SetLaunchAtLogin { on: true }).await;
    let ServerMsg::Settings(settings) = next(&mut ws).await else {
        panic!("expected new settings")
    };
    assert_eq!(settings.launch_at_login, Some(true));
}
