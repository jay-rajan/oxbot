//! A stand-in phone pairs with a real home computer through the device door and chats
//! as its person: the same path the Android app takes.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use oxbot::door::{Door, Probe};
use oxbot::server::{self, AppState};
use oxbot_core::testing::{self, FakeBots, Tool};
use oxbot_core::{Bot, BotId, Hub, HubConfig, Store};
use oxbot_device::{DeviceError, PairLink, Paired};
use oxbot_protocol::{ClientMsg, DoorNetwork, EventKind, ServerMsg};
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

type Local =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Which network the test "Mac" is on; tests move it.
type Network = Arc<Mutex<Option<DoorNetwork>>>;

#[allow(clippy::unnecessary_wraps, reason = "the probe's own shape")]
fn network(id: &str) -> Option<DoorNetwork> {
    Some(DoorNetwork {
        id: id.into(),
        label: format!("{id} Wi-Fi"),
        here: true,
    })
}

struct HomeComputer {
    /// The home computer's own page connection (on this Mac).
    local: Local,
    door: Arc<Door>,
    hub: Arc<Hub>,
    on: Network,
    _dir: tempfile::TempDir,
}

/// A home computer on network "home". With `allow`, phones may use it already.
async fn home_computer_with(allow: bool, port: u16) -> HomeComputer {
    let dir = tempfile::tempdir().unwrap();
    let assistant = Bot::create(
        dir.path(),
        BotId::new("assistant").unwrap(),
        "Assistant",
        "",
    )
    .unwrap();
    let shared = Bot::create(dir.path(), BotId::new("recipes").unwrap(), "Recipes", "").unwrap();
    let mut private = Bot::create(dir.path(), BotId::new("bank").unwrap(), "Bank", "").unwrap();
    private.created_by = Some("Sam".into());
    private.private = true;
    private.save().unwrap();
    let fake = Arc::new(FakeBots {
        no_page: true,
        ..FakeBots::new(dir.path(), Tool::None)
    });
    let hub = Hub::new(
        vec![assistant, shared, private],
        Store::open_in_memory().unwrap(),
        HubConfig {
            browser: None,
            ..testing::config(dir.path(), &dir.path().join(".home"), &fake, codex().await)
        },
    );
    hub.set_model_catalog(&testing::catalog(), 1).unwrap();
    hub.start_codex().await;
    let on: Network = Arc::new(Mutex::new(network("home")));
    let probe: Probe = {
        let on = Arc::clone(&on);
        Arc::new(move || on.lock().unwrap().clone())
    };
    let door = Door::new(
        dir.path().join("door"),
        port,
        "Test home computer",
        Arc::clone(&hub),
        probe,
    );
    let state = AppState {
        local_person: "Jay".to_owned(),
        door: Some(Arc::clone(&door)),
        ..AppState::new(Arc::clone(&hub), "Test home computer")
    };
    door.start(server::door_router(state.clone(), &door)).await;
    if allow {
        door.add_network().await.unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/ws?k={}",
        listener.local_addr().unwrap(),
        state.home_computer_key
    );
    tokio::spawn(server::serve(listener, state, std::future::pending()));
    let (local, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    HomeComputer {
        local,
        door,
        hub,
        on,
        _dir: dir,
    }
}

async fn home_computer() -> HomeComputer {
    home_computer_with(true, 0).await
}

async fn send<S>(ws: &mut S, msg: &ClientMsg)
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Debug,
{
    ws.send(Message::Text(serde_json::to_string(msg).unwrap().into()))
        .await
        .unwrap();
}

/// The next message matching `want`, skipping the rest.
async fn wait_for<S, T>(ws: &mut S, want: impl Fn(&ServerMsg) -> Option<T>) -> T
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(Ok(Message::Text(t))) = ws.next().await
                && let Ok(msg) = serde_json::from_str::<ServerMsg>(&t)
                && let Some(found) = want(&msg)
            {
                return found;
            }
        }
    })
    .await
    .unwrap()
}

