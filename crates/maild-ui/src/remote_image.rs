//! Remote email image fetcher for `Route::Remote(token, idx)`.
//!
//! All remote images allowed by reading policy (`allow_remote = true`) are rewritten by `html.rs`
//! into `<url_prefix>/remote/<token>/<idx>` while document CSP stays permanently locked to
//! `img-src <csp_source> data:`. The WebView itself never opens outbound network connections.
//!
//! This module fetches those remote images over `reqwest::Client` routed through `maild-proxy`
//! (`ProxyPurpose::RemoteImage`), enforcing strict scheme, redirect, SSRF, `Content-Type`,
//! size, and timeout invariants, and hot-reloading its `reqwest::Client` whenever `maild`'s
//! proxy configuration or OS system proxy changes.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use maild_proxy::reqwest_adapter::{apply_proxy_with_resolver, resolve_url_route};
use maild_proxy::{
    detect_system_proxy, is_loopback_host, CustomProxyConfig, ProxyConfig, ProxyContext,
    ProxyContextDefaults, ProxyEndpoint, ProxyMode, ProxyProtocol, ProxyResolver, ResolveOptions,
    ResolvedRoute, SystemProxySnapshot, TargetScheme,
};
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, LOCATION};
use reqwest::StatusCode;
use url::Url;

use crate::bridge::Bridge;
use crate::documents::DocumentStore;

/// Maximum remote image body size (10 MiB).
pub const MAX_REMOTE_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Maximum number of HTTP redirects followed per image request.
pub const MAX_REDIRECTS: usize = 5;
/// TCP/TLS connect timeout per hop.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Total end-to-end budget across all redirects and body streaming.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Successfully fetched and validated remote image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchedImage {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

/// Failure classification for remote image fetching (never contains proxy credentials).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteImageError {
    NotFound,
    Blocked,
    InvalidUrl(&'static str),
    SsrfBlocked(&'static str),
    TooManyRedirects,
    HttpStatus(u16),
    NonImageContentType,
    TooLarge,
    Timeout,
    Network,
}

impl fmt::Display for RemoteImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("document or remote image index not found"),
            Self::Blocked => f.write_str("remote content blocked for document"),
            Self::InvalidUrl(reason) => write!(f, "invalid remote image url: {reason}"),
            Self::SsrfBlocked(reason) => write!(f, "blocked by SSRF guard: {reason}"),
            Self::TooManyRedirects => f.write_str("exceeded redirect limit"),
            Self::HttpStatus(code) => write!(f, "upstream returned HTTP {code}"),
            Self::NonImageContentType => f.write_str("upstream Content-Type is not image/*"),
            Self::TooLarge => f.write_str("remote image exceeds 10 MiB limit"),
            Self::Timeout => f.write_str("remote image request timed out"),
            Self::Network => f.write_str("remote image network request failed"),
        }
    }
}

impl std::error::Error for RemoteImageError {}

struct FetcherState {
    config: ProxyConfig,
    system: SystemProxySnapshot,
    resolver: Option<Arc<dyn ProxyResolver>>,
    client: reqwest::Client,
    generation: u64,
    resolve_options: ResolveOptions,
    #[cfg(any(test, debug_assertions))]
    accept_invalid_certs: bool,
}

/// Thread-safe, hot-reloadable remote image fetcher backed by `maild-proxy`.
pub struct RemoteImageFetcher {
    state: RwLock<FetcherState>,
    request_timeout: Duration,
}

impl Default for RemoteImageFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteImageFetcher {
    /// Construct with default `ProxyMode::System` + current OS system proxy snapshot.
    pub fn new() -> Self {
        Self::with_proxy_config(ProxyConfig::default(), detect_system_proxy())
    }

    /// Construct with explicit proxy config and system snapshot.
    pub fn with_proxy_config(config: ProxyConfig, system: SystemProxySnapshot) -> Self {
        let resolve_options = ResolveOptions::default();
        let client = build_http_client(
            config.clone(),
            system.clone(),
            None,
            resolve_options,
            #[cfg(any(test, debug_assertions))]
            false,
        )
        .expect("build default remote image reqwest client");
        Self {
            state: RwLock::new(FetcherState {
                config,
                system,
                resolver: None,
                client,
                generation: 0,
                resolve_options,
                #[cfg(any(test, debug_assertions))]
                accept_invalid_certs: false,
            }),
            request_timeout: REQUEST_TIMEOUT,
        }
    }

