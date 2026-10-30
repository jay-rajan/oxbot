//! `oxbot chat <bot>`: a terminal client that speaks the same protocol as the
//! phone app.
#![allow(clippy::print_stdout, reason = "this module is a terminal UI")]

use std::io::Write as _;

use anyhow::{Context as _, bail};
use futures_util::{SinkExt, StreamExt};
use oxbot_protocol::{Answer, ClientMsg, EventKind, Outcome, ServerMsg, SignInOutcome};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

/// Connects to `url` and chats with `bot` until the user types `/quit` or
/// presses Ctrl-D.
///
/// # Errors
/// Fails if the server can't be reached or speaks another protocol version.
pub async fn run(url: &str, bot: &str) -> anyhow::Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("couldn't reach oxbot at {url}. Is `oxbot serve` running?"))?;
    let (mut sink, mut stream) = ws.split();

    let send =
        |msg: &ClientMsg| Message::Text(serde_json::to_string(msg).unwrap_or_default().into());
    sink.send(send(&ClientMsg::Subscribe {
        bot: bot.to_owned(),
        since: 0,
    }))
    .await?;

    println!(
        "Chatting with {bot}. /cancel stops a reply, /yes or /no answers a card, /quit leaves."
    );
    let mut view = View::default();
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            frame = stream.next() => {
                let Some(frame) = frame else { bail!("server closed the connection") };
                if let Message::Text(text) = frame? {
                    let msg: ServerMsg = serde_json::from_str(&text)?;
                    if let ServerMsg::Hello(hello) = &msg
                        && !hello.is_compatible()
                    {
                        bail!("server speaks protocol v{}; update oxbot", hello.protocol_version);
                    }
                    view.show(&msg);
                }
            }
            line = stdin.next_line() => {
                let Some(line) = line? else { break };
                let msg = match line.trim() {
                    "" => continue,
                    "/quit" => break,
                    "/cancel" => ClientMsg::Cancel { bot: bot.to_owned() },
                    "/yes" | "/no" => {
                        let Some(id) = view.card.take() else {
                            println!("   · nothing to answer");
                            continue;
                        };
                        let answer = if line.trim() == "/yes" { Answer::AllowOnce } else { Answer::Deny };
                        ClientMsg::Answer { id, answer }
                    }
                    text => ClientMsg::Send { bot: bot.to_owned(), text: text.to_owned() },
                };
                sink.send(send(&msg)).await?;
            }
        }
    }
    Ok(())
}

/// Prints messages, joining streamed deltas into one reply.
#[derive(Debug, Default)]
struct View {
    streaming: bool,
    /// The approval card waiting for `/yes` or `/no`.
    card: Option<u64>,
}

/// Red text in a terminal.
fn red(text: &str) -> String {
    format!("\x1b[31m{text}\x1b[0m")
}

impl View {
    fn show_event(&mut self, kind: &EventKind) {
        match kind {
            EventKind::UserMessage { text, from } => {
                println!("{} › {text}", from.as_deref().unwrap_or("you"));
            }
            EventKind::AssistantMessage { text } => {
                if self.streaming {
                    println!(); // already shown piece by piece
                    self.streaming = false;
                } else {
                    println!("bot › {text}");
                }
            }
            EventKind::ToolUse { name, summary } => println!("      · {name} {summary}"),
            EventKind::TurnDone { ok, duration_ms } => {
                #[allow(clippy::cast_precision_loss, reason = "a duration for display")]
                let secs = *duration_ms as f64 / 1000.0;
                println!(
                    "      ({} in {secs:.1}s)",
                    if *ok { "done" } else { "stopped" }
                );
            }
            EventKind::Approval {
                action,
                target,
                site,
                outcome,
            } => {
                let what = match outcome {
                    Outcome::Allowed => "You allowed",
                    Outcome::Denied => "You said no to",
                    Outcome::TimedOut => "No answer, so no to",
                };
                println!(
                    "   {}",
                    red(&format!("⚠ {what}: {action} {target} on {site}"))
                );
            }
            EventKind::Blocked { message } => println!("   {}", red(&format!("⚠ {message}"))),
            EventKind::SignIn { site, outcome } => {
                let what = match outcome {
                    SignInOutcome::Done => "Signed in to",
                    SignInOutcome::Cancelled => "Didn't sign in to",
                    SignInOutcome::TimedOut => "No answer; didn't sign in to",
                };
                println!("   · {what} {site}");
            }
            EventKind::Error { message } => println!("   ! {message}"),
        }
    }

    fn show(&mut self, msg: &ServerMsg) {
        match msg {
            ServerMsg::Hello(h) => println!(
                "Connected to {} (oxbot {}).",
                h.server_name, h.server_version
            ),
            ServerMsg::Delta { text, .. } => {
                if !self.streaming {
                    print!("bot › ");
                    self.streaming = true;
                }
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            ServerMsg::Event(e) => self.show_event(&e.kind),
            ServerMsg::ApprovalNeeded(a) => {
                self.card = Some(a.id);
                println!(
                    "   {}",
                    red(&format!(
                        "⚠ {} wants to {} {} on {}",
                        a.bot, a.action, a.target, a.site
                    ))
                );
                if let Some(detail) = &a.detail {
                    println!("     {}", red(detail));
                }
                println!("     /yes to allow once, /no to refuse (or answer on the oxbot page)");
            }
            ServerMsg::ApprovalResolved { id } => {
                if self.card == Some(*id) {
                    self.card = None;
                }
            }
            ServerMsg::Bots { bots } => {
                for b in bots {
                    println!("{} ({}){}", b.id, b.name, if b.busy { " busy" } else { "" });
                }
            }
            ServerMsg::SignInNeeded(s) if !s.window_open => {
                println!(
                    "   {}",
                    red(&format!(
                        "⚠ {} needs you to sign in to {} ({}). Open the oxbot app to sign in.",
                        s.bot, s.site, s.why
                    ))
                );
            }
            ServerMsg::Setup(report) if !report.ready() => {
                for check in report.checks.iter().filter(|c| !c.ok) {
                    println!(
                        "   ! {}: {} {}",
                        check.name,
                        check.detail,
                        check.fix.as_deref().unwrap_or("")
                    );
                }
            }
            ServerMsg::Notice { message, .. } => println!("   · {message}"),
            ServerMsg::Log(line) if line.level != "info" => {
                println!("   [{}] {}", line.level, line.message);
            }
            // The terminal client shows replies only; the page shows status and logs.
            ServerMsg::Log(_)
            | ServerMsg::Activity { .. }
            | ServerMsg::Browser { .. }
            | ServerMsg::BotCreated(_)
            | ServerMsg::Settings(_)
            | ServerMsg::SignInNeeded(_)
            | ServerMsg::SignInResolved { .. }
            | ServerMsg::Setup(_)
            | ServerMsg::SetupProgress { .. }
            | ServerMsg::Sites { .. }
            | ServerMsg::People { .. }
            | ServerMsg::Usage(_) => {}
            ServerMsg::Error { message } => println!("   ! {message}"),
        }
    }
}
