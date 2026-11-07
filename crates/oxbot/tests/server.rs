//! Starts the real server on a free port with stand-in bots on oxbot's own
//! loop (`oxbot_core::testing`), and talks to it over a WebSocket like the
//! apps do.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use oxbot::server::{self, AppControls, AppState};
use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{Bot, BotId, Hub, HubConfig, Store};
use oxbot_protocol::{ClientMsg, EventKind, ServerHello, ServerMsg};
use tokio_tungstenite::tungstenite::Message;

/// The `codex` program bots run on, if this Mac has Codex 0.148 (found
/// once). Without it bots can't take turns: a test that needs one says
/// why and returns.
async fn codex() -> Option<&'static std::path::Path> {
    static CODEX: tokio::sync::OnceCell<Option<std::path::PathBuf>> =
        tokio::sync::OnceCell::const_new();
    CODEX
        .get_or_init(|| async {
            let probe = std::env::temp_dir().join(format!("oxbot-tests-{}", std::process::id()));
            testing::codex(&probe).await
        })
        .await
        .as_deref()
}

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
    start_with_store(dir, app, Store::open_in_memory().unwrap()).await
}

async fn start_with_store(
    dir: &std::path::Path,
    app: Option<Arc<dyn AppControls>>,
    store: Store,
) -> String {
    let bot = Bot::create(dir, BotId::new("assistant").unwrap(), "Assistant", "").unwrap();
    let fake = Arc::new(FakeBots::new(dir, Tool::Note));
    let config = HubConfig {
        browser: None,
        ..testing::config(dir, &dir.join(".home"), &fake, codex().await)
    };
    let hub = Hub::new(vec![bot], store, config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = AppState {
        data_dir: dir.to_owned(),
        app,
        local_person: "Jay".to_owned(),
        ..AppState::new(hub, "test-mac")
    };
    // The Farm's own window carries the Farm secret.
    let url = format!(
        "ws://{}/ws?k={}",
        listener.local_addr().unwrap(),
        state.farm_key
    );
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

/// Connects and reads the opening messages: hello, settings, setup,
/// people, your own profile, the home and own site lists, the websites
/// for several steps at once, and your own
/// sections.
async fn connect(url: &str) -> (Ws, ServerHello, oxbot_protocol::AppSettings) {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let ServerMsg::Hello(hello) = next(&mut ws).await else {
        panic!("expected hello")
    };
    let ServerMsg::Settings(settings) = next(&mut ws).await else {
        panic!("expected settings")
    };
    assert!(matches!(next(&mut ws).await, ServerMsg::Setup(_)));
    assert!(matches!(next(&mut ws).await, ServerMsg::People { .. }));
    assert!(matches!(next(&mut ws).await, ServerMsg::Profile(_)));
    assert!(matches!(
        next(&mut ws).await,
        ServerMsg::Sites { owner: None, .. }
    ));
    assert!(matches!(
        next(&mut ws).await,
        ServerMsg::Sites { owner: Some(_), .. }
    ));
    assert!(matches!(next(&mut ws).await, ServerMsg::StepSites { .. }));
    assert!(matches!(next(&mut ws).await, ServerMsg::Sections { .. }));
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
    if codex().await.is_none() {
        return;
    }
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
            files: vec![],
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
            files: vec![],
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
async fn opening_a_long_chat_sends_one_page_and_pages_back() {
    const SEEDED: u64 = 5_000;
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let id = BotId::new("assistant").unwrap();
    for i in 1..=SEEDED {
        let kind = if i % 10 == 0 {
            EventKind::AssistantMessage {
                text: format!("reply {i}"),
            }
        } else {
            EventKind::ToolUse {
                name: "Read".into(),
                summary: format!("step {i}"),
            }
        };
        store.append(&id, kind).unwrap();
    }
    let url = start_with_store(tmp.path(), None, store).await;
    let (mut ws, _, _) = connect(&url).await;

    // The list knows the latest numbers without reading the history.
    send(&mut ws, &ClientMsg::ListBots).await;
    let ServerMsg::Bots { bots } = next(&mut ws).await else {
        panic!("expected bots")
    };
    assert_eq!((bots[0].last_seq, bots[0].last_reply_seq), (SEEDED, SEEDED));

    send(
        &mut ws,
        &ClientMsg::OpenBot {
            bot: "assistant".into(),
            limit: None,
        },
    )
    .await;
    let ServerMsg::History {
        bot,
        events,
        first_seq,
        has_older,
    } = next(&mut ws).await
    else {
        panic!("expected one history page")
    };
    assert_eq!(bot, "assistant");
    assert!(!events.is_empty() && events.len() <= 200);
    assert!(has_older);
    assert_eq!(first_seq, events[0].seq);
    assert_eq!(events.last().unwrap().seq, SEEDED);
    // Nothing else was replayed: the next frame answers the next request.
    send(&mut ws, &ClientMsg::ListBots).await;
    assert!(matches!(next(&mut ws).await, ServerMsg::Bots { .. }));

    // Older pages go back to the start, in order, at most 500 at a time.
    let mut seen: Vec<u64> = events.iter().map(|e| e.seq).collect();
    let (mut before, mut more) = (first_seq, has_older);
    while more {
        send(
            &mut ws,
            &ClientMsg::Older {
                bot: "assistant".into(),
                before,
                limit: Some(10_000),
            },
        )
        .await;
        let ServerMsg::History {
            events,
            first_seq,
            has_older,
            ..
        } = next(&mut ws).await
        else {
            panic!("expected an older page")
        };
        assert!(!events.is_empty() && events.len() <= 500);
        assert!(events.iter().all(|e| e.seq < before));
        let mut page: Vec<u64> = events.iter().map(|e| e.seq).collect();
        page.extend(seen);
        seen = page;
        (before, more) = (first_seq, has_older);
    }
    assert_eq!(seen, (1..=SEEDED).collect::<Vec<_>>());

    // Opening subscribed: a new turn streams in after the page.
    send(
        &mut ws,
        &ClientMsg::Send {
            bot: "assistant".into(),
            text: "one more".into(),
            files: vec![],
        },
    )
    .await;
    let first = loop {
        if let ServerMsg::Event(e) = next(&mut ws).await {
            break e;
        }
    };
    assert_eq!(first.seq, SEEDED + 1);
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
        .split('/')
        .next()
        .unwrap()
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

/// One plain HTTP GET; the whole response, headers included.
async fn http_get(addr: &str, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tcp.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut response = String::new();
    tcp.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test]
async fn the_react_page_is_served_at_next_with_a_strict_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let url = start(tmp.path()).await;
    let addr = url
        .trim_start_matches("ws://")
        .split('/')
        .next()
        .unwrap()
        .to_owned();

    let page = http_get(&addr, "/next/").await.to_ascii_lowercase();
    assert!(page.starts_with("http/1.1 200"), "{page}");
    assert!(page.contains("content-type: text/html"));
    assert!(page.contains("content-security-policy: default-src 'self'; script-src 'self';"));
    assert!(page.contains("src=\"./app.js\""));
    assert!(!page.contains("<script>"), "no inline scripts");

    let js = http_get(&addr, "/next/app.js").await.to_ascii_lowercase();
    assert!(js.starts_with("http/1.1 200"));
    assert!(js.contains("content-type: text/javascript"));
    let css = http_get(&addr, "/next/app.css").await.to_ascii_lowercase();
    assert!(css.starts_with("http/1.1 200"));
    assert!(css.contains("content-type: text/css"));

    // Without the slash, relative files wouldn't resolve: it redirects.
    let bare = http_get(&addr, "/next").await.to_ascii_lowercase();
    assert!(bare.starts_with("http/1.1 308"));
    assert!(bare.contains("location: /next/"));
    assert!(
        http_get(&addr, "/next/other.js")
            .await
            .starts_with("HTTP/1.1 404")
    );
}

#[tokio::test]
async fn being_on_this_mac_is_not_enough_to_be_the_farm() {
    let tmp = tempfile::tempdir().unwrap();
    let url = start(tmp.path()).await;
    let bare = url.split('?').next().unwrap().to_owned();
    // No secret, or a wrong one: refused, whatever the Origin says.
    assert!(
        tokio_tungstenite::connect_async(bare.as_str())
            .await
            .is_err()
    );
    let wrong = format!("{bare}?k={}", "1".repeat(64));
    assert!(
        tokio_tungstenite::connect_async(wrong.as_str())
            .await
            .is_err()
    );
    // A test connection naming a person only works in test mode.
    let named = format!("{bare}?as=Mallory");
    assert!(
        tokio_tungstenite::connect_async(named.as_str())
            .await
            .is_err()
    );
    // With it: the Farm.
    let (_, _, settings) = connect(&url).await;
    assert!(settings.farm);
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
        private: false,
        name: "Daily News".into(),
        instructions: "Summarize the news.".into(),
        engine: None,
        models: Vec::new(),
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

#[tokio::test]
async fn catching_up_after_a_long_time_away_gets_the_latest_page() {
    const SEEDED: u64 = 1_234;
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open_in_memory().unwrap();
    let id = BotId::new("assistant").unwrap();
    for i in 1..=SEEDED {
        store
            .append(
                &id,
                EventKind::ToolUse {
                    name: "Read".into(),
                    summary: format!("step {i}"),
                },
            )
            .unwrap();
    }
    let url = start_with_store(tmp.path(), None, store).await;
    let (mut ws, _, _) = connect(&url).await;

    // A client that had seen up to #10: far more than a page behind.
    send(
        &mut ws,
        &ClientMsg::Subscribe {
            bot: "assistant".into(),
            since: 10,
        },
    )
    .await;
    let ServerMsg::History {
        events,
        first_seq,
        has_older,
        ..
    } = next(&mut ws).await
    else {
        panic!("expected one page, not a replay of every event")
    };
    let max = u64::from(oxbot_protocol::MAX_PAGE);
    assert_eq!(events.len() as u64, max);
    assert_eq!(first_seq, SEEDED - max + 1);
    assert_eq!(events.last().unwrap().seq, SEEDED);
    assert!(has_older);
    send(&mut ws, &ClientMsg::ListBots).await;
    assert!(matches!(next(&mut ws).await, ServerMsg::Bots { .. }));

    // A client a little behind still gets each event it missed.
    let (mut near, _, _) = connect(&url).await;
    send(
        &mut near,
        &ClientMsg::Subscribe {
            bot: "assistant".into(),
            since: SEEDED - 2,
        },
    )
    .await;
    for seq in [SEEDED - 1, SEEDED] {
        let ServerMsg::Event(e) = next(&mut near).await else {
            panic!("expected a replayed event")
        };
        assert_eq!(e.seq, seq);
    }

    // Both stream on from the end: a new turn arrives as events.
    send(
        &mut ws,
        &ClientMsg::Send {
            bot: "assistant".into(),
            text: "one more".into(),
            files: vec![],
        },
    )
    .await;
    for client in [&mut ws, &mut near] {
        let first = loop {
            if let ServerMsg::Event(e) = next(client).await {
                break e;
            }
        };
        assert_eq!(first.seq, SEEDED + 1);
    }
}

/// Counts the notifications posted, instead of posting them.
#[derive(Debug, Default)]
struct CountingNotifier(Mutex<Vec<(String, String)>>);

impl oxbot::notify::Notifier for CountingNotifier {
    fn notify(&self, title: &str, body: &str) {
        self.0
            .lock()
            .unwrap()
            .push((title.to_owned(), body.to_owned()));
    }
}

impl CountingNotifier {
    fn bodies(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(_, b)| b.clone())
            .collect()
    }
}

/// A server whose Herder answers without tools (one model request a turn)
/// and may make 5 requests a day, with notifications counted. Returns the
/// Farm's address and a Gate person's (`?as=Sam`, test mode).
async fn start_alerting(
    dir: &std::path::Path,
    notifier: Arc<CountingNotifier>,
) -> (String, String) {
    let bot = Bot::create(dir, BotId::new("assistant").unwrap(), "Assistant", "").unwrap();
    let fake = Arc::new(FakeBots {
        no_page: true,
        ..FakeBots::new(dir, Tool::None)
    });
    let config = HubConfig {
        browser: None,
        ..testing::config(dir, &dir.join(".home"), &fake, codex().await)
    };
    let hub = Hub::new(vec![bot], Store::open_in_memory().unwrap(), config);
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    hub.set_engine_prefs(oxbot_protocol::EnginePrefs {
        bot_daily_requests: 5,
        ..oxbot_protocol::EnginePrefs::default()
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let state = AppState {
        data_dir: dir.to_owned(),
        local_person: "Jay".to_owned(),
        e2e: true,
        notifier: Some(notifier),
        ..AppState::new(hub, "test-mac")
    };
    let addr = listener.local_addr().unwrap();
    let farm = format!("ws://{addr}/ws?k={}", state.farm_key);
    let gate = format!("ws://{addr}/ws?as=Sam");
    oxbot::notify::start(&state);
    tokio::spawn(server::serve(listener, state, std::future::pending()));
    (farm, gate)
}

/// Sends `text` to the assistant and reads until its turn is done,
/// returning the usage alerts that came meanwhile.
async fn alerts_in_turn(ws: &mut Ws, text: &str) -> Vec<oxbot_protocol::UsageAlert> {
    send(
        ws,
        &ClientMsg::Send {
            bot: "assistant".into(),
            text: text.into(),
            files: vec![],
        },
    )
    .await;
    let mut alerts = Vec::new();
    loop {
        match next(ws).await {
            ServerMsg::UsageAlert(a) => alerts.push(a),
            ServerMsg::Event(e) if matches!(e.kind, EventKind::TurnDone { .. }) => {
                return alerts;
            }
            _ => {}
        }
    }
}

/// Reads the opening messages up to the people list, returning the usage
/// alerts among them.
async fn opening_alerts(url: &str) -> (Ws, Vec<oxbot_protocol::UsageAlert>) {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let mut alerts = Vec::new();
    loop {
        match next(&mut ws).await {
            ServerMsg::UsageAlert(a) => alerts.push(a),
            ServerMsg::People { .. } => return (ws, alerts),
            _ => {}
        }
    }
}

async fn subscribe(ws: &mut Ws) {
    send(
        ws,
        &ClientMsg::Subscribe {
            bot: "assistant".into(),
            since: 0,
        },
    )
    .await;
}

#[tokio::test]
async fn usage_alerts_reach_everyone_with_one_notification_each() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let notifier = Arc::new(CountingNotifier::default());
    let (farm, gate) = start_alerting(tmp.path(), Arc::clone(&notifier)).await;
    let (mut ws, alerts) = opening_alerts(&farm).await;
    assert_eq!(alerts, []);
    let (mut sam, _) = opening_alerts(&gate).await;
    subscribe(&mut ws).await;

    // 1 and 2 of 5 requests: no level yet; 3 (60%) passes 50%.
    assert_eq!(alerts_in_turn(&mut ws, "one").await, []);
    assert_eq!(alerts_in_turn(&mut ws, "two").await, []);
    let first = alerts_in_turn(&mut ws, "three").await;
    let got: Vec<_> = first.iter().map(|a| (a.limit.as_str(), a.level)).collect();
    assert_eq!(got, [("openrouter_bot_daily:assistant", 50)]);
    assert!(
        first[0]
            .message
            .starts_with("Assistant's AI questions today are at 60%"),
        "{}",
        first[0].message
    );
    let second = alerts_in_turn(&mut ws, "four").await;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].level, 80);

    // A Gate person gets them too.
    let mut on_gate = Vec::new();
    while on_gate.len() < 2 {
        if let ServerMsg::UsageAlert(a) = next(&mut sam).await {
            on_gate.push(a);
        }
    }
    // One Mac notification per alert, saying the same.
    for _ in 0..50 {
        if notifier.bodies().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let bodies = notifier.bodies();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert_eq!(bodies[0], first[0].message);

    // Connecting later: the latest live alert of the window, for the
    // banner.
    let (_, live) = opening_alerts(&farm).await;
    let live: Vec<_> = live.iter().map(|a| (a.limit.as_str(), a.level)).collect();
    assert_eq!(live, [("openrouter_bot_daily:assistant", 80)]);
}

#[tokio::test]
async fn only_the_farm_turns_usage_alerts_off_and_then_none_come() {
    if codex().await.is_none() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let notifier = Arc::new(CountingNotifier::default());
    let (farm, gate) = start_alerting(tmp.path(), Arc::clone(&notifier)).await;
    let (mut sam, _) = opening_alerts(&gate).await;
    send(&mut sam, &ClientMsg::SetUsageAlerts { on: false }).await;
    loop {
        if let ServerMsg::Error { message } = next(&mut sam).await {
            assert!(
                message.contains("only be done on the home computer"),
                "{message}"
            );
            break;
        }
    }

    let (mut ws, _) = opening_alerts(&farm).await;
    send(&mut ws, &ClientMsg::SetUsageAlerts { on: false }).await;
    let settings = loop {
        if let ServerMsg::Settings(s) = next(&mut ws).await {
            break s;
        }
    };
    assert!(!settings.usage_alerts);
    subscribe(&mut ws).await;
    for turn in ["one", "two", "three", "four"] {
        assert_eq!(alerts_in_turn(&mut ws, turn).await, []);
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(notifier.bodies(), Vec::<String>::new());
    let (_, live) = opening_alerts(&farm).await;
    assert_eq!(live, [], "no banner while off");
}
