//! The store's tests.

use super::*;
use oxbot_claude::{LimitWindow, TurnUsage};
use oxbot_protocol::{EventKind, SignInOutcome};

use crate::bot::personal_herder_id;

#[test]
fn each_person_keeps_their_own_color() {
    let store = Store::open_in_memory().unwrap();
    let (jay, new) = store.person("Jay").unwrap();
    assert!(new);
    let (anu, _) = store.person("Anu").unwrap();
    assert_ne!(jay.color, anu.color);
    assert_eq!(store.person("Jay").unwrap(), (jay.clone(), false));
    assert_eq!(store.people().unwrap(), [jay, anu]);
    let all: std::collections::HashSet<String> = (0..12)
        .map(|i| store.person(&format!("p{i}")).unwrap().0.color)
        .collect();
    assert_eq!(all.len(), 12, "the first 12 people all differ");
}

fn bot(id: &str) -> BotId {
    BotId::new(id).unwrap()
}

#[test]
fn sites_are_cleared_or_trimmed_per_browser() {
    let store = Store::open_in_memory().unwrap();
    for site in ["a.example", "b.example", "c.example"] {
        store.add_site(HOME, site).unwrap();
    }
    store.add_site("p_1", "a.example").unwrap();
    let gone = store
        .keep_only_sites(HOME, &["b.example".to_owned()])
        .unwrap();
    assert_eq!(gone, ["a.example", "c.example"]);
    let left: Vec<_> = store
        .sites(HOME)
        .unwrap()
        .into_iter()
        .map(|s| s.site)
        .collect();
    assert_eq!(left, ["b.example"]);
    assert_eq!(store.clear_sites(HOME).unwrap(), 1);
    assert_eq!(store.sites(HOME).unwrap().len(), 0);
    assert_eq!(
        store.sites("p_1").unwrap().len(),
        1,
        "other browsers keep theirs"
    );
}

#[test]
fn sign_ins_from_before_the_sites_list_are_found_in_the_history() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    {
        // A database from before migration 5, with two sign-ins in it.
        let store = Store::open(&path).unwrap();
        let bot = bot("a");
        for (site, outcome) in [
            ("substack.com", SignInOutcome::Done),
            ("mail.example.com", SignInOutcome::Cancelled),
        ] {
            store
                .append(
                    &bot,
                    EventKind::SignIn {
                        site: site.into(),
                        outcome,
                    },
                )
                .unwrap();
        }
        let conn = store.conn();
        conn.execute_batch("DELETE FROM sites; DROP TABLE gates; PRAGMA user_version = 4;")
            .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let names: Vec<String> = store
        .sites(HOME)
        .unwrap()
        .into_iter()
        .map(|s| s.site)
        .collect();
    assert_eq!(names, ["substack.com"], "only finished sign-ins count");
}

#[test]
fn signed_in_sites_are_kept_until_signed_out() {
    let store = Store::open_in_memory().unwrap();
    store.add_site(HOME, "substack.com").unwrap();
    store.add_site(HOME, "google.com").unwrap();
    store.add_site(HOME, "substack.com").unwrap(); // signing in again updates it
    let names: Vec<String> = store
        .sites(HOME)
        .unwrap()
        .into_iter()
        .map(|s| s.site)
        .collect();
    assert_eq!(names.len(), 2);
    assert!(store.remove_site(HOME, "google.com").unwrap());
    assert!(!store.remove_site(HOME, "google.com").unwrap());
    assert_eq!(store.sites(HOME).unwrap()[0].site, "substack.com");
}

#[test]
fn each_browser_keeps_its_own_sites() {
    let store = Store::open_in_memory().unwrap();
    store.add_site(HOME, "google.com").unwrap();
    store.add_site("Sam", "google.com").unwrap();
    store.add_site("Sam", "bank.example").unwrap();
    assert_eq!(store.sites(HOME).unwrap().len(), 1);
    assert_eq!(store.sites("Sam").unwrap().len(), 2);
    assert!(store.remove_site("Sam", "google.com").unwrap());
    assert_eq!(store.sites(HOME).unwrap().len(), 1, "home keeps its own");
    assert_eq!(store.site_owners().unwrap(), ["", "Sam"]);
}

#[test]
fn a_removed_gate_s_key_stops_working() {
    let store = Store::open_in_memory().unwrap();
    store.add_gate("g1", "Jay", "Pixel", "hash-1").unwrap();
    store.add_gate("g2", "Sam", "Laptop", "hash-2").unwrap();
    assert_eq!(store.gate_by_key("hash-1").unwrap().unwrap().person, "Jay");
    assert!(store.gate_by_key("nope").unwrap().is_none());
    assert_eq!(store.gates().unwrap().len(), 2);
    assert!(store.remove_gate("g1").unwrap());
    assert!(store.gate_by_key("hash-1").unwrap().is_none());
    assert!(!store.remove_gate("g1").unwrap());
    assert!(
        store.add_gate("g3", "Ann", "Phone", "hash-2").is_err(),
        "keys are unique"
    );
}

