//! Unit tests for parsing, trimming, rendering, the map, find, section and
//! diff.

use std::fmt::Write as _;
use std::time::Instant;

use super::tests_golden::{PAGES, fixture, snap, strict};
use super::{Baseline, FindQuery, PageNews, Snapshot, diff, est, origin_path, page_news};

/// A snapshot from bare node lines.
fn of(lines: &str) -> Snapshot {
    Snapshot::parse(&format!("## Latest page snapshot\n{lines}"), &strict).unwrap()
}

fn none(_: &str) -> u8 {
    0
}

fn query<'a>(
    q: Option<&'a str>,
    roles: &[&str],
    near: Option<&'a str>,
    limit: usize,
) -> FindQuery<'a> {
    FindQuery {
        query: q,
        roles: roles.iter().map(|r| (*r).to_owned()).collect(),
        near,
        limit,
    }
}

/// A shop page: a filter form and `items` list items with ids from
/// `first`.
fn shop(snap_id: u32, items: usize, first: usize, checked: bool, button: &str) -> String {
    let mut s = String::from(
        "## Pages\n1: Shop (https://shop.example.com/search?q=lamp) [selected]\n## Latest page snapshot\n",
    );
    let _ = writeln!(
        s,
        "uid={snap_id}_0 RootWebArea \"Shop\" url=\"https://shop.example.com/search?q=lamp\""
    );
    let _ = writeln!(s, "  uid={snap_id}_1 main");
    let _ = writeln!(s, "    uid={snap_id}_2 form \"Filters\"");
    let check = if checked { " checked" } else { "" };
    let _ = writeln!(
        s,
        "      uid={snap_id}_3 checkbox \"Remote\" checkable{check} focusable"
    );
    let _ = writeln!(s, "      uid={snap_id}_4 button \"{button}\" focusable");
    let _ = writeln!(s, "    uid={snap_id}_5 list");
    for i in first..first + items {
        let _ = writeln!(s, "      uid={snap_id}_{} listitem", 10 + i * 3);
        let _ = writeln!(
            s,
            "        uid={snap_id}_{} link \"Lamp {i}\" url=\"https://shop.example.com/p/{i}\"",
            11 + i * 3
        );
        let _ = writeln!(
            s,
            "        uid={snap_id}_{} StaticText \"${i}.00\"",
            12 + i * 3
        );
    }
    s
}

fn baseline(raw: &str, handle: &str) -> Baseline {
    let s = Snapshot::parse(raw, &strict).unwrap();
    let t = s.trim();
    Baseline {
        handle: handle.to_owned(),
        origin_path: origin_path(s.url()),
        lines: t.lines(),
    }
}

fn diff_of(old: &str, new: &str) -> Option<String> {
    let base = baseline(old, "r14");
    let s = Snapshot::parse(new, &strict).unwrap();
    diff(&base, &s.trim(), &s, "r15", 4000)
}

// ---- parse ----

#[test]
fn no_snapshot_section_is_none() {
    assert!(
        Snapshot::parse(
            "Successfully navigated to https://shop.example.com/.",
            &none
        )
        .is_none()
    );
}

#[test]
fn duplicate_uid_keeps_the_strictest_label() {
    // Page text with a line break posing as the real "Pay now" button.
    let raw = "uid=1_0 RootWebArea \"Shop\"\n  uid=1_1 StaticText \"Note:\n  uid=1_2 button \"Search\"\n  more text\"\n  uid=1_2 button \"Pay now\" focusable\n";
    let s = of(raw);
    assert_eq!(s.element_count(), 3);
    let lines = s.trim().lines();
    assert_eq!(lines["1_2"], "uid=1_2 button \"Pay now\"");
    // The losing line is kept as text of the node before it.
    assert!(
        lines["1_1"].contains("uid=1_2 button \"Search\""),
        "{}",
        lines["1_1"]
    );

    // The same the other way round: the fake comes after the real one.
    let raw = "uid=1_0 RootWebArea \"Shop\"\n  uid=1_2 button \"Pay now\" focusable\n  uid=1_1 StaticText \"x\n  uid=1_2 button \"Search\"\"\n";
    let lines = of(raw).trim().lines();
    assert_eq!(lines["1_2"], "uid=1_2 button \"Pay now\"");
    assert!(lines["1_1"].contains("Search"));

    // On a tie the first line wins.
    let raw = "uid=1_0 RootWebArea \"Shop\"\n  uid=1_2 button \"Go\"\n  uid=1_3 link \"Help\"\n  uid=1_2 button \"Stop\"\n";
    let s = Snapshot::parse(&format!("## Latest page snapshot\n{raw}"), &none).unwrap();
    assert_eq!(s.trim().lines()["1_2"], "uid=1_2 button \"Go\"");
}

