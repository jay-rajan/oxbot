use std::sync::Mutex;

use oxbot_llm::{Catalog, ModelInfo, ScriptedModel};
use oxbot_loop::store::MemoryLoopStore;
use oxbot_loop::{CatalogCache, LoopEngine, LoopSetup};

use super::*;

/// OpenRouter as a test sees it: every body it got, and what to answer.
#[derive(Debug, Default)]
struct FakeUpstream {
    bodies: Mutex<Vec<Value>>,
    /// Errors to give first, in order.
    errors: Mutex<Vec<LlmError>>,
}

const STREAM: &str = "event: response.created\ndata: {\"type\":\"response.created\"}\n\n\
    event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"gen-9\",\
    \"model\":\"e2e/a:free\",\"usage\":{\"input_tokens\":40,\"output_tokens\":4}}}\n\n";

impl Upstream for FakeUpstream {
    fn send(
        &self,
        body: Vec<u8>,
        _stream: bool,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<RawAnswer, LlmError>> {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&body).unwrap());
        let err = {
            let mut errors = self.errors.lock().unwrap();
            (!errors.is_empty()).then(|| errors.remove(0))
        };
        Box::pin(async move {
            if let Some(err) = err {
                return Err(err);
            }
            let chunks: Vec<Result<Bytes, LlmError>> = STREAM
                .as_bytes()
                .chunks(13)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect();
            Ok(RawAnswer {
                content_type: "text/event-stream".into(),
                body: futures_util::stream::iter(chunks).boxed(),
            })
        })
    }
}

fn model(id: &str) -> ModelInfo {
    ModelInfo {
        id: id.into(),
        name: id.into(),
        context_length: 128_000,
        max_completion: Some(8000),
        prompt_usd_per_token: 0.0,
        completion_usd_per_token: 0.0,
        tools: true,
        images_in: false,
        text_io: true,
        free: true,
        supported_parameters: ["tools", "tool_choice", "max_tokens"]
            .map(str::to_owned)
            .to_vec(),
    }
}

struct Harness {
    relay: Relay,
    upstream: Arc<FakeUpstream>,
    store: Arc<MemoryLoopStore>,
    dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    let store = Arc::new(MemoryLoopStore::new());
    let mut setup = LoopSetup::new(Arc::new(ScriptedModel::new()), Arc::clone(&store) as _);
    setup.catalog = Arc::new(CatalogCache::new(Catalog {
        models: vec![model("e2e/a:free"), model("e2e/b:free")],
    }));
    let engine = LoopEngine::new(setup);
    let upstream = Arc::new(FakeUpstream::default());
    let relay = Relay::start(RelaySetup {
        gateway: engine.gateway(),
        upstream: Arc::clone(&upstream) as _,
        log: Arc::new(NoLog),
    })
    .await
    .unwrap();
    Harness {
        relay,
        upstream,
        store,
        dir: tempfile::tempdir().unwrap(),
    }
}

fn info(dir: &Path) -> ClientInfo {
    ClientInfo {
        bot: "scout".into(),
        context_dir: dir.to_path_buf(),
        privacy: Privacy::Standard,
        choice: ModelChoice::Auto,
        central: false,
    }
}

fn start(meter: Option<mpsc::UnboundedSender<CallRecord>>) -> TurnStart {
    TurnStart {
        session: "s1".into(),
        turn: 2,
        person: Some("p_sam".into()),
        lane: Lane::Live,
        min_window: None,
        max_requests: 3,
        meter,
    }
}

/// A plain HTTP/1.1 request to the relay: (status, body).
async fn post(addr: SocketAddr, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\n\
         x-codex-turn-metadata: {{\"installation_id\":\"inst-1\"}}\r\nsession-id: sess-1\r\n\
         originator: codex\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    tcp.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    tcp.read_to_string(&mut out).await.unwrap();
    let status = out[9..12].parse().unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_owned()
    };
    (status, body)
}

/// A chunked body's bytes.
fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&rest[..n]);
        body = &rest[n + 2..];
    }
    out
}

const REQUEST: &str = r#"{"model":"whatever","stream":true,"instructions":"rules",
    "input":[{"type":"message","id":"msg_1","role":"user","content":[{"type":"input_text","text":"hi"}]}],
    "prompt_cache_key":"thread-1","client_metadata":{"session_id":"sess-1"},"store":false,
    "tools":[{"type":"function","name":"t","parameters":{}}],"tool_choice":"auto"}"#;

