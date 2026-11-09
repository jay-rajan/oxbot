//! Notes oxbot adds to a Codex turn: checking a final answer before it's
//! accepted, and the nudge to wrap up near the step limit.
//!
//! When a turn that used tools answers in plain text, oxbot looks for the
//! values in the answer (numbers of 10 or more, decimals, money, percents,
//! temperatures) that nothing the bot read in this task had ([`numbers`]),
//! and sends the answer back once with a short checklist: the task word
//! for word, those values, and "check each part the task asked for".
//!
//! Answers taken from the web must link their pages: a turn keeps the
//! pages it read ([`SourcePage`], from each result's page and the rows of
//! `read_pages`). A substantial answer with no link at all goes back once
//! with [`link_line`] (in the same note as the check above), and if it
//! still has none, oxbot adds a Sources list itself ([`with_sources`]):
//! only pages the turn really read, never an address of the model's.
//!
//! [`numbers`]: crate::numbers

use std::fmt::Write as _;

/// Tool calls a turn needs before its answer is checked.
pub const CHECK_AFTER_CALLS: usize = 2;
/// Most ungrounded values named.
pub const MAX_NAMED: usize = 8;
/// Most characters of the task quoted back.
const MAX_GOAL: usize = 600;

/// The note that sends an answer back: the task word for word, the values
/// nothing read had (if any), and the checklist. `browser`: name the page
/// tools; else the stored results and files.
#[must_use]
pub fn note(goal: &str, ungrounded: &[String], browser: bool) -> String {
    let goal = goal.trim();
    let goal = match goal.char_indices().nth(MAX_GOAL) {
        Some((cut, _)) => format!("{}…", &goal[..cut]),
        None => goal.to_owned(),
    };
    let mut note = format!("[oxbot] Before you answer, check it. The task was: «{goal}».");
    if !ungrounded.is_empty() {
        let tools = if browser {
            "find_in_page, read_section or query_rows"
        } else {
            "read_result or the file tools"
        };
        let _ = write!(
            note,
            " These values aren't in anything you read in this task: {}. Check them ({tools}) or \
             remove them.",
            ungrounded.join(", ")
        );
    }
    note.push_str(
        " Check each part the task asked for (every filter, the number of items, each field). \
         Then reply to the person with your complete answer, written for them: they never see \
         this note, so don't describe your checks.",
    );
    note
}

/// Most pages a link note or a Sources list names.
pub const MAX_SOURCES: usize = 8;
/// An answer this long (characters), or with a list, gives information
/// that needs its pages linked; a shorter one ("Posted it.") doesn't.
const SOURCED_MIN_CHARS: usize = 160;

/// A web page a turn read: its address and title (may be empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePage {
    /// Its address, as the browser showed it.
    pub url: String,
    /// Its title, if known.
    pub title: String,
}

/// The pages one successful tool result read: the page it names
/// (`page_url`, `page_title`, as its result's notes say), and for
/// `read_pages` (`tool`) each row's page. Only pages worth citing (see
/// [`is_source`]).
#[must_use]
pub fn pages_in_result(
    tool: &str,
    page_url: Option<&str>,
    page_title: Option<&str>,
    text: &str,
) -> Vec<SourcePage> {
    let mut pages = Vec::new();
    if let Some(url) = page_url {
        add_source(
            &mut pages,
            SourcePage {
                url: url.to_owned(),
                title: page_title.unwrap_or_default().to_owned(),
            },
        );
    }
    if tool == crate::tools::read_pages::READ_PAGES {
        for line in text.lines().filter(|l| l.starts_with('{')) {
            let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(url) = row[crate::tools::read_pages::SOURCE].as_str() {
                add_source(
                    &mut pages,
                    SourcePage {
                        url: url.to_owned(),
                        title: String::new(),
                    },
                );
            }
        }
    }
    pages
}

/// Adds `page` to `pages` (in reading order) unless it's there already
/// (by address, without its `#…`) or isn't worth citing. A title learned
/// later fills an empty one.
pub fn add_source(pages: &mut Vec<SourcePage>, page: SourcePage) {
    if !is_source(&page) {
        return;
    }
    let key = |u: &str| {
        u.split('#')
            .next()
            .unwrap_or(u)
            .trim_end_matches('/')
            .to_owned()
    };
    let new = key(&page.url);
    if let Some(known) = pages.iter_mut().find(|p| key(&p.url) == new) {
        if known.title.is_empty() {
            known.title = page.title;
        }
        return;
    }
    pages.push(page);
}