#[test]
fn names_continue_on_following_lines() {
    let s = of(
        "uid=1_0 RootWebArea \"T\"\n  uid=1_1 button \"Two\nlines\" focusable\n  uid=1_2 link \"Next\"\n",
    );
    let lines = s.trim().lines();
    assert_eq!(lines["1_1"], "uid=1_1 button \"Two lines\"");
    assert_eq!(lines["1_2"], "uid=1_2 link \"Next\"");
}

#[test]
fn ignored_and_noise_nodes_are_see_through() {
    let s = of(concat!(
        "uid=1_0 RootWebArea \"T\"\n",
        "  uid=1_1 ignored\n",
        "    uid=1_2 generic\n",
        "      uid=1_3 button \"Deep\"\n",
        "  uid=1_4 ignored \"Hello\"\n",
        "  uid=1_5 StaticText \"world\"\n",
        "    uid=1_6 InlineTextBox \"world\"\n",
        "  uid=1_7 LineBreak\n",
    ));
    let t = s.trim();
    assert_eq!(
        t.render(),
        "uid=1_0 RootWebArea \"T\"\n  uid=1_3 button \"Deep\"\n  uid=1_4 text \"Hello world\""
    );
    // Merged and see-through uids still lead somewhere.
    assert!(
        t.section("1_6", 10, 1000)
            .unwrap()
            .contains("uid=1_6 is shown as part of uid=1_4")
    );
}

#[test]
fn attributes_parse_in_formatter_order() {
    let s = of(concat!(
        "uid=1_0 RootWebArea \"T\" url=\"https://shop.example.com/\"\n",
        "  uid=1_1 checkbox \"A \"quoted\" name\" checkable checked disableable disabled focusable focused required\n",
        "  uid=1_2 button \"Buy\" disabled\" focusable\n",
        "  uid=1_3 combobox \"Size\" expandable expanded focusable haspopup=\"listbox\" value=\"M\"\n",
        "  uid=1_4 option \"M\" selectable selected value=\"M\"\n",
        "  uid=1_5 heading \"Title\" level=\"2\" [selected in the DevTools Elements panel]\n",
        "  uid=1_6 link \"Out\" url=\"https://news.example.com/a/b?x=1#top\"\n",
        "  uid=1_7 checkbox \"Mixed\" checkable checked=\"mixed\"\n",
    ));
    let lines = s.trim().lines();
    assert_eq!(
        lines["1_1"],
        "uid=1_1 checkbox \"A \"quoted\" name\" checked disabled focused required"
    );
    // A fake attribute inside the name stays in the name.
    assert_eq!(lines["1_2"], "uid=1_2 button \"Buy\" disabled\"");
    assert_eq!(
        lines["1_3"],
        "uid=1_3 combobox \"Size\" expanded value=\"M\""
    );
    assert_eq!(lines["1_4"], "uid=1_4 option \"M\" selected");
    assert_eq!(lines["1_5"], "uid=1_5 heading \"Title\" level=\"2\"");
    assert_eq!(
        lines["1_6"],
        "uid=1_6 link \"Out\" url=\"news.example.com/a/b?x=1\""
    );
    assert_eq!(lines["1_7"], "uid=1_7 checkbox \"Mixed\" checked=\"mixed\"");
    assert_eq!(s.url(), "https://shop.example.com/");
    assert_eq!(s.title(), "T");
}

#[test]
fn pages_and_title_come_from_the_selected_entry() {
    let s = snap("table");
    assert_eq!(s.pages().len(), 1);
    let sel = s.selected().unwrap();
    assert_eq!(sel.id, 1);
    assert_eq!(sel.url, "https://data.example.com/reports/sales?year=2026");
    assert_eq!(s.title(), "Sales report 2026 - Data");
    let s = snap("modal");
    assert_eq!(s.pages()[0].title, "");
    assert_eq!(s.pages()[1].title, "News");
    // No title in the entry: the root's name.
    assert_eq!(s.title(), "Shop");
    assert_eq!(s.url(), "https://shop.example.com/");
    assert!(snap("search").other_text().contains("## Console messages"));
}

