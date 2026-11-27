//! 外部翻译 provider 的 Cordis 热插拔注册表与调度总线（ADR-0050）。
//!
//! 设计要点：
//! - 独立于 `transport_provider` 的可撤销角色属主与全局单调递增 `generation`：
//!   双角色插件连接共享同一数字 id 分配器与 pending 表，但替换/撤销某一角色只失效该角色。
//! - `disable` / `remove` / `upgrade` / `rollback` / EOF / forced abort 全部立即撤销活跃代次
//!   （包括 `independent` 插件的停用）。
//! - 注册与每次派发（进队列前与拿到并发槽后）均重验包启用态、当前版本与活跃代次。
//! - 每端点有界并发 `Semaphore(max_concurrency.clamp(1, 4))`、每端点排队上限、
//!   每调用会话上限、全局并发与字节预算，以及含换行符的 1 MiB 整帧预算。
//! - 同会话可取消通路：`cancelTranslation` 校验调用会话与 `job_id`（拒绝跨会话取消），
//!   立即中止本地等待/后续派发，并尽力向原插件发送 `translation/cancel { request_id }`。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cordis::{Context, Service};
use maild_client::{
    CancelTranslationResult, InvokeTranslationRequest, InvokeTranslationResult,
    PluginTranslateRequest, PluginTranslateResponse, PluginTranslationProbeRequest,
    PluginTranslationProbeResponse, TestTranslationProviderRequest, TestTranslationProviderResult,
    TranslationProviderInfo, TranslationProviderRegisterRequest, TranslationProvidersResult,
    PROVIDER_BUSY_CODE, PROVIDER_CANCELLED_CODE, PROVIDER_ERROR_CODE, PROVIDER_TIMEOUT_CODE,
    PROVIDER_UNAVAILABLE_CODE,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot, watch, Notify, Semaphore};

use crate::events::TranslationProvidersChanged;
use crate::external::{ExternalPluginManager, InstalledTranslationPackage};
use crate::provider::{CallOwner, ProviderReply, SessionCallState};

/// 稳定错误码（ADR-0021 / ADR-0050）。
pub const PROVIDER_UNAVAILABLE: i64 = PROVIDER_UNAVAILABLE_CODE;
pub const PROVIDER_TIMEOUT: i64 = PROVIDER_TIMEOUT_CODE;
pub const PROVIDER_ERROR: i64 = PROVIDER_ERROR_CODE;
pub const PROVIDER_BUSY: i64 = PROVIDER_BUSY_CODE;
pub const PROVIDER_CANCELLED: i64 = PROVIDER_CANCELLED_CODE;

/// 协议单帧硬上限（含末尾 `\n`）。
pub const MAX_WIRE_FRAME_BYTES: usize = 1024 * 1024;
/// 宿主侧批次上限：协商值取 `min(宿主上限, 插件声明值)`。
pub const HOST_MAX_BATCH_ITEMS: u32 = 64;
pub const HOST_MAX_BATCH_CHARS: u32 = 64_000;
pub const DEFAULT_BATCH_ITEMS: u32 = 16;
pub const DEFAULT_BATCH_CHARS: u32 = 12_000;
pub const MAX_CONCURRENCY_CAP: u32 = 4;

/// 排队、会话与全局资源硬上限（ADR-0041 / ADR-0050）。
pub const MAX_ENDPOINT_QUEUED: usize = 4;
pub const MAX_SESSION_INFLIGHT: usize = 8;
pub const MAX_GLOBAL_INFLIGHT: usize = 16;
pub const MAX_GLOBAL_INFLIGHT_BYTES: usize = 4 * 1024 * 1024;

pub const DEFAULT_TRANSLATE_TIMEOUT_MS: u64 = 45_000;
pub const MIN_TRANSLATE_TIMEOUT_MS: u64 = 50;
pub const MAX_TRANSLATE_TIMEOUT_MS: u64 = 60_000;
pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 15_000;
pub const MAX_PROBE_TIMEOUT_MS: u64 = 15_000;

const MAX_REGISTER_LANGUAGES: usize = 128;
const MAX_LANG_TAG_BYTES: usize = 35;
const MAX_VARIANT_REVISION_BYTES: usize = 128;
const MAX_JOB_ID_BYTES: usize = 128;
const MAX_SANITIZED_DETAIL_BYTES: usize = 256;
const MAX_PLUGIN_DECLARED_BATCH_ITEMS: u32 = 256;
const MAX_PLUGIN_DECLARED_BATCH_CHARS: u32 = 256_000;

/// 校验并规范化后的动态注册能力元数据（展示名/l10n/版本均以 manifest 为准）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedRegistration {
    pub supported_languages: Vec<String>,
    pub max_batch_items: u32,
    pub max_batch_chars: u32,
    pub max_concurrency: u32,
    pub supports_cancel: bool,
    pub variant_revision: String,
}

impl ValidatedRegistration {
    pub fn validate(req: &TranslationProviderRegisterRequest) -> Result<Self, String> {
        if req.supported_languages.len() > MAX_REGISTER_LANGUAGES {
            return Err(format!(
                "supported_languages 数量超过上限 {MAX_REGISTER_LANGUAGES}"
            ));
        }
        let mut seen_langs = HashSet::with_capacity(req.supported_languages.len());
        let mut supported_languages = Vec::with_capacity(req.supported_languages.len());
        for raw in &req.supported_languages {
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.len() > MAX_LANG_TAG_BYTES {
                return Err("supported_languages 包含长度无效的语言标签".into());
            }
            if trimmed != "*"
                && !trimmed
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err("supported_languages 包含非法字符".into());
            }
            let key = trimmed.to_ascii_lowercase();
            if !seen_langs.insert(key) {
                return Err("supported_languages 不允许重复语言标签".into());
            }
            supported_languages.push(trimmed.to_owned());
        }

        let declared_items = req.max_batch_items.unwrap_or(DEFAULT_BATCH_ITEMS);
        if declared_items == 0 || declared_items > MAX_PLUGIN_DECLARED_BATCH_ITEMS {
            return Err(format!(
                "max_batch_items 必须在 1..={MAX_PLUGIN_DECLARED_BATCH_ITEMS} 之间"
            ));
        }
        let declared_chars = req.max_batch_chars.unwrap_or(DEFAULT_BATCH_CHARS);
        if declared_chars == 0 || declared_chars > MAX_PLUGIN_DECLARED_BATCH_CHARS {
            return Err(format!(
                "max_batch_chars 必须在 1..={MAX_PLUGIN_DECLARED_BATCH_CHARS} 之间"
            ));
        }
        let declared_concurrency = req.max_concurrency.unwrap_or(1);
        if declared_concurrency == 0 {
            return Err("max_concurrency 必须至少为 1".into());
        }
        if req.variant_revision.len() > MAX_VARIANT_REVISION_BYTES
            || req.variant_revision.chars().any(char::is_control)
        {
            return Err(format!(
                "variant_revision 不能超过 {MAX_VARIANT_REVISION_BYTES} 字节且不能包含控制字符"
            ));
        }

