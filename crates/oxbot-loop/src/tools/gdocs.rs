//! `read_document`: the whole text of a Google Doc, Slides presentation or
//! Sheet. The editors draw only part of a file (Slides and Sheets on a
//! canvas, loaded as you scroll), so a page snapshot misses most of it.
//! Instead oxbot reads the file's own export addresses on Google's site,
//! from a tab on that site (so the person's sign-in applies), with one
//! fixed GET each ([`oxbot_engine::SealedWork::Fetch`]). The bot only
//! names the file; it never gets to run a script or pick another address.
//!
//! What Google gives (checked against public files, without signing in):
//!
//! - Slides: `/presentation/d/ID/htmlpresent` is an HTML page with one
//!   `<div class="slide" title="Slide N">` per slide (its text in
//!   `section.slide-content`, its speaker notes in `aside.slide-notes`),
//!   served by `docs.google.com` itself. `/presentation/d/ID/export/txt`
//!   is plain text without slide boundaries (the fallback).
//! - Docs: `/document/d/ID/export?format=txt` is plain text (after a
//!   redirect to a Google download host); `/document/d/ID/mobilebasic` is
//!   the whole document as simple HTML (the fallback).
//! - Sheets: `/spreadsheets/d/ID/htmlview` names every sheet
//!   (`items.push({name: "…", …, gid: "N"})`); then
//!   `/spreadsheets/d/ID/export?format=csv&gid=N` for each (redirected
//!   like Docs), or `/spreadsheets/d/ID/gviz/tq?tqx=out:csv&gid=N`.
//!
//! This module is the pure part: the tool as the model sees it, reading
//! the address the bot gave, the addresses oxbot reads (and the hub's
//! check of them), and turning what Google returns into text.

use std::fmt::Write as _;
use std::sync::LazyLock;

use oxbot_llm::ToolSpec;
use regex::Regex;
use serde_json::{Value, json};

/// The name models call `read_document` by.
pub const READ_DOCUMENT: &str = "mcp__oxbot__read_document";
/// Google's site for Docs, Slides and Sheets.
pub const GOOGLE_DOCS: &str = "https://docs.google.com";
/// Google Drive's site.
const GOOGLE_DRIVE_HOST: &str = "drive.google.com";
/// End-to-end tests only (with `OXBOT_E2E=1`): the test site that stands
/// in for [`GOOGLE_DOCS`] (a `.test` host).
pub const E2E_DOCS_URL: &str = "OXBOT_E2E_DOCS_URL";
/// The most rows of one sheet shown.
pub const MAX_SHEET_ROWS: usize = 500;
/// The most sheets of one spreadsheet read.
pub const MAX_SHEETS: usize = 30;
/// The most text oxbot hands back (characters); the model reads it in
/// parts through the stored result.
pub const MAX_TEXT: usize = 2_000_000;

/// `read_document` as the model sees it.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: READ_DOCUMENT.to_owned(),
        description: "Read a whole Google Doc, Slides presentation or Sheet; use this instead of \
            reading the editor page. The editors show only part of a file (slides and sheets load \
            as you scroll), so a snapshot misses most of it. Give the file's address \
            (docs.google.com/document/d/…, /presentation/d/…, /spreadsheets/d/…, or a \
            drive.google.com/file/d/… link to one). oxbot reads it from Google with the \
            browser's sign-in and returns all of it: every slide numbered with its speaker \
            notes, the document's text, or each sheet as a table (the first 500 rows). Big \
            results are saved: read the rest with read_result."
            .to_owned(),
        parameters: json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The Google file's address, as in the browser or a link."
                }
            },
            "required": ["url"]
        }),
    }
}

/// The site oxbot reads Google files from: [`GOOGLE_DOCS`], or in
/// end-to-end tests the test site named by [`E2E_DOCS_URL`].
#[must_use]
pub fn docs_base() -> String {
    let e2e = std::env::var_os("OXBOT_E2E").is_some_and(|v| v == "1");
    if e2e
        && let Ok(base) = std::env::var(E2E_DOCS_URL)
        && test_base_ok(&base)
    {
        return base;
    }
    GOOGLE_DOCS.to_owned()
}

/// A test stand-in may only be a bare `.test` site.
fn test_base_ok(base: &str) -> bool {
    split_url(base)
        .is_some_and(|u| u.host.to_ascii_lowercase().ends_with(".test") && u.path.is_empty())
}

/// Which kind of Google file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A Google Doc.
    Doc,
    /// A Google Slides presentation.
    Slides,
    /// A Google Sheet.
    Sheet,
}