#[test]
fn long_names_are_cut_with_a_count() {
    let long = "x".repeat(2034);
    let s = of(&format!(
        "uid=1_0 RootWebArea \"T\"\n  uid=1_1 StaticText \"{long}\"\n"
    ));
    let line = &s.trim().lines()["1_1"];
    assert!(line.ends_with("…\" (+1,834 chars)"), "{line}");
}

// ---- page news and urls ----

#[test]
fn page_news_reads_every_format() {
    let titled = page_news(
        "## Pages\n1: Shop (https://shop.example.com/a) [selected]\n2: https://news.example.com/",
    );
    assert_eq!(
        titled.selected,
        Some((
            1,
            "https://shop.example.com/a".to_owned(),
            "Shop".to_owned()
        ))
    );
    let bare = page_news(
        "## Pages\n1: https://news.example.com/\n3: https://shop.example.com/ [selected] isolatedContext=x",
    );
    assert_eq!(
        bare.selected,
        Some((3, "https://shop.example.com/".to_owned(), String::new()))
    );
    let nav =
        page_news("Successfully navigated to https://shop.example.com/search?q=lamp.\n## Pages\n");
    assert_eq!(
        nav.navigated_to.as_deref(),
        Some("https://shop.example.com/search?q=lamp")
    );
    let clicked = page_news(
        "Successfully clicked on the element\nPage navigated to https://shop.example.com/p/1.",
    );
    assert_eq!(
        clicked.navigated_to.as_deref(),
        Some("https://shop.example.com/p/1")
    );
    let back = page_news("Successfully navigated back to https://shop.example.com/.");
    assert_eq!(
        back.navigated_to.as_deref(),
        Some("https://shop.example.com/")
    );
    let forward = page_news("Successfully navigated forward to https://shop.example.com/p/2.");
    assert_eq!(
        forward.navigated_to.as_deref(),
        Some("https://shop.example.com/p/2")
    );
    assert_eq!(
        page_news("Clicked.\n## Console messages\nPage navigated to https://evil.example.com/."),
        PageNews::default()
    );
    // A title with brackets keeps its url.
    let odd = page_news("## Pages\n2: Lamp (big) (https://shop.example.com/p/1) [selected]");
    assert_eq!(odd.selected.unwrap().2, "Lamp (big)");
    // Page text after the snapshot heading can't fake a page.
    let fake = page_news(
        "## Latest page snapshot\nuid=1_0 RootWebArea\n## Pages\n9: https://evil.example.com/ [selected]",
    );
    assert_eq!(fake.selected, None);
}

#[test]
fn origin_path_drops_scheme_query_and_fragment() {
    assert_eq!(
        origin_path("https://Shop.Example.com/search?q=lamp#top"),
        "shop.example.com/search"
    );
    assert_eq!(origin_path("https://shop.example.com"), "shop.example.com/");
    assert_eq!(
        origin_path("http://sam@shop.example.com:8080/a/b"),
        "shop.example.com:8080/a/b"
    );
}

// ---- trim and render ----

#[test]
fn repeats_fold_and_stay_searchable() {
    let s = Snapshot::parse(&shop(3, 20, 0, false, "Apply"), &strict).unwrap();
    let t = s.trim().with_handle("r3");
    let text = t.render();
    assert!(
        text.contains("… 14 more like the above (uids 3_25 … 3_64); find_in_page(page: \"r3\", …) to search them"),
        "{text}"
    );
    assert!(text.contains("Lamp 4\"") && text.contains("Lamp 19\"") && !text.contains("Lamp 5\""));
    let hit = t
        .find(&query(Some("lamp 12"), &[], None, 20), 1000)
        .unwrap();
    assert!(hit.contains("uid=3_47 link \"Lamp 12\""), "{hit}");
    assert_eq!(t.lines().len(), s.trim().lines().len());
    // Seven in a row hide only one: no fold.
    let s = Snapshot::parse(&shop(3, 7, 0, false, "Apply"), &strict).unwrap();
    assert!(!s.trim().render().contains("more like the above"));
}