        Ok(Self {
            supported_languages,
            max_batch_items: declared_items.min(HOST_MAX_BATCH_ITEMS),
            max_batch_chars: declared_chars.min(HOST_MAX_BATCH_CHARS),
            max_concurrency: declared_concurrency.clamp(1, MAX_CONCURRENCY_CAP),
            supports_cancel: req.supports_cancel,
            variant_revision: req.variant_revision.clone(),
        })
    }
}

/// 翻译调用的分类错误，映射到稳定 JSON-RPC 错误码与 `error.data.reason`。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranslationError {
    Unavailable(String),
    Timeout,
    Busy(String),
    Cancelled,
    Upstream(String),
    InvalidResponse(String),
    InvalidParams(String),
    PermissionDenied(String),
}

impl TranslationError {
    pub fn rpc_code(&self) -> i64 {
        match self {
            Self::Unavailable(_) => PROVIDER_UNAVAILABLE,
            Self::Timeout => PROVIDER_TIMEOUT,
            Self::Busy(_) => PROVIDER_BUSY,
            Self::Cancelled => PROVIDER_CANCELLED,
            Self::Upstream(_) | Self::InvalidResponse(_) => PROVIDER_ERROR,
            Self::InvalidParams(_) => crate::api::INVALID_PARAMS,
            Self::PermissionDenied(_) => crate::api::METHOD_NOT_PERMITTED,
        }
    }

    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "unavailable",
            Self::Timeout => "timeout",
            Self::Busy(_) => "busy",
            Self::Cancelled => "cancelled",
            Self::Upstream(_) => "provider_error",
            Self::InvalidResponse(_) => "invalid_response",
            Self::InvalidParams(_) => "invalid_params",
            Self::PermissionDenied(_) => "permission_denied",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Unavailable(detail)
            | Self::Busy(detail)
            | Self::Upstream(detail)
            | Self::InvalidResponse(detail)
            | Self::InvalidParams(detail)
            | Self::PermissionDenied(detail) => detail.clone(),
            Self::Timeout => "翻译插件调用超时".into(),
            Self::Cancelled => "翻译任务已取消".into(),
        }
    }

    pub fn into_rpc_error(self) -> crate::api::JsonRpcError {
        let code = self.rpc_code();
        let reason = self.reason_code();
        let message = self.message();
        crate::api::JsonRpcError {
            code,
            message,
            data: Some(json!({ "reason": reason })),
        }
    }
}

/// 脱敏且有界的诊断文本：去除控制字符、折叠空白、截断在 UTF-8 边界。
pub(crate) fn sanitize_detail(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_SANITIZED_DETAIL_BYTES));
    let mut prev_space = false;
    for ch in raw.chars() {
        let mapped = if ch.is_control() || ch.is_whitespace() {
            if prev_space {
                continue;
            }
            prev_space = true;
            ' '
        } else {
            prev_space = false;
            ch
        };
        if out.len() + mapped.len_utf8() > MAX_SANITIZED_DETAIL_BYTES {
            break;
        }
        out.push(mapped);
    }
    out.trim().to_owned()
}

/// 校验安全的标识符字段（如 `job_id`、`batch_id`）。
pub(crate) fn validate_safe_id(label: &str, value: &str) -> Result<(), TranslationError> {
    if value.is_empty()
        || value.len() > MAX_JOB_ID_BYTES
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
    {
        return Err(TranslationError::InvalidParams(format!(
            "{label} 必须是 1..={MAX_JOB_ID_BYTES} 字节的安全 ASCII 标识符"
        )));
    }
    Ok(())
}

/// 校验语言标签（允许 `"auto"` 仅当 `allow_auto` 为 true）。
pub(crate) fn validate_lang_tag(
    label: &str,
    value: &str,
    allow_auto: bool,
) -> Result<(), TranslationError> {
    if value.is_empty()
        || value.len() > MAX_LANG_TAG_BYTES
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(TranslationError::InvalidParams(format!(
            "{label} 不是合法的语言标签"
        )));
    }
    if !allow_auto && value.eq_ignore_ascii_case("auto") {
        return Err(TranslationError::InvalidParams(format!(
            "{label} 不能为 auto"
        )));
    }
    Ok(())
}

/// 一个已注册且可独立撤销的翻译 provider 端点。
pub(crate) struct TranslationProviderEndpoint {
    plugin_id: String,
    generation: u64,
    package_version: String,
    metadata: ValidatedRegistration,
    output: mpsc::Sender<String>,
    calls: Arc<SessionCallState>,
    semaphore: Arc<Semaphore>,
    pending_count: AtomicUsize,
    revoked: AtomicBool,
    revoke_notify: Notify,
}

impl TranslationProviderEndpoint {
    pub(crate) fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn package_version(&self) -> &str {
        &self.package_version
    }

    pub(crate) fn metadata(&self) -> &ValidatedRegistration {
        &self.metadata
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.revoked.load(Ordering::Acquire) && !self.output.is_closed()
    }

    /// 立即撤销本代次端点：唤醒所有排队等待者，并仅失败属于本翻译代次的未决调用。
    pub(crate) fn revoke(&self, reason: &str) {
        let was_revoked = self.revoked.swap(true, Ordering::AcqRel);
        self.revoke_notify.notify_waiters();
        self.calls
            .fail_owner(CallOwner::Translation(self.generation), reason);
        if !was_revoked {
            eprintln!(
                "[maild/translation] 撤销 {}（generation={}）：{}",
                self.plugin_id,
                self.generation,
                sanitize_detail(reason)
            );
        }
    }