    /// Attach an optional [`ProxyResolver`] (e.g. conditional per-URL rules) evaluated before the
    /// global proxy config, atomically rebuilding the `reqwest::Client`.
    pub fn set_resolver(&self, resolver: Option<Arc<dyn ProxyResolver>>) {
        let mut guard = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(client) = build_http_client(
            guard.config.clone(),
            guard.system.clone(),
            resolver.clone(),
            guard.resolve_options,
            #[cfg(any(test, debug_assertions))]
            guard.accept_invalid_certs,
        ) {
            guard.resolver = resolver;
            guard.client = client;
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn set_accept_invalid_certs(&self, accept: bool) {
        let mut guard = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.accept_invalid_certs = accept;
        if let Ok(client) = build_http_client(
            guard.config.clone(),
            guard.system.clone(),
            guard.resolver.clone(),
            guard.resolve_options,
            guard.accept_invalid_certs,
        ) {
            guard.client = client;
        }
    }

    #[cfg(any(test, debug_assertions))]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Current proxy config snapshot (redacted; never exposes passwords).
    pub fn current_config_redacted(&self) -> (ProxyConfig, u64) {
        let guard = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (guard.config.without_secret(), guard.generation)
    }

    /// Atomically swap the active proxy config + system snapshot and rebuild `reqwest::Client`
    /// so stale pooled connections to a previous proxy are dropped immediately.
    pub fn apply_proxy_config(
        &self,
        config: ProxyConfig,
        system: SystemProxySnapshot,
        generation: u64,
    ) {
        let mut guard = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match build_http_client(
            config.clone(),
            system.clone(),
            guard.resolver.clone(),
            guard.resolve_options,
            #[cfg(any(test, debug_assertions))]
            guard.accept_invalid_certs,
        ) {
            Ok(client) => {
                guard.config = config;
                guard.system = system;
                guard.client = client;
                guard.generation = generation;
            }
            Err(_) => {
                eprintln!("[maild-ui/remote-image] 重建 HTTP 客户端失败，保留旧代理配置");
            }
        }
    }

    /// Fetch the latest global proxy config (including optional auth secrets) from `maild` over
    /// local stream IPC (`getProxyConfig { include_secret: true }`) and atomically refresh the
    /// active `reqwest::Client`. Never touches OS Keychain and never logs credentials.
    pub async fn refresh_from_bridge(&self, bridge: &Bridge, generation: u64) {
        match bridge.proxy_config_with_secret().await {
            Ok(result) => {
                let (config, system) = convert_client_proxy_result(&result);
                self.apply_proxy_config(config, system, generation);
            }
            Err(error) if error.reason == "proxy_unsupported" => {
                self.apply_proxy_config(ProxyConfig::default(), detect_system_proxy(), generation);
            }
            Err(error) => {
                eprintln!(
                    "[maild-ui/remote-image] 同步代理配置失败: code={:?}, reason={}",
                    error.code, error.reason
                );
            }
        }
    }

    /// Handle `Route::Remote(token, idx)` against `store`.
    pub async fn serve_remote(
        &self,
        store: &DocumentStore,
        token: &str,
        index: usize,
    ) -> Result<FetchedImage, RemoteImageError> {
        let url = {
            let document = store.get(token).ok_or(RemoteImageError::NotFound)?;
            if !document.allow_remote {
                return Err(RemoteImageError::Blocked);
            }
            document
                .remote_urls
                .get(index)
                .cloned()
                .ok_or(RemoteImageError::NotFound)?
        };
        let result = self.fetch_url(&url).await;
        if let Err(ref err) = result {
            let host = Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| "<invalid>".into());
            eprintln!("[maild-ui/remote-image] 抓取失败 ({host}): {err}");
        }
        result
    }

    /// Fetch a single validated remote image URL through the active proxy configuration.
    pub async fn fetch_url(&self, raw_url: &str) -> Result<FetchedImage, RemoteImageError> {
        tokio::time::timeout(self.request_timeout, self.fetch_url_inner(raw_url))
            .await
            .map_err(|_| RemoteImageError::Timeout)?
    }

    async fn fetch_url_inner(&self, raw_url: &str) -> Result<FetchedImage, RemoteImageError> {
        let mut current = parse_and_validate_url(raw_url)?;
        let (client, config, system, resolver, resolve_options) = {
            let guard = self
                .state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                guard.client.clone(),
                guard.config.clone(),
                guard.system.clone(),
                guard.resolver.clone(),
                guard.resolve_options,
            )
        };

        let mut redirects = 0usize;
        loop {
            let host = current
                .host_str()
                .ok_or(RemoteImageError::InvalidUrl("missing host"))?;
            let port = current
                .port_or_known_default()
                .ok_or(RemoteImageError::InvalidUrl("missing port"))?;
            let scheme = match current.scheme() {
                "https" => TargetScheme::Https,
                "http" => TargetScheme::Http,
                _ => return Err(RemoteImageError::InvalidUrl("scheme must be http or https")),
            };

            let route = resolve_url_route(
                &config,
                &system,
                resolver.as_deref(),
                &ProxyContext::new(maild_proxy::ProxyTarget::new(host, port, scheme))
                    .with_purpose(maild_proxy::ProxyPurpose::RemoteImage),
                resolve_options,
            );
            check_ssrf_target(host, port, &route).await?;

            // Build a brand-new request on every hop so no sensitive headers (`Authorization`,
            // `Cookie`, `Referer`) can ever be forwarded across redirects.
            let mut response = client
                .get(current.clone())
                .header("Referrer-Policy", HeaderValue::from_static("no-referrer"))
                .header(ACCEPT, HeaderValue::from_static("image/*"))
                .send()
                .await
                .map_err(|err| {
                    if err.is_timeout() {
                        RemoteImageError::Timeout
                    } else {
                        RemoteImageError::Network
                    }
                })?;

            let status = response.status();
            if is_redirect(status) {
                if redirects >= MAX_REDIRECTS {
                    return Err(RemoteImageError::TooManyRedirects);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or(RemoteImageError::InvalidUrl("redirect missing Location"))?;
                let next = current
                    .join(location)
                    .map_err(|_| RemoteImageError::InvalidUrl("invalid redirect Location"))?;
                current = validate_parsed_url(next)?;
                redirects += 1;
                continue;
            }

            if !status.is_success() {
                return Err(RemoteImageError::HttpStatus(status.as_u16()));
            }

            let content_type = parse_image_content_type(response.headers())?;

            if response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<usize>().ok())
                .is_some_and(|len| len > MAX_REMOTE_IMAGE_BYTES)
            {
                return Err(RemoteImageError::TooLarge);
            }

            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|err| {
                if err.is_timeout() {
                    RemoteImageError::Timeout
                } else {
                    RemoteImageError::Network
                }
            })? {
                if bytes.len().saturating_add(chunk.len()) > MAX_REMOTE_IMAGE_BYTES {
                    return Err(RemoteImageError::TooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }

            return Ok(FetchedImage {
                content_type,
                bytes,
            });
        }
    }
}

fn build_http_client(
    config: ProxyConfig,
    system: SystemProxySnapshot,
    resolver: Option<Arc<dyn ProxyResolver>>,
    resolve_options: ResolveOptions,
    #[cfg(any(test, debug_assertions))] accept_invalid_certs: bool,
) -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT);
    #[cfg(any(test, debug_assertions))]
    let builder = if accept_invalid_certs {
        builder.danger_accept_invalid_certs(true)
    } else {
        builder
    };
    apply_proxy_with_resolver(
        builder,
        config,
        system,
        resolver,
        ProxyContextDefaults::remote_image(),
        resolve_options,
    )
    .build()
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn parse_and_validate_url(raw: &str) -> Result<Url, RemoteImageError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return Err(RemoteImageError::InvalidUrl("empty or control chars"));
    }
    let parsed = Url::parse(trimmed).map_err(|_| RemoteImageError::InvalidUrl("parse failed"))?;
    validate_parsed_url(parsed)
}