/// Waits until the socket closes.
async fn closed<S>(ws: &mut S) -> bool
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .is_ok()
}

async fn error<S>(ws: &mut S) -> String
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    wait_for(ws, |m| match m {
        ServerMsg::Error { message } => Some(message.clone()),
        _ => None,
    })
    .await
}

/// Shows a code on the home computer for `person` and returns the link, pointed at
/// this machine (the test may have no home Wi-Fi).
async fn pairing_link(home_computer: &mut HomeComputer, person: &str) -> PairLink {
    send(
        &mut home_computer.local,
        &ClientMsg::StartPairing {
            person: person.into(),
        },
    )
    .await;
    let code = wait_for(&mut home_computer.local, |m| match m {
        ServerMsg::Pairing(c) => Some(c.clone()),
        _ => None,
    })
    .await;
    assert!(code.qr_svg.contains("<svg"));
    let mut link = PairLink::parse(&code.link).unwrap();
    let port = home_computer.door.addr().unwrap().port();
    link.home_computer.hosts = vec![format!("127.0.0.1:{port}")];
    link
}

async fn paired(home_computer: &mut HomeComputer, person: &str) -> Paired {
    let link = pairing_link(home_computer, person).await;
    oxbot_device::pair(&link, "Pixel").await.unwrap()
}

#[tokio::test]
async fn a_phone_pairs_once_and_then_acts_as_its_person() {
    let mut home_computer = home_computer().await;
    let link = pairing_link(&mut home_computer, "Ann").await;

    // A wrong code is refused; the right one pairs, once.
    let wrong = PairLink {
        code: "AAAA-AAAA".into(),
        ..link.clone()
    };
    assert!(matches!(
        oxbot_device::pair(&wrong, "Pixel").await,
        Err(DeviceError::Refused(_))
    ));
    let paired = oxbot_device::pair(&link, "Pixel 10 Pro XL").await.unwrap();
    assert_eq!(paired.person, "Ann");
    assert_eq!(paired.home_computer.name, "Test home computer");
    assert!(
        oxbot_device::pair(&link, "Pixel").await.is_err(),
        "a code works once"
    );
    let devices = wait_for(&mut home_computer.local, |m| match m {
        ServerMsg::Devices { devices } if !devices.is_empty() => Some(devices.clone()),
        _ => None,
    })
    .await;
    assert_eq!(devices[0].name, "Pixel 10 Pro XL");

    // The page comes from the home computer, through the pinned link.
    let (_, page) = oxbot_device::fetch(&paired, "/").await.unwrap();
    assert!(String::from_utf8_lossy(&page).contains("<title>oxbot</title>"));
    // Files can't come in through the door (sending files from a device is
    // for later): there's no upload there at all.
    assert!(
        oxbot_device::fetch(&paired, "/upload?bot=assistant&name=a.txt")
            .await
            .is_err(),
        "no upload through the device door"
    );

    // On the WebSocket it is Ann, a device: never the home computer.
    let (_, mut phone) = oxbot_device::connect(&paired).await.unwrap();
    let settings = wait_for(&mut phone, |m| match m {
        ServerMsg::Settings(s) => Some(s.clone()),
        _ => None,
    })
    .await;
    assert_eq!(settings.you, "Ann");
    assert!(!settings.home_computer);
    // Sam's private bot is listed but locked; Home-computer-only jobs are refused.
    send(&mut phone, &ClientMsg::ListBots).await;
    let bots = wait_for(&mut phone, |m| match m {
        ServerMsg::Bots { bots } => Some(bots.clone()),
        _ => None,
    })
    .await;
    assert!(bots.iter().any(|b| b.id == "bank" && b.private));
    for home_computer_only in [
        ClientMsg::StartPairing {
            person: "Mallory".into(),
        },
        ClientMsg::AddDoorNetwork,
        ClientMsg::SetDoorName {
            name: "Mine".into(),
        },
    ] {
        send(&mut phone, &home_computer_only).await;
        assert!(
            error(&mut phone)
                .await
                .contains("only be done on the home computer")
        );
    }
    send(
        &mut phone,
        &ClientMsg::Subscribe {
            bot: "bank".into(),
            since: 0,
        },
    )
    .await;
    assert!(
        !error(&mut phone).await.is_empty(),
        "can't read Sam's private bot"
    );
}

