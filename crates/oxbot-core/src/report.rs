//! Research reports: what a bot saves with `save_report`, the checks on
//! it, and the safe HTML page oxbot makes from it.
//!
//! The bot writes Markdown and lists its sources; every source must be a
//! page it opened in the browser in this conversation (see [`check`]).
//! oxbot keeps the Markdown and renders it on every open ([`render`]):
//!
//! - no HTML from the bot or a page ever reaches the output: raw HTML is
//!   shown as text;
//! - no images (they would make the viewer fetch from another host);
//! - a link stays a link only if it's an `http(s)` page among the report's
//!   sources or the pages the bot opened; anything else (`javascript:`,
//!   `data:`, the home network, a page it never opened) is plain text;
//! - `[n]` becomes a link to source `n` in the list at the end.
//!
//! The page also carries a strict Content-Security-Policy, and the app
//! shows it in a sandboxed frame with scripts off.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use oxbot_browser::{normalize_web_url, site_of, web_host};
use pulldown_cmark::{
    CowStr, Event, Options, Parser, Tag, TagEnd, TextMergeStream, html::push_html,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::PersonId;

/// The longest title, in characters.
pub const MAX_TITLE: usize = 120;
/// The longest one-line summary, in characters.
pub const MAX_SUMMARY: usize = 200;
/// The longest report, in bytes of Markdown.
pub const MAX_MARKDOWN: usize = 200 * 1024;
/// The most sources one report may cite.
pub const MAX_SOURCES: usize = 50;
/// The longest source title kept, in characters.
const MAX_SOURCE_TITLE: usize = 200;
/// The longest question kept, in characters.
pub const MAX_QUESTION: usize = 1000;
/// The most other opened pages a report may link to.
const MAX_LINKS: usize = 200;

/// One numbered source of a report: a page the bot opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Its number, from 1, as cited (`[1]`).
    pub n: u32,
    /// The page, as [`normalize_web_url`] writes it.
    pub url: String,
    /// The page's title.
    pub title: String,
    /// Its site, e.g. `example.com`.
    pub site: String,
    /// When the bot first opened it (ms since the Unix epoch).
    pub read_ms: i64,
}

/// A saved report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Its id: `r_` and 32 hex digits.
    pub id: String,
    /// When it was saved (ms since the Unix epoch).
    pub created_ms: i64,
    /// Who the turn that made it acted for (`None`: the home computer or unknown).
    pub asked_by: Option<PersonId>,
    /// What was asked, as the chat shows it.
    pub question: String,
    /// Its title, one line.
    pub title: String,
    /// One line about it, shown on its card.
    pub summary: String,
    /// The report as the bot wrote it.
    pub markdown: String,
    /// Its sources, in order.
    pub sources: Vec<Source>,
    /// Other pages the bot opened that the report links to (normalized).
    pub links: Vec<String>,
}

/// A page the bot opened in this conversation: its title and when it first
/// opened it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visit {
    /// The page's title.
    pub title: String,
    /// When (ms since the Unix epoch).
    pub first_ms: i64,
}

/// A report that passed [`check`], ready to save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// One line.
    pub title: String,
    /// One line.
    pub summary: String,
    /// As written.
    pub markdown: String,
    /// Numbered from 1.
    pub sources: Vec<Source>,
    /// Other opened pages it links to.
    pub links: Vec<String>,
}

/// A new report id: `r_` and 32 hex digits, which can't be guessed (access
/// is still checked on every open).
#[must_use]
pub fn new_id() -> String {
    format!("r_{}", uuid::Uuid::new_v4().simple())
}

