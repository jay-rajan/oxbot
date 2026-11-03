//! Errors from model services, sorted into kinds the router acts on.

use std::time::Duration;

use serde_json::Value;

/// What went wrong with a model request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Couldn't connect, or the connection broke.
    Network,
    /// No answer in time (connect, first byte, between chunks, or overall).
    Timeout,
    /// Too many requests for now (this model or the account).
    RateLimited,
    /// OpenRouter's free daily requests are used up.
    FreeDailyLimit,
    /// The model or its providers are down or busy.
    Overloaded,
    /// No provider meets the request's routing rules (e.g. parameters).
    NoProvider,
    /// No provider meets the data policy: the request's, or the account's
    /// privacy settings on OpenRouter's site ("No endpoints found matching
    /// your data policy").
    DataPolicy,
    /// The conversation is longer than the model's window.
    ContextLength,
    /// The request was refused as invalid (e.g. tool schema, parameters).
    BadRequest,
    /// The model doesn't exist (any more).
    NotFound,
    /// The key was refused.
    Unauthorized,
    /// There is no key: OpenRouter isn't signed in.
    NotSignedIn,
    /// Out of credits, or the key's limit is reached.
    PaymentRequired,
    /// A spending budget or guardrail limit is reached.
    Budget,
    /// The provider refused the content (moderation, policy, refusal).
    Refused,
    /// The service failed on its side.
    Server,
    /// The answer didn't follow the protocol (bad stream, bad JSON).
    Protocol,
    /// The request was cancelled by oxbot.
    Cancelled,
    /// The client is set up wrongly (e.g. a base URL that isn't allowed).
    Config,
}

/// What the caller should do about an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RetryClass {
    /// Try the same model again after a short wait.
    RetrySame,
    /// Cool this model down and try the next one.
    NextModel,
    /// Shorten the conversation, then retry once.
    Compact,
    /// Stop using free models until the daily reset.
    StopFreeToday,
    /// The owner must sign in to OpenRouter again.
    SignInAgain,
    /// Out of credits or over a budget: stop.
    OutOfCredits,
    /// The provider refused the content: stop this turn, don't retry
    /// elsewhere.
    Refused,
    /// Cancelled: nothing to do.
    Cancelled,
    /// Give up.
    Fail,
}

impl ErrorKind {
    /// What to do about this kind of error.
    #[must_use]
    pub fn retry_class(self) -> RetryClass {
        match self {
            Self::Network | Self::Timeout | Self::Server | Self::Protocol => RetryClass::RetrySame,
            Self::RateLimited
            | Self::Overloaded
            | Self::NoProvider
            | Self::DataPolicy
            | Self::NotFound
            | Self::BadRequest => RetryClass::NextModel,
            Self::ContextLength => RetryClass::Compact,
            Self::FreeDailyLimit => RetryClass::StopFreeToday,
            Self::Unauthorized | Self::NotSignedIn => RetryClass::SignInAgain,
            Self::PaymentRequired | Self::Budget => RetryClass::OutOfCredits,
            Self::Refused => RetryClass::Refused,
            Self::Cancelled => RetryClass::Cancelled,
            Self::Config => RetryClass::Fail,
        }
    }

    /// A short name for logs and the meter, e.g. `rate_limited`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::RateLimited => "rate_limited",
            Self::FreeDailyLimit => "daily_limit",
            Self::Overloaded => "busy",
            Self::NoProvider => "no_provider",
            Self::DataPolicy => "data_policy",
            Self::ContextLength => "context_length",
            Self::BadRequest => "bad_request",
            Self::NotFound => "not_found",
            Self::Unauthorized => "unauthorized",
            Self::NotSignedIn => "not_signed_in",
            Self::PaymentRequired => "payment_required",
            Self::Budget => "budget",
            Self::Refused => "refused",
            Self::Server => "server",
            Self::Protocol => "protocol",
            Self::Cancelled => "cancelled",
            Self::Config => "config",
        }
    }
}

/// An error from a model service. Never contains the API key.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct LlmError {
    /// What kind of error.
    pub kind: ErrorKind,
    /// The HTTP status, or the status-like code of a mid-stream error.
    pub status: Option<u16>,
    /// How long the service asked us to wait (`Retry-After`).
    pub retry_after: Option<Duration>,
    /// A short, single-line description (the service's own words when it
    /// gave some).
    pub message: String,
    /// Whether part of the answer had already arrived (and should be
    /// thrown away).
    pub partial: bool,
}

/// Longest message kept from a service's error.
const MAX_MESSAGE: usize = 300;

