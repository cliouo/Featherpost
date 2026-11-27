//! Built-in OpenAI-compatible [`TranslationProvider`] routing through `maild-proxy`
//! (`ProxyPurpose::Translation`).
//!
//! Security & reliability invariants:
//! - **First valid proxy snapshot guard**: refuses outbound requests until `apply_proxy_snapshot`
//!   has succeeded at least once; rejects stale `generation` overwrites; never silently falls back
//!   to `Direct` on proxy fetch failure.
//! - **Transport hardening**: `redirect::Policy::none()`, rejects embedded userinfo/query/fragment,
//!   requires `https://` for public hosts, allows keyless `http://` on loopback (always `Direct`
//!   via `maild-proxy`), flags plaintext LAN `http://`.
//! - **DNS pinning for Direct public HTTPS**: resolves locally, rejects non-public IPs, and pins
//!   the verified `SocketAddr`s into the client builder to prevent DNS rebinding TOCTOU.
//! - **Strict JSON contract**: `temperature: 0.1`, `response_format: {"type": "json_object"}`,
//!   payload `{"items":[{"id":0,"text":"..."}]}`. Retries AT MOST ONCE without `response_format`
//!   within the same total deadline budget ONLY when upstream HTTP 400 clearly indicates
//!   `response_format` is unsupported.
//! - **Hard limits & sanitization**: connect timeout, total request deadline, max response bytes,
//!   max expansion ratio, strict item count + unique/complete ID validation, and sanitized errors
//!   (never leaking email text, API key, or raw upstream response body).

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use maild_proxy::reqwest_adapter::{apply_proxy_with_resolver, resolve_url_route};
use maild_proxy::{
    ProxyConfig, ProxyContextDefaults, ProxyProtocol, ProxyResolver, ProxyTarget,
    ResolveOptions, ResolvedRoute, SystemProxySnapshot, TargetScheme,
};
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::ResolvedOpenAiConfig;
use crate::endpoint::{is_public_ip, EndpointTransport, NormalizedServiceIdentity};
use crate::error::{sanitize_http_status, sanitize_reqwest_error, TranslateError};
use crate::types::{
    validate_batch_response, BatchLimits, ContentKind, ProbeResult, ProbeStage,
    ProviderCapabilities, ProviderKind, ProviderMetadata, TextBlock, TranslatedBlock,
    TranslationBatchRequest, TranslationBatchResponse, TranslationProvider,
    BUILTIN_OPENAI_PROVIDER_ID, DEFAULT_MAX_EXPANSION_RATIO, HOST_DEFAULT_BATCH_LIMITS,
};

/// Default HTTP connect timeout for translation requests.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default total deadline budget for a single batch request (including any single fallback retry).
pub const DEFAULT_TOTAL_DEADLINE: Duration = Duration::from_secs(45);
/// Default deadline for `probe`.
pub const DEFAULT_PROBE_DEADLINE: Duration = Duration::from_secs(15);
/// Default maximum raw HTTP response body size (256 KiB).
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// Configurable runtime limits for [`OpenAiCompatibleProvider`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenAiHttpLimits {
    pub connect_timeout: Duration,
    pub total_deadline: Duration,
    pub probe_deadline: Duration,
    pub max_response_bytes: usize,
    pub max_expansion_ratio: usize,
    pub batch_limits: BatchLimits,
}

impl Default for OpenAiHttpLimits {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            total_deadline: DEFAULT_TOTAL_DEADLINE,
            probe_deadline: DEFAULT_PROBE_DEADLINE,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_expansion_ratio: DEFAULT_MAX_EXPANSION_RATIO,
            batch_limits: HOST_DEFAULT_BATCH_LIMITS,
        }
    }
}

#[derive(Default)]
struct ProxySnapshotState {
    initialized: bool,
    generation: u64,
    config: ProxyConfig,
    system: SystemProxySnapshot,
    resolver: Option<Arc<dyn ProxyResolver>>,
    resolve_options: ResolveOptions,
    client: Option<reqwest::Client>,
    #[cfg(any(test, debug_assertions))]
    accept_invalid_certs: bool,
}

