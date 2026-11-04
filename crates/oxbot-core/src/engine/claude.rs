//! The Claude engine: Claude Code (`claude -p`) as oxbot has always run it,
//! behind the engine traits.
//!
//! [`ClaudeEngine::start`] builds the same `ClaudeConfig` the hub used to
//! (same flags, environment, tool servers and resume rule) and spawns the
//! child. Two tasks then run each session:
//!
//! - an actor owns the `ClaudeProcess` and writes to it: messages,
//!   interrupts, and answers to permission requests and tool calls;
//! - a reader turns the child's events into [`EngineEvent`]s, in order. A
//!   permission request becomes [`EngineEvent::Permission`]; a message for
//!   oxbot's own tool server is answered by [`tools::handle`], whose tool
//!   calls become [`EngineEvent::HostTool`]s.
//!
//! The reader never waits on the actor, and tasks waiting for an answer
//! hold only weak handles: dropping the session kills the child at once,
//! as dropping the `ClaudeProcess` always did.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use oxbot_browser::MCP_SERVER_NAME;
use oxbot_claude::{
    ClaudeConfig, ClaudeError, ClaudeEvent, ClaudeProcess, Decision as ClaudeDecision, Session,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::{
    BoxFuture, CallRecord, Decision, Engine, EngineError, EngineEvent, EngineKind, HostToolCall,
    ModelChoice, NotReady, PermissionRequest, Reply, Resume, SessionSpec, Started, TurnInput,
};
use crate::tools::{self, TOOL_SERVER_NAME, ToolSet};

/// Room for events the hub hasn't read yet, as before.
const EVENT_ROOM: usize = 256;

/// Runs bots on Claude Code.
#[derive(Debug, Clone)]
pub struct ClaudeEngine {
    /// The `claude` program. Tests point this at a fake.
    program: PathBuf,
}

impl ClaudeEngine {
    /// An engine that runs `program`.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The `claude` child's config for `spec`.
    #[must_use]
    pub fn config(&self, spec: &SessionSpec) -> ClaudeConfig {
        let session = match &spec.resume {
            Resume::New(id) => Session::New(id.clone()),
            Resume::Resume(id) => Session::Resume(id.clone()),
        };
        let mut config = ClaudeConfig::new(&spec.work_dir, session);
        config.program.clone_from(&self.program);
        // A pinned loop model means nothing to Claude Code.
        config.model = match &spec.model {
            ModelChoice::Claude(alias) | ModelChoice::Only(alias) => Some(alias.clone()),
            ModelChoice::Auto | ModelChoice::Pin(_) => None,
        };
        config.extra_env.clone_from(&spec.env);
        config.instructions = Some(spec.system_prompt.clone());
        // Every tool that isn't pre-allowed is checked by oxbot.
        config.ask_oxbot = true;

        let mut servers = serde_json::Map::new();
        if let Some(browser) = &spec.browser {
            servers.insert(MCP_SERVER_NAME.to_owned(), browser.mcp_json.clone());
            servers.insert(TOOL_SERVER_NAME.to_owned(), tools::mcp_entry());
        }
        // Herder always gets its team tools, browser or not.
        if spec.host_tools.central {
            servers.insert(TOOL_SERVER_NAME.to_owned(), tools::mcp_entry());
        }
        if !servers.is_empty() {
            config.mcp_config = Some(json!({ "mcpServers": servers }).to_string());
            // A sign-in or a task can take minutes: let a tool call wait that long.
            config.extra_env.push((
                "MCP_TOOL_TIMEOUT".to_owned(),
                spec.tool_wait.as_millis().to_string(),
            ));
        }
        config
    }

    fn spawn(&self, spec: &SessionSpec) -> Result<Started, EngineError> {
        let config = self.config(spec);
        let (process, claude_events) =
            ClaudeProcess::spawn(&config).map_err(|err| EngineError::Start(err.to_string()))?;
        let (events, events_rx) = mpsc::channel(EVENT_ROOM);
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let reader = Reader {
            bot: spec.bot.clone(),
            session: spec.resume.id().to_owned(),
            set: ToolSet::from(spec.host_tools),
            events,
            commands: commands.downgrade(),
            requests: Mutex::new(Requests::default()),
        };
        tokio::spawn(run_process(process, commands_rx, spec.bot.clone()));
        tokio::spawn(reader.run(claude_events));
        Ok(Started {
            session: Box::new(ClaudeSession {
                id: spec.resume.id().to_owned(),
                commands,
            }),
            events: events_rx,
            resumed_turn: None,
        })
    }
}

impl Engine for ClaudeEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Claude
    }

    fn ready(&self) -> Result<(), NotReady> {
        Ok(())
    }

    fn start(&self, spec: SessionSpec) -> BoxFuture<'_, Result<Started, EngineError>> {
        let started = self.spawn(&spec);
        Box::pin(async move { started })
    }
}