#[test]
fn a_deleted_bot_leaves_nothing_behind() {
    let store = Store::open_in_memory().unwrap();
    let a = bot("a");
    store
        .append(
            &a,
            EventKind::UserMessage {
                text: "hi".into(),
                from: None,
            },
        )
        .unwrap();
    store
        .append(
            &bot("b"),
            EventKind::UserMessage {
                text: "hi".into(),
                from: None,
            },
        )
        .unwrap();
    store.delete_bot(&a).unwrap();
    assert_eq!(store.last_seq(&a).unwrap(), 0);
    assert_eq!(store.last_seq(&bot("b")).unwrap(), 1);
}

#[test]
fn an_event_this_version_cant_read_doesnt_hide_the_chat() {
    let store = Store::open_in_memory().unwrap();
    let a = bot("a");
    store
        .append(
            &a,
            EventKind::UserMessage {
                text: "hi".into(),
                from: None,
            },
        )
        .unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO events (bot, seq, ts_ms, body) VALUES ('a', 2, 0, '{\"kind\":\"from_the_future\"}')",
            [],
        )
        .unwrap();
    store
        .append(
            &a,
            EventKind::AssistantMessage {
                text: "hello".into(),
            },
        )
        .unwrap();
    let seqs: Vec<u64> = store
        .history(&a, 0)
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(seqs, [1, 3], "the unreadable one is skipped, the rest show");
}

#[test]
fn events_are_numbered_per_bot() {
    let store = Store::open_in_memory().unwrap();
    let a = store
        .append(
            &bot("a"),
            EventKind::UserMessage {
                text: "hi".into(),
                from: None,
            },
        )
        .unwrap();
    let b = store
        .append(
            &bot("b"),
            EventKind::UserMessage {
                text: "yo".into(),
                from: None,
            },
        )
        .unwrap();
    let a2 = store
        .append(
            &bot("a"),
            EventKind::AssistantMessage {
                text: "hello".into(),
            },
        )
        .unwrap();
    assert_eq!((a.seq, b.seq, a2.seq), (1, 1, 2));
    assert_eq!(store.last_seq(&bot("a")).unwrap(), 2);
    assert_eq!(store.last_seq(&bot("c")).unwrap(), 0);
    // Only replies count for "new messages": b has none, a's is #2.
    assert_eq!(store.last_reply_seq(&bot("a")).unwrap(), 2);
    assert_eq!(store.last_reply_seq(&bot("b")).unwrap(), 0);
}

/// `n` events for `bot`: every tenth a reply, the rest tool steps.
fn seed(store: &Store, bot: &BotId, n: u64) {
    for i in 1..=n {
        let kind = if i % 10 == 0 {
            EventKind::AssistantMessage {
                text: format!("reply {i}"),
            }
        } else {
            EventKind::ToolUse {
                name: "Read".into(),
                summary: format!("step {i}"),
            }
        };
        store.append(bot, kind).unwrap();
    }
}

#[test]
fn history_pages_back_to_the_start() {
    let store = Store::open_in_memory().unwrap();
    let a = bot("a");
    seed(&store, &a, 25);
    seed(&store, &bot("b"), 3);
    let latest = store.history_page(&a, None, 10).unwrap();
    let seqs = |p: &HistoryPage| p.events.iter().map(|e| e.seq).collect::<Vec<_>>();
    assert_eq!(seqs(&latest), (16..=25).collect::<Vec<_>>());
    assert_eq!((latest.first_seq, latest.last_seq), (16, 25));
    assert!(latest.has_older);
    let middle = store.history_page(&a, Some(16), 10).unwrap();
    assert_eq!(seqs(&middle), (6..=15).collect::<Vec<_>>());
    assert!(middle.has_older);
    let first = store.history_page(&a, Some(6), 10).unwrap();
    assert_eq!(seqs(&first), (1..=5).collect::<Vec<_>>());
    assert!(!first.has_older);
    // Exactly a page left: nothing older.
    assert!(!store.history_page(&a, Some(11), 10).unwrap().has_older);
    let empty = store.history_page(&bot("c"), None, 10).unwrap();
    assert!(empty.events.is_empty() && !empty.has_older && empty.last_seq == 0);
}

#[test]
fn every_bots_latest_numbers_come_from_the_index() {
    let store = Store::open_in_memory().unwrap();
    seed(&store, &bot("a"), 25);
    seed(&store, &bot("b"), 3);
    let all = store.all_seqs().unwrap();
    assert_eq!(
        all["a"],
        Seqs {
            last: 25,
            last_reply: 20
        }
    );
    assert_eq!(
        all["b"],
        Seqs {
            last: 3,
            last_reply: 0
        }
    );
    assert_eq!(store.event(&bot("a"), 20).unwrap().unwrap().seq, 20);
    assert!(store.event(&bot("a"), 26).unwrap().is_none());
    let plan: String = store
        .conn()
        .query_row(
            "EXPLAIN QUERY PLAN SELECT bot, MAX(seq) FROM events
             WHERE json_extract(body, '$.kind') = 'assistant_message' GROUP BY bot",
            [],
            |r| r.get(3),
        )
        .unwrap();
    assert!(plan.contains("events_replies"), "{plan}");
}