/// Whether a page is worth citing: a web page on the internet (http or
/// https; not `about:blank`, oxbot's own pages on this computer or the
/// home network), and not a sign-in wall.
#[must_use]
pub fn is_source(page: &SourcePage) -> bool {
    let Some((host, path)) = host_and_path(&page.url) else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let local = host == "localhost"
        || host.rsplit('.').next() == Some("local")
        || host
            .trim_matches(|c| c == '[' || c == ']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| match ip {
                std::net::IpAddr::V4(v4) => {
                    v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
                }
                std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
            });
    if local {
        return false;
    }
    let path = path.to_ascii_lowercase();
    let wall_path = [
        "login", "log-in", "signin", "sign-in", "sign_in", "/auth", "oauth", "/sso", "/i/flow/",
    ]
    .iter()
    .any(|w| path.contains(w));
    let wall_host = ["accounts.", "login.", "auth.", "signin."]
        .iter()
        .any(|h| host.starts_with(h));
    let title = page.title.trim().to_ascii_lowercase();
    let wall_title = ["sign in", "log in", "login", "sign-in"]
        .iter()
        .any(|t| title.starts_with(t));
    !(wall_path || wall_host || wall_title)
}

/// The host and the rest (path, query) of an http(s) address.
fn host_and_path(url: &str) -> Option<(&str, &str)> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let host = authority.rsplit('@').next().unwrap_or(authority);
    // Without its port (an IPv6 address keeps its colons).
    let host = match host.rfind(':') {
        Some(i) if !host.ends_with(']') && host[i + 1..].chars().all(|c| c.is_ascii_digit()) => {
            &host[..i]
        }
        _ => host,
    };
    (!host.is_empty()).then_some((host, path))
}

/// Whether `text` has a web link: a Markdown link or a bare address.
#[must_use]
pub fn has_link(text: &str) -> bool {
    ["https://", "http://"].iter().any(|scheme| {
        text.match_indices(scheme).any(|(i, _)| {
            text[i + scheme.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '[')
        })
    })
}

