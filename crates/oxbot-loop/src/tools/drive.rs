//! The tools that drive the browser themselves, several steps per call:
//! `read_pages` ([`super::read_pages`]) and `page_steps`
//! ([`super::steps`]). Every step is a browser tool call of its own:
//!
//! 1. asked of the gate quietly (the same rules as a step the model asks
//!    for, but a step that would need the person's OK stops the run
//!    instead of showing an approval request);
//! 2. told to the hub as its own tool call (a line in the chat, and the
//!    hub learns the page, its elements and the taint from its result);
//! 3. run through the bot's browser tool.
//!
//! `read_pages` then asks the hub to copy each page and run the bot's
//! script on the copy in the sealed tab ([`EngineEvent::Sealed`]).

use std::sync::PoisonError;

use oxbot_engine::{EngineEvent, Reply, SealedCall, SealedReply, SealedWork, ToolUse};
use oxbot_llm::ToolCall;
use serde_json::{Value, json};

use super::browser::BROWSER_PREFIX;
use super::read_pages::{self, PageRead, Pages};
use super::steps::{self, Stop};
use super::{Raw, ToolEnv, ToolOutcome, finish};
use crate::gate::{GateCheck, Gated};

/// How one step ended.
#[derive(Debug)]
enum StepDone {
    /// It ran.
    Ran,
    /// The browser tool said it failed.
    Failed(String),
    /// It would need the person's OK.
    WouldAsk(String),
    /// oxbot said no.
    Denied(String),
    /// A practice run recorded it instead.
    Simulated(String),
    /// The task was stopped.
    Stopped,
}

/// One browser step for call `parent`, its `n`th: tool `tool` (its own
/// name) with `args`.
async fn step(
    gate: &GateCheck,
    env: &ToolEnv<'_>,
    parent: &str,
    n: usize,
    tool: &str,
    args: Value,
) -> StepDone {
    let Some(browser) = env.browser else {
        return StepDone::Failed("This bot has no browser right now.".to_owned());
    };
    let name = format!("{BROWSER_PREFIX}{tool}");
    let id = format!("{parent}~{n}");
    match gate.check_quiet(&name, &args, &id, env.cancel).await {
        None => return StepDone::Stopped,
        Some(Gated::Run(_)) => {}
        Some(Gated::Denied(why)) => return StepDone::Denied(why),
        Some(Gated::WouldAsk(what)) => return StepDone::WouldAsk(what),
        Some(Gated::Simulated(text)) => return StepDone::Simulated(text),
    }
    let _ = env
        .events
        .send(EngineEvent::Assistant {
            text: String::new(),
            tool_uses: vec![ToolUse {
                id: id.clone(),
                name,
                input: args.clone(),
            }],
        })
        .await;
    let out = tokio::select! {
        r = browser.call(tool, args) => r,
        () = env.cancel.cancelled() => return StepDone::Stopped,
    };
    let (text, failed) = match out {
        Ok(out) => (out.text, out.is_error),
        Err(err) => (err, true),
    };
    env.pages
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .learn(&text);
    let _ = env
        .events
        .send(EngineEvent::ToolResult {
            tool_use_id: id,
            text: text.clone(),
        })
        .await;
    if failed {
        StepDone::Failed(super::short_error(&text))
    } else {
        StepDone::Ran
    }
}

/// The selected tab's id, asking the browser tool for its pages when the
/// session doesn't know it yet (the browser tool needs it for every step).
async fn page_id(gate: &GateCheck, env: &ToolEnv<'_>, parent: &str) -> Option<u32> {
    let known = env
        .pages
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .selected_id();
    if known.is_some() {
        return known;
    }
    let _ = step(gate, env, parent, 0, "list_pages", json!({})).await;
    env.pages
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .selected_id()
}

/// The selected tab's address, as last known.
fn current_url(env: &ToolEnv<'_>) -> Option<String> {
    env.pages
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .current()
        .map(|p| p.url)
        .filter(|u| !u.is_empty())
}

/// Asks the hub to do `work` in the sealed tab.
async fn sealed(env: &ToolEnv<'_>, work: SealedWork) -> SealedReply {
    let (reply, wait) = Reply::<SealedReply>::channel();
    let _ = env
        .events
        .send(EngineEvent::Sealed(SealedCall { work, reply }))
        .await;
    tokio::select! {
        r = wait => r,
        () = env.cancel.cancelled() => SealedReply::failed("Stopped."),
    }
}

/// How opening one page of `read_pages` went.
enum Visit {
    /// It's open.
    Open,
    /// It didn't open: the page's line in the table.
    Failed(PageRead),
    /// The run stops here: why.
    Stop(String),
}

