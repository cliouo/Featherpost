//! Explicit management DTOs. Stored secrets and raw provider errors never enter the webview.
use crate::{Bridge, UiError, UiErrorCode};
use maild_client::{
    AccountDraftInput, AccountProfileView, AddressView, CustomProxyConfig, CustomProxyUpdate,
    NotificationStatus, PackagePreview, ProbeReportView, ProfilesResult, ProxyConfig,
    ProxyConfigResult, ProxyConfigUpdate, ProxyEndpointInfo, ProxyPreferenceChanged, QuietHours,
    SystemProxySnapshot, TestProxyRequest, TestProxyResult,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Secrets {
    pub incoming: Option<String>,
    pub outgoing: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountEdit {
    pub id: Option<String>,
    pub revision: Option<u64>,
    pub draft: AccountDraftInput,
    pub secrets: Secrets,
}
impl AccountEdit {
    fn params(&self) -> Result<Value, UiError> {
        maild_client::validate_draft_input(&self.draft)
            .map_err(|_| UiError::invalid("account_fields_invalid", "请检查地址、服务器、端口、TLS 和轮询间隔。"))?;
        let mut credentials = json!({});
        for (role, secret) in [
            ("incoming", &self.secrets.incoming),
            ("outgoing", &self.secrets.outgoing),
        ] {
            if let Some(secret) = secret.as_ref().filter(|s| !s.is_empty()) {
                if secret.len() > 8192 || secret.contains('\0') {
                    return Err(UiError::invalid("credentials_invalid", "认证信息无效或过长。"));
                }
                credentials[role] = json!(secret);
            }
        }
        let mut draft = json!(self.draft);
        // A full editor can explicitly remove SMTP; omission means unchanged in the core API.
        if self.draft.outgoing.is_none() {
            draft["outgoing"] = Value::Null;
        }
        if let Some(id) = &self.id {
            let revision = self
                .revision
                .filter(|n| *n > 0)
                .ok_or_else(|| UiError::invalid("account_revision_missing", "账户版本缺失，请重新载入。"))?;
            let mut p = json!({"id":id,"expected_revision":revision,"patch":draft});
            if credentials.as_object().is_some_and(|c| !c.is_empty()) {
                p["credentials"] = credentials;
            }
            Ok(p)
        } else {
            Ok(json!({"draft":draft,"credentials":credentials}))
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AccountAction {
    Enable {
        id: String,
        revision: u64,
        enabled: bool,
    },
    Remove {
        id: String,
        revision: u64,
    },
}
/// webview 看到的 rules（ADR-0043）。
///
/// `notify_new_mail` / `muted_accounts` 用 `Option` 表达「后台是否认识这两个键」：内核在缺省值时
/// 不序列化它们（旧客户端逐字节不变），所以桥按响应形状判断支持与否——支持时总是给出明确值，
/// 界面据 `undefined` 隐藏开关；不支持时省略，保存时也不会把旧后台不认识的键发过去
/// （内核 `RulesPatch` 拒绝未知字段）。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RulesConfig {
    pub show_sensitive: bool,
    #[serde(default)]
    pub silent_keywords: Vec<String>,
    #[serde(default)]
    pub quiet_hours: Vec<QuietHours>,
    #[serde(default)]
    pub timezone_offset_minutes: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_system_timezone: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_new_mail: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muted_accounts: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UiRules {
    pub rules: RulesConfig,
    pub persisted: Option<bool>,
}

impl UiRules {
    /// 从 `getSettings`/`setSettings` 原始响应投影。`persisted` 键（ADR-0043 起 get/set 都回）
    /// 或任一新键出现 = 后台认识普适新邮件通知与系统时区开关，缺省值补成明确值。
    fn from_response(value: Value) -> Result<Self, UiError> {
        let supports_new_mail = value.get("persisted").is_some_and(Value::is_boolean)
            || value
                .get("rules")
                .and_then(Value::as_object)
                .is_some_and(|rules| {
                    rules.contains_key("notify_new_mail")
                        || rules.contains_key("muted_accounts")
                        || rules.contains_key("follow_system_timezone")
                });
        let mut result: UiRules = serde_json::from_value(value)
            .map_err(|error| UiError::failed("settings_rules", error, "rules_unrecognized", "后台返回的规则无法识别。"))?;
        if supports_new_mail {
            result.rules.follow_system_timezone.get_or_insert(false);
            result.rules.notify_new_mail.get_or_insert(false);
            result.rules.muted_accounts.get_or_insert_with(Vec::new);
        }
        Ok(result)
    }
}

/// 探测诊断在桥这一侧再过一道：去控制字符、限长。内核 `probe.rs` 给的本就是固定中文文案
/// （不含凭据与路径），这里防的是旧后台或 provider 插件带来的长串。
const MAX_PROBE_DETAIL_BYTES: usize = 240;

fn safe_probe_detail(detail: &str) -> String {
    let mut out = String::new();
    for ch in detail.chars().filter(|ch| !ch.is_control()) {
        if out.len() + ch.len_utf8() > MAX_PROBE_DETAIL_BYTES {
            break;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

/// `ui_settings_discover_provider` 的结果：只给向导预填所需的端点（`settings/types.ts` 的
/// `DiscoveredServers`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DiscoveredEndpointView {
    pub server: String,
    pub port: u16,
    pub security: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DiscoveredServers {
    pub incoming: DiscoveredEndpointView,
    pub outgoing: Option<DiscoveredEndpointView>,
    pub source: String,
}

impl From<maild_client::ProviderDiscovery> for DiscoveredServers {
    fn from(found: maild_client::ProviderDiscovery) -> Self {
        let endpoint = |endpoint: maild_client::DiscoveredEndpoint| DiscoveredEndpointView {
            server: endpoint.server,
            port: endpoint.port,
            security: endpoint.security,
        };
        Self {
            incoming: endpoint(found.incoming),
            outgoing: Some(endpoint(found.outgoing)),
            source: found.source,
        }
    }
}
#[derive(Deserialize, Serialize)]
pub struct AddressPage {
    pub addresses: Vec<AddressView>,
    pub total: usize,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum AddressAction {
    Create {
        account: String,
        kind: String,
        address: Option<String>,
        label: String,
        expires_secs: Option<u64>,
    },
    Update {
        id: String,
        state: Option<String>,
        label: Option<String>,
    },
    Remove {
        id: String,
    },
}
#[derive(Serialize)]
pub struct UiPackages {
    pub default: String,
    pub frontends: Vec<maild_client::FrontendInfo>,
    pub plugins: Vec<maild_client::ExternalPluginInfo>,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageAction {
    Recovery,
    Enable { id: String, enabled: bool },
    Remove { kind: String, id: String },
    Open { id: String },
    Default { id: String },
    Refresh,
}

// —— 全局代理（Phase 2：显式无密码 WebView DTO 与脱敏日志） ——

const MAX_PROXY_HOST_BYTES: usize = 255;
const MAX_PROXY_USERNAME_BYTES: usize = 255;
const MAX_PROXY_PASSWORD_BYTES: usize = 1024;
const MAX_PROXY_BYPASS_ENTRIES: usize = 256;
const MAX_PROXY_BYPASS_ENTRY_BYTES: usize = 255;
const MAX_PROXY_TARGET_BYTES: usize = 265;

/// 出站给 WebView 的代理端点摘要：结构上不含 `password` 字段。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UiProxyEndpoint {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

impl From<ProxyEndpointInfo> for UiProxyEndpoint {
    fn from(ep: ProxyEndpointInfo) -> Self {
        Self {
            protocol: ep.protocol,
            host: ep.host,
            port: ep.port,
            username: ep.username,
        }
    }
}

/// 出站给 WebView 的自定义代理配置：结构上不含 `password` 字段。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UiCustomProxy {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub bypass: Vec<String>,
}

impl From<CustomProxyConfig> for UiCustomProxy {
    fn from(cfg: CustomProxyConfig) -> Self {
        Self {
            protocol: cfg.protocol,
            host: cfg.host,
            port: cfg.port,
            username: cfg.username,
            bypass: cfg.bypass,
        }
    }
}

/// 出站给 WebView 的代理配置主体。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UiProxyConfigData {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<UiCustomProxy>,
}

impl From<ProxyConfig> for UiProxyConfigData {
    fn from(cfg: ProxyConfig) -> Self {
        Self {
            mode: cfg.mode,
            custom: cfg.custom.map(UiCustomProxy::from),
        }
    }
}

/// 出站给 WebView 的系统代理快照（所有端点不含密码）。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UiSystemProxySnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<UiProxyEndpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub https: Option<UiProxyEndpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socks5: Option<UiProxyEndpoint>,
    pub bypass: Vec<String>,
    pub exclude_simple_hostnames: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pac_url: Option<String>,
    pub source: String,
}

impl From<SystemProxySnapshot> for UiSystemProxySnapshot {
    fn from(snapshot: SystemProxySnapshot) -> Self {
        Self {
            http: snapshot.http.map(UiProxyEndpoint::from),
            https: snapshot.https.map(UiProxyEndpoint::from),
            socks5: snapshot.socks5.map(UiProxyEndpoint::from),
            bypass: snapshot.bypass,
            exclude_simple_hostnames: snapshot.exclude_simple_hostnames,
            pac_url: snapshot.pac_url,
            source: snapshot.source,
        }
    }
}

/// `ui_proxy_config` / `ui_set_proxy_config` 的应答：不含任何明文密码。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UiProxyConfig {
    pub config: UiProxyConfigData,
    pub has_password: bool,
    pub effective_system: UiSystemProxySnapshot,
    pub persisted: bool,
}

impl From<ProxyConfigResult> for UiProxyConfig {
    fn from(result: ProxyConfigResult) -> Self {
        Self {
            config: UiProxyConfigData::from(result.config),
            has_password: result.has_password,
            effective_system: UiSystemProxySnapshot::from(result.effective_system),
            persisted: result.persisted,
        }
    }
}

/// `maild://proxy` 推送事件载荷：不含任何明文密码。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UiProxyEvent {
    pub config: UiProxyConfigData,
    pub has_password: bool,
    pub effective_system: UiSystemProxySnapshot,
    pub persisted: bool,
    pub generation: u64,
}

impl UiProxyEvent {
    pub fn from_config(config: UiProxyConfig, generation: u64) -> Self {
        Self {
            config: config.config,
            has_password: config.has_password,
            effective_system: config.effective_system,
            persisted: config.persisted,
            generation,
        }
    }
}

impl From<ProxyPreferenceChanged> for UiProxyEvent {
    fn from(event: ProxyPreferenceChanged) -> Self {
        Self {
            config: UiProxyConfigData::from(event.config),
            has_password: event.has_password,
            effective_system: UiSystemProxySnapshot::from(event.effective_system),
            persisted: event.persisted,
            generation: event.generation,
        }
    }
}

/// WebView 提交的自定义代理配置（Debug 脱敏，绝不输出密码）。
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiCustomProxyUpdate {
    pub protocol: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub clear_password: bool,
    #[serde(default)]
    pub bypass: Vec<String>,
}

impl fmt::Debug for UiCustomProxyUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UiCustomProxyUpdate")
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .field("clear_password", &self.clear_password)
            .field("bypass", &self.bypass)
            .finish()
    }
}

/// WebView 提交的全局代理配置更新。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiProxyConfigUpdate {
    pub mode: String,
    #[serde(default)]
    pub custom: Option<UiCustomProxyUpdate>,
    #[serde(default)]
    pub clear_password: bool,
}

/// WebView 提交的代理连通性测试请求（可测已保存配置或未保存草稿）。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiTestProxyRequest {
    #[serde(default)]
    pub config: Option<UiProxyConfigUpdate>,
    #[serde(default)]
    pub use_saved_password: bool,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub scheme: Option<String>,
}

/// `ui_test_proxy` 应答：经脱敏和长度裁剪的诊断结果。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UiTestProxyResult {
    pub ok: bool,
    pub route: String,
    pub stage: String,
    pub latency_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_code: Option<String>,
    pub detail: String,
}