    async fn wait_revoked(&self) {
        loop {
            let notified = self.revoke_notify.notified();
            if self.revoked.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    fn max_pending_slots(&self) -> usize {
        (self.metadata.max_concurrency as usize).saturating_add(MAX_ENDPOINT_QUEUED)
    }

    fn try_reserve_slot(self: &Arc<Self>) -> Result<EndpointSlotGuard, TranslationError> {
        let limit = self.max_pending_slots();
        let mut current = self.pending_count.load(Ordering::Acquire);
        loop {
            if current >= limit {
                return Err(TranslationError::Busy(
                    "翻译插件当前并发与队列已满，请稍后重试".into(),
                ));
            }
            match self.pending_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(EndpointSlotGuard {
                        endpoint: Arc::clone(self),
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    /// 尽力向插件发送 `translation/cancel` 通知（不阻塞调用方）。
    pub(crate) fn send_best_effort_cancel(&self, request_id: u64) -> bool {
        if !self.metadata.supports_cancel || !self.is_active() {
            return false;
        }
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "translation/cancel",
            "params": { "request_id": request_id }
        })
        .to_string();
        self.output.try_send(frame).is_ok()
    }
}

struct EndpointSlotGuard {
    endpoint: Arc<TranslationProviderEndpoint>,
}

impl Drop for EndpointSlotGuard {
    fn drop(&mut self) {
        self.endpoint.pending_count.fetch_sub(1, Ordering::AcqRel);
    }
}

struct ActiveInvocation {
    invocation_id: u64,
    session_id: u64,
    cancel_tx: watch::Sender<bool>,
    endpoint: Arc<TranslationProviderEndpoint>,
    plugin_request_id: Mutex<Option<u64>>,
    cancel_notified: AtomicBool,
}

impl ActiveInvocation {
    fn trigger_cancel(&self) -> bool {
        self.cancel_tx.send_replace(true);
        if self.cancel_notified.swap(true, Ordering::AcqRel) {
            return false;
        }
        let req_id = *self.plugin_request_id.lock().unwrap();
        if let Some(req_id) = req_id {
            self.endpoint.send_best_effort_cancel(req_id)
        } else {
            false
        }
    }
}

#[derive(Default)]
struct InflightTracker {
    global_count: usize,
    global_bytes: usize,
    session_counts: HashMap<u64, usize>,
    /// `job_id -> Vec<Arc<ActiveInvocation>>`（同 `job_id` 可有多个并发批次，且归属单一 `session_id`）。
    jobs: HashMap<String, Vec<Arc<ActiveInvocation>>>,
}

/// 预留全局与会话并发/字节预算的 RAII 守卫（在读循环上同步获取，防止任务风暴）。
pub(crate) struct DispatchBudgetGuard {
    registry: Arc<TranslationProviderRegistry>,
    session_id: u64,
    bytes: usize,
}

impl Drop for DispatchBudgetGuard {
    fn drop(&mut self) {
        let mut tracker = self.registry.inflight.lock().unwrap();
        tracker.global_count = tracker.global_count.saturating_sub(1);
        tracker.global_bytes = tracker.global_bytes.saturating_sub(self.bytes);
        if let Some(count) = tracker.session_counts.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                tracker.session_counts.remove(&self.session_id);
            }
        }
    }
}

struct InvocationGuard {
    registry: Arc<TranslationProviderRegistry>,
    job_id: String,
    invocation_id: u64,
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        let mut tracker = self.registry.inflight.lock().unwrap();
        if let Some(entries) = tracker.jobs.get_mut(&self.job_id) {
            entries.retain(|entry| entry.invocation_id != self.invocation_id);
            if entries.is_empty() {
                tracker.jobs.remove(&self.job_id);
            }
        }
    }
}

/// Cordis 服务：外部翻译 provider 注册表与调度中心。
pub struct TranslationProviderRegistry {
    endpoints: Mutex<BTreeMap<String, Arc<TranslationProviderEndpoint>>>,
    next_generation: AtomicU64,
    catalog_generation: AtomicU64,
    next_invocation_id: AtomicU64,
    late_responses: AtomicU64,
    inflight: Mutex<InflightTracker>,
}

impl Service for TranslationProviderRegistry {
    fn name(&self) -> &'static str {
        "translation.provider-registry"
    }
}