/// Result of calling [`OpenAiCompatibleProvider::apply_proxy_snapshot`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxySnapshotOutcome {
    Applied { generation: u64 },
    IgnoredStale { incoming: u64, current: u64 },
}

/// Built-in OpenAI-compatible provider supporting OpenAI, DeepSeek, compatible gateways, and
/// local loopback Ollama.
pub struct OpenAiCompatibleProvider {
    proxy_state: RwLock<ProxySnapshotState>,
    endpoint_config: RwLock<Option<ResolvedOpenAiConfig>>,
    limits: OpenAiHttpLimits,
}

impl Default for OpenAiCompatibleProvider {
    fn default() -> Self {
        Self::new(OpenAiHttpLimits::default())
    }
}

impl OpenAiCompatibleProvider {
    pub fn new(limits: OpenAiHttpLimits) -> Self {
        Self {
            proxy_state: RwLock::new(ProxySnapshotState::default()),
            endpoint_config: RwLock::new(None),
            limits,
        }
    }

    /// Update the active validated endpoint configuration (from [`crate::TranslationConfigStore`]).
    pub fn set_config(&self, config: Option<ResolvedOpenAiConfig>) {
        let mut guard = self
            .endpoint_config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = config;
    }

    /// Returns whether at least one valid proxy snapshot has been loaded.
    pub fn is_proxy_ready(&self) -> bool {
        let guard = self
            .proxy_state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.initialized && guard.client.is_some()
    }

    /// Current applied proxy snapshot generation (if initialized).
    pub fn proxy_generation(&self) -> Option<u64> {
        let guard = self
            .proxy_state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.initialized.then_some(guard.generation)
    }

    /// Attach an optional [`ProxyResolver`] (e.g., conditional proxy rules).
    pub fn set_proxy_resolver(&self, resolver: Option<Arc<dyn ProxyResolver>>) {
        let mut guard = self
            .proxy_state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.resolver = resolver;
        if guard.initialized {
            if let Ok(client) = build_http_client(
                guard.config.clone(),
                guard.system.clone(),
                guard.resolver.clone(),
                guard.resolve_options,
                self.limits.connect_timeout,
                None,
                #[cfg(any(test, debug_assertions))]
                guard.accept_invalid_certs,
            ) {
                guard.client = Some(client);
            }
        }
    }

    /// Apply a new validated proxy snapshot from `maild` (or system snapshot).
    ///
    /// - Rejects stale updates (`generation <= current` once initialized).
    /// - If building the `reqwest::Client` fails, retains the previous valid snapshot and NEVER
    ///   silently falls back to `Direct`.
    pub fn apply_proxy_snapshot(
        &self,
        config: ProxyConfig,
        system: SystemProxySnapshot,
        generation: u64,
    ) -> Result<ProxySnapshotOutcome, TranslateError> {
        let mut guard = self
            .proxy_state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.initialized && generation <= guard.generation {
            return Ok(ProxySnapshotOutcome::IgnoredStale {
                incoming: generation,
                current: guard.generation,
            });
        }
        let client = build_http_client(
            config.clone(),
            system.clone(),
            guard.resolver.clone(),
            guard.resolve_options,
            self.limits.connect_timeout,
            None,
            #[cfg(any(test, debug_assertions))]
            guard.accept_invalid_certs,
        )
        .map_err(|_| TranslateError::Network("failed to build proxy HTTP client".into()))?;

        guard.config = config;
        guard.system = system;
        guard.generation = generation;
        guard.initialized = true;
        guard.client = Some(client);
        Ok(ProxySnapshotOutcome::Applied { generation })
    }

    /// Record that fetching a proxy snapshot failed.
    ///
    /// Deliberately a no-op on the active snapshot: if no valid snapshot has been loaded yet,
    /// `is_proxy_ready()` remains `false` (blocking outbound traffic rather than silently falling
    /// back to `Direct`); if a previous valid snapshot exists, it is retained intact.
    pub fn record_proxy_fetch_failure(&self) {
        // Intentionally preserve existing state and never downgrade to Direct.
    }

