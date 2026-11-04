//! "Sign in with `OpenRouter`" (OAuth with PKCE) and the household's key.
//!
//! The Farm asks to sign in; oxbot makes a PKCE verifier and a random
//! `state` (kept in memory only, one at a time, for 10 minutes) and opens
//! `OpenRouter`'s page in the Mac's default browser. `OpenRouter` sends the
//! browser back to `http://localhost:{port}/openrouter/callback` on oxbot's
//! own port (never the Gate door) with a code, which oxbot trades for a
//! key. The key goes into the Keychain ([`crate::keychain`]) and the
//! shared [`KeyCell`] the model clients read, and nowhere else: never a
//! file, a log line, the database, a child's environment or the page.

use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use oxbot_llm::{
    Auth, Catalog, ClientConfig, Endpoint, KeyCell, KeyInfo, OpenAiClient, Pkce, Provider,
    SecretStore, Timeouts, auth_url, exchange_code, random_state,
};

use crate::server::AppState;

/// How long a sign-in may take.
const SIGN_IN_LIFE: Duration = Duration::from_mins(10);
/// The callback's path on oxbot's own port.
pub const CALLBACK_PATH: &str = "/openrouter/callback";
/// The stand-in `OpenRouter`'s address in test mode.
pub const E2E_URL: &str = "OXBOT_OPENROUTER_URL";

/// A sign-in under way.
struct PendingAuth {
    state: String,
    pkce: Pkce,
    made: Instant,
}

/// Why a callback didn't sign in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackError {
    /// No sign-in waiting, too old, or the wrong `state`.
    Stale,
    /// `OpenRouter` sent no code (the person said no).
    NoCode,
    /// `OpenRouter` refused the code, or couldn't be reached.
    Exchange,
    /// The Keychain wouldn't keep the key.
    Keep,
}

impl CallbackError {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::NoCode => "no code",
            Self::Exchange => "exchange",
            Self::Keep => "keychain",
        }
    }
}

/// The household's `OpenRouter` account on this Mac.
pub struct OpenRouter {
    key: KeyCell,
    secrets: Arc<dyn SecretStore>,
    client: OpenAiClient,
    pending: Mutex<Option<PendingAuth>>,
    key_info: RwLock<Option<KeyInfo>>,
    /// Names the key on `OpenRouter`'s site, e.g. `oxbot on Jay's Mac`.
    label: String,
    /// Open the sign-in page in the Mac's browser (not in test mode).
    open_browser: bool,
}

impl std::fmt::Debug for OpenRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouter")
            .field("key", &self.key)
            .field("endpoint", self.client.endpoint())
            .finish_non_exhaustive()
    }
}

/// Where requests go: `OpenRouter`, or in test mode the stand-in named by
/// `OXBOT_OPENROUTER_URL` (this Mac only).
///
/// # Errors
/// The stand-in's address isn't a local one.
pub fn endpoint(e2e: bool) -> Result<Endpoint, oxbot_llm::LlmError> {
    match std::env::var(E2E_URL).ok().filter(|_| e2e) {
        Some(url) => Endpoint::loopback(&url, Provider::OpenRouter),
        None => Ok(Endpoint::openrouter()),
    }
}

impl OpenRouter {
    /// The account for the key kept in `secrets`, talking to `endpoint`.
    /// `label` names the key; `open_browser` opens sign-in pages in the
    /// Mac's browser. The kept key isn't read yet: call
    /// [`OpenRouter::load_kept_key`] (the Keychain may ask the person
    /// first, so nothing waits for it).
    ///
    /// # Errors
    /// The model client couldn't be set up.
    pub fn new(
        endpoint: Endpoint,
        secrets: Arc<dyn SecretStore>,
        label: String,
        open_browser: bool,
    ) -> Result<Self, oxbot_llm::LlmError> {
        let key = KeyCell::loading();
        let client = OpenAiClient::new(ClientConfig {
            endpoint,
            auth: Auth::Key(key.clone()),
            timeouts: Timeouts::default(),
        })?;
        Ok(Self {
            key,
            secrets,
            client,
            pending: Mutex::new(None),
            key_info: RwLock::new(None),
            label,
            open_browser,
        })
    }