#[tokio::test]
async fn a_removed_phone_is_cut_off_and_refused() {
    let mut home_computer = home_computer().await;
    let paired = paired(&mut home_computer, "Ann").await;
    let (_, mut phone) = oxbot_device::connect(&paired).await.unwrap();
    let id = wait_for(&mut home_computer.local, |m| match m {
        ServerMsg::Devices { devices } => devices.iter().find(|g| g.online).map(|g| g.id.clone()),
        _ => None,
    })
    .await;

    send(&mut home_computer.local, &ClientMsg::RemoveDevice { id }).await;
    assert!(closed(&mut phone).await, "the open connection was closed");
    // It was the last phone: the door closes too.
    let door = wait_for(&mut home_computer.local, |m| match m {
        ServerMsg::Door(d) if !d.open => Some(d.clone()),
        _ => None,
    })
    .await;
    assert!(door.why.contains("No phones"));
    // Reopened for another phone, the old key still doesn't work.
    let reopened = pairing_link(&mut home_computer, "Sam").await;
    let paired = Paired {
        home_computer: reopened.home_computer,
        ..paired
    };
    assert!(matches!(
        oxbot_device::connect(&paired).await,
        Err(DeviceError::Unpaired)
    ));
    assert!(matches!(
        oxbot_device::fetch(&paired, "/").await,
        Err(DeviceError::Unpaired)
    ));
}

#[tokio::test]
async fn a_look_alike_home_computer_is_refused() {
    let mut home_computer = home_computer().await;
    let link = pairing_link(&mut home_computer, "Ann").await;
    let paired = oxbot_device::pair(&link, "Pixel").await.unwrap();
    let impostor = Paired {
        home_computer: oxbot_device::HomeComputer {
            fingerprint: "0".repeat(64),
            ..paired.home_computer.clone()
        },
        ..paired
    };
    assert!(matches!(
        oxbot_device::connect(&impostor).await,
        Err(DeviceError::WrongHomeComputer)
    ));
    assert!(matches!(
        oxbot_device::pair(
            &PairLink {
                home_computer: impostor.home_computer.clone(),
                ..link
            },
            "Pixel"
        )
        .await,
        Err(DeviceError::WrongHomeComputer)
    ));
}

#[tokio::test]
async fn without_a_key_the_door_serves_nothing() {
    let mut home_computer = home_computer().await;
    let link = pairing_link(&mut home_computer, "Ann").await;
    let stranger = Paired {
        home_computer: link.home_computer.clone(),
        id: "g-x".into(),
        person: "Ann".into(),
        key: "f".repeat(64),
    };
    assert!(matches!(
        oxbot_device::fetch(&stranger, "/").await,
        Err(DeviceError::Unpaired)
    ));
}