#[test]
fn history_returns_events_after_since() {
    let store = Store::open_in_memory().unwrap();
    for i in 0..3 {
        store
            .append(
                &bot("a"),
                EventKind::UserMessage {
                    text: i.to_string(),
                    from: None,
                },
            )
            .unwrap();
    }
    let after_one = store.history(&bot("a"), 1).unwrap();
    assert_eq!(after_one.iter().map(|e| e.seq).collect::<Vec<_>>(), [2, 3]);
    assert_eq!(
        after_one[0].kind,
        EventKind::UserMessage {
            text: "1".into(),
            from: None
        }
    );
}

#[test]
fn sessions_can_be_saved_replaced_and_forgotten() {
    let store = Store::open_in_memory().unwrap();
    let saved = |id: &str| SavedSession {
        id: id.into(),
        prompt: "p".into(),
    };
    assert_eq!(store.session(&bot("a")).unwrap(), None);
    store.set_session(&bot("a"), Some(&saved("s1"))).unwrap();
    store.set_session(&bot("a"), Some(&saved("s2"))).unwrap();
    assert_eq!(store.session(&bot("a")).unwrap(), Some(saved("s2")));
    store.set_session(&bot("a"), None).unwrap();
    assert_eq!(store.session(&bot("a")).unwrap(), None);
}

#[test]
fn a_sessions_taint_lasts_until_a_new_session() {
    let store = Store::open_in_memory().unwrap();
    let saved = |id: &str| SavedSession {
        id: id.into(),
        prompt: "p".into(),
    };
    store.set_session_taint(&bot("a"), Some("x")).unwrap();
    assert_eq!(store.session_taint(&bot("a")).unwrap(), None, "no session");
    store.set_session(&bot("a"), Some(&saved("s1"))).unwrap();
    assert_eq!(store.session_taint(&bot("a")).unwrap(), None);
    store
        .set_session_taint(&bot("a"), Some("a web page"))
        .unwrap();
    // Resuming saves the same session again: the taint stays.
    store.set_session(&bot("a"), Some(&saved("s1"))).unwrap();
    assert_eq!(
        store.session_taint(&bot("a")).unwrap().as_deref(),
        Some("a web page")
    );
    // A new session starts clean.
    store.set_session(&bot("a"), Some(&saved("s2"))).unwrap();
    assert_eq!(store.session_taint(&bot("a")).unwrap(), None);
    store.set_session_taint(&bot("a"), Some("y")).unwrap();
    store.set_session(&bot("a"), None).unwrap();
    store.set_session(&bot("a"), Some(&saved("s2"))).unwrap();
    assert_eq!(store.session_taint(&bot("a")).unwrap(), None, "forgotten");
}

#[test]
fn an_old_database_is_migrated() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("old.db");
    // The milestone 2 schema, without the prompt column.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE sessions (bot TEXT PRIMARY KEY, claude_session TEXT NOT NULL, updated_ms INTEGER NOT NULL);
             INSERT INTO sessions VALUES ('a', 's1', 0);",
        )
        .unwrap();
    let store = Store::open(&path).unwrap();
    let saved = store.session(&bot("a")).unwrap().unwrap();
    assert_eq!((saved.id.as_str(), saved.prompt.as_str()), ("s1", ""));
    drop(store);
    Store::open(&path).unwrap(); // migrating twice is a no-op
}

/// A database exactly as oxbot left it before people had ids (version
/// 8, made by the real migrations), with people, browser owners and
/// Gates known only by name.
fn version_8_database(path: &Path) {
    Store::open_at_version(path, 8).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "INSERT INTO people (name, color, first_ms) VALUES ('Jay', 3, 100), ('Sam', 1, 200), ('sam', 2, 300);
         INSERT INTO sites (owner, site, signed_in_ms) VALUES
           ('', 'accounts.google.com', 1010), ('', 'google.com', 1005), ('', 'wikipedia.org', 1007),
           ('Sam', 'bank.example', 1020), ('Ann', 'mail.example.com', 1030);
         INSERT INTO gates (id, person, name, key_hash, paired_ms, last_seen_ms) VALUES
           ('g1', 'Ann', 'Pixel', 'hash-1', 1040, 1040), ('g2', 'Lee', 'Laptop', 'hash-2', 1050, 1050);",
    )
    .unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 8);
}