impl From<TestProxyResult> for UiTestProxyResult {
    fn from(result: TestProxyResult) -> Self {
        Self {
            ok: result.ok,
            route: safe_probe_detail(&result.route),
            stage: result.stage,
            latency_ms: result.latency_ms,
            detail_code: result.detail_code,
            detail: safe_probe_detail(&result.detail),
        }
    }
}

fn validate_custom_proxy_update(
    custom: &UiCustomProxyUpdate,
    top_clear_password: bool,
) -> Result<CustomProxyUpdate, UiError> {
    let invalid = || {
        UiError::invalid(
            "proxy_invalid",
            "请检查代理协议、地址、端口、认证信息和旁路规则。",
        )
    };
    let protocol = custom.protocol.trim().to_ascii_lowercase();
    if !matches!(protocol.as_str(), "http" | "https" | "socks5" | "socks5h") {
        return Err(invalid());
    }
    let host = custom.host.trim();
    if host.is_empty()
        || host.len() > MAX_PROXY_HOST_BYTES
        || host
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '@' | '?' | '#' | '\\'))
    {
        return Err(invalid());
    }
    if custom.port == 0 {
        return Err(invalid());
    }
    let clear = top_clear_password || custom.clear_password;
    if clear && custom.password.is_some() {
        return Err(invalid());
    }
    let username = match custom.username.as_deref() {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                if trimmed.len() > MAX_PROXY_USERNAME_BYTES
                    || trimmed.chars().any(char::is_control)
                    || trimmed.contains(':')
                {
                    return Err(invalid());
                }
                Some(trimmed.to_string())
            }
        }
    };
    let password = match custom.password.as_deref() {
        None => None,
        Some("") => None,
        Some(raw) => {
            if raw.len() > MAX_PROXY_PASSWORD_BYTES
                || raw.chars().any(|ch| matches!(ch, '\r' | '\n' | '\0'))
            {
                return Err(invalid());
            }
            Some(raw.to_string())
        }
    };
    if custom.bypass.len() > MAX_PROXY_BYPASS_ENTRIES {
        return Err(invalid());
    }
    let mut bypass = Vec::with_capacity(custom.bypass.len());
    for entry in &custom.bypass {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > MAX_PROXY_BYPASS_ENTRY_BYTES
            || trimmed.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        {
            return Err(invalid());
        }
        bypass.push(trimmed.to_string());
    }
    Ok(CustomProxyUpdate {
        protocol,
        host: host.to_string(),
        port: custom.port,
        username,
        password,
        clear_password: custom.clear_password,
        bypass,
    })
}