impl Kind {
    /// Its part of an address (`/document/d/ID`).
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::Doc => "document",
            Self::Slides => "presentation",
            Self::Sheet => "spreadsheets",
        }
    }

    fn from_path(part: &str) -> Option<Self> {
        Some(match part {
            "document" => Self::Doc,
            "presentation" => Self::Slides,
            "spreadsheets" => Self::Sheet,
            _ => return None,
        })
    }

    /// The file, in words.
    #[must_use]
    pub fn noun(self) -> &'static str {
        match self {
            Self::Doc => "Google Doc",
            Self::Slides => "Google Slides presentation",
            Self::Sheet => "Google Sheet",
        }
    }

    /// The editor, in words.
    #[must_use]
    pub fn editor(self) -> &'static str {
        match self {
            Self::Doc => "Google Docs",
            Self::Slides => "Google Slides",
            Self::Sheet => "Google Sheets",
        }
    }
}

/// A Google file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleFile {
    /// Its kind.
    pub kind: Kind,
    /// Its id.
    pub id: String,
}

/// What an address the bot gave names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A Google file.
    File(GoogleFile),
    /// A file in Google Drive (its id): it may or may not be a Google
    /// file; opening it tells.
    Drive(String),
}

/// An address in parts.
struct Parts<'a> {
    scheme: String,
    host: String,
    path: &'a str,
    query: &'a str,
}

/// Splits an address: scheme and host (lower case, port kept), path and
/// query (no `#…`). `None` for one with a user name or no host.
fn split_url(url: &str) -> Option<Parts<'_>> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let rest = rest.split('#').next().unwrap_or_default();
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    Some(Parts {
        scheme: scheme.to_ascii_lowercase(),
        host: authority.to_ascii_lowercase(),
        path: path.trim_end_matches('/'),
        query,
    })
}

/// Whether `id` looks like a Google file id.
fn id_ok(id: &str) -> bool {
    (10..=200).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The `id` in a query (`?id=…&…`).
fn query_id(query: &str) -> Option<&str> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("id="))
        .filter(|id| id_ok(id))
}

/// The file an address names, on `base` (normally [`GOOGLE_DOCS`]) or
/// Google Drive.
///
/// # Errors
/// What's wrong with it, for the model.
pub fn parse(url: &str, base: &str) -> Result<Target, String> {
    let not_google = || {
        format!(
            "{} isn't a Google Docs, Slides or Sheets address. read_document reads only those; \
             read other pages in the browser.",
            url.trim()
        )
    };
    let u = split_url(url).ok_or_else(not_google)?;
    let b = split_url(base).ok_or_else(not_google)?;
    let docs = u.host == b.host && u.scheme == b.scheme;
    let drive = u.host == GOOGLE_DRIVE_HOST && u.scheme == "https";
    if !docs && !drive {
        return Err(not_google());
    }
    let segs: Vec<&str> = u.path.split('/').filter(|s| !s.is_empty()).collect();
    if drive {
        let id = match segs.as_slice() {
            ["file", "d", id, ..] | ["file", "u", _, "d", id, ..] if id_ok(id) => Some(*id),
            ["open"] => query_id(u.query),
            _ => None,
        };
        return id
            .map(|id| Target::Drive(id.to_owned()))
            .ok_or_else(not_google);
    }
    if segs.as_slice() == ["open"] {
        return query_id(u.query)
            .map(|id| Target::Drive(id.to_owned()))
            .ok_or_else(not_google);
    }
    let Some(kind) = segs.first().and_then(|k| Kind::from_path(k)) else {
        return Err(not_google());
    };
    // `/document/u/1/d/ID/…` is the same file, for a second account.
    let rest = match segs.get(1..) {
        Some(["u", _, rest @ ..] | rest) => rest,
        None => &[][..],
    };
    match rest {
        ["d", "e", ..] => Err(format!(
            "{} is a copy published to the web, not the file itself: read it in the browser, or \
             give the file's own address.",
            url.trim()
        )),
        ["d", id, ..] if id_ok(id) => Ok(Target::File(GoogleFile {
            kind,
            id: (*id).to_owned(),
        })),
        _ => Err(not_google()),
    }
}

impl GoogleFile {
    /// Its editor's address (what the person opens).
    #[must_use]
    pub fn editor_url(&self, base: &str) -> String {
        format!("{}/edit", self.file_url(base))
    }

    fn file_url(&self, base: &str) -> String {
        format!("{base}/{}/d/{}", self.kind.path(), self.id)
    }

    /// Slides: every slide and its notes, as HTML.
    #[must_use]
    pub fn slides_html_url(&self, base: &str) -> String {
        format!("{}/htmlpresent", self.file_url(base))
    }