impl LlmError {
    /// An error of this kind with this message.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
            message: tidy(&message.into()),
            partial: false,
        }
    }

    /// The request was cancelled.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "Stopped.")
    }

    /// What to do about it.
    #[must_use]
    pub fn retry_class(&self) -> RetryClass {
        self.kind.retry_class()
    }

    /// Marks it as happening after part of the answer arrived.
    #[must_use]
    pub fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// An error answer from the service: its HTTP status, `Retry-After`,
    /// and body (OpenRouter's `{"error": {"code", "message", "metadata"}}`,
    /// or anything else).
    #[must_use]
    pub fn from_response(status: u16, retry_after: Option<Duration>, body: &str) -> Self {
        let parsed: Option<Value> = serde_json::from_str(body).ok();
        let error = parsed.as_ref().and_then(|v| v.get("error"));
        let mut err = if let Some(e) = error {
            Self::from_error_object(Some(status), e)
        } else {
            let text = if body.trim().is_empty() {
                format!("The model service answered with HTTP {status}.")
            } else {
                body.to_owned()
            };
            let kind = classify(Some(status), None, &text, false);
            Self::new(kind, text)
        };
        err.status = Some(status);
        err.retry_after = retry_after;
        err
    }

    /// A mid-stream `error` object (the HTTP status was 200). Its `code`
    /// is the status the error would have had.
    #[must_use]
    pub fn from_stream_error(error: &Value) -> Self {
        let code = error
            .get("code")
            .and_then(Value::as_u64)
            .and_then(|c| u16::try_from(c).ok());
        let mut err = Self::from_error_object(code, error);
        err.status = code;
        err.partial = true;
        err
    }

    fn from_error_object(status: Option<u16>, error: &Value) -> Self {
        let meta = error.get("metadata");
        let error_type = meta
            .and_then(|m| m.get("error_type"))
            .or_else(|| error.get("type"))
            .and_then(Value::as_str)
            .or_else(|| error.get("code").and_then(Value::as_str));
        let has_limit_source = meta.and_then(|m| m.get("limit_source")).is_some();
        let mut message = match error.get("message") {
            Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
            Some(v) if !v.is_null() => v.to_string(),
            _ => error.as_str().map_or_else(
                || "The model service refused the request.".to_owned(),
                str::to_owned,
            ),
        };
        let kind = classify(status, error_type, &message, has_limit_source);
        if kind == ErrorKind::Refused {
            // Reasons only; the flagged text itself is the person's own.
            let reasons: Vec<&str> = meta
                .and_then(|m| m.get("reasons"))
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !reasons.is_empty() {
                message = format!("{message} ({})", reasons.join(", "));
            }
        }
        Self::new(kind, message)
    }
}

/// Sorts an error by OpenRouter's `error_type` first, then the status,
/// then the message. "No provider" for the data policy is told apart.
fn classify(
    status: Option<u16>,
    error_type: Option<&str>,
    message: &str,
    has_limit_source: bool,
) -> ErrorKind {
    match classify_kind(status, error_type, message, has_limit_source) {
        ErrorKind::NoProvider if message.to_ascii_lowercase().contains("data policy") => {
            ErrorKind::DataPolicy
        }
        kind => kind,
    }
}