/// Whether `id` looks like a report id (checked before any lookup).
#[must_use]
pub fn is_id(id: &str) -> bool {
    id.strip_prefix("r_")
        .is_some_and(|hex| hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Checks `save_report`'s arguments against the pages the bot opened
/// (`visited`, keyed by normalized address). Returns the report to save, or
/// what to tell the bot. Nothing is saved on an error.
///
/// # Errors
/// The text for the bot: what's wrong and how to fix it.
pub fn check(args: &Value, visited: &BTreeMap<String, Visit>) -> Result<Draft, String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("");
    let title = one_line(text("title"));
    let summary = one_line(text("summary"));
    let markdown = text("markdown").trim().to_owned();
    if title.is_empty() || summary.is_empty() || markdown.is_empty() {
        return Err(
            "Not saved: a report needs a title, a summary and the report itself.".to_owned(),
        );
    }
    if title.chars().count() > MAX_TITLE {
        return Err(format!(
            "Not saved: the title is longer than {MAX_TITLE} characters. Shorten it."
        ));
    }
    if summary.chars().count() > MAX_SUMMARY {
        return Err(format!(
            "Not saved: the summary is longer than {MAX_SUMMARY} characters. Make it one short line."
        ));
    }
    if markdown.len() > MAX_MARKDOWN {
        return Err(format!(
            "Not saved: the report is longer than {} KB. Shorten it.",
            MAX_MARKDOWN / 1024
        ));
    }
    let sources = check_sources(args.get("sources"), visited)?;
    let cited = citations(&markdown);
    let count = u32::try_from(sources.len()).unwrap_or(u32::MAX);
    let bad: Vec<String> = cited
        .iter()
        .filter(|n| **n == 0 || **n > count)
        .map(|n| format!("[{n}]"))
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "Not saved: {} cite{} a source that isn't in the list (there {} {count}). Number \
             citations in the order of your sources, from [1].",
            bad.join(", "),
            if bad.len() == 1 { "s" } else { "" },
            if count == 1 { "is" } else { "are" },
        ));
    }
    let uncited: Vec<String> = sources
        .iter()
        .filter(|s| !cited.contains(&s.n))
        .map(|s| format!("[{}] {}", s.n, s.url))
        .collect();
    if !uncited.is_empty() {
        return Err(format!(
            "Not saved: the report never cites {}. Cite each source where you use it, or drop it.",
            uncited.join(", ")
        ));
    }
    let listed: BTreeSet<&str> = sources.iter().map(|s| s.url.as_str()).collect();
    let links = link_targets(&markdown)
        .into_iter()
        .filter(|url| !listed.contains(url.as_str()) && visited.contains_key(url))
        .take(MAX_LINKS)
        .collect();
    Ok(Draft {
        title,
        summary,
        markdown,
        sources,
        links,
    })
}

/// The `sources` argument as numbered sources, each a page the bot opened.
fn check_sources(
    list: Option<&Value>,
    visited: &BTreeMap<String, Visit>,
) -> Result<Vec<Source>, String> {
    let list = match list {
        Some(Value::Array(list)) => list,
        Some(Value::Null) | None => &Vec::new(),
        Some(_) => return Err("Not saved: sources must be a list of {url, title}.".to_owned()),
    };
    if list.is_empty() {
        return Err(
            "Not saved: a report needs at least one source, a page you opened in the browser."
                .to_owned(),
        );
    }
    if list.len() > MAX_SOURCES {
        return Err(format!(
            "Not saved: at most {MAX_SOURCES} sources. Keep the ones you cite most."
        ));
    }
    let mut sources = Vec::with_capacity(list.len());
    let mut not_web = Vec::new();
    let mut not_opened = Vec::new();
    let mut twice = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let given = item.get("url").and_then(Value::as_str).unwrap_or("");
        let shown: String = given.chars().take(200).collect();
        let Some(url) = normalize_web_url(given) else {
            not_web.push(shown);
            continue;
        };
        let Some(visit) = visited.get(&url) else {
            not_opened.push(shown);
            continue;
        };
        if sources.iter().any(|s: &Source| s.url == url) {
            twice.push(shown);
            continue;
        }
        let title = one_line(item.get("title").and_then(Value::as_str).unwrap_or(""));
        let title = if title.is_empty() {
            one_line(&visit.title)
        } else {
            title
        };
        let title: String = title.chars().take(MAX_SOURCE_TITLE).collect();
        let site = web_host(&url).map(|h| site_of(&h)).unwrap_or_default();
        sources.push(Source {
            n: u32::try_from(i + 1).unwrap_or(u32::MAX),
            title: if title.is_empty() {
                site.clone()
            } else {
                title
            },
            url,
            site,
            read_ms: visit.first_ms,
        });
    }
    if !not_web.is_empty() {
        return Err(format!(
            "Not saved: these sources aren't web pages (only http and https pages on the web \
             count): {}. Drop them and their claims.",
            not_web.join(", ")
        ));
    }
    if !not_opened.is_empty() {
        return Err(format!(
            "Not saved: these sources weren't opened in the browser in this conversation: {}. \
             Open them first, or drop them and their claims.",
            not_opened.join(", ")
        ));
    }
    if !twice.is_empty() {
        return Err(format!(
            "Not saved: these sources are listed more than once: {}. List each page once.",
            twice.join(", ")
        ));
    }
    Ok(sources)
}

