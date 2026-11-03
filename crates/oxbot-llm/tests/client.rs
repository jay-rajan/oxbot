//! The model client against a small local HTTP server (no real network).
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use oxbot_llm::{
    ApiKey, Auth, CancellationToken, ChatDelta, ChatModel, ChatRequest, ClientConfig, Endpoint,
    ErrorKind, KeyCell, Message, OpenAiClient, Pkce, Provider, ProviderPrefs, Timeouts, ToolSpec,
    complete, exchange_code,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const FAKE_KEY: &str = "sk-or-v1-test-fake-0123456789";

/// One request as the server saw it.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// What the server answers: status, headers, then body pieces, each
/// written after its delay.
#[derive(Clone)]
struct Answer {
    status: u16,
    headers: Vec<(&'static str, String)>,
    pieces: Vec<(Duration, Vec<u8>)>,
}

impl Answer {
    fn sse(events: &[&str]) -> Self {
        let body: String = events.iter().flat_map(|e| [*e, "\n\n"]).collect();
        Self {
            status: 200,
            headers: vec![("content-type", "text/event-stream".into())],
            pieces: vec![(Duration::ZERO, body.into_bytes())],
        }
    }
    fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            headers: vec![("content-type", "application/json".into())],
            pieces: vec![(Duration::ZERO, body.to_string().into_bytes())],
        }
    }
}

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    /// Serves `answers` in order, one per connection.
    async fn start(answers: Vec<Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://127.0.0.1:{}/api/v1",
            listener.local_addr().unwrap().port()
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        tokio::spawn(async move {
            for answer in answers {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let log = Arc::clone(&log);
                tokio::spawn(async move {
                    let Some(req) = read_request(&mut sock).await else {
                        return;
                    };
                    log.lock().unwrap().push(req);
                    let mut head = format!("HTTP/1.1 {} X\r\nconnection: close\r\n", answer.status);
                    for (k, v) in &answer.headers {
                        head.push_str(k);
                        head.push_str(": ");
                        head.push_str(v);
                        head.push_str("\r\n");
                    }
                    head.push_str("\r\n");
                    if sock.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    for (delay, bytes) in answer.pieces {
                        tokio::time::sleep(delay).await;
                        if sock.write_all(&bytes).await.is_err() || sock.flush().await.is_err() {
                            return;
                        }
                    }
                    let _ = sock.shutdown().await;
                });
            }
        });
        Self { base, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn client(&self, auth: Auth, timeouts: Timeouts) -> OpenAiClient {
        let endpoint = Endpoint::loopback(&self.base, Provider::OpenRouter).unwrap();
        OpenAiClient::new(ClientConfig {
            endpoint,
            auth,
            timeouts,
        })
        .unwrap()
    }

    fn signed_in(&self) -> OpenAiClient {
        self.client(
            Auth::Key(KeyCell::with_key(ApiKey::new(FAKE_KEY.into()).unwrap())),
            Timeouts::default(),
        )
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Seen {
        method,
        path,
        headers,
        body,
    })
}

fn request() -> ChatRequest {
    let mut req = ChatRequest::new(
        "a/model:free",
        vec![Message::system("rules"), Message::user("hello")],
    );
    req.fallbacks = vec!["b/model:free".into()];
    req.tools = vec![ToolSpec {
        name: "Read".into(),
        description: "Reads a file".into(),
        parameters: json!({"type": "object"}),
    }];
    req.provider = Some(ProviderPrefs::private_bot());
    req
}

const TEXT_AND_TOOL: &[&str] = &[
    ": OPENROUTER PROCESSING",
    r#"data: {"id":"gen-1","model":"a/model:free","choices":[{"delta":{"role":"assistant","content":"Reading "}}]}"#,
    r#"data: {"id":"gen-1","model":"a/model:free","choices":[{"delta":{"content":"it."}}]}"#,
    r#"data: {"id":"gen-1","model":"a/model:free","choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"Read","arguments":"{\"file_"}}]}}]}"#,
    r#"data: {"id":"gen-1","model":"a/model:free","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"path\":\"a.txt\"}"}}]}}]}"#,
    r#"data: {"id":"gen-1","model":"a/model:free","choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    r#"data: {"id":"gen-1","model":"a/model:free","choices":[],"usage":{"prompt_tokens":50,"completion_tokens":9,"cost":0}}"#,
    "data: [DONE]",
];

#[tokio::test]
async fn streams_an_answer_and_sends_the_right_request() {
    let server = Server::start(vec![Answer::sse(TEXT_AND_TOOL)]).await;
    let client = server.signed_in();
    let mut texts = Vec::new();
    let answer = complete(&client, request(), CancellationToken::new(), |d| {
        if let ChatDelta::Text(t) = d {
            texts.push(t.clone());
        }
    })
    .await
    .unwrap();
    assert_eq!(texts, ["Reading ", "it."]);
    assert_eq!(answer.message.text(), "Reading it.");
    assert_eq!(answer.message.tool_calls[0].id, "c1");
    assert_eq!(
        answer.message.tool_calls[0].arguments,
        json!({"file_path": "a.txt"})
    );
    assert_eq!(answer.model, "a/model:free");
    assert_eq!(answer.generation_id.as_deref(), Some("gen-1"));
    assert_eq!(answer.usage.prompt_tokens, 50);
    assert_eq!(answer.usage.cost_usd, Some(0.0));

    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    let req = &seen[0];
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/api/v1/chat/completions");
    assert_eq!(
        req.header("authorization"),
        Some(format!("Bearer {FAKE_KEY}").as_str())
    );
    assert_eq!(req.header("accept"), Some("text/event-stream"));
    assert_eq!(req.header("content-type"), Some("application/json"));
    // No attribution or tracking headers.
    for h in ["http-referer", "referer", "x-title", "user-agent", "cookie"] {
        assert!(req.header(h).is_none(), "{h} sent");
    }
    let body = req.json();
    assert_eq!(body["provider"]["data_collection"], "deny");
    assert_eq!(body["provider"]["zdr"], true);
    assert_eq!(body["models"], json!(["a/model:free", "b/model:free"]));
    assert_eq!(body["stream"], true);
    assert_eq!(body["parallel_tool_calls"], false);
}

#[tokio::test]
async fn pieces_split_anywhere_with_pauses() {
    let whole: String = TEXT_AND_TOOL
        .iter()
        .flat_map(|e| [*e, "\r\n\r\n"])
        .collect();
    let pieces = whole
        .as_bytes()
        .chunks(7)
        .enumerate()
        .map(|(i, c)| {
            (
                if i % 10 == 0 {
                    Duration::from_millis(2)
                } else {
                    Duration::ZERO
                },
                c.to_vec(),
            )
        })
        .collect();
    let answer = Answer {
        status: 200,
        headers: vec![("content-type", "text/event-stream".into())],
        pieces,
    };
    let server = Server::start(vec![answer]).await;
    let got = complete(
        &server.signed_in(),
        request(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(got.message.text(), "Reading it.");
    assert_eq!(
        got.message.tool_calls[0].arguments,
        json!({"file_path": "a.txt"})
    );
}

#[tokio::test]
async fn rate_limit_answer_is_classified_with_retry_after() {
    let mut answer = Answer::json(
        429,
        &json!({"error": {"code": 429, "message": "Rate limit exceeded: free-models-per-min.", "metadata": {}}}),
    );
    answer.headers.push(("retry-after", "3".into()));
    let server = Server::start(vec![answer]).await;
    let err = complete(
        &server.signed_in(),
        request(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::RateLimited);
    assert_eq!(err.status, Some(429));
    assert_eq!(err.retry_after, Some(Duration::from_secs(3)));
    assert!(!err.partial);
}

#[tokio::test]
async fn mid_stream_error_ends_the_stream_after_text() {
    let server = Server::start(vec![Answer::sse(&[
        r#"data: {"choices":[{"delta":{"content":"Half"}}]}"#,
        r#"data: {"id":"gen-2","model":"x","error":{"code":502,"message":"Provider returned error","metadata":{"error_type":"provider_unavailable"}},"choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#,
        r#"data: {"choices":[{"delta":{"content":"never seen"}}]}"#,
    ])])
    .await;
    let mut stream = server
        .signed_in()
        .stream(request(), CancellationToken::new());
    assert_eq!(
        stream.next().await.unwrap().unwrap(),
        ChatDelta::Text("Half".into())
    );
    let err = stream.next().await.unwrap().unwrap_err();
    assert_eq!(err.kind, ErrorKind::Overloaded);
    assert!(err.partial);
    assert!(stream.next().await.is_none(), "nothing after an error");
}

#[tokio::test]
async fn cut_off_stream_is_a_protocol_error() {
    let server = Server::start(vec![Answer::sse(&[
        r#"data: {"choices":[{"delta":{"content":"Half"}}]}"#,
    ])])
    .await;
    let err = complete(
        &server.signed_in(),
        request(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Protocol);
    assert!(err.partial);
}

#[tokio::test]
async fn stream_ending_without_done_marker_but_with_finish_is_fine() {
    let server = Server::start(vec![Answer::sse(&[
        r#"data: {"model":"llama","choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]}"#,
    ])])
    .await;
    let got = complete(
        &server.signed_in(),
        request(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(got.message.text(), "ok");
    assert_eq!(got.model, "llama");
}

#[tokio::test]
async fn silence_between_chunks_times_out() {
    let mut answer = Answer::sse(&[r#"data: {"choices":[{"delta":{"content":"Hi"}}]}"#]);
    answer
        .pieces
        .push((Duration::from_secs(5), b"data: [DONE]\n\n".to_vec()));
    let server = Server::start(vec![answer]).await;
    let timeouts = Timeouts {
        between_chunks: Duration::from_millis(150),
        ..Timeouts::default()
    };
    let client = server.client(
        Auth::Key(KeyCell::with_key(ApiKey::new(FAKE_KEY.into()).unwrap())),
        timeouts,
    );
    let started = std::time::Instant::now();
    let err = complete(&client, request(), CancellationToken::new(), |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Timeout);
    assert!(err.partial);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn keep_alive_comments_count_as_signs_of_life() {
    let mut pieces = Vec::new();
    for _ in 0..4 {
        pieces.push((
            Duration::from_millis(80),
            b": OPENROUTER PROCESSING\n\n".to_vec(),
        ));
    }
    let done = Answer::sse(&[
        r#"data: {"choices":[{"delta":{"content":"late"},"finish_reason":"stop"}]}"#,
        "data: [DONE]",
    ]);
    pieces.extend(done.pieces);
    let answer = Answer {
        status: 200,
        headers: done.headers,
        pieces,
    };
    let server = Server::start(vec![answer]).await;
    let timeouts = Timeouts {
        between_chunks: Duration::from_millis(200),
        ..Timeouts::default()
    };
    let client = server.client(
        Auth::Key(KeyCell::with_key(ApiKey::new(FAKE_KEY.into()).unwrap())),
        timeouts,
    );
    let got = complete(&client, request(), CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(got.message.text(), "late");
}

#[tokio::test]
async fn cancel_stops_a_waiting_request() {
    let mut answer = Answer::sse(&[]);
    answer.pieces = vec![(Duration::from_secs(10), b"data: [DONE]\n\n".to_vec())];
    let server = Server::start(vec![answer]).await;
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        stop.cancel();
    });
    let started = std::time::Instant::now();
    let err = complete(&server.signed_in(), request(), cancel, |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(3));

    let already = CancellationToken::new();
    already.cancel();
    let err = complete(&server.signed_in(), request(), already, |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn without_a_key_nothing_is_sent() {
    let server = Server::start(vec![Answer::sse(TEXT_AND_TOOL)]).await;
    let cell = KeyCell::new();
    let client = server.client(Auth::Key(cell.clone()), Timeouts::default());
    let err = complete(&client, request(), CancellationToken::new(), |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::NotSignedIn);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(server.seen().is_empty());

    // Signing in later is seen by the same client.
    cell.set(ApiKey::new(FAKE_KEY.into()).unwrap());
    complete(&client, request(), CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(server.seen().len(), 1);
}

#[tokio::test]
async fn local_servers_get_no_key_and_no_openrouter_fields() {
    let server = Server::start(vec![Answer::json(200, &json!({
        "id": "x", "model": "llama3.1:8b",
        "choices": [{"message": {"role": "assistant", "content": "Hi from here."}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 4}
    }))])
    .await;
    let endpoint = Endpoint::loopback(&server.base, Provider::Local).unwrap();
    let client = OpenAiClient::new(ClientConfig {
        endpoint,
        auth: Auth::None,
        timeouts: Timeouts::default(),
    })
    .unwrap();
    assert_eq!(client.provider(), Provider::Local);
    let got = complete(&client, request(), CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(got.message.text(), "Hi from here.");
    assert_eq!(got.usage.cost_usd, None);
    let seen = server.seen();
    assert!(seen[0].header("authorization").is_none());
    let body = seen[0].json();
    assert!(body.get("provider").is_none());
    assert!(body.get("models").is_none());
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let mut answer = Answer::json(302, &json!({}));
    answer
        .headers
        .push(("location", "http://127.0.0.1:9/elsewhere".into()));
    let server = Server::start(vec![answer, Answer::sse(TEXT_AND_TOOL)]).await;
    let err = complete(
        &server.signed_in(),
        request(),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, Some(302));
    assert_eq!(server.seen().len(), 1);
}

#[tokio::test]
async fn model_list_is_fetched_without_the_key_and_key_info_with_it() {
    let server = Server::start(vec![
        Answer::json(
            200,
            &json!({"data": [
                {"id": "e2e/tool-model:free", "name": "Tool model", "context_length": 128_000,
                 "pricing": {"prompt": "0", "completion": "0"}, "supported_parameters": ["tools"]},
                {"id": "e2e/small:free", "context_length": 8000,
                 "pricing": {"prompt": "0", "completion": "0"}, "supported_parameters": ["tools"]}
            ]}),
        ),
        Answer::json(
            200,
            &json!({"data": {"limit": null, "usage_daily": 0, "is_free_tier": false,
            "free_model_daily_requests": {"used": 3, "limit": 1000, "remaining": 997}}}),
        ),
    ])
    .await;
    let client = server.signed_in();
    let catalog = client.models().await.unwrap();
    let free: Vec<&str> = catalog.free_for_loop().map(|m| m.id.as_str()).collect();
    assert_eq!(free, ["e2e/tool-model:free"]);
    let key = client.key_info().await.unwrap();
    assert_eq!(key.free_daily.unwrap().remaining, 997);

    let seen = server.seen();
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", "/api/v1/models")
    );
    assert!(
        seen[0].header("authorization").is_none(),
        "the model list needs no key"
    );
    assert_eq!(seen[1].path, "/api/v1/key");
    assert_eq!(
        seen[1].header("authorization"),
        Some(format!("Bearer {FAKE_KEY}").as_str())
    );
}

#[tokio::test]
async fn refused_key_is_reported_without_the_key() {
    let server = Server::start(vec![Answer::json(
        401,
        &json!({"error": {"code": 401, "message": "No auth credentials found"}}),
    )])
    .await;
    let err = server.signed_in().key_info().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unauthorized);
    assert!(!format!("{err} {err:?}").contains("0123456789"));
}

#[tokio::test]
async fn sign_in_code_is_exchanged_for_a_key() {
    let server = Server::start(vec![Answer::json(
        200,
        &json!({"key": FAKE_KEY, "user_id": "user_e2e"}),
    )])
    .await;
    let client = server.client(Auth::None, Timeouts::default());
    let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
    let key = exchange_code(&client, "code-1", &pkce).await.unwrap();
    assert_eq!(key.expose_for_storage(), FAKE_KEY.as_bytes());
    let seen = server.seen();
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("POST", "/api/v1/auth/keys")
    );
    assert!(seen[0].header("authorization").is_none());
    assert_eq!(
        seen[0].json(),
        json!({
            "code": "code-1",
            "code_verifier": "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "code_challenge_method": "S256"
        })
    );
}

#[tokio::test]
async fn refused_sign_in_code_is_an_error() {
    let server = Server::start(vec![Answer::json(
        400,
        &json!({"error": {"code": 400, "message": "Invalid code"}}),
    )])
    .await;
    let client = server.client(Auth::None, Timeouts::default());
    let pkce = Pkce::new().unwrap();
    let err = exchange_code(&client, "stale", &pkce).await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::BadRequest);
}

#[tokio::test]
async fn unreachable_server_is_a_network_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://127.0.0.1:{}/api/v1",
        listener.local_addr().unwrap().port()
    );
    drop(listener);
    let endpoint = Endpoint::loopback(&base, Provider::OpenRouter).unwrap();
    let client = OpenAiClient::new(ClientConfig {
        endpoint,
        auth: Auth::Key(KeyCell::with_key(ApiKey::new(FAKE_KEY.into()).unwrap())),
        timeouts: Timeouts::default(),
    })
    .unwrap();
    let err = complete(&client, request(), CancellationToken::new(), |_| {})
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
    assert!(!err.message.contains(FAKE_KEY));
}

#[test]
fn only_openrouter_or_loopback_addresses() {
    for ok in [
        "http://127.0.0.1:1234/api/v1",
        "http://localhost:11434/v1",
        "https://[::1]:8443/v1",
        "http://127.0.0.2/x/",
    ] {
        assert!(Endpoint::loopback(ok, Provider::Local).is_ok(), "{ok}");
    }
    for bad in [
        "https://openrouter.ai/api/v1",
        "https://example.com/v1",
        "http://10.0.0.5:11434/v1",
        "http://localhost.example.com/v1",
        "file:///tmp/x",
        "http://user:pw@127.0.0.1/v1",
        "http://127.0.0.1/v1?x=1",
        "not a url",
    ] {
        let err = Endpoint::loopback(bad, Provider::Local).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Config, "{bad}");
    }
    let or = Endpoint::openrouter();
    assert_eq!(or.base(), "https://openrouter.ai/api/v1");
    assert_eq!(
        or.url("chat/completions"),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    assert_eq!(or.site_url("auth"), "https://openrouter.ai/auth");
    assert!(!or.is_loopback());
}

#[test]
fn client_debug_shows_no_key() {
    let client = OpenAiClient::new(ClientConfig::openrouter(KeyCell::with_key(
        ApiKey::new(FAKE_KEY.into()).unwrap(),
    )))
    .unwrap();
    let shown = format!("{client:?}");
    assert!(!shown.contains("0123456789"), "{shown}");
    assert!(shown.contains("openrouter.ai"));
}
