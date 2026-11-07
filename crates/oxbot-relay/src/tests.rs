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
    /// What to answer instead of [`STREAM`] (and whether it's JSON).
    answer: Mutex<Option<(String, bool)>>,
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
        let (answer, json) = self
            .answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| (STREAM.to_owned(), false));
        Box::pin(async move {
            if let Some(err) = err {
                return Err(err);
            }
            let chunks: Vec<Result<Bytes, LlmError>> = answer
                .as_bytes()
                .chunks(13)
                .map(|c| Ok(Bytes::copy_from_slice(c)))
                .collect();
            Ok(RawAnswer {
                content_type: if json {
                    "application/json"
                } else {
                    "text/event-stream"
                }
                .into(),
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

/// The names a test's oxbot knows.
struct Names(Vec<personal::Known>);

impl KnownNames for Names {
    fn known_names(&self) -> Vec<personal::Known> {
        self.0.clone()
    }
}

async fn harness() -> Harness {
    harness_knowing(Vec::new()).await
}

async fn harness_knowing(names: Vec<personal::Known>) -> Harness {
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
        names: Arc::new(Names(names)),
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

/// A request whose message has personal details: an email, a phone
/// number and a name oxbot knows.
const PERSONAL: &str = r#"{"model":"m","stream":true,"instructions":"You help Ann Lee.",
    "input":[{"type":"message","role":"user","content":[{"type":"input_text",
      "text":"[Ann Lee] email ann.lee@example.com or call 416-555-0100"}]},
     {"type":"function_call","call_id":"c1","name":"type_text",
      "arguments":"{\"text\":\"ann.lee@example.com\"}"},
     {"type":"function_call_output","call_id":"c1","output":"Typed it for Ann."}]}"#;

/// A streamed answer that uses the stand-ins, split mid-way through one.
fn answer_with_stand_ins() -> String {
    let events = [
        json!({"type": "response.created", "response": {"id": "g"}}),
        json!({"type": "response.output_text.delta", "item_id": "m", "delta": "Hi [per"}),
        json!({"type": "response.output_text.delta", "item_id": "m", "delta": "son 1], I'll write to [email 1]."}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "f", "delta": "{\"text\":\"[phone 1]\"}"}),
        json!({"type": "response.completed", "response": {"id": "g", "model": "e2e/a:free",
            "usage": {"input_tokens": 9, "output_tokens": 3}}}),
    ];
    events.iter().fold(String::new(), |mut out, e| {
        use std::fmt::Write as _;
        let _ = write!(out, "event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap());
        out
    })
}

#[tokio::test]
async fn personal_details_leave_as_stand_ins_and_come_back_real() {
    use std::os::unix::fs::PermissionsExt as _;
    let h = harness_knowing(vec![personal::Known::person("Ann Lee")]).await;
    *h.upstream.answer.lock().unwrap() = Some((answer_with_stand_ins(), false));
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    client.begin_turn(start(None));
    let (status, body) = post(h.relay.addr(), "/v1/responses", Some(&token), PERSONAL).await;
    assert_eq!(status, 200);
    let sent = h.upstream.bodies.lock().unwrap()[0].clone();
    let text = sent.to_string();
    for real in ["Ann", "ann.lee@example.com", "416-555-0100"] {
        assert!(!text.contains(real), "{real} left: {text}");
    }
    assert_eq!(sent["instructions"], "You help [person 1].");
    assert_eq!(
        sent["input"][0]["content"][0]["text"],
        "[[person 1]] email [email 1] or call [phone 1]"
    );
    assert_eq!(sent["input"][1]["arguments"], r#"{"text":"[email 1]"}"#);
    assert_eq!(sent["input"][2]["output"], "Typed it for [person 2].");
    // Codex gets the real details back, as valid events.
    let said: String = body
        .split("\n\n")
        .filter_map(|f| f.lines().find_map(|l| l.strip_prefix("data: ")))
        .map(|d| serde_json::from_str::<Value>(d).unwrap())
        .filter(|e| e["type"] == "response.output_text.delta")
        .map(|e| e["delta"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(said, "Hi Ann Lee, I'll write to ann.lee@example.com.");
    assert!(
        body.contains(r#""delta":"{\"text\":\"416-555-0100\"}""#),
        "{body}"
    );
    assert!(!body.contains("[email") && !body.contains("[per"), "{body}");
    // What was sent is kept as it left.
    let logs: Vec<_> = std::fs::read_dir(h.dir.path().join(LOG_DIR))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                && !p
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("stand-ins")
        })
        .collect();
    assert_eq!(logs.len(), 1);
    let kept = std::fs::read_to_string(&logs[0]).unwrap();
    assert!(!kept.contains("ann.lee@example.com") && kept.contains("[email 1]"));
    // The chat's stand-ins are kept, owner-only, and never swept.
    let path = stand_ins_path(h.dir.path(), "s1");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(sweep(h.dir.path(), now_ms() + 1, MAX_LOG_BYTES), 1);
    assert!(path.exists());
    let _ = client.end_turn();

    // A new Codex process for the same chat (oxbot restarted) keeps them:
    // the phone number is still [phone 1], a new email is [email 2].
    drop(client);
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    client.begin_turn(start(None));
    let again = PERSONAL.replace("call 416-555-0100", "call (416) 555-0100 or bo@x.io");
    assert_eq!(
        post(h.relay.addr(), "/v1/responses", Some(&token), &again)
            .await
            .0,
        200
    );
    let sent = h.upstream.bodies.lock().unwrap()[1].clone();
    assert_eq!(
        sent["input"][0]["content"][0]["text"],
        "[[person 1]] email [email 1] or call [phone 1] or [email 2]"
    );
}

#[tokio::test]
async fn a_whole_answer_comes_back_real() {
    let h = harness_knowing(vec![personal::Known::person("Ann Lee")]).await;
    let whole = json!({"id": "g", "model": "e2e/a:free", "output": [
        {"type": "message", "content": [{"type": "output_text", "text": "Done, [person 1]."}]},
        {"type": "function_call", "arguments": "{\"to\":\"[email 1]\"}"}],
        "usage": {"input_tokens": 3, "output_tokens": 1}});
    *h.upstream.answer.lock().unwrap() = Some((whole.to_string(), true));
    let (client, token) = h.relay.register(info(h.dir.path())).unwrap();
    client.begin_turn(start(None));
    let request = PERSONAL.replace(r#""stream":true"#, r#""stream":false"#);
    let (status, body) = post(h.relay.addr(), "/v1/responses", Some(&token), &request).await;
    assert_eq!(status, 200);
    let got: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(got["output"][0]["content"][0]["text"], "Done, Ann Lee.");
    assert_eq!(
        got["output"][1]["arguments"],
        r#"{"to":"ann.lee@example.com"}"#
    );
}

#[tokio::test]
async fn a_private_bot_may_use_free_models_once_details_are_removed() {
    let h = harness_knowing(vec![personal::Known::person("Ann Lee")]).await;
    let private = ClientInfo {
        privacy: Privacy::Private,
        ..info(h.dir.path())
    };
    let (client, token) = h.relay.register(private).unwrap();
    client.begin_turn(start(None));
    assert_eq!(
        post(h.relay.addr(), "/v1/responses", Some(&token), PERSONAL)
            .await
            .0,
        200
    );
    // Removal is always on: no "deny", and only stand-ins leave.
    let bodies = h.upstream.bodies.lock().unwrap().clone();
    assert!(
        bodies[0]["provider"].get("data_collection").is_none(),
        "{}",
        bodies[0]
    );
    assert!(bodies[0].to_string().contains("[email 1]"));
    assert!(!bodies[0].to_string().contains("ann.lee@example.com"));
    assert!(!h.store.calls()[0].private);
}