#[test]
fn people_known_by_name_get_ids_and_keep_their_folders_and_herders() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("v8.db");
    version_8_database(&path);

    let store = Store::open(&path).unwrap();
    let people = store.person_records().unwrap();
    let names: Vec<&str> = people.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        ["Jay", "Sam", "sam", "Ann", "Lee"],
        "site owners and Gates' people too"
    );
    for p in &people {
        assert_eq!(PersonId::new(p.id.as_str()).as_ref(), Ok(&p.id));
        // Their Chrome profile stays where it was: no sign-in is lost.
        assert_eq!(p.folder, person_folder(&p.name), "{}", p.name);
        assert!(!p.farm, "chosen later, by the hub");
    }
    let person = |name: &str| people.iter().find(|p| p.name == name).unwrap().clone();
    assert_eq!(person("Jay").color, PEOPLE_COLORS[3], "colors are kept");
    assert_eq!(person("Sam").color, PEOPLE_COLORS[1]);
    assert_eq!(
        person("Ann").color,
        PEOPLE_COLORS[0],
        "new ones get a free color"
    );
    // Their Herders keep their ids; `sam` no longer shares Sam's.
    for name in ["Jay", "Sam", "Ann", "Lee"] {
        assert_eq!(person(name).herder, personal_herder_id(name), "{name}");
    }
    assert_ne!(person("sam").herder, person("Sam").herder);
    assert!(crate::bot::is_personal_herder(
        person("sam").herder.as_str()
    ));

    // Browser owners are ids now, and hosts are main domains.
    let sites = |owner: &str| -> Vec<(String, i64)> {
        store
            .sites(owner)
            .unwrap()
            .into_iter()
            .map(|s| (s.site, s.signed_in_ms))
            .collect()
    };
    assert_eq!(
        sites(""),
        [
            ("google.com".to_owned(), 1010),
            ("wikipedia.org".to_owned(), 1007)
        ]
    );
    assert_eq!(
        sites(person("Sam").id.as_str()),
        [("bank.example".to_owned(), 1020)]
    );
    assert_eq!(
        sites(person("Ann").id.as_str()),
        [("example.com".to_owned(), 1030)]
    );
    assert!(sites("Sam").is_empty(), "never by name again");
    // Checked against Chrome's cookies on the next start.
    assert!(store.site_check_due(HOME).unwrap());
    assert!(store.site_check_due(person("Sam").id.as_str()).unwrap());
    store.site_check_done(HOME).unwrap();
    assert!(!store.site_check_due(HOME).unwrap());

    // Gates point at their person.
    let (gate, id) = store.gate_and_person_by_key("hash-1").unwrap().unwrap();
    assert_eq!((gate.person.as_str(), id), ("Ann", person("Ann").id));
    let (gate, id) = store.gate_and_person_by_key("hash-2").unwrap().unwrap();
    assert_eq!((gate.person.as_str(), id), ("Lee", person("Lee").id));

    // Opening it again changes nothing.
    drop(store);
    let again = Store::open(&path).unwrap();
    assert_eq!(again.person_records().unwrap(), people);
    assert_eq!(
        sites_of(&again, person("Sam").id.as_str()),
        ["bank.example"]
    );
    let version: i64 = again
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(usize::try_from(version).unwrap(), MIGRATIONS.len());
}

fn sites_of(store: &Store, owner: &str) -> Vec<String> {
    store
        .sites(owner)
        .unwrap()
        .into_iter()
        .map(|s| s.site)
        .collect()
}

#[test]
fn people_added_by_an_older_oxbot_get_the_folder_it_would_have_used() {
    let store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO people (name, color, first_ms) VALUES ('Sam', 0, 1)",
            [],
        )
        .unwrap();
    fill_people(&store.conn()).unwrap();
    let sam = store.person_by_name("Sam").unwrap().unwrap();
    assert_eq!(sam.folder, person_folder("Sam"));
    assert_eq!(sam.herder, personal_herder_id("Sam"));
}

#[test]
fn new_people_get_their_id_as_folder_and_never_share_a_herder() {
    let store = Store::open_in_memory().unwrap();
    let (sam, new) = store.find_or_create_person("Sam").unwrap();
    assert!(new);
    assert_eq!(sam.folder, sam.id.as_str());
    assert_eq!(sam.herder, personal_herder_id("Sam"));
    let (other, _) = store.find_or_create_person("sam").unwrap();
    assert_ne!(other.id, sam.id, "names are told apart exactly");
    assert_ne!(other.herder, sam.herder);
    assert_eq!(
        store.find_or_create_person("Sam").unwrap(),
        (sam.clone(), false)
    );

    assert!(!store.rename_person(&other.id, "Sam").unwrap(), "taken");
    assert!(store.rename_person(&sam.id, "Samuel").unwrap());
    let renamed = store.person_by_id(&sam.id).unwrap().unwrap();
    assert_eq!(
        (
            renamed.name.as_str(),
            renamed.folder.as_str(),
            &renamed.herder
        ),
        ("Samuel", sam.folder.as_str(), &sam.herder)
    );
    store.set_farm_person(&other.id).unwrap();
    assert_eq!(store.farm_person().unwrap().unwrap().id, other.id);
}

#[test]
fn history_survives_reopening() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.db");
    Store::open(&path)
        .unwrap()
        .append(
            &bot("a"),
            EventKind::UserMessage {
                text: "kept".into(),
                from: None,
            },
        )
        .unwrap();
    let events = Store::open(&path).unwrap().history(&bot("a"), 0).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn usage_adds_up_and_limits_are_replaced() {
    let store = Store::open_in_memory().unwrap();
    let bot = BotId::new("news").unwrap();
    let turn = TurnUsage {
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: 100,
        cache_write_tokens: 50,
        cost_usd: 0.25,
    };
    store.add_usage(&bot, &turn).unwrap();
    store.add_usage(&bot, &turn).unwrap();
    let usage = store.usage().unwrap();
    assert_eq!(usage.bots.len(), 1);
    assert_eq!((usage.bots[0].turns, usage.bots[0].output_tokens), (2, 10));
    assert!((usage.bots[0].cost_usd - 0.5).abs() < 1e-9);
    assert_eq!(usage.limits, []);

    let window = |name: &str, used: f64, resets_at: i64| LimitWindow {
        name: name.into(),
        used,
        resets_at,
        status: "allowed".into(),
    };
    store
        .set_limits(&[window("seven_day", 0.3, 20), window("five_hour", 0.1, 10)])
        .unwrap();
    store
        .set_limits(&[window("five_hour", 0.2, 10), window("seven_day", 0.4, 20)])
        .unwrap();
    let usage = store.usage().unwrap();
    assert_eq!(usage.limits.len(), 2);
    assert_eq!(
        (usage.limits[0].name.as_str(), usage.limits[0].used),
        ("five_hour", 0.2)
    );
    assert_eq!(usage.limits[1].resets_at_ms, 20_000);
    assert!(usage.limits_seen_ms > 0);
}