    /// Test/debug-only helper for hermetic TLS / loopback proxy tests.
    #[cfg(any(test, debug_assertions))]
    pub fn set_test_options(&self, resolve_options: ResolveOptions, accept_invalid_certs: bool) {
        let mut guard = self
            .proxy_state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.resolve_options = resolve_options;
        guard.accept_invalid_certs = accept_invalid_certs;
        if guard.initialized {
            if let Ok(client) = build_http_client(
                guard.config.clone(),
                guard.system.clone(),
                guard.resolver.clone(),
                guard.resolve_options,
                self.limits.connect_timeout,
                None,
                guard.accept_invalid_certs,
            ) {
                guard.client = Some(client);
            }
        }
    }

    /// Probe an unsaved draft configuration (resolved via [`crate::TranslationConfigStore::resolve_draft_openai`])
    /// using a fixed, email-free sample.
    pub async fn probe_config(
        &self,
        config: &ResolvedOpenAiConfig,
        target_lang: &str,
    ) -> ProbeResult {
        let started = Instant::now();
        let plaintext_lan_transport = config.identity.transport.is_plaintext_lan();
        let route = match self.describe_route(&config.identity) {
            Ok(r) => r,
            Err(err) => {
                return ProbeResult {
                    ok: false,
                    stage: ProbeStage::Proxy,
                    reason: err.reason_code().to_string(),
                    latency_ms: started.elapsed().as_millis() as u64,
                    route: "uninitialized".to_string(),
                    plaintext_lan_transport,
                };
            }
        };

        let request = TranslationBatchRequest {
            job_id: "probe".to_string(),
            batch_index: 0,
            source_lang: Some("en".to_string()),
            target_lang: target_lang.trim().to_string(),
            content_kind: ContentKind::Plain,
            items: vec![TextBlock {
                id: 0,
                text: "Hello".to_string(),
                anchors: Vec::new(),
            }],
            deadline: Some(Instant::now() + self.limits.probe_deadline),
        };

        match self.execute_batch(config, &request).await {
            Ok(_) => ProbeResult {
                ok: true,
                stage: ProbeStage::Done,
                reason: "ok".to_string(),
                latency_ms: started.elapsed().as_millis() as u64,
                route,
                plaintext_lan_transport,
            },
            Err(err) => ProbeResult {
                ok: false,
                stage: classify_probe_stage(&err),
                reason: err.reason_code().to_string(),
                latency_ms: started.elapsed().as_millis() as u64,
                route,
                plaintext_lan_transport,
            },
        }
    }

    fn describe_route(&self, identity: &NormalizedServiceIdentity) -> Result<String, TranslateError> {
        let guard = self
            .proxy_state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !guard.initialized || guard.client.is_none() {
            return Err(TranslateError::ProxyNotReady);
        }
        let target = target_from_identity(identity);
        let ctx = ProxyContextDefaults::translation().build_context(target);
        let resolved = resolve_url_route(
            &guard.config,
            &guard.system,
            guard.resolver.as_deref(),
            &ctx,
            guard.resolve_options,
        );
        Ok(format_sanitized_route(&resolved, identity.transport))
    }

