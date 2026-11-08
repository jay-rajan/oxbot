//! `page_steps`: several fills, picks, ticks and clicks on the page the
//! bot is on, in one call (issue #23). Each step is the browser tool call
//! it stands for, checked by oxbot's gate as usual, but quietly: the run
//! stops at the first step that would need the person's OK (or that leaves
//! the site), and says what ran and where it stopped, so the bot calls that
//! step on its own and the person is asked the usual way.
//!
//! On any website except those the owner set to fine-grained control
//! (Settings → Websites); the hub's gate refuses the call there. This module is the pure part:
//! the tool as the model sees it, reading the steps, the browser call each
//! one is, and the report.

use std::fmt::Write as _;

use oxbot_llm::ToolSpec;
use serde_json::{Value, json};

/// The name models call `page_steps` by.
pub const PAGE_STEPS: &str = "mcp__oxbot__page_steps";
/// The most steps in one call.
pub const MAX_STEPS: usize = 15;
/// The longest wait a `wait_for` step may ask for.
const MAX_WAIT_MS: u64 = 10_000;

/// `page_steps` as the model sees it.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: PAGE_STEPS.to_owned(),
        description: "Do several steps on the page you're on in one go: fill in fields, pick \
            options, tick boxes, click, press keys, scroll to an element and wait for text, up \
            to 15 in a row, with uids from your latest snapshot. Not on websites the person set \
            to fine-grained control in Settings → Websites: there, do the steps one at a time. \
            oxbot checks each step as usual and stops at the first one that needs the person's \
            OK (Add to cart, Submit, Post, Sign in…) or that leaves the site, and tells you what \
            ran and where it stopped: call that step on its own so the person is asked. Take a \
            snapshot afterwards to see the page. Example: steps: [{\"action\": \"fill\", \"uid\": \
            \"4_2\", \"value\": \"Toronto\"}, {\"action\": \"check\", \"uid\": \"4_5\"}, \
            {\"action\": \"click\", \"uid\": \"4_9\"}]"
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "description": "The steps, in order (at most 15).",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {
                                "type": "string",
                                "enum": ["fill", "select", "check", "click", "press", "scroll", "wait_for"],
                                "description": "fill: type value into a field; select: pick the option value in a list; check: tick (checked: false to untick) a box; click; press: a key such as Enter or Tab; scroll: bring an element into view; wait_for: wait until text shows."
                            },
                            "uid": {"type": "string", "description": "The element's uid from your latest snapshot (fill, select, check, click, scroll)."},
                            "value": {"type": "string", "description": "What to fill in, or the option to pick."},
                            "checked": {"type": "boolean", "description": "check: true to tick (the default), false to untick."},
                            "key": {"type": "string", "description": "press: the key, e.g. Enter, Tab, ArrowDown."},
                            "text": {"type": "string", "description": "wait_for: the text to wait for."}
                        },
                        "required": ["action"]
                    }
                }
            },
            "required": ["steps"]
        }),
    }
}

/// One step, read and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Type `value` into `uid`.
    Fill {
        /// The field.
        uid: String,
        /// What to type.
        value: String,
    },
    /// Pick option `value` in the list `uid`.
    Select {
        /// The list.
        uid: String,
        /// The option.
        value: String,
    },
    /// Tick (or untick) the box `uid`.
    Check {
        /// The box.
        uid: String,
        /// Ticked after.
        checked: bool,
    },
    /// Click `uid`.
    Click {
        /// The element.
        uid: String,
    },
    /// Press `key`.
    Press {
        /// The key.
        key: String,
    },
    /// Bring `uid` into view.
    Scroll {
        /// The element.
        uid: String,
    },
    /// Wait until `text` shows.
    WaitFor {
        /// The text.
        text: String,
    },
}

impl Step {
    /// The browser tool this step runs as (its own name, e.g. `click`) and
    /// its arguments, on page `page_id` if known.
    #[must_use]
    pub fn browser_call(&self, page_id: Option<u32>) -> (&'static str, Value) {
        let (tool, mut args) = match self {
            Self::Fill { uid, value } | Self::Select { uid, value } => {
                ("fill", json!({"uid": uid, "value": value}))
            }
            Self::Check { uid, checked } => (
                "fill",
                json!({"uid": uid, "value": if *checked { "true" } else { "false" }}),
            ),
            Self::Click { uid } => ("click", json!({"uid": uid})),
            Self::Press { key } => ("press_key", json!({"key": key})),
            Self::Scroll { uid } => ("hover", json!({"uid": uid})),
            Self::WaitFor { text } => ("wait_for", json!({"text": [text], "timeout": MAX_WAIT_MS})),
        };
        if let (Some(id), Some(map)) = (page_id, args.as_object_mut()) {
            map.insert("pageId".to_owned(), json!(id));
        }
        (tool, args)
    }