impl TranslationProviderRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            endpoints: Mutex::new(BTreeMap::new()),
            next_generation: AtomicU64::new(1),
            catalog_generation: AtomicU64::new(1),
            next_invocation_id: AtomicU64::new(1),
            late_responses: AtomicU64::new(0),
            inflight: Mutex::new(InflightTracker::default()),
        })
    }

    /// 登记一个新的翻译 provider 端点（latest-wins），并立即撤销被顶替的旧代次。
    pub(crate) fn register(
        &self,
        plugin_id: String,
        package_version: String,
        metadata: ValidatedRegistration,
        output: mpsc::Sender<String>,
        calls: Arc<SessionCallState>,
    ) -> (
        Arc<TranslationProviderEndpoint>,
        Option<Arc<TranslationProviderEndpoint>>,
    ) {
        let generation = self.next_generation.fetch_add(1, Ordering::AcqRel);
        let concurrency = metadata.max_concurrency.clamp(1, MAX_CONCURRENCY_CAP) as usize;
        let endpoint = Arc::new(TranslationProviderEndpoint {
            plugin_id: plugin_id.clone(),
            generation,
            package_version,
            metadata,
            output,
            calls,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            pending_count: AtomicUsize::new(0),
            revoked: AtomicBool::new(false),
            revoke_notify: Notify::new(),
        });
        let previous = self
            .endpoints
            .lock()
            .unwrap()
            .insert(plugin_id, Arc::clone(&endpoint));
        if let Some(old) = &previous {
            old.revoke("translation provider 已被同一插件的新连接顶替");
        }
        (endpoint, previous)
    }

    /// 生命周期事件（disable / remove / upgrade / rollback）主动撤销某插件当前活跃代次。
    pub(crate) fn revoke_plugin(&self, plugin_id: &str, reason: &str) -> bool {
        let removed = self.endpoints.lock().unwrap().remove(plugin_id);
        if let Some(endpoint) = removed {
            endpoint.revoke(reason);
            true
        } else {
            false
        }
    }

    /// 仅当当前注册仍是 `endpoint` 时移除，并始终撤销 `endpoint`。
    pub(crate) fn remove_if_owner(
        &self,
        plugin_id: &str,
        endpoint: &Arc<TranslationProviderEndpoint>,
        reason: &str,
    ) -> bool {
        let removed = {
            let mut endpoints = self.endpoints.lock().unwrap();
            match endpoints.get(plugin_id) {
                Some(current) if Arc::ptr_eq(current, endpoint) => {
                    endpoints.remove(plugin_id);
                    true
                }
                _ => false,
            }
        };
        endpoint.revoke(reason);
        removed
    }

    /// 停机时撤销全部活跃翻译 provider 端点。
    pub(crate) fn revoke_all(&self, reason: &str) {
        let drained: Vec<Arc<TranslationProviderEndpoint>> = {
            let mut endpoints = self.endpoints.lock().unwrap();
            std::mem::take(&mut *endpoints).into_values().collect()
        };
        for endpoint in drained {
            endpoint.revoke(reason);
        }
    }

    pub(crate) fn endpoint_of(&self, plugin_id: &str) -> Option<Arc<TranslationProviderEndpoint>> {
        self.endpoints.lock().unwrap().get(plugin_id).cloned()
    }

    fn is_current_endpoint(
        &self,
        plugin_id: &str,
        endpoint: &Arc<TranslationProviderEndpoint>,
    ) -> bool {
        self.endpoints
            .lock()
            .unwrap()
            .get(plugin_id)
            .is_some_and(|current| Arc::ptr_eq(current, endpoint))
    }

    pub(crate) fn note_late_response(&self) {
        self.late_responses.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn catalog_generation(&self) -> u64 {
        self.catalog_generation.load(Ordering::Acquire)
    }

    pub(crate) fn bump_catalog_generation(&self) -> u64 {
        self.catalog_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// 合并已安装的 `translation_provider` 包清单与当前实时注册代次/能力。
    pub(crate) fn snapshot_providers(
        &self,
        packages: Vec<InstalledTranslationPackage>,
        catalog_generation: u64,
    ) -> TranslationProvidersResult {
        let live: BTreeMap<String, Arc<TranslationProviderEndpoint>> =
            self.endpoints.lock().unwrap().clone();
        let providers = packages
            .into_iter()
            .map(|pkg| {
                let endpoint = live.get(&pkg.id).filter(|ep| {
                    ep.is_active() && pkg.enabled && ep.package_version() == pkg.version
                });
                match endpoint {
                    Some(ep) => {
                        let meta = ep.metadata();
                        TranslationProviderInfo {
                            id: pkg.id,
                            name: pkg.name,
                            description: pkg.description,
                            l10n: pkg.l10n,
                            version: pkg.version,
                            lifecycle: pkg.lifecycle,
                            enabled: pkg.enabled,
                            state: pkg.state,
                            has_ui: pkg.has_ui,
                            registered: true,
                            generation: ep.generation(),
                            supported_languages: meta.supported_languages.clone(),
                            max_batch_items: meta.max_batch_items,
                            max_batch_chars: meta.max_batch_chars,
                            max_concurrency: meta.max_concurrency,
                            supports_cancel: meta.supports_cancel,
                            variant_revision: meta.variant_revision.clone(),
                        }
                    }
                    None => TranslationProviderInfo {
                        id: pkg.id,
                        name: pkg.name,
                        description: pkg.description,
                        l10n: pkg.l10n,
                        version: pkg.version,
                        lifecycle: pkg.lifecycle,
                        enabled: pkg.enabled,
                        state: pkg.state,
                        has_ui: pkg.has_ui,
                        registered: false,
                        generation: 0,
                        supported_languages: Vec::new(),
                        max_batch_items: 0,
                        max_batch_chars: 0,
                        max_concurrency: 0,
                        supports_cancel: false,
                        variant_revision: String::new(),
                    },
                }
            })
            .collect();
        TranslationProvidersResult {
            generation: catalog_generation,
            providers,
        }
    }

    /// 预留全局与会话级在途并发及字节预算；满载立即返回 `TranslationError::Busy`。
    pub(crate) fn try_reserve_budget(
        self: &Arc<Self>,
        session_id: u64,
        bytes: usize,
    ) -> Result<DispatchBudgetGuard, TranslationError> {
        let mut tracker = self.inflight.lock().unwrap();
        let session_active = tracker.session_counts.get(&session_id).copied().unwrap_or(0);
        if session_active >= MAX_SESSION_INFLIGHT {
            return Err(TranslationError::Busy(format!(
                "当前会话在途翻译任务已达上限（{MAX_SESSION_INFLIGHT}）"
            )));
        }
        if tracker.global_count >= MAX_GLOBAL_INFLIGHT {
            return Err(TranslationError::Busy(format!(
                "全局在途翻译任务已达上限（{MAX_GLOBAL_INFLIGHT}）"
            )));
        }
        if tracker.global_bytes.saturating_add(bytes) > MAX_GLOBAL_INFLIGHT_BYTES {
            return Err(TranslationError::Busy(
                "全局在途翻译载荷字节预算已满，请稍后重试".into(),
            ));
        }
        tracker.global_count += 1;
        tracker.global_bytes = tracker.global_bytes.saturating_add(bytes);
        *tracker.session_counts.entry(session_id).or_insert(0) += 1;
        Ok(DispatchBudgetGuard {
            registry: Arc::clone(self),
            session_id,
            bytes,
        })
    }

    /// 取消调用会话名下指定 `job_id` 的全部在途批次；若 `job_id` 属于其他会话则拒绝（-32016）。
    pub(crate) fn cancel_job(
        &self,
        caller_session_id: u64,
        job_id: &str,
    ) -> Result<CancelTranslationResult, TranslationError> {
        validate_safe_id("job_id", job_id)?;
        let targets: Vec<Arc<ActiveInvocation>> = {
            let tracker = self.inflight.lock().unwrap();
            let Some(entries) = tracker.jobs.get(job_id) else {
                return Ok(CancelTranslationResult {
                    cancelled: false,
                    cancelled_requests: 0,
                    notified_provider: 0,
                });
            };
            if entries.iter().any(|entry| entry.session_id != caller_session_id) {
                return Err(TranslationError::PermissionDenied(
                    "不允许跨会话取消其他连接的翻译任务".into(),
                ));
            }
            entries.clone()
        };
        let mut cancelled_requests = 0u32;
        let mut notified_provider = 0u32;
        for invocation in targets {
            cancelled_requests = cancelled_requests.saturating_add(1);
            if invocation.trigger_cancel() {
                notified_provider = notified_provider.saturating_add(1);
            }
        }
        Ok(CancelTranslationResult {
            cancelled: cancelled_requests > 0,
            cancelled_requests,
            notified_provider,
        })
    }

    /// 调用会话断开（EOF / shutdown）时取消该会话名下的全部在途翻译任务。
    pub(crate) fn cancel_session(&self, session_id: u64) {
        let targets: Vec<Arc<ActiveInvocation>> = {
            let tracker = self.inflight.lock().unwrap();
            tracker
                .jobs
                .values()
                .flatten()
                .filter(|entry| entry.session_id == session_id)
                .cloned()
                .collect()
        };
        for invocation in targets {
            let _ = invocation.trigger_cancel();
        }
    }

    fn verify_endpoint_live(
        &self,
        manager: &ExternalPluginManager,
        provider_id: &str,
        expected_generation: Option<u64>,
        existing: Option<&Arc<TranslationProviderEndpoint>>,
    ) -> Result<Arc<TranslationProviderEndpoint>, TranslationError> {
        let pkg = manager
            .installed_translation_provider(provider_id)
            .ok_or_else(|| {
                let _ = self.revoke_plugin(provider_id, "插件已卸载或不再声明 translation_provider");
                TranslationError::Unavailable(format!("翻译插件未安装或未声明能力: {provider_id}"))
            })?;
        if !pkg.enabled {
            let _ = self.revoke_plugin(provider_id, "翻译插件已停用");
            return Err(TranslationError::Unavailable(format!(
                "翻译插件已停用: {provider_id}"
            )));
        }
        let endpoint = match existing {
            Some(ep) => {
                if !self.is_current_endpoint(provider_id, ep) {
                    return Err(TranslationError::Unavailable(
                        "翻译插件连接已换代或注销".into(),
                    ));
                }
                Arc::clone(ep)
            }
            None => self.endpoint_of(provider_id).ok_or_else(|| {
                TranslationError::Unavailable(format!("翻译插件未注册在线连接: {provider_id}"))
            })?,
        };
        if !endpoint.is_active() {
            let _ = self.remove_if_owner(provider_id, &endpoint, "翻译插件连接已关闭");
            return Err(TranslationError::Unavailable("翻译插件连接已关闭".into()));
        }
        if endpoint.package_version() != pkg.version {
            let _ = self.remove_if_owner(
                provider_id,
                &endpoint,
                "翻译插件包版本已变化，撤销旧代次",
            );
            return Err(TranslationError::Unavailable(
                "翻译插件版本已更新，请使用新代次重新发起".into(),
            ));
        }
        if let Some(expected) = expected_generation {
            if endpoint.generation() != expected {
                return Err(TranslationError::Unavailable(format!(
                    "翻译插件代次不匹配（期望 {expected}，当前 {}）",
                    endpoint.generation()
                )));
            }
        }
        Ok(endpoint)
    }

    /// 执行一次受限的翻译批次调用（在读循环之外的独立异步任务中运行）。
    pub(crate) async fn invoke_translation(
        self: &Arc<Self>,
        ctx: &Context,
        session_id: u64,
        request: InvokeTranslationRequest,
        _budget: DispatchBudgetGuard,
    ) -> Result<InvokeTranslationResult, TranslationError> {
        validate_invoke_request(&request)?;
        let manager = ctx
            .get::<ExternalPluginManager>()
            .ok_or_else(|| TranslationError::Unavailable("external 插件管理器未装配".into()))?;

        let endpoint = self.verify_endpoint_live(
            &manager,
            &request.provider_id,
            Some(request.generation),
            None,
        )?;
        enforce_negotiated_batch_limits(&request, endpoint.metadata())?;

        // 校验与登记：先确保 job_id 未被其他会话占用，防止跨会话 job_id 冲突。
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let invocation_id = self.next_invocation_id.fetch_add(1, Ordering::AcqRel);
        let invocation = Arc::new(ActiveInvocation {
            invocation_id,
            session_id,
            cancel_tx,
            endpoint: Arc::clone(&endpoint),
            plugin_request_id: Mutex::new(None),
            cancel_notified: AtomicBool::new(false),
        });
        {
            let mut tracker = self.inflight.lock().unwrap();
            let entries = tracker.jobs.entry(request.job_id.clone()).or_default();
            if entries.iter().any(|existing| existing.session_id != session_id) {
                return Err(TranslationError::PermissionDenied(
                    "job_id 正被其他会话占用".into(),
                ));
            }
            entries.push(Arc::clone(&invocation));
        }
        let _inv_guard = InvocationGuard {
            registry: Arc::clone(self),
            job_id: request.job_id.clone(),
            invocation_id,
        };

        let _slot = endpoint.try_reserve_slot()?;

        let timeout_ms = request
            .timeout_ms
            .unwrap_or(DEFAULT_TRANSLATE_TIMEOUT_MS)
            .clamp(MIN_TRANSLATE_TIMEOUT_MS, MAX_TRANSLATE_TIMEOUT_MS);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

        // 等待并发槽（受总 deadline、撤销与取消统一约束）。
        let _permit = tokio::select! {
            biased;
            _ = endpoint.wait_revoked() => {
                return Err(TranslationError::Unavailable("翻译插件端点已撤销".into()));
            }
            _ = wait_cancelled(&mut cancel_rx) => {
                return Err(TranslationError::Cancelled);
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(TranslationError::Timeout);
            }
            acquired = endpoint.semaphore.clone().acquire_owned() => {
                acquired.map_err(|_| TranslationError::Unavailable("翻译插件并发槽已关闭".into()))?
            }
        };

        // 拿到并发槽后再次重验包状态、版本与代次（防止排队期间被停用/升级/顶替）。
        self.verify_endpoint_live(
            &manager,
            &request.provider_id,
            Some(request.generation),
            Some(&endpoint),
        )?;

        let expected_ids: HashSet<u32> = request.items.iter().map(|item| item.id).collect();
        let plugin_payload = PluginTranslateRequest {
            job_id: request.job_id.clone(),
            batch_id: request.batch_id.clone(),
            source_lang: request.source_lang.clone(),
            target_lang: request.target_lang.clone(),
            content_kind: request.content_kind.clone(),
            items: request.items,
        };
        let req_id = endpoint.calls.allocate_id();
        let req_key = req_id.to_string();
        let frame_line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "translation/translate",
            "params": plugin_payload,
        }))
        .map_err(|_| TranslationError::InvalidParams("翻译请求序列化失败".into()))?;
        if frame_line.len().saturating_add(1) > MAX_WIRE_FRAME_BYTES {
            return Err(TranslationError::InvalidParams(format!(
                "序列化后的翻译请求帧超过 1 MiB 协议上限（含换行符 {} 字节）",
                frame_line.len().saturating_add(1)
            )));
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        endpoint.calls.insert_owned(
            &req_key,
            CallOwner::Translation(endpoint.generation()),
            reply_tx,
        );
        struct PendingCallGuard<'a> {
            calls: &'a SessionCallState,
            key: String,
        }
        impl Drop for PendingCallGuard<'_> {
            fn drop(&mut self) {
                self.calls.remove(&self.key);
            }
        }
        let _pending_guard = PendingCallGuard {
            calls: &endpoint.calls,
            key: req_key.clone(),
        };

        // 记录插件请求 ID，使并发抵达的 cancelTranslation 可以尽力下发 translation/cancel。
        *invocation.plugin_request_id.lock().unwrap() = Some(req_id);
        if *cancel_rx.borrow() {
            return Err(TranslationError::Cancelled);
        }

        // 出站写入（受总 deadline、撤销与取消约束）。
        tokio::select! {
            biased;
            _ = endpoint.wait_revoked() => {
                return Err(TranslationError::Unavailable("翻译插件端点已撤销".into()));
            }
            _ = wait_cancelled(&mut cancel_rx) => {
                return Err(TranslationError::Cancelled);
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(TranslationError::Timeout);
            }
            sent = endpoint.output.send(frame_line) => {
                if sent.is_err() {
                    let _ = self.remove_if_owner(&request.provider_id, &endpoint, "翻译插件连接已关闭");
                    return Err(TranslationError::Unavailable("翻译插件连接已关闭".into()));
                }
            }
        }

        // 等待插件应答（受总 deadline、撤销与取消约束）。
        // 注意：撤销时会同步 via `fail_owner` 向仍在等待的 `reply_rx` 发送 `-32018`，
        // 因此把 `reply_rx` 置于 `wait_revoked()` 之前，避免插件送出完整应答后随即关闭时误丢正常应答。
        let reply = tokio::select! {
            biased;
            _ = wait_cancelled(&mut cancel_rx) => {
                let _ = invocation.trigger_cancel();
                return Err(TranslationError::Cancelled);
            }
            outcome = reply_rx => {
                outcome.map_err(|_| TranslationError::Unavailable("翻译插件端点已失效".into()))?
            }
            _ = endpoint.wait_revoked() => {
                return Err(TranslationError::Unavailable("翻译插件端点已撤销".into()));
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = endpoint.send_best_effort_cancel(req_id);
                return Err(TranslationError::Timeout);
            }
        };

        // 应答抵达时最后一次检查是否刚被取消或撤销。
        if *cancel_rx.borrow() {
            return Err(TranslationError::Cancelled);
        }
        if !endpoint.is_active() {
            return Err(TranslationError::Unavailable("翻译插件端点已撤销".into()));
        }

        match reply {
            ProviderReply::Err { code, message } => {
                if code == PROVIDER_UNAVAILABLE {
                    Err(TranslationError::Unavailable(sanitize_detail(&message)))
                } else {
                    let clean = sanitize_detail(&message);
                    let detail = if clean.is_empty() {
                        format!("翻译插件返回错误 ({code})")
                    } else {
                        clean
                    };
                    Err(TranslationError::Upstream(detail))
                }
            }
            ProviderReply::Ok(value) => {
                let parsed: PluginTranslateResponse = serde_json::from_value(value).map_err(|_| {
                    TranslationError::InvalidResponse("翻译插件响应结构无效".into())
                })?;
                validate_plugin_translate_response(&parsed, &expected_ids)?;
                let variant_revision = parsed
                    .variant_revision
                    .filter(|rev| !rev.is_empty())
                    .unwrap_or_else(|| endpoint.metadata().variant_revision.clone());
                let result = InvokeTranslationResult {
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    variant_revision,
                    detected_source_lang: parsed.detected_source_lang,
                    items: parsed.items,
                };
                // 确保发回前端的完整 JSON-RPC 应答（含换行符）不超过 1 MiB 预算。
                let estimated_envelope = serde_json::to_vec(&result)
                    .map_err(|_| TranslationError::InvalidResponse("翻译结果序列化失败".into()))?;
                if estimated_envelope.len().saturating_add(128) > MAX_WIRE_FRAME_BYTES {
                    return Err(TranslationError::InvalidResponse(
                        "翻译插件返回结果超过 1 MiB 协议帧上限".into(),
                    ));
                }
                Ok(result)
            }
        }
    }

    /// 发送固定无邮件样本 `translation/probe { target_lang }` 探测外部翻译插件连通性。
    pub(crate) async fn test_provider(
        self: &Arc<Self>,
        ctx: &Context,
        request: TestTranslationProviderRequest,
        _budget: DispatchBudgetGuard,
    ) -> Result<TestTranslationProviderResult, TranslationError> {
        validate_safe_id("provider_id", &request.provider_id)?;
        validate_lang_tag("target_lang", &request.target_lang, false)?;
        if request.generation == Some(0) {
            return Err(TranslationError::InvalidParams(
                "generation 若提供则必须大于 0".into(),
            ));
        }

        let started = Instant::now();
        let elapsed_ms = || started.elapsed().as_millis() as u64;
        let manager = match ctx.get::<ExternalPluginManager>() {
            Some(manager) => manager,
            None => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: 0,
                    stage: "lookup".into(),
                    reason: "unavailable".into(),
                    latency_ms: elapsed_ms(),
                    detail: "external 插件管理器未装配".into(),
                });
            }
        };

        let endpoint = match self.verify_endpoint_live(
            &manager,
            &request.provider_id,
            request.generation,
            None,
        ) {
            Ok(ep) => ep,
            Err(err) => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: 0,
                    stage: "lookup".into(),
                    reason: err.reason_code().into(),
                    latency_ms: elapsed_ms(),
                    detail: sanitize_detail(&err.message()),
                });
            }
        };

        let _slot = match endpoint.try_reserve_slot() {
            Ok(slot) => slot,
            Err(err) => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: err.reason_code().into(),
                    latency_ms: elapsed_ms(),
                    detail: sanitize_detail(&err.message()),
                });
            }
        };

        let timeout_ms = request
            .timeout_ms
            .unwrap_or(DEFAULT_PROBE_TIMEOUT_MS)
            .clamp(MIN_TRANSLATE_TIMEOUT_MS, MAX_PROBE_TIMEOUT_MS);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);

        let _permit = tokio::select! {
            biased;
            _ = endpoint.wait_revoked() => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: "unavailable".into(),
                    latency_ms: elapsed_ms(),
                    detail: "翻译插件端点已撤销".into(),
                });
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: "timeout".into(),
                    latency_ms: elapsed_ms(),
                    detail: "翻译插件探测超时".into(),
                });
            }
            acquired = endpoint.semaphore.clone().acquire_owned() => {
                match acquired {
                    Ok(permit) => permit,
                    Err(_) => {
                        return Ok(TestTranslationProviderResult {
                            ok: false,
                            provider_id: request.provider_id,
                            generation: endpoint.generation(),
                            stage: "probe".into(),
                            reason: "unavailable".into(),
                            latency_ms: elapsed_ms(),
                            detail: "翻译插件并发槽已关闭".into(),
                        });
                    }
                }
            }
        };

        if let Err(err) = self.verify_endpoint_live(
            &manager,
            &request.provider_id,
            request.generation,
            Some(&endpoint),
        ) {
            return Ok(TestTranslationProviderResult {
                ok: false,
                provider_id: request.provider_id,
                generation: endpoint.generation(),
                stage: "lookup".into(),
                reason: err.reason_code().into(),
                latency_ms: elapsed_ms(),
                detail: sanitize_detail(&err.message()),
            });
        }

        let req_id = endpoint.calls.allocate_id();
        let req_key = req_id.to_string();
        let frame_line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": req_id,
            "method": "translation/probe",
            "params": PluginTranslationProbeRequest {
                target_lang: request.target_lang,
            },
        }))
        .map_err(|_| TranslationError::InvalidParams("探测请求序列化失败".into()))?;

        let (reply_tx, reply_rx) = oneshot::channel();
        endpoint.calls.insert_owned(
            &req_key,
            CallOwner::Translation(endpoint.generation()),
            reply_tx,
        );
        struct PendingProbeGuard<'a> {
            calls: &'a SessionCallState,
            key: String,
        }
        impl Drop for PendingProbeGuard<'_> {
            fn drop(&mut self) {
                self.calls.remove(&self.key);
            }
        }
        let _pending_guard = PendingProbeGuard {
            calls: &endpoint.calls,
            key: req_key,
        };

        tokio::select! {
            biased;
            _ = endpoint.wait_revoked() => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: "unavailable".into(),
                    latency_ms: elapsed_ms(),
                    detail: "翻译插件端点已撤销".into(),
                });
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: "timeout".into(),
                    latency_ms: elapsed_ms(),
                    detail: "翻译插件探测超时".into(),
                });
            }
            sent = endpoint.output.send(frame_line) => {
                if sent.is_err() {
                    let _ = self.remove_if_owner(&request.provider_id, &endpoint, "翻译插件连接已关闭");
                    return Ok(TestTranslationProviderResult {
                        ok: false,
                        provider_id: request.provider_id,
                        generation: endpoint.generation(),
                        stage: "probe".into(),
                        reason: "unavailable".into(),
                        latency_ms: elapsed_ms(),
                        detail: "翻译插件连接已关闭".into(),
                    });
                }
            }
        }

        let reply = tokio::select! {
            biased;
            outcome = reply_rx => match outcome {
                Ok(reply) => reply,
                Err(_) => {
                    return Ok(TestTranslationProviderResult {
                        ok: false,
                        provider_id: request.provider_id,
                        generation: endpoint.generation(),
                        stage: "probe".into(),
                        reason: "unavailable".into(),
                        latency_ms: elapsed_ms(),
                        detail: "翻译插件端点已失效".into(),
                    });
                }
            },
            _ = endpoint.wait_revoked() => {
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: "unavailable".into(),
                    latency_ms: elapsed_ms(),
                    detail: "翻译插件端点已撤销".into(),
                });
            }
            _ = tokio::time::sleep_until(deadline) => {
                let _ = endpoint.send_best_effort_cancel(req_id);
                return Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: "timeout".into(),
                    latency_ms: elapsed_ms(),
                    detail: "翻译插件探测超时".into(),
                });
            }
        };

        match reply {
            ProviderReply::Err { code, message } => {
                let reason = if code == PROVIDER_UNAVAILABLE {
                    "unavailable"
                } else {
                    "provider_error"
                };
                Ok(TestTranslationProviderResult {
                    ok: false,
                    provider_id: request.provider_id,
                    generation: endpoint.generation(),
                    stage: "probe".into(),
                    reason: reason.into(),
                    latency_ms: elapsed_ms(),
                    detail: sanitize_detail(&message),
                })
            }
            ProviderReply::Ok(value) => {
                match serde_json::from_value::<PluginTranslationProbeResponse>(value) {
                    Ok(parsed) => Ok(TestTranslationProviderResult {
                        ok: parsed.ok,
                        provider_id: request.provider_id,
                        generation: endpoint.generation(),
                        stage: if parsed.ok { "done" } else { "probe" }.into(),
                        reason: if parsed.ok { "ok" } else { "provider_error" }.into(),
                        latency_ms: elapsed_ms(),
                        detail: sanitize_detail(&parsed.detail),
                    }),
                    Err(_) => Ok(TestTranslationProviderResult {
                        ok: false,
                        provider_id: request.provider_id,
                        generation: endpoint.generation(),
                        stage: "validate".into(),
                        reason: "invalid_response".into(),
                        latency_ms: elapsed_ms(),
                        detail: "翻译插件探测响应格式无效".into(),
                    }),
                }
            }
        }
    }
}