fn validate_parsed_url(parsed: Url) -> Result<Url, RemoteImageError> {
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(RemoteImageError::InvalidUrl("unsupported scheme"));
    }
    if !parsed.has_host() {
        return Err(RemoteImageError::InvalidUrl("missing host"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(RemoteImageError::InvalidUrl("embedded credentials forbidden"));
    }
    Ok(parsed)
}

fn parse_image_content_type(
    headers: &reqwest::header::HeaderMap,
) -> Result<String, RemoteImageError> {
    let raw = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .ok_or(RemoteImageError::NonImageContentType)?;
    let media = raw.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let Some(subtype) = media.strip_prefix("image/") else {
        return Err(RemoteImageError::NonImageContentType);
    };
    if subtype.is_empty()
        || !subtype
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    {
        return Err(RemoteImageError::NonImageContentType);
    }
    Ok(media)
}

/// SSRF guard evaluated before EVERY hop (initial URL + every redirect target).
///
/// - Rejects loopback hosts, internal/local domain suffixes, single-label hostnames, and
///   non-public IPv4/IPv6 literals regardless of proxy mode.
/// - For `Direct` and local-DNS `socks5`, resolves DNS locally and requires EVERY resolved IP to
///   be globally routable (`is_public_ip`).
async fn check_ssrf_target(
    raw_host: &str,
    port: u16,
    route: &ResolvedRoute,
) -> Result<(), RemoteImageError> {
    let host = raw_host
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw_host.trim())
        .trim_end_matches('.')
        .to_ascii_lowercase();

    if host.is_empty() {
        return Err(RemoteImageError::SsrfBlocked("empty host"));
    }
    if is_loopback_host(&host) {
        return Err(RemoteImageError::SsrfBlocked("loopback host"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(&ip) {
            return Err(RemoteImageError::SsrfBlocked("non-public IP literal"));
        }
        return Ok(());
    }
    if !host.contains('.') {
        return Err(RemoteImageError::SsrfBlocked("single-label hostname"));
    }
    for suffix in [".local", ".internal", ".localdomain", ".home.arpa", ".lan"] {
        if host.ends_with(suffix) {
            return Err(RemoteImageError::SsrfBlocked("internal domain suffix"));
        }
    }

    let needs_local_dns_check = match route {
        ResolvedRoute::Direct => true,
        ResolvedRoute::Proxy(ep) => ep.protocol == ProxyProtocol::Socks5,
    };
    if needs_local_dns_check {
        let resolved = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| RemoteImageError::Network)?;
        let addrs: Vec<std::net::SocketAddr> = resolved.collect();
        if addrs.is_empty() {
            return Err(RemoteImageError::Network);
        }
        if addrs.iter().any(|addr| !is_public_ip(&addr.ip())) {
            return Err(RemoteImageError::SsrfBlocked("DNS resolved to non-public IP"));
        }
    }
    Ok(())
}

pub(crate) fn is_public_ip(ip: &IpAddr) -> bool {
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
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64/10
        || (a == 192 && b == 0 && c == 0) // IETF 192.0.0/24
        || (a == 198 && (18..20).contains(&b)) // Benchmark 198.18/15
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
        || (first & 0xfe00) == 0xfc00 // ULA fc00::/7
        || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (first & 0xffc0) == 0xfec0 // deprecated site-local fec0::/10
        || (first == 0x2001 && ip.segments()[1] == 0x0db8) // documentation 2001:db8::/32
        || first == 0x0064 // NAT64 64:ff9b::/96
        || first == 0x0000)
}