/// `text` on one line: every run of whitespace (line breaks too) is one space.
fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The Markdown parser oxbot uses for reports: tables and strikethrough,
/// nothing else (no footnotes, heading attributes or math).
fn parser(markdown: &str) -> TextMergeStream<'_, Parser<'_>> {
    TextMergeStream::new(Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ))
}

/// Where a run of text sits: in a code block or a link's own text,
/// `[n]` is shown as written and isn't a citation.
#[derive(Default)]
struct Where {
    code: u32,
    link: u32,
}

impl Where {
    fn cites(&self) -> bool {
        self.code == 0 && self.link == 0
    }
}

/// The citation numbers (`[3]`) in a report's text, outside code and link
/// text: what [`render`] turns into links.
#[must_use]
pub fn citations(markdown: &str) -> BTreeSet<u32> {
    let mut found = BTreeSet::new();
    let mut at = Where::default();
    for event in parser(markdown) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => at.code += 1,
            Event::End(TagEnd::CodeBlock) => at.code = at.code.saturating_sub(1),
            Event::Start(Tag::Link { .. }) => at.link += 1,
            Event::End(TagEnd::Link) => at.link = at.link.saturating_sub(1),
            Event::Text(text) if at.cites() => {
                found.extend(split_citations(&text).into_iter().filter_map(|p| match p {
                    Piece::Cite(n) => Some(n),
                    Piece::Text(_) => None,
                }));
            }
            _ => {}
        }
    }
    found
}

/// The pages a report's links point to, normalized (non-web ones left out).
fn link_targets(markdown: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for event in parser(markdown) {
        if let Event::Start(Tag::Link { dest_url, .. }) = event
            && let Some(url) = normalize_web_url(&dest_url)
            && !out.contains(&url)
        {
            out.push(url);
        }
    }
    out
}

/// A piece of text: plain, or a citation `[n]`.
#[derive(Debug, PartialEq, Eq)]
enum Piece<'a> {
    Text(&'a str),
    Cite(u32),
}

/// Splits `text` at citations: `[` then 1 to 3 digits then `]`.
fn split_citations(text: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            let digits = bytes[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if (1..=3).contains(&digits) && bytes.get(i + 1 + digits) == Some(&b']') {
                let n = text[i + 1..i + 1 + digits].parse().unwrap_or(0);
                if start < i {
                    pieces.push(Piece::Text(&text[start..i]));
                }
                pieces.push(Piece::Cite(n));
                i += digits + 2;
                start = i;
                continue;
            }
        }
        i += 1;
    }
    if start < text.len() {
        pieces.push(Piece::Text(&text[start..]));
    }
    pieces
}

/// `text` safe in HTML text and in a quoted attribute.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A link that opens outside the app, to a page already checked.
fn link_open(url: &str) -> String {
    format!(
        "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">",
        escape(url)
    )
}

