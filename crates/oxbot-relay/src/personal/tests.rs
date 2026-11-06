use std::sync::Arc;

use serde_json::json;

use super::detect::luhn;
use super::*;

fn redactor() -> Redactor {
    Redactor::new(vec![
        Known::person("Ann Lee"),
        Known::person("Sam"),
        Known::place("Toronto, ON"),
    ])
}

/// `text` redacted by a fresh redactor that knows no names.
fn plain(text: &str) -> String {
    Redactor::new(Vec::new()).redact(text)
}

/// Asserts `text` is left as it is.
fn kept(text: &str) {
    assert_eq!(plain(text), text, "changed: {text}");
}

#[test]
fn emails_are_caught_but_not_image_names() {
    assert_eq!(
        plain("Write to ann.lee+jobs@example.co.uk today."),
        "Write to [email 1] today."
    );
    assert_eq!(plain("a@b.io, A@B.IO"), "[email 1], [email 1]");
    kept("<img src=\"logo@2x.png\"> and @types/node and user@localhost");
}

#[test]
fn north_american_phone_numbers_in_every_usual_shape() {
    for phone in [
        "416 555 0100",
        "416-555-0100",
        "(416) 555-0100",
        "416.555.0100",
        "+1 416 555 0100",
        "1-416-555-0100",
        "+1-416-555-0100",
    ] {
        assert_eq!(
            plain(&format!("Call {phone}.")),
            "Call [phone 1].",
            "{phone}"
        );
    }
    // Ten bare digits only when they're called a phone number.
    assert_eq!(plain("phone: 4165550100"), "phone: [phone 1]");
    kept("Order 4165550100 shipped");
    // The same number written two ways is one stand-in.
    assert_eq!(
        plain("416-555-0100 or +1 (416) 555-0100"),
        "[phone 1] or [phone 1]"
    );
}

#[test]
fn international_phone_numbers() {
    assert_eq!(plain("UK: +44 20 7946 0958"), "UK: [phone 1]");
    assert_eq!(plain("India +91 98765 43210."), "India [phone 1].");
    assert_eq!(plain("tel:+14165550100"), "tel:[phone 1]");
}

#[test]
fn prices_dates_times_and_versions_are_not_phone_numbers() {
    kept("It costs $1,234.56 or 1234.56 CAD, due 2026-11-06 at 10:30.");
    kept("Version 2.416.555.0100 and build 416-555-0100-2 and v2.3.4");
    kept("Between 9:00 and 17:00, 12/25/2026, 3 items, 100 km");
    kept("ms 1730000000000 and id 17300000000001234");
    kept("uid=1_3 button \"Post\" and 123-456-7890"); // 123 isn't an area code
}

#[test]
fn luhn_works() {
    let d = |s: &str| s.bytes().map(|b| b - b'0').collect::<Vec<_>>();
    assert!(luhn(&d("4111111111111111")));
    assert!(luhn(&d("046454286")));
    assert!(!luhn(&d("4111111111111112")));
    assert!(!luhn(&d("5")));
}

#[test]
fn card_numbers_pass_luhn_and_look_like_cards() {
    for card in [
        "4111 1111 1111 1111",
        "4111-1111-1111-1111",
        "4111111111111111",
        "5555 5555 5555 4444",
        "3782 822463 10005",
    ] {
        assert_eq!(
            plain(&format!("Card {card} ok")),
            "Card [card 1] ok",
            "{card}"
        );
    }
    // Fails Luhn, mixed grouping, no card prefix, an order number.
    kept("4111 1111 1111 1112");
    kept("4111-1111 1111-1111");
    kept("Order #112-3456789-1234567 and tracking 1Z999AA10123456784");
    kept("Ticket 9111111111111111");
}