/// Convert `maild_client::ProxyConfigResult` (from `getProxyConfig { include_secret: true }`)
/// into core `maild_proxy` types.
pub fn convert_client_proxy_result(
    result: &maild_client::ProxyConfigResult,
) -> (ProxyConfig, SystemProxySnapshot) {
    let mode = match result.config.mode.trim().to_ascii_lowercase().as_str() {
        "direct" => ProxyMode::Direct,
        "custom" => ProxyMode::Custom,
        _ => ProxyMode::System,
    };
    let custom = result.config.custom.as_ref().and_then(|c| {
        let protocol = ProxyProtocol::parse_scheme(&c.protocol).ok()?;
        if c.host.trim().is_empty() || c.port == 0 {
            return None;
        }
        Some(CustomProxyConfig {
            protocol,
            host: c.host.clone(),
            port: c.port,
            username: c.username.clone(),
            password: c.password.clone(),
            bypass: c.bypass.clone(),
        })
    });
    let config = if mode == ProxyMode::Custom && custom.is_none() {
        ProxyConfig::default()
    } else {
        ProxyConfig { mode, custom }
    };

    let convert_ep = |ep: &maild_client::ProxyEndpointInfo| -> Option<ProxyEndpoint> {
        let protocol = ProxyProtocol::parse_scheme(&ep.protocol).ok()?;
        if ep.host.trim().is_empty() || ep.port == 0 {
            return None;
        }
        Some(ProxyEndpoint {
            protocol,
            host: ep.host.clone(),
            port: ep.port,
            username: ep.username.clone(),
            password: ep.password.clone(),
        })
    };

    let sys = &result.effective_system;
    let system = SystemProxySnapshot {
        http: sys.http.as_ref().and_then(convert_ep),
        https: sys.https.as_ref().and_then(convert_ep),
        socks5: sys.socks5.as_ref().and_then(convert_ep),
        bypass: sys.bypass.clone(),
        exclude_simple_hostnames: sys.exclude_simple_hostnames,
        pac_url: sys.pac_url.clone(),
        source: sys.source.clone(),
    };
    (config, system)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation_allows_only_http_and_https_without_userinfo() {
        assert!(parse_and_validate_url("https://cdn.example.test/a.png").is_ok());
        assert!(parse_and_validate_url("http://cdn.example.test:8080/a.png").is_ok());
        assert!(parse_and_validate_url("file:///etc/passwd").is_err());
        assert!(parse_and_validate_url("ftp://cdn.example.test/a.png").is_err());
        assert!(parse_and_validate_url("data:image/png;base64,AA==").is_err());
        assert!(parse_and_validate_url("https://user:pass@cdn.example.test/a.png").is_err());
    }

    #[test]
    fn content_type_validation_requires_image_media_type() {
        let check = |value: &str| {
            let mut map = reqwest::header::HeaderMap::new();
            map.insert(CONTENT_TYPE, HeaderValue::from_str(value).unwrap());
            parse_image_content_type(&map)
        };
        assert_eq!(check("image/png").unwrap(), "image/png");
        assert_eq!(
            check("IMAGE/SVG+XML; charset=utf-8").unwrap(),
            "image/svg+xml"
        );
        assert_eq!(check("image/webp").unwrap(), "image/webp");
        assert!(check("text/html; charset=utf-8").is_err());
        assert!(check("application/javascript").is_err());
        assert!(check("image/").is_err());
        assert!(check("image/png/extra").is_err());
        assert!(parse_image_content_type(&reqwest::header::HeaderMap::new()).is_err());
    }

    #[tokio::test]
    async fn ssrf_guard_blocks_loopback_private_link_local_and_internal_hosts() {
        let via_proxy = ResolvedRoute::Proxy(ProxyEndpoint {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: 8080,
            username: None,
            password: None,
        });
        for blocked in [
            "localhost",
            "sub.localhost",
            "127.0.0.1",
            "127.5.6.7",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "[::1]",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "printer.local",
            "metadata.internal",
            "intranet",
        ] {
            assert!(
                matches!(
                    check_ssrf_target(blocked, 80, &via_proxy).await,
                    Err(RemoteImageError::SsrfBlocked(_))
                ),
                "{blocked} must be blocked by SSRF guard"
            );
        }
        assert!(check_ssrf_target("cdn.example.test", 443, &via_proxy)
            .await
            .is_ok());
        assert!(check_ssrf_target("93.184.216.34", 443, &via_proxy)
            .await
            .is_ok());
    }
}