/// 广播最新 `translation/providers` 快照（递增目录事件代次）。
pub(crate) fn emit_providers_changed(ctx: &Context) {
    let Some(registry) = ctx.get::<TranslationProviderRegistry>() else {
        return;
    };
    let packages = ctx
        .get::<ExternalPluginManager>()
        .map(|manager| manager.translation_provider_packages())
        .unwrap_or_default();
    let generation = registry.bump_catalog_generation();
    let snapshot = registry.snapshot_providers(packages, generation);
    ctx.emit::<TranslationProvidersChanged>(snapshot);
}

/// 估算 `invokeTranslation` 请求的载荷字节占用（用于全局在途字节预算）。
pub(crate) fn estimate_invoke_bytes(req: &InvokeTranslationRequest) -> usize {
    let mut total = req
        .provider_id
        .len()
        .saturating_add(req.job_id.len())
        .saturating_add(req.batch_id.as_deref().map_or(0, str::len))
        .saturating_add(req.source_lang.len())
        .saturating_add(req.target_lang.len())
        .saturating_add(req.content_kind.len())
        .saturating_add(256);
    for item in &req.items {
        total = total
            .saturating_add(item.text.len())
            .saturating_add(item.context.as_deref().map_or(0, str::len))
            .saturating_add(32);
    }
    total
}

