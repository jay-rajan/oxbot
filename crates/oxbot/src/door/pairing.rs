//! One-time pairing codes: shown on the home computer for one person, good
//! once, for ten minutes. Each device (by its address) gets five tries, then
//! waits ten minutes; the code keeps working for everyone else until
//! [`CODE_TRIES_TOTAL`] wrong tries in all stop it. Pure logic with the time
//! passed in, so every rule can be tested without waiting.

use std::collections::HashMap;
use std::net::IpAddr;

use anyhow::Context as _;
use oxbot_core::id::PersonId;

/// How long a pairing code works.
pub const CODE_LIFE_MS: i64 = 10 * 60 * 1000;
/// Wrong guesses one address may make before it has to wait.
pub const CODE_TRIES: u8 = 5;
/// How long an address that guessed wrong [`CODE_TRIES`] times waits.
pub const BLOCKED_MS: i64 = 10 * 60 * 1000;
/// Wrong guesses from everyone together before a code stops working.
pub const CODE_TRIES_TOTAL: u8 = 30;
/// The most addresses remembered (a home network has far fewer).
const MAX_REMEMBERED: usize = 1024;
/// Letters for codes: no 0/O, 1/I/L, so they're easy to read and type.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// A code showing on the home computer.
#[derive(Debug, Clone)]
struct Showing {
    person: String,
    /// The person's id, if they were known when the code was shown.
    person_id: Option<PersonId>,
    code: String,
    expires_ms: i64,
    /// Wrong guesses still allowed, from everyone together.
    tries_left: u8,
}

/// One address's wrong guesses.
#[derive(Debug, Clone, Copy, Default)]
struct Guesses {
    wrong: u8,
    /// Blocked until then (ms), after [`CODE_TRIES`] wrong guesses.
    blocked_until_ms: i64,
}

/// What a typed code turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub enum Check {
    /// Right: pair the device for this person. The code is used up.
    Paired(String),
    /// No code is showing (none made, used, cancelled or ran out).
    NoCode,
    /// Wrong. `closed` when that was the last try and the code stopped.
    Wrong {
        /// The code stopped working.
        closed: bool,
    },
    /// This address guessed wrong too often: it waits ([`BLOCKED_MS`]),
    /// right code or not.
    Blocked,
}

/// The pairing code on the home computer's screen, if any. At most one at a time.
#[derive(Debug, Default)]
pub struct PairingCodes {
    showing: Option<Showing>,
    /// Wrong guesses by address. Kept across codes, so waiting for a new
    /// code doesn't end a block.
    guesses: HashMap<IpAddr, Guesses>,
}

impl PairingCodes {
    /// Shows a new code for `person`, replacing any other. Returns the code
    /// (`K7QMR3XP`) and when it stops working.
    pub fn show(&mut self, person: &str, now_ms: i64) -> (String, i64) {
        self.show_for(person, None, now_ms)
    }

    /// Like [`PairingCodes::show`], for a person oxbot knows already
    /// (`person_id`), so the device pairs for them whatever their name is by
    /// then.
    pub fn show_for(
        &mut self,
        person: &str,
        person_id: Option<PersonId>,
        now_ms: i64,
    ) -> (String, i64) {
        let code = new_code();
        let expires_ms = now_ms + CODE_LIFE_MS;
        self.showing = Some(Showing {
            person: person.to_owned(),
            person_id,
            code: code.clone(),
            expires_ms,
            tries_left: CODE_TRIES_TOTAL,
        });
        // A new code starts everyone's count again; blocks stay.
        self.guesses.retain(|_, g| g.blocked_until_ms > now_ms);
        (code, expires_ms)
    }

    /// Stops showing the code. Returns whether one was showing.
    pub fn cancel(&mut self) -> bool {
        self.showing.take().is_some()
    }

    /// Whether a code is showing and still good at `now_ms`.
    #[must_use]
    pub fn is_showing(&self, now_ms: i64) -> bool {
        self.showing
            .as_ref()
            .is_some_and(|s| s.expires_ms >= now_ms)
    }

    /// Checks a code typed on the device at `from`, at `now_ms`.
    pub fn check(&mut self, typed: &str, from: IpAddr, now_ms: i64) -> Check {
        self.check_for(typed, from, now_ms).0
    }

    /// Like [`PairingCodes::check`], with the person's id when the code was
    /// shown for someone oxbot knew (see [`PairingCodes::show_for`]).
    pub fn check_for(
        &mut self,
        typed: &str,
        from: IpAddr,
        now_ms: i64,
    ) -> (Check, Option<PersonId>) {
        let Some(showing) = self.showing.as_mut() else {
            return (Check::NoCode, None);
        };
        if showing.expires_ms < now_ms {
            self.showing = None;
            return (Check::NoCode, None);
        }
        if self
            .guesses
            .get(&from)
            .is_some_and(|g| g.blocked_until_ms > now_ms)
        {
            return (Check::Blocked, None);
        }
        if !crate::secret::same(&normalize_code(typed), &showing.code) {
            showing.tries_left = showing.tries_left.saturating_sub(1);
            let closed = showing.tries_left == 0;
            if closed {
                self.showing = None;
            }
            if self.guesses.len() >= MAX_REMEMBERED && !self.guesses.contains_key(&from) {
                self.guesses.retain(|_, g| g.blocked_until_ms > now_ms);
            }
            if self.guesses.len() < MAX_REMEMBERED || self.guesses.contains_key(&from) {
                let guesses = self.guesses.entry(from).or_default();
                guesses.wrong += 1;
                if guesses.wrong >= CODE_TRIES {
                    *guesses = Guesses {
                        wrong: 0,
                        blocked_until_ms: now_ms + BLOCKED_MS,
                    };
                }
            }
            return (Check::Wrong { closed }, None);
        }
        let showing = self.showing.take();
        let (person, id) = showing.map_or_else(Default::default, |s| (s.person, s.person_id));
        (Check::Paired(person), id)
    }
}