#[test]
fn migration_9_derivations_are_frozen() {
    use migrations::{v9_herder_id, v9_herder_id_for, v9_person_folder, v9_site_of};
    let id = PersonId::new("p_0123456789abcdef").unwrap();
    // Worked out with the live functions when migration 9 shipped.
    for (name, folder, herder, herder_for) in [
        (
            "Sam",
            "sam-9836be19",
            "assistant--sam-d5f7dc",
            "assistant--sam-01234567",
        ),
        (
            "sam",
            "sam-822d9e19",
            "assistant--sam-d5f7dc",
            "assistant--sam-01234567",
        ),
        (
            "Ann Lee",
            "ann-lee-0e18607a",
            "assistant--annlee-ca45ea",
            "assistant--annlee-01234567",
        ),
        (
            "",
            "person-cbf29ce4",
            "assistant--222325",
            "assistant--01234567",
        ),
        (
            "Zoë",
            "zo-17a3b76d",
            "assistant--zo-ba3f9c",
            "assistant--zo-01234567",
        ),
        (
            "  Bob  ",
            "bob-f7ab82ba",
            "assistant--bob-4a0a54",
            "assistant--bob-01234567",
        ),
        (
            "[Jay]",
            "jay-d48d1d5f",
            "assistant--jay-a3e089",
            "assistant--jay-01234567",
        ),
        (
            "A very long name that goes on and on beyond",
            "a-very-long-name-that-goes-on-an-00b8033c",
            "assistant--averylongnamethatgoe-0044e4",
            "assistant--averylongnamethatgoe-01234567",
        ),
    ] {
        assert_eq!(v9_person_folder(name), folder, "{name:?}");
        assert_eq!(v9_herder_id(name), herder, "{name:?}");
        assert_eq!(v9_herder_id_for(name, &id), herder_for, "{name:?}");
        // Every frozen id is still a valid bot id.
        assert!(BotId::new(v9_herder_id(name)).is_ok());
        assert!(BotId::new(v9_herder_id_for(name, &id)).is_ok());
    }
    for (host, site) in [
        ("accounts.google.com", "google.com"),
        (".google.com", "google.com"),
        ("www.bbc.co.uk", "bbc.co.uk"),
        ("jay.substack.com", "substack.com"),
        ("mail.example.com", "example.com"),
        ("127.0.0.1", "127.0.0.1"),
        ("localhost", "localhost"),
        ("Example.COM", "example.com"),
        ("router.local", "router.local"),
        ("foo.github.io", "foo.github.io"),
    ] {
        assert_eq!(v9_site_of(host), site, "{host:?}");
    }
}

#[test]
fn a_database_from_a_newer_oxbot_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    drop(Store::open(&path).unwrap());
    let newer = MIGRATIONS.len() + 3;
    Connection::open(&path)
        .unwrap()
        .execute_batch(&format!("PRAGMA user_version = {newer}"))
        .unwrap();
    let err = Store::open(&path).unwrap_err();
    assert!(
        matches!(err, StoreError::Newer { found, known } if found == newer && known == MIGRATIONS.len()),
        "{err:?}"
    );
    assert!(err.to_string().contains("newer oxbot"), "{err}");
    // Nothing was changed: still the newer version.
    let version: i64 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(usize::try_from(version).unwrap(), newer);
}

#[test]
fn the_database_is_copied_before_an_upgrade_rebuilds_tables() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    version_8_database(&path);
    let store = Store::open(&path).unwrap();
    let copy = migrations::backup_path(&path, 8);
    assert_eq!(copy, tmp.path().join("oxbot.db.v8.bak"));
    // The copy is the database as it was: version 8, people by name.
    let old = Connection::open(&copy).unwrap();
    let version: i64 = old
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 8);
    let names: Vec<String> = old
        .prepare("SELECT name FROM people ORDER BY first_ms")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(names, ["Jay", "Sam", "sam"]);
    let has_ids: bool = old
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('people') WHERE name = 'id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!has_ids, "taken before migration 9");
    drop((old, store));

    // An up-to-date database is opened without another copy.
    std::fs::remove_file(&copy).unwrap();
    drop(Store::open(&path).unwrap());
    assert!(!copy.exists());
    // A fresh database has nothing to copy, and neither has one in memory.
    let fresh = tmp.path().join("new.db");
    drop(Store::open(&fresh).unwrap());
    assert!(!migrations::backup_path(&fresh, 0).exists());
    drop(Store::open_in_memory().unwrap());
    let left: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "bak"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_copy_already_there_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    version_8_database(&path);
    let copy = migrations::backup_path(&path, 8);
    std::fs::write(&copy, b"an earlier copy").unwrap();
    drop(Store::open(&path).unwrap());
    assert_eq!(std::fs::read(&copy).unwrap(), b"an earlier copy");
}

