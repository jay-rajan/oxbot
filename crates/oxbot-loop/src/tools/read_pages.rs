//! `read_pages`: read many pages with one small script, in one model turn.
//! The bot gives addresses (or "follow the Next link") and a JavaScript
//! extractor; oxbot opens each page in the bot's own browser (through the
//! gate, as a visit), copies it, and runs the extractor on the copy in a
//! sealed tab with no network (the hub does that part, see
//! `oxbot_browser::sealed`). The rows come back as one table, each with the
//! page it came from.
//!
//! This module is the pure part: the tool as the model sees it, reading
//! its arguments into a plan, and putting the pages' rows together.

use std::fmt::Write as _;

use oxbot_llm::ToolSpec;
use serde_json::{Map, Value, json};

/// The name models call `read_pages` by.
pub const READ_PAGES: &str = "mcp__oxbot__read_pages";
/// The most pages one call reads.
pub const MAX_PAGES: usize = 20;
/// The longest script oxbot takes.
pub const MAX_SCRIPT: usize = 20_000;
/// The most of the table kept whole (characters); the model is shown less
/// (see the tools' inline cap) and can read the rest back.
pub const MAX_TABLE: usize = 400_000;
/// The field each row gets: the page it came from.
pub const SOURCE: &str = "source";

/// `read_pages` as the model sees it.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: READ_PAGES.to_owned(),
        description: "Read many pages in one go with a small script, instead of opening and \
            reading them one at a time. Use it for lists and tables, search results, several \
            product pages, following \"Next\" through pages of results, or comparing products. \
            oxbot opens each page in the browser (only pages bots may visit; nothing is clicked \
            or sent), copies it, and runs your script on the copy in a sealed tab with no \
            network, so the script can only read. Give urls (up to 20 addresses), or follow (a \
            CSS selector or the words of the link to the next page) with max_pages (up to 20) \
            to start from the page you're on. script is the body of a JavaScript function that \
            gets doc (the page, a parsed Document) and url, and returns rows: an array of \
            objects, or one object. You get one table back, each row with the page it came \
            from, and which pages failed. Example:\n\
            urls: [\"https://shop.example.com/p/1\", \"https://shop.example.com/p/2\"]\n\
            script: const t = s => doc.querySelector(s)?.textContent.trim();\n\
            return { name: t('h1'), price: t('.price'), rating: t('.rating') };"
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "urls": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "The pages to read, in order (at most 20)."
                },
                "follow": {
                    "type": "string",
                    "description": "Instead of urls: a CSS selector or the words of the link to the next page (e.g. \"Next\"), followed from the page you're on."
                },
                "max_pages": {
                    "type": "integer",
                    "description": "With follow: the most pages to read, this one included (at most 20; default 5)."
                },
                "script": {
                    "type": "string",
                    "description": "The body of a JavaScript function of (doc, url) that returns rows (JSON). It may use await."
                }
            },
            "required": ["script"]
        }),
    }
}

/// Which pages a call reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pages {
    /// These addresses, in order.
    Urls(Vec<String>),
    /// From the page the browser is on, following this link up to `max`
    /// pages in all.
    Follow {
        /// A selector or the link's words.
        link: String,
        /// The most pages, the first included.
        max: usize,
    },
}

/// A call, read and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Which pages.
    pub pages: Pages,
    /// The extractor.
    pub script: String,
}

impl Plan {
    /// The most pages it can read.
    #[must_use]
    pub fn max_pages(&self) -> usize {
        match &self.pages {
            Pages::Urls(urls) => urls.len(),
            Pages::Follow { max, .. } => *max,
        }
    }

    /// The link to follow, if following.
    #[must_use]
    pub fn follow(&self) -> Option<&str> {
        match &self.pages {
            Pages::Follow { link, .. } => Some(link),
            Pages::Urls(_) => None,
        }
    }
}

