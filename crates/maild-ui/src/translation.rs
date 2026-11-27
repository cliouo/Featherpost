//! `maild-ui` translation bridge, active `--config` path propagation, external plugin adapter,
//! proxy & plugin hot-sync, and controlled HTML/plain-text translation orchestration.
//!
//! Security & architectural invariants (ADR-0033, ADR-0050):
//! - Active config directory & cache namespace come from product config resolution (never silently
//!   falling back to default `AppPaths::discover()` when `--config` or `--socket` is supplied).
//! - Built-in `OpenAiCompatibleProvider` waits for the first valid proxy snapshot from `maild-ui`'s
//!   proxy pump and never silently downgrades to `Direct` on fetch failure.
//! - External plugins run through `ExternalTranslationProviderAdapter` over daemon IPC with frozen
//!   `(provider_id, generation, variant_revision)` across all batches; never falls back to built-in
//!   when an external provider fails, disconnects, or runs against an older daemon.
//! - Controlled commands never accept raw HTML or outbound URLs from JS: HTML translation operates
//!   solely on a live `DocumentStore` token, and plain-text translation operates solely on a
//!   controlled `(account, folder, uid)` message fetched from `maild`.
//! - Progress events (`maild://translation-progress`) go ONLY to the owning window and contain
//!   zero plaintext email content, API keys, or raw upstream errors.
//! - Every error crossing into `UiError` is strictly sanitized and mapped to a registered reason.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use async_trait::async_trait;
use maild_client::{
    CancelTranslationResult, ClientError, ErrorKind, InvokeTranslationRequest,
    InvokeTranslationResult, PackageL10n, TestTranslationProviderRequest,
    TestTranslationProviderResult, TranslationBatchItem, TranslationProviderInfo,
    TranslationProvidersResult, PROVIDER_BUSY_CODE, PROVIDER_CANCELLED_CODE, PROVIDER_ERROR_CODE,
    PROVIDER_TIMEOUT_CODE, PROVIDER_UNAVAILABLE_CODE,
};
use maild_paths::{AppPaths, ConfigSource};
use maild_proxy::{detect_system_proxy, ProxyConfig, SystemProxySnapshot};
use maild_translate::{
    detect_language, extract_html_blocks, extract_plain_text, render_html_translation,
    render_plain_text, BatchLimits, CacheClearReport, CacheStats, ContentKind, DetectedLanguage,
    OpenAiCompatibleProvider, OpenAiConfigUpdate, OpenAiHttpLimits, PlainTextBlockView,
    ProbeResult, ProbeStage, ProviderCapabilities, ProviderKind, ProviderMetadata,
    TranslateDocumentRequest, TranslateError, TranslatedBlock, TranslationBatchRequest,
    TranslationBatchResponse, TranslationCache, TranslationConfigStore, TranslationEngine,
    TranslationPreferenceUpdate, TranslationPreferenceView, TranslationProvider,
    TranslationRegistry, BUILTIN_OPENAI_PROVIDER_ID, BUILTIN_PROVIDER_PREFIX,
    DEFAULT_TOTAL_DEADLINE, HOST_DEFAULT_BATCH_LIMITS,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::bridge::{Bridge, UiCompat, UiError, UiErrorCode};
use crate::documents::{DocumentStore, ProtocolBase, TranslateView};
use crate::remote_image::convert_client_proxy_result;

/// Emitted ONLY to the owning window during HTML or plain-text translation.
/// Contains ZERO plaintext email content, API keys, or raw upstream error text.
pub const TRANSLATION_PROGRESS_EVENT: &str = "maild://translation-progress";

/// Emitted whenever the external translation provider catalog changes.
pub const TRANSLATION_PROVIDERS_EVENT: &str = "maild://translation-providers";

/// Active storage locations for `translation_preference.toml` and `translation_cache.sqlite3`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslationStoragePaths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl TranslationStoragePaths {
    /// Resolve storage paths using the same config/endpoint rules as `maild-ui` startup.
    ///
    /// Rules:
    /// - Default platform startup -> `AppPaths.config_dir` & `AppPaths.data_dir`.
    /// - Explicit `--config` (absolute or relative) or `FEATHERPOST_CONFIG` -> active config
    ///   file's parent directory for `config_dir`, and a per-config-file cache namespace under
    ///   `<config_dir>/.translation-cache/<hash>` when non-default (so multiple configs in the
    ///   same directory never collide in SQLite!).
    /// - Legacy working-directory `config.toml` -> cwd for `config_dir`, per-config cache namespace.
    /// - Direct `--socket` / `--pipe` / `--url` (or `FEATHERPOST_API_*` env override) WITHOUT
    ///   `--config` / `FEATHERPOST_CONFIG` -> endpoint-namespaced dirs under
    ///   `AppPaths.{config_dir,data_dir}/translation-namespaces/<hash>` so custom socket sessions
    ///   never pollute the default user configuration or cache.
    pub fn resolve(
        cli_config: Option<&Path>,
        cli_socket: Option<&Path>,
        cli_pipe: Option<&str>,
        cli_url: Option<&str>,
    ) -> Result<Self, String> {
        let app_paths = AppPaths::discover().map_err(|e| e.to_string())?;
        let cwd = std::env::current_dir().map_err(|e| format!("无法获取当前目录: {e}"))?;
        let config_env = std::env::var_os(maild_paths::CONFIG_ENV);
        let endpoint_env = std::env::var_os(maild_client::API_SOCKET_ENV)
            .or_else(|| std::env::var_os(maild_client::API_PIPE_ENV))
            .or_else(|| std::env::var_os(maild_client::API_URL_ENV))
            .map(|v| v.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty());

        Self::resolve_with(
            cli_config,
            cli_socket,
            cli_pipe,
            cli_url,
            config_env.as_deref(),
            endpoint_env.as_deref(),
            &app_paths,
            &cwd,
            |p| p.exists(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_with(
        cli_config: Option<&Path>,
        cli_socket: Option<&Path>,
        cli_pipe: Option<&str>,
        cli_url: Option<&str>,
        config_env: Option<&std::ffi::OsStr>,
        endpoint_env: Option<&str>,
        app_paths: &AppPaths,
        cwd: &Path,
        exists: impl Fn(&Path) -> bool,
    ) -> Result<Self, String> {
        // 1. Explicit `--config` always takes highest precedence.
        if let Some(explicit) = cli_config {
            let abs = absolutize_path(explicit, cwd);
            return Self::from_resolved_config(&abs, ConfigSource::Explicit, app_paths);
        }

        // 2. Direct endpoint override (`--socket` / `--pipe` / `--url` or env) without `--config`.
        let direct_endpoint: Option<String> = if let Some(sock) = cli_socket {
            Some(format!("socket:{}", absolutize_path(sock, cwd).display()))
        } else if let Some(pipe) = cli_pipe {
            Some(format!("pipe:{pipe}"))
        } else if let Some(url) = cli_url {
            Some(format!("url:{url}"))
        } else {
            endpoint_env.map(|e| format!("env:{e}"))
        };

        // Check `FEATHERPOST_CONFIG` env var next.
        if let Some(env_val) = config_env {
            if env_val.is_empty() {
                return Err(format!("{} 不能为空", maild_paths::CONFIG_ENV));
            }
            let path = PathBuf::from(env_val);
            if !path.is_absolute() {
                return Err(format!(
                    "{} 必须是绝对路径: {}",
                    maild_paths::CONFIG_ENV,
                    path.display()
                ));
            }
            return Self::from_resolved_config(&path, ConfigSource::Environment, app_paths);
        }

        // Socket/pipe/url-only session without config -> dedicated endpoint namespace so it
        // never silently shares or overwrites the default platform profile.
        if let Some(ep_key) = direct_endpoint {
            let ns = short_namespace_hash(&ep_key);
            return Ok(Self {
                config_dir: app_paths.config_dir.join("translation-namespaces").join(&ns),
                cache_dir: app_paths.data_dir.join("translation-namespaces").join(&ns),
            });
        }

        // 3. Default discovery (matching `maild_paths::resolve_config` platform vs legacy cwd).
        let platform_exists = exists(&app_paths.config_file);
        let legacy = cwd.join(maild_paths::CONFIG_FILE_NAME);
        let legacy_exists = exists(&legacy);
        if platform_exists && legacy_exists && app_paths.config_file != legacy {
            return Err(format!(
                "平台配置 {:?} 与当前目录配置 {:?} 同时存在；请用 --config 明确选择",
                app_paths.config_file, legacy
            ));
        }
        if platform_exists || !legacy_exists {
            return Ok(Self {
                config_dir: app_paths.config_dir.clone(),
                cache_dir: app_paths.data_dir.clone(),
            });
        }
        Self::from_resolved_config(&legacy, ConfigSource::LegacyWorkingDirectory, app_paths)
    }

    fn from_resolved_config(
        config_file: &Path,
        source: ConfigSource,
        app_paths: &AppPaths,
    ) -> Result<Self, String> {
        if source == ConfigSource::Platform || config_file == app_paths.config_file {
            return Ok(Self {
                config_dir: app_paths.config_dir.clone(),
                cache_dir: app_paths.data_dir.clone(),
            });
        }
        let parent = config_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| format!("配置路径缺少有效父目录: {}", config_file.display()))?
            .to_path_buf();
        let ns = short_namespace_hash(&config_file.to_string_lossy());
        let cache_dir = parent.join(".translation-cache").join(ns);
        Ok(Self {
            config_dir: parent,
            cache_dir,
        })
    }
}

fn absolutize_path(path: &Path, cwd: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut normalized = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn short_namespace_hash(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"fp-translation-ns-v1\0");
    h.update(key.as_bytes());
    let digest = hex::encode(h.finalize());
    digest[..16].to_string()
}

/// Abstract daemon RPC surface used by [`ExternalTranslationProviderAdapter`] and
/// [`TranslationManager`], enabling deterministic mock-daemon testing alongside real [`Bridge`].
#[async_trait]
pub trait DaemonTranslationClient: Send + Sync {
    async fn list_translation_providers(&self) -> Result<TranslationProvidersResult, ClientError>;
    async fn invoke_translation(
        &self,
        request: &InvokeTranslationRequest,
    ) -> Result<InvokeTranslationResult, ClientError>;
    async fn cancel_translation(&self, job_id: &str) -> Result<CancelTranslationResult, ClientError>;
    async fn test_translation_provider(
        &self,
        request: &TestTranslationProviderRequest,
    ) -> Result<TestTranslationProviderResult, ClientError>;
    fn compat(&self) -> UiCompat;
}

#[async_trait]
impl DaemonTranslationClient for Bridge {
    async fn list_translation_providers(&self) -> Result<TranslationProvidersResult, ClientError> {
        self.handle().list_translation_providers().await
    }

    async fn invoke_translation(
        &self,
        request: &InvokeTranslationRequest,
    ) -> Result<InvokeTranslationResult, ClientError> {
        self.handle().invoke_translation(request).await
    }

    async fn cancel_translation(&self, job_id: &str) -> Result<CancelTranslationResult, ClientError> {
        self.handle().cancel_translation(job_id).await
    }

    async fn test_translation_provider(
        &self,
        request: &TestTranslationProviderRequest,
    ) -> Result<TestTranslationProviderResult, ClientError> {
        self.handle().test_translation_provider(request).await
    }

    fn compat(&self) -> UiCompat {
        Bridge::compat(self)
    }
}

/// Distinguish an older `maild` that does not implement translation RPCs (`-32601` Method Not Found
/// OR legacy fail-closed `仅限用户主体` `-32016` on unknown methods) from a real missing capability
/// permission (`-32016` on a compatible daemon).
pub fn is_legacy_translation_unsupported(error: &ClientError, compat: UiCompat) -> bool {
    if error.rpc_code == Some(-32601) {
        return true;
    }
    if error.rpc_code == Some(-32016) {
        if error.message.contains("仅限用户主体") {
            return true;
        }
        if compat == UiCompat::KernelOutdated {
            return true;
        }
    }
    false
}

/// Adapter bridging a registered external Cordis translation plugin over daemon IPC into
/// `maild-translate::TranslationProvider`.
///
/// Freezes `provider_id`, `generation`, and `variant_revision` at construction time so mid-job
/// upgrades/restarts fail closed instead of stitching cross-generation batches.
pub struct ExternalTranslationProviderAdapter {
    daemon: Arc<dyn DaemonTranslationClient>,
    meta: ProviderMetadata,
    raw_variant_revision: String,
}

impl ExternalTranslationProviderAdapter {
    pub fn from_info(
        daemon: Arc<dyn DaemonTranslationClient>,
        info: &TranslationProviderInfo,
    ) -> Self {
        let raw_variant_revision = info.variant_revision.clone();
        let composite_variant = if raw_variant_revision.is_empty() {
            info.version.clone()
        } else {
            format!("{}|{}", info.version, raw_variant_revision)
        };
        let provider_limits = BatchLimits {
            max_batch_items: (info.max_batch_items as usize).max(1),
            max_batch_chars: (info.max_batch_chars as usize).max(1),
            max_concurrency: (info.max_concurrency as usize).max(1),
        };
        let negotiated = BatchLimits::negotiate(HOST_DEFAULT_BATCH_LIMITS, provider_limits);
        let meta = ProviderMetadata {
            id: info.id.clone(),
            kind: ProviderKind::External,
            display_name: info.name.clone(),
            version: info.version.clone(),
            variant_revision: composite_variant,
            generation: info.generation,
            capabilities: ProviderCapabilities {
                supported_languages: info.supported_languages.clone(),
                limits: negotiated,
                supports_cancel: info.supports_cancel,
            },
        };
        Self {
            daemon,
            meta,
            raw_variant_revision,
        }
    }
}

#[async_trait]
impl TranslationProvider for ExternalTranslationProviderAdapter {
    fn metadata(&self) -> ProviderMetadata {
        self.meta.clone()
    }

    async fn translate_batch(
        &self,
        request: TranslationBatchRequest,
    ) -> Result<TranslationBatchResponse, TranslateError> {
        let timeout_ms = match request.deadline {
            Some(dl) => {
                let now = Instant::now();
                if now >= dl {
                    return Err(TranslateError::Timeout);
                }
                let rem = dl.saturating_duration_since(now).as_millis() as u64;
                Some(rem.max(1))
            }
            None => Some(DEFAULT_TOTAL_DEADLINE.as_millis() as u64),
        };

        let wire_req = InvokeTranslationRequest {
            provider_id: self.meta.id.clone(),
            generation: self.meta.generation,
            job_id: request.job_id.clone(),
            batch_id: Some(format!("{}:b{}", request.job_id, request.batch_index)),
            source_lang: request
                .source_lang
                .unwrap_or_else(|| "auto".to_string()),
            target_lang: request.target_lang,
            content_kind: request.content_kind.as_str().to_string(),
            timeout_ms,
            items: request
                .items
                .into_iter()
                .map(|b| TranslationBatchItem {
                    id: b.id,
                    text: b.text,
                    context: None,
                })
                .collect(),
        };

        let resp = self
            .daemon
            .invoke_translation(&wire_req)
            .await
            .map_err(|err| map_daemon_invoke_error(&err, self.daemon.compat()))?;

        // Fail closed if daemon returned a different provider, generation, or variant_revision.
        if resp.provider_id != self.meta.id || resp.generation != self.meta.generation {
            return Err(TranslateError::ProviderUnavailable(
                "provider_generation_changed".into(),
            ));
        }
        if !resp.variant_revision.is_empty()
            && !self.raw_variant_revision.is_empty()
            && resp.variant_revision != self.raw_variant_revision
        {
            return Err(TranslateError::ProviderUnavailable(
                "provider_variant_changed".into(),
            ));
        }

        Ok(TranslationBatchResponse {
            items: resp
                .items
                .into_iter()
                .map(|item| TranslatedBlock {
                    id: item.id,
                    text: item.text,
                })
                .collect(),
            detected_source_lang: resp.detected_source_lang,
        })
    }

    async fn probe(&self, target_lang: &str) -> ProbeResult {
        let started = Instant::now();
        let req = TestTranslationProviderRequest {
            provider_id: self.meta.id.clone(),
            generation: Some(self.meta.generation),
            target_lang: target_lang.trim().to_string(),
            timeout_ms: Some(15_000),
        };
        match self.daemon.test_translation_provider(&req).await {
            Ok(res) => sanitize_external_probe_result(res, started.elapsed().as_millis() as u64),
            Err(err) => {
                let mapped = map_daemon_invoke_error(&err, self.daemon.compat());
                ProbeResult {
                    ok: false,
                    stage: classify_external_probe_stage(&mapped),
                    reason: sanitize_probe_reason_code(&mapped).into(),
                    latency_ms: started.elapsed().as_millis() as u64,
                    route: "ipc://daemon".into(),
                    plaintext_lan_transport: false,
                }
            }
        }
    }

    async fn cancel(&self, job_id: &str) -> Result<(), TranslateError> {
        let _ = self.daemon.cancel_translation(job_id).await;
        Ok(())
    }
}

fn map_daemon_invoke_error(err: &ClientError, compat: UiCompat) -> TranslateError {
    if is_legacy_translation_unsupported(err, compat) {
        return TranslateError::ProviderUnavailable("external_unsupported".into());
    }
    match (err.kind, err.rpc_code) {
        (ErrorKind::Connection, _) => TranslateError::ProviderUnavailable("daemon_offline".into()),
        (_, Some(PROVIDER_UNAVAILABLE_CODE)) => {
            TranslateError::ProviderUnavailable("provider_unavailable".into())
        }
        (_, Some(PROVIDER_TIMEOUT_CODE)) => TranslateError::Timeout,
        (_, Some(PROVIDER_ERROR_CODE)) => TranslateError::UpstreamStatus {
            status: 502,
            reason: "external provider error".into(),
        },
        (_, Some(PROVIDER_BUSY_CODE)) => TranslateError::ProviderUnavailable("provider_busy".into()),
        (_, Some(PROVIDER_CANCELLED_CODE)) => TranslateError::Cancelled,
        (_, Some(-32016)) => TranslateError::ProviderUnavailable("permission_denied".into()),
        (_, Some(-32602)) => TranslateError::InvalidConfig("invalid_params".into()),
        _ => TranslateError::UpstreamStatus {
            status: 502,
            reason: "external provider failed".into(),
        },
    }
}

fn sanitize_external_probe_result(
    res: TestTranslationProviderResult,
    fallback_latency_ms: u64,
) -> ProbeResult {
    let latency_ms = if res.latency_ms > 0 {
        res.latency_ms
    } else {
        fallback_latency_ms
    };
    if res.ok {
        return ProbeResult {
            ok: true,
            stage: ProbeStage::Done,
            reason: "ok".into(),
            latency_ms,
            route: "ipc://daemon".into(),
            plaintext_lan_transport: false,
        };
    }
    let stage = match res.stage.trim().to_ascii_lowercase().as_str() {
        "config" => ProbeStage::Config,
        "proxy" => ProbeStage::Proxy,
        "network" | "connect" | "ipc" => ProbeStage::Network,
        "tls" => ProbeStage::Tls,
        "auth" => ProbeStage::Auth,
        "response" | "validate" => ProbeStage::Response,
        "done" => ProbeStage::Done,
        _ => ProbeStage::Upstream,
    };
    // Project daemon reason onto a finite sanitized set; NEVER forward arbitrary upstream `detail`.
    let reason = match res.reason.trim().to_ascii_lowercase().as_str() {
        "ok" => "ok",
        "unavailable" | "provider_unavailable" | "not_registered" | "disabled" => {
            "translation_provider_unavailable"
        }
        "timeout" | "provider_timeout" => "translation_timeout",
        "busy" | "provider_busy" | "overloaded" => "translation_provider_busy",
        "cancelled" | "canceled" => "translation_cancelled",
        "auth" | "auth_failed" => "translation_auth_failed",
        "invalid_response" => "translation_invalid_response",
        _ => "translation_upstream_error",
    };
    ProbeResult {
        ok: false,
        stage,
        reason: reason.into(),
        latency_ms,
        route: "ipc://daemon".into(),
        plaintext_lan_transport: false,
    }
}

fn classify_external_probe_stage(err: &TranslateError) -> ProbeStage {
    match err {
        TranslateError::InvalidConfig(_) => ProbeStage::Config,
        TranslateError::Timeout | TranslateError::Network(_) => ProbeStage::Network,
        TranslateError::AuthFailed => ProbeStage::Auth,
        TranslateError::InvalidResponse(_) | TranslateError::ResponseTooLarge => {
            ProbeStage::Response
        }
        _ => ProbeStage::Upstream,
    }
}

fn sanitize_probe_reason_code(err: &TranslateError) -> &'static str {
    match err {
        TranslateError::ProviderUnavailable(tag) if tag == "external_unsupported" => {
            "translation_external_unsupported"
        }
        TranslateError::ProviderUnavailable(tag) if tag == "provider_busy" => {
            "translation_provider_busy"
        }
        TranslateError::ProviderUnavailable(tag) if tag == "permission_denied" => "denied",
        other => other.reason_code(),
    }
}

/// Sanitized projection of [`TranslateError`] into [`UiError`].
///
/// Logs ONLY the stable reason code to stderr — never logs email text, API keys, TOML source
/// lines, or raw upstream HTTP response bodies.
pub fn translate_error_to_ui(err: TranslateError) -> UiError {
    eprintln!("[maild-ui/translation] 操作失败: reason={}", err.reason_code());
    let (code, reason, message) = match err {
        TranslateError::InvalidConfig(_) => (
            UiErrorCode::Invalid,
            "translation_invalid_config",
            "翻译配置无效，请检查服务地址、模型与参数。",
        ),
        TranslateError::CredentialRebindRequired => (
            UiErrorCode::Invalid,
            "translation_credential_rebind_required",
            "服务地址已变更，请重新输入新服务的 API 密钥，或切换为无密钥本地回环服务。",
        ),
        TranslateError::ConfigUnavailable(_) => (
            UiErrorCode::Failed,
            "translation_config_unavailable",
            "翻译配置文件损坏、权限不安全或版本过新，已停止读写。",
        ),
        TranslateError::RevisionConflict { .. } => (
            UiErrorCode::Conflict,
            "translation_revision_conflict",
            "翻译设置已在别处修改，请刷新后重新保存。",
        ),
        TranslateError::ProxyNotReady => (
            UiErrorCode::Failed,
            "translation_proxy_not_ready",
            "代理配置尚未同步完成，请稍后重试。",
        ),
        TranslateError::ProviderUnavailable(ref tag) if tag == "external_unsupported" => (
            UiErrorCode::Failed,
            "translation_external_unsupported",
            "当前后台版本不支持外部翻译插件。",
        ),
        TranslateError::ProviderUnavailable(ref tag) if tag == "provider_busy" => (
            UiErrorCode::Limit,
            "translation_provider_busy",
            "翻译插件当前负载较高，请稍后重试。",
        ),
        TranslateError::ProviderUnavailable(ref tag) if tag == "permission_denied" => (
            UiErrorCode::Denied,
            "denied",
            "当前前端没有执行这项操作的授权。",
        ),
        TranslateError::ProviderUnavailable(ref tag) if tag == "not_configured" => (
            UiErrorCode::Invalid,
            "translation_provider_not_configured",
            "尚未配置可用的翻译服务，请先在「设置 › 翻译」完成配置。",
        ),
        TranslateError::ProviderUnavailable(_) => (
            UiErrorCode::Failed,
            "translation_provider_unavailable",
            "所选翻译服务当前不可用、已离线或版本已变更。",
        ),
        TranslateError::EndpointRejected(_) => (
            UiErrorCode::Invalid,
            "translation_endpoint_rejected",
            "翻译服务地址不符合安全要求（公网仅允许 HTTPS，局域网明文需显式确认）。",
        ),
        TranslateError::RedirectBlocked => (
            UiErrorCode::Failed,
            "translation_redirect_blocked",
            "翻译服务返回了重定向响应，出于安全原因已拦截。",
        ),
        TranslateError::DnsBlocked => (
            UiErrorCode::Failed,
            "translation_dns_blocked",
            "公网翻译服务的域名解析到了非公网地址，已拦截。",
        ),
        TranslateError::AuthFailed => (
            UiErrorCode::Failed,
            "translation_auth_failed",
            "翻译服务认证失败，请检查 API 密钥。",
        ),
        TranslateError::RateLimited => (
            UiErrorCode::Limit,
            "translation_rate_limited",
            "翻译请求频率超限，请稍后重试。",
        ),
        TranslateError::UpstreamStatus { .. } => (
            UiErrorCode::Failed,
            "translation_upstream_error",
            "翻译服务返回了错误响应，请稍后重试。",
        ),
        TranslateError::ResponseTooLarge => (
            UiErrorCode::Failed,
            "translation_response_too_large",
            "翻译服务返回内容超过大小限制。",
        ),
        TranslateError::InvalidResponse(_) => (
            UiErrorCode::Failed,
            "translation_invalid_response",
            "翻译服务返回的数据格式无效。",
        ),
        TranslateError::Network(_) => (
            UiErrorCode::Failed,
            "translation_network_error",
            "连接翻译服务失败，请检查网络与代理设置。",
        ),
        TranslateError::Timeout => (
            UiErrorCode::Failed,
            "translation_timeout",
            "翻译请求超时，请稍后重试。",
        ),
        TranslateError::Cancelled => (
            UiErrorCode::Failed,
            "translation_cancelled",
            "翻译已取消。",
        ),
        TranslateError::Cache(_) => (
            UiErrorCode::Failed,
            "translation_cache_error",
            "本地翻译缓存读写失败。",
        ),
        TranslateError::Io(_) => (
            UiErrorCode::Failed,
            "translation_io_error",
            "翻译配置文件读写失败。",
        ),
    };
    UiError {
        code,
        reason,
        message: message.into(),
    }
}

// —— Controlled IPC DTOs ——

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiTranslationProviderEntry {
    pub id: String,
    pub kind: ProviderKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "PackageL10n::is_empty")]
    pub l10n: PackageL10n,
    pub version: String,
    pub lifecycle: String,
    pub enabled: bool,
    pub state: String,
    pub has_ui: bool,
    pub registered: bool,
    pub configured: bool,
    pub proxy_ready: bool,
    pub generation: u64,
    pub supported_languages: Vec<String>,
    pub max_batch_items: u32,
    pub max_batch_chars: u32,
    pub max_concurrency: u32,
    pub supports_cancel: bool,
    pub variant_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiTranslationProvidersResponse {
    pub catalog_generation: u64,
    pub external_supported: bool,
    pub providers: Vec<UiTranslationProviderEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTestTranslationProviderRequest {
    pub provider_id: String,
    pub target_lang: String,
    /// Optional unsaved OpenAI-compatible draft config (only allowed for `builtin.openai_compatible`).
    #[serde(default)]
    pub draft_openai: Option<OpenAiConfigUpdate>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiDetectLanguageRequest {
    /// Controlled HTML document token (preferred for HTML emails).
    #[serde(default)]
    pub token: Option<String>,
    /// Controlled message identity (for plain-text or non-prepared messages).
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub uid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UiLanguageDetection {
    pub language: DetectedLanguage,
    pub confidence: f32,
    pub effective_chars: usize,
    pub target_lang: String,
    pub matches_target: bool,
    pub is_ignored: bool,
    /// True only when a provider is configured, detect banner is enabled, language is known,
    /// differs from `target_lang`, and is not in `ignored_languages`.
    pub should_offer_banner: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTranslateHtmlRequest {
    pub token: String,
    #[serde(default)]
    pub target_lang: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub force_refresh: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiHtmlTranslationResult {
    pub job_id: String,
    pub token: String,
    pub rev: u64,
    pub bilingual_url: String,
    pub translated_url: String,
    pub bilingual_dark_url: String,
    pub translated_dark_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_subject: Option<String>,
    pub source_lang: String,
    pub target_lang: String,
    pub provider_id: String,
    pub provider_generation: u64,
    pub from_cache: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTranslatePlainRequest {
    pub account: String,
    pub folder: String,
    pub uid: String,
    #[serde(default)]
    pub target_lang: Option<String>,
    #[serde(default)]
    pub provider_id: Option<String>,
    #[serde(default)]
    pub force_refresh: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiPlainTranslationResult {
    pub job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated_subject: Option<String>,
    pub blocks: Vec<PlainTextBlockView>,
    pub source_lang: String,
    pub target_lang: String,
    pub provider_id: String,
    pub provider_generation: u64,
    pub from_cache: bool,
}

/// Progress event emitted ONLY to the owning window (`maild://translation-progress`).
/// Deliberately contains ZERO email text, keys, or raw error details.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UiTranslationProgress {
    pub job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub completed_batches: usize,
    pub total_batches: usize,
    /// `"running"` | `"completed"` | `"cancelled"` | `"failed"`
    pub status: &'static str,
}

#[derive(Default)]
struct ProxySyncTracker {
    last_event_generation: u64,
    snapshot_seq: u64,
}

struct ExternalCatalogState {
    catalog_generation: u64,
    providers: Vec<TranslationProviderInfo>,
    external_supported: bool,
    synced: bool,
}

impl Default for ExternalCatalogState {
    fn default() -> Self {
        Self {
            catalog_generation: 0,
            providers: Vec::new(),
            external_supported: true,
            synced: false,
        }
    }
}

struct ActiveManagerJob {
    provider_id: String,
    cancel_flag: Arc<AtomicBool>,
    cancel_notify: Arc<tokio::sync::Notify>,
}

/// Central translation orchestrator owned by `AppState`.
pub struct TranslationManager {
    storage_paths: TranslationStoragePaths,
    config_store: Arc<TranslationConfigStore>,
    cache: Arc<TranslationCache>,
    openai: Arc<OpenAiCompatibleProvider>,
    registry: Arc<TranslationRegistry>,
    engine: Arc<TranslationEngine>,
    daemon: Arc<dyn DaemonTranslationClient>,
    proxy_sync: Mutex<ProxySyncTracker>,
    external_catalog: RwLock<ExternalCatalogState>,
    active_jobs: Mutex<HashMap<String, ActiveManagerJob>>,
    next_job_seq: AtomicU64,
}

impl TranslationManager {
    pub fn new(
        storage_paths: TranslationStoragePaths,
        daemon: Arc<dyn DaemonTranslationClient>,
    ) -> Result<Self, TranslateError> {
        Self::with_limits(storage_paths, daemon, OpenAiHttpLimits::default())
    }

    pub fn with_limits(
        storage_paths: TranslationStoragePaths,
        daemon: Arc<dyn DaemonTranslationClient>,
        limits: OpenAiHttpLimits,
    ) -> Result<Self, TranslateError> {
        ensure_private_dir(&storage_paths.config_dir)?;
        ensure_private_dir(&storage_paths.cache_dir)?;

        let config_store = Arc::new(TranslationConfigStore::open(&storage_paths.config_dir));
        let cache = Arc::new(TranslationCache::open(&storage_paths.cache_dir)?);
        let openai = Arc::new(OpenAiCompatibleProvider::new(limits));
        openai.set_config(config_store.resolved_openai_config());

        let registry = Arc::new(TranslationRegistry::new());
        registry.register_builtin(openai.clone())?;
        let engine = Arc::new(TranslationEngine::new(registry.clone(), cache.clone()));

        Ok(Self {
            storage_paths,
            config_store,
            cache,
            openai,
            registry,
            engine,
            daemon,
            proxy_sync: Mutex::new(ProxySyncTracker::default()),
            external_catalog: RwLock::new(ExternalCatalogState::default()),
            active_jobs: Mutex::new(HashMap::new()),
            next_job_seq: AtomicU64::new(1),
        })
    }

    pub fn storage_paths(&self) -> &TranslationStoragePaths {
        &self.storage_paths
    }

    pub fn openai_provider(&self) -> &Arc<OpenAiCompatibleProvider> {
        &self.openai
    }

    pub fn registry(&self) -> &Arc<TranslationRegistry> {
        &self.registry
    }

    pub fn cache(&self) -> &Arc<TranslationCache> {
        &self.cache
    }

    /// Apply a validated proxy snapshot to the built-in OpenAI-compatible provider.
    ///
    /// - When `incoming_generation > 0` (from `preferences/proxy` push), rejects stale generations.
    /// - When `incoming_generation == 0` (full resync on connect/reconnect/lag), advances the local
    ///   monotonic snapshot sequence so the provider receives fresh credentials/routes.
    pub fn apply_proxy_snapshot(
        &self,
        config: ProxyConfig,
        system: SystemProxySnapshot,
        incoming_generation: u64,
    ) {
        let mut tracker = self
            .proxy_sync
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if incoming_generation > 0 {
            if incoming_generation <= tracker.last_event_generation {
                return;
            }
            tracker.last_event_generation = incoming_generation;
        }
        tracker.snapshot_seq = tracker.snapshot_seq.saturating_add(1).max(incoming_generation);
        let seq = tracker.snapshot_seq;
        drop(tracker);

        if let Err(err) = self.openai.apply_proxy_snapshot(config, system, seq) {
            eprintln!(
                "[maild-ui/translation] 应用代理快照失败: reason={}",
                err.reason_code()
            );
        }
    }

    /// Refresh built-in provider's proxy client from `maild` over local stream IPC.
    ///
    /// - On `proxy_unsupported` (old daemon without proxy RPC): applies default `ProxyMode::System`
    ///   + OS system snapshot so built-in translation works cleanly.
    /// - On any other fetch failure: records failure WITHOUT downgrading to `Direct`.
    pub async fn refresh_proxy_from_bridge(&self, bridge: &Bridge, incoming_generation: u64) {
        match bridge.proxy_config_with_secret().await {
            Ok(result) => {
                let (config, system) = convert_client_proxy_result(&result);
                self.apply_proxy_snapshot(config, system, incoming_generation);
            }
            Err(error) if error.reason == "proxy_unsupported" => {
                self.apply_proxy_snapshot(
                    ProxyConfig::default(),
                    detect_system_proxy(),
                    incoming_generation,
                );
            }
            Err(error) => {
                self.openai.record_proxy_fetch_failure();
                eprintln!(
                    "[maild-ui/translation] 同步代理配置失败: code={:?}, reason={}",
                    error.code, error.reason
                );
            }
        }
    }

    /// Merge an incoming `translation/providers` push or `listTranslationProviders` snapshot by
    /// monotonic `generation` and sync [`TranslationRegistry`].
    ///
    /// Returns `Some(merged_view)` if the snapshot was applied (newer or equal on initial sync),
    /// or `None` if dropped as stale.
    pub fn apply_external_providers_snapshot(
        &self,
        snapshot: TranslationProvidersResult,
    ) -> Option<UiTranslationProvidersResponse> {
        {
            let mut cat = self
                .external_catalog
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cat.synced && snapshot.generation < cat.catalog_generation {
                return None;
            }
            cat.catalog_generation = snapshot.generation;
            cat.providers = snapshot.providers.clone();
            cat.external_supported = true;
            cat.synced = true;
        }

        self.sync_registry_external_providers(&snapshot.providers);
        Some(self.build_providers_response())
    }

    fn sync_registry_external_providers(&self, providers: &[TranslationProviderInfo]) {
        let mut active_ids = HashSet::new();
        for info in providers {
            if info.id.is_empty() || info.id.starts_with(BUILTIN_PROVIDER_PREFIX) {
                continue;
            }
            if info.enabled && info.registered && info.generation > 0 {
                let adapter = Arc::new(ExternalTranslationProviderAdapter::from_info(
                    Arc::clone(&self.daemon),
                    info,
                ));
                if self.registry.register_external(adapter).is_ok() {
                    active_ids.insert(info.id.clone());
                }
            }
        }

        // Unregister any external provider that was disabled, unregistered, or removed.
        for existing in self.registry.list() {
            if existing.kind == ProviderKind::External && !active_ids.contains(&existing.id) {
                self.registry.unregister(&existing.id);
            }
        }
    }

    /// Perform a full resync of external translation providers from daemon (`listTranslationProviders`).
    ///
    /// - Old daemon (`-32601` / `UserOnly -32016` / `KernelOutdated`): marks `external_supported = false`,
    ///   unregisters any stale external adapters, and returns the built-in provider cleanly.
    /// - Offline daemon: retains built-in provider, marks external providers offline/unregistered.
    /// - Real `-32016` missing permission on a compatible daemon: surfaces `UiErrorCode::Denied`.
    pub async fn sync_external_providers_from_daemon(
        &self,
    ) -> Result<UiTranslationProvidersResponse, UiError> {
        match self.daemon.list_translation_providers().await {
            Ok(snapshot) => {
                let _ = self.apply_external_providers_snapshot(snapshot);
                Ok(self.build_providers_response())
            }
            Err(err) if is_legacy_translation_unsupported(&err, self.daemon.compat()) => {
                {
                    let mut cat = self
                        .external_catalog
                        .write()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    cat.external_supported = false;
                    cat.providers.clear();
                    cat.synced = true;
                }
                self.sync_registry_external_providers(&[]);
                Ok(self.build_providers_response())
            }
            Err(err) if err.kind == ErrorKind::Connection => {
                // Daemon offline: clear active external registrations so we don't attempt stale
                // calls, but keep built-in provider inspectable.
                self.sync_registry_external_providers(&[]);
                Ok(self.build_providers_response())
            }
            Err(err) if err.rpc_code == Some(-32016) => Err(UiError {
                code: UiErrorCode::Denied,
                reason: "denied",
                message: "当前前端没有执行这项操作的授权。".into(),
            }),
            Err(_) => {
                // Transient daemon error: degrade list safely to current catalog.
                Ok(self.build_providers_response())
            }
        }
    }

    pub fn build_providers_response(&self) -> UiTranslationProvidersResponse {
        let pref = self.config_store.view();
        let builtin_meta = self.openai.metadata();
        let builtin_configured = pref.state_healthy && pref.openai_compatible.is_some();
        let builtin_proxy_ready = self.openai.is_proxy_ready();

        let mut entries = vec![UiTranslationProviderEntry {
            id: builtin_meta.id,
            kind: ProviderKind::Builtin,
            name: builtin_meta.display_name,
            description: None,
            l10n: PackageL10n::default(),
            version: builtin_meta.version,
            lifecycle: "builtin".into(),
            enabled: true,
            state: if builtin_configured {
                "ready".into()
            } else {
                "unconfigured".into()
            },
            has_ui: false,
            registered: true,
            configured: builtin_configured,
            proxy_ready: builtin_proxy_ready,
            generation: builtin_meta.generation,
            supported_languages: builtin_meta.capabilities.supported_languages,
            max_batch_items: builtin_meta.capabilities.limits.max_batch_items as u32,
            max_batch_chars: builtin_meta.capabilities.limits.max_batch_chars as u32,
            max_concurrency: builtin_meta.capabilities.limits.max_concurrency as u32,
            supports_cancel: builtin_meta.capabilities.supports_cancel,
            variant_revision: builtin_meta.variant_revision,
        }];

        let (catalog_generation, external_supported, external_list) = {
            let cat = self
                .external_catalog
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                cat.catalog_generation,
                cat.external_supported,
                cat.providers.clone(),
            )
        };

        for info in external_list {
            if info.id.is_empty() || info.id.starts_with(BUILTIN_PROVIDER_PREFIX) {
                continue;
            }
            let registered_in_registry = self.registry.get(&info.id).is_some();
            entries.push(UiTranslationProviderEntry {
                id: info.id,
                kind: ProviderKind::External,
                name: info.name,
                description: info.description,
                l10n: info.l10n,
                version: info.version,
                lifecycle: info.lifecycle,
                enabled: info.enabled,
                state: info.state,
                has_ui: info.has_ui,
                registered: info.registered && registered_in_registry,
                configured: info.enabled && info.registered && registered_in_registry,
                proxy_ready: true,
                generation: info.generation,
                supported_languages: info.supported_languages,
                max_batch_items: info.max_batch_items,
                max_batch_chars: info.max_batch_chars,
                max_concurrency: info.max_concurrency,
                supports_cancel: info.supports_cancel,
                variant_revision: info.variant_revision,
            });
        }

        UiTranslationProvidersResponse {
            catalog_generation,
            external_supported,
            providers: entries,
        }
    }

    pub fn get_preference(&self) -> TranslationPreferenceView {
        self.config_store.view()
    }

    pub fn save_preference(
        &self,
        update: TranslationPreferenceUpdate,
    ) -> Result<TranslationPreferenceView, UiError> {
        let view = self
            .config_store
            .save(&update)
            .map_err(translate_error_to_ui)?;
        self.openai.set_config(self.config_store.resolved_openai_config());
        Ok(view)
    }

    pub async fn test_provider(
        &self,
        request: UiTestTranslationProviderRequest,
    ) -> Result<ProbeResult, UiError> {
        let provider_id = request.provider_id.trim();
        let target_lang = request.target_lang.trim();
        if provider_id.is_empty() || target_lang.is_empty() {
            return Err(UiError::invalid(
                "translation_invalid_config",
                "翻译配置无效，请检查服务地址、模型与参数。",
            ));
        }

        if provider_id == BUILTIN_OPENAI_PROVIDER_ID {
            if let Some(draft) = request.draft_openai.as_ref() {
                let resolved = self
                    .config_store
                    .resolve_draft_openai(draft)
                    .map_err(translate_error_to_ui)?;
                return Ok(self.openai.probe_config(&resolved, target_lang).await);
            }
            return Ok(self.openai.probe(target_lang).await);
        }

        if request.draft_openai.is_some() {
            return Err(UiError::invalid(
                "translation_invalid_config",
                "翻译配置无效，请检查服务地址、模型与参数。",
            ));
        }

        // External provider: NEVER redirect to built-in!
        let _ = self.sync_external_providers_from_daemon().await;
        if !self
            .external_catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .external_supported
        {
            return Err(translate_error_to_ui(TranslateError::ProviderUnavailable(
                "external_unsupported".into(),
            )));
        }

        let Some(provider) = self.registry.get(provider_id) else {
            return Err(translate_error_to_ui(TranslateError::ProviderUnavailable(
                "provider_unavailable".into(),
            )));
        };
        Ok(provider.probe(target_lang).await)
    }

    /// Detect language for a controlled sanitized HTML document in [`DocumentStore`].
    pub fn detect_language_for_document(
        &self,
        store: &DocumentStore,
        base: &ProtocolBase,
        token: &str,
    ) -> Result<UiLanguageDetection, UiError> {
        let doc = store.get(token).ok_or_else(|| {
            UiError::not_found("document_released", "文档已释放或不存在。")
        })?;
        let link_prefix = base.link_prefix(token);
        let extracted = extract_html_blocks(&doc.html, Some(&doc.subject), &link_prefix)
            .map_err(translate_error_to_ui)?;
        let body_sample = extracted.body_text_for_detection();
        let subj = (!doc.subject.trim().is_empty()).then_some(doc.subject.as_str());
        Ok(self.evaluate_detection(&body_sample, subj))
    }

    /// Detect language for a controlled plain-text (or raw-fetched) message.
    pub fn detect_language_for_plain_message(
        &self,
        subject: &str,
        body: &str,
    ) -> UiLanguageDetection {
        let extracted = extract_plain_text(body, Some(subject));
        let body_sample = extracted.body_text_for_detection();
        let subj = (!subject.trim().is_empty()).then_some(subject);
        self.evaluate_detection(&body_sample, subj)
    }

    fn evaluate_detection(&self, body_text: &str, subject: Option<&str>) -> UiLanguageDetection {
        let pref = self.config_store.view();
        let det = detect_language(body_text, subject);
        let matches_target = det.language.matches_target(&pref.target_lang);
        let is_ignored = det.language.is_ignored(&pref.ignored_languages);
        let has_active_provider = self.is_active_provider_ready(&pref);
        let should_offer_banner = pref.state_healthy
            && pref.detect_banner_enabled
            && has_active_provider
            && !det.language.is_unknown()
            && !matches_target
            && !is_ignored;
        UiLanguageDetection {
            language: det.language,
            confidence: det.confidence,
            effective_chars: det.effective_chars,
            target_lang: pref.target_lang,
            matches_target,
            is_ignored,
            should_offer_banner,
        }
    }

    fn is_active_provider_ready(&self, pref: &TranslationPreferenceView) -> bool {
        if !pref.state_healthy || pref.active_provider_id.is_empty() {
            return false;
        }
        if pref.active_provider_id == BUILTIN_OPENAI_PROVIDER_ID {
            pref.openai_compatible.is_some()
        } else {
            self.registry.get(&pref.active_provider_id).is_some()
        }
    }

    fn allocate_job_id(&self) -> String {
        let seq = self.next_job_seq.fetch_add(1, Ordering::SeqCst);
        let mut rnd = [0u8; 4];
        let _ = getrandom::fill(&mut rnd);
        format!("tr-{seq}-{}", hex::encode(rnd))
    }

    fn resolve_provider_and_target(
        &self,
        requested_provider: Option<&str>,
        requested_target: Option<&str>,
    ) -> Result<(String, u64, String), UiError> {
        let pref = self.config_store.view();
        if !pref.state_healthy {
            return Err(translate_error_to_ui(TranslateError::ConfigUnavailable(
                "config_unhealthy".into(),
            )));
        }
        let provider_id = requested_provider
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(pref.active_provider_id.as_str())
            .to_string();
        if provider_id.is_empty() {
            return Err(translate_error_to_ui(TranslateError::ProviderUnavailable(
                "not_configured".into(),
            )));
        }
        let target_lang = requested_target
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(pref.target_lang.as_str())
            .to_string();
        if target_lang.is_empty() {
            return Err(translate_error_to_ui(TranslateError::InvalidConfig(
                "target_lang required".into(),
            )));
        }

        if provider_id == BUILTIN_OPENAI_PROVIDER_ID {
            if pref.openai_compatible.is_none() {
                return Err(translate_error_to_ui(TranslateError::ProviderUnavailable(
                    "not_configured".into(),
                )));
            }
            let gen = self.openai.metadata().generation;
            return Ok((provider_id, gen, target_lang));
        }

        // External provider: fail closed if old daemon does not support external providers or
        // provider is offline/unregistered. NEVER fall back to built-in!
        let ext_supported = self
            .external_catalog
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .external_supported;
        if !ext_supported {
            return Err(translate_error_to_ui(TranslateError::ProviderUnavailable(
                "external_unsupported".into(),
            )));
        }
        let provider = self.registry.get(&provider_id).ok_or_else(|| {
            translate_error_to_ui(TranslateError::ProviderUnavailable(
                "provider_unavailable".into(),
            ))
        })?;
        let gen = provider.metadata().generation;
        Ok((provider_id, gen, target_lang))
    }

    /// Translate a controlled HTML document identified solely by its live `DocumentStore` token.
    ///
    /// - Extracts blocks via implicit-close-safe `lol_html` Pass 1.
    /// - Freezes `(provider_id, provider_generation, variant_revision)` across all batches.
    /// - Emits sanitized progress events via `on_progress` (bound to owning window).
    /// - Cancels cleanly if `cancel_job` or document `release`/`evict` occurs mid-flight.
    /// - Verifies `token` liveness, `Arc::ptr_eq`, and `job_id` before committing to `Document`.
    /// - Renders BOTH bilingual and translated-only HTML so switching `display_mode` costs 0 RPCs.
    pub async fn translate_html_document<F>(
        &self,
        store: &DocumentStore,
        base: &ProtocolBase,
        request: UiTranslateHtmlRequest,
        mut on_progress: F,
    ) -> Result<UiHtmlTranslationResult, UiError>
    where
        F: FnMut(UiTranslationProgress),
    {
        let doc = store.get(&request.token).ok_or_else(|| {
            UiError::not_found("document_released", "文档已释放或不存在。")
        })?;

        let (provider_id, expected_generation, target_lang) = self.resolve_provider_and_target(
            request.provider_id.as_deref(),
            request.target_lang.as_deref(),
        )?;

        let link_prefix = base.link_prefix(&request.token);
        let extracted = extract_html_blocks(&doc.html, Some(&doc.subject), &link_prefix)
            .map_err(translate_error_to_ui)?;
        let body_sample = extracted.body_text_for_detection();
        let subj = (!doc.subject.trim().is_empty()).then_some(doc.subject.as_str());
        let detected = detect_language(&body_sample, subj);
        let source_lang = detected.language.as_tag().to_string();

        let job_id = self.allocate_job_id();
        let (cancel_flag, cancel_notify, replaced_job_id) =
            doc.begin_translation_job(job_id.clone());
        if let Some(prev_id) = replaced_job_id {
            self.notify_daemon_cancel_if_external(&prev_id).await;
        }
        self.register_active_job(
            job_id.clone(),
            provider_id.clone(),
            Arc::clone(&cancel_flag),
            Arc::clone(&cancel_notify),
        );

        let mut engine_req = TranslateDocumentRequest::new(
            job_id.clone(),
            provider_id.clone(),
            source_lang.clone(),
            target_lang.clone(),
            ContentKind::Html,
            extracted.blocks.clone(),
        );
        engine_req.expected_generation = Some(expected_generation);
        engine_req.force_refresh = request.force_refresh;
        engine_req.total_deadline = Some(DEFAULT_TOTAL_DEADLINE);
        engine_req.cancel_flag = Some(Arc::clone(&cancel_flag));

        let token_for_progress = request.token.clone();
        let job_id_for_progress = job_id.clone();
        let engine_res = self
            .engine
            .translate_document_with_progress(
                engine_req,
                Some(Arc::clone(&cancel_notify)),
                |completed, total| {
                    on_progress(UiTranslationProgress {
                        job_id: job_id_for_progress.clone(),
                        token: Some(token_for_progress.clone()),
                        completed_batches: completed,
                        total_batches: total,
                        status: "running",
                    });
                },
            )
            .await;

        self.unregister_active_job(&job_id);

        let tr_result = match engine_res {
            Ok(res) => res,
            Err(err) => {
                doc.clear_active_job_if(&job_id);
                let status = if matches!(err, TranslateError::Cancelled) {
                    "cancelled"
                } else {
                    "failed"
                };
                on_progress(UiTranslationProgress {
                    job_id: job_id.clone(),
                    token: Some(request.token.clone()),
                    completed_batches: 0,
                    total_batches: 0,
                    status,
                });
                return Err(translate_error_to_ui(err));
            }
        };

        // Re-verify document liveness and identity BEFORE Pass 2 rendering and commit!
        if !store.is_live_instance(&request.token, &doc) || cancel_flag.load(Ordering::SeqCst) {
            doc.clear_active_job_if(&job_id);
            on_progress(UiTranslationProgress {
                job_id: job_id.clone(),
                token: Some(request.token.clone()),
                completed_batches: 0,
                total_batches: 0,
                status: "cancelled",
            });
            return Err(translate_error_to_ui(TranslateError::Cancelled));
        }

        let rendered = render_html_translation(&doc.html, &extracted, &tr_result.items)
            .map_err(|err| {
                doc.clear_active_job_if(&job_id);
                translate_error_to_ui(err)
            })?;

        // Final commit gate: token still live AND job_id still active & uncancelled on this doc.
        if !store.is_live_instance(&request.token, &doc) {
            doc.clear_active_job_if(&job_id);
            return Err(translate_error_to_ui(TranslateError::Cancelled));
        }
        let Some(rev) = doc.commit_translation(
            &job_id,
            tr_result.provider_id.clone(),
            tr_result.provider_generation,
            source_lang.clone(),
            target_lang.clone(),
            rendered.translated_subject.clone(),
            rendered.bilingual_html,
            rendered.translated_html,
        ) else {
            return Err(translate_error_to_ui(TranslateError::Cancelled));
        };

        on_progress(UiTranslationProgress {
            job_id: job_id.clone(),
            token: Some(request.token.clone()),
            completed_batches: 1,
            total_batches: 1,
            status: "completed",
        });

        Ok(UiHtmlTranslationResult {
            job_id,
            token: request.token.clone(),
            rev,
            bilingual_url: base.translated_doc_url(
                &request.token,
                TranslateView::Bilingual,
                rev,
                false,
            ),
            translated_url: base.translated_doc_url(
                &request.token,
                TranslateView::Translated,
                rev,
                false,
            ),
            bilingual_dark_url: base.translated_doc_url(
                &request.token,
                TranslateView::Bilingual,
                rev,
                true,
            ),
            translated_dark_url: base.translated_doc_url(
                &request.token,
                TranslateView::Translated,
                rev,
                true,
            ),
            translated_subject: rendered.translated_subject,
            source_lang,
            target_lang,
            provider_id: tr_result.provider_id,
            provider_generation: tr_result.provider_generation,
            from_cache: tr_result.from_cache,
        })
    }

    /// Independent token-less translation path for plain-text emails.
    /// Caller fetches `(subject, body)` strictly from the daemon via controlled message identity.
    pub async fn translate_plain_text<F>(
        &self,
        subject: &str,
        body: &str,
        requested_provider: Option<&str>,
        requested_target: Option<&str>,
        force_refresh: bool,
        mut on_progress: F,
    ) -> Result<UiPlainTranslationResult, UiError>
    where
        F: FnMut(UiTranslationProgress),
    {
        let (provider_id, expected_generation, target_lang) =
            self.resolve_provider_and_target(requested_provider, requested_target)?;

        let extracted = extract_plain_text(body, Some(subject));
        let body_sample = extracted.body_text_for_detection();
        let subj = (!subject.trim().is_empty()).then_some(subject);
        let detected = detect_language(&body_sample, subj);
        let source_lang = detected.language.as_tag().to_string();

        let job_id = self.allocate_job_id();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_notify = Arc::new(tokio::sync::Notify::new());
        self.register_active_job(
            job_id.clone(),
            provider_id.clone(),
            Arc::clone(&cancel_flag),
            Arc::clone(&cancel_notify),
        );

        let mut engine_req = TranslateDocumentRequest::new(
            job_id.clone(),
            provider_id.clone(),
            source_lang.clone(),
            target_lang.clone(),
            ContentKind::Plain,
            extracted.blocks.clone(),
        );
        engine_req.expected_generation = Some(expected_generation);
        engine_req.force_refresh = force_refresh;
        engine_req.total_deadline = Some(DEFAULT_TOTAL_DEADLINE);
        engine_req.cancel_flag = Some(Arc::clone(&cancel_flag));

        let job_id_for_progress = job_id.clone();
        let engine_res = self
            .engine
            .translate_document_with_progress(
                engine_req,
                Some(Arc::clone(&cancel_notify)),
                |completed, total| {
                    on_progress(UiTranslationProgress {
                        job_id: job_id_for_progress.clone(),
                        token: None,
                        completed_batches: completed,
                        total_batches: total,
                        status: "running",
                    });
                },
            )
            .await;

        self.unregister_active_job(&job_id);

        let tr_result = match engine_res {
            Ok(res) => res,
            Err(err) => {
                let status = if matches!(err, TranslateError::Cancelled) {
                    "cancelled"
                } else {
                    "failed"
                };
                on_progress(UiTranslationProgress {
                    job_id: job_id.clone(),
                    token: None,
                    completed_batches: 0,
                    total_batches: 0,
                    status,
                });
                return Err(translate_error_to_ui(err));
            }
        };

        let rendered =
            render_plain_text(&extracted, &tr_result.items).map_err(translate_error_to_ui)?;

        on_progress(UiTranslationProgress {
            job_id: job_id.clone(),
            token: None,
            completed_batches: 1,
            total_batches: 1,
            status: "completed",
        });

        Ok(UiPlainTranslationResult {
            job_id,
            original_subject: rendered.original_subject,
            translated_subject: rendered.translated_subject,
            blocks: rendered.blocks,
            source_lang,
            target_lang,
            provider_id: tr_result.provider_id,
            provider_generation: tr_result.provider_generation,
            from_cache: tr_result.from_cache,
        })
    }

    fn register_active_job(
        &self,
        job_id: String,
        provider_id: String,
        cancel_flag: Arc<AtomicBool>,
        cancel_notify: Arc<tokio::sync::Notify>,
    ) {
        let mut guard = self
            .active_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(
            job_id,
            ActiveManagerJob {
                provider_id,
                cancel_flag,
                cancel_notify,
            },
        );
    }

    fn unregister_active_job(&self, job_id: &str) -> Option<String> {
        let mut guard = self
            .active_jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(job_id).map(|j| j.provider_id)
    }

    /// Cancel an in-flight HTML or plain-text translation job immediately, and notify the daemon
    /// best-effort if it was routed to an external plugin.
    pub async fn cancel_job(&self, job_id: &str) -> bool {
        let active = {
            let mut guard = self
                .active_jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.remove(job_id)
        };
        if let Some(job) = active {
            job.cancel_flag.store(true, Ordering::SeqCst);
            job.cancel_notify.notify_waiters();
            if !job.provider_id.starts_with(BUILTIN_PROVIDER_PREFIX) {
                let _ = self.daemon.cancel_translation(job_id).await;
            }
            true
        } else {
            false
        }
    }

    /// Called when a document is released or evicted from `DocumentStore` to notify the daemon
    /// best-effort if the cancelled job belonged to an external plugin.
    pub async fn notify_daemon_cancel_if_external(&self, job_id: &str) {
        let provider_id = self.unregister_active_job(job_id);
        if provider_id.is_some_and(|id| !id.starts_with(BUILTIN_PROVIDER_PREFIX)) {
            let _ = self.daemon.cancel_translation(job_id).await;
        }
    }

    pub fn cache_stats(&self) -> Result<CacheStats, UiError> {
        self.cache.stats().map_err(translate_error_to_ui)
    }

    pub async fn clear_cache(&self) -> Result<CacheClearReport, UiError> {
        let cache = Arc::clone(&self.cache);
        tokio::task::spawn_blocking(move || cache.clear())
            .await
            .map_err(|_| translate_error_to_ui(TranslateError::Cache("clear task panicked".into())))?
            .map_err(translate_error_to_ui)
    }
}

fn ensure_private_dir(dir: &Path) -> Result<(), TranslateError> {
    std::fs::create_dir_all(dir)
        .map_err(|_| TranslateError::Io("failed to create translation directory".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app_paths(root: &Path) -> AppPaths {
        AppPaths::from_roots(
            &root.join("platform-config"),
            &root.join("platform-data"),
            Some(&root.join("platform-run")),
            Some(&root.join("platform-state")),
            root,
        )
    }

    #[test]
    fn storage_paths_propagate_explicit_relative_env_default_socket_and_multiconfig() {
        let root = PathBuf::from("/virtual/home");
        let app_paths = test_app_paths(&root);
        let cwd = PathBuf::from("/virtual/work");

        // 1. Default platform config
        let default_paths = TranslationStoragePaths::resolve_with(
            None,
            None,
            None,
            None,
            None,
            None,
            &app_paths,
            &cwd,
            |_| false,
        )
        .unwrap();
        assert_eq!(default_paths.config_dir, app_paths.config_dir);
        assert_eq!(default_paths.cache_dir, app_paths.data_dir);

        // 2. Explicit absolute --config
        let abs = TranslationStoragePaths::resolve_with(
            Some(Path::new("/tmp/custom/config.toml")),
            None,
            None,
            None,
            None,
            None,
            &app_paths,
            &cwd,
            |_| false,
        )
        .unwrap();
        assert_eq!(abs.config_dir, PathBuf::from("/tmp/custom"));
        assert_ne!(abs.config_dir, app_paths.config_dir);
        assert!(abs.cache_dir.starts_with("/tmp/custom/.translation-cache/"));

        // 3. Explicit relative --config
        let rel = TranslationStoragePaths::resolve_with(
            Some(Path::new("./nested/my.toml")),
            None,
            None,
            None,
            None,
            None,
            &app_paths,
            &cwd,
            |_| false,
        )
        .unwrap();
        assert_eq!(rel.config_dir, PathBuf::from("/virtual/work/nested"));
        assert!(rel
            .cache_dir
            .starts_with("/virtual/work/nested/.translation-cache/"));

        // 4. Multi-config in the same directory -> distinct cache namespaces!
        let cfg_a = TranslationStoragePaths::resolve_with(
            Some(Path::new("/tmp/shared/a.toml")),
            None,
            None,
            None,
            None,
            None,
            &app_paths,
            &cwd,
            |_| false,
        )
        .unwrap();
        let cfg_b = TranslationStoragePaths::resolve_with(
            Some(Path::new("/tmp/shared/b.toml")),
            None,
            None,
            None,
            None,
            None,
            &app_paths,
            &cwd,
            |_| false,
        )
        .unwrap();
        assert_eq!(cfg_a.config_dir, PathBuf::from("/tmp/shared"));
        assert_eq!(cfg_b.config_dir, PathBuf::from("/tmp/shared"));
        assert_ne!(
            cfg_a.cache_dir, cfg_b.cache_dir,
            "two configs in the same dir must have distinct cache namespaces"
        );

        // 5. Env var FEATHERPOST_CONFIG
        let env_paths = TranslationStoragePaths::resolve_with(
            None,
            None,
            None,
            None,
            Some(std::ffi::OsStr::new("/etc/fp/env.toml")),
            None,
            &app_paths,
            &cwd,
            |_| false,
        )
        .unwrap();
        assert_eq!(env_paths.config_dir, PathBuf::from("/etc/fp"));
        assert!(env_paths
            .cache_dir
            .starts_with("/etc/fp/.translation-cache/"));

        // 6. Socket-only -> isolated from default platform config & cache!
        let sock_paths = TranslationStoragePaths::resolve_with(
            None,
            Some(Path::new("/tmp/dev.sock")),
            None,
            None,
            None,
            None,
            &app_paths,
            &cwd,
            |_| false,
        )
        .unwrap();
        assert_ne!(sock_paths.config_dir, app_paths.config_dir);
        assert_ne!(sock_paths.cache_dir, app_paths.data_dir);
        assert!(sock_paths
            .config_dir
            .starts_with(app_paths.config_dir.join("translation-namespaces")));
        assert!(sock_paths
            .cache_dir
            .starts_with(app_paths.data_dir.join("translation-namespaces")));
    }

    #[test]
    fn legacy_daemon_detection_distinguishes_old_daemon_from_real_denial() {
        let method_not_found = ClientError {
            kind: ErrorKind::Rpc,
            message: "method not found".into(),
            rpc_code: Some(-32601),
            rpc_data: None,
        };
        assert!(is_legacy_translation_unsupported(
            &method_not_found,
            UiCompat::Compatible
        ));

        let legacy_user_only = ClientError {
            kind: ErrorKind::Rpc,
            message: "插件主体无权调用 listTranslationProviders：该方法仅限用户主体".into(),
            rpc_code: Some(-32016),
            rpc_data: None,
        };
        assert!(is_legacy_translation_unsupported(
            &legacy_user_only,
            UiCompat::Compatible
        ));

        let real_permission_denied = ClientError {
            kind: ErrorKind::Rpc,
            message: "缺少 capability plugins_manage".into(),
            rpc_code: Some(-32016),
            rpc_data: None,
        };
        assert!(!is_legacy_translation_unsupported(
            &real_permission_denied,
            UiCompat::Compatible
        ));
    }
}
