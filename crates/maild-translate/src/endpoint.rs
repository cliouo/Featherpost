//! Endpoint URL validation, normalization, transport classification, and DNS public-IP guard.
//!
//! Implements the P0 credential-to-normalized-endpoint binding identity (`scheme + host +
//! effective port + normalized API root path`) and enforces strict transport rules:
//! - Reject embedded userinfo, query parameters (no query secrets), fragments, and `..` path traversal
//! - Public hosts MUST use `https://`
//! - Loopback hosts allow `http://` and `https://` (including keyless local Ollama)
//! - Private/LAN hosts allow `https://` and explicit `http://` (flagged as `plaintext_lan_transport`)

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use maild_proxy::is_loopback_host;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::TranslateError;

/// Maximum accepted length of a user-configured base URL.
pub const MAX_BASE_URL_BYTES: usize = 1024;

/// Transport security classification of a validated endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointTransport {
    PublicHttps,
    LoopbackHttp,
    LoopbackHttps,
    LanHttp,
    LanHttps,
}

impl EndpointTransport {
    /// Whether this endpoint is allowed to operate without an API key (e.g., local Ollama).
    pub const fn allows_keyless(self) -> bool {
        matches!(self, Self::LoopbackHttp | Self::LoopbackHttps)
    }

    /// Whether this endpoint uses unencrypted HTTP over a non-loopback LAN network.
    pub const fn is_plaintext_lan(self) -> bool {
        matches!(self, Self::LanHttp)
    }

    /// Whether this endpoint targets a loopback host.
    pub const fn is_loopback(self) -> bool {
        matches!(self, Self::LoopbackHttp | Self::LoopbackHttps)
    }

    /// Whether this endpoint targets a public internet host.
    pub const fn is_public(self) -> bool {
        matches!(self, Self::PublicHttps)
    }
}

/// Normalized service identity used to bind stored API keys and compute cache variant revisions.
///
/// Includes `scheme`, normalized `host`, effective `port`, and normalized `api_root_path` so
/// shared-origin multi-tenant gateways (e.g. `https://gw.example.com/team-a/v1` vs
/// `https://gw.example.com/team-b/v1`) never share credentials across tenants.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NormalizedServiceIdentity {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub api_root_path: String,
    pub transport: EndpointTransport,
}

impl NormalizedServiceIdentity {
    /// Parse, validate, and normalize an OpenAI-compatible base URL.
    pub fn parse(raw_base_url: &str) -> Result<Self, TranslateError> {
        let trimmed = raw_base_url.trim();
        if trimmed.is_empty() {
            return Err(TranslateError::EndpointRejected("base URL is empty".into()));
        }
        if trimmed.len() > MAX_BASE_URL_BYTES {
            return Err(TranslateError::EndpointRejected(
                "base URL exceeds maximum length".into(),
            ));
        }
        if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(TranslateError::EndpointRejected(
                "base URL contains control or whitespace characters".into(),
            ));
        }

        // Inspect the raw path before `Url::parse` silently collapses `.` / `..` segments so
        // gateway path-traversal confusion attempts are explicitly rejected.
        if let Some((_, rest)) = trimmed.split_once("://") {
            if let Some(path_idx) = rest.find('/') {
                let raw_tail = &rest[path_idx..];
                let raw_path = raw_tail.split(['?', '#']).next().unwrap_or("");
                let raw_lower = raw_path.to_ascii_lowercase();
                if raw_lower.contains("%2e")
                    || raw_lower.contains("%2f")
                    || raw_lower.contains('\\')
                    || raw_path.split('/').any(|seg| seg == "." || seg == "..")
                {
                    return Err(TranslateError::EndpointRejected(
                        "URL path contains forbidden traversal or encoded path characters".into(),
                    ));
                }
            }
        }

        let parsed = Url::parse(trimmed)
            .map_err(|_| TranslateError::EndpointRejected("invalid URL syntax".into()))?;

