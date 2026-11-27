//! Sanitized error types for the translation core engine.
//!
//! Security invariant: error values and their `Display`/`Debug` representations MUST NEVER
//! contain plaintext email content, API keys, raw TOML source lines, or raw upstream HTTP
//! response bodies.

use std::fmt;

/// Stable machine-readable error reasons and sanitized diagnostic details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranslateError {
    /// Invalid configuration or input parameter.
    InvalidConfig(String),
    /// Base URL changed without supplying a new API key (or explicit keyless loopback).
    CredentialRebindRequired,
    /// Config file is corrupted, has an unknown future version, or failed security checks.
    /// Store operates fail-closed and refuses to overwrite the file on disk.
    ConfigUnavailable(String),
    /// Optimistic concurrency revision mismatch while saving preferences.
    RevisionConflict { expected: u64, actual: u64 },
    /// Built-in provider has not received its first valid proxy snapshot yet.
    ProxyNotReady,
    /// Requested provider is missing, unconfigured, disabled, or its generation changed.
    ProviderUnavailable(String),
    /// Outbound request was rejected because the target URL/endpoint violated security policy.
    EndpointRejected(String),
    /// HTTP redirect responses are strictly blocked to prevent credential or SSRF leakage.
    RedirectBlocked,
    /// DNS resolution returned a non-public address for a public HTTPS endpoint.
    DnsBlocked,
    /// Upstream returned HTTP 401/403 authentication or authorization failure.
    AuthFailed,
    /// Upstream returned HTTP 429 rate limit.
    RateLimited,
    /// Upstream returned an unexpected non-success HTTP status code (sanitized).
    UpstreamStatus { status: u16, reason: String },
    /// Upstream response exceeded the maximum allowed byte size.
    ResponseTooLarge,
    /// Upstream response was malformed or failed strict ID / expansion validation.
    InvalidResponse(String),
    /// Transport / network error (sanitized, never containing secrets or payload).
    Network(String),
    /// Connect or total request deadline expired.
    Timeout,
    /// Translation was cancelled by the caller.
    Cancelled,
    /// Persistent cache I/O or integrity error.
    Cache(String),
    /// Filesystem I/O error (sanitized).
    Io(String),
}

impl TranslateError {
    /// Stable snake_case reason code suitable for bridging into UI / RPC layers.
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::InvalidConfig(_) => "translation_invalid_config",
            Self::CredentialRebindRequired => "translation_credential_rebind_required",
            Self::ConfigUnavailable(_) => "translation_config_unavailable",
            Self::RevisionConflict { .. } => "translation_revision_conflict",
            Self::ProxyNotReady => "translation_proxy_not_ready",
            Self::ProviderUnavailable(_) => "translation_provider_unavailable",
            Self::EndpointRejected(_) => "translation_endpoint_rejected",
            Self::RedirectBlocked => "translation_redirect_blocked",
            Self::DnsBlocked => "translation_dns_blocked",
            Self::AuthFailed => "translation_auth_failed",
            Self::RateLimited => "translation_rate_limited",
            Self::UpstreamStatus { .. } => "translation_upstream_error",
            Self::ResponseTooLarge => "translation_response_too_large",
            Self::InvalidResponse(_) => "translation_invalid_response",
            Self::Network(_) => "translation_network_error",
            Self::Timeout => "translation_timeout",
            Self::Cancelled => "translation_cancelled",
            Self::Cache(_) => "translation_cache_error",
            Self::Io(_) => "translation_io_error",
        }
    }
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(msg) => write!(f, "invalid translation config: {msg}"),
            Self::CredentialRebindRequired => write!(
                f,
                "endpoint changed: provide a new API key or select a keyless loopback service"
            ),
            Self::ConfigUnavailable(reason) => {
                write!(f, "translation config unavailable (fail-closed): {reason}")
            }
            Self::RevisionConflict { expected, actual } => write!(
                f,
                "translation config revision conflict (expected {expected}, actual {actual})"
            ),
            Self::ProxyNotReady => {
                write!(f, "proxy configuration snapshot has not been initialized yet")
            }
            Self::ProviderUnavailable(reason) => {
                write!(f, "translation provider unavailable: {reason}")
            }
            Self::EndpointRejected(reason) => write!(f, "endpoint rejected: {reason}"),
            Self::RedirectBlocked => write!(f, "HTTP redirect blocked for translation endpoint"),
            Self::DnsBlocked => write!(f, "DNS resolved public endpoint to non-public address"),
            Self::AuthFailed => write!(f, "upstream authentication failed"),
            Self::RateLimited => write!(f, "upstream rate limit exceeded"),
            Self::UpstreamStatus { status, reason } => {
                write!(f, "upstream returned HTTP {status}: {reason}")
            }
            Self::ResponseTooLarge => write!(f, "upstream response exceeded size limit"),
            Self::InvalidResponse(reason) => write!(f, "invalid translation response: {reason}"),
            Self::Network(reason) => write!(f, "network error: {reason}"),
            Self::Timeout => write!(f, "translation request timed out"),
            Self::Cancelled => write!(f, "translation request cancelled"),
            Self::Cache(reason) => write!(f, "translation cache error: {reason}"),
            Self::Io(reason) => write!(f, "translation I/O error: {reason}"),
        }
    }
}

impl std::error::Error for TranslateError {}

/// Classify a reqwest transport error into a sanitized [`TranslateError`] without leaking
/// URLs, query parameters, credentials, or upstream payloads.
pub fn sanitize_reqwest_error(err: &reqwest::Error) -> TranslateError {
    if err.is_timeout() {
        return TranslateError::Timeout;
    }
    if err.is_redirect() {
        return TranslateError::RedirectBlocked;
    }
    if err.is_connect() {
        return TranslateError::Network("connection failed".into());
    }
    if err.is_body() || err.is_decode() {
        return TranslateError::Network("response stream error".into());
    }
    if err.is_request() {
        return TranslateError::Network("request transport error".into());
    }
    TranslateError::Network("transport error".into())
}

/// Produce a fixed, sanitized classification for a non-2xx HTTP status code.
/// Raw upstream body bytes are NEVER forwarded into the returned error string.
pub fn sanitize_http_status(status: u16) -> TranslateError {
    match status {
        300..=399 => TranslateError::RedirectBlocked,
        401 | 403 => TranslateError::AuthFailed,
        429 => TranslateError::RateLimited,
        400 => TranslateError::UpstreamStatus {
            status,
            reason: "bad request".into(),
        },
        404 => TranslateError::UpstreamStatus {
            status,
            reason: "endpoint or model not found".into(),
        },
        408 => TranslateError::Timeout,
        413 => TranslateError::UpstreamStatus {
            status,
            reason: "payload too large".into(),
        },
        422 => TranslateError::UpstreamStatus {
            status,
            reason: "unprocessable request".into(),
        },
        500..=599 => TranslateError::UpstreamStatus {
            status,
            reason: "upstream server error".into(),
        },
        _ => TranslateError::UpstreamStatus {
            status,
            reason: "unexpected upstream status".into(),
        },
    }
}