/// Reads `read_pages`' arguments.
///
/// # Errors
/// What's wrong with them, for the model.
pub fn plan(args: &Value) -> Result<Plan, String> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("script is needed: the body of a function of (doc, url) that returns rows")?;
    if script.chars().count() > MAX_SCRIPT {
        return Err(format!(
            "the script is too long: at most {MAX_SCRIPT} characters"
        ));
    }
    let urls: Vec<String> = match args.get("urls") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(list)) => list
            .iter()
            .map(|u| {
                u.as_str()
                    .map(|u| u.trim().to_owned())
                    .filter(|u| !u.is_empty())
                    .ok_or_else(|| "urls must be a list of addresses".to_owned())
            })
            .collect::<Result<_, _>>()?,
        Some(Value::String(one)) if !one.trim().is_empty() => vec![one.trim().to_owned()],
        Some(_) => return Err("urls must be a list of addresses".to_owned()),
    };
    let follow = args
        .get("follow")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|f| !f.is_empty());
    let pages = match (urls.is_empty(), follow) {
        (false, Some(_)) => return Err("give urls or follow, not both".to_owned()),
        (true, None) => {
            return Err(
                "give urls (the pages to read) or follow (the link to the next page)".to_owned(),
            );
        }
        (false, None) => {
            if urls.len() > MAX_PAGES {
                return Err(format!(
                    "{} pages is too many: at most {MAX_PAGES} in one call",
                    urls.len()
                ));
            }
            if let Some(bad) = urls
                .iter()
                .find(|u| !(u.starts_with("https://") || u.starts_with("http://")))
            {
                return Err(format!("{bad} isn't a web address (http or https)"));
            }
            Pages::Urls(urls)
        }
        (true, Some(link)) => {
            let max = args
                .get("max_pages")
                .and_then(Value::as_u64)
                .map_or(5, |m| usize::try_from(m).unwrap_or(MAX_PAGES));
            if max == 0 || max > MAX_PAGES {
                return Err(format!("max_pages must be from 1 to {MAX_PAGES}"));
            }
            Pages::Follow {
                link: link.to_owned(),
                max,
            }
        }
    };
    Ok(Plan {
        pages,
        script: script.to_owned(),
    })
}

/// What one page came to.
#[derive(Debug, Clone, PartialEq)]
pub struct PageRead {
    /// Its address (as the browser showed it).
    pub url: String,
    /// What the script returned, or why there's nothing.
    pub outcome: Result<Value, String>,
    /// The page was too big to copy whole.
    pub cut: bool,
}

/// The rows of one page's result: an array's items, or the one value;
/// each an object with its page as [`SOURCE`] (fields sorted by name).
fn rows_of(url: &str, value: &Value) -> Vec<Value> {
    let items: Vec<&Value> = match value {
        Value::Array(list) => list.iter().collect(),
        Value::Null => Vec::new(),
        one => vec![one],
    };
    items
        .into_iter()
        .map(|item| {
            let mut row = Map::new();
            row.insert(SOURCE.to_owned(), json!(url));
            match item {
                Value::Object(fields) => {
                    for (k, v) in fields {
                        if k != SOURCE {
                            row.insert(k.clone(), v.clone());
                        }
                    }
                }
                other => {
                    row.insert("value".to_owned(), other.clone());
                }
            }
            Value::Object(row)
        })
        .collect()
}