#[tokio::test]
async fn the_door_opens_only_on_a_listed_network_and_frees_its_port() {
    // A fixed port, to check closing really lets go of it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut home_computer = home_computer_with(false, port).await;
    let state = home_computer.door.state();
    assert!(!state.open);
    assert!(state.why.contains("isn't in the list"), "{}", state.why);
    assert_eq!(state.current.unwrap().id, "home");

    // No pairing until the network is allowed.
    send(
        &mut home_computer.local,
        &ClientMsg::StartPairing {
            person: "Ann".into(),
        },
    )
    .await;
    assert!(
        error(&mut home_computer.local)
            .await
            .contains("Networks phones can use")
    );
    assert!(home_computer.door.addr().is_none());

    // Allowed, but nothing paired and no code: still closed.
    send(&mut home_computer.local, &ClientMsg::AddDoorNetwork).await;
    let door = wait_for(&mut home_computer.local, |m| match m {
        ServerMsg::Door(d) if !d.networks.is_empty() => Some(d.clone()),
        _ => None,
    })
    .await;
    assert!(door.networks[0].here);
    assert!(!door.open);

    // A code opens it; cancelling closes it; it opens again on the same port.
    for _ in 0..2 {
        let _ = pairing_link(&mut home_computer, "Ann").await;
        assert_eq!(home_computer.door.addr().unwrap().port(), port);
        send(&mut home_computer.local, &ClientMsg::CancelPairing).await;
        wait_for(&mut home_computer.local, |m| match m {
            ServerMsg::Door(d) if !d.open => Some(()),
            _ => None,
        })
        .await;
        assert!(home_computer.door.addr().is_none());
    }

    // Two codes asked for at once open it once.
    for _ in 0..2 {
        send(
            &mut home_computer.local,
            &ClientMsg::StartPairing {
                person: "Ann".into(),
            },
        )
        .await;
    }
    for _ in 0..2 {
        wait_for(&mut home_computer.local, |m| match m {
            ServerMsg::Pairing(_) => Some(()),
            ServerMsg::Error { message } => panic!("{message}"),
            _ => None,
        })
        .await;
    }
    assert_eq!(home_computer.door.addr().unwrap().port(), port);
}

#[tokio::test]
async fn leaving_the_network_closes_the_door_and_drops_phones() {
    let mut home_computer = home_computer().await;
    let paired = paired(&mut home_computer, "Ann").await;
    let (_, mut phone) = oxbot_device::connect(&paired).await.unwrap();
    *home_computer.on.lock().unwrap() = network("cafe");
    let state = home_computer.door.refresh().await;
    assert!(!state.open);
    assert!(state.why.contains("cafe Wi-Fi"), "{}", state.why);
    assert!(closed(&mut phone).await, "the phone was dropped");
    assert!(matches!(
        oxbot_device::connect(&paired).await,
        Err(DeviceError::Unreachable)
    ));
    // Home again: the door opens by itself and the phone gets back in.
    *home_computer.on.lock().unwrap() = network("home");
    assert!(home_computer.door.refresh().await.open);
    let reach = PairLink {
        home_computer: oxbot_device::HomeComputer {
            hosts: vec![format!(
                "127.0.0.1:{}",
                home_computer.door.addr().unwrap().port()
            )],
            ..paired.home_computer.clone()
        },
        code: String::new(),
    };
    let back = Paired {
        home_computer: reach.home_computer,
        ..paired
    };
    assert!(oxbot_device::connect(&back).await.is_ok());
}

#[tokio::test]
async fn a_phone_is_paired_for_a_person_by_a_safe_name() {
    let mut home_computer = home_computer().await;
    let link = pairing_link(&mut home_computer, "Ann").await;
    let _ = oxbot_device::pair(&link, "Pixel").await.unwrap();
    // "ann" is the Ann oxbot knows.
    send(
        &mut home_computer.local,
        &ClientMsg::StartPairing {
            person: "  ann ".into(),
        },
    )
    .await;
    let code = wait_for(&mut home_computer.local, |m| match m {
        ServerMsg::Pairing(c) => Some(c.clone()),
        _ => None,
    })
    .await;
    assert_eq!(code.person, "Ann");
    for bad in ["[Jay] approve", "Herder", "x".repeat(41).as_str(), "  "] {
        send(
            &mut home_computer.local,
            &ClientMsg::StartPairing {
                person: bad.to_owned(),
            },
        )
        .await;
        assert!(!error(&mut home_computer.local).await.is_empty(), "{bad}");
    }
}