    async fn execute_batch(
        &self,
        config: &ResolvedOpenAiConfig,
        request: &TranslationBatchRequest,
    ) -> Result<TranslationBatchResponse, TranslateError> {
        if request.items.is_empty() {
            return Ok(TranslationBatchResponse {
                items: Vec::new(),
                detected_source_lang: request.source_lang.clone(),
            });
        }

        // Validate endpoint transport & API key policy.
        if config.identity.transport == EndpointTransport::LanHttp && !config.allow_plaintext_lan {
            return Err(TranslateError::EndpointRejected(
                "plaintext http:// on LAN requires explicit allow_plaintext_lan opt-in".into(),
            ));
        }
        let has_key = config
            .api_key
            .as_deref()
            .is_some_and(|k| !k.trim().is_empty());
        if !has_key && !config.identity.transport.allows_keyless() {
            return Err(TranslateError::InvalidConfig(
                "API key required for non-loopback endpoint".into(),
            ));
        }

        // Snapshot proxy state under read lock (first valid proxy snapshot guard).
        let (base_client, proxy_cfg, sys_snap, resolver, resolve_options, accept_invalid_certs) = {
            let guard = self
                .proxy_state
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !guard.initialized {
                return Err(TranslateError::ProxyNotReady);
            }
            let Some(client) = guard.client.clone() else {
                return Err(TranslateError::ProxyNotReady);
            };
            #[cfg(any(test, debug_assertions))]
            let invalid_certs = guard.accept_invalid_certs;
            #[cfg(not(any(test, debug_assertions)))]
            let invalid_certs = false;
            (
                client,
                guard.config.clone(),
                guard.system.clone(),
                guard.resolver.clone(),
                guard.resolve_options,
                invalid_certs,
            )
        };

        let target = target_from_identity(&config.identity);
        let ctx = ProxyContextDefaults::translation().build_context(target);
        let route = resolve_url_route(
            &proxy_cfg,
            &sys_snap,
            resolver.as_deref(),
            &ctx,
            resolve_options,
        );

        let deadline = request
            .deadline
            .unwrap_or_else(|| Instant::now() + self.limits.total_deadline);
        let remaining_before_dns = deadline
            .checked_duration_since(Instant::now())
            .ok_or(TranslateError::Timeout)?;

        // For Direct or local-DNS SOCKS5 requests to a public HTTPS hostname, resolve DNS once,
        // verify every IP is globally routable, and pin those addresses on the client so there
        // is zero DNS-rebinding TOCTOU between verification and TCP connect.
        let client = if config.identity.transport.is_public() && !config.identity.is_ip_literal() {
            let local_dns_used = match &route {
                ResolvedRoute::Direct => true,
                ResolvedRoute::Proxy(ep) => ep.protocol == ProxyProtocol::Socks5,
            };
            if local_dns_used {
                let addrs = tokio::time::timeout(
                    remaining_before_dns,
                    resolve_and_verify_public_dns(&config.identity.host, config.identity.port),
                )
                .await
                .map_err(|_| TranslateError::Timeout)??;
                if matches!(route, ResolvedRoute::Direct) {
                    build_http_client(
                        proxy_cfg,
                        sys_snap,
                        resolver,
                        resolve_options,
                        self.limits.connect_timeout,
                        Some((config.identity.host.as_str(), &addrs)),
                        #[cfg(any(test, debug_assertions))]
                        accept_invalid_certs,
                    )
                    .map_err(|_| {
                        TranslateError::Network("failed to build pinned HTTP client".into())
                    })?
                } else {
                    base_client
                }
            } else {
                // Remote-DNS proxy (HTTP/HTTPS CONNECT or SOCKS5h): proxy performs DNS resolution.
                base_client
            }
        } else {
            let _ = accept_invalid_certs;
            base_client
        };

        let url = config.identity.chat_completions_url()?;

        // Attempt 1: strict `response_format: {"type": "json_object"}`.
        let api_key_str = config.api_key.as_deref().map(String::as_str);
        let first_body = build_chat_completion_payload(config, request, true)?;
        let first_attempt = self
            .send_single_attempt(&client, &url, api_key_str, first_body, deadline)
            .await;

        let (raw_response, used_fallback) = match first_attempt {
            Ok(bytes) => (bytes, false),
            Err(AttemptFailure::ResponseFormatUnsupported400) => {
                // Retry AT MOST ONCE without `response_format`, strictly within the SAME total deadline.
                let fallback_body = build_chat_completion_payload(config, request, false)?;
                match self
                    .send_single_attempt(
                        &client,
                        &url,
                        api_key_str,
                        fallback_body,
                        deadline,
                    )
                    .await
                {
                    Ok(bytes) => (bytes, true),
                    Err(AttemptFailure::Error(err)) => return Err(err),
                    Err(AttemptFailure::ResponseFormatUnsupported400) => {
                        return Err(sanitize_http_status(400));
                    }
                }
            }
            Err(AttemptFailure::Error(err)) => return Err(err),
        };

        parse_and_validate_completion(
            &raw_response,
            &request.items,
            self.limits.max_expansion_ratio,
            used_fallback,
        )
    }