    /// A few words for the report: `click 4_9`, `fill 4_2 with "Toronto"`.
    #[must_use]
    pub fn describe(&self) -> String {
        let short = |s: &str| {
            let s: String = s.chars().take(40).collect();
            s.replace('"', "'")
        };
        match self {
            Self::Fill { uid, value } => format!("fill {uid} with \"{}\"", short(value)),
            Self::Select { uid, value } => format!("pick \"{}\" in {uid}", short(value)),
            Self::Check { uid, checked: true } => format!("tick {uid}"),
            Self::Check {
                uid,
                checked: false,
            } => format!("untick {uid}"),
            Self::Click { uid } => format!("click {uid}"),
            Self::Press { key } => format!("press {}", short(key)),
            Self::Scroll { uid } => format!("scroll to {uid}"),
            Self::WaitFor { text } => format!("wait for \"{}\"", short(text)),
        }
    }
}

/// Reads `page_steps`' arguments.
///
/// # Errors
/// What's wrong with them, for the model.
pub fn parse(args: &Value) -> Result<Vec<Step>, String> {
    let list = args
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("steps must be a list of steps")?;
    if list.is_empty() {
        return Err("steps is empty: give at least one step".to_owned());
    }
    if list.len() > MAX_STEPS {
        return Err(format!(
            "{} steps is too many: at most {MAX_STEPS} in one call",
            list.len()
        ));
    }
    list.iter()
        .enumerate()
        .map(|(i, s)| parse_step(s).map_err(|why| format!("step {}: {why}", i + 1)))
        .collect()
}