        let scheme = parsed.scheme().to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(TranslateError::EndpointRejected(
                "scheme must be http or https".into(),
            ));
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(TranslateError::EndpointRejected(
                "embedded URL credentials (userinfo) are forbidden".into(),
            ));
        }
        if parsed.query().is_some() {
            return Err(TranslateError::EndpointRejected(
                "URL query parameters are forbidden".into(),
            ));
        }
        if parsed.fragment().is_some() {
            return Err(TranslateError::EndpointRejected(
                "URL fragments are forbidden".into(),
            ));
        }

        let raw_host = parsed
            .host_str()
            .ok_or_else(|| TranslateError::EndpointRejected("missing host".into()))?;
        let host = normalize_host(raw_host);
        if host.is_empty() {
            return Err(TranslateError::EndpointRejected("empty host".into()));
        }

        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| TranslateError::EndpointRejected("missing port".into()))?;
        if port == 0 {
            return Err(TranslateError::EndpointRejected("port cannot be 0".into()));
        }

        // Reject forbidden IP classes (unspecified, broadcast, multicast, reserved).
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_forbidden_ip(&ip) {
                return Err(TranslateError::EndpointRejected(
                    "host IP address is not routable".into(),
                ));
            }
        }

        let host_class = classify_host(&host);
        let transport = match (host_class, scheme.as_str()) {
            (HostClass::Loopback, "http") => EndpointTransport::LoopbackHttp,
            (HostClass::Loopback, "https") => EndpointTransport::LoopbackHttps,
            (HostClass::Lan, "http") => EndpointTransport::LanHttp,
            (HostClass::Lan, "https") => EndpointTransport::LanHttps,
            (HostClass::Public, "https") => EndpointTransport::PublicHttps,
            (HostClass::Public, "http") => {
                return Err(TranslateError::EndpointRejected(
                    "public translation endpoints require https://".into(),
                ));
            }
            _ => {
                return Err(TranslateError::EndpointRejected(
                    "unsupported scheme and host combination".into(),
                ));
            }
        };

        let api_root_path = normalize_api_root_path(parsed.path())?;

        Ok(Self {
            scheme,
            host,
            port,
            api_root_path,
            transport,
        })
    }

    /// Canonical string representation used for credential binding and cache keys.
    /// Format: `{scheme}://{host}:{port}{api_root_path}` (with bracketed IPv6).
    pub fn canonical_key(&self) -> String {
        let host_fmt = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "{}://{}:{}{}",
            self.scheme, host_fmt, self.port, self.api_root_path
        )
    }

    /// Full URL for OpenAI-compatible `POST .../chat/completions`.
    pub fn chat_completions_url(&self) -> Result<Url, TranslateError> {
        let host_fmt = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let path = if self.api_root_path.is_empty() {
            "/v1/chat/completions".to_string()
        } else {
            format!("{}/chat/completions", self.api_root_path)
        };
        let raw = format!("{}://{}:{}{}", self.scheme, host_fmt, self.port, path);
        Url::parse(&raw)
            .map_err(|_| TranslateError::EndpointRejected("failed to construct endpoint URL".into()))
    }

    /// True if `self.host` is a literal IP address rather than a DNS name.
    pub fn is_ip_literal(&self) -> bool {
        self.host.parse::<IpAddr>().is_ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostClass {
    Loopback,
    Lan,
    Public,
}

fn normalize_host(raw: &str) -> String {
    let trimmed = raw.trim();
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    let cleaned = unbracketed.trim_end_matches('.').to_ascii_lowercase();
    // Canonicalize IP literals (e.g. IPv6 compression or IPv4-mapped IPv6).
    if let Ok(ip) = cleaned.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => {
                if let Some(mapped) = v6.to_ipv4_mapped() {
                    mapped.to_string()
                } else {
                    v6.to_string()
                }
            }
        }
    } else {
        cleaned
    }
}

fn normalize_api_root_path(raw_path: &str) -> Result<String, TranslateError> {
    // Reject encoded or literal dot segments to prevent path normalization confusion across gateways.
    let lower = raw_path.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains('\\') {
        return Err(TranslateError::EndpointRejected(
            "URL path contains forbidden encoded path characters".into(),
        ));
    }
    let mut segments: Vec<&str> = Vec::new();
    for seg in raw_path.split('/') {
        if seg.is_empty() {
            continue;
        }
        if seg == "." || seg == ".." {
            return Err(TranslateError::EndpointRejected(
                "URL path contains forbidden traversal segments".into(),
            ));
        }
        segments.push(seg);
    }
    // If user pasted the full `/chat/completions` suffix, strip it so root identity is canonical.
    if segments.len() >= 2
        && segments[segments.len() - 2].eq_ignore_ascii_case("chat")
        && segments[segments.len() - 1].eq_ignore_ascii_case("completions")
    {
        segments.truncate(segments.len() - 2);
    }
    if segments.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("/{}", segments.join("/")))
    }
}

fn classify_host(normalized_host: &str) -> HostClass {
    if is_loopback_host(normalized_host) {
        return HostClass::Loopback;
    }
    if let Ok(ip) = normalized_host.parse::<IpAddr>() {
        return if is_public_ip(&ip) {
            HostClass::Public
        } else {
            HostClass::Lan
        };
    }
    if !normalized_host.contains('.') {
        return HostClass::Lan;
    }
    for suffix in [".local", ".lan", ".internal", ".localdomain", ".home.arpa"] {
        if normalized_host.ends_with(suffix) {
            return HostClass::Lan;
        }
    }
    HostClass::Public
}

fn is_forbidden_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast() || v4.octets()[0] == 0
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(&IpAddr::V4(mapped));
            }
            v6.is_unspecified() || v6.is_multicast()
        }
    }
}

/// Returns true iff `ip` is globally routable (excludes loopback, private, link-local, CGNAT,
/// documentation, unspecified, multicast, ULA, and reserved ranges).
pub fn is_public_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => is_public_ipv6(v6),
    }
}

fn is_public_ipv4(ip: &Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (18..20).contains(&b))
        || a >= 240)
}

fn is_public_ipv6(ip: &Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_ipv4(&v4);
    }
    let first = ip.segments()[0];
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        || (first & 0xfe00) == 0xfc00
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        || (first == 0x2001 && ip.segments()[1] == 0x0db8)
        || first == 0x0064
        || first == 0x0000)
}