/// What the actor does with the child.
enum Command {
    Send {
        text: String,
        done: oneshot::Sender<Result<(), ClaudeError>>,
    },
    Interrupt {
        done: oneshot::Sender<Result<(), ClaudeError>>,
    },
    Answer {
        request_id: String,
        input: Value,
        decision: ClaudeDecision,
    },
    AnswerMcp {
        request_id: String,
        response: Value,
    },
    Shutdown {
        done: oneshot::Sender<()>,
    },
}

/// A running `claude` child.
struct ClaudeSession {
    id: String,
    /// The only strong handle to the actor: dropping the session ends it,
    /// which kills the child.
    commands: mpsc::UnboundedSender<Command>,
}

impl ClaudeSession {
    async fn ask(
        &self,
        command: impl FnOnce(oneshot::Sender<Result<(), ClaudeError>>) -> Command,
    ) -> Result<(), EngineError> {
        let (done, wait) = oneshot::channel();
        self.commands.send(command(done)).map_err(|_| stopped())?;
        match wait.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(EngineError::Stopped(err.to_string())),
            Err(_) => Err(stopped()),
        }
    }
}

fn stopped() -> EngineError {
    EngineError::Stopped("claude stopped".to_owned())
}

impl super::Session for ClaudeSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn send(&mut self, turn: TurnInput) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move {
            self.ask(|done| Command::Send {
                text: turn.text,
                done,
            })
            .await
        })
    }

    fn interrupt(&mut self) -> BoxFuture<'_, Result<(), EngineError>> {
        Box::pin(async move { self.ask(|done| Command::Interrupt { done }).await })
    }

    fn shutdown(self: Box<Self>) -> BoxFuture<'static, ()> {
        let commands = self.commands;
        Box::pin(async move {
            let (done, wait) = oneshot::channel();
            if commands.send(Command::Shutdown { done }).is_ok() {
                let _ = wait.await;
            }
        })
    }
}

/// The actor: the only writer to the child. Ends (and kills the child) on
/// shutdown or when the session is dropped.
async fn run_process(
    mut process: ClaudeProcess,
    mut commands: mpsc::UnboundedReceiver<Command>,
    bot: String,
) {
    while let Some(command) = commands.recv().await {
        match command {
            Command::Send { text, done } => {
                let _ = done.send(process.send(&text).await);
            }
            Command::Interrupt { done } => {
                let _ = done.send(process.interrupt().await);
            }
            Command::Answer {
                request_id,
                input,
                decision,
            } => {
                if let Err(err) = process.answer(&request_id, &input, &decision).await {
                    tracing::warn!(%bot, %err, "couldn't answer Claude's permission request");
                }
            }
            Command::AnswerMcp {
                request_id,
                response,
            } => {
                if let Err(err) = process.answer_mcp(&request_id, response).await {
                    tracing::warn!(%bot, %err, "couldn't answer Claude's tool call");
                }
            }
            Command::Shutdown { done } => {
                process.shutdown().await;
                let _ = done.send(());
                return;
            }
        }
    }
    // The session was dropped: dropping the process kills the child.
}