fn parse_step(s: &Value) -> Result<Step, String> {
    let text = |key: &str| {
        s.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    let uid = || {
        text("uid")
            .map(|u| u.trim_start_matches("uid=").to_owned())
            .ok_or_else(|| "it needs the element's uid".to_owned())
    };
    // A value may be empty (clearing a field), but must be there.
    let value = || {
        s.get("value")
            .and_then(|v| match v {
                Value::String(t) => Some(t.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .ok_or_else(|| "it needs a value".to_owned())
    };
    match text("action").as_deref() {
        Some("fill") => Ok(Step::Fill {
            uid: uid()?,
            value: value()?,
        }),
        Some("select") => Ok(Step::Select {
            uid: uid()?,
            value: value()?,
        }),
        Some("check") => Ok(Step::Check {
            uid: uid()?,
            checked: s.get("checked").and_then(Value::as_bool).unwrap_or(true),
        }),
        Some("click") => Ok(Step::Click { uid: uid()? }),
        Some("press") => Ok(Step::Press {
            key: text("key").ok_or("it needs the key to press")?,
        }),
        Some("scroll") => Ok(Step::Scroll { uid: uid()? }),
        Some("wait_for") => Ok(Step::WaitFor {
            text: text("text").ok_or("it needs the text to wait for")?,
        }),
        Some(other) => Err(format!(
            "\"{other}\" isn't a step; use fill, select, check, click, press, scroll or wait_for"
        )),
        None => Err("it needs an action".to_owned()),
    }
}

/// Why a run stopped before its last step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stop {
    /// This step needs the person's OK (what would be asked).
    NeedsOk(String),
    /// oxbot refused this step (why).
    Refused(String),
    /// This step failed in the browser (its error).
    Failed(String),
    /// After this step the tab showed another site (its host).
    LeftSite(String),
    /// The person stopped the task.
    Stopped,
}

/// The report the model gets: what ran, and where and why it stopped.
/// `ran` steps ran (the stopping one included when it ran and then left
/// the site); `stop` is the step it stopped at, with why.
#[must_use]
pub fn report(steps: &[Step], site: &str, ran: usize, stop: Option<(usize, &Stop)>) -> String {
    let mut out = String::new();
    let on = if site.is_empty() {
        String::new()
    } else {
        format!(" on {site}")
    };
    if ran == steps.len() {
        let _ = writeln!(out, "Ran all {} steps{on}:", steps.len());
    } else {
        let _ = writeln!(out, "Ran {ran} of {} steps{on}:", steps.len());
    }
    for (i, step) in steps.iter().enumerate().take(ran) {
        let _ = writeln!(out, "{}. {} — done", i + 1, step.describe());
    }
    if let Some((at, why)) = stop {
        let n = at + 1;
        let what = steps.get(at).map(Step::describe).unwrap_or_default();
        let rest = steps.len().saturating_sub(n);
        let not_run = match rest {
            0 => String::new(),
            1 => " The step after it didn't run.".to_owned(),
            k => format!(" The {k} steps after it didn't run."),
        };
        match why {
            Stop::NeedsOk(asks) => {
                let (tool, _) = steps
                    .get(at)
                    .map_or(("click", Value::Null), |s| s.browser_call(None));
                let _ = write!(
                    out,
                    "Stopped at step {n} ({what}): it needs the person's OK ({asks}), so it \
                     didn't run. Call mcp__chrome-devtools__{tool} for it on its own, and the \
                     person is asked.{not_run}"
                );
            }
            Stop::Refused(why) => {
                let _ = write!(
                    out,
                    "Stopped at step {n} ({what}): oxbot said no: {why}{not_run}"
                );
            }
            Stop::Failed(err) => {
                let _ = write!(
                    out,
                    "Stopped at step {n} ({what}): it failed: {err}{not_run}"
                );
            }
            Stop::LeftSite(host) => {
                let _ = write!(
                    out,
                    "Stopped after step {n} ({what}): the page is now on {host}, another site, so \
                     the rest is one step at a time there.{not_run}"
                );
            }
            Stop::Stopped => {
                let _ = write!(out, "Stopped at step {n}: the task was stopped.");
            }
        }
        out.push('\n');
    }
    out.push_str("Take a snapshot to see the page now.");
    out
}

/// The host of a web address, lowercase (`None` for anything else).
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_read_and_checked() {
        let steps = parse(&json!({"steps": [
            {"action": "fill", "uid": "uid=4_2", "value": "Toronto"},
            {"action": "select", "uid": "4_3", "value": 2},
            {"action": "check", "uid": "4_5"},
            {"action": "check", "uid": "4_6", "checked": false},
            {"action": "click", "uid": "4_9"},
            {"action": "press", "key": "Tab"},
            {"action": "scroll", "uid": "4_12"},
            {"action": "wait_for", "text": "Results"}
        ]}))
        .unwrap();
        assert_eq!(steps.len(), 8);
        assert_eq!(
            steps[0],
            Step::Fill {
                uid: "4_2".into(),
                value: "Toronto".into()
            }
        );
        assert_eq!(
            steps[1],
            Step::Select {
                uid: "4_3".into(),
                value: "2".into()
            }
        );
        assert_eq!(
            steps[3],
            Step::Check {
                uid: "4_6".into(),
                checked: false
            }
        );
    }

    #[test]
    fn bad_steps_say_which_and_why() {
        assert!(parse(&json!({})).unwrap_err().contains("list"));
        assert!(parse(&json!({"steps": []})).unwrap_err().contains("empty"));
        let many: Vec<Value> = (0..16)
            .map(|_| json!({"action": "press", "key": "Tab"}))
            .collect();
        assert!(
            parse(&json!({"steps": many}))
                .unwrap_err()
                .contains("at most 15")
        );
        assert_eq!(
            parse(&json!({"steps": [{"action": "click"}]})).unwrap_err(),
            "step 1: it needs the element's uid"
        );
        assert_eq!(
            parse(&json!({"steps": [{"action": "press", "key": "Tab"}, {"action": "fill", "uid": "1_2"}]}))
                .unwrap_err(),
            "step 2: it needs a value"
        );
        assert!(
            parse(&json!({"steps": [{"action": "evaluate", "uid": "1"}]}))
                .unwrap_err()
                .contains("isn't a step")
        );
    }

    #[test]
    fn each_step_is_the_browser_call_it_stands_for() {
        let check = Step::Check {
            uid: "4_5".into(),
            checked: true,
        };
        assert_eq!(
            check.browser_call(Some(2)),
            ("fill", json!({"uid": "4_5", "value": "true", "pageId": 2}))
        );
        assert_eq!(
            Step::Scroll { uid: "1".into() }.browser_call(None),
            ("hover", json!({"uid": "1"}))
        );
        assert_eq!(
            Step::Press {
                key: "Enter".into()
            }
            .browser_call(None)
            .0,
            "press_key"
        );
        assert_eq!(
            Step::WaitFor {
                text: "Done".into()
            }
            .browser_call(None),
            ("wait_for", json!({"text": ["Done"], "timeout": 10_000}))
        );
    }

    #[test]
    fn the_report_says_what_ran_and_where_it_stopped() {
        let steps = parse(&json!({"steps": [
            {"action": "fill", "uid": "4_2", "value": "2"},
            {"action": "click", "uid": "4_9"},
            {"action": "click", "uid": "4_10"}
        ]}))
        .unwrap();
        let text = report(
            &steps,
            "shop.example.com",
            1,
            Some((
                1,
                &Stop::NeedsOk("click button \"Add to cart\" on shop.example.com".into()),
            )),
        );
        assert!(
            text.starts_with(
                "Ran 1 of 3 steps on shop.example.com:\n1. fill 4_2 with \"2\" — done\n"
            ),
            "{text}"
        );
        assert!(text.contains("Stopped at step 2 (click 4_9): it needs the person's OK (click button \"Add to cart\" on shop.example.com)"), "{text}");
        assert!(
            text.contains("Call mcp__chrome-devtools__click for it on its own"),
            "{text}"
        );
        assert!(text.contains("The step after it didn't run."), "{text}");
        let all = report(&steps, "", 3, None);
        assert!(all.starts_with("Ran all 3 steps:\n"), "{all}");
        let left = report(
            &steps,
            "a.example",
            2,
            Some((1, &Stop::LeftSite("b.example".into()))),
        );
        assert!(left.contains("the page is now on b.example"), "{left}");
    }

    #[test]
    fn hosts_are_read_from_web_addresses_only() {
        assert_eq!(
            host_of("https://Shop.Example.com:8443/p?q=1").as_deref(),
            Some("shop.example.com")
        );
        assert_eq!(host_of("http://u@a.example/").as_deref(), Some("a.example"));
        assert_eq!(host_of("about:blank"), None);
        assert_eq!(host_of("https:///x"), None);
    }
}