/// The table for the model: one JSON row per line (each with its page),
/// then the pages that failed and why the run stopped early, if it did.
/// Kept to [`MAX_TABLE`] characters, with a note when rows were left out.
/// Returns the text and how many rows it has.
#[must_use]
pub fn table(reads: &[PageRead], stopped: Option<&str>) -> (String, usize) {
    let mut rows = Vec::new();
    for read in reads {
        if let Ok(value) = &read.outcome {
            rows.extend(rows_of(&read.url, value));
        }
    }
    let ok = reads.iter().filter(|r| r.outcome.is_ok()).count();
    let mut body = String::new();
    let mut shown = 0;
    for row in &rows {
        let line = row.to_string();
        if body.len() + line.len() + 1 > MAX_TABLE {
            break;
        }
        body.push_str(&line);
        body.push('\n');
        shown += 1;
    }
    let mut out = format!(
        "Read {ok} of {} page{} with your script: {} row{}.\n",
        reads.len(),
        if reads.len() == 1 { "" } else { "s" },
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
    );
    out.push_str(&body);
    if shown < rows.len() {
        let _ = writeln!(
            out,
            "[cut: the table is too big, so only the first {shown} of {} rows are here; return \
             fewer fields or read fewer pages]",
            rows.len()
        );
    }
    let failed: Vec<String> = reads
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            r.outcome
                .as_ref()
                .err()
                .map(|why| format!("- page {} ({}): {why}", i + 1, r.url))
        })
        .collect();
    if !failed.is_empty() {
        out.push_str("Pages that failed:\n");
        for f in failed {
            out.push_str(&f);
            out.push('\n');
        }
    }
    let cut: Vec<&str> = reads
        .iter()
        .filter(|r| r.cut)
        .map(|r| r.url.as_str())
        .collect();
    if !cut.is_empty() {
        let _ = writeln!(
            out,
            "Only the start of these pages was read (they're very big): {}",
            cut.join(", ")
        );
    }
    if let Some(why) = stopped {
        let _ = writeln!(out, "Stopped early: {why}");
    }
    (out.trim_end().to_owned(), rows.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plan_is_urls_or_follow() {
        let p = plan(&json!({"urls": ["https://a.example/1", " https://a.example/2 "], "script": " return 1 "}))
            .unwrap();
        assert_eq!(
            p.pages,
            Pages::Urls(vec![
                "https://a.example/1".into(),
                "https://a.example/2".into()
            ])
        );
        assert_eq!(p.script, "return 1");
        assert_eq!(p.max_pages(), 2);
        assert_eq!(p.follow(), None);
        let f = plan(&json!({"follow": "Next", "max_pages": 3, "script": "return 1"})).unwrap();
        assert_eq!(
            f.pages,
            Pages::Follow {
                link: "Next".into(),
                max: 3
            }
        );
        assert_eq!(f.follow(), Some("Next"));
        assert_eq!(
            plan(&json!({"follow": "a.next", "script": "x"}))
                .unwrap()
                .max_pages(),
            5
        );
    }

    #[test]
    fn bad_plans_say_why() {
        let e = |args: Value| plan(&args).unwrap_err();
        assert!(e(json!({"urls": ["https://a.example/"]})).contains("script is needed"));
        assert!(e(json!({"script": "x"})).contains("give urls"));
        assert!(
            e(json!({"urls": ["https://a.example/"], "follow": "Next", "script": "x"}))
                .contains("not both")
        );
        let many: Vec<String> = (0..21).map(|i| format!("https://a.example/{i}")).collect();
        assert!(e(json!({"urls": many, "script": "x"})).contains("at most 20"));
        assert!(
            e(json!({"urls": ["file:///etc/passwd"], "script": "x"}))
                .contains("isn't a web address")
        );
        assert!(e(json!({"follow": "Next", "max_pages": 21, "script": "x"})).contains("1 to 20"));
        assert!(e(json!({"follow": "Next", "max_pages": 0, "script": "x"})).contains("1 to 20"));
        assert!(e(json!({"urls": [1], "script": "x"})).contains("list of addresses"));
        assert!(
            e(json!({"urls": ["https://a.example/"], "script": "x".repeat(20_001)}))
                .contains("too long")
        );
    }

    #[test]
    fn rows_come_back_as_one_table_with_their_page() {
        let reads = vec![
            PageRead {
                url: "https://a.example/1".into(),
                outcome: Ok(json!([{"name": "Lamp", "price": "$10"}, "loose"])),
                cut: false,
            },
            PageRead {
                url: "https://a.example/2".into(),
                outcome: Err("the script failed: x is not defined".into()),
                cut: false,
            },
            PageRead {
                url: "https://a.example/3".into(),
                outcome: Ok(json!({"name": "Desk", "source": "spoofed"})),
                cut: true,
            },
        ];
        let (text, n) = table(&reads, Some("page 4 would need the person's OK"));
        assert_eq!(n, 3);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Read 2 of 3 pages with your script: 3 rows.");
        assert_eq!(
            lines[1],
            r#"{"name":"Lamp","price":"$10","source":"https://a.example/1"}"#
        );
        assert_eq!(
            lines[2],
            r#"{"source":"https://a.example/1","value":"loose"}"#
        );
        // A row can't claim another page.
        assert_eq!(
            lines[3],
            r#"{"name":"Desk","source":"https://a.example/3"}"#
        );
        assert!(
            text.contains("- page 2 (https://a.example/2): the script failed"),
            "{text}"
        );
        assert!(text.contains(
            "Only the start of these pages was read (they're very big): https://a.example/3"
        ));
        assert!(text.ends_with("Stopped early: page 4 would need the person's OK"));
    }

    #[test]
    fn a_huge_table_is_cut_with_a_note() {
        let big: Vec<Value> = (0..20_000)
            .map(|i| json!({"n": i, "pad": "x".repeat(40)}))
            .collect();
        let reads = vec![PageRead {
            url: "https://a.example/".into(),
            outcome: Ok(Value::Array(big)),
            cut: false,
        }];
        let (text, n) = table(&reads, None);
        assert_eq!(n, 20_000);
        assert!(text.len() < MAX_TABLE + 1_000);
        assert!(
            text.contains("[cut: the table is too big, so only the first"),
            "{}",
            &text[text.len() - 200..]
        );
    }
}