pub(crate) fn validate_proxy_update(
    update: &UiProxyConfigUpdate,
) -> Result<ProxyConfigUpdate, UiError> {
    let invalid = || {
        UiError::invalid(
            "proxy_invalid",
            "请检查代理协议、地址、端口、认证信息和旁路规则。",
        )
    };
    let mode = update.mode.trim().to_ascii_lowercase();
    if !matches!(mode.as_str(), "system" | "direct" | "custom") {
        return Err(invalid());
    }
    let custom = match &update.custom {
        Some(custom) => Some(validate_custom_proxy_update(custom, update.clear_password)?),
        None => {
            if mode == "custom" && !update.clear_password {
                return Err(invalid());
            }
            None
        }
    };
    Ok(ProxyConfigUpdate {
        mode,
        custom,
        clear_password: update.clear_password,
    })
}

pub(crate) fn validate_test_proxy_request(
    request: &UiTestProxyRequest,
) -> Result<TestProxyRequest, UiError> {
    let config = request
        .config
        .as_ref()
        .map(validate_proxy_update)
        .transpose()?;
    let target = match request.target.as_deref() {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                if trimmed.len() > MAX_PROXY_TARGET_BYTES
                    || trimmed.chars().any(|ch| ch.is_control() || ch.is_whitespace())
                    || !trimmed.contains(':')
                {
                    return Err(UiError::invalid(
                        "proxy_target_invalid",
                        "测试目标格式应为 host:port。",
                    ));
                }
                Some(trimmed.to_string())
            }
        }
    };
    let scheme = match request.scheme.as_deref() {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim().to_ascii_lowercase();
            if trimmed.is_empty() {
                None
            } else if matches!(
                trimmed.as_str(),
                "imap" | "imaps" | "smtp" | "smtps" | "http" | "https" | "raw_tcp"
            ) {
                Some(trimmed)
            } else {
                return Err(UiError::invalid(
                    "proxy_target_invalid",
                    "测试目标格式应为 host:port。",
                ));
            }
        }
    };
    Ok(TestProxyRequest {
        config,
        use_saved_password: request.use_saved_password,
        target,
        scheme,
    })
}