    /// Slides: the text without slide boundaries.
    #[must_use]
    pub fn slides_txt_url(&self, base: &str) -> String {
        format!("{}/export/txt", self.file_url(base))
    }

    /// Docs: the text.
    #[must_use]
    pub fn doc_txt_url(&self, base: &str) -> String {
        format!("{}/export?format=txt", self.file_url(base))
    }

    /// Docs: the whole document as simple HTML.
    #[must_use]
    pub fn doc_html_url(&self, base: &str) -> String {
        format!("{}/mobilebasic", self.file_url(base))
    }

    /// Sheets: the page that names every sheet.
    #[must_use]
    pub fn sheet_list_url(&self, base: &str) -> String {
        format!("{}/htmlview", self.file_url(base))
    }

    /// Sheets: one sheet as CSV.
    #[must_use]
    pub fn sheet_csv_url(&self, base: &str, gid: &str) -> String {
        format!("{}/export?format=csv&gid={gid}", self.file_url(base))
    }

    /// Sheets: one sheet as CSV, through the chart data source.
    #[must_use]
    pub fn sheet_gviz_url(&self, base: &str, gid: &str) -> String {
        format!("{}/gviz/tq?tqx=out:csv&gid={gid}", self.file_url(base))
    }

    /// Every address oxbot may read for this file (sheet `gid` if given).
    fn readable(&self, base: &str, gid: Option<&str>) -> Vec<String> {
        match self.kind {
            Kind::Slides => vec![self.slides_html_url(base), self.slides_txt_url(base)],
            Kind::Doc => vec![self.doc_txt_url(base), self.doc_html_url(base)],
            Kind::Sheet => {
                let mut v = vec![self.sheet_list_url(base)];
                if let Some(gid) = gid {
                    v.push(self.sheet_csv_url(base, gid));
                    v.push(self.sheet_gviz_url(base, gid));
                }
                v
            }
        }
    }
}

/// The address that opens Drive file `id` in its own editor (Google
/// sends a Google file to its editor, anything else to Drive's viewer).
#[must_use]
pub fn open_url(base: &str, id: &str) -> String {
    format!("{base}/open?id={id}")
}

/// The hub's check: `url` is one of the addresses oxbot reads for a
/// Google file on `base`, exactly as oxbot builds them.
#[must_use]
pub fn may_fetch(url: &str, base: &str) -> bool {
    if !url.starts_with(&format!("{base}/")) {
        return false;
    }
    let Ok(Target::File(file)) = parse(url, base) else {
        return false;
    };
    let gid = url
        .rsplit_once("&gid=")
        .map(|(_, g)| g)
        .filter(|g| !g.is_empty() && g.len() <= 12 && g.bytes().all(|b| b.is_ascii_digit()));
    file.readable(base, gid).iter().any(|u| u == url)
}

/// Whether `url` (on `base`) is on Google's Docs site at all.
#[must_use]
pub fn on_docs_site(url: &str, base: &str) -> bool {
    match (split_url(url), split_url(base)) {
        (Some(u), Some(b)) => u.host == b.host && u.scheme == b.scheme,
        _ => false,
    }
}

/// The kind of editor a tab at `url` shows, if it's a Google file's
/// editor (not one of the addresses oxbot reads).
#[must_use]
pub fn editor_kind(url: &str, base: &str) -> Option<Kind> {
    let Ok(Target::File(file)) = parse(url, base) else {
        return None;
    };
    let path = split_url(url)
        .map(|u| u.path.to_owned())
        .unwrap_or_default();
    let tail = path.rsplit('/').next().unwrap_or_default();
    let reading = path.contains("/export") || path.contains("/gviz/");
    (!reading && !matches!(tail, "htmlpresent" | "mobilebasic" | "htmlview")).then_some(file.kind)
}

/// What a snapshot of a Google editor says first.
#[must_use]
pub fn editor_hint(kind: Kind) -> String {
    format!(
        "[oxbot: This is a {} editor: the page shows only part of it. Use read_document to read \
         all of it.]\n",
        kind.editor()
    )
}

/// What one read got back (the hub's answer, see
/// [`oxbot_engine::SealedWork::Fetch`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Got {
    /// The HTTP status.
    pub status: u16,
    /// Its content type.
    pub content_type: String,
    /// Where it ended (after redirects).
    pub url: String,
    /// The text.
    pub text: String,
    /// Only its start was read.
    pub cut: bool,
}

impl Got {
    /// Reads the hub's answer.
    #[must_use]
    pub fn from_value(v: &Value) -> Self {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
        Self {
            status: v["status"]
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .unwrap_or(0),
            content_type: s("type"),
            url: s("url"),
            text: s("text"),
            cut: v["cut"].as_bool().unwrap_or(false),
        }
    }

