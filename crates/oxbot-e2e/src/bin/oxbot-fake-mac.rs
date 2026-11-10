//! A stand-in for oxbot's Mac helper (`oxbot-mac`) in end-to-end tests, so
//! no test ever uses the Mac's microphone:
//!
//! - `can-dictate`: yes if `OXBOT_FAKE_DICTATION=1`, else no.
//! - `dictate`: "hears" a sentence in two pieces, then all of it once oxbot
//!   closes its input.
//! - `frames VIDEO OUTDIR [--max N]`: one still frame a second of a
//!   QuickTime/MP4 video (its length read from the file's header), at most
//!   N, each a small grey JPEG. oxbot runs the helper in a sandbox that
//!   lets it start no other program, so this stand-in can't hand video to
//!   the real helper; that one is tested on its own (`oxbot-core`'s
//!   `attach::mac` tests).

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
        Some("frames") => match frames(&args[1..]) {
            Ok((frames, ms)) => say(&format!(r#"{{"duration_ms":{ms},"frames":{frames}}}"#)),
            Err(why) => {
                say(&format!(r#"{{"error":"{why}"}}"#));
                std::process::exit(1);
            }
        },
        _ => {
            say(r#"{"error":"usage: oxbot-mac version | can-dictate | dictate | frames"}"#);
            std::process::exit(1);
        }
    }
}

/// A small grey JPEG, written for every frame.
const FRAME: &[u8] = include_bytes!("../../../../e2e/fixtures/still-frame.jpg");

/// Writes the frames of `VIDEO` into `OUTDIR` (`01.jpg`…), like the real
/// helper: one a second, rounded up, at most `--max` (8). Returns how many
/// and the video's length in ms.
fn frames(args: &[String]) -> Result<(u64, u64), &'static str> {
    let [video, out, rest @ ..] = args else {
        return Err("usage: oxbot-mac frames VIDEO OUTDIR");
    };
    let max = rest
        .iter()
        .position(|a| a == "--max")
        .and_then(|i| rest.get(i + 1)?.parse::<u64>().ok())
        .unwrap_or(8)
        .clamp(1, 30);
    let bytes = std::fs::read(video).map_err(|_| "This video can't be opened.")?;
    let ms = movie_length_ms(&bytes).ok_or("There's no picture in this video.")?;
    let count = ms.div_ceil(1000).clamp(1, max);
    std::fs::create_dir_all(out).map_err(|_| "Couldn't make the frames folder.")?;
    for n in 1..=count {
        std::fs::write(std::path::Path::new(out).join(format!("{n:02}.jpg")), FRAME)
            .map_err(|_| "No still frames could be taken from this video.")?;
    }
    Ok((count, ms))
}

/// A QuickTime/MP4 file's length from its `mvhd` box (version 0 or 1).
fn movie_length_ms(bytes: &[u8]) -> Option<u64> {
    let at = bytes.windows(4).position(|w| w == b"mvhd")? + 4;
    let be = |from: usize, len: usize| -> Option<u64> {
        let b = bytes.get(from..from + len)?;
        Some(b.iter().fold(0u64, |n, x| (n << 8) | u64::from(*x)))
    };
    let (scale, length) = if bytes.get(at)? == &1 {
        (be(at + 20, 4)?, be(at + 24, 8)?)
    } else {
        (be(at + 12, 4)?, be(at + 16, 4)?)
    };
    (scale > 0 && length > 0).then(|| length * 1000 / scale)
}