/// 全局代理方法错误投影。绝不打印 `error.message`（防对端或参数回显敏感信息），只记录安全元信息。
/// 老 daemon 不认识 `getProxyConfig` 时返回 `-32601` 或 fail-closed 的 `-32016`（新 daemon 上
/// `getProxyConfig` 为 `Open`，故读时的 `-32016` 只可能来自老 daemon）。
pub(crate) fn proxy_error(method: &str, error: maild_client::ClientError) -> UiError {
    use maild_client::ErrorKind;
    eprintln!(
        "[maild-ui] {method} 失败: kind={}, code={:?}",
        error.kind.as_str(),
        error.rpc_code
    );
    let unsupported = match error.rpc_code {
        Some(-32601) => true,
        Some(-32016) => method == "getProxyConfig",
        _ => false,
    };
    if unsupported {
        return UiError {
            code: UiErrorCode::Failed,
            reason: "proxy_unsupported",
            message: "后台版本不支持全局代理设置。".into(),
        };
    }
    let (code, reason, message) = match (error.kind, error.rpc_code) {
        (ErrorKind::Connection, _) => (
            UiErrorCode::Offline,
            "offline",
            "后台连接不可用，正在自动重试。",
        ),
        (_, Some(-32016)) => (
            UiErrorCode::Denied,
            "denied",
            "当前前端没有执行这项操作的授权。",
        ),
        (_, Some(-32001)) => (
            UiErrorCode::Failed,
            "proxy_not_persisted",
            "代理配置文件无法写入，这次更改没有保存。",
        ),
        (_, Some(-32602)) => (
            UiErrorCode::Invalid,
            "proxy_invalid",
            "请检查代理协议、地址、端口、认证信息和旁路规则。",
        ),
        _ => (UiErrorCode::Failed, "failed", "操作未完成，请稍后重试。"),
    };
    UiError {
        code,
        reason,
        message: message.into(),
    }
}

/// Never log server text here: probe responses and errors can contain credentials echoed by peers.
pub(crate) fn management_error(error: maild_client::ClientError) -> UiError {
    use maild_client::ErrorKind;
    let (code, reason, message) = match (error.kind, error.rpc_code) {
        (ErrorKind::Connection, _) => (
            UiErrorCode::Offline,
            "management_offline",
            "后台连接不可用。若刚提交过变更，请先刷新确认结果，不要重复提交。",
        ),
        (_, Some(-32016)) => (
            UiErrorCode::Denied,
            "management_denied",
            "当前前端没有管理这项功能的授权。",
        ),
        (_, Some(-32011)) => (
            UiErrorCode::Conflict,
            "account_conflict",
            "账户已在别处修改，请重新载入后再编辑。",
        ),
        (_, Some(-32010)) => (
            UiErrorCode::NotFound,
            "account_missing",
            "账户已不存在或已移除，请刷新列表。",
        ),
        (_, Some(-32012)) => (
            UiErrorCode::Failed,
            "probe_failed",
            "连接探测未通过，请检查服务器、TLS 与认证信息。",
        ),
        (_, Some(-32013)) => (
            UiErrorCode::Failed,
            "credential_store_locked",
            "无法访问系统凭据库，请解锁后重试。",
        ),
        (_, Some(-32017)) => (
            UiErrorCode::Failed,
            "package_operation_failed",
            "包操作未完成。请刷新实际清单，确认版本和状态后再操作；没有自动重试。",
        ),
        (_, Some(-32602)) => (
            UiErrorCode::Invalid,
            "management_invalid",
            "请检查必填字段、地址、时间范围和端口。",
        ),
        _ => (
            UiErrorCode::Failed,
            "management_failed",
            "操作未完成，请检查后台与扩展状态，刷新后再试。",
        ),
    };
    UiError {
        code,
        reason,
        message: message.into(),
    }
}
/// 账户页只拿成文所需的部分：daemon 拼的中文整句 `detail` 照旧不过桥；离线原因的稳定 code 与
/// 服务器原文（ADR-0048 L6）过桥，原文同探测诊断一样再去控制字符、限长。
fn profile(mut p: AccountProfileView) -> AccountProfileView {
    p.runtime.detail.clear();
    p.runtime.server_detail = safe_probe_detail(&p.runtime.server_detail);
    p
}