pub(crate) fn validate_invoke_request(
    req: &InvokeTranslationRequest,
) -> Result<(), TranslationError> {
    validate_safe_id("provider_id", &req.provider_id)?;
    let lowered = req.provider_id.to_ascii_lowercase();
    if lowered == "builtin" || lowered.starts_with("builtin.") {
        return Err(TranslationError::InvalidParams(
            "invokeTranslation 仅面向外部翻译插件，不能调用 builtin.*".into(),
        ));
    }
    if req.generation == 0 {
        return Err(TranslationError::InvalidParams(
            "generation 必须为正整数".into(),
        ));
    }
    validate_safe_id("job_id", &req.job_id)?;
    if let Some(batch_id) = &req.batch_id {
        validate_safe_id("batch_id", batch_id)?;
    }
    validate_lang_tag("source_lang", &req.source_lang, true)?;
    validate_lang_tag("target_lang", &req.target_lang, false)?;
    if !matches!(req.content_kind.as_str(), "html" | "plain") {
        return Err(TranslationError::InvalidParams(
            "content_kind 必须是 \"html\" 或 \"plain\"".into(),
        ));
    }
    if req.items.is_empty() {
        return Err(TranslationError::InvalidParams(
            "items 不能为空".into(),
        ));
    }
    if req.items.len() > HOST_MAX_BATCH_ITEMS as usize {
        return Err(TranslationError::InvalidParams(format!(
            "items 数量超过宿主上限 {HOST_MAX_BATCH_ITEMS}"
        )));
    }
    let mut ids = HashSet::with_capacity(req.items.len());
    let mut total_chars = 0usize;
    for item in &req.items {
        if !ids.insert(item.id) {
            return Err(TranslationError::InvalidParams(format!(
                "items 包含重复的 id: {}",
                item.id
            )));
        }
        total_chars = total_chars.saturating_add(item.text.chars().count());
    }
    if total_chars > HOST_MAX_BATCH_CHARS as usize {
        return Err(TranslationError::InvalidParams(format!(
            "items 总字符数超过宿主上限 {HOST_MAX_BATCH_CHARS}"
        )));
    }
    Ok(())
}