    /// Reads the kept key on a blocking thread (the Keychain may show the
    /// person a dialog first) and settles the shared cell. A sign-in or
    /// sign-out in the meantime wins.
    pub async fn load_kept_key(&self) {
        let secrets = Arc::clone(&self.secrets);
        let found = match tokio::task::spawn_blocking(move || secrets.load()).await {
            Ok(Ok(found)) => found,
            Ok(Err(err)) => {
                tracing::warn!(%err, "couldn't read the OpenRouter key");
                None
            }
            Err(err) => {
                tracing::warn!(%err, "reading the OpenRouter key stopped");
                None
            }
        };
        self.key.finish_loading(found);
    }

    /// Whether the kept key is still being read.
    #[must_use]
    pub fn waiting_for_keychain(&self) -> bool {
        self.key.is_loading()
    }

    /// The key the model clients read (shared; set and cleared here).
    #[must_use]
    pub fn key_cell(&self) -> KeyCell {
        self.key.clone()
    }

    /// Where the key is kept.
    #[must_use]
    pub fn secrets(&self) -> Arc<dyn SecretStore> {
        Arc::clone(&self.secrets)
    }

    /// The client for `OpenRouter` (or the stand-in), with the key.
    #[must_use]
    pub fn client(&self) -> &OpenAiClient {
        &self.client
    }

    /// Whether the key is kept in this data folder's key file, not the
    /// Keychain (a dev or test copy).
    #[must_use]
    pub fn key_in_file(&self) -> bool {
        self.secrets.in_file()
    }

    /// Whether a key is kept (signed in).
    #[must_use]
    pub fn signed_in(&self) -> bool {
        self.key.is_set()
    }

    /// The key's limits and use, as last read.
    #[must_use]
    pub fn key_info(&self) -> Option<KeyInfo> {
        self.key_info
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Starts a sign-in that comes back to oxbot on `port`; returns the
    /// page's address (also opened in the Mac's browser). A new sign-in
    /// replaces one still waiting.
    ///
    /// # Errors
    /// The system's random source failed.
    pub fn start_sign_in(&self, port: u16) -> Result<String, oxbot_llm::oauth::RandomError> {
        let pkce = Pkce::new()?;
        let state = random_state()?;
        let callback = format!("http://localhost:{port}{CALLBACK_PATH}");
        let url = auth_url(
            self.client.endpoint(),
            &callback,
            &pkce,
            &state,
            &self.label,
        );
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(PendingAuth {
            state,
            pkce,
            made: Instant::now(),
        });
        tracing::info!("OpenRouter sign-in started");
        if self.open_browser {
            open_in_browser(&url);
        }
        Ok(url)
    }

    /// Finishes the sign-in `state` belongs to, with `code`: trades it for
    /// a key, keeps it, and reads its limits. The waiting sign-in is used
    /// up either way.
    ///
    /// # Errors
    /// See [`CallbackError`].
    pub async fn finish(
        &self,
        code: Option<&str>,
        state: Option<&str>,
    ) -> Result<(), CallbackError> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(pending) = pending else {
            return Err(CallbackError::Stale);
        };
        let fresh = pending.made.elapsed() <= SIGN_IN_LIFE;
        let same = state.is_some_and(|s| crate::secret::same(s, &pending.state));
        if !fresh || !same {
            return Err(CallbackError::Stale);
        }
        let Some(code) = code.filter(|c| !c.is_empty() && c.len() <= 1024) else {
            return Err(CallbackError::NoCode);
        };
        let key = match exchange_code(&self.client, code, &pending.pkce).await {
            Ok(key) => key,
            Err(err) => {
                tracing::warn!(kind = ?err.kind, "OpenRouter refused the sign-in code");
                return Err(CallbackError::Exchange);
            }
        };
        // The Keychain may ask the person first: never on the server's
        // own threads.
        let secrets = Arc::clone(&self.secrets);
        let saved = tokio::task::spawn_blocking(move || secrets.save(&key).map(|()| key)).await;
        let key = match saved {
            Ok(Ok(key)) => key,
            Ok(Err(err)) => {
                tracing::warn!(%err, "couldn't keep the OpenRouter key");
                return Err(CallbackError::Keep);
            }
            Err(err) => {
                tracing::warn!(%err, "keeping the OpenRouter key stopped");
                return Err(CallbackError::Keep);
            }
        };
        self.key.set(key);
        self.refresh_key_info().await;
        Ok(())
    }

