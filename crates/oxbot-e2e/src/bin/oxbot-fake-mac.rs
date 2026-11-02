//! A stand-in for oxbot's Mac helper (`oxbot-mac`) in end-to-end tests, so
//! no test ever uses the Mac's microphone:
//!
//! - `can-dictate`: yes if `OXBOT_FAKE_DICTATION=1`, else no.
//! - `dictate`: "hears" a sentence in two pieces, then all of it once oxbot
//!   closes its input.
//! - anything else (still frames from video) goes to the real helper in
//!   `OXBOT_REAL_MAC_HELPER`, if there is one.

use std::io::Read as _;

#[allow(clippy::print_stdout, reason = "the helper's answers")]
fn say(line: &str) {
    println!("{line}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("version") => say(r#"{"version":"fake"}"#),
        Some("can-dictate") => {
            if std::env::var("OXBOT_FAKE_DICTATION").is_ok_and(|v| v == "1") {
                say(r#"{"ok":true,"locale":"en_US"}"#);
            } else {
                say(r#"{"ok":false,"why":"On-device dictation isn't available (stand-in)."}"#);
            }
        }
        Some("dictate") => {
            say(r#"{"text":"Remind me","final":false}"#);
            std::thread::sleep(std::time::Duration::from_millis(300));
            say(r#"{"text":"Remind me to call the plumber","final":false}"#);
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
            say(r#"{"text":"Remind me to call the plumber","final":true}"#);
        }
        _ => {
            let real = std::env::var_os("OXBOT_REAL_MAC_HELPER")
                .filter(|p| std::path::Path::new(p).is_file());
            if let Some(real) = real {
                let status = std::process::Command::new(real).args(&args).status();
                std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
            }
            say(r#"{"error":"Taking still frames from video isn't available on this Mac."}"#);
            std::process::exit(1);
        }
    }
}