/// Opens page `n` (`url`) of a `read_pages` call `parent`, as a visit the
/// gate checks.
async fn visit(gate: &GateCheck, env: &ToolEnv<'_>, parent: &str, n: usize, url: &str) -> Visit {
    let mut args = json!({"type": "url", "url": url});
    if let Some(id) = page_id(gate, env, parent).await {
        args["pageId"] = json!(id);
    }
    let failed = |why: String| {
        Visit::Failed(PageRead {
            url: url.to_owned(),
            outcome: Err(why),
            cut: false,
        })
    };
    match step(gate, env, parent, n, "navigate_page", args).await {
        StepDone::Ran => Visit::Open,
        StepDone::Failed(err) => failed(format!("it didn't open: {err}")),
        StepDone::Denied(why) => failed(format!("oxbot didn't open it: {why}")),
        StepDone::WouldAsk(what) => Visit::Stop(format!(
            "page {n} ({url}) would need the person's OK ({what}), so it wasn't opened. Open it \
             yourself if it's needed, and they're asked."
        )),
        StepDone::Simulated(text) => Visit::Stop(format!("this is a practice run: {text}")),
        StepDone::Stopped => Visit::Stop("the task was stopped.".to_owned()),
    }
}

/// `read_pages`: opens each page, has the hub run the script on a sealed
/// copy, and hands back one table.
pub(crate) async fn read_pages(
    call: &ToolCall,
    gate: &GateCheck,
    env: &ToolEnv<'_>,
) -> ToolOutcome {
    let plan = match read_pages::plan(&call.arguments) {
        Ok(plan) => plan,
        Err(why) => return ToolOutcome::note(format!("[tool error] {why}"), true),
    };
    if env.browser.is_none() {
        return ToolOutcome::note("[tool error] This bot has no browser right now.", true);
    }
    let mut reads: Vec<PageRead> = Vec::new();
    let mut stopped: Option<String> = None;
    let mut next: Option<String> = None;
    for n in 0..plan.max_pages() {
        let target = match &plan.pages {
            Pages::Urls(urls) => urls.get(n).cloned(),
            Pages::Follow { .. } if n == 0 => None,
            Pages::Follow { .. } => match next.take() {
                Some(url) => Some(url),
                None => break,
            },
        };
        if let Some(url) = &target {
            match visit(gate, env, &call.id, n + 1, url).await {
                Visit::Open => {}
                Visit::Failed(read) => {
                    reads.push(read);
                    if plan.follow().is_some() {
                        break;
                    }
                    continue;
                }
                Visit::Stop(why) => {
                    stopped = Some(why);
                    break;
                }
            }
        }
        let Some(url) = current_url(env).or(target) else {
            stopped = Some("the browser isn't on a page yet: give urls.".to_owned());
            break;
        };
        let reply = sealed(
            env,
            SealedWork::Read {
                url: url.clone(),
                script: plan.script.clone(),
                follow: plan.follow().map(str::to_owned),
            },
        )
        .await;
        if env.cancel.is_cancelled() {
            stopped = Some("the task was stopped.".to_owned());
            break;
        }
        reads.push(PageRead {
            url: url.clone(),
            outcome: if reply.ok {
                Ok(reply.value)
            } else {
                Err(reply.error)
            },
            cut: reply.cut,
        });
        // The same page again would go round in circles.
        next = reply.next.filter(|n| n != &url);
    }
    if plan.follow().is_some() && next.is_some() && stopped.is_none() {
        stopped = Some(format!(
            "max_pages ({}) was reached; there are more pages.",
            plan.max_pages()
        ));
    }
    let _ = sealed(env, SealedWork::Done).await;
    let (text, _) = read_pages::table(&reads, stopped.as_deref());
    let urls: Vec<&str> = reads.iter().map(|r| r.url.as_str()).take(3).collect();
    let why = if urls.is_empty() {
        "web pages".to_owned()
    } else {
        format!(
            "web pages ({}{})",
            urls.join(", "),
            if reads.len() > 3 { ", …" } else { "" }
        )
    };
    let raw = Raw {
        text,
        wrap: Some((
            "web_page",
            format!(" url=\"{} pages read with a script\"", reads.len()),
        )),
        error: false,
        why: Some(why),
        page: None,
        taints: true,
        snapshot: None,
    };
    finish(raw, &call.name, true, env)
}