#[test]
fn render_within_never_drops_dialogs_or_focus() {
    let s = snap("modal");
    let t = s.trim();
    let (text, omitted) = t.render_within(150, "r9");
    assert!(omitted > 0);
    for uid in ["4_86", "4_87", "4_88", "4_90", "4_95"] {
        assert!(
            text.contains(&format!("uid={uid} ")),
            "{uid} missing:\n{text}"
        );
    }
    assert!(text.ends_with(&format!("[trimmed: {omitted} lines omitted; full page r9]")));
    // Footer and banner go before main's links.
    let (text, _) = snap("search").trim().render_within(900, "r9");
    assert!(!text.contains("uid=3_3672 link \"About\""));
    assert!(text.contains("uid=3_3670 link \"Next\""), "{text}");
    // Small pages come back whole.
    let t = snap("form").trim();
    assert_eq!(t.render_within(4000, "r9"), (t.render(), 0));
}

#[test]
fn render_unfolded_shows_every_item_and_cuts_to_the_cap() {
    let t = snap("search").trim();
    let (full, cut) = t.render_unfolded_within(1_000_000);
    assert!(!cut);
    assert!(!full.contains("more like the above"));
    assert!(full.contains("Desk lamp model 150,"));
    let (small, cut) = t.render_unfolded_within(2000);
    assert!(cut);
    assert!(est(&small) <= 2000);
    assert!(small.starts_with("uid=3_0 RootWebArea"));
}

#[test]
fn output_is_deterministic() {
    for name in PAGES {
        let (a, b) = (snap(name), snap(name));
        let (ta, tb) = (a.trim(), b.trim());
        assert_eq!(ta.render(), tb.render());
        assert_eq!(ta.map(&a, "r1"), tb.map(&b, "r1"));
        assert_eq!(ta.render_within(500, "r1"), tb.render_within(500, "r1"));
        assert_eq!(ta.est_tokens(), est(&ta.render()));
    }
}

/// A 16 MB page parses, trims and maps under the cap, fast.
#[test]
fn sixteen_megabytes_parse_and_map_quickly() {
    let mut raw = String::from(
        "## Pages\n1: Shop (https://shop.example.com/search?q=lamp) [selected]\n## Latest page snapshot\nuid=7_0 RootWebArea \"Shop\"\n  uid=7_1 main\n    uid=7_2 list\n",
    );
    let mut i = 3;
    while raw.len() < 16 * 1024 * 1024 {
        let _ = write!(
            raw,
            "      uid=7_{i} listitem\n        uid=7_{} generic\n          uid=7_{} link \"Desk lamp {i}\" focusable url=\"https://shop.example.com/p/{i}?ref=x\"\n            uid=7_{} StaticText \"Desk lamp {i}\"\n              uid=7_{} InlineTextBox \"Desk lamp {i}\"\n          uid=7_{} StaticText \"$24.00\"\n",
            i + 1,
            i + 2,
            i + 3,
            i + 4,
            i + 5
        );
        i += 6;
    }
    let start = Instant::now();
    let s = Snapshot::parse(&raw, &strict).unwrap();
    let t = s.trim().with_handle("r2");
    let map = t.map(&s, "r2");
    let elapsed = start.elapsed();
    assert!(s.element_count() > 300_000);
    assert!(est(&map) <= 1000, "{}", est(&map));
    assert!(est(&t.render()) < 2000);
    assert!(elapsed.as_secs_f64() < 8.0, "took {elapsed:?}");
}

// ---- map ----

#[test]
fn map_shows_dialogs_on_top_and_no_body_text() {
    let s = snap("modal");
    let map = s.trim().map(&s, "r7");
    let on_top = map.find("On top:").unwrap();
    let dialog = map.find("uid=4_88 dialog \"Sign in to continue\"").unwrap();
    assert!(on_top < dialog && dialog < map.find("Headings:").unwrap());
    assert!(map.contains("Focused: uid=4_90 textbox \"Email\" focused required"));
    assert!(map.contains("  1: https://shop.example.com/ [selected]"));
    assert!(map.ends_with("ask_page(page: \"r7\", question)."));
    let s = snap("search");
    let map = s.trim().map(&s, "r7");
    assert!(!map.contains("Desk lamp model"), "{map}");
    assert!(!map.contains("out of 5 stars"));
}