/// A new code, e.g. `K7QMR3XP` (shown as `K7QM-R3XP`).
fn new_code() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 8];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes
        .iter()
        .map(|b| char::from(CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()]))
        .collect()
}

/// A typed code, made comparable: capitals, no spaces or dashes.
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// `K7QMR3XP` as people read it: `K7QM-R3XP`.
#[must_use]
pub fn display_code(code: &str) -> String {
    if code.len() == 8 {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_owned()
    }
}

/// The QR code of `link`, as an SVG drawing.
///
/// # Errors
/// Fails if the link is too long for a QR code.
pub fn qr_svg(link: &str) -> anyhow::Result<String> {
    let code = qrcode::QrCode::with_error_correction_level(link, qrcode::EcLevel::M)
        .context("the pairing link is too long for a QR code")?;
    Ok(code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;
    const PHONE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20));
    const OTHER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 21));

    #[test]
    fn codes_are_easy_to_type_and_compare_loosely() {
        let code = new_code();
        assert_eq!(code.len(), 8);
        assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        assert_eq!(normalize_code(" k7qm-r3xp "), "K7QMR3XP");
        assert_eq!(display_code("K7QMR3XP"), "K7QM-R3XP");
    }

    #[test]
    fn a_code_pairs_once_for_its_person() {
        let mut codes = PairingCodes::default();
        let (code, _) = codes.show("Ann", NOW);
        let typed = display_code(&code).to_lowercase();
        assert_eq!(
            codes.check(&typed, PHONE, NOW + 1),
            Check::Paired("Ann".into())
        );
        assert_eq!(codes.check(&code, PHONE, NOW + 2), Check::NoCode);
    }

    #[test]
    fn a_code_shown_for_someone_known_pairs_for_them_by_id() {
        let mut codes = PairingCodes::default();
        let sam = PersonId::random();
        let (code, _) = codes.show_for("Sam", Some(sam.clone()), NOW);
        assert_eq!(
            codes.check_for(&code, PHONE, NOW + 1),
            (Check::Paired("Sam".into()), Some(sam))
        );
        let (code, _) = codes.show("Ann", NOW);
        assert_eq!(
            codes.check_for(&code, PHONE, NOW + 1),
            (Check::Paired("Ann".into()), None),
            "someone new is made when the device pairs"
        );
    }

    #[test]
    fn a_code_runs_out_after_ten_minutes() {
        let mut codes = PairingCodes::default();
        let (code, expires) = codes.show("Ann", NOW);
        assert_eq!(expires, NOW + CODE_LIFE_MS);
        assert!(codes.is_showing(expires));
        assert!(!codes.is_showing(expires + 1));
        assert_eq!(codes.check(&code, PHONE, expires + 1), Check::NoCode);
    }

    #[test]
    fn five_wrong_tries_block_that_device_but_not_the_code() {
        let mut codes = PairingCodes::default();
        let (code, _) = codes.show("Ann", NOW);
        for _ in 0..CODE_TRIES {
            assert_eq!(
                codes.check("AAAA-AAAA", PHONE, NOW),
                Check::Wrong { closed: false }
            );
        }
        assert_eq!(
            codes.check(&code, PHONE, NOW),
            Check::Blocked,
            "even the right one"
        );
        // A new code doesn't end the wait.
        let (code, _) = codes.show("Ann", NOW);
        assert_eq!(codes.check(&code, PHONE, NOW + 1), Check::Blocked);
        // Another device still pairs with it.
        assert_eq!(
            codes.check(&code, OTHER, NOW + 1),
            Check::Paired("Ann".into())
        );
        // Ten minutes on, the device may try again.
        let (code, _) = codes.show("Ann", NOW + BLOCKED_MS);
        assert_eq!(
            codes.check(&code, PHONE, NOW + BLOCKED_MS + 1),
            Check::Paired("Ann".into())
        );
    }

    #[test]
    fn thirty_wrong_tries_in_all_stop_the_code() {
        let mut codes = PairingCodes::default();
        let (code, _) = codes.show("Ann", NOW);
        let mut last = Check::NoCode;
        for i in 0..CODE_TRIES_TOTAL {
            let from = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, i));
            last = codes.check("AAAA-AAAA", from, NOW);
        }
        assert_eq!(last, Check::Wrong { closed: true });
        assert_eq!(
            codes.check(&code, OTHER, NOW),
            Check::NoCode,
            "even the right one"
        );
    }

    #[test]
    fn a_new_code_replaces_the_old_and_cancel_stops_it() {
        let mut codes = PairingCodes::default();
        let (old, _) = codes.show("Ann", NOW);
        let (new, _) = codes.show("Sam", NOW);
        if old != new {
            assert_eq!(
                codes.check(&old, PHONE, NOW),
                Check::Wrong { closed: false }
            );
        }
        assert!(codes.cancel());
        assert!(!codes.cancel());
        assert_eq!(codes.check(&new, PHONE, NOW), Check::NoCode);
    }

    #[test]
    fn the_qr_code_is_an_svg() {
        assert!(qr_svg("oxbot://pair?c=K7QM").unwrap().starts_with("<?xml"));
    }
}