/// Whether `answer`, given after reading `pages`, must link them: it
/// gives information (long enough, or a list of two or more items) and
/// has no link at all.
#[must_use]
pub fn needs_links(answer: &str, pages: &[SourcePage]) -> bool {
    if pages.is_empty() || has_link(answer) {
        return false;
    }
    let items = answer
        .lines()
        .map(str::trim_start)
        .filter(|l| {
            l.starts_with("- ")
                || l.starts_with("* ")
                || l.split_once(". ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .count();
    answer.trim().chars().count() >= SOURCED_MIN_CHARS || items >= 2
}

/// The line a check note adds for an answer with no links: link each
/// item, and the pages read (up to [`MAX_SOURCES`]).
#[must_use]
pub fn link_line(pages: &[SourcePage]) -> String {
    let urls: Vec<&str> = pages
        .iter()
        .take(MAX_SOURCES)
        .map(|p| p.url.as_str())
        .collect();
    format!(
        "Add a link to the page for each item you took from the web, as [title](url). Pages you \
         read: {}.",
        urls.join(", ")
    )
}

/// `answer` with oxbot's own Sources list after it, if it needs links
/// ([`needs_links`]): the pages read (up to [`MAX_SOURCES`]), those whose
/// site or title words appear in the answer first, then in reading
/// order. Every address is one the turn read.
#[must_use]
pub fn with_sources(answer: &str, pages: &[SourcePage]) -> String {
    if !needs_links(answer, pages) {
        return answer.to_owned();
    }
    let lower = answer.to_lowercase();
    let mentioned = |p: &SourcePage| {
        let host = host_and_path(&p.url).map_or("", |(h, _)| h).to_lowercase();
        let host_words = host
            .split('.')
            .filter(|w| w.len() >= 4 && !matches!(*w, "www" | "com" | "org" | "net"));
        let title = p.title.to_lowercase();
        let title_words = title
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 5);
        host_words.chain(title_words).any(|w| lower.contains(w))
    };
    let (first, rest): (Vec<&SourcePage>, Vec<&SourcePage>) =
        pages.iter().partition(|p| mentioned(p));
    let mut out = answer.trim_end().to_owned();
    out.push_str("\n\n**Sources**");
    for page in first.into_iter().chain(rest).take(MAX_SOURCES) {
        let _ = write!(
            out,
            "\n- [{}]({})",
            source_label(page),
            link_target(&page.url)
        );
    }
    out
}

/// How a source shows: `host — title`, or `host — path` without one.
fn source_label(page: &SourcePage) -> String {
    let (host, path) = host_and_path(&page.url).unwrap_or((page.url.as_str(), ""));
    let host = host.strip_prefix("www.").unwrap_or(host);
    let title = page.title.trim();
    let what = if title.is_empty() {
        path.split(['?', '#'])
            .next()
            .unwrap_or("")
            .trim_matches('/')
    } else {
        title
    };
    let label = if what.is_empty() {
        host.to_owned()
    } else {
        format!("{host} — {what}")
    };
    // Brackets would end the link's text early.
    label.replace('[', "(").replace(']', ")")
}

/// An address as a Markdown link's target: spaces and parentheses
/// escaped, so the link can't end early.
fn link_target(url: &str) -> String {
    url.replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
}

/// Shortest first answer worth keeping over a much shorter checked one.
const KEEP_FIRST_MIN_CHARS: usize = 300;

/// The answer to show after a check: the checked one, unless it shrank to
/// less than half of a substantial first answer. Small models sometimes
/// answer the check with a report about their checks ("The plan meets all
/// the requirements…") instead of the answer; then the first one is kept.
#[must_use]
pub fn answer_after_check(first: &str, checked: &str) -> String {
    let (first, checked) = (first.trim(), checked.trim());
    let first_len = first.chars().count();
    if first_len >= KEEP_FIRST_MIN_CHARS && checked.chars().count() * 2 < first_len {
        return first.to_owned();
    }
    if checked.is_empty() {
        return first.to_owned();
    }
    checked.to_owned()
}

/// Share of the step budget after which the model is told to wrap up.
const WRAP_UP_PERCENT: u32 = 70;

/// The wrap-up note once `step` reaches 70% of `max_steps` (once:
/// `nudged` says it was given).
pub fn wrap_up_note(step: u32, max_steps: u32, nudged: &mut bool) -> Option<String> {
    let at = (max_steps * WRAP_UP_PERCENT).div_ceil(100);
    if *nudged || step < at || step >= max_steps {
        return None;
    }
    *nudged = true;
    let left = max_steps - step;
    Some(format!(
        "[oxbot] You have {left} steps left: finish with what you have, and say what's missing."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_quotes_the_task_and_names_the_values() {
        assert_eq!(
            note(
                "[Jay] the 5 tallest finished towers",
                &["351.8".into(), "13".into()],
                true
            ),
            "[oxbot] Before you answer, check it. The task was: «[Jay] the 5 tallest finished \
             towers». These values aren't in anything you read in this task: 351.8, 13. Check \
             them (find_in_page, read_section or query_rows) or remove them. Check each part the \
             task asked for (every filter, the number of items, each field). Then reply to the \
             person with your complete answer, written for them: they never see this note, so \
             don't describe your checks."
        );
        assert_eq!(
            note(" [Sam] sum the file ", &[], false),
            "[oxbot] Before you answer, check it. The task was: «[Sam] sum the file». Check each \
             part the task asked for (every filter, the number of items, each field). Then reply \
             to the person with your complete answer, written for them: they never see this \
             note, so don't describe your checks."
        );
        assert!(
            note("[Sam] go", &["12".into()], false).contains("(read_result or the file tools)")
        );
        assert!(note("[Sam] go", &[], false).contains("they never see this note"));
        let long = "x".repeat(700);
        let n = note(&long, &[], true);
        assert!(n.contains(&format!("«{}…»", "x".repeat(600))), "{n}");
    }

    fn page(url: &str, title: &str) -> SourcePage {
        SourcePage {
            url: url.to_owned(),
            title: title.to_owned(),
        }
    }

    #[test]
    fn links_are_found_bare_or_in_markdown() {
        assert!(has_link("See [OpenAI](https://openai.com/news)."));
        assert!(has_link("From https://anthropic.com/news, two items."));
        assert!(has_link("http://example.com"));
        assert!(!has_link("Five items from openai.com and anthropic.com."));
        assert!(!has_link("Write https:// before the address."));
        assert!(!has_link(""));
    }

    #[test]
    fn only_web_pages_that_arent_sign_in_walls_are_sources() {
        let mut pages = Vec::new();
        for (url, title) in [
            ("https://openai.com/news/", "News"),
            ("https://openai.com/news#top", ""),
            ("about:blank", ""),
            ("chrome://newtab", ""),
            ("http://127.0.0.1:7878/report/1", "Report"),
            ("http://localhost:3000/", ""),
            ("http://192.168.1.1/", "Router"),
            ("https://x.com/i/flow/login", "X"),
            ("https://accounts.google.com/v3/signin", ""),
            ("https://example.com/members", "Sign in to Example"),
            ("https://www.anthropic.com/news", "Newsroom"),
        ] {
            add_source(&mut pages, page(url, title));
        }
        assert_eq!(
            pages,
            [
                page("https://openai.com/news/", "News"),
                page("https://www.anthropic.com/news", "Newsroom"),
            ]
        );
    }

    #[test]
    fn read_pages_rows_name_their_pages() {
        let text = "Read 2 of 2 pages with your script: 2 rows.\n\
                    {\"price\":3,\"source\":\"https://shop.example.com/a\"}\n\
                    {\"price\":4,\"source\":\"https://shop.example.com/b\"}\n";
        let pages = pages_in_result(crate::tools::read_pages::READ_PAGES, None, None, text);
        assert_eq!(
            pages.iter().map(|p| p.url.as_str()).collect::<Vec<_>>(),
            ["https://shop.example.com/a", "https://shop.example.com/b"]
        );
        let one = pages_in_result("take_snapshot", Some("https://a.example/"), Some("A"), text);
        assert_eq!(
            one,
            [page("https://a.example/", "A")],
            "rows only for read_pages"
        );
    }

    #[test]
    fn a_long_or_listed_answer_without_links_needs_them() {
        let read = [page("https://openai.com/news", "News")];
        let list = "Five updates:\n- GPT news\n- Pricing";
        assert!(needs_links(list, &read));
        assert!(needs_links(&"word ".repeat(40), &read));
        assert!(!needs_links("Posted it.", &read), "a short reply");
        assert!(!needs_links(list, &[]), "nothing read");
        assert!(!needs_links(
            "- [GPT news](https://openai.com/news)\n- b",
            &read
        ));
        assert_eq!(
            link_line(&read),
            "Add a link to the page for each item you took from the web, as [title](url). Pages \
             you read: https://openai.com/news."
        );
    }

    #[test]
    fn the_sources_list_names_only_pages_read_most_relevant_first() {
        let pages = [
            page("https://www.bbc.co.uk/weather", "Weather"),
            page("https://openai.com/index/gpt-news/", ""),
            page("https://www.anthropic.com/news", "Newsroom (latest)"),
        ];
        let answer = "Two updates:\n- Anthropic shipped a model\n- OpenAI changed pricing";
        assert_eq!(
            with_sources(answer, &pages),
            "Two updates:\n- Anthropic shipped a model\n- OpenAI changed pricing\n\n**Sources**\n\
             - [openai.com — index/gpt-news](https://openai.com/index/gpt-news/)\n\
             - [anthropic.com — Newsroom (latest)](https://www.anthropic.com/news)\n\
             - [bbc.co.uk — Weather](https://www.bbc.co.uk/weather)"
        );
        // Linked already, or short: as it was.
        let linked = "- [a](https://openai.com/index/gpt-news/)\n- b";
        assert_eq!(with_sources(linked, &pages), linked);
        assert_eq!(with_sources("Done.", &pages), "Done.");
        // At most 8, each a page that was read.
        let many: Vec<SourcePage> = (0..12)
            .map(|i| page(&format!("https://site{i}.example/p"), ""))
            .collect();
        let out = with_sources("- one\n- two", &many);
        assert_eq!(out.matches("\n- [").count(), MAX_SOURCES);
        for line in out.lines().filter(|l| l.starts_with("- [")) {
            let url = line.rsplit_once("](").unwrap().1.trim_end_matches(')');
            assert!(many.iter().any(|p| p.url == url), "{url} wasn't read");
        }
    }

    #[test]
    fn a_report_about_the_checks_never_replaces_the_answer() {
        let plan = "Monday: stir-fry. 1 lb chicken, 2 peppers. Steps: 1. Cook rice. ".repeat(10);
        let report = "The plan meets all the task requirements; recipes are in meal-plan.md.";
        assert_eq!(answer_after_check(&plan, report), plan.trim());
        // A checked answer of about the same size wins (it may fix values).
        let fixed = plan.replace("2 peppers", "3 peppers");
        assert_eq!(answer_after_check(&plan, &fixed), fixed.trim());
        // A short first answer: the checked one wins, even if shorter.
        assert_eq!(
            answer_after_check("It's 42 km.", "About 40 km."),
            "About 40 km."
        );
        // An empty checked answer keeps the first.
        assert_eq!(answer_after_check("Here it is.", "  "), "Here it is.");
    }
}
