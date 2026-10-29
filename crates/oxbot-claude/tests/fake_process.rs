//! Runs `ClaudeProcess` against a fake `claude` script, so no real model is
//! called.
#![allow(clippy::unwrap_used, reason = "test helpers may panic")]

use std::path::PathBuf;

use oxbot_claude::{ClaudeConfig, ClaudeEvent, ClaudeProcess, Session};

fn fake_config(dir: &std::path::Path) -> ClaudeConfig {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut config = ClaudeConfig::new(
        dir,
        Session::New("33333333-4444-4555-8666-777777777777".into()),
    );
    config.program = root.join("fake-claude.sh");
    config.extra_env = vec![
        (
            "FAKE_CLAUDE_FIXTURE".into(),
            root.join("fixtures/tool-use.jsonl").display().to_string(),
        ),
        (
            "FAKE_CLAUDE_ARGS".into(),
            dir.join("args.txt").display().to_string(),
        ),
    ];
    config
}

#[tokio::test]
async fn a_turn_streams_events_until_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let (mut claude, mut events) = ClaudeProcess::spawn(&fake_config(dir.path())).unwrap();

    claude.send("save a note").await.unwrap();
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        let done = matches!(event, ClaudeEvent::TurnResult { .. });
        seen.push(event);
        if done {
            break;
        }
    }
    assert!(matches!(seen[0], ClaudeEvent::Init { .. }));
    assert!(matches!(
        seen.last(),
        Some(ClaudeEvent::TurnResult { ok: true, .. })
    ));

    let recorded = std::fs::read_to_string(dir.path().join("args.txt")).unwrap();
    assert!(recorded.contains("--input-format stream-json"));
    assert!(
        recorded.contains("DISABLE_TELEMETRY=1"),
        "privacy env must reach the child"
    );

    claude.interrupt().await.unwrap();
    assert!(matches!(
        events.recv().await,
        Some(ClaudeEvent::ControlAck { request_id }) if request_id == "oxbot-1"
    ));

    claude.shutdown().await;
    assert!(
        events.recv().await.is_none(),
        "channel closes when the child exits"
    );
}

#[tokio::test]
async fn a_missing_program_is_a_spawn_error() {
    let mut config = ClaudeConfig::new("/tmp", Session::New("x".into()));
    config.program = PathBuf::from("/definitely/not/claude");
    assert!(matches!(
        ClaudeProcess::spawn(&config),
        Err(oxbot_claude::ClaudeError::Spawn { .. })
    ));
}