    async fn send_single_attempt(
        &self,
        client: &reqwest::Client,
        url: &url::Url,
        api_key: Option<&str>,
        body: Vec<u8>,
        deadline: Instant,
    ) -> Result<Vec<u8>, AttemptFailure> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(AttemptFailure::Error(TranslateError::Timeout))?;

        let mut req = client
            .post(url.clone())
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(body);

        if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
            let auth_val = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                AttemptFailure::Error(TranslateError::InvalidConfig(
                    "API key contains invalid HTTP header bytes".into(),
                ))
            })?;
            req = req.header(AUTHORIZATION, auth_val);
        }

        let mut response = tokio::time::timeout(remaining, req.send())
            .await
            .map_err(|_| AttemptFailure::Error(TranslateError::Timeout))?
            .map_err(|err| AttemptFailure::Error(sanitize_reqwest_error(&err)))?;

        let status = response.status();
        if status.is_redirection() {
            return Err(AttemptFailure::Error(TranslateError::RedirectBlocked));
        }

        // Check declared Content-Length before reading body.
        if let Some(declared_len) = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<usize>().ok())
        {
            if declared_len > self.limits.max_response_bytes {
                return Err(AttemptFailure::Error(TranslateError::ResponseTooLarge));
            }
        }

        // Read response body incrementally within the same total deadline and byte cap.
        let mut bytes = Vec::new();
        loop {
            let chunk_budget = deadline
                .checked_duration_since(Instant::now())
                .ok_or(AttemptFailure::Error(TranslateError::Timeout))?;
            let next = tokio::time::timeout(chunk_budget, response.chunk())
                .await
                .map_err(|_| AttemptFailure::Error(TranslateError::Timeout))?
                .map_err(|err| AttemptFailure::Error(sanitize_reqwest_error(&err)))?;
            let Some(chunk) = next else {
                break;
            };
            if bytes.len().saturating_add(chunk.len()) > self.limits.max_response_bytes {
                return Err(AttemptFailure::Error(TranslateError::ResponseTooLarge));
            }
            bytes.extend_from_slice(&chunk);
        }

        if status == StatusCode::BAD_REQUEST && indicates_response_format_unsupported(&bytes) {
            return Err(AttemptFailure::ResponseFormatUnsupported400);
        }
        if !status.is_success() {
            // Raw response body is NEVER returned in the error.
            return Err(AttemptFailure::Error(sanitize_http_status(status.as_u16())));
        }

        Ok(bytes)
    }
}

enum AttemptFailure {
    ResponseFormatUnsupported400,
    Error(TranslateError),
}

#[async_trait]
impl TranslationProvider for OpenAiCompatibleProvider {
    fn metadata(&self) -> ProviderMetadata {
        let guard = self
            .endpoint_config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (variant_revision, generation) = match &*guard {
            Some(cfg) => (cfg.variant_revision(), cfg.revision),
            None => ("unconfigured".to_string(), 0),
        };
        ProviderMetadata {
            id: BUILTIN_OPENAI_PROVIDER_ID.to_string(),
            kind: ProviderKind::Builtin,
            display_name: "OpenAI Compatible".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            variant_revision,
            generation,
            capabilities: ProviderCapabilities {
                supported_languages: Vec::new(),
                limits: self.limits.batch_limits,
                supports_cancel: true,
            },
        }
    }