/// Turns one child's events into engine events.
struct Reader {
    bot: String,
    /// The Claude session.
    session: String,
    /// oxbot's own tools this bot has (fixed for the session).
    set: ToolSet,
    events: mpsc::Sender<EngineEvent>,
    commands: mpsc::WeakUnboundedSender<Command>,
    /// Model requests under way, for the meter.
    requests: Mutex<Requests>,
}

/// Claude's model requests under way in a session: each one's model and
/// start, by the API's message id, and how many there were this turn.
#[derive(Debug, Default)]
struct Requests {
    started: HashMap<String, (String, Instant, i64)>,
    step: u32,
}

impl Reader {
    /// One finished request as a meter record: the same row the loop
    /// writes, from Claude's own counts (no cost: the turn has the total).
    fn request_done(&self, id: &str, usage: &oxbot_claude::TurnUsage) -> CallRecord {
        let mut requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        let (model, started, ts_ms) = requests
            .started
            .remove(id)
            .unwrap_or_else(|| (String::new(), Instant::now(), crate::store::now_ms()));
        requests.step += 1;
        CallRecord {
            ts_ms,
            day: oxbot_loop::meter::utc_day(ts_ms),
            bot: self.bot.clone(),
            person: None,
            session: Some(self.session.clone()),
            turn: None,
            step: Some(requests.step),
            provider: super::CLAUDE_PROVIDER.to_owned(),
            engine: "claude".to_owned(),
            requested: model.clone(),
            model,
            free: false,
            private: false,
            generation_id: Some(id.to_owned()).filter(|i| !i.is_empty()),
            prompt_tokens: usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens,
            completion_tokens: usage.output_tokens,
            cached_tokens: usage.cache_read_tokens,
            cost_usd: 0.0,
            latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            outcome: "ok".to_owned(),
            kind: oxbot_engine::CallKind::Chat,
        }
    }
}

impl Reader {
    async fn run(self, mut claude: mpsc::Receiver<ClaudeEvent>) {
        while let Some(event) = claude.recv().await {
            let Some(event) = self.convert(event) else {
                continue;
            };
            if self.events.send(event).await.is_err() {
                break;
            }
        }
        // Dropping `events` closes the hub's channel: the session ended.
    }

    fn convert(&self, event: ClaudeEvent) -> Option<EngineEvent> {
        Some(match event {
            ClaudeEvent::Init { model, .. } => {
                // A new turn: its requests count from one.
                self.requests
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .step = 0;
                EngineEvent::Started { model }
            }
            ClaudeEvent::RequestStarted { id, model } => {
                self.requests
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .started
                    .insert(id, (model, Instant::now(), crate::store::now_ms()));
                return None;
            }
            ClaudeEvent::RequestDone { id, usage } => {
                EngineEvent::ModelCall(self.request_done(&id, &usage))
            }
            ClaudeEvent::TextDelta(text) => EngineEvent::TextDelta(text),
            ClaudeEvent::Assistant { text, tool_uses } => {
                EngineEvent::Assistant { text, tool_uses }
            }
            ClaudeEvent::ToolResult { tool_use_id, text } => {
                EngineEvent::ToolResult { tool_use_id, text }
            }
            ClaudeEvent::Limits(windows) => EngineEvent::Limits(windows),
            ClaudeEvent::Activity(activity) => EngineEvent::Activity(activity),
            ClaudeEvent::TurnResult {
                ok,
                text,
                duration_ms,
                usage,
            } => EngineEvent::TurnDone {
                ok,
                text,
                duration_ms,
                usage: Some(usage),
            },
            ClaudeEvent::Permission(request) => {
                let (reply, wait) = Reply::channel();
                let commands = self.commands.clone();
                let request_id = request.request_id;
                let input = request.input.clone();
                tokio::spawn(async move {
                    let decision = claude_decision(wait.await);
                    if let Some(commands) = commands.upgrade() {
                        let _ = commands.send(Command::Answer {
                            request_id,
                            input,
                            decision,
                        });
                    }
                });
                EngineEvent::Permission(PermissionRequest {
                    tool: request.tool,
                    input: request.input,
                    tool_use_id: request.tool_use_id,
                    reply,
                })
            }
            ClaudeEvent::McpMessage {
                request_id,
                server,
                message,
            } => {
                if server == TOOL_SERVER_NAME {
                    tokio::spawn(host_message(
                        message,
                        request_id,
                        self.set,
                        self.events.downgrade(),
                        self.commands.clone(),
                    ));
                } else {
                    let bot = &self.bot;
                    tracing::warn!(%bot, server, "message for a tool server oxbot doesn't host");
                }
                return None;
            }
            ClaudeEvent::ControlAck { .. } => return None,
        })
    }
}