    /// Why this isn't the file's content, if it isn't: a sign-in page, no
    /// access, no such file, or another error. `html_ok`: HTML is what was
    /// asked for.
    #[must_use]
    pub fn problem(&self, html_ok: bool) -> Option<String> {
        let signin = self.url.contains("accounts.google.com")
            || self.url.contains("ServiceLogin")
            || matches!(self.status, 401 | 403);
        if signin {
            return Some(
                "Google wants the person to sign in, or this Google account can't open the file \
                 (call request_sign_in if a sign-in is needed)"
                    .to_owned(),
            );
        }
        match self.status {
            200 => {}
            404 => {
                return Some(
                    "Google says there's no such file, or it isn't shared with this Google account"
                        .to_owned(),
                );
            }
            429 => return Some("Google says too many requests: try again later".to_owned()),
            n => return Some(format!("Google answered with an error ({n})")),
        }
        if !html_ok && self.content_type.contains("text/html") {
            return Some("Google sent a web page instead of the file's text".to_owned());
        }
        None
    }
}

static TAG: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").ok());
static SCRIPT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?is)<(script|style|noscript)\b[^>]*>.*?</(script|style|noscript)\s*>").ok()
});
static LINE_END: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\s*(?:<br\s*/?>|</(?:p|div|li|tr|h[1-6]|section|aside|table|ul|ol)\s*>))+")
        .ok()
});
static CELL_END: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(?i)</t[dh]\s*>").ok());
static SLIDE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"<div\b[^>]*\bclass="slide"[^>]*>"#).ok());
static SLIDE_CONTENT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?s)<section\b[^>]*\bclass="slide-content"[^>]*>(.*?)</section>"#).ok()
});
static SLIDE_NOTES: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?s)<aside\b[^>]*\bclass="slide-notes"[^>]*>(.*?)</aside>"#).ok()
});
static TITLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok());
static DOC_CONTENT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"<div\b[^>]*\bclass="doc-content"[^>]*>"#).ok());
static SHEET_ITEM: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"items\.push\(\{name: "((?:[^"\\]|\\.)*)",[^}]*?\bgid: "(\d{1,12})""#).ok()
});

/// `text` with every match of `re` replaced by `with`.
fn replace(re: Option<&Regex>, text: &str, with: &str) -> String {
    match re {
        Some(re) => re.replace_all(text, with).into_owned(),
        None => text.to_owned(),
    }
}

/// Text with HTML's character references read.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| {
            let name = &tail[1..e];
            let c = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                _ => {
                    let n = if let Some(hex) =
                        name.strip_prefix("#x").or_else(|| name.strip_prefix("#X"))
                    {
                        u32::from_str_radix(hex, 16).ok()?
                    } else {
                        name.strip_prefix('#')?.parse().ok()?
                    };
                    char::from_u32(n)?
                }
            };
            Some((c, e + 1))
        });
        if let Some((c, len)) = decoded {
            out.push(c);
            rest = &tail[len..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Plain text: the lines trimmed, soft breaks (Google's vertical tab, which
/// its HTML shows as U+FFFD) as line breaks, and no more than one blank
/// line in a row.
fn tidy(text: &str) -> String {
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace(['\r', '\u{b}', '\u{fffd}'], "\n");
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

/// An HTML fragment's text: a line per paragraph, block or row, cells
/// apart by tabs.
fn html_text(html: &str) -> String {
    let html = replace(SCRIPT.as_ref(), html, "");
    let html = replace(LINE_END.as_ref(), &html, "\n");
    let html = replace(CELL_END.as_ref(), &html, "\t");
    let text = replace(TAG.as_ref(), &html, "");
    tidy(&decode_entities(&text))
}

/// A page's title, without " - Google Slides" and the like.
fn title_of(html: &str) -> Option<String> {
    let t = decode_entities(TITLE.as_ref()?.captures(html)?.get(1)?.as_str().trim());
    let t = ["Google Docs", "Google Slides", "Google Sheets"]
        .iter()
        .fold(t, |t, tail| {
            t.trim_end_matches(tail)
                .trim_end()
                .trim_end_matches(['-', '–'])
                .trim_end()
                .to_owned()
        });
    (!t.is_empty()).then_some(t)
}

/// One slide's text and speaker notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slide {
    /// What's on it.
    pub text: String,
    /// Its speaker notes.
    pub notes: String,
}

/// The slides on Slides' `htmlpresent` page, in order.
#[must_use]
pub fn slides_from_html(html: &str) -> Vec<Slide> {
    let Some(slide) = SLIDE.as_ref() else {
        return Vec::new();
    };
    let starts: Vec<usize> = slide.find_iter(html).map(|m| m.start()).collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(html.len());
            let chunk = &html[start..end];
            let part = |re: Option<&Regex>| {
                re.and_then(|re| re.captures(chunk))
                    .and_then(|c| c.get(1))
                    .map(|m| html_text(m.as_str()))
                    .unwrap_or_default()
            };
            Slide {
                text: part(SLIDE_CONTENT.as_ref()),
                notes: part(SLIDE_NOTES.as_ref()),
            }
        })
        .collect()
}

/// Slides, numbered: "Slide 1 of 9:", its text, then its notes.
#[must_use]
pub fn slides_text(slides: &[Slide]) -> String {
    let n = slides.len();
    let mut out = String::new();
    for (i, s) in slides.iter().enumerate() {
        let _ = writeln!(out, "Slide {} of {n}:", i + 1);
        if s.text.is_empty() {
            out.push_str("(no text)\n");
        } else {
            out.push_str(&s.text);
            out.push('\n');
        }
        if !s.notes.is_empty() {
            let _ = writeln!(out, "Speaker notes: {}", s.notes);
        }
        out.push('\n');
    }
    out.trim_end().to_owned()
}

/// Slides' `export/txt`, tidied: Google gives no slide boundaries there.
#[must_use]
pub fn slides_from_txt(txt: &str) -> String {
    tidy(txt)
}

/// A Doc's `export?format=txt`, tidied.
#[must_use]
pub fn doc_from_txt(txt: &str) -> String {
    tidy(txt)
}

/// A Doc's text from its `mobilebasic` page.
#[must_use]
pub fn doc_from_html(html: &str) -> String {
    let body = DOC_CONTENT
        .as_ref()
        .and_then(|re| re.find(html))
        .map_or(html, |m| &html[m.end()..]);
    html_text(body)
}

/// A sheet of a spreadsheet: its name and id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetTab {
    /// Its name.
    pub name: String,
    /// Its id (`gid`).
    pub gid: String,
}