fn enforce_negotiated_batch_limits(
    req: &InvokeTranslationRequest,
    meta: &ValidatedRegistration,
) -> Result<(), TranslationError> {
    let max_items = meta.max_batch_items.min(HOST_MAX_BATCH_ITEMS) as usize;
    if req.items.len() > max_items {
        return Err(TranslationError::InvalidParams(format!(
            "items 数量 {} 超过协商批次上限 {max_items}",
            req.items.len()
        )));
    }
    let max_chars = meta.max_batch_chars.min(HOST_MAX_BATCH_CHARS) as usize;
    let total_chars: usize = req.items.iter().map(|item| item.text.chars().count()).sum();
    if total_chars > max_chars {
        return Err(TranslationError::InvalidParams(format!(
            "items 总字符数 {total_chars} 超过协商字符上限 {max_chars}"
        )));
    }
    Ok(())
}

fn validate_plugin_translate_response(
    response: &PluginTranslateResponse,
    expected_ids: &HashSet<u32>,
) -> Result<(), TranslationError> {
    if response.items.len() != expected_ids.len() {
        return Err(TranslationError::InvalidResponse(format!(
            "翻译插件返回项数量不匹配（期望 {}，实际 {}）",
            expected_ids.len(),
            response.items.len()
        )));
    }
    let mut seen = HashSet::with_capacity(expected_ids.len());
    for item in &response.items {
        if !expected_ids.contains(&item.id) {
            return Err(TranslationError::InvalidResponse(format!(
                "翻译插件返回了未请求的项 id={}",
                item.id
            )));
        }
        if !seen.insert(item.id) {
            return Err(TranslationError::InvalidResponse(format!(
                "翻译插件返回了重复的项 id={}",
                item.id
            )));
        }
    }
    if let Some(lang) = &response.detected_source_lang {
        if lang.len() > MAX_LANG_TAG_BYTES || lang.chars().any(char::is_control) {
            return Err(TranslationError::InvalidResponse(
                "翻译插件返回的 detected_source_lang 无效".into(),
            ));
        }
    }
    if let Some(rev) = &response.variant_revision {
        if rev.len() > MAX_VARIANT_REVISION_BYTES || rev.chars().any(char::is_control) {
            return Err(TranslationError::InvalidResponse(
                "翻译插件返回的 variant_revision 无效".into(),
            ));
        }
    }
    Ok(())
}