/// Answers one message for oxbot's own tool server: each tool call goes
/// to the hub as [`EngineEvent::HostTool`].
async fn host_message(
    message: Value,
    request_id: String,
    set: ToolSet,
    events: mpsc::WeakSender<EngineEvent>,
    commands: mpsc::WeakUnboundedSender<Command>,
) {
    let response = tools::handle(&message, set, |name, args| async move {
        let (reply, wait) = Reply::channel();
        let call = EngineEvent::HostTool(HostToolCall { name, args, reply });
        // A strong handle only while sending: a long tool call mustn't keep
        // the session's events open. Not sent, the reply is dropped: a no.
        if let Some(events) = events.upgrade() {
            let _ = events.send(call).await;
        }
        let answer = wait.await;
        (answer.ok, answer.text)
    })
    .await;
    if let Some(commands) = commands.upgrade() {
        let _ = commands.send(Command::AnswerMcp {
            request_id,
            response,
        });
    }
}

/// The gate's answer as Claude takes it. Claude can't fake a tool's
/// result, so a practice run's result is a no with the same words.
fn claude_decision(decision: Decision) -> ClaudeDecision {
    match decision {
        Decision::Allow => ClaudeDecision::Allow,
        // A practice run: the same words the loop gives as the tool's result.
        Decision::Deny(message) | Decision::Simulate(message) => ClaudeDecision::Deny(message),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::engine::{BrowserSpec, GateMode, HostTools, McpCommand, Privacy};

    fn spec() -> SessionSpec {
        SessionSpec {
            bot: "helper".into(),
            work_dir: PathBuf::from("/bots/helper/work"),
            context_dir: PathBuf::from("/bots/helper/context"),
            knowledge_dir: None,
            system_prompt: "Be brief.".into(),
            resume: Resume::New("abc".into()),
            model: ModelChoice::Claude("sonnet".into()),
            browser: None,
            host_tools: HostTools::default(),
            host_tool_specs: Vec::new(),
            privacy: Privacy::Standard,
            gate_mode: GateMode::Live,
            env: vec![("PATH".into(), "/bin".into())],
            tool_wait: Duration::from_secs(90),
        }
    }

    #[test]
    fn each_claude_request_is_a_meter_row() {
        let (events, _rx) = mpsc::channel(4);
        let (commands, _crx) = mpsc::unbounded_channel::<Command>();
        let reader = Reader {
            bot: "helper".into(),
            session: "abc".into(),
            set: ToolSet::from(HostTools::default()),
            events,
            commands: commands.downgrade(),
            requests: Mutex::new(Requests::default()),
        };
        assert!(
            reader
                .convert(ClaudeEvent::Init {
                    session_id: "abc".into(),
                    model: "claude-sonnet-4-5".into(),
                })
                .is_some()
        );
        assert!(
            reader
                .convert(ClaudeEvent::RequestStarted {
                    id: "msg_1".into(),
                    model: "claude-sonnet-4-5".into(),
                })
                .is_none()
        );
        let usage = oxbot_claude::TurnUsage {
            input_tokens: 2,
            output_tokens: 40,
            cache_read_tokens: 3_000,
            cache_write_tokens: 500,
            cost_usd: 0.0,
        };
        let Some(EngineEvent::ModelCall(record)) = reader.convert(ClaudeEvent::RequestDone {
            id: "msg_1".into(),
            usage,
        }) else {
            panic!("no meter row");
        };
        assert_eq!(record.provider, "anthropic");
        assert_eq!(record.model, "claude-sonnet-4-5");
        assert_eq!(record.bot, "helper");
        assert_eq!(record.session.as_deref(), Some("abc"));
        assert_eq!(record.step, Some(1));
        assert_eq!(record.prompt_tokens, 3_502);
        assert_eq!(record.cached_tokens, 3_000);
        assert_eq!(record.completion_tokens, 40);
        assert_eq!(record.kind, oxbot_engine::CallKind::Chat);
        assert!(!record.free && record.cost_usd == 0.0);
        assert_eq!(record.outcome, "ok");
    }

    #[test]
    fn the_config_is_what_the_hub_always_built() {
        let engine = ClaudeEngine::new("/bin/claude");
        let config = engine.config(&spec());
        assert_eq!(config.program, PathBuf::from("/bin/claude"));
        assert_eq!(config.session, Session::New("abc".into()));
        assert_eq!(config.model.as_deref(), Some("sonnet"));
        assert_eq!(config.instructions.as_deref(), Some("Be brief."));
        assert!(config.ask_oxbot);
        // No browser and not Herder: no tool servers, no tool wait.
        assert_eq!(config.mcp_config, None);
        assert_eq!(config.extra_env, vec![("PATH".into(), "/bin".into())]);
    }

    #[test]
    fn a_browser_or_herder_gets_oxbots_tool_server() {
        let engine = ClaudeEngine::new("claude");
        let mut with_browser = spec();
        with_browser.browser = Some(BrowserSpec {
            mcp: McpCommand::default(),
            mcp_json: json!({"command": "node"}),
        });
        let config = engine.config(&with_browser);
        let servers: Value = serde_json::from_str(config.mcp_config.as_deref().unwrap()).unwrap();
        assert_eq!(servers["mcpServers"][MCP_SERVER_NAME]["command"], "node");
        assert_eq!(servers["mcpServers"][TOOL_SERVER_NAME]["type"], "sdk");
        assert!(
            config
                .extra_env
                .contains(&("MCP_TOOL_TIMEOUT".into(), "90000".into()))
        );

        let mut herder = spec();
        herder.host_tools.central = true;
        herder.model = ModelChoice::Auto;
        let config = engine.config(&herder);
        let servers: Value = serde_json::from_str(config.mcp_config.as_deref().unwrap()).unwrap();
        assert_eq!(
            servers["mcpServers"].as_object().unwrap().len(),
            1,
            "Herder without a browser gets only its own tools"
        );
        assert_eq!(config.model, None);
        herder.model = ModelChoice::Pin("some/model:free".into());
        assert_eq!(engine.config(&herder).model, None);
    }

    #[test]
    fn a_practice_run_is_a_no_for_claude() {
        assert_eq!(claude_decision(Decision::Allow), ClaudeDecision::Allow);
        assert_eq!(
            claude_decision(Decision::Deny("no".into())),
            ClaudeDecision::Deny("no".into())
        );
        let ClaudeDecision::Deny(text) =
            claude_decision(Decision::Simulate("(would click)".into()))
        else {
            panic!("a practice run must not run")
        };
        assert_eq!(text, "(would click)", "the same words the loop gives");
    }

    #[tokio::test]
    async fn a_missing_program_fails_to_start() {
        let engine = ClaudeEngine::new("/no/such/claude");
        let mut spec = spec();
        spec.work_dir = std::env::temp_dir();
        let err = engine.start(spec).await.unwrap_err();
        assert!(matches!(err, EngineError::Start(_)));
        assert!(
            err.to_string()
                .starts_with("couldn't start /no/such/claude")
        );
    }
}