#[test]
fn government_ids() {
    assert_eq!(plain("SSN 123-45-6789"), "SSN [id number 1]");
    kept("Code 000-12-3456 and 666-12-3456 and 900-12-3456");
    assert_eq!(plain("My SIN is 046 454 286."), "My SIN is [id number 1].");
    assert_eq!(plain("sin 046-454-286"), "sin [id number 1]");
    kept("046 454 287 is not one");
    assert_eq!(
        plain("social security number: 123456789"),
        "social security number: [id number 1]"
    );
}

#[test]
fn account_numbers_when_called_that() {
    assert_eq!(
        plain("Account number: 0012345678, transit 12345"),
        "Account number: [account 1], transit [account 2]"
    );
    assert_eq!(plain("acct #98765432"), "acct #[account 1]");
    assert_eq!(
        plain("IBAN GB82 WEST 1234 5698 7654 32 please"),
        "IBAN [account 1] please"
    );
    assert_eq!(plain("pay GB82WEST12345698765432"), "pay [account 1]");
    kept("account settings, 3 accounts, checking 2 items");
    kept("GB82 WEST 1234 5698 7654 33");
}

#[test]
fn street_addresses_and_postal_codes() {
    assert_eq!(plain("I live at 12 Elm St."), "I live at [address 1]");
    assert_eq!(
        plain("Ship to 1203-55 Bloor Street West, Toronto ON M5V 2T6"),
        "Ship to [address 1], Toronto ON [postal code 1]"
    );
    assert_eq!(
        plain("350 5th Ave, Apt 4B, New York, NY 10118"),
        "[address 1], New York, NY [postal code 1]"
    );
    assert_eq!(plain("PO Box 42"), "[address 1]");
    assert_eq!(plain("zip 90210-1234"), "zip [postal code 1]");
    // Street-like words in normal text.
    kept("Only 10 minutes drive from 3 great parks; Step 2 is easy.");
    kept("We sold 12 elm trees on the main street, 5 Ways to cook.");
    kept("He scored 90210 points");
}

#[test]
fn known_names_by_whole_word_and_capital() {
    let mut r = redactor();
    assert_eq!(
        r.redact("Ann Lee and Sam's dog met Ann in Toronto, ON."),
        "[person 1] and [person 2]'s dog met [person 3] in [place 1]."
    );
    // The same stand-ins next time; `Toronto` alone is its own.
    assert_eq!(
        r.redact("SAM, Lee and ann went to Toronto"),
        "[person 2], [person 4] and ann went to [place 2]"
    );
    // Not inside other words.
    assert_eq!(r.redact("Samsung, Annex, Leeds"), "Samsung, Annex, Leeds");
}

#[test]
fn overlapping_finds_keep_the_better_one() {
    let mut r = redactor();
    // An email holding a name is one email.
    assert_eq!(r.redact("sam@example.com"), "[email 1]");
    // An address with a name in it is one address.
    assert_eq!(r.redact("12 Sam Street"), "[address 1]");
}

#[test]
fn text_that_reads_like_a_stand_in_never_clashes() {
    let mut r = Redactor::new(Vec::new());
    // Not used yet: it means itself.
    assert_eq!(
        r.redact("the page says [email 1]"),
        "the page says [email 1]"
    );
    assert_eq!(r.redact("a@b.io"), "[email 2]");
    assert_eq!(r.restore("[email 1] [email 2]"), "[email 1] a@b.io");
    // Already used for something else: it gets a new one.
    let mut r = Redactor::new(Vec::new());
    assert_eq!(r.redact("a@b.io"), "[email 1]");
    assert_eq!(
        r.redact("then [Email 1] literally"),
        "then [email 2] literally"
    );
    assert_eq!(r.restore("[email 1] / [email 2]"), "a@b.io / [Email 1]");
}