/// `page_steps`: runs the steps in a row, each through the gate, and stops
/// at the first that needs the person's OK or leaves the site.
pub(crate) async fn page_steps(
    call: &ToolCall,
    gate: &GateCheck,
    env: &ToolEnv<'_>,
) -> ToolOutcome {
    let list = match steps::parse(&call.arguments) {
        Ok(list) => list,
        Err(why) => return ToolOutcome::note(format!("[tool error] {why}"), true),
    };
    if env.browser.is_none() {
        return ToolOutcome::note("[tool error] This bot has no browser right now.", true);
    }
    let id = page_id(gate, env, &call.id).await;
    let start = current_url(env).unwrap_or_default();
    let site = steps::host_of(&start).unwrap_or_default();
    let mut ran = 0;
    let mut stop: Option<(usize, Stop)> = None;
    for (i, s) in list.iter().enumerate() {
        let (tool, args) = s.browser_call(id);
        let why = match step(gate, env, &call.id, i + 1, tool, args).await {
            StepDone::Ran => {
                ran += 1;
                let now = current_url(env).and_then(|u| steps::host_of(&u));
                match now {
                    Some(host) if !site.is_empty() && host != site => Stop::LeftSite(host),
                    _ => continue,
                }
            }
            StepDone::Failed(err) => Stop::Failed(err),
            StepDone::WouldAsk(what) | StepDone::Simulated(what) => Stop::NeedsOk(what),
            StepDone::Denied(why) => Stop::Refused(why),
            StepDone::Stopped => Stop::Stopped,
        };
        stop = Some((i, why));
        break;
    }
    let text = steps::report(&list, &site, ran, stop.as_ref().map(|(i, s)| (*i, s)));
    let mut outcome = ToolOutcome::note(text, false);
    // What would be asked names the page's own buttons: web text.
    outcome.untrusted = true;
    outcome.meta.tool = Some(call.name.clone());
    outcome.meta.trust_why = Some(if start.is_empty() {
        "a web page".to_owned()
    } else {
        format!("a web page ({start})")
    });
    outcome.meta.page_url = Some(start).filter(|u| !u.is_empty());
    outcome
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use oxbot_engine::Decision;
    use oxbot_llm::Part;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::budget::TokenBudget;
    use crate::results::ResultStore;
    use crate::store::MemoryLoopStore;
    use crate::tools::browser::{BrowserOutput, BrowserTools, ScriptedBrowser};
    use crate::tools::pages::PageTracker;

    /// A browser on `start` whose pages are whatever it's sent to.
    fn browser() -> Arc<ScriptedBrowser> {
        ScriptedBrowser::new(|name, args| {
            let text = match name {
                "navigate_page" => {
                    let url = args["url"].as_str().unwrap_or("");
                    format!("Successfully navigated to {url}.\n## Pages\n1: Page ({url}) [selected]")
                }
                "list_pages" => "## Pages\n1: Shop (https://shop.example.com/p/1) [selected]".to_owned(),
                // A link to another site.
                "click" if args["uid"] == "7" => {
                    "Successfully navigated to https://other.example.org/.\n## Pages\n1: Other (https://other.example.org/) [selected]".to_owned()
                }
                _ => "done".to_owned(),
            };
            BrowserOutput {
                text,
                ..BrowserOutput::default()
            }
        })
    }

    /// What the stand-in hub saw: (tool, quiet) for each permission, and
    /// the sealed work asked for.
    #[derive(Default)]
    struct Seen {
        asked: Vec<(String, bool)>,
        sealed: Vec<SealedWork>,
        uses: Vec<String>,
    }

    /// A stand-in hub: visits run except to `blocked.example.com`; a click
    /// on uid 9 would need the person's OK; the sealed tab answers with the
    /// page's address (and, following, a next page up to page 3).
    fn hub(
        mut rx: mpsc::Receiver<EngineEvent>,
        seen: Arc<Mutex<Seen>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Some(e) = rx.recv().await {
                match e {
                    EngineEvent::Permission(r) => {
                        seen.lock().unwrap().asked.push((r.tool.clone(), r.quiet));
                        let url = r.input["url"].as_str().unwrap_or("");
                        r.reply.send(if url.contains("blocked.example.com") {
                            Decision::Deny("not that site".into())
                        } else if r.input["uid"] == "9" {
                            Decision::WouldAsk(
                                "click button \"Add to cart\" on shop.example.com".into(),
                            )
                        } else {
                            Decision::Allow
                        });
                    }
                    EngineEvent::Sealed(c) => {
                        seen.lock().unwrap().sealed.push(c.work.clone());
                        let reply = match &c.work {
                            SealedWork::Read {
                                url,
                                script,
                                follow,
                            } if script.contains("throw") => {
                                let _ = (url, follow);
                                SealedReply::failed("the script failed: boom")
                            }
                            SealedWork::Read { url, follow, .. } => {
                                let n: u32 = url
                                    .rsplit('=')
                                    .next()
                                    .and_then(|n| n.parse().ok())
                                    .unwrap_or(1);
                                SealedReply {
                                    ok: true,
                                    value: json!([{"page": url}]),
                                    next: follow.as_ref().filter(|_| n < 3).map(|_| {
                                        format!("https://shop.example.com/list?page={}", n + 1)
                                    }),
                                    ..SealedReply::default()
                                }
                            }
                            SealedWork::Done => SealedReply {
                                ok: true,
                                ..SealedReply::default()
                            },
                        };
                        c.reply.send(reply);
                    }
                    EngineEvent::Assistant { tool_uses, .. } => {
                        seen.lock()
                            .unwrap()
                            .uses
                            .extend(tool_uses.into_iter().map(|t| t.name));
                    }
                    _ => {}
                }
            }
        })
    }

    struct Run {
        _tmp: tempfile::TempDir,
        work: PathBuf,
        results: ResultStore,
        pages: Mutex<PageTracker>,
        cancel: CancellationToken,
        browser: Arc<dyn BrowserTools>,
        scripted: Arc<ScriptedBrowser>,
    }

    impl Run {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let work = tmp.path().join("work");
            let results = ResultStore::new(
                Arc::new(MemoryLoopStore::new()),
                "scout",
                "s1",
                &tmp.path().join("context"),
            );
            let scripted = browser();
            Self {
                _tmp: tmp,
                work,
                results,
                pages: Mutex::new(PageTracker::default()),
                cancel: CancellationToken::new(),
                browser: Arc::clone(&scripted) as Arc<dyn BrowserTools>,
                scripted,
            }
        }

        async fn call(&self, call: &ToolCall) -> (ToolOutcome, Seen) {
            let (tx, rx) = mpsc::channel(64);
            let seen = Arc::new(Mutex::new(Seen::default()));
            let task = hub(rx, Arc::clone(&seen));
            let gate = GateCheck::new(tx.clone());
            let env = ToolEnv {
                work_dir: &self.work,
                events: &tx,
                browser: Some(&self.browser),
                cancel: &self.cancel,
                results: &self.results,
                pages: &self.pages,
                strictness: Arc::new(|_: &str| 0),
                budget: TokenBudget::for_model(64_000, Some(4_096)),
                turn: 0,
                step: 1,
                visible: &|_: &str| false,
            };
            let out = if call.name == read_pages::READ_PAGES {
                read_pages(call, &gate, &env).await
            } else {
                page_steps(call, &gate, &env).await
            };
            drop(env);
            drop((gate, tx));
            task.await.unwrap();
            let seen = std::mem::take(&mut *seen.lock().unwrap());
            (out, seen)
        }
    }

    fn text(out: &ToolOutcome) -> &str {
        let Part::Text(t) = &out.for_model[0] else {
            panic!()
        };
        t
    }

    #[tokio::test]
    async fn three_pages_in_one_call_each_a_visit_and_a_sealed_read() {
        let r = Run::new();
        let call = ToolCall::new(
            "c1",
            read_pages::READ_PAGES,
            json!({"urls": ["https://shop.example.com/p?n=1", "https://blocked.example.com/", "https://shop.example.com/p?n=3"], "script": "return {}"}),
        );
        let (out, seen) = r.call(&call).await;
        let t = text(&out);
        assert!(
            t.contains("Read 2 of 3 pages with your script: 2 rows."),
            "{t}"
        );
        assert!(t.contains(r#"{"page":"https://shop.example.com/p?n=1","source":"https://shop.example.com/p?n=1"}"#), "{t}");
        assert!(
            t.contains(
                "- page 2 (https://blocked.example.com/): oxbot didn't open it: not that site"
            ),
            "{t}"
        );
        assert!(t.starts_with("<web_page"), "outside text, wrapped: {t}");
        assert!(out.untrusted);
        // Each visit was checked quietly as a navigation, then shown to the hub.
        let navs: Vec<_> = seen
            .asked
            .iter()
            .filter(|(t, _)| t.ends_with("navigate_page"))
            .collect();
        assert_eq!(navs.len(), 3);
        assert!(navs.iter().all(|(_, quiet)| *quiet));
        assert_eq!(
            seen.uses
                .iter()
                .filter(|u| u.ends_with("navigate_page"))
                .count(),
            2
        );
        // Two reads and the sealed tab closed.
        assert_eq!(seen.sealed.len(), 3);
        assert_eq!(seen.sealed[2], SealedWork::Done);
        // The live page was only navigated: nothing was clicked or filled.
        assert!(
            r.scripted
                .calls()
                .iter()
                .all(|(n, _)| n == "navigate_page" || n == "list_pages")
        );
    }

    #[tokio::test]
    async fn following_next_stops_at_the_last_page_or_max_pages() {
        let r = Run::new();
        r.pages
            .lock()
            .unwrap()
            .learn("## Pages\n1: List (https://shop.example.com/list?page=1) [selected]");
        let call = ToolCall::new(
            "c1",
            read_pages::READ_PAGES,
            json!({"follow": "Next", "max_pages": 5, "script": "return 1"}),
        );
        let (out, seen) = r.call(&call).await;
        let t = text(&out);
        assert!(t.contains("Read 3 of 3 pages"), "{t}");
        assert!(!t.contains("Stopped early"), "{t}");
        // The first page is the one it's on: no visit for it.
        assert_eq!(
            seen.asked
                .iter()
                .filter(|(t, _)| t.ends_with("navigate_page"))
                .count(),
            2
        );
        let r = Run::new();
        r.pages
            .lock()
            .unwrap()
            .learn("## Pages\n1: List (https://shop.example.com/list?page=1) [selected]");
        let call = ToolCall::new(
            "c1",
            read_pages::READ_PAGES,
            json!({"follow": "Next", "max_pages": 2, "script": "return 1"}),
        );
        let (out, _) = r.call(&call).await;
        assert!(
            text(&out).contains("Stopped early: max_pages (2) was reached"),
            "{}",
            text(&out)
        );
    }

    #[tokio::test]
    async fn a_failing_script_is_a_line_not_the_end() {
        let r = Run::new();
        let call = ToolCall::new(
            "c1",
            read_pages::READ_PAGES,
            json!({"urls": ["https://shop.example.com/a"], "script": "throw 1"}),
        );
        let (out, _) = r.call(&call).await;
        assert!(
            text(&out).contains("- page 1 (https://shop.example.com/a): the script failed: boom"),
            "{}",
            text(&out)
        );
        let bad = ToolCall::new("c2", read_pages::READ_PAGES, json!({"script": "return 1"}));
        let (out, seen) = r.call(&bad).await;
        assert!(out.error);
        assert!(
            seen.asked.is_empty() && seen.sealed.is_empty(),
            "nothing ran"
        );
    }

    #[tokio::test]
    async fn steps_run_in_a_row_and_stop_where_the_person_must_say_yes() {
        let r = Run::new();
        let call = ToolCall::new(
            "c1",
            steps::PAGE_STEPS,
            json!({"steps": [
                {"action": "fill", "uid": "4", "value": "2"},
                {"action": "check", "uid": "5"},
                {"action": "click", "uid": "9"},
                {"action": "click", "uid": "10"}
            ]}),
        );
        let (out, seen) = r.call(&call).await;
        let t = text(&out);
        assert!(
            t.starts_with("Ran 2 of 4 steps on shop.example.com:"),
            "{t}"
        );
        assert!(t.contains("Stopped at step 3 (click 9): it needs the person's OK (click button \"Add to cart\" on shop.example.com)"), "{t}");
        assert!(out.untrusted);
        // Every step asked the gate quietly; the one that would ask didn't run.
        assert!(seen.asked.iter().all(|(_, quiet)| *quiet));
        let ran: Vec<(String, Value)> = r.scripted.calls();
        let names: Vec<&str> = ran.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["list_pages", "fill", "fill"]);
        assert_eq!(ran[1].1, json!({"uid": "4", "value": "2", "pageId": 1}));
        assert!(!ran.iter().any(|(n, a)| n == "click" && a["uid"] == "9"));
    }

    #[tokio::test]
    async fn a_step_that_leaves_the_site_ends_the_batch() {
        let r = Run::new();
        r.pages
            .lock()
            .unwrap()
            .learn("## Pages\n1: Shop (https://shop.example.com/p/1) [selected]");
        let call = ToolCall::new(
            "c1",
            steps::PAGE_STEPS,
            json!({"steps": [{"action": "click", "uid": "7"}, {"action": "fill", "uid": "4", "value": "x"}]}),
        );
        let (out, _) = r.call(&call).await;
        let t = text(&out);
        assert!(
            t.contains("Stopped after step 1 (click 7): the page is now on other.example.org"),
            "{t}"
        );
        assert_eq!(r.scripted.calls().len(), 1, "the fill never ran");
    }
}