/// A JavaScript string literal's text (`\x3d`, `é`, `\/`, `\"`).
fn js_unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The sheets Sheets' `htmlview` page names, in order, once each.
#[must_use]
pub fn sheets_from_html(html: &str) -> Vec<SheetTab> {
    let mut tabs: Vec<SheetTab> = Vec::new();
    let Some(item) = SHEET_ITEM.as_ref() else {
        return tabs;
    };
    for c in item.captures_iter(html) {
        let gid = c[2].to_owned();
        if tabs.iter().any(|t| t.gid == gid) {
            continue;
        }
        tabs.push(SheetTab {
            name: js_unescape(&c[1]),
            gid,
        });
    }
    tabs
}

/// CSV's rows (RFC 4180: quoted fields may hold commas, quotes and line
/// breaks).
#[must_use]
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.trim_start_matches('\u{feff}');
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// One cell for a Markdown table row.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace("\r\n", " ")
        .replace(['\n', '\r'], " ")
        .trim()
        .to_owned()
}

/// A sheet as a Markdown table (its first row as the header), empty rows
/// and trailing empty columns left out, at most `max_rows` rows below the
/// header, saying how many more there are.
#[must_use]
pub fn sheet_table(name: &str, csv: &str, max_rows: usize) -> String {
    let mut rows: Vec<Vec<String>> = parse_csv(csv)
        .into_iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .collect();
    let width = rows
        .iter()
        .map(|r| {
            r.iter()
                .rposition(|c| !c.trim().is_empty())
                .map_or(0, |p| p + 1)
        })
        .max()
        .unwrap_or(0);
    for r in &mut rows {
        r.resize(width, String::new());
    }
    let mut out = String::new();
    let total = rows.len().saturating_sub(1);
    let _ = writeln!(
        out,
        "## Sheet \"{name}\": {total} row{} below the header, {width} column{}",
        if total == 1 { "" } else { "s" },
        if width == 1 { "" } else { "s" }
    );
    let Some((header, body)) = rows.split_first() else {
        out.push_str("(empty)\n");
        return out;
    };
    let line = |r: &[String]| {
        format!(
            "| {} |\n",
            r.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | ")
        )
    };
    out.push_str(&line(header));
    let _ = writeln!(out, "|{}", " --- |".repeat(width));
    for r in body.iter().take(max_rows) {
        out.push_str(&line(r));
    }
    if body.len() > max_rows {
        let _ = writeln!(
            out,
            "[{} more rows left out (rows {} to {}): only the first {max_rows} rows of a sheet \
             are read]",
            body.len() - max_rows,
            max_rows + 1,
            body.len()
        );
    }
    out
}