#[test]
fn one_person_per_folder_and_herder_and_one_farm() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    // Version 10 had no such rules: two people share a Herder and a
    // folder, and two are the Farm's.
    Store::open_at_version(&path, 10).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "INSERT INTO people (id, name, color, first_ms, folder, herder, farm) VALUES
               ('p_00000000000000a1', 'Sam', 0, 1, 'sam-folder', 'assistant--sam-d5f7dc', 1),
               ('p_00000000000000b2', 'sam', 1, 2, 'sam-folder', 'assistant--sam-d5f7dc', 1);",
        )
        .unwrap();
    let store = Store::open(&path).unwrap();
    let people = store.person_records().unwrap();
    let (first, second) = (&people[0], &people[1]);
    assert_eq!(
        (first.folder.as_str(), first.herder.as_str()),
        ("sam-folder", "assistant--sam-d5f7dc"),
        "the first keeps theirs"
    );
    assert_ne!(second.folder, first.folder);
    assert_ne!(second.herder, first.herder);
    assert!(crate::bot::is_personal_herder(second.herder.as_str()));
    assert_eq!(
        people.iter().filter(|p| p.farm).count(),
        1,
        "one Farm person"
    );
    assert!(first.farm);

    // Choosing another Farm person moves it.
    store.set_farm_person(&second.id).unwrap();
    assert_eq!(store.farm_person().unwrap().unwrap().id, second.id);
    store.set_farm_person(&first.id).unwrap();
    assert_eq!(store.farm_person().unwrap().unwrap().id, first.id);

    let conn = store.conn();
    for bad in [
        "UPDATE people SET farm = 1",
        "UPDATE people SET farm = 2 WHERE name = 'sam'",
        "UPDATE people SET herder = 'assistant--sam-d5f7dc' WHERE name = 'sam'",
        "UPDATE people SET folder = 'sam-folder' WHERE name = 'sam'",
    ] {
        assert!(conn.execute(bad, []).is_err(), "{bad}");
    }
}

#[test]
fn a_person_added_by_an_older_oxbot_never_takes_someone_elses_herder() {
    let store = Store::open_in_memory().unwrap();
    let (sam, _) = store.find_or_create_person("Sam").unwrap();
    // An older oxbot adds `sam` by name only.
    store
        .conn()
        .execute(
            "INSERT INTO people (name, color, first_ms) VALUES ('sam', 1, 2)",
            [],
        )
        .unwrap();
    fill_people(&store.conn()).unwrap();
    let other = store.person_by_name("sam").unwrap().unwrap();
    assert_ne!(other.herder, sam.herder);
    assert_eq!(other.folder, person_folder("sam"), "its old folder is free");
}

#[test]
fn every_bots_latest_numbers_take_a_few_seeks_per_bot() {
    let store = Store::open_in_memory().unwrap();
    seed(&store, &bot("a"), 25);
    seed(&store, &bot("b"), 3);
    seed(&store, &bot("c"), 10);
    let all = store.all_seqs().unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(
        all["c"],
        Seqs {
            last: 10,
            last_reply: 10
        }
    );
    assert!(
        Store::open_in_memory()
            .unwrap()
            .all_seqs()
            .unwrap()
            .is_empty()
    );
    // No step reads the whole table: every lookup is an index search.
    let conn = store.conn();
    let mut stmt = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {}", events::ALL_SEQS))
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map([], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let plan = plan.join("\n");
    assert!(!plan.contains("SCAN e"), "{plan}");
    assert!(plan.contains("events_replies"), "{plan}");
}

#[test]
fn catching_up_sends_the_events_missed_or_only_the_latest_page() {
    let store = Store::open_in_memory().unwrap();
    let a = bot("a");
    seed(&store, &a, 25);
    let seqs = |events: &[BotEvent]| events.iter().map(|e| e.seq).collect::<Vec<_>>();
    // A few missed: each one, in order.
    let Replay::Events(missed) = store.replay(&a, 20, 10).unwrap() else {
        panic!("expected events");
    };
    assert_eq!(seqs(&missed), [21, 22, 23, 24, 25]);
    // Exactly the limit: still events.
    assert!(matches!(store.replay(&a, 15, 10).unwrap(), Replay::Events(e) if e.len() == 10));
    // More than the limit: the latest page, with older ones to ask for.
    let Replay::Page(page) = store.replay(&a, 14, 10).unwrap() else {
        panic!("expected a page");
    };
    assert_eq!(seqs(&page.events), (16..=25).collect::<Vec<_>>());
    assert_eq!(
        (page.first_seq, page.last_seq, page.has_older),
        (16, 25, true)
    );
    // Nothing missed.
    assert_eq!(store.replay(&a, 25, 10).unwrap(), Replay::Events(vec![]));
}

fn rule(site: &str, label: &str) -> RuleKey {
    RuleKey {
        site: site.into(),
        action: "click".into(),
        label: label.into(),
    }
}

#[test]
fn always_allow_rules_come_with_migration_12() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    Store::open_at_version(&path, 11).unwrap();
    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert!(version >= 12);
    assert!(
        !tmp.path().join("oxbot.db.v11.bak").exists(),
        "a new table needs no copy"
    );
    let shop = bot("shop");
    let sam = store.find_or_create_person("Sam").unwrap().0.id;
    let save = rule("www.example.com", "button \"Save\"");
    store.add_allow_rule(&shop, &save, Some(&sam)).unwrap();
    // The same rule twice is kept once, as first set.
    store.add_allow_rule(&shop, &save, None).unwrap();
    let rules = store.allow_rules(&shop).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].by_person.as_ref(), Some(&sam));
    assert_eq!(rules[0].rule.label, "button \"Save\"");
    assert!(store.allow_rule_matches(&shop, &save).unwrap());
    for other in [
        rule("example.com", "button \"Save\""),
        rule("www.example.com", "button \"save\""),
    ] {
        assert!(!store.allow_rule_matches(&shop, &other).unwrap());
    }
    assert!(!store.allow_rule_matches(&bot("other"), &save).unwrap());

    // Removing needs the right bot.
    let id = rules[0].rule.id;
    assert!(!store.remove_allow_rule(&bot("other"), id).unwrap());
    assert!(store.remove_allow_rule(&shop, id).unwrap());
    assert!(!store.allow_rule_matches(&shop, &save).unwrap());

    // Deleting a bot deletes its rules, and only its.
    store.add_allow_rule(&shop, &save, None).unwrap();
    store.add_allow_rule(&bot("other"), &save, None).unwrap();
    store.delete_bot(&shop).unwrap();
    assert_eq!(store.allow_rules(&shop).unwrap(), []);
    assert!(store.allow_rule_matches(&bot("other"), &save).unwrap());
}