impl Bridge {
    pub(crate) async fn management_call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, UiError> {
        self.guard(method)?;
        self.handle()
            .request_typed(method, params)
            .await
            .map_err(management_error)
    }
    pub async fn settings_profiles(&self) -> Result<Vec<AccountProfileView>, UiError> {
        let r: ProfilesResult = self
            .management_call("listAccountProfiles", json!({}))
            .await?;
        Ok(r.profiles.into_iter().map(profile).collect())
    }
    pub async fn settings_probe(
        &self,
        mut request: AccountEdit,
    ) -> Result<ProbeReportView, UiError> {
        let existing_id = request.id.take();
        let mut params = request.params()?;
        if let Some(id) = existing_id {
            let revision = request
                .revision
                .filter(|r| *r > 0)
                .ok_or_else(|| UiError::invalid("account_revision_missing", "账户版本缺失，请重新载入。"))?;
            params["id"] = json!(id);
            params["expected_revision"] = json!(revision);
        }
        let mut r: ProbeReportView = self.management_call("probeAccount", params).await?;
        // ADR-0042：固定诊断（如「授权码错误」「465 端口需 SSL」）对用户是修复指引，不再清空。
        r.incoming.detail = safe_probe_detail(&r.incoming.detail);
        if let Some(outgoing) = &mut r.outgoing {
            outgoing.detail = safe_probe_detail(&outgoing.detail);
        }
        Ok(r)
    }
    pub async fn settings_save_account(
        &self,
        request: AccountEdit,
    ) -> Result<AccountProfileView, UiError> {
        let params = request.params()?;
        if request.id.is_some() {
            self.management_call("updateAccount", params)
                .await
                .map(profile)
        } else {
            let r: maild_client::CreateAccountResult =
                self.management_call("createAccount", params).await?;
            if !r.persisted {
                return Err(UiError::invalid("account_save_unconfirmed", "后台未确认账户保存。"));
            }
            Ok(profile(r.profile))
        }
    }
    pub async fn settings_account_action(&self, action: AccountAction) -> Result<(), UiError> {
        match action {
            AccountAction::Enable {
                id,
                revision,
                enabled,
            } => {
                let _: AccountProfileView = self
                    .management_call(
                        "setAccountEnabled",
                        json!({"id":id,"expected_revision":revision,"enabled":enabled}),
                    )
                    .await?;
            }
            AccountAction::Remove { id, revision } => {
                let r: maild_client::RemoveAccountResult = self
                    .management_call(
                        "removeAccount",
                        json!({"id":id,"expected_revision":revision}),
                    )
                    .await?;
                if !r.archived || !r.messages_retained {
                    return Err(UiError::invalid("account_keep_unconfirmed", "后台未确认保留本地邮件，请刷新账户状态。"));
                }
                if !r.credentials_deleted {
                    return Err(UiError::invalid("account_credentials_partial", "账户已移除、本地邮件已保留，但部分凭据删除未完成。请刷新账户列表，并检查系统凭据库状态。"));
                }
            }
        }
        Ok(())
    }
    pub async fn settings_rules(&self, patch: Option<RulesConfig>) -> Result<UiRules, UiError> {
        let response: Value = match patch {
            Some(patch) => {
                // 整包提交：新键只在界面拿到过（= 后台认识）时出现，值永远明确——关掉通知、
                // 清空静音名单也能落到后台，而不是被「缺省不序列化」吞掉。
                let rules = serde_json::to_value(&patch).map_err(|error| {
                    UiError::failed("settings_rules", error, "failed", "操作未完成，请稍后重试。")
                })?;
                self.management_call("setSettings", json!({ "rules": rules }))
                    .await?
            }
            None => self.management_call("getSettings", json!({})).await?,
        };
        UiRules::from_response(response)
    }

