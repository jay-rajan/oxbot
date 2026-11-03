//! "Sign in with OpenRouter": OAuth with PKCE (RFC 7636, S256).
//!
//! The server keeps one [`Pkce`] and a [`random_state`] in memory, sends the
//! person to [`auth_url`], and on the callback calls [`exchange_code`],
//! which returns the new key as an [`ApiKey`]. The verifier never leaves
//! memory except in the exchange request, and is wiped when dropped.

use std::fmt;

use reqwest::Url;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::client::{Endpoint, OpenAiClient};
use crate::error::{ErrorKind, LlmError};
use crate::secret::ApiKey;

/// A PKCE verifier and its S256 challenge.
pub struct Pkce {
    verifier: Zeroizing<String>,
    challenge: String,
}

/// The system's random source failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the system's random source failed")]
pub struct RandomError;

impl Pkce {
    /// A fresh verifier: 64 random bytes, base64url (86 characters).
    ///
    /// # Errors
    /// The system's random source failed.
    pub fn new() -> Result<Self, RandomError> {
        let mut bytes = Zeroizing::new([0u8; 64]);
        SystemRandom::new()
            .fill(bytes.as_mut())
            .map_err(|_| RandomError)?;
        Ok(Self::from_verifier(base64url(bytes.as_ref())))
    }

    /// From a known verifier (tests).
    #[must_use]
    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64url(digest(&SHA256, verifier.as_bytes()).as_ref());
        Self {
            verifier: Zeroizing::new(verifier),
            challenge,
        }
    }

    /// The challenge for the sign-in address.
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    fn verifier(&self) -> &str {
        &self.verifier
    }
}

impl fmt::Debug for Pkce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

/// A fresh `state` value: 32 random bytes as hex.
///
/// # Errors
/// The system's random source failed.
pub fn random_state() -> Result<String, RandomError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| RandomError)?;
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 15)]));
    }
    Ok(out)
}

/// The sign-in page address: the service's `/auth` with the callback, the
/// S256 challenge, `state` and a label for the new key.
#[must_use]
pub fn auth_url(
    endpoint: &Endpoint,
    callback_url: &str,
    pkce: &Pkce,
    state: &str,
    key_label: &str,
) -> String {
    let site = endpoint.site_url("auth");
    let Ok(mut url) = Url::parse(&site) else {
        return site;
    };
    url.query_pairs_mut()
        .append_pair("callback_url", callback_url)
        .append_pair("code_challenge", pkce.challenge())
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("key_label", key_label);
    url.into()
}

#[derive(Deserialize)]
struct KeyAnswer {
    key: String,
}

/// Exchanges the callback's `code` for a key (`POST /auth/keys`). Only the
/// key is kept; the rest of the answer (the account id) is dropped.
///
/// # Errors
/// The code was refused (expired, used, or the verifier doesn't match), or
/// network/format errors.
pub async fn exchange_code(
    client: &OpenAiClient,
    code: &str,
    pkce: &Pkce,
) -> Result<ApiKey, LlmError> {
    let mut body = Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({
            "code": code,
            "code_verifier": pkce.verifier(),
            "code_challenge_method": "S256",
        }))
        .map_err(|e| {
            LlmError::new(
                ErrorKind::Protocol,
                format!("The sign-in request couldn't be written: {e}"),
            )
        })?,
    );
    let answer = client
        .post_json("auth/keys", std::mem::take(&mut *body))
        .await?;
    let parsed: KeyAnswer = serde_json::from_slice(&answer).map_err(|_| {
        LlmError::new(
            ErrorKind::Protocol,
            "OpenRouter's sign-in answer had no key.",
        )
    })?;
    ApiKey::new(parsed.key).map_err(|e| {
        LlmError::new(
            ErrorKind::Protocol,
            format!("OpenRouter's sign-in answer had a bad key: {e}."),
        )
    })
}

/// Base64url without padding (RFC 4648 §5), as PKCE uses.
#[must_use]
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Provider;

    #[test]
    fn rfc7636_test_vector() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(
            pkce.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64url_matches_rfc4648_vectors() {
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "Zg"),
            (b"fo", "Zm8"),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg"),
            (b"fooba", "Zm9vYmE"),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (input, expected) in cases {
            assert_eq!(base64url(input), expected);
        }
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn fresh_verifiers_are_long_random_and_url_safe() {
        let a = Pkce::new().unwrap();
        let b = Pkce::new().unwrap();
        assert_eq!(a.verifier().len(), 86);
        assert_ne!(a.verifier(), b.verifier());
        assert!(
            a.verifier()
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
        assert!(!format!("{a:?}").contains(a.verifier()));
        let state = random_state().unwrap();
        assert_eq!(state.len(), 64);
        assert_ne!(state, random_state().unwrap());
    }

    #[test]
    fn auth_url_has_every_parameter_encoded() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        let url = auth_url(
            &Endpoint::openrouter(),
            "http://localhost:4280/openrouter/callback",
            &pkce,
            "abc123",
            "oxbot on Jay's Mac",
        );
        assert_eq!(
            url,
            "https://openrouter.ai/auth?callback_url=http%3A%2F%2Flocalhost%3A4280%2Fopenrouter%2Fcallback\
             &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256\
             &state=abc123&key_label=oxbot+on+Jay%27s+Mac"
        );
        let stand_in =
            Endpoint::loopback("http://127.0.0.1:9/api/v1", Provider::OpenRouter).unwrap();
        assert!(auth_url(&stand_in, "cb", &pkce, "s", "l").starts_with("http://127.0.0.1:9/auth?"));
    }
}