async fn wait_cancelled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_validation_clamps_concurrency_and_rejects_invalid_values() {
        let valid = ValidatedRegistration::validate(&TranslationProviderRegisterRequest {
            supported_languages: vec!["en".into(), "zh-Hans".into()],
            max_batch_items: Some(128),
            max_batch_chars: Some(100_000),
            max_concurrency: Some(9),
            supports_cancel: true,
            variant_revision: "gpt-4o-mini@v1".into(),
        })
        .unwrap();
        assert_eq!(valid.max_batch_items, HOST_MAX_BATCH_ITEMS);
        assert_eq!(valid.max_batch_chars, HOST_MAX_BATCH_CHARS);
        assert_eq!(valid.max_concurrency, MAX_CONCURRENCY_CAP);

        assert!(ValidatedRegistration::validate(&TranslationProviderRegisterRequest {
            max_concurrency: Some(0),
            ..Default::default()
        })
        .is_err());

        assert!(ValidatedRegistration::validate(&TranslationProviderRegisterRequest {
            supported_languages: vec!["en".into(), "EN".into()],
            ..Default::default()
        })
        .is_err());

        assert!(ValidatedRegistration::validate(&TranslationProviderRegisterRequest {
            variant_revision: "bad\nline".into(),
            ..Default::default()
        })
        .is_err());
    }

    #[test]
    fn sanitize_detail_strips_control_chars_and_bounds_length() {
        let noisy = format!("err\r\n\tsecret\u{0007} {}", "x".repeat(400));
        let clean = sanitize_detail(&noisy);
        assert!(!clean.chars().any(char::is_control));
        assert!(clean.len() <= MAX_SANITIZED_DETAIL_BYTES);
        assert!(clean.starts_with("err secret"));
    }

    #[test]
    fn register_supersedes_old_generation_and_respects_owner() {
        let registry = TranslationProviderRegistry::new();
        let (tx1, _rx1) = mpsc::channel(4);
        let (tx2, _rx2) = mpsc::channel(4);
        let calls1 = SessionCallState::new();
        let calls2 = SessionCallState::new();
        let meta = ValidatedRegistration::validate(&TranslationProviderRegisterRequest::default())
            .unwrap();

        let (ep1, prev1) = registry.register(
            "org.example.tr".into(),
            "1.0.0".into(),
            meta.clone(),
            tx1,
            calls1.clone(),
        );
        assert!(prev1.is_none());
        assert_eq!(ep1.generation(), 1);
        assert!(ep1.is_active());

        // 同连接上挂一条 transport pending 与一条 translation pending：
        // 顶替 translation 只能失败 translation pending，不得影响 transport pending！
        let (tr_tx, mut tr_rx) = oneshot::channel();
        let (tp_tx, mut tp_rx) = oneshot::channel();
        calls1.insert_owned("1", CallOwner::Translation(ep1.generation()), tr_tx);
        calls1.insert_owned("2", CallOwner::Transport(99), tp_tx);

        let (ep2, prev2) = registry.register(
            "org.example.tr".into(),
            "1.0.0".into(),
            meta,
            tx2,
            calls2,
        );
        assert!(Arc::ptr_eq(&prev2.unwrap(), &ep1));
        assert_eq!(ep2.generation(), 2);
        assert!(!ep1.is_active());
        assert!(ep2.is_active());

        // Translation pending 立即收到 -32018；transport pending 仍存活！
        match tr_rx.try_recv().unwrap() {
            ProviderReply::Err { code, .. } => assert_eq!(code, PROVIDER_UNAVAILABLE),
            other => panic!("expected unavailable, got {other:?}"),
        }
        assert!(tp_rx.try_recv().is_err(), "transport pending 不应被翻译角色顶替误伤");

        // 旧连接清理不能误删新端点。
        assert!(!registry.remove_if_owner("org.example.tr", &ep1, "old closed"));
        assert!(registry.endpoint_of("org.example.tr").is_some());
        assert!(registry.remove_if_owner("org.example.tr", &ep2, "new closed"));
        assert!(registry.endpoint_of("org.example.tr").is_none());
    }
}