    async fn translate_batch(
        &self,
        request: TranslationBatchRequest,
    ) -> Result<TranslationBatchResponse, TranslateError> {
        let config = {
            let guard = self
                .endpoint_config
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.clone().ok_or_else(|| {
                TranslateError::ProviderUnavailable("built-in provider is not configured".into())
            })?
        };
        self.execute_batch(&config, &request).await
    }

    async fn probe(&self, target_lang: &str) -> ProbeResult {
        let config = {
            let guard = self
                .endpoint_config
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.clone()
        };
        let Some(cfg) = config else {
            return ProbeResult {
                ok: false,
                stage: ProbeStage::Config,
                reason: "translation_provider_unconfigured".to_string(),
                latency_ms: 0,
                route: "unconfigured".to_string(),
                plaintext_lan_transport: false,
            };
        };
        self.probe_config(&cfg, target_lang).await
    }
}

fn build_http_client(
    config: ProxyConfig,
    system: SystemProxySnapshot,
    resolver: Option<Arc<dyn ProxyResolver>>,
    resolve_options: ResolveOptions,
    connect_timeout: Duration,
    pinned_dns: Option<(&str, &[SocketAddr])>,
    #[cfg(any(test, debug_assertions))] accept_invalid_certs: bool,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect_timeout);

    if let Some((host, addrs)) = pinned_dns {
        builder = builder.resolve_to_addrs(host, addrs);
    }

    #[cfg(any(test, debug_assertions))]
    if accept_invalid_certs {
        builder = builder.danger_accept_invalid_certs(true);
    }

    apply_proxy_with_resolver(
        builder,
        config,
        system,
        resolver,
        ProxyContextDefaults::translation(),
        resolve_options,
    )
    .build()
}

fn target_from_identity(identity: &NormalizedServiceIdentity) -> ProxyTarget {
    let scheme = if identity.scheme == "https" {
        TargetScheme::Https
    } else {
        TargetScheme::Http
    };
    ProxyTarget::new(&identity.host, identity.port, scheme)
}

fn format_sanitized_route(route: &ResolvedRoute, transport: EndpointTransport) -> String {
    match route {
        ResolvedRoute::Direct => match transport {
            EndpointTransport::LoopbackHttp | EndpointTransport::LoopbackHttps => {
                "direct (loopback)".to_string()
            }
            EndpointTransport::LanHttp => "direct (lan-plaintext-http)".to_string(),
            EndpointTransport::LanHttps => "direct (lan-https)".to_string(),
            EndpointTransport::PublicHttps => "direct".to_string(),
        },
        ResolvedRoute::Proxy(ep) => {
            let dns_note = match ep.protocol {
                ProxyProtocol::Socks5 => "local-dns",
                ProxyProtocol::Http | ProxyProtocol::Https | ProxyProtocol::Socks5h => "remote-dns",
            };
            format!("proxy {} ({dns_note})", ep.redacted_url())
        }
    }
}

async fn resolve_and_verify_public_dns(
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, TranslateError> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| TranslateError::Network("DNS resolution failed".into()))?
        .collect();
    if addrs.is_empty() {
        return Err(TranslateError::Network("DNS returned no addresses".into()));
    }
    if addrs.iter().any(|addr| !is_public_ip(&addr.ip())) {
        return Err(TranslateError::DnsBlocked);
    }
    Ok(addrs)
}