    /// 服务商识别（ADR-0042），供向导「自动检测服务器」。用户主动点按，允许有界的公网探测；
    /// 输入无效或无法识别时给 `None`，界面按惯例预填。
    pub async fn settings_discover_provider(
        &self,
        address: String,
    ) -> Result<Option<DiscoveredServers>, UiError> {
        match self.discover_provider(address, true).await {
            Ok(found) => Ok(Some(found.into())),
            Err(error) if error.code == UiErrorCode::Invalid => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// 完整的服务商识别结果（含授权引导、帮助页）。`allow_network = false` 时零网络。
    pub async fn discover_provider(
        &self,
        input: String,
        allow_network: bool,
    ) -> Result<maild_client::ProviderDiscovery, UiError> {
        let input = input.trim().to_string();
        if input.is_empty() || input.len() > 320 || input.chars().any(char::is_control) {
            return Err(UiError::invalid("provider_query_invalid", "请输入有效的邮箱地址或域名。"));
        }
        self.management_call(
            "discoverEmailProvider",
            json!({ "input": input, "allow_network": allow_network }),
        )
        .await
    }
    pub async fn settings_notification(
        &self,
        request: bool,
    ) -> Result<NotificationStatus, UiError> {
        self.management_call(
            if request {
                "requestNotificationAuthorization"
            } else {
                "getNotificationStatus"
            },
            json!({}),
        )
        .await
    }
    pub async fn settings_addresses(
        &self,
        account: &str,
        offset: u64,
    ) -> Result<AddressPage, UiError> {
        self.management_call(
            "listAddresses",
            json!({"account":account,"limit":100,"offset":offset,"include_inactive":true}),
        )
        .await
    }
    pub async fn settings_address_action(&self, action: AddressAction) -> Result<(), UiError> {
        let (method, params) = match action {
            AddressAction::Create {
                account,
                kind,
                address,
                label,
                expires_secs,
            } => (
                "createAddress",
                json!({"account":account,"kind":kind,"address":address,"label":label,"expires_secs":expires_secs}),
            ),
            AddressAction::Update { id, state, label } => (
                "updateAddress",
                json!({"id":id,"state":state,"label":label}),
            ),
            AddressAction::Remove { id } => ("removeAddress", json!({"id":id})),
        };
        let _: Value = self.management_call(method, params).await?;
        Ok(())
    }
    pub async fn settings_packages(&self) -> Result<UiPackages, UiError> {
        let f: maild_client::FrontendsResult =
            self.management_call("listFrontends", json!({})).await?;
        let p: maild_client::ExternalPluginsResult = self
            .management_call("listExternalPlugins", json!({}))
            .await?;
        Ok(UiPackages {
            default: f.default,
            frontends: f.frontends,
            plugins: p.plugins,
        })
    }
    pub async fn settings_package_action(&self, action: PackageAction) -> Result<(), UiError> {
        if matches!(action, PackageAction::Recovery) {
            // 版本不一致时原生修复窗口正是排障出口，只携带 background，不改任何数据。
            let result: Value = self
                .handle()
                .request_typed(
                    "openFrontend",
                    json!({"frontend":maild_client::BUILTIN_RECOVERY_FRONTEND,"intent":{"route":"background"}}),
                )
                .await
                .map_err(management_error)?;
            if result["activation_requested"] != true {
                return Err(UiError::invalid("recovery_unavailable", &format!(
                    "修复入口未能打开，请从完整 {} 打开修复与后台管理。",
                    maild_brand::APP_BUNDLE_NAME
                )));
            }
            return Ok(());
        }
        let (method, params) = match action {
            PackageAction::Recovery => unreachable!(),
            PackageAction::Enable { id, enabled } => {
                ("setPluginEnabled", json!({"id":id,"enabled":enabled}))
            }
            PackageAction::Remove { kind, id } => {
                check_kind(&kind)?;
                if kind == "frontend" && id == self.frontend_id() {
                    return Err(UiError::invalid("remove_self_frontend", "不能从当前窗口移除自身前端。"));
                }
                ("removePlugin", json!({"kind":kind,"id":id}))
            }
            PackageAction::Open { id } => ("openExternalPluginUi", json!({"id":id})),
            PackageAction::Default { id } => ("setDefaultFrontend", json!({"id":id})),
            PackageAction::Refresh => ("refreshPlugins", json!({})),
        };
        let result: Value = self.management_call(method, params).await?;
        if method == "refreshPlugins" && result["frontend"]["reloaded"] == false {
            return Err(UiError::invalid(
                "frontend_rescan_failed",
                "前端目录重新扫描失败，已切入原生修复入口。请检查安装包状态。",
            ));
        }
        Ok(())
    }
    pub async fn settings_rollback_preview(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<PackagePreview, UiError> {
        check_kind(kind)?;
        self.management_call(
            "previewPluginPackage",
            json!({"kind":kind,"rollback_id":id}),
        )
        .await
    }
    pub async fn settings_rollback(&self, preview: PackagePreview) -> Result<(), UiError> {
        check_kind(&preview.kind)?;
        let _: Value = self
            .management_call(
                "rollbackPlugin",
                json!({"kind":preview.kind,"id":preview.id,"expected_preview":preview}),
            )
            .await?;
        Ok(())
    }

    pub async fn proxy_config(&self) -> Result<UiProxyConfig, UiError> {
        let result = self
            .handle()
            .proxy_config()
            .await
            .map_err(|error| proxy_error("getProxyConfig", error))?;
        Ok(UiProxyConfig::from(result))
    }

    pub async fn set_proxy_config(
        &self,
        update: UiProxyConfigUpdate,
    ) -> Result<UiProxyConfig, UiError> {
        let wire = validate_proxy_update(&update)?;
        self.guard("setProxyConfig")?;
        let result = self
            .handle()
            .set_proxy_config(&wire)
            .await
            .map_err(|error| proxy_error("setProxyConfig", error))?;
        Ok(UiProxyConfig::from(result))
    }

    pub async fn test_proxy(
        &self,
        request: UiTestProxyRequest,
    ) -> Result<UiTestProxyResult, UiError> {
        let wire = validate_test_proxy_request(&request)?;
        self.guard("testProxy")?;
        let result = self
            .handle()
            .test_proxy(&wire)
            .await
            .map_err(|error| proxy_error("testProxy", error))?;
        Ok(UiTestProxyResult::from(result))
    }
}
pub(crate) fn check_kind(kind: &str) -> Result<(), UiError> {
    if matches!(kind, "plugin" | "frontend") {
        Ok(())
    } else {
        Err(UiError::invalid("package_kind_unknown", "未知包类型。"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_detail_keeps_fixed_guidance_but_is_bounded() {
        assert_eq!(
            safe_probe_detail("认证被拒绝：请使用客户端授权码而不是登录密码"),
            "认证被拒绝：请使用客户端授权码而不是登录密码"
        );
        assert_eq!(safe_probe_detail("a\u{0}b\r\nc"), "abc");
        let long = "长".repeat(400);
        let bounded = safe_probe_detail(&long);
        assert!(bounded.len() <= MAX_PROBE_DETAIL_BYTES);
        assert!(bounded.chars().all(|ch| ch == '长'));
    }

    /// ADR-0048 L6：账户页拿到离线原因的 code 与有界的服务器原文；中文整句仍不过桥。
    #[test]
    fn profile_projection_keeps_the_detail_code_and_bounds_server_text() {
        let view: AccountProfileView = serde_json::from_value(json!({
            "id": "acct_abcdefghijklmnopqrstuvwxyz23", "revision": 1, "display_name": "Main",
            "address": "u@example.test", "enabled": true, "source": "managed", "editable": true,
            "incoming": {"kind": "imap", "server": "imap.example.test", "port": 993,
                "security": "implicit", "username": "u", "mailbox": "INBOX", "idle": true,
                "poll_secs": 60},
            "credentials": {"incoming": "saved"},
            "runtime": {"state": "error", "detail": "IMAP 登录失败: no\r\nresponse",
                "detail_code": "login_failed", "server_detail": format!("no\r\nresponse{}", "x".repeat(400))}
        }))
        .unwrap();
        let projected = profile(view);
        assert_eq!(projected.runtime.detail, "");
        assert_eq!(projected.runtime.detail_code.as_deref(), Some("login_failed"));
        assert!(projected.runtime.server_detail.starts_with("noresponse"));
        assert!(projected.runtime.server_detail.len() <= MAX_PROBE_DETAIL_BYTES);
    }

    /// 新后台：缺省值在内核侧不序列化，桥据 `persisted` 键补成明确值，界面才知道该显示开关。
    #[test]
    fn rules_projection_makes_new_mail_fields_explicit_only_for_new_daemons() {
        let base = json!({
            "show_sensitive": true, "silent_keywords": [], "quiet_hours": [],
            "timezone_offset_minutes": 0
        });
        let new = UiRules::from_response(json!({ "rules": base, "persisted": false })).unwrap();
        assert_eq!(new.rules.follow_system_timezone, Some(false));
        assert_eq!(new.rules.notify_new_mail, Some(false));
        assert_eq!(new.rules.muted_accounts, Some(Vec::new()));
        let rendered = serde_json::to_value(&new).unwrap();
        assert_eq!(rendered["rules"]["follow_system_timezone"], false);
        assert_eq!(rendered["rules"]["notify_new_mail"], false);
        assert_eq!(rendered["rules"]["muted_accounts"], json!([]));

        let old = UiRules::from_response(json!({ "rules": base })).unwrap();
        assert_eq!(old.rules.follow_system_timezone, None);
        assert_eq!(old.rules.notify_new_mail, None);
        let rendered = serde_json::to_value(&old.rules).unwrap();
        assert!(rendered.get("follow_system_timezone").is_none());
        assert!(rendered.get("notify_new_mail").is_none(), "旧后台不认识的键不回传给它");
        assert!(rendered.get("muted_accounts").is_none());

        let mut on = base.clone();
        on["follow_system_timezone"] = json!(true);
        on["notify_new_mail"] = json!(true);
        on["muted_accounts"] = json!(["work"]);
        let on = UiRules::from_response(json!({ "rules": on })).unwrap();
        assert_eq!(on.rules.follow_system_timezone, Some(true));
        assert_eq!(on.rules.notify_new_mail, Some(true));
        assert_eq!(on.rules.muted_accounts, Some(vec!["work".to_string()]));
    }

    #[test]
    fn discovered_servers_projection_matches_webview_shape() {
        let found: maild_client::ProviderDiscovery = serde_json::from_value(json!({
            "provider_id": "qq", "display_name": "QQ 邮箱", "source": "preset",
            "confidence": "verified",
            "incoming": { "server": "imap.qq.com", "port": 993, "security": "implicit" },
            "outgoing": { "server": "smtp.qq.com", "port": 465, "security": "implicit" },
        }))
        .unwrap();
        let view = serde_json::to_value(DiscoveredServers::from(found)).unwrap();
        assert_eq!(
            view,
            json!({
                "incoming": { "server": "imap.qq.com", "port": 993, "security": "implicit" },
                "outgoing": { "server": "smtp.qq.com", "port": 465, "security": "implicit" },
                "source": "preset"
            })
        );
    }

    #[test]
    fn proxy_dtos_never_expose_passwords_and_redact_debug() {
        let wire = ProxyConfigResult {
            config: ProxyConfig {
                mode: "custom".into(),
                custom: Some(CustomProxyConfig {
                    protocol: "socks5h".into(),
                    host: "127.0.0.1".into(),
                    port: 7890,
                    username: Some("alice".into()),
                    password: Some("top-secret-password".into()),
                    bypass: vec!["*.internal".into()],
                }),
            },
            has_password: true,
            effective_system: SystemProxySnapshot {
                http: Some(ProxyEndpointInfo {
                    protocol: "http".into(),
                    host: "127.0.0.1".into(),
                    port: 8080,
                    username: Some("sys".into()),
                    password: Some("sys-secret-password".into()),
                }),
                source: "env".into(),
                ..Default::default()
            },
            persisted: true,
        };
        let ui = UiProxyConfig::from(wire.clone());
        let json_str = serde_json::to_string(&ui).unwrap();
        assert!(!json_str.contains("top-secret-password"), "{json_str}");
        assert!(!json_str.contains("sys-secret-password"), "{json_str}");
        assert!(!json_str.contains("\"password\""), "{json_str}");
        assert!(ui.has_password);

        let event = UiProxyEvent::from(ProxyPreferenceChanged {
            config: wire.config,
            has_password: true,
            effective_system: wire.effective_system,
            persisted: true,
            generation: 7,
        });
        let event_json = serde_json::to_string(&event).unwrap();
        assert!(!event_json.contains("top-secret-password"), "{event_json}");
        assert!(!event_json.contains("\"password\""), "{event_json}");
        assert_eq!(event.generation, 7);

        let upd = UiCustomProxyUpdate {
            protocol: "socks5h".into(),
            host: "127.0.0.1".into(),
            port: 7890,
            username: Some("alice".into()),
            password: Some("super-secret-input".into()),
            clear_password: false,
            bypass: vec![],
        };
        let debug = format!("{upd:?}");
        assert!(!debug.contains("super-secret-input"), "{debug}");
        assert!(debug.contains("[REDACTED]"), "{debug}");
    }

    #[test]
    fn proxy_errors_handle_old_daemon_and_unpersisted_config() {
        let rpc = |code: i64| maild_client::ClientError {
            kind: maild_client::ErrorKind::Rpc,
            message: "secret-echo-never-shown".into(),
            rpc_code: Some(code),
            rpc_data: None,
        };
        for code in [-32601, -32016] {
            let err = proxy_error("getProxyConfig", rpc(code));
            assert_eq!((err.code, err.reason), (UiErrorCode::Failed, "proxy_unsupported"));
        }
        assert_eq!(
            proxy_error("setProxyConfig", rpc(-32601)).reason,
            "proxy_unsupported"
        );
        assert_eq!(
            proxy_error("testProxy", rpc(-32601)).reason,
            "proxy_unsupported"
        );
        let denied = proxy_error("setProxyConfig", rpc(-32016));
        assert_eq!((denied.code, denied.reason), (UiErrorCode::Denied, "denied"));
        let unpersisted = proxy_error("setProxyConfig", rpc(-32001));
        assert_eq!(
            (unpersisted.code, unpersisted.reason),
            (UiErrorCode::Failed, "proxy_not_persisted")
        );
        let invalid = proxy_error("setProxyConfig", rpc(-32602));
        assert_eq!(
            (invalid.code, invalid.reason),
            (UiErrorCode::Invalid, "proxy_invalid")
        );
        let offline = proxy_error(
            "getProxyConfig",
            maild_client::ClientError::connection("down"),
        );
        assert_eq!((offline.code, offline.reason), (UiErrorCode::Offline, "offline"));
    }

    #[test]
    fn proxy_update_and_test_validation_reject_bad_inputs() {
        assert!(validate_proxy_update(&UiProxyConfigUpdate {
            mode: "system".into(),
            custom: None,
            clear_password: false,
        })
        .is_ok());
        assert_eq!(
            validate_proxy_update(&UiProxyConfigUpdate {
                mode: "custom".into(),
                custom: None,
                clear_password: false,
            })
            .unwrap_err()
            .reason,
            "proxy_invalid"
        );
        assert_eq!(
            validate_proxy_update(&UiProxyConfigUpdate {
                mode: "custom".into(),
                custom: Some(UiCustomProxyUpdate {
                    protocol: "socks5h".into(),
                    host: "127.0.0.1".into(),
                    port: 0,
                    username: None,
                    password: None,
                    clear_password: false,
                    bypass: vec![],
                }),
                clear_password: false,
            })
            .unwrap_err()
            .reason,
            "proxy_invalid"
        );
        assert_eq!(
            validate_proxy_update(&UiProxyConfigUpdate {
                mode: "custom".into(),
                custom: Some(UiCustomProxyUpdate {
                    protocol: "socks5h".into(),
                    host: "127.0.0.1".into(),
                    port: 7890,
                    username: None,
                    password: Some("pw".into()),
                    clear_password: true,
                    bypass: vec![],
                }),
                clear_password: false,
            })
            .unwrap_err()
            .reason,
            "proxy_invalid"
        );
        assert_eq!(
            validate_test_proxy_request(&UiTestProxyRequest {
                target: Some("no-port".into()),
                ..Default::default()
            })
            .unwrap_err()
            .reason,
            "proxy_target_invalid"
        );
        assert!(validate_test_proxy_request(&UiTestProxyRequest {
            target: Some("imap.gmail.com:993".into()),
            ..Default::default()
        })
        .is_ok());
    }
}