#[tokio::test]
async fn only_a_registered_token_gets_in_and_only_to_the_one_route() {
    let h = harness().await;
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    client.begin_turn(start(None));
    let addr = h.relay.addr();
    assert_eq!(post(addr, "/v1/responses", None, REQUEST).await.0, 401);
    assert_eq!(
        post(addr, "/v1/responses", Some("nope"), REQUEST).await.0,
        401
    );
    assert_eq!(
        post(addr, "/v1/chat/completions", Some(&token), "{}")
            .await
            .0,
        404
    );
    assert_eq!(post(addr, "/v1/models", Some(&token), "{}").await.0, 404);
    assert_eq!(
        post(addr, "/v1/responses", Some(&token), REQUEST).await.0,
        200
    );
    // Gone once the process's place is dropped.
    drop(client);
    assert_eq!(
        post(addr, "/v1/responses", Some(&token), REQUEST).await.0,
        401
    );
    assert_eq!(h.upstream.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn what_leaves_is_stripped_and_the_answer_is_metered_for_the_turns_person() {
    let h = harness().await;
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    client.begin_turn(start(Some(tx)));
    let (status, body) = post(h.relay.addr(), "/v1/responses", Some(&token), REQUEST).await;
    assert_eq!(status, 200);
    assert!(body.contains("response.completed"), "{body}");
    let sent = h.upstream.bodies.lock().unwrap()[0].clone();
    let text = sent.to_string();
    for gone in [
        "thread-1",
        "sess-1",
        "inst-1",
        "msg_1",
        "prompt_cache_key",
        "store",
        "whatever",
    ] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
    assert_eq!(sent["model"], "e2e/a:free");
    let record = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.engine, "codex");
    assert_eq!(record.provider, "openrouter");
    assert_eq!(record.person.as_deref(), Some("p_sam"));
    assert_eq!(record.session.as_deref(), Some("s1"));
    assert_eq!(record.turn, Some(2));
    assert_eq!(record.prompt_tokens, 40);
    assert_eq!(record.generation_id.as_deref(), Some("gen-9"));
    assert_eq!(h.store.calls().len(), 1);
    assert_eq!(client.end_turn().requests, 1);
}

#[tokio::test]
async fn a_rate_limited_model_is_retried_on_the_next_without_codex_seeing_it() {
    let h = harness().await;
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    client.begin_turn(start(None));
    h.upstream
        .errors
        .lock()
        .unwrap()
        .push(LlmError::new(ErrorKind::RateLimited, "slow down"));
    let (status, _) = post(h.relay.addr(), "/v1/responses", Some(&token), REQUEST).await;
    assert_eq!(status, 200);
    let bodies = h.upstream.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 2);
    assert_ne!(bodies[0]["model"], bodies[1]["model"]);
}

#[tokio::test]
async fn no_turn_or_too_many_steps_is_refused_in_oxbots_words() {
    let h = harness().await;
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    let addr = h.relay.addr();
    let (status, body) = post(addr, "/v1/responses", Some(&token), REQUEST).await;
    assert_eq!(status, 400);
    assert!(body.contains("No task is running"), "{body}");
    client.begin_turn(start(None));
    for _ in 0..3 {
        assert_eq!(
            post(addr, "/v1/responses", Some(&token), REQUEST).await.0,
            200
        );
    }
    let (status, body) = post(addr, "/v1/responses", Some(&token), REQUEST).await;
    assert_eq!(status, 400);
    assert!(body.contains("3 steps"), "{body}");
    let summary = client.end_turn();
    assert_eq!(summary.requests, 3);
    assert!(summary.stop.unwrap().contains("3 steps"));
}

#[tokio::test]
async fn what_was_sent_is_kept_owner_only_and_swept() {
    use std::os::unix::fs::PermissionsExt as _;
    let h = harness().await;
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    client.begin_turn(start(None));
    assert_eq!(
        post(h.relay.addr(), "/v1/responses", Some(&token), REQUEST)
            .await
            .0,
        200
    );
    let dir = h.dir.path().join(LOG_DIR);
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let file = files[0].path();
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let kept: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(kept, h.upstream.bodies.lock().unwrap()[0]);
    assert_eq!(sweep(h.dir.path(), 0, MAX_LOG_BYTES), 0);
    // Over the size cap, the oldest go even when they're new.
    assert_eq!(sweep(h.dir.path(), 0, 1), 1);
    assert_eq!(
        post(h.relay.addr(), "/v1/responses", Some(&token), REQUEST)
            .await
            .0,
        200
    );
    assert_eq!(sweep(h.dir.path(), now_ms() + 1, MAX_LOG_BYTES), 1);
}