#[test]
fn map_shrinks_to_the_hard_cap() {
    let mut raw =
        String::from("## Latest page snapshot\nuid=1_0 RootWebArea \"Big form\"\n  uid=1_1 main\n");
    for i in 0..300 {
        let _ = writeln!(
            raw,
            "    uid=1_{} heading \"Section number {i} with a long heading name\" level=\"2\"",
            2 + i * 2
        );
        let _ = writeln!(
            raw,
            "    uid=1_{} textbox \"Field {i} with a long label\" focusable value=\"some value {i}\"",
            3 + i * 2
        );
    }
    let s = Snapshot::parse(&raw, &strict).unwrap();
    let map = s.trim().map(&s, "r4");
    assert!(est(&map) <= 1000, "{}", est(&map));
    assert!(map.contains("more headings") || !map.contains("Headings:"));
    assert!(map.contains("+ "), "{map}");
}

// ---- find ----

#[test]
fn find_all_words_then_any_word() {
    let t = snap("search").trim().with_handle("r12");
    let all = t
        .find(&query(Some("model 250,"), &[], None, 20), 4000)
        .unwrap();
    assert!(
        all.starts_with("1 match in r12 for \"model 250,\""),
        "{all}"
    );
    let both = t
        .find(&query(Some("lamp $24"), &[], None, 20), 4000)
        .unwrap();
    // The listitem holds both words; no single child does.
    assert!(
        both.contains(" listitem (text: \"Desk lamp model 2, "),
        "{both}"
    );
    let any = t
        .find(&query(Some("zebra Terms"), &[], None, 20), 4000)
        .unwrap();
    assert!(
        any.starts_with("1 match in r12 for any word of \"zebra Terms\" (none had all words)"),
        "{any}"
    );
}

#[test]
fn find_filters_by_role_near_and_limit() {
    let t = snap("search").trim().with_handle("r12");
    let boxes = t.find(&query(None, &["checkbox"], None, 20), 4000).unwrap();
    assert!(boxes.starts_with("4 matches"), "{boxes}");
    assert!(
        boxes.contains(
            "checkbox \"$25 to $50\" checked  — main › \"Results for \"lamp\"\" › form \"Filters\""
        ),
        "{boxes}"
    );
    let near_mark = t
        .find(&query(None, &["link"], Some("pagination"), 20), 4000)
        .unwrap();
    assert!(near_mark.starts_with("8 matches"), "{near_mark}");
    let near_head = t
        .find(&query(Some("next"), &[], Some("Results for"), 20), 4000)
        .unwrap();
    assert!(
        near_head.contains("uid=3_3670 link \"Next\""),
        "{near_head}"
    );
    let limited = t
        .find(&query(Some("lamp"), &["link"], None, 5), 4000)
        .unwrap();
    assert_eq!(limited.lines().count(), 7, "{limited}");
    assert!(limited.ends_with("295 more; narrow with role or near"));
    // Section paths: the last 4 parts, outermost first.
    let one = t
        .find(&query(Some("model 250,"), &["link"], None, 20), 4000)
        .unwrap();
    assert!(
        one.contains("  — main › \"Results for \"lamp\"\" › list \"Search results\" › listitem"),
        "{one}"
    );
    // Product page: a heading's section ends at the next heading of its level.
    let p = snap("product").trim();
    let reviews = p
        .find(&query(None, &["button"], Some("Reviews"), 20), 4000)
        .unwrap();
    assert!(reviews.starts_with("8 matches"), "{reviews}");
    let spec = p
        .find(&query(None, &["button"], Some("Specifications"), 20), 4000)
        .unwrap();
    assert!(spec.starts_with("0 matches"), "{spec}");
}

#[test]
fn find_respects_the_cap_and_needs_a_query_or_role() {
    let t = snap("search").trim();
    let capped = t.find(&query(Some("cart"), &[], None, 40), 200).unwrap();
    assert!(est(&capped) <= 200, "{}", est(&capped));
    assert!(capped.ends_with("more; narrow with role or near"));
    assert!(capped.starts_with("301 matches in the page"));
    assert!(t.find(&query(None, &[], None, 20), 4000).is_err());
    assert!(
        t.find(&query(Some("x"), &[], Some("no such heading"), 20), 4000)
            .is_err()
    );
}

// ---- section ----