/// The text handed back: a line saying what was read, then the content,
/// kept to [`MAX_TEXT`] characters.
#[must_use]
pub fn result_text(head: &str, body: &str, cut: bool) -> String {
    let mut out = format!("{}\n\n", head.trim_end());
    let mut body_cut = cut;
    match body.char_indices().nth(MAX_TEXT) {
        Some((at, _)) => {
            out.push_str(&body[..at]);
            body_cut = true;
        }
        None => out.push_str(body),
    }
    if body_cut {
        out.push_str("\n[cut: the file is very big, so only its start is here]");
    }
    out
}

/// A title for the head line: ` "Name"`, or nothing.
#[must_use]
pub fn titled(html: &str) -> String {
    title_of(html).map_or_else(String::new, |t| format!(" \"{t}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = GOOGLE_DOCS;

    fn file(kind: Kind, id: &str) -> Target {
        Target::File(GoogleFile {
            kind,
            id: id.to_owned(),
        })
    }

    #[test]
    fn ids_come_from_every_form_of_address() {
        let id = "1EAYk18WDjIG-zp_0vLm3CsfQh_i8eXc67Jo2O9C6Vuc";
        for (url, kind) in [
            (
                format!("https://docs.google.com/presentation/d/{id}/edit#slide=id.p7"),
                Kind::Slides,
            ),
            (
                format!("https://docs.google.com/presentation/d/{id}"),
                Kind::Slides,
            ),
            (
                format!("https://DOCS.google.com/document/d/{id}/edit?usp=sharing"),
                Kind::Doc,
            ),
            (
                format!("https://docs.google.com/document/u/1/d/{id}/edit"),
                Kind::Doc,
            ),
            (
                format!(" https://docs.google.com/spreadsheets/d/{id}/edit#gid=12 "),
                Kind::Sheet,
            ),
        ] {
            assert_eq!(parse(&url, BASE), Ok(file(kind, id)), "{url}");
        }
        for url in [
            format!("https://drive.google.com/file/d/{id}/view?usp=drive_link"),
            format!("https://drive.google.com/file/u/0/d/{id}/view"),
            format!("https://drive.google.com/open?id={id}"),
            format!("https://docs.google.com/open?id={id}&authuser=0"),
        ] {
            assert_eq!(parse(&url, BASE), Ok(Target::Drive(id.to_owned())), "{url}");
        }
    }

    #[test]
    fn other_addresses_are_refused() {
        let id = "1EAYk18WDjIG-zp_0vLm3CsfQh_i8eXc67Jo2O9C6Vuc";
        for url in [
            format!("https://evil.example.com/presentation/d/{id}/edit"),
            format!("https://docs.google.com.evil.example/presentation/d/{id}"),
            format!("https://docs.google.com@evil.example/presentation/d/{id}"),
            format!("http://docs.google.com/presentation/d/{id}"),
            format!("https://docs.google.com/forms/d/{id}/edit"),
            "https://docs.google.com/presentation/d/short/edit".to_owned(),
            format!("https://drive.google.com/drive/folders/{id}"),
            format!("file:///presentation/d/{id}"),
            "not a url".to_owned(),
        ] {
            let e = parse(&url, BASE).unwrap_err();
            assert!(
                e.contains("isn't a Google Docs, Slides or Sheets address"),
                "{url}: {e}"
            );
        }
        let e = parse(
            &format!("https://docs.google.com/spreadsheets/d/e/2PACX-{id}/pubhtml"),
            BASE,
        )
        .unwrap_err();
        assert!(e.contains("published to the web"), "{e}");
    }

    #[test]
    fn the_test_site_stands_in_only_for_itself() {
        let base = "http://docs.example.test";
        assert!(test_base_ok(base));
        assert!(!test_base_ok("https://docs.google.com"));
        assert!(!test_base_ok("http://docs.example.test/x"));
        assert_eq!(
            parse(
                "http://docs.example.test/presentation/d/e2e-deck-0001/edit",
                base
            ),
            Ok(file(Kind::Slides, "e2e-deck-0001"))
        );
        assert!(
            parse(
                "https://docs.google.com/presentation/d/e2e-deck-0001/edit",
                base
            )
            .is_err()
        );
    }

    #[test]
    fn oxbot_reads_only_the_files_own_export_addresses() {
        let f = GoogleFile {
            kind: Kind::Sheet,
            id: "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms".into(),
        };
        assert_eq!(
            f.sheet_csv_url(BASE, "0"),
            "https://docs.google.com/spreadsheets/d/1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms/export?format=csv&gid=0"
        );
        assert!(may_fetch(&f.sheet_list_url(BASE), BASE));
        assert!(may_fetch(&f.sheet_csv_url(BASE, "1234"), BASE));
        assert!(may_fetch(&f.sheet_gviz_url(BASE, "7"), BASE));
        assert!(!may_fetch(&f.editor_url(BASE), BASE));
        assert!(!may_fetch(
            &format!("{}&x=1", f.sheet_csv_url(BASE, "1")),
            BASE
        ));
        assert!(!may_fetch(&f.sheet_csv_url(BASE, "1;x"), BASE));
        assert!(!may_fetch(&f.sheet_list_url("https://evil.example"), BASE));
        let s = GoogleFile {
            kind: Kind::Slides,
            id: "1EAYk18WDjIG-zp_0vLm3CsfQh_i8eXc67Jo2O9C6Vuc".into(),
        };
        assert!(may_fetch(&s.slides_html_url(BASE), BASE));
        assert!(may_fetch(&s.slides_txt_url(BASE), BASE));
        // Another kind's export for a Slides id isn't one oxbot builds.
        assert!(!may_fetch(
            &s.slides_html_url(BASE).replace("presentation", "document"),
            BASE
        ));
        let d = GoogleFile {
            kind: Kind::Doc,
            id: s.id.clone(),
        };
        assert_eq!(
            d.doc_txt_url(BASE),
            "https://docs.google.com/document/d/1EAYk18WDjIG-zp_0vLm3CsfQh_i8eXc67Jo2O9C6Vuc/export?format=txt"
        );
        assert!(may_fetch(&d.doc_html_url(BASE), BASE));
        assert!(!may_fetch(
            &format!("{BASE}/document/d/{}/export?format=pdf", s.id),
            BASE
        ));
    }

    #[test]
    fn editors_get_the_hint_and_reads_dont() {
        let id = "1EAYk18WDjIG-zp_0vLm3CsfQh_i8eXc67Jo2O9C6Vuc";
        assert_eq!(
            editor_kind(&format!("{BASE}/presentation/d/{id}/edit#slide=id.p"), BASE),
            Some(Kind::Slides)
        );
        assert_eq!(
            editor_kind(&format!("{BASE}/spreadsheets/d/{id}/edit"), BASE),
            Some(Kind::Sheet)
        );
        assert_eq!(
            editor_kind(&format!("{BASE}/presentation/d/{id}/htmlpresent"), BASE),
            None
        );
        assert_eq!(
            editor_kind(&format!("{BASE}/document/d/{id}/export?format=txt"), BASE),
            None
        );
        assert_eq!(editor_kind("https://example.com/", BASE), None);
        assert!(editor_hint(Kind::Slides).contains(
            "This is a Google Slides editor: the page shows only part of it. Use read_document to read all of it."
        ));
    }

    #[test]
    fn slides_come_numbered_with_their_notes() {
        let html = r#"<html><head><title>Divit Learning - Google Slides</title><style>.slide{}</style></head><body>
<div role="group" class="slide" title="Slide 1"><h1 class="offscreen-header">1 of 3</h1><section class="slide-content" aria-label="Slide 1" style="background-image: url(x?a=1&amp;b=2)"><div class="shape"><p style="x">Week of Oct&nbsp;8</p></div><div class="shape"><p>Fractions &amp; decimals</p><p>Line one&#39;s�second line</p></div></section><aside class="slide-notes"><p>Ask about homework</p></aside></div>
<div role="group" class="slide" title="Slide 2"><section class="slide-content"><div class="shape" role="img" title="Picture"></div></section></div>
<div role="group" class="slide" title="Slide 3"><section class="slide-content"><div class="table"><table><tr><td><p>a</p></td><td><p>b</p></td></tr></table></div></section></div>
<script>var x = "<p>not text</p>";</script></body></html>"#;
        let slides = slides_from_html(html);
        assert_eq!(slides.len(), 3);
        assert_eq!(
            slides[0].text,
            "Week of Oct 8\nFractions & decimals\nLine one's\nsecond line"
        );
        assert_eq!(slides[0].notes, "Ask about homework");
        assert_eq!(slides[1].text, "");
        assert!(slides[2].text.contains('a') && slides[2].text.contains('b'));
        let text = slides_text(&slides);
        assert!(text.starts_with("Slide 1 of 3:\nWeek of Oct 8\n"), "{text}");
        assert!(
            text.contains("Speaker notes: Ask about homework\n\nSlide 2 of 3:\n(no text)"),
            "{text}"
        );
        assert!(!text.contains("not text"));
        assert_eq!(titled(html), " \"Divit Learning\"");
    }

    #[test]
    fn plain_text_exports_are_tidied() {
        let txt = "\u{feff}{Name Here}\r\nBaby Album\r\nImage Sources:\u{b}http://a\u{b}\r\n\r\n\r\n\r\nBath time";
        assert_eq!(
            slides_from_txt(txt),
            "{Name Here}\nBaby Album\nImage Sources:\nhttp://a\n\nBath time"
        );
        assert_eq!(doc_from_txt("\u{feff}Sample doc  \n"), "Sample doc");
        let basic = r#"<html><head><title>Plan - Google Docs</title><script>var a="<p>x</p>";</script></head><body><div class="doc-content"><h1>Plan</h1><p><span>First</span> step</p><table><tr><td>a</td><td>b</td></tr></table></div></body></html>"#;
        assert_eq!(doc_from_html(basic), "Plan\nFirst step\na\tb");
    }

    #[test]
    fn sheets_are_listed_from_their_page() {
        let html = r#"var items = []; items.push({name: "Class Data", pageUrl: "https:\/\/docs.google.com\/spreadsheets\/d\/x\/htmlview\/sheet?headers\x3dtrue&gid=0", gid: "0",initialSheet: ("0" == gid)});
items.push({name: "Q3 \x27plan\x27 é", pageUrl: "x", gid: "1234"}); items.push({name: "Class Data", pageUrl: "x", gid: "0"});"#;
        assert_eq!(
            sheets_from_html(html),
            vec![
                SheetTab {
                    name: "Class Data".into(),
                    gid: "0".into()
                },
                SheetTab {
                    name: "Q3 'plan' é".into(),
                    gid: "1234".into()
                },
            ]
        );
    }

    #[test]
    fn csv_reads_quotes_commas_and_line_breaks() {
        let rows = parse_csv("\u{feff}a,\"b, c\",\"say \"\"hi\"\"\"\r\n1,\"two\nlines\",3\r\n");
        assert_eq!(
            rows,
            vec![
                vec!["a".to_owned(), "b, c".into(), "say \"hi\"".into()],
                vec!["1".to_owned(), "two\nlines".into(), "3".into()],
            ]
        );
        assert_eq!(parse_csv("x,y"), vec![vec!["x".to_owned(), "y".into()]]);
        assert_eq!(parse_csv("").len(), 0);
    }

    #[test]
    fn a_sheet_is_a_table_with_a_cap() {
        let mut csv = String::from("Name,Score,,\n");
        for i in 1..=7 {
            let _ = writeln!(csv, "P{i},{i}|x,,");
        }
        csv.push_str(",,,\n");
        let t = sheet_table("Scores", &csv, 5);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(
            lines[0],
            "## Sheet \"Scores\": 7 rows below the header, 2 columns"
        );
        assert_eq!(lines[1], "| Name | Score |");
        assert_eq!(lines[2], "| --- | --- |");
        assert_eq!(lines[3], "| P1 | 1\\|x |");
        assert_eq!(lines.len(), 9);
        assert_eq!(
            lines[8],
            "[2 more rows left out (rows 6 to 7): only the first 5 rows of a sheet are read]"
        );
        assert!(sheet_table("Empty", "", 5).contains("(empty)"));
    }

    #[test]
    fn what_google_answers_is_checked() {
        let got = |status, url: &str, kind: &str| Got {
            status,
            content_type: kind.into(),
            url: url.into(),
            text: String::new(),
            cut: false,
        };
        assert_eq!(
            got(200, "https://x/export", "text/plain").problem(false),
            None
        );
        assert!(
            got(
                200,
                "https://accounts.google.com/ServiceLogin?x",
                "text/html"
            )
            .problem(true)
            .unwrap()
            .contains("request_sign_in")
        );
        assert!(got(403, "", "").problem(true).unwrap().contains("sign in"));
        assert!(
            got(404, "", "")
                .problem(true)
                .unwrap()
                .contains("no such file")
        );
        assert!(got(500, "", "").problem(true).unwrap().contains("(500)"));
        assert!(
            got(200, "", "text/html")
                .problem(false)
                .unwrap()
                .contains("web page")
        );
        assert_eq!(got(200, "", "text/html").problem(true), None);
        let v = json!({"status": 200, "type": "text/csv", "url": "u", "text": "a,b", "cut": true});
        assert_eq!(Got::from_value(&v).text, "a,b");
        assert!(Got::from_value(&v).cut);
    }

    #[test]
    fn huge_results_are_cut_with_a_note() {
        let body = "x".repeat(MAX_TEXT + 10);
        let t = result_text("Head", &body, false);
        assert!(t.starts_with("Head\n\nxxx"));
        assert!(t.ends_with("only its start is here]"));
        assert!(!result_text("Head", "short", false).contains("[cut"));
        assert!(result_text("Head", "short", true).contains("[cut"));
    }
}