fn classify_kind(
    status: Option<u16>,
    error_type: Option<&str>,
    message: &str,
    has_limit_source: bool,
) -> ErrorKind {
    let lower = message.to_ascii_lowercase();
    let per_day = lower.contains("per-day") || lower.contains("per day") || lower.contains("daily");
    let no_endpoint = lower.contains("no endpoints found")
        || lower.contains("no allowed providers")
        || lower.contains("data policy")
        || lower.contains("no available provider");
    if let Some(t) = error_type {
        let by_type = match t {
            "context_length_exceeded" | "token_limit_exceeded" | "payload_too_large" => {
                Some(ErrorKind::ContextLength)
            }
            "authentication" | "unauthorized" | "invalid_api_key" => Some(ErrorKind::Unauthorized),
            "payment_required" | "insufficient_quota" => Some(ErrorKind::PaymentRequired),
            "permission_denied" if has_limit_source => Some(ErrorKind::Budget),
            "permission_denied" | "content_policy_violation" | "refusal" | "moderation" => {
                Some(ErrorKind::Refused)
            }
            "rate_limit_exceeded" if per_day => Some(ErrorKind::FreeDailyLimit),
            "rate_limit_exceeded" => Some(ErrorKind::RateLimited),
            "provider_unavailable" | "provider_overloaded" | "timeout" => Some(if no_endpoint {
                ErrorKind::NoProvider
            } else {
                ErrorKind::Overloaded
            }),
            "not_found" if no_endpoint => Some(ErrorKind::NoProvider),
            "not_found" => Some(ErrorKind::NotFound),
            "server" | "unmapped" => Some(ErrorKind::Server),
            "invalid_request"
            | "invalid_prompt"
            | "max_tokens_exceeded"
            | "string_too_long"
            | "unprocessable"
            | "precondition_failed"
            | "invalid_image"
            | "image_too_large"
            | "image_too_small"
            | "unsupported_image_format"
            | "image_not_found"
            | "image_download_failed" => Some(ErrorKind::BadRequest),
            _ => None,
        };
        if let Some(kind) = by_type {
            return kind;
        }
    }
    match status {
        Some(400) if is_context_message(&lower) => ErrorKind::ContextLength,
        Some(401) => ErrorKind::Unauthorized,
        Some(402) if lower.contains("in-flight") || lower.contains("in flight") => {
            ErrorKind::RateLimited
        }
        Some(402) => ErrorKind::PaymentRequired,
        Some(403)
            if has_limit_source || lower.contains("budget") || lower.contains("limit exceeded") =>
        {
            ErrorKind::Budget
        }
        Some(403) => ErrorKind::Refused,
        Some(404 | 503) if no_endpoint => ErrorKind::NoProvider,
        Some(404) => ErrorKind::NotFound,
        Some(408) => ErrorKind::Timeout,
        Some(413) => ErrorKind::ContextLength,
        Some(429) if per_day => ErrorKind::FreeDailyLimit,
        Some(429) => ErrorKind::RateLimited,
        Some(502..=504 | 520..=529) => ErrorKind::Overloaded,
        Some(s) if s >= 500 => ErrorKind::Server,
        _ if is_context_message(&lower) => ErrorKind::ContextLength,
        Some(_) => ErrorKind::BadRequest,
        None => ErrorKind::Server,
    }
}

fn is_context_message(lower: &str) -> bool {
    lower.contains("context length")
        || lower.contains("context_length")
        || lower.contains("maximum context")
        || lower.contains("context window")
        || lower.contains("too many tokens")
}

/// One line, at most [`MAX_MESSAGE`] characters.
fn tidy(message: &str) -> String {
    let one_line = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= MAX_MESSAGE {
        return one_line;
    }
    let mut short: String = one_line.chars().take(MAX_MESSAGE - 1).collect();
    short.push('…');
    short
}