#[test]
fn restore_round_trips_and_leaves_unknown_stand_ins() {
    let mut r = redactor();
    let text = "Ann Lee (ann@x.com, 416-555-0100) lives at 12 Elm St, Toronto, ON M5V 2T6; card 4111 1111 1111 1111.";
    let sent = r.redact(text);
    for real in ["Ann", "ann@x.com", "416", "Elm", "Toronto", "M5V", "4111"] {
        assert!(!sent.contains(real), "{real} left: {sent}");
    }
    assert_eq!(r.restore(&sent), text);
    assert_eq!(
        r.restore("[email 9] and [Email 1]"),
        "[email 9] and ann@x.com"
    );
}

#[test]
fn stand_ins_survive_being_saved() {
    let mut r = redactor();
    r.redact("a@b.io and 416-555-0100 and Sam");
    let json = r.stand_ins().to_json();
    let back = StandIns::from_json(&json).unwrap();
    assert_eq!(&back, r.stand_ins());
    let mut again = Redactor::resume(vec![Known::person("Sam")], back);
    assert_eq!(
        again.redact("Sam: c@d.io, a@b.io"),
        "[person 1]: [email 2], [email 1]"
    );
    assert!(StandIns::from_json("nope").is_err());
}

#[test]
fn a_request_is_redacted_in_every_text_field_but_not_its_ids() {
    let mut r = redactor();
    let mut req = json!({
        "instructions": "You help Sam.",
        "input": [
            {"type": "message", "role": "user",
             "content": [{"type": "input_text", "text": "[Sam] email ann@x.com"}]},
            {"type": "function_call", "id": "fc_416-555-0100", "call_id": "call_1",
             "name": "type_text", "arguments": "{\"text\":\"ann@x.com\",\"who\":\"O\\\"Brien Sam\"}"},
            {"type": "function_call_output", "call_id": "call_1",
             "output": "Page: Ann Lee, 12 Elm St"},
            {"type": "reasoning", "encrypted_content": "Sam ann@x.com",
             "summary": [{"type": "summary_text", "text": "Sam wants mail"}]}
        ],
        "tools": [{"type": "function", "name": "t", "description": "for Sam"}]
    });
    r.redact_request(req.as_object_mut().unwrap());
    assert_eq!(req["instructions"], "You help [person 1].");
    assert_eq!(
        req["input"][0]["content"][0]["text"],
        "[[person 1]] email [email 1]"
    );
    let args: Value = serde_json::from_str(req["input"][1]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(
        args,
        json!({"text": "[email 1]", "who": "O\"Brien [person 1]"})
    );
    assert_eq!(req["input"][1]["id"], "fc_416-555-0100");
    assert_eq!(req["input"][1]["name"], "type_text");
    assert_eq!(req["input"][2]["output"], "Page: [person 2], [address 1]");
    assert_eq!(req["input"][3]["encrypted_content"], "Sam ann@x.com");
    assert_eq!(
        req["input"][3]["summary"][0]["text"],
        "[person 1] wants mail"
    );
    // Tools are oxbot's own.
    assert_eq!(req["tools"][0]["description"], "for Sam");
}

#[test]
fn a_whole_answer_is_restored_with_arguments_still_json() {
    let mut r = Redactor::new(vec![Known::person("Ann \"Annie\" Lee")]);
    r.redact("Ann \"Annie\" Lee, a@b.io");
    let body = json!({"output": [
        {"type": "message", "content": [{"type": "output_text", "text": "Hi [person 1], [email 1]"}]},
        {"type": "function_call", "name": "fill", "arguments": "{\"to\":\"[person 1] <[email 1]>\"}"}
    ]});
    let back: Value =
        serde_json::from_slice(&r.stand_ins().restore_body(body.to_string().as_bytes())).unwrap();
    assert_eq!(
        back["output"][0]["content"][0]["text"],
        "Hi Ann \"Annie\" Lee, a@b.io"
    );
    let args: Value =
        serde_json::from_str(back["output"][1]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args["to"], "Ann \"Annie\" Lee <a@b.io>");
    assert_eq!(
        r.stand_ins().restore_body(b"plain [email 1]"),
        b"plain a@b.io"
    );
}

/// A stream of these events, as the provider sends them.
fn sse(events: &[Value]) -> String {
    events.iter().fold(String::new(), |mut out, e| {
        use std::fmt::Write as _;
        let _ = write!(out, "event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap());
        out
    })
}

/// The events Codex got.
fn events(out: &str) -> Vec<Value> {
    out.split("\n\n")
        .filter(|f| !f.trim().is_empty())
        .map(|f| {
            let data = f.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
            serde_json::from_str(data).unwrap()
        })
        .collect()
}

/// All of a text's deltas, joined.
fn joined(events: &[Value], kind: &str) -> String {
    events
        .iter()
        .filter(|e| e["type"] == kind)
        .map(|e| e["delta"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_stream_is_restored_however_it_is_split() {
    let mut r = Redactor::new(vec![Known::person("Ann"), Known::person("O\"Neil")]);
    assert_eq!(
        r.redact("Ann a@b.io O\"Neil"),
        "[person 1] [email 1] [person 2]"
    );
    let stand_ins = Arc::new(r.stand_ins().clone());
    let text_deltas = [
        "Write to [em",
        "ail 1] for [",
        "person 1",
        "]. Done [",
        "x] [",
    ];
    let arg_deltas = ["{\"to\":\"[email", " 1]\",\"n\":\"[person 2]\"}"];
    let mut evs = vec![json!({"type": "response.created", "response": {"id": "r"}})];
    for d in text_deltas {
        evs.push(json!({"type": "response.output_text.delta", "item_id": "m1", "content_index": 0, "delta": d}));
    }
    for d in arg_deltas {
        evs.push(
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc1", "delta": d}),
        );
    }
    evs.push(
        json!({"type": "response.output_item.done", "item": {"type": "function_call",
        "arguments": "{\"to\":\"[person 2]\"}"}}),
    );
    evs.push(
        json!({"type": "response.completed", "response": {"output": [
        {"type": "message", "content": [{"type": "output_text", "text": "Write to [email 1]"}]}]}}),
    );
    let stream = sse(&evs);
    for split in [1, 2, 3, 7, 50, stream.len()] {
        let mut restorer = SseRestorer::new(Arc::clone(&stand_ins));
        let mut out = Vec::new();
        for chunk in stream.as_bytes().chunks(split) {
            out.extend(restorer.push(chunk));
        }
        out.extend(restorer.finish());
        let out = String::from_utf8(out).unwrap();
        let got = events(&out);
        assert_eq!(
            joined(&got, "response.output_text.delta"),
            "Write to a@b.io for Ann. Done [x] [",
            "split {split}"
        );
        let args: Value =
            serde_json::from_str(&joined(&got, "response.function_call_arguments.delta")).unwrap();
        assert_eq!(args, json!({"to": "a@b.io", "n": "O\"Neil"}));
        let done = got
            .iter()
            .find(|e| e["type"] == "response.output_item.done")
            .unwrap();
        let args: Value =
            serde_json::from_str(done["item"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(args["to"], "O\"Neil");
        assert_eq!(
            got.last().unwrap()["response"]["output"][0]["content"][0]["text"],
            "Write to a@b.io"
        );
        assert!(!out.contains("[email"), "{out}");
        assert!(out.contains("event: response.output_text.delta\ndata: "));
    }
}

#[test]
fn a_stream_passes_what_it_cant_read() {
    let r = Redactor::new(Vec::new());
    let mut restorer = SseRestorer::new(Arc::new(r.stand_ins().clone()));
    let mut out =
        restorer.push(b": keep-alive\n\ndata: [DONE]\n\nevent: x\r\ndata: {\"a\":1}\r\n\r\n");
    out.extend(restorer.finish());
    let out = String::from_utf8(out).unwrap();
    assert!(out.starts_with(": keep-alive\n\ndata: [DONE]\n\n"), "{out}");
    assert!(out.contains("event: x\ndata: {\"a\":1}\n\n"), "{out}");
}