    /// Forgets the key: the Keychain item and the shared cell.
    ///
    /// # Errors
    /// The Keychain item couldn't be deleted (the cell is cleared anyway).
    pub async fn sign_out(&self) -> Result<(), oxbot_llm::SecretError> {
        self.key.clear();
        *self
            .key_info
            .write()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
        tracing::info!("signed out of OpenRouter");
        let secrets = Arc::clone(&self.secrets);
        tokio::task::spawn_blocking(move || secrets.delete())
            .await
            .unwrap_or_else(|err| Err(oxbot_llm::SecretError(err.to_string())))
    }

    /// Reads the key's limits and today's use again (nothing if signed out).
    pub async fn refresh_key_info(&self) {
        if !self.signed_in() {
            return;
        }
        match self.client.key_info().await {
            Ok(info) => {
                *self
                    .key_info
                    .write()
                    .unwrap_or_else(PoisonError::into_inner) = Some(info);
            }
            Err(err) => {
                tracing::warn!(kind = ?err.kind, "couldn't read the OpenRouter key's limits");
            }
        }
    }

    /// Reads the model list (needs no key).
    ///
    /// # Errors
    /// Network or format errors.
    pub async fn fetch_models(&self) -> Result<Catalog, oxbot_llm::LlmError> {
        self.client.models().await
    }
}

/// Opens `url` in the Mac's default browser, never the bots' Chrome. The
/// address is one argument to `open`, no shell in between.
fn open_in_browser(url: &str) {
    if let Err(err) = std::process::Command::new("/usr/bin/open")
        .arg("--")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .spawn()
    {
        tracing::warn!(%err, "couldn't open the OpenRouter sign-in page");
    }
}

/// Whether `host` (a `Host` header) is oxbot's own address on `port`: a
/// page on any other name may not finish a sign-in (DNS rebinding).
fn own_host(host: &str, port: u16) -> bool {
    port != 0 && (host == format!("localhost:{port}") || host == format!("127.0.0.1:{port}"))
}

/// A small page for the browser tab, cached nowhere and sending no
/// referrer.
fn page(status: StatusCode, text: &str) -> Response {
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>oxbot</title>\
         <body style=\"font:16px -apple-system,system-ui,sans-serif;margin:48px\"><p>{text}</p></body>"
    );
    (
        status,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::REFERRER_POLICY, "no-referrer"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'",
            ),
        ],
        Html(body),
    )
        .into_response()
}

/// The callback query: only these two are read.
#[derive(serde::Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