#[test]
fn a_version_12_database_gets_sections_and_keeps_its_rules() {
    // A database from the previous release (version 12, with an "Always
    // allow" rule) upgrades to 13 and keeps its rule.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    Store::open_at_version(&path, 12).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "INSERT INTO allow_rules (bot, site, action, label, created_ms)
             VALUES ('shop', 'www.example.com', 'click', 'button \"Save\"', 1);",
        )
        .unwrap();
    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(usize::try_from(version).unwrap(), MIGRATIONS.len());
    let tables: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('sections', 'bot_sections')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 2);
    assert!(
        store
            .allow_rule_matches(&bot("shop"), &rule("www.example.com", "button \"Save\""))
            .unwrap(),
        "the rule from version 12 is kept"
    );
    assert!(
        !tmp.path().join("oxbot.db.v12.bak").exists(),
        "new tables need no copy"
    );
}

#[test]
fn sections_stay_each_persons() {
    let store = Store::open_in_memory().unwrap();
    let (jay, sam) = (PersonId::random(), PersonId::random());
    let kitchen = store
        .create_section(&jay, "Kitchen", Some(&bot("recipes")), 50)
        .unwrap();
    assert!(kitchen.starts_with("s_"));
    let money = store.create_section(&jay, "Money", None, 50).unwrap();
    store.create_section(&sam, "kitchen", None, 50).unwrap();
    assert!(matches!(
        store.create_section(&jay, "KITCHEN", None, 50),
        Err(SectionError::NameTaken)
    ));
    assert!(matches!(
        store.create_section(&jay, "Third", None, 2),
        Err(SectionError::TooMany)
    ));
    assert!(matches!(
        store.rename_section(&jay, &money, "kitchen"),
        Err(SectionError::NameTaken)
    ));
    let names = |p: &PersonId| -> Vec<String> {
        store
            .sections(p)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect()
    };
    assert_eq!(names(&jay), ["Kitchen", "Money"]);
    assert_eq!(names(&sam), ["kitchen"]);

    // Another person's section id is refused, and changes nothing.
    assert!(matches!(
        store.place_bot(&sam, &bot("bank"), Some(&kitchen)),
        Err(SectionError::NoSuchSection)
    ));
    assert!(matches!(
        store.delete_section(&sam, &kitchen),
        Err(SectionError::NoSuchSection)
    ));
    store.place_bot(&sam, &bot("recipes"), None).unwrap();
    store
        .place_bot(&jay, &bot("recipes"), Some(&money))
        .unwrap();
    store.place_bot(&jay, &bot("bank"), Some(&kitchen)).unwrap();
    let sams_kitchen = store.sections(&sam).unwrap()[0].id.clone();
    store
        .place_bot(&sam, &bot("recipes"), Some(&sams_kitchen))
        .unwrap();
    assert_eq!(
        store.placements(&jay).unwrap(),
        [
            ("bank".to_owned(), kitchen.clone()),
            ("recipes".to_owned(), money.clone())
        ],
        "a bot is in one section at most"
    );

    // Deleting a section takes its bots out, and only its own.
    store.delete_section(&jay, &kitchen).unwrap();
    assert_eq!(
        store.placements(&jay).unwrap(),
        [("recipes".to_owned(), money)]
    );
    assert_eq!(store.placements(&sam).unwrap().len(), 1);

    // Deleting a bot takes it out of everyone's sections.
    let mut filed = store.delete_bot(&bot("recipes")).unwrap();
    filed.sort();
    let mut want = vec![jay.clone(), sam.clone()];
    want.sort();
    assert_eq!(filed, want);
    assert_eq!(store.placements(&jay).unwrap().len(), 0);
    assert_eq!(store.placements(&sam).unwrap().len(), 0);
    assert_eq!(store.delete_bot(&bot("recipes")).unwrap().len(), 0);
}

