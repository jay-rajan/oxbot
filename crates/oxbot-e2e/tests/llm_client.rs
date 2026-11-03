//! oxbot's own model client (`oxbot-llm`) against the stand-in
//! `OpenRouter`: what the stand-in speaks is what the client expects.

#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::Path;

use oxbot_e2e::http::send;
use oxbot_e2e::serve::FakeOpenRouter;
use oxbot_llm::{
    ApiKey, Auth, CancellationToken, ChatRequest, ClientConfig, Endpoint, KeyCell, Message,
    OpenAiClient, Part, Pkce, Provider, ProviderPrefs, Timeouts, ToolSpec, auth_url, complete,
    exchange_code,
};

const SCRIPT: &str = r###"
[[reply]]
when = "search jobs"
say = "Looking."
tool = "mcp__chrome-devtools__click"
args = { uid = "2_4" }
snapshot = "## Pages\n1: Jobs (https://jobs.example.com/) [selected]\n## Latest page snapshot\nuid=2_1 RootWebArea \"Jobs\"\n  uid=2_4 button \"Search\""
after = "Searched the jobs."

[[reply]]
when = "busy"
say = "Answered."
busy = ["e2e/tool-model:free"]

[[reply]]
say = "Got it."
"###;

async fn start(dir: &Path) -> FakeOpenRouter {
    let script = dir.join("script.toml");
    std::fs::write(&script, SCRIPT).unwrap();
    FakeOpenRouter::start(
        Path::new(env!("CARGO_BIN_EXE_oxbot-fake-openrouter")),
        &script,
        None,
        None,
    )
    .await
    .unwrap()
}

fn client(fake: &FakeOpenRouter, key: KeyCell) -> OpenAiClient {
    OpenAiClient::new(ClientConfig {
        endpoint: Endpoint::loopback(&fake.api_url(), Provider::OpenRouter).unwrap(),
        auth: Auth::Key(key),
        timeouts: Timeouts::default(),
    })
    .unwrap()
}

fn signed_in(fake: &FakeOpenRouter) -> OpenAiClient {
    client(
        fake,
        KeyCell::with_key(ApiKey::new(fake.canary.clone()).unwrap()),
    )
}

fn tools() -> Vec<ToolSpec> {
    oxbot_e2e::fake_devtools::tools()
        .iter()
        .map(|t| ToolSpec {
            name: format!("mcp__chrome-devtools__{}", t["name"].as_str().unwrap()),
            description: t["description"].as_str().unwrap_or("").to_owned(),
            parameters: t["inputSchema"].clone(),
        })
        .collect()
}

fn request(model: &str, fallbacks: &[&str], messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        fallbacks: fallbacks.iter().map(|m| (*m).to_owned()).collect(),
        messages,
        tools: tools(),
        parallel_tool_calls: Some(false),
        max_tokens: Some(4096),
        provider: Some(ProviderPrefs::private_bot()),
        // As oxbot's router sends it: only what every model asked takes
        // (the backup has no tool_choice), since require_parameters is on.
        supported: Some(vec!["tools".into(), "max_tokens".into()]),
        ..ChatRequest::default()
    }
}

#[tokio::test]
async fn the_client_runs_a_browsing_turn_on_the_stand_in() {
    let dir = tempfile::tempdir().unwrap();
    let fake = start(dir.path()).await;
    let llm = signed_in(&fake);

    let catalog = llm.models().await.unwrap();
    let free: Vec<&str> = catalog.free_for_loop().map(|m| m.id.as_str()).collect();
    assert_eq!(free, ["e2e/tool-model:free", "e2e/backup:free"]);
    let key = llm.key_info().await.unwrap();
    assert_eq!(key.free_daily.unwrap().remaining, 1000);

    let mut messages = vec![Message::system("rules"), Message::user("[Jay] search jobs")];
    let mut called = Vec::new();
    let text = loop {
        let mut streamed = String::new();
        let answer = complete(
            &llm,
            request("e2e/tool-model:free", &[], messages.clone()),
            CancellationToken::new(),
            |d| {
                if let oxbot_llm::ChatDelta::Text(t) = d {
                    streamed.push_str(t);
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(answer.model, "e2e/tool-model:free");
        // The stand-in counts the request: a token per 3 bytes (the tool
        // list alone is several thousand).
        assert!(
            answer.usage.prompt_tokens > 1000,
            "{}",
            answer.usage.prompt_tokens
        );
        let calls = answer.message.tool_calls.clone();
        messages.push(answer.message.clone());
        let Some(call) = calls.first() else {
            break streamed;
        };
        called.push((call.name.clone(), call.arguments.clone()));
        messages.push(Message::tool(
            call.id.clone(),
            vec![Part::Text("done".into())],
        ));
        assert!(called.len() < 10);
    };
    let names: Vec<&str> = called.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "mcp__chrome-devtools__navigate_page",
            "mcp__chrome-devtools__take_snapshot",
            "mcp__chrome-devtools__click"
        ]
    );
    assert_eq!(called[2].1["uid"], "2_4");
    assert_eq!(text, "Searched the jobs.");

    let log = fake.requests();
    assert!(log.contains(r#""data_collection":"deny""#), "{log}");
    assert!(log.contains(r#""bearer":"ok""#));
    assert!(!log.contains(r#""status":4"#), "{log}");
}

#[tokio::test]
async fn busy_models_fall_back_or_answer_429() {
    let dir = tempfile::tempdir().unwrap();
    let fake = start(dir.path()).await;
    let llm = signed_in(&fake);
    let ask = || vec![Message::user("[Jay] busy?")];
    let answer = complete(
        &llm,
        request("e2e/tool-model:free", &["e2e/backup:free"], ask()),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(answer.model, "e2e/backup:free");
    let err = complete(
        &llm,
        request("e2e/tool-model:free", &[], ask()),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, Some(429));
    assert_eq!(err.retry_after, Some(std::time::Duration::from_secs(1)));
    // Without the key, the stand-in refuses.
    let err = complete(
        &client(
            &fake,
            KeyCell::with_key(ApiKey::new("sk-or-v1-wrong".into()).unwrap()),
        ),
        request("e2e/backup:free", &[], ask()),
        CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, Some(401));
}

#[tokio::test]
async fn sign_in_hands_the_client_the_canary() {
    let dir = tempfile::tempdir().unwrap();
    let fake = start(dir.path()).await;
    let llm = client(&fake, KeyCell::new());
    let pkce = Pkce::new().unwrap();
    let url = auth_url(
        llm.endpoint(),
        "http://localhost:7878/openrouter/callback",
        &pkce,
        "st4te",
        "oxbot on Mac",
    );
    let prefix = format!("http://127.0.0.1:{}", fake.port);
    let path = url.strip_prefix(&prefix).unwrap();
    assert!(path.starts_with("/auth?"), "{url}");
    let page = send(fake.port, "GET", path, &[], None).await.unwrap();
    assert_eq!(page.status, 200, "{}", page.body);
    let at = page.body.find("name=code value=\"").unwrap() + 17;
    let code = &page.body[at..at + 32];
    let back = send(
        fake.port,
        "GET",
        &format!("/auth/approve?code={code}"),
        &[],
        None,
    )
    .await
    .unwrap();
    assert!(back.header("location").unwrap().ends_with("&state=st4te"));
    let key = exchange_code(&llm, code, &pkce).await.unwrap();
    assert_eq!(key.expose_for_storage(), fake.canary.as_bytes());
    // A code works once.
    assert!(exchange_code(&llm, code, &pkce).await.is_err());
}