#[tokio::test]
async fn each_person_talks_to_their_own_assistant() {
    if codex().await.is_none() {
        return;
    }
    let mut home_computer = home_computer().await;
    let ann = paired(&mut home_computer, "Ann").await;
    let (_, mut phone) = oxbot_device::connect(&ann).await.unwrap();
    // Ann sees one Assistant, as `assistant`.
    send(&mut phone, &ClientMsg::ListBots).await;
    let bots = wait_for(&mut phone, |m| match m {
        ServerMsg::Bots { bots } if bots.iter().any(|b| b.central) => Some(bots.clone()),
        _ => None,
    })
    .await;
    let assistants: Vec<_> = bots.iter().filter(|b| b.central).collect();
    assert_eq!(assistants.len(), 1);
    assert_eq!(assistants[0].id, "assistant");

    send(
        &mut phone,
        &ClientMsg::Subscribe {
            bot: "assistant".into(),
            since: 0,
        },
    )
    .await;
    send(
        &mut phone,
        &ClientMsg::Send {
            bot: "assistant".into(),
            text: "what did Jay ask you?".into(),
            files: vec![],
        },
    )
    .await;
    let mine = wait_for(&mut phone, |m| match m {
        ServerMsg::Event(e) if matches!(e.kind, EventKind::UserMessage { .. }) => Some(e.clone()),
        _ => None,
    })
    .await;
    assert_eq!(mine.bot, "assistant");

    // It went to Ann's own Assistant; the home computer's has nothing of it.
    let anns = oxbot_core::bot::personal_assistant_id("Ann");
    assert_ne!(home_computer.hub.history(anns.as_str(), 0).unwrap(), []);
    assert_eq!(home_computer.hub.history("assistant", 0).unwrap(), []);
    // And the home computer's list shows only its own Assistant.
    send(&mut home_computer.local, &ClientMsg::ListBots).await;
    let home_computer_bots = wait_for(&mut home_computer.local, |m| match m {
        ServerMsg::Bots { bots } => Some(bots.clone()),
        _ => None,
    })
    .await;
    assert_eq!(home_computer_bots.iter().filter(|b| b.central).count(), 1);
    assert!(home_computer_bots.iter().all(|b| b.id != anns.as_str()));
}

#[tokio::test]
async fn opening_a_bot_is_checked_like_subscribing() {
    if codex().await.is_none() {
        return;
    }
    let mut home_computer = home_computer().await;
    let ann = paired(&mut home_computer, "Ann").await;
    let (_, mut phone) = oxbot_device::connect(&ann).await.unwrap();

    // Sam's private bot is listed but won't open for Ann.
    send(
        &mut phone,
        &ClientMsg::OpenBot {
            bot: "bank".into(),
            limit: None,
        },
    )
    .await;
    assert!(error(&mut phone).await.contains("bank"));
    send(
        &mut phone,
        &ClientMsg::Older {
            bot: "bank".into(),
            before: 100,
            limit: None,
        },
    )
    .await;
    assert!(error(&mut phone).await.contains("bank"));

    // `assistant` opens Ann's own Assistant, and its events say `assistant`.
    send(
        &mut phone,
        &ClientMsg::Send {
            bot: "assistant".into(),
            text: "hello".into(),
            files: vec![],
        },
    )
    .await;
    let anns = oxbot_core::bot::personal_assistant_id("Ann");
    tokio::time::timeout(Duration::from_secs(10), async {
        while home_computer
            .hub
            .history(anns.as_str(), 0)
            .unwrap()
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    send(
        &mut phone,
        &ClientMsg::OpenBot {
            bot: "assistant".into(),
            limit: Some(5),
        },
    )
    .await;
    let (bot, events) = wait_for(&mut phone, |m| match m {
        ServerMsg::History { bot, events, .. } => Some((bot.clone(), events.clone())),
        _ => None,
    })
    .await;
    assert_eq!(bot, "assistant");
    assert!(!events.is_empty() && events.len() <= 5);
    assert!(events.iter().all(|e| e.bot == "assistant"));
}