/// The report's body as safe HTML (see the module docs).
fn body_html(report: &Report) -> String {
    let sources: BTreeMap<String, u32> = report
        .sources
        .iter()
        .map(|s| (s.url.clone(), s.n))
        .collect();
    let numbers: BTreeSet<u32> = report.sources.iter().map(|s| s.n).collect();
    let allowed = |url: &str| -> Option<String> {
        let url = normalize_web_url(url)?;
        (sources.contains_key(&url) || report.links.contains(&url)).then_some(url)
    };
    let mut at = Where::default();
    // For each open link: whether it was kept (so its end is too).
    let mut kept_links: Vec<bool> = Vec::new();
    let mut out: Vec<Event<'_>> = Vec::new();
    for event in parser(&report.markdown) {
        match event {
            // Raw HTML from the bot (or a page it quoted) is shown as text.
            Event::Start(Tag::HtmlBlock) => out.push(Event::Start(Tag::Paragraph)),
            Event::End(TagEnd::HtmlBlock) => out.push(Event::End(TagEnd::Paragraph)),
            Event::Html(html) | Event::InlineHtml(html) => {
                out.push(Event::InlineHtml(CowStr::from(escape(&html))));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                at.link += 1;
                match allowed(&dest_url) {
                    Some(url) => {
                        kept_links.push(true);
                        out.push(Event::InlineHtml(CowStr::from(link_open(&url))));
                    }
                    None => kept_links.push(false),
                }
            }
            Event::End(TagEnd::Link) => {
                at.link = at.link.saturating_sub(1);
                if kept_links.pop() == Some(true) {
                    out.push(Event::InlineHtml(CowStr::from("</a>")));
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                at.code += 1;
                out.push(Event::Start(Tag::CodeBlock(kind)));
            }
            Event::End(TagEnd::CodeBlock) => {
                at.code = at.code.saturating_sub(1);
                out.push(Event::End(TagEnd::CodeBlock));
            }
            // A wide table scrolls on its own, not the whole page.
            Event::Start(Tag::Table(align)) => {
                out.push(Event::InlineHtml(CowStr::from("<div class=\"tbl\">")));
                out.push(Event::Start(Tag::Table(align)));
            }
            Event::End(TagEnd::Table) => {
                out.push(Event::End(TagEnd::Table));
                out.push(Event::InlineHtml(CowStr::from("</div>")));
            }
            Event::Text(text) => {
                let mut html = String::with_capacity(text.len() + 16);
                for piece in split_citations(&text) {
                    match piece {
                        Piece::Cite(n) if at.cites() && numbers.contains(&n) => {
                            let _ = write!(
                                html,
                                "<sup class=\"cite\"><a href=\"about:srcdoc#src-{n}\">[{n}]</a></sup>"
                            );
                        }
                        Piece::Cite(n) => {
                            let _ = write!(html, "[{n}]");
                        }
                        Piece::Text(t) => html.push_str(&escape(t)),
                    }
                }
                out.push(Event::InlineHtml(CowStr::from(html)));
            }
            // Not enabled; never passed through as they are.
            Event::InlineMath(t) | Event::DisplayMath(t) | Event::FootnoteReference(t) => {
                out.push(Event::InlineHtml(CowStr::from(escape(&t))));
            }
            // No images, only their words; and nothing of what isn't enabled.
            Event::Start(
                Tag::Image { .. } | Tag::FootnoteDefinition(_) | Tag::MetadataBlock(_),
            )
            | Event::End(TagEnd::Image | TagEnd::FootnoteDefinition | TagEnd::MetadataBlock(_)) => {
            }
            other => out.push(other),
        }
    }
    let mut html = String::with_capacity(report.markdown.len() * 3 / 2);
    push_html(&mut html, out.into_iter());
    html
}

/// The report's Content-Security-Policy: nothing loads, nothing runs.
pub const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src 'none'; \
    font-src 'none'; media-src 'none'; connect-src 'none'; script-src 'none'; \
    form-action 'none'; base-uri 'none'; frame-ancestors 'self'";

/// The report as one complete, safe HTML document. `bot_name` is who made
/// it; dates are shown at `utc_offset_min` minutes east of UTC (the
/// viewer's time zone).
#[must_use]
pub fn render(report: &Report, bot_name: &str, utc_offset_min: i32) -> String {
    let when = |ms: i64| escape(&format_time(ms, utc_offset_min));
    let mut page = String::with_capacity(report.markdown.len() * 2 + 8 * 1024);
    let _ = write!(
        page,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{CSP}\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <title>{title}</title><style>{STYLE}</style></head><body><main>\
         <header><h1>{title}</h1><p class=\"meta\">{date} · By {bot}</p>\
         <p class=\"asked\"><span>Asked</span> {question}</p></header>\
         <article>{body}</article>\
         <section class=\"sources\"><h2>Sources</h2><ol>",
        title = escape(&report.title),
        date = when(report.created_ms),
        bot = escape(bot_name),
        question = escape(&report.question),
        body = body_html(report),
    );
    for s in &report.sources {
        let _ = write!(
            page,
            "<li id=\"src-{n}\"><span class=\"st\">{title}</span> \
             <span class=\"site\">{site}</span><br>{open}{url}</a> \
             <span class=\"read\">read {read}</span></li>",
            n = s.n,
            title = escape(&s.title),
            site = escape(&s.site),
            open = link_open(&s.url),
            url = escape(&s.url),
            read = when(s.read_ms),
        );
    }
    page.push_str("</ol></section></main></body></html>");
    page
}

/// `2 Nov 2026, 14:05` (with `UTC` when shown at UTC).
fn format_time(ms: i64, utc_offset_min: i32) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let offset = i64::from(utc_offset_min.clamp(-14 * 60, 14 * 60));
    let minutes = ms.div_euclid(60_000) + offset;
    let days = minutes.div_euclid(24 * 60);
    let in_day = minutes.rem_euclid(24 * 60);
    let (year, month, day) = civil_from_days(days);
    let month = usize::try_from(month - 1)
        .ok()
        .and_then(|m| MONTHS.get(m))
        .unwrap_or(&"");
    let zone = if utc_offset_min == 0 { " UTC" } else { "" };
    format!(
        "{day} {month} {year}, {:02}:{:02}{zone}",
        in_day / 60,
        in_day % 60
    )
}