fn a_report(id: &str, title: &str) -> crate::report::Report {
    crate::report::Report {
        id: id.into(),
        created_ms: 0,
        asked_by: None,
        question: "research coffee".into(),
        title: title.into(),
        summary: "Cafe A wins.".into(),
        markdown: "Cafe A [1].".into(),
        sources: vec![crate::report::Source {
            n: 1,
            url: "https://cafe-a.example.com/".into(),
            title: "Cafe A".into(),
            site: "example.com".into(),
            read_ms: 5,
        }],
        links: vec!["https://maps.example.com/".into()],
    }
}

#[test]
fn reports_come_with_migration_14_and_their_card() {
    // A database at version 13 (with sections) upgrades to 14.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("oxbot.db");
    Store::open_at_version(&path, 13).unwrap();
    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert!(version >= 14);
    assert!(
        !tmp.path().join("oxbot.db.v13.bak").exists(),
        "new tables need no copy"
    );
    let cafes = bot("cafes");
    let sam = store.find_or_create_person("Sam").unwrap().0.id;
    store
        .append(
            &cafes,
            EventKind::UserMessage {
                text: "research coffee".into(),
                from: None,
            },
        )
        .unwrap();
    let mut report = a_report("r_1", "Coffee");
    report.asked_by = Some(sam.clone());
    let card = store.add_report(&cafes, &mut report).unwrap();
    assert_eq!(card.seq, 2);
    assert_eq!(
        report.created_ms, card.ts_ms,
        "the card's time is the report's"
    );
    assert_eq!(
        card.kind,
        EventKind::Report {
            id: "r_1".into(),
            title: "Coffee".into(),
            summary: "Cafe A wins.".into(),
            sources: 1
        }
    );
    assert_eq!(store.history(&cafes, 1).unwrap(), [card]);
    assert_eq!(store.report(&cafes, "r_1").unwrap(), Some(report.clone()));
    assert_eq!(
        store.report(&bot("other"), "r_1").unwrap(),
        None,
        "another bot's id finds nothing"
    );
    let mut newer = a_report("r_2", "Tea");
    store.add_report(&cafes, &mut newer).unwrap();
    let list = store.reports(&cafes).unwrap();
    assert_eq!(
        list.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["r_2", "r_1"]
    );
    assert_eq!(list[1].sources, 1);
    assert_eq!(store.all_reports(&cafes).unwrap(), [report, newer]);
    assert!(!store.delete_report(&bot("other"), "r_1").unwrap());
    assert!(store.delete_report(&cafes, "r_1").unwrap());
    assert_eq!(store.report(&cafes, "r_1").unwrap(), None);
}

#[test]
fn opened_pages_belong_to_one_session_and_are_capped() {
    let store = Store::open_in_memory().unwrap();
    let cafes = bot("cafes");
    let session = |id: &str| SavedSession {
        id: id.into(),
        prompt: "p".into(),
    };
    store.set_session(&cafes, Some(&session("s1"))).unwrap();
    store
        .add_session_page(&cafes, "https://cafe-a.example.com/", "Cafe A")
        .unwrap();
    store
        .add_session_page(&cafes, "https://cafe-a.example.com/", "Again")
        .unwrap();
    let pages = store.session_pages(&cafes).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].1.title, "Cafe A", "the first visit counts");
    // Resuming the same session keeps them; a new one starts with none.
    store.set_session(&cafes, Some(&session("s1"))).unwrap();
    assert_eq!(store.session_pages(&cafes).unwrap().len(), 1);
    store.set_session(&cafes, Some(&session("s2"))).unwrap();
    assert_eq!(store.session_pages(&cafes).unwrap(), []);
    store
        .add_session_page(&cafes, "https://cafe-b.example.com/", "Cafe B")
        .unwrap();
    store.set_session(&cafes, None).unwrap();
    assert_eq!(store.session_pages(&cafes).unwrap(), []);

    for i in 0..MAX_SESSION_PAGES + 5 {
        store
            .add_session_page(&cafes, &format!("https://p{i}.example.com/"), "")
            .unwrap();
    }
    let pages = store.session_pages(&cafes).unwrap();
    assert_eq!(pages.len(), MAX_SESSION_PAGES);
    assert_eq!(
        pages[0].0, "https://p5.example.com/",
        "the oldest went first"
    );
}

#[test]
fn deleting_a_bot_deletes_its_reports_and_opened_pages() {
    let store = Store::open_in_memory().unwrap();
    let cafes = bot("cafes");
    store
        .add_report(&cafes, &mut a_report("r_1", "Coffee"))
        .unwrap();
    store
        .add_report(&bot("other"), &mut a_report("r_2", "Tea"))
        .unwrap();
    store
        .add_session_page(&cafes, "https://cafe-a.example.com/", "Cafe A")
        .unwrap();
    store.delete_bot(&cafes).unwrap();
    assert_eq!(store.reports(&cafes).unwrap(), []);
    assert_eq!(store.session_pages(&cafes).unwrap(), []);
    assert_eq!(store.reports(&bot("other")).unwrap().len(), 1);
}