#[test]
fn section_numbers_lines_and_folds() {
    let t = snap("search").trim().with_handle("r12");
    let sec = t.section("3_61", 150, 4000).unwrap();
    let mut lines = sec.lines();
    assert_eq!(
        lines.next(),
        Some("Section uid=3_61 (list) in r12: 26 lines")
    );
    assert_eq!(lines.next(), Some(" 1  uid=3_61 list \"Search results\""));
    assert!(sec.contains("22    … 294 more like the above"), "{sec}");
    let short = t.section("uid=3_61", 3, 4000).unwrap();
    assert_eq!(short.lines().count(), 5);
    assert!(short.ends_with(
        "[showing lines 1–3 of 26; read_section a child uid, or find_in_page, to see more]"
    ));
    // A folded item reads like any other.
    let hidden = t.section("3_2414", 10, 4000).unwrap();
    assert!(hidden.contains("Desk lamp model 197,"), "{hidden}");
    assert!(
        t.section("9_9", 10, 4000)
            .unwrap_err()
            .contains("no uid=9_9")
    );
    let capped = t.section("3_0", 150, 300).unwrap();
    assert!(est(&capped) <= 300);
}

// ---- diff ----

#[test]
fn diff_shows_a_state_change() {
    let d = diff_of(
        &shop(3, 20, 0, false, "Apply"),
        &shop(3, 20, 0, true, "Apply"),
    )
    .unwrap();
    assert_eq!(
        d,
        "## Page changes since r14 (shop.example.com/search?q=lamp), full page r15\n~ uid=3_3 checkbox \"Remote\" checked  (was: no state)\nunchanged: 65 elements"
    );
}

#[test]
fn diff_gives_up_on_navigation_low_overlap_and_big_change() {
    let old = shop(3, 20, 0, false, "Apply");
    let other = old.replace("search?q=lamp", "cart");
    assert!(diff_of(&old, &other).is_none());
    // Fresh uids: under half seen before.
    assert!(diff_of(&old, &shop(4, 20, 0, false, "Apply")).is_none());
    // Over 40% of the lines changed.
    assert!(diff_of(&old, &shop(3, 20, 13, false, "Apply")).is_none());
    // A diff over the cap gives up too.
    let base = baseline(&old, "r14");
    let s = Snapshot::parse(&shop(3, 20, 0, true, "Apply"), &strict).unwrap();
    assert!(diff(&base, &s.trim(), &s, "r15", 10).is_none());
}

#[test]
fn diff_shows_a_stricter_label_and_never_folds_it() {
    let d = diff_of(
        &shop(3, 20, 0, false, "Search"),
        &shop(3, 20, 0, false, "Pay now"),
    )
    .unwrap();
    assert!(
        d.contains("\n~ uid=3_4 button \"Pay now\"  (was: \"Search\")"),
        "{d}"
    );
}

#[test]
fn diff_folds_added_and_removed_runs() {
    // Five items more at the end, with their parent as context.
    let d = diff_of(
        &shop(3, 20, 0, false, "Apply"),
        &shop(3, 25, 0, false, "Apply"),
    )
    .unwrap();
    assert!(
        d.contains("\n+ 5 like: listitem (uids 3_70 … 3_82); find_in_page(page: \"r15\", …) to see them  (in: list uid=3_5)"),
        "{d}"
    );
    assert!(d.ends_with("unchanged: 66 elements"), "{d}");
    // Five fewer: folded by role groups in uid order.
    let d = diff_of(
        &shop(3, 25, 0, false, "Apply"),
        &shop(3, 20, 0, false, "Apply"),
    )
    .unwrap();
    assert!(d.contains("\n- 5 like: listitem (uids 3_70 … 3_82)"), "{d}");
    // One added item is listed whole.
    let d = diff_of(
        &shop(3, 20, 0, false, "Apply"),
        &shop(3, 21, 0, false, "Apply"),
    )
    .unwrap();
    assert!(
        d.contains("\n+ uid=3_70 listitem  (in: list uid=3_5)\n+   uid=3_71 link \"Lamp 20\""),
        "{d}"
    );
}

/// Measured on the long search-results fixture (300 results, 209 KB):
/// raw snapshot 69,723 tokens, the old 12,000-char preview 4,008 tokens,
/// the page map 394 tokens, the full trimmed page 1,191 tokens.
#[test]
fn measurement_map_vs_old_preview() {
    let raw = fixture("search");
    let s = Snapshot::parse(&raw, &strict).unwrap();
    let t = s.trim();
    let preview: String = raw.chars().take(12_000).collect();
    let (raw_t, preview_t, map_t, full_t) = (
        est(&raw),
        est(&preview),
        est(&t.map(&s, "r12")),
        t.est_tokens(),
    );
    assert_eq!(
        (raw_t, preview_t, map_t, full_t),
        (69_723, 4_008, 394, 1_191)
    );
    assert!(map_t * 10 < preview_t && map_t <= 600);
}