/// Days since 1970-01-01 as (year, month 1–12, day 1–31), proleptic
/// Gregorian (H. Hinnant's `civil_from_days`).
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The page's look: system fonts and oxbot's colors, light and dark,
/// readable on a phone and a Mac. Public so the app's page can allow exactly
/// this stylesheet (by its hash) in the report frame.
pub const STYLE: &str = ":root{--bg:#FFFFFF;--ink:#111111;--muted:#555555;--line:#E4E4E0;\
--soft:#F4F4F2;--accent:#D61F1F;color-scheme:light dark}\
@media (prefers-color-scheme:dark){:root{--bg:#181C22;--ink:#F2F2F0;--muted:#A2A7AF;\
--line:#2F353E;--soft:#20252D;--accent:#FF6060}}\
*{box-sizing:border-box}html{-webkit-text-size-adjust:100%}\
body{margin:0;background:var(--bg);color:var(--ink);\
font:16px/1.6 -apple-system,BlinkMacSystemFont,\"Segoe UI\",system-ui,sans-serif;overflow-wrap:anywhere}\
main{max-width:46rem;margin:0 auto;padding:20px 18px 48px}\
header{border-bottom:1px solid var(--line);padding-bottom:12px;margin-bottom:16px}\
h1{font-size:1.5rem;line-height:1.25;margin:0 0 6px}\
h2{font-size:1.2rem;margin:1.6em 0 .5em}h3{font-size:1.05rem;margin:1.4em 0 .4em}\
.meta{color:var(--muted);font-size:.85rem;margin:0}\
.asked{margin:8px 0 0;font-size:.95rem}.asked span{color:var(--muted);font-size:.8rem;\
text-transform:uppercase;letter-spacing:.06em;margin-right:6px}\
a{color:var(--accent)}sup.cite{font-size:.72em;line-height:0}sup.cite a{text-decoration:none}\
code{font:.88em ui-monospace,\"SF Mono\",Menlo,monospace;background:var(--soft);padding:0 4px;border-radius:4px}\
pre{background:var(--soft);padding:10px 12px;border-radius:8px;overflow-x:auto}pre code{padding:0;background:none}\
blockquote{margin:1em 0;padding:0 14px;border-left:3px solid var(--line);color:var(--muted)}\
.tbl{overflow-x:auto;margin:1em 0}table{border-collapse:collapse;min-width:100%;font-size:.92rem}\
th,td{border-bottom:1px solid var(--line);padding:7px 10px;text-align:left;vertical-align:top}\
th{background:var(--soft);font-weight:600}\
hr{border:0;border-top:1px solid var(--line);margin:1.5em 0}\
.sources{border-top:1px solid var(--line);margin-top:28px}\
.sources ol{padding-left:1.4em;font-size:.9rem}.sources li{margin:0 0 10px}\
.sources li:target{background:var(--soft);border-radius:6px}\
.st{font-weight:600}.site,.read{color:var(--muted);font-size:.82rem}";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const A: &str = "https://cafe-a.example.com/";
    const B: &str = "https://cafe-b.example.com/menu";

    fn visited() -> BTreeMap<String, Visit> {
        [
            (A, "Cafe A"),
            (B, "Cafe B menu"),
            ("https://maps.example.com/", "Map"),
        ]
        .into_iter()
        .map(|(url, title)| {
            (
                url.to_owned(),
                Visit {
                    title: title.to_owned(),
                    first_ms: 1_793_000_000_000,
                },
            )
        })
        .collect()
    }

    fn report(markdown: &str) -> Report {
        Report {
            id: new_id(),
            created_ms: 1_793_000_000_000,
            asked_by: None,
            question: "research coffee near me".to_owned(),
            title: "Coffee near home".to_owned(),
            summary: "Cafe A wins.".to_owned(),
            markdown: markdown.to_owned(),
            sources: vec![
                Source {
                    n: 1,
                    url: A.to_owned(),
                    title: "Cafe A".to_owned(),
                    site: "example.com".to_owned(),
                    read_ms: 1_793_000_000_000,
                },
                Source {
                    n: 2,
                    url: B.to_owned(),
                    title: "Cafe B".to_owned(),
                    site: "example.com".to_owned(),
                    read_ms: 1_793_000_000_000,
                },
            ],
            links: vec![
                "https://maps.example.com/".to_owned(),
                "https://cafe-a.example.com/?a=%22b%22".to_owned(),
            ],
        }
    }

    fn args(markdown: &str, sources: &Value) -> Value {
        json!({ "title": "Coffee", "summary": "Cafe A wins.", "markdown": markdown, "sources": sources })
    }

    #[test]
    fn a_report_with_opened_and_cited_sources_is_kept() {
        let draft = check(
            &args(
                "**Cafe A** [1] beats Cafe B [2]. See the [map](https://maps.example.com/#here).",
                &json!([{ "url": A, "title": "" }, { "url": "https://CAFE-B.example.com/menu#x", "title": "Cafe B" }]),
            ),
            &visited(),
        )
        .unwrap();
        assert_eq!(draft.sources.len(), 2);
        assert_eq!(draft.sources[0].title, "Cafe A", "the page's own title");
        assert_eq!(draft.sources[1].url, B, "compared in one form");
        assert_eq!(draft.sources[1].site, "example.com");
        assert_eq!(draft.links, ["https://maps.example.com/"]);
    }

    #[test]
    fn sources_must_be_web_pages_the_bot_opened() {
        let not_opened = check(
            &args(
                "x [1]",
                &json!([{ "url": "https://elsewhere.example.org/" }]),
            ),
            &visited(),
        )
        .unwrap_err();
        assert!(
            not_opened.contains("weren't opened in the browser"),
            "{not_opened}"
        );
        assert!(not_opened.contains("elsewhere.example.org"), "{not_opened}");
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "http://192.168.1.1/",
        ] {
            let err = check(&args("x [1]", &json!([{ "url": bad }])), &visited()).unwrap_err();
            assert!(err.contains("aren't web pages"), "{bad}: {err}");
        }
        let none = check(&args("x", &json!([])), &visited()).unwrap_err();
        assert!(none.contains("at least one source"), "{none}");
        let twice = check(
            &args("x [1] [2]", &json!([{ "url": A }, { "url": A }])),
            &visited(),
        )
        .unwrap_err();
        assert!(twice.contains("more than once"), "{twice}");
    }

    #[test]
    fn every_citation_has_a_source_and_every_source_is_cited() {
        let two = json!([{ "url": A }, { "url": B }]);
        let out_of_range = check(&args("x [1] [2] [9]", &two), &visited()).unwrap_err();
        assert!(out_of_range.contains("[9]"), "{out_of_range}");
        let uncited = check(&args("only [1]", &two), &visited()).unwrap_err();
        assert!(uncited.contains("never cites [2]"), "{uncited}");
        let zero = check(&args("x [0] [1] [2]", &two), &visited()).unwrap_err();
        assert!(zero.contains("[0]"), "{zero}");
    }

    #[test]
    fn lengths_are_checked() {
        let one = json!([{ "url": A }]);
        let mut long = args("x [1]", &one);
        long["title"] = json!("t".repeat(MAX_TITLE + 1));
        assert!(check(&long, &visited()).unwrap_err().contains("title"));
        let big = "x [1] ".repeat(MAX_MARKDOWN / 4);
        assert!(
            check(&args(&big, &one), &visited())
                .unwrap_err()
                .contains("KB")
        );
        let many: Vec<Value> = (0..=MAX_SOURCES).map(|_| json!({ "url": A })).collect();
        assert!(
            check(&args("x [1]", &json!(many)), &visited())
                .unwrap_err()
                .contains("at most")
        );
        let mut lines = args("x [1]", &one);
        lines["title"] = json!("Coffee\n\nnear  home");
        assert_eq!(check(&lines, &visited()).unwrap().title, "Coffee near home");
    }

    #[test]
    fn citations_are_read_outside_code_and_link_text() {
        let cited = citations(
            "a [1] b [2][3] `[4]` [x [5]](https://a.example/)\n\n```\n[6]\n```\n[1234] [ 7]",
        );
        assert_eq!(cited.into_iter().collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn headings_tables_and_citations_render() {
        let html = render(
            &report(
                "## Best pick\n\nCafe A [1].\n\n| Cafe | Rating |\n|---|---|\n| A | 4.8 [1] |\n| B | 4.1 [2] |\n",
            ),
            "Coffee Scout",
            0,
        );
        assert!(html.contains("<h2>Best pick</h2>"), "{html}");
        assert!(html.contains("<div class=\"tbl\"><table>"), "{html}");
        assert!(
            html.contains(
                "<td>4.8 <sup class=\"cite\"><a href=\"about:srcdoc#src-1\">[1]</a></sup></td>"
            ),
            "{html}"
        );
        assert!(
            html.contains("<li id=\"src-2\"><span class=\"st\">Cafe B</span>"),
            "{html}"
        );
        assert!(
            html.contains(&format!(
                "<a href=\"{A}\" target=\"_blank\" rel=\"noopener noreferrer\">{A}</a>"
            )),
            "{html}"
        );
        assert!(html.contains("read 26 Oct 2026, 07:33 UTC"), "{html}");
        assert!(html.contains("prefers-color-scheme:dark"));
        assert!(html.contains(&format!("content=\"{CSP}\"")));
        assert!(html.contains("By Coffee Scout"));
        assert!(html.contains("research coffee near me"));
    }

    #[test]
    fn dates_show_in_the_viewers_time_zone() {
        assert_eq!(format_time(0, 0), "1 Jan 1970, 00:00 UTC");
        assert_eq!(format_time(1_793_000_000_000, -300), "26 Oct 2026, 02:33");
        assert_eq!(format_time(1_772_323_200_000, 0), "1 Mar 2026, 00:00 UTC");
        assert_eq!(format_time(951_782_400_000, 0), "29 Feb 2000, 00:00 UTC");
    }

    /// The rendered HTML has only the tags a report uses, with no
    /// attribute that runs or loads anything, and links only to the web or
    /// to its own sources list.
    fn assert_inert(html: &str) {
        const TAGS: [&str; 31] = [
            "p",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "a",
            "sup",
            "div",
            "table",
            "thead",
            "tbody",
            "tr",
            "th",
            "td",
            "ul",
            "ol",
            "li",
            "code",
            "pre",
            "em",
            "strong",
            "del",
            "blockquote",
            "span",
            "section",
            "article",
            "main",
            "body",
            "html",
        ];
        let mut rest = html;
        while let Some(at) = rest.find('<') {
            let end = rest[at..].find('>').map_or(rest.len(), |e| at + e);
            let tag = &rest[at + 1..end];
            let name: String = tag
                .trim_start_matches('/')
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            assert!(
                TAGS.contains(&name.as_str()) || ["hr", "br"].contains(&name.as_str()),
                "<{tag}> in {html}"
            );
            let mut lower = tag.to_ascii_lowercase();
            for side in ["left", "center", "right"] {
                lower = lower.replace(&format!("style=\"text-align: {side}\""), "");
            }
            lower = lower.replace("rel=\"noopener noreferrer\"", "rel");
            for attr in lower.split_whitespace().skip(1) {
                let key = attr.split('=').next().unwrap_or("");
                assert!(
                    ["href", "target", "rel", "class", "id", "start"].contains(&key)
                        || attr.starts_with('"'),
                    "attribute {attr} in <{tag}>"
                );
                if key == "href" {
                    let value = attr.trim_start_matches("href=\"");
                    assert!(
                        value.starts_with("https://")
                            || value.starts_with("http://")
                            || value.starts_with("about:srcdoc#src-"),
                        "link {attr} in {html}"
                    );
                    assert!(!value.starts_with("http://192.168"), "{html}");
                    assert!(!value.contains("not-a-source"), "{html}");
                }
            }
            rest = &rest[end..];
        }
        assert!(!html.contains("tracker.example"), "{html}");
    }

    #[test]
    fn scripts_raw_html_and_bad_links_are_shown_safely() {
        let cases = [
            (
                "<script>alert(1)</script>",
                "&lt;script&gt;alert(1)&lt;/script&gt;",
            ),
            (
                "Hi <script>alert(1)</script> there [1] [2]",
                "Hi &lt;script&gt;",
            ),
            (
                "<img src=x onerror=alert(1)>",
                "&lt;img src=x onerror=alert(1)&gt;",
            ),
            (
                "<a href=\"javascript:alert(1)\">x</a>",
                "&lt;a href=&quot;javascript",
            ),
            ("[x](javascript:alert(1))", "<p>x</p>"),
            ("[x](JaVaScRiPt:alert(1))", "<p>x</p>"),
            ("[x](&#106;avascript:alert(1))", "<p>x</p>"),
            ("<javascript:alert(1)>", "<p>javascript"),
            ("[x][r]\n\n[r]: javascript:alert(1)", "<p>x</p>"),
            ("[x](data:text/html,<b>hi</b>)", "x"),
            ("[x](vbscript:msgbox)", "<p>x</p>"),
            ("[home](http://192.168.1.1/)", "<p>home</p>"),
            ("[phish](https://not-a-source.example/)", "<p>phish</p>"),
            ("![t](https://tracker.example/p.png)", "<p>t</p>"),
            (
                "[q](<https://cafe-a.example.com/?a=\"b\"> \"ti\\\"tle\")",
                "href=\"https://cafe-a.example.com/?a=%22b%22\"",
            ),
            (
                "| a |\n|---|\n| <iframe src=x></iframe> |",
                "<td>&lt;iframe src=x&gt;&lt;/iframe&gt;</td>",
            ),
            (
                "<div onclick=\"x()\">\nhi\n</div>",
                "&lt;div onclick=&quot;x()&quot;&gt;",
            ),
            ("<svg onload=alert(1)>", "&lt;svg onload"),
        ];
        for (markdown, shown) in cases {
            let html = body_html(&report(markdown));
            assert!(html.contains(shown), "{markdown} → {html}");
            assert_inert(&html);
        }
        let quoted = body_html(&report(
            "[q](<https://cafe-a.example.com/?a=\"b\"> \"ti\\\"tle\")",
        ));
        assert!(!quoted.contains("title="), "no title attribute: {quoted}");
        let kept = body_html(&report(
            "[Cafe A](https://cafe-a.example.com/) and [map](https://maps.example.com/)",
        ));
        assert!(
            kept.contains(&format!(
                "<a href=\"{A}\" target=\"_blank\" rel=\"noopener noreferrer\">Cafe A</a>"
            )),
            "{kept}"
        );
        assert!(
            kept.contains("<a href=\"https://maps.example.com/\""),
            "{kept}"
        );
    }

    #[test]
    fn titles_and_questions_cant_break_out_of_the_page() {
        let mut r = report("x [1] [2]");
        r.title = "</style><script>alert(1)</script>".to_owned();
        r.question = "<img src=x onerror=alert(1)>".to_owned();
        r.sources[0].title = "\"><script>x</script>".to_owned();
        let html = render(&r, "<b>Bot</b>", 0);
        assert_eq!(html.matches("<script").count(), 0, "{html}");
        assert_eq!(html.matches("</style>").count(), 1, "only the page's own");
        assert!(html.contains("&lt;/style&gt;&lt;script&gt;"));
        assert!(html.contains("By &lt;b&gt;Bot&lt;/b&gt;"));
    }

    #[test]
    fn a_big_report_stays_within_limits() {
        let row = "| <b>A</b> [1] | 4.8 [2] | [link](javascript:x) |\n";
        let mut markdown = String::from("| a | b | c |\n|---|---|---|\n");
        while markdown.len() < MAX_MARKDOWN - row.len() {
            markdown.push_str(row);
        }
        let html = render(&report(&markdown), "Bot", 0);
        assert!(html.len() < MAX_MARKDOWN * 4, "{}", html.len());
        assert_inert(&html[html.find("<article>").unwrap()..]);
        let nested = "> ".repeat(5000) + "deep [1]";
        assert_inert(&body_html(&report(&nested)));
    }

    #[test]
    fn ids_are_random_and_checked() {
        let id = new_id();
        assert!(is_id(&id), "{id}");
        assert_ne!(id, new_id());
        for bad in [
            "",
            "r_",
            "r_xyz",
            "x_0123456789abcdef0123456789abcdef",
            "r_0123456789abcdef0123456789abcdef0",
        ] {
            assert!(!is_id(bad), "{bad}");
        }
    }

    #[test]
    fn split_finds_citations_only() {
        assert_eq!(
            split_citations("a [1] b[22][x] [1234]"),
            [
                Piece::Text("a "),
                Piece::Cite(1),
                Piece::Text(" b"),
                Piece::Cite(22),
                Piece::Text("[x] [1234]"),
            ]
        );
    }
}