/// Reads a `Retry-After` value in seconds (dates aren't used by model
/// services). Capped at one hour.
#[must_use]
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let secs: f64 = value.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(secs.min(3600.0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(code: u16, message: &str, metadata: &Value) -> String {
        json!({"error": {"code": code, "message": message, "metadata": metadata}}).to_string()
    }

    #[test]
    fn rate_limits_per_minute_and_per_day_differ() {
        let minute = LlmError::from_response(
            429,
            Some(Duration::from_secs(7)),
            &body(429, "Rate limit exceeded: free-models-per-min.", &json!({})),
        );
        assert_eq!(minute.kind, ErrorKind::RateLimited);
        assert_eq!(minute.retry_class(), RetryClass::NextModel);
        assert_eq!(minute.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(minute.status, Some(429));

        let day = LlmError::from_response(
            429,
            None,
            &body(
                429,
                "Rate limit exceeded: free-models-per-day. Add 10 credits",
                &json!({}),
            ),
        );
        assert_eq!(day.kind, ErrorKind::FreeDailyLimit);
        assert_eq!(day.retry_class(), RetryClass::StopFreeToday);

        let typed = LlmError::from_response(
            429,
            None,
            &body(
                429,
                "Rate limit exceeded",
                &json!({"error_type": "rate_limit_exceeded", "provider_code": "x"}),
            ),
        );
        assert_eq!(typed.kind, ErrorKind::RateLimited);
    }

    #[test]
    fn statuses_map_to_kinds() {
        let cases = [
            (
                400,
                "This endpoint's maximum context length is 65536 tokens.",
                ErrorKind::ContextLength,
            ),
            (
                400,
                "tools[0].function.parameters is invalid",
                ErrorKind::BadRequest,
            ),
            (401, "User not found.", ErrorKind::Unauthorized),
            (402, "Insufficient credits", ErrorKind::PaymentRequired),
            (403, "Input was flagged", ErrorKind::Refused),
            (
                404,
                "No endpoints found matching your data policy (Free model training). Configure: https://openrouter.ai/settings/privacy",
                ErrorKind::DataPolicy,
            ),
            (
                404,
                "No endpoints found that can handle the requested parameters.",
                ErrorKind::NoProvider,
            ),
            (404, "Model does not exist", ErrorKind::NotFound),
            (408, "Timed out", ErrorKind::Timeout),
            (500, "Internal error", ErrorKind::Server),
            (502, "Bad gateway", ErrorKind::Overloaded),
            (
                503,
                "No available provider meets your routing requirements",
                ErrorKind::NoProvider,
            ),
            (503, "Service unavailable", ErrorKind::Overloaded),
        ];
        for (status, message, kind) in cases {
            let err = LlmError::from_response(status, None, &body(status, message, &json!({})));
            assert_eq!(err.kind, kind, "{status} {message}");
        }
    }

    #[test]
    fn error_type_wins_over_status() {
        let err = LlmError::from_response(
            400,
            None,
            &body(
                400,
                "too long",
                &json!({"error_type": "context_length_exceeded"}),
            ),
        );
        assert_eq!(err.kind, ErrorKind::ContextLength);
        assert_eq!(err.retry_class(), RetryClass::Compact);

        let overloaded = LlmError::from_response(
            503,
            None,
            &body(503, "busy", &json!({"error_type": "provider_overloaded"})),
        );
        assert_eq!(overloaded.kind, ErrorKind::Overloaded);

        let budget = LlmError::from_response(
            403,
            None,
            &body(
                403,
                "Key limit exceeded",
                &json!({"error_type": "permission_denied", "limit_source": "openrouter_key_limit"}),
            ),
        );
        assert_eq!(budget.kind, ErrorKind::Budget);
        assert_eq!(budget.retry_class(), RetryClass::OutOfCredits);
    }

    #[test]
    fn moderation_keeps_reasons_but_not_the_flagged_text() {
        let err = LlmError::from_response(
            403,
            None,
            &body(
                403,
                "Input was flagged",
                &json!({
                    "reasons": ["violence"],
                    "flagged_input": "the person's own words",
                    "provider_name": "P",
                    "model_slug": "m"
                }),
            ),
        );
        assert_eq!(err.kind, ErrorKind::Refused);
        assert_eq!(err.message, "Input was flagged (violence)");
        assert_eq!(err.retry_class(), RetryClass::Refused);
    }

    #[test]
    fn non_json_bodies_still_classify() {
        let err = LlmError::from_response(502, None, "<html>bad gateway</html>");
        assert_eq!(err.kind, ErrorKind::Overloaded);
        let empty = LlmError::from_response(500, None, "");
        assert_eq!(empty.kind, ErrorKind::Server);
        assert!(empty.message.contains("500"));
    }

    #[test]
    fn mid_stream_errors_are_partial() {
        let err = LlmError::from_stream_error(&json!({
            "code": 429, "message": "Rate limit exceeded", "metadata": {"error_type": "rate_limit_exceeded"}
        }));
        assert_eq!(err.kind, ErrorKind::RateLimited);
        assert_eq!(err.status, Some(429));
        assert!(err.partial);

        let overloaded = LlmError::from_stream_error(
            &json!({"code": 502, "message": "Provider returned error"}),
        );
        assert_eq!(overloaded.kind, ErrorKind::Overloaded);
    }

    #[test]
    fn messages_are_one_short_line() {
        let long = "word\n".repeat(200);
        let err = LlmError::new(ErrorKind::Server, long);
        assert!(!err.message.contains('\n'));
        assert_eq!(err.message.chars().count(), MAX_MESSAGE);
        assert!(err.message.ends_with('…'));
    }

    #[test]
    fn retry_after_reads_seconds() {
        assert_eq!(parse_retry_after("5"), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after(" 1.5 "),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_retry_after("99999"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    #[test]
    fn every_kind_has_a_retry_class() {
        assert_eq!(ErrorKind::Network.retry_class(), RetryClass::RetrySame);
        assert_eq!(
            ErrorKind::NotSignedIn.retry_class(),
            RetryClass::SignInAgain
        );
        assert_eq!(ErrorKind::Cancelled.retry_class(), RetryClass::Cancelled);
        assert_eq!(ErrorKind::Config.retry_class(), RetryClass::Fail);
        assert_eq!(ErrorKind::Overloaded.as_str(), "busy");
        assert_eq!(ErrorKind::DataPolicy.retry_class(), RetryClass::NextModel);
        assert_eq!(ErrorKind::DataPolicy.as_str(), "data_policy");
    }
}