/// `GET /openrouter/callback?code=…&state=…`, on oxbot's own port only.
/// Needs no Farm secret (it's a top-level page `OpenRouter` sends the
/// browser to); the single-use `state` decides. Logs only how it ended.
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !own_host(host, state.port) {
        tracing::warn!("refused an OpenRouter sign-in on another address");
        return page(StatusCode::FORBIDDEN, "This isn't oxbot's own address.");
    }
    let Some(account) = state.openrouter.clone() else {
        return page(
            StatusCode::NOT_FOUND,
            "OpenRouter isn't available in this oxbot.",
        );
    };
    match account
        .finish(query.code.as_deref(), query.state.as_deref())
        .await
    {
        Ok(()) => {
            tracing::info!("OpenRouter sign-in finished");
            state.openrouter_changed();
            page(
                StatusCode::OK,
                "Signed in to OpenRouter. You can close this tab.",
            )
        }
        Err(err) => {
            tracing::warn!(why = err.as_str(), "OpenRouter sign-in failed");
            let text = match err {
                CallbackError::Stale => {
                    "This sign-in link isn't valid any more. Start again in oxbot."
                }
                CallbackError::NoCode => "OpenRouter didn't sign you in. Start again in oxbot.",
                CallbackError::Exchange => {
                    "OpenRouter didn't accept this sign-in. Start again in oxbot."
                }
                CallbackError::Keep => {
                    "oxbot couldn't keep the key in the Keychain. Start again in oxbot."
                }
            };
            page(StatusCode::BAD_REQUEST, text)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use oxbot_llm::MemorySecrets;

    fn account() -> OpenRouter {
        let stand_in =
            Endpoint::loopback("http://127.0.0.1:9/api/v1", Provider::OpenRouter).unwrap();
        OpenRouter::new(
            stand_in,
            Arc::new(MemorySecrets::new()),
            "oxbot on test".into(),
            false,
        )
        .unwrap()
    }

    #[test]
    fn only_oxbots_own_address_finishes_a_sign_in() {
        assert!(own_host("localhost:7878", 7878));
        assert!(own_host("127.0.0.1:7878", 7878));
        assert!(!own_host("evil.example:7878", 7878));
        assert!(!own_host("localhost:7879", 7878));
        assert!(!own_host("localhost:0", 0));
    }

    #[tokio::test]
    async fn a_sign_in_link_works_once_and_only_with_its_state() {
        let or = account();
        let url = or.start_sign_in(7878).unwrap();
        assert!(url.starts_with("http://127.0.0.1:9/auth?callback_url=http%3A%2F%2Flocalhost%3A7878%2Fopenrouter%2Fcallback"), "{url}");
        assert!(url.contains("code_challenge_method=S256"));
        let state = url
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();
        assert_eq!(state.len(), 64);
        // The wrong state uses it up.
        assert_eq!(
            or.finish(Some("c"), Some("nope")).await,
            Err(CallbackError::Stale)
        );
        assert_eq!(
            or.finish(Some("c"), Some(&state)).await,
            Err(CallbackError::Stale)
        );
        // A new one: no code is "no", and it's gone after.
        let url = or.start_sign_in(7878).unwrap();
        let state = url
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();
        assert_eq!(
            or.finish(None, Some(&state)).await,
            Err(CallbackError::NoCode)
        );
        assert_eq!(
            or.finish(Some("c"), Some(&state)).await,
            Err(CallbackError::Stale)
        );
        assert!(!or.signed_in());
    }

    #[tokio::test]
    async fn signing_out_clears_the_key_everywhere() {
        let secrets = Arc::new(MemorySecrets::new());
        secrets
            .save(&oxbot_llm::ApiKey::new("sk-or-v1-test-0011223344".into()).unwrap())
            .unwrap();
        let stand_in =
            Endpoint::loopback("http://127.0.0.1:9/api/v1", Provider::OpenRouter).unwrap();
        let or = OpenRouter::new(stand_in, secrets.clone(), "l".into(), false).unwrap();
        or.load_kept_key().await;
        assert!(or.signed_in(), "the kept key is read at start");
        let shared = or.key_cell();
        assert!(!format!("{or:?}").contains("0011223344"));
        or.sign_out().await.unwrap();
        assert!(!shared.is_set() && !or.signed_in());
        assert!(secrets.load().unwrap().is_none());
    }

    /// A Keychain that waits for the person to answer its dialog.
    #[derive(Debug)]
    pub(crate) struct SlowSecrets {
        pub inner: MemorySecrets,
        pub answer: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl SlowSecrets {
        /// The store and the person's answer (send to let it read).
        pub(crate) fn new() -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
            let (tx, rx) = std::sync::mpsc::channel();
            let inner = MemorySecrets::new();
            inner
                .save(&oxbot_llm::ApiKey::new("sk-or-v1-test-slow".into()).unwrap())
                .unwrap();
            (
                Arc::new(Self {
                    inner,
                    answer: std::sync::Mutex::new(rx),
                }),
                tx,
            )
        }
    }

    impl SecretStore for SlowSecrets {
        fn load(&self) -> Result<Option<oxbot_llm::ApiKey>, oxbot_llm::SecretError> {
            let _ = self.answer.lock().unwrap().recv();
            self.inner.load()
        }
        fn save(&self, key: &oxbot_llm::ApiKey) -> Result<(), oxbot_llm::SecretError> {
            self.inner.save(key)
        }
        fn delete(&self) -> Result<(), oxbot_llm::SecretError> {
            self.inner.delete()
        }
    }

    #[tokio::test]
    async fn a_slow_keychain_never_holds_anything_up() {
        let (secrets, answer) = SlowSecrets::new();
        let stand_in =
            Endpoint::loopback("http://127.0.0.1:9/api/v1", Provider::OpenRouter).unwrap();
        let or = Arc::new(OpenRouter::new(stand_in, secrets, "l".into(), false).unwrap());
        assert!(or.waiting_for_keychain() && !or.signed_in());
        let loading = tokio::spawn({
            let or = Arc::clone(&or);
            async move { or.load_kept_key().await }
        });
        // Other work goes on while the dialog waits.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(or.waiting_for_keychain() && !loading.is_finished());
        answer.send(()).unwrap();
        loading.await.unwrap();
        assert!(!or.waiting_for_keychain() && or.signed_in());
    }
}
