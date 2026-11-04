//! What a session keeps about its conversation: the notes on each stored
//! result ([`state::EntryMeta`]) and the model's to-do list.

pub mod state;

/// `url` short: host and path, query cut at 40 characters.
#[must_use]
pub fn short_url(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split('#').next().unwrap_or(rest);
    match rest.split_once('?') {
        Some((path, query)) if query.chars().count() > 40 => {
            format!("{path}?{}…", query.chars().take(40).collect::<String>())
        }
        _ => rest.to_owned(),
    }
}

/// Tokens in words: `812 tokens`, `14.2k tokens`.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn tokens_in_words(n: u64) -> String {
    if n < 1_000 {
        format!("{n} tokens")
    } else {
        format!("{:.1}k tokens", n as f64 / 1_000.0)
    }
}

/// The tool's own short name: `take_snapshot`, `Read`, `find_in_page`.
#[must_use]
pub fn short_tool(name: &str) -> &str {
    name.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__").map(|(_, t)| t))
        .unwrap_or(name)
}