fn build_chat_completion_payload(
    config: &ResolvedOpenAiConfig,
    request: &TranslationBatchRequest,
    include_response_format: bool,
) -> Result<Vec<u8>, TranslateError> {
    #[derive(Serialize)]
    struct InputItem<'a> {
        id: u32,
        text: &'a str,
    }
    #[derive(Serialize)]
    struct InputEnvelope<'a> {
        items: Vec<InputItem<'a>>,
    }

    let source_clause = match request
        .source_lang
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
    {
        Some(src) => format!("from {src} "),
        None => String::new(),
    };
    let system_prompt = format!(
        "You are a precise email translator. Translate each item's `text` {source_clause}into {}. \
         Preserve every placeholder tag like [[1]]...[[/1]] verbatim without adding, removing, or renumbering tags. \
         Return ONLY a valid JSON object matching {{\"items\":[{{\"id\":0,\"text\":\"...\"}}]}} with the exact same item count and IDs. \
         Never add explanations or commentary.",
        request.target_lang
    );

    let user_payload = serde_json::to_string(&InputEnvelope {
        items: request
            .items
            .iter()
            .map(|b| InputItem {
                id: b.id,
                text: &b.text,
            })
            .collect(),
    })
    .map_err(|_| TranslateError::Io("failed to serialize translation batch".into()))?;

    let mut body = json!({
        "model": config.model,
        "temperature": 0.1,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_payload }
        ]
    });
    if include_response_format {
        body["response_format"] = json!({ "type": "json_object" });
    }

    serde_json::to_vec(&body)
        .map_err(|_| TranslateError::Io("failed to serialize OpenAI request".into()))
}

/// Narrow check: returns true ONLY when a 400 response clearly states that `response_format` /
/// `json_object` is unsupported by the upstream server.
fn indicates_response_format_unsupported(body: &[u8]) -> bool {
    let preview_len = body.len().min(4096);
    let lower = String::from_utf8_lossy(&body[..preview_len]).to_ascii_lowercase();
    let mentions_param = lower.contains("response_format") || lower.contains("json_object");
    let mentions_rejection = lower.contains("unsupported")
        || lower.contains("not support")
        || lower.contains("not allowed")
        || lower.contains("unknown parameter")
        || lower.contains("unrecognized")
        || lower.contains("invalid parameter")
        || lower.contains("invalid_request_error");
    mentions_param && mentions_rejection
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ModelItemsEnvelope {
    items: Vec<TranslatedBlock>,
}

fn parse_and_validate_completion(
    raw_bytes: &[u8],
    expected: &[TextBlock],
    max_expansion_ratio: usize,
    used_fallback: bool,
) -> Result<TranslationBatchResponse, TranslateError> {
    let chat: OpenAiChatResponse = serde_json::from_slice(raw_bytes)
        .map_err(|_| TranslateError::InvalidResponse("malformed upstream JSON envelope".into()))?;
    let content = chat
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| TranslateError::InvalidResponse("missing choice message content".into()))?;

    let trimmed = content.trim();
    let json_str = if used_fallback {
        strip_markdown_json_fence(trimmed)
    } else {
        trimmed
    };

    let envelope: ModelItemsEnvelope = serde_json::from_str(json_str).map_err(|_| {
        TranslateError::InvalidResponse("model output did not match {\"items\":[...]}".into())
    })?;

    let ordered = validate_batch_response(expected, &envelope.items, max_expansion_ratio)?;
    Ok(TranslationBatchResponse {
        items: ordered,
        detected_source_lang: None,
    })
}

fn strip_markdown_json_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let after_lang = rest
            .split_once('\n')
            .map(|(_, body)| body)
            .unwrap_or(rest);
        if let Some(inner) = after_lang.strip_suffix("```") {
            return inner.trim();
        }
    }
    trimmed
}

fn classify_probe_stage(err: &TranslateError) -> ProbeStage {
    match err {
        TranslateError::InvalidConfig(_)
        | TranslateError::CredentialRebindRequired
        | TranslateError::ConfigUnavailable(_)
        | TranslateError::EndpointRejected(_) => ProbeStage::Config,
        TranslateError::ProxyNotReady => ProbeStage::Proxy,
        TranslateError::AuthFailed => ProbeStage::Auth,
        TranslateError::RedirectBlocked
        | TranslateError::DnsBlocked
        | TranslateError::Network(_)
        | TranslateError::Timeout => ProbeStage::Network,
        TranslateError::RateLimited | TranslateError::UpstreamStatus { .. } => ProbeStage::Upstream,
        TranslateError::ResponseTooLarge | TranslateError::InvalidResponse(_) => ProbeStage::Response,
        _ => ProbeStage::Upstream,
    }
}
